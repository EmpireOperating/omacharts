//! The main window.
//!
//! A header bar, a chart, and a watchlist that can be hidden outright. The
//! chart is the main character: the chrome is one row of controls and
//! everything else is the drawing.
//!
//! Nothing here waits on the network. Opening a symbol paints whatever the
//! cache holds immediately, asks a worker for the gap, and repaints when the
//! answer arrives.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use omacharts_engine::providers::Yahoo;
use omacharts_engine::{
    resample, BarStyle, Indicator, Instrument, Provider, SearchIndex, Session, Timeframe,
};

use crate::loader::{Loader, Request, Response, BACKGROUND, FOREGROUND};
use crate::store::Store;
use crate::theming::Theming;
use crate::ui::chart::{ChartView, Drawn};
use crate::ui::preferences::Preferences;
use crate::ui::search::SymbolSearch;
use crate::ui::watchlist::{Quote, Watchlist, DEFAULTS};
use gtk::gio;

/// How often we look for a desktop theme change. Cheap enough to be invisible,
/// often enough to feel immediate.
const THEME_POLL_SECONDS: u32 = 2;

/// One row of a popover menu, optionally ticked.
fn menu_item(label: &str, selected: bool) -> gtk::Button {
    let tick = gtk::Image::from_icon_name(if selected {
        "object-select-symbolic"
    } else {
        "empty-symbolic"
    });
    tick.set_opacity(if selected { 1.0 } else { 0.0 });

    let text = gtk::Label::new(Some(label));
    text.set_xalign(0.0);
    text.set_hexpand(true);

    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.append(&tick);
    row.append(&text);

    let button = gtk::Button::new();
    button.set_child(Some(&row));
    button.add_css_class("flat");
    button
}

/// How many symbols either side of the selection to fetch ahead.
///
/// Small on purpose. Every request is one someone may never want, and the
/// window moves with the selection — arrow up and the two above you are
/// already being fetched. Filling an entire watchlist up front is fifty
/// requests in a burst for a list most of which never gets looked at.
const PREFETCH_WINDOW: usize = 3;

const LAST_SYMBOL: &str = "last_symbol";
const LAST_SUFFIX: &str = "last_suffix";
const LAST_TIMEFRAME: &str = "last_timeframe";
const SHOW_WATCHLIST: &str = "show_watchlist";
pub const SETTING_BAR_STYLE: &str = "bar_style";

pub struct Window {
    pub window: adw::ApplicationWindow,
    chart: Rc<ChartView>,
    store: Rc<Store>,
    index: Rc<SearchIndex>,
    theming: Rc<RefCell<Theming>>,
    search: Rc<SymbolSearch>,
    watchlist: RefCell<Option<Rc<Watchlist>>>,
    provider: Rc<Yahoo>,
    loader: Loader,
    current: Rc<RefCell<Option<Instrument>>>,
    /// Folded series, keyed by cache key and the resolution shown.
    ///
    /// The bars are the chart — there is nothing else to preload — so once a
    /// symbol has been fetched, switching to it is a read and a draw. This
    /// removes even the read, which is what makes arrowing back and forth over
    /// the same few symbols feel like nothing is happening at all.
    series: Rc<RefCell<HashMap<(String, Timeframe), Rc<Vec<omacharts_engine::Bar>>>>>,
    timeframe: Rc<RefCell<Timeframe>>,
    symbol_button: gtk::Button,
    /// The chart's legend: what this is, and at what resolution.
    legend: gtk::Label,
    /// Regular or extended hours.
    session: Rc<RefCell<Session>>,
    /// One set of indicators, shared by every symbol.
    indicators: Rc<RefCell<Vec<Indicator>>>,
    bar_style: Rc<RefCell<BarStyle>>,
    /// The split, so the keyboard can show and hide the rail.
    split: adw::OverlaySplitView,
    /// The preset strip, so a typed resolution can update it.
    timeframe_buttons: RefCell<Option<Vec<(Timeframe, gtk::ToggleButton)>>>,
}

impl Window {
    pub fn build(app: &adw::Application, store: Rc<Store>) -> Rc<Window> {
        store.seed_watchlist_if_empty(DEFAULTS);

        let index = Rc::new(SearchIndex::new(omacharts_engine::symbols::seed()));
        let theming = Rc::new(RefCell::new(Theming::load(&store)));
        theming.borrow_mut().apply();

        let (theme, scheme) = {
            let t = theming.borrow();
            (t.theme(), t.bar_scheme())
        };
        let chart = ChartView::new(theme, scheme);
        let (sender, receiver) = async_channel::unbounded::<Response>();
        let loader = Loader::new(Yahoo::new(), sender.clone());

        let window = adw::ApplicationWindow::new(app);
        window.set_title(Some("omacharts"));
        window.set_default_size(1280, 800);

        let symbol_button = gtk::Button::new();
        symbol_button.add_css_class("flat");
        symbol_button.set_tooltip_text(Some("Find a symbol (Ctrl+K)"));

        let readout = gtk::Label::new(None);
        readout.add_css_class("readout-symbol");
        readout.set_valign(gtk::Align::Center);
        readout.set_can_target(false);

        let split = adw::OverlaySplitView::new();

        let this = Rc::new(Window {
            window: window.clone(),
            chart: chart.clone(),
            store: store.clone(),
            index: index.clone(),
            theming: theming.clone(),
            search: SymbolSearch::new(index.clone()),
            watchlist: RefCell::new(None),
            provider: Rc::new(Yahoo::new()),
            loader,
            current: Rc::new(RefCell::new(None)),
            series: Rc::new(RefCell::new(HashMap::new())),
            timeframe: Rc::new(RefCell::new(
                store
                    .setting(LAST_TIMEFRAME)
                    .and_then(|k| Timeframe::parse(&k))
                    .unwrap_or(Timeframe::days(1)),
            )),
            symbol_button,
            legend: readout.clone(),
            indicators: Rc::new(RefCell::new(store.indicators())),
            bar_style: Rc::new(RefCell::new(
                store
                    .setting(SETTING_BAR_STYLE)
                    .and_then(|k| BarStyle::from_key(&k))
                    .unwrap_or_default(),
            )),
            split: split.clone(),
            session: Rc::new(RefCell::new(
                store
                    .setting(crate::ui::chart_settings::SETTING_SESSION)
                    .and_then(|k| Session::from_key(&k))
                    .unwrap_or_default(),
            )),
            timeframe_buttons: RefCell::new(None),
        });

        let watchlist = this.build_watchlist();
        *this.watchlist.borrow_mut() = Some(watchlist.clone());

        let split = &this.split;
        split.set_sidebar_position(gtk::PackType::End);
        split.set_sidebar(Some(&watchlist.widget));
        split.set_collapsed(false);
        split.set_show_sidebar(store.setting_bool(SHOW_WATCHLIST, true));

        let gear = gtk::Button::from_icon_name("emblem-system-symbolic");
        gear.add_css_class("flat");
        gear.add_css_class("legend-gear");
        gear.set_tooltip_text(Some("Chart settings"));
        gear.set_valign(gtk::Align::Center);

        let legend_bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        legend_bar.set_halign(gtk::Align::Start);
        legend_bar.set_valign(gtk::Align::Start);
        legend_bar.set_margin_start(12);
        legend_bar.set_margin_top(6);
        legend_bar.append(&readout);
        legend_bar.append(&gear);

        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&chart.area));
        overlay.add_overlay(&legend_bar);
        split.set_content(Some(&overlay));

        let opener = this.clone();
        gear.connect_clicked(move |_| opener.open_chart_settings());

        let header = this.build_header(&split);
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(split));
        window.set_content(Some(&toolbar));

        this.chart.set_bar_style(*this.bar_style.borrow());
        let menu_owner = this.clone();
        this.chart.set_context_menu_handler(move |x, y| menu_owner.chart_menu(x, y));
        this.wire_shortcuts();
        this.wire_responses(receiver);
        this.wire_theme_polling();

        this.restore_last_symbol();
        this
    }

    fn build_header(self: &Rc<Self>, split: &adw::OverlaySplitView) -> adw::HeaderBar {
        let header = adw::HeaderBar::new();

        let this = self.clone();
        self.symbol_button.connect_clicked(move |_| this.open_search());
        header.pack_start(&self.symbol_button);
        header.set_title_widget(Some(&self.build_timeframes()));

        let menu = gio::Menu::new();
        menu.append(Some("Preferences"), Some("win.preferences"));
        menu.append(Some("Keyboard Shortcuts"), Some("win.shortcuts"));
        let menu_button = gtk::MenuButton::new();
        menu_button.set_icon_name("open-menu-symbolic");
        menu_button.set_menu_model(Some(&menu));
        header.pack_end(&menu_button);

        let toggle = gtk::ToggleButton::new();
        toggle.set_icon_name("sidebar-show-right-symbolic");
        toggle.set_tooltip_text(Some("Watchlist (Ctrl+B)"));
        toggle.set_active(split.shows_sidebar());
        let split_weak = split.downgrade();
        let store = self.store.clone();
        toggle.connect_toggled(move |toggle| {
            if let Some(split) = split_weak.upgrade() {
                split.set_show_sidebar(toggle.is_active());
            }
            store.set_setting_bool(SHOW_WATCHLIST, toggle.is_active());
        });
        header.pack_end(&toggle);

        // Ctrl+B needs to drive the button so its pressed state stays honest.
        let action = gio::SimpleAction::new("watchlist", None);
        let toggle_weak = toggle.downgrade();
        action.connect_activate(move |_, _| {
            if let Some(toggle) = toggle_weak.upgrade() {
                toggle.set_active(!toggle.is_active());
            }
        });
        self.window.add_action(&action);

        header
    }

    fn build_timeframes(self: &Rc<Self>) -> gtk::Box {
        let strip = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        strip.add_css_class("linked");
        strip.add_css_class("timeframe-strip");

        let mut first: Option<gtk::ToggleButton> = None;
        let mut buttons = Vec::new();
        for timeframe in Timeframe::PRESETS {
            let button = gtk::ToggleButton::with_label(&timeframe.label());
            button.add_css_class("flat");
            match &first {
                Some(anchor) => button.set_group(Some(anchor)),
                None => first = Some(button.clone()),
            }
            button.set_active(timeframe == *self.timeframe.borrow());

            let this = self.clone();
            button.connect_toggled(move |button| {
                if button.is_active() {
                    this.set_timeframe(timeframe);
                }
            });
            strip.append(&button);
            buttons.push((timeframe, button));
        }
        *self.timeframe_buttons.borrow_mut() = Some(buttons);
        strip
    }

    fn build_watchlist(self: &Rc<Self>) -> Rc<Watchlist> {
        let store = self.store.clone();
        let provider = self.provider.clone();
        // Quotes come from the cache alone. A rail that fetches is a rail that
        // gets you throttled.
        let quote: crate::ui::watchlist::QuoteLookup = Rc::new(move |instrument: &Instrument| {
            let symbol = provider.symbol_for(instrument)?;
            let bars = store.load_bars(&format!("yahoo:{symbol}"), Timeframe::days(1));
            let (previous, last) = (bars.get(bars.len().checked_sub(2)?)?, bars.last()?);
            let change = last.close - previous.close;
            Some(Quote {
                last: last.close,
                change,
                change_pct: if previous.close == 0.0 {
                    0.0
                } else {
                    change / previous.close * 100.0
                },
            })
        });

        let this = self.clone();
        Watchlist::new(
            self.store.clone(),
            self.index.clone(),
            self.search.clone(),
            quote,
            move |instrument| this.show(instrument),
        )
    }



    fn wire_shortcuts(self: &Rc<Self>) {
        let preferences = gio::SimpleAction::new("preferences", None);
        let this = self.clone();
        preferences.connect_activate(move |_, _| this.open_preferences());
        self.window.add_action(&preferences);

        let shortcuts = gio::SimpleAction::new("shortcuts", None);
        let this = self.clone();
        shortcuts.connect_activate(move |_, _| this.show_shortcuts());
        self.window.add_action(&shortcuts);

        let find = gio::SimpleAction::new("find", None);
        let this = self.clone();
        find.connect_activate(move |_, _| this.open_search());
        self.window.add_action(&find);

        let keys = gtk::EventControllerKey::new();
        let this = self.clone();
        keys.connect_key_pressed(move |_, key, _, state| {
            use gtk::gdk::Key;
            let ctrl = state.contains(gtk::gdk::ModifierType::CONTROL_MASK);
            let alt = state.contains(gtk::gdk::ModifierType::ALT_MASK);

            match key {
                // Find a symbol. Ctrl+K is what modern apps use; Ctrl+F is
                // what this desktop uses for find.
                Key::k | Key::f if ctrl => {
                    this.open_search();
                    return glib::Propagation::Stop;
                }
                // Show and hide the rail. F9 is the platform's sidebar key.
                Key::b if ctrl => {
                    this.toggle_watchlist();
                    return glib::Propagation::Stop;
                }
                Key::F9 => {
                    this.toggle_watchlist();
                    return glib::Propagation::Stop;
                }
                // Move the keyboard between panes, the way F6 does elsewhere.
                Key::F6 => {
                    this.cycle_focus();
                    return glib::Propagation::Stop;
                }
                // Back to the chart, from wherever the keyboard ended up.
                Key::Escape => {
                    this.chart.area.grab_focus();
                    return glib::Propagation::Stop;
                }
                Key::i if ctrl => {
                    this.open_chart_settings();
                    return glib::Propagation::Stop;
                }
                Key::comma if ctrl => {
                    this.open_preferences();
                    return glib::Propagation::Stop;
                }
                Key::question if ctrl => {
                    this.show_shortcuts();
                    return glib::Propagation::Stop;
                }
                Key::w if ctrl => {
                    this.window.close();
                    return glib::Propagation::Stop;
                }
                Key::r | Key::R if alt => {
                    this.chart.reset_view();
                    return glib::Propagation::Stop;
                }
                _ => {}
            }

            if alt {
                return glib::Propagation::Proceed;
            }

            // The rail owns the arrows while the keyboard is on it.
            let on_watchlist = this
                .watchlist
                .borrow()
                .as_ref()
                .map(|w| w.has_focus())
                .unwrap_or(false);

            match (key, ctrl) {
                (Key::slash, false) => {
                    this.open_search();
                    glib::Propagation::Stop
                }
                (Key::Left, false) if !on_watchlist => {
                    this.chart.pan_bars(-5);
                    glib::Propagation::Stop
                }
                (Key::Right, false) if !on_watchlist => {
                    this.chart.pan_bars(5);
                    glib::Propagation::Stop
                }
                (Key::plus | Key::equal, false) => {
                    this.chart.zoom(1.0 / 1.25);
                    glib::Propagation::Stop
                }
                (Key::minus, false) => {
                    this.chart.zoom(1.25);
                    glib::Propagation::Stop
                }
                (Key::End, false) if !on_watchlist => {
                    this.chart.go_to_latest();
                    glib::Propagation::Stop
                }
                (_, true) => glib::Propagation::Proceed,
                _ => {
                    // Start typing and the chart does what every charting tool
                    // does: letters look for a symbol, digits set the
                    // resolution. The keystroke carries into the box.
                    match key.to_unicode() {
                        Some(c) if c.is_ascii_alphabetic() => {
                            this.open_search_with(&c.to_string());
                            glib::Propagation::Stop
                        }
                        Some(c) if c.is_ascii_digit() => {
                            this.prompt_resolution(&c.to_string());
                            glib::Propagation::Stop
                        }
                        _ => glib::Propagation::Proceed,
                    }
                }
            }
        });
        self.window.add_controller(keys);
    }

    /// Fold arriving bars into the chart. Runs on the main thread.
    fn wire_responses(self: &Rc<Self>, receiver: async_channel::Receiver<Response>) {
        let this = self.clone();
        glib::spawn_future_local(async move {
            while let Ok(response) = receiver.recv().await {
                match response {
                    Response::Bars { key, timeframe, bars } => {
                        this.present(&key, timeframe, bars, false);
                    }
                    Response::Failed { key, timeframe, bars, error, rate_limited } => {
                        // A failure shows the same chart, marked stale. Never
                        // an empty pane.
                        this.present(&key, timeframe, bars, true);
                        let _ = (error, rate_limited);
                    }
                }
            }
        });
    }

    fn wire_theme_polling(self: &Rc<Self>) {
        let this = self.clone();
        glib::timeout_add_seconds_local(THEME_POLL_SECONDS, move || {
            if this.theming.borrow_mut().poll_omarchy() {
                this.restyle();
            }
            glib::ControlFlow::Continue
        });
    }

    /// Repaint with the current theme and bar scheme.
    pub fn restyle(self: &Rc<Self>) {
        let (theme, scheme) = {
            let mut theming = self.theming.borrow_mut();
            theming.apply();
            (theming.theme(), theming.bar_scheme())
        };
        self.chart.restyle(theme, scheme);
        if let Some(watchlist) = self.watchlist.borrow().as_ref() {
            watchlist.rebuild();
        }
    }

    fn open_search(self: &Rc<Self>) {
        self.open_search_with("");
    }

    fn open_search_with(self: &Rc<Self>, query: &str) {
        let this = self.clone();
        self.search.present_with(&self.window, "Find symbol", query, move |instrument| {
            this.show(instrument)
        });
    }

    /// The resolution box: type "3", "15", "4h", "1D".
    ///
    /// A bare number means minutes, so the digit that opened this is already
    /// the start of an answer.
    fn prompt_resolution(self: &Rc<Self>, start: &str) {
        let entry = gtk::Entry::new();
        entry.set_text(start);
        entry.set_position(-1);
        entry.set_placeholder_text(Some("3, 15, 4h, 1D"));
        entry.set_width_chars(10);

        let hint = gtk::Label::new(Some("Resolution"));
        hint.add_css_class("dim-label");
        hint.add_css_class("caption");
        hint.set_xalign(0.0);

        // Says what is about to happen, as it is typed. "240" reading back as
        // "4 hours" is the whole reason this is here.
        let preview = gtk::Label::new(None);
        preview.add_css_class("caption");
        preview.set_xalign(0.0);

        let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
        content.append(&hint);
        content.append(&entry);
        content.append(&preview);

        let describe = |text: &str, preview: &gtk::Label| {
            preview.remove_css_class("dim-label");
            preview.remove_css_class("error");
            match Timeframe::parse(text) {
                Some(timeframe) => preview.set_text(&timeframe.description()),
                None if text.trim().is_empty() => {
                    preview.set_text("3, 15, 4h, 1D");
                    preview.add_css_class("dim-label");
                }
                None => {
                    preview.set_text("not a resolution");
                    preview.add_css_class("error");
                }
            }
        };
        describe(&entry.text(), &preview);

        let preview_weak = preview.downgrade();
        entry.connect_changed(move |entry| {
            if let Some(preview) = preview_weak.upgrade() {
                describe(&entry.text(), &preview);
            }
        });

        let popover = gtk::Popover::new();
        popover.set_child(Some(&content));
        popover.set_parent(&self.symbol_button);

        let this = self.clone();
        let popover_weak = popover.downgrade();
        entry.connect_activate(move |entry| {
            if let Some(timeframe) = Timeframe::parse(&entry.text()) {
                this.apply_timeframe(timeframe);
            }
            if let Some(popover) = popover_weak.upgrade() {
                popover.popdown();
            }
        });

        popover.popup();
        entry.grab_focus();
        entry.set_position(-1);
    }

    pub fn open_chart_settings(self: &Rc<Self>) {
        crate::ui::chart_settings::ChartSettings::present(self, self.store.clone());
    }

    /// Re-fold and repaint what is on screen, after something that changes
    /// how the bars are read rather than which bars they are.
    fn redraw_current(self: &Rc<Self>) {
        self.series.borrow_mut().clear();
        let instrument = self.current.borrow().clone();
        if let Some(instrument) = instrument {
            self.show(instrument);
        }
    }

    fn open_preferences(self: &Rc<Self>) {
        let this = self.clone();
        Preferences::present(
            &self.window,
            self.store.clone(),
            self.theming.clone(),
            Rc::new(move || this.restyle()),
        );
    }

    fn show_shortcuts(self: &Rc<Self>) {
        let body = "Type a letter   Find a symbol\n\
                    Type a number   Set the resolution\n\
                    Ctrl+K          Find a symbol\n\
                    Ctrl+I          Chart settings and indicators\n\
                    Ctrl+,          Preferences\n\
                    \n\
                    Ctrl+B · F9     Show or hide the watchlist\n\
                    F6              Move between chart and watchlist\n\
                    Esc             Back to the chart\n\
                    ↑ ↓             Next or previous symbol\n\
                    Ctrl+↑ ↓        Next or previous section\n\
                    Delete          Remove the symbol from the watchlist\n\
                    \n\
                    ← →             Pan\n\
                    + −             Zoom\n\
                    End             Jump to the latest bar\n\
                    Alt+R           Reset the chart";
        let dialog = adw::AlertDialog::new(Some("Keyboard shortcuts"), Some(body));
        dialog.add_response("close", "Close");
        dialog.present(Some(&self.window));
    }

    /// Switch resolution from somewhere other than the strip, keeping the
    /// strip's buttons honest about what is being shown.
    fn apply_timeframe(self: &Rc<Self>, timeframe: Timeframe) {
        self.set_timeframe(timeframe);
        if let Some(buttons) = self.timeframe_buttons.borrow().as_ref() {
            for (preset, button) in buttons {
                button.set_active(*preset == timeframe);
            }
        }
    }

    fn set_timeframe(self: &Rc<Self>, timeframe: Timeframe) {
        *self.timeframe.borrow_mut() = timeframe;
        self.store.set_setting(LAST_TIMEFRAME, &timeframe.key());
        let instrument = self.current.borrow().clone();
        if let Some(instrument) = instrument {
            self.show(instrument);
        }
    }

    /// Chart an instrument: paint from cache now, fetch the gap in the
    /// background.
    pub fn show(self: &Rc<Self>, instrument: Instrument) {
        let timeframe = *self.timeframe.borrow();
        let Some(symbol) = self.provider.symbol_for(&instrument) else {
            return;
        };
        let key = format!("{}:{symbol}", self.provider.id());
        let native = timeframe.native();

        self.symbol_button.set_label(&instrument.display_symbol());
        self.legend
            .set_text(&format!("{}  ·  {}", instrument.display_symbol(), timeframe.label()));
        self.window
            .set_title(Some(&format!("{} · omacharts", instrument.display_symbol())));
        *self.current.borrow_mut() = Some(instrument.clone());
        self.store.set_setting(LAST_SYMBOL, &instrument.symbol);
        self.store
            .set_setting(LAST_SUFFIX, instrument.suffix.as_deref().unwrap_or(""));

        // Cache first, so the chart is on screen before any request leaves.
        let memo = self.series.borrow().get(&(key.clone(), timeframe)).cloned();
        let shown = match memo {
            Some(bars) => {
                let empty = bars.is_empty();
                self.chart.set_series(instrument.clone(), timeframe, (*bars).clone());
                empty
            }
            None => {
                let cached = self.store.load_bars(&key, native);
                let empty = cached.is_empty();
                self.paint(&key, &instrument, timeframe, cached);
                empty
            }
        };
        let cached_was_empty = shown;
        self.chart.set_stale(false);

        // The chart on screen overtakes everything queued behind it, and the
        // old neighbours stop being the nearest ones the moment we move.
        if let Some(watchlist) = self.watchlist.borrow().as_ref() {
            watchlist.highlight(&instrument);
        }
        // The chart on screen overtakes everything queued behind it, and the
        // old neighbourhood is forgotten: those symbols are no longer the ones
        // a keypress away.
        self.chart.set_loading(cached_was_empty);
        self.loader.drop_prefetches();
        self.loader
            .fetch(Request { key, symbol, timeframe, speculative: false }, FOREGROUND);
        self.prefetch_neighbours(&instrument, timeframe);
    }


    /// Fetch ahead around the selection, nearest first.
    ///
    /// This is what makes arrowing through the rail feel instant: by the time
    /// you press the key the next one is already cached, and moving shifts the
    /// window so the symbols now a keypress away are the ones being fetched.
    /// Everything here is speculative, so the provider paces it apart and
    /// drops it entirely if Yahoo starts refusing.
    fn prefetch_neighbours(self: &Rc<Self>, current: &Instrument, timeframe: Timeframe) {
        let Some(watchlist) = self.watchlist.borrow().as_ref().cloned() else {
            return;
        };
        let order = watchlist.flat_order();
        let Some(position) = order
            .iter()
            .position(|i| i.symbol == current.symbol && i.suffix == current.suffix)
        else {
            return;
        };

        let first = position.saturating_sub(PREFETCH_WINDOW);
        let last = (position + PREFETCH_WINDOW).min(order.len().saturating_sub(1));

        for index in first..=last {
            if index == position {
                continue;
            }
            let instrument = &order[index];
            let Some(symbol) = self.provider.symbol_for(instrument) else { continue };
            let key = format!("{}:{symbol}", self.provider.id());
            let distance = index.abs_diff(position) as u32;

            if self.store.coverage(&key, timeframe.native()).is_none() {
                self.loader.fetch(
                    Request {
                        key: key.clone(),
                        symbol: symbol.clone(),
                        timeframe,
                        speculative: true,
                    },
                    distance,
                );
            }
            // The rail's change column reads daily bars, so a neighbour needs
            // them even if you never open it — but after its chart data.
            let daily = Timeframe::days(1);
            if timeframe.native() != daily && self.store.coverage(&key, daily).is_none() {
                self.loader.fetch(
                    Request { key, symbol, timeframe: daily, speculative: true },
                    distance + BACKGROUND,
                );
            }
        }
    }

    /// Apply bars that arrived for `key`, ignoring a reply for a chart the user
    /// has already navigated away from.
    fn present(self: &Rc<Self>, key: &str, native: Timeframe, bars: Vec<omacharts_engine::Bar>, stale: bool) {
        let instrument = self.current.borrow().clone();
        let Some(instrument) = instrument else { return };
        let Some(symbol) = self.provider.symbol_for(&instrument) else { return };
        if key != format!("{}:{symbol}", self.provider.id()) {
            // Something we prefetched. Not our chart, but the rail's change
            // column may now have numbers it did not have a moment ago.
            if let Some(watchlist) = self.watchlist.borrow().as_ref() {
                watchlist.refresh_quotes();
            }
            return;
        }
        let timeframe = *self.timeframe.borrow();
        if timeframe.native() != native {
            return;
        }
        self.paint(key, &instrument, timeframe, bars);
        self.chart.set_stale(stale);
        self.chart.set_loading(false);
        if let Some(watchlist) = self.watchlist.borrow().as_ref() {
            watchlist.refresh_quotes();
        }
    }

    /// Fold to the shown resolution, memoise, and draw.
    fn paint(
        self: &Rc<Self>,
        key: &str,
        instrument: &Instrument,
        timeframe: Timeframe,
        bars: Vec<omacharts_engine::Bar>,
    ) {
        let bars = omacharts_engine::session::filter(
            &bars,
            *self.session.borrow(),
            instrument,
            timeframe.is_intraday(),
        );
        let bars = if timeframe.is_derived() {
            resample(&bars, timeframe, instrument.session_origin)
        } else {
            bars
        };
        self.recompute_indicators(instrument, timeframe, &bars);
        let bars = Rc::new(bars);
        {
            let mut series = self.series.borrow_mut();
            // A watchlist's worth of series is a couple of megabytes; past that
            // this is holding onto symbols nobody is going back to.
            if series.len() > 96 {
                series.clear();
            }
            series.insert((key.to_string(), timeframe), bars.clone());
        }
        self.chart.set_series(instrument.clone(), timeframe, (*bars).clone());
    }

    /// Run the indicators over the bars now on screen and hand them to the
    /// chart, coloured.
    ///
    /// Colour comes from the theme's palette by slot, so a set of indicators
    /// is distinguishable without anyone choosing anything — and follows the
    /// theme when it changes.
    fn recompute_indicators(
        self: &Rc<Self>,
        instrument: &Instrument,
        timeframe: Timeframe,
        bars: &[omacharts_engine::Bar],
    ) {
        let theme = self.theming.borrow().theme();
        let drawn: Vec<Drawn> = self
            .indicators
            .borrow()
            .iter()
            .enumerate()
            .map(|(slot, indicator)| Drawn {
                color: indicator.color(&theme, slot),
                output: omacharts_engine::indicators::compute(
                    indicator,
                    bars,
                    instrument.session_origin,
                    timeframe,
                ),
                indicator: indicator.clone(),
            })
            .collect();
        self.chart.set_indicators(drawn);
    }

    /// Replace the set of indicators and redraw.
    pub fn set_indicators(self: &Rc<Self>, indicators: Vec<Indicator>) {
        self.store.set_indicators(&indicators);
        *self.indicators.borrow_mut() = indicators;
        self.redraw_current();
    }

    /// The active theme, for anything that needs to resolve a colour.
    pub fn theme(&self) -> omacharts_engine::Theme {
        self.theming.borrow().theme()
    }

    pub fn session(&self) -> Rc<RefCell<Session>> {
        self.session.clone()
    }

    pub fn bar_style(&self) -> BarStyle {
        *self.bar_style.borrow()
    }

    pub fn set_bar_style(self: &Rc<Self>, style: BarStyle) {
        *self.bar_style.borrow_mut() = style;
        self.store.set_setting(SETTING_BAR_STYLE, style.key());
        self.chart.set_bar_style(style);
    }

    /// Right-clicking the chart offers the things you change most, and a way
    /// to everything else.
    fn chart_menu(self: &Rc<Self>, x: f64, y: f64) {
        let items = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let popover = gtk::Popover::new();
        popover.set_child(Some(&items));
        popover.set_parent(&self.chart.area);
        popover.set_has_arrow(false);
        popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));

        let current_style = self.bar_style();
        for style in BarStyle::ALL {
            let item = menu_item(style.label(), style == current_style);
            let this = self.clone();
            let popover_weak = popover.downgrade();
            item.connect_clicked(move |_| {
                this.set_bar_style(style);
                if let Some(p) = popover_weak.upgrade() {
                    p.popdown();
                }
            });
            items.append(&item);
        }

        items.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        let current_session = *self.session.borrow();
        for session in Session::ALL {
            let item = menu_item(session.label(), session == current_session);
            let this = self.clone();
            let popover_weak = popover.downgrade();
            item.connect_clicked(move |_| {
                *this.session.borrow_mut() = session;
                this.store
                    .set_setting(crate::ui::chart_settings::SETTING_SESSION, session.key());
                this.redraw_current();
                if let Some(p) = popover_weak.upgrade() {
                    p.popdown();
                }
            });
            items.append(&item);
        }

        items.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        let settings = menu_item("Chart settings…", false);
        let this = self.clone();
        let popover_weak = popover.downgrade();
        settings.connect_clicked(move |_| {
            if let Some(p) = popover_weak.upgrade() {
                p.popdown();
            }
            this.open_chart_settings();
        });
        items.append(&settings);

        popover.popup();
    }

    /// Show or hide the rail.
    pub fn toggle_watchlist(self: &Rc<Self>) {
        let showing = !self.split.shows_sidebar();
        self.split.set_show_sidebar(showing);
        self.store.set_setting_bool(SHOW_WATCHLIST, showing);
    }

    /// Move the keyboard between the chart and the rail, the way F6 does
    /// everywhere else on this desktop.
    pub fn cycle_focus(self: &Rc<Self>) {
        let Some(watchlist) = self.watchlist.borrow().as_ref().cloned() else { return };
        if watchlist.has_focus() {
            self.chart.area.grab_focus();
        } else {
            if !self.split.shows_sidebar() {
                self.toggle_watchlist();
            }
            watchlist.grab_focus();
        }
    }

    pub fn search(&self) -> Rc<SymbolSearch> {
        self.search.clone()
    }

    /// Re-fold and repaint, after something that changes how the bars are read.
    pub fn refresh(self: &Rc<Self>) {
        self.redraw_current();
    }

    pub fn indicators(&self) -> Vec<Indicator> {
        self.indicators.borrow().clone()
    }

    /// An id nothing on the chart is using.
    pub fn next_indicator_id(&self) -> u32 {
        self.indicators.borrow().iter().map(|i| i.id).max().unwrap_or(0) + 1
    }

    fn restore_last_symbol(self: &Rc<Self>) {
        let symbol = self.store.setting(LAST_SYMBOL).unwrap_or_else(|| "GSPC".to_string());
        let suffix = self.store.setting(LAST_SUFFIX).filter(|s| !s.is_empty());
        let instrument = self
            .index
            .find(&symbol, suffix.as_deref())
            .or_else(|| self.index.find("GSPC", None))
            .cloned();
        if let Some(instrument) = instrument {
            self.show(instrument);
        }
    }
}
