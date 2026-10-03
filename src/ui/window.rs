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
use crate::ui::colors;
use crate::ui::preferences::Preferences;
use crate::ui::search::SymbolSearch;
use crate::ui::watchlist::{Quote, Watchlist, DEFAULTS};
use gtk::gio;

/// How often we look for a desktop theme change. Cheap enough to be invisible,
/// often enough to feel immediate.
const THEME_POLL_SECONDS: u32 = 2;

/// Read the stored resolution strip, falling back to the presets.
fn parse_timeframes(stored: Option<&str>) -> Vec<Timeframe> {
    let mut listed: Vec<Timeframe> = stored
        .map(|s| s.split(',').filter_map(|k| Timeframe::parse(k.trim())).collect())
        .unwrap_or_default();
    listed.sort_by_key(|t| t.seconds());
    listed.dedup();
    if listed.is_empty() {
        return Timeframe::PRESETS.to_vec();
    }
    listed
}

fn format_timeframes(listed: &[Timeframe]) -> String {
    listed.iter().map(|t| t.key()).collect::<Vec<_>>().join(",")
}

/// What Ctrl+B should do next.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum WatchlistAction {
    Open,
    Focus,
    Close,
}

/// The whole of the Ctrl+B decision, kept separate from the widgets so it can
/// be read — and tested — without one.
fn watchlist_action(open: bool, focused: bool) -> WatchlistAction {
    match (open, focused) {
        (false, _) => WatchlistAction::Open,
        (true, false) => WatchlistAction::Focus,
        (true, true) => WatchlistAction::Close,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_resolution_strip_is_ordered_by_length() {
        let listed = parse_timeframes(Some("1D,3m,4h,15"));
        assert_eq!(
            listed.iter().map(|t| t.key()).collect::<Vec<_>>(),
            vec!["3m", "15m", "4h", "1D"]
        );
    }

    #[test]
    fn an_empty_or_unreadable_strip_falls_back_to_the_presets() {
        assert_eq!(parse_timeframes(None), Timeframe::PRESETS.to_vec());
        assert_eq!(parse_timeframes(Some("")), Timeframe::PRESETS.to_vec());
        assert_eq!(parse_timeframes(Some("banana,,")), Timeframe::PRESETS.to_vec());
    }

    #[test]
    fn the_strip_round_trips_and_does_not_repeat_itself() {
        let listed = parse_timeframes(Some("3m,3m,60,1h"));
        // "60" and "1h" are the same resolution.
        assert_eq!(listed.iter().map(|t| t.key()).collect::<Vec<_>>(), vec!["3m", "1h"]);
        assert_eq!(parse_timeframes(Some(&format_timeframes(&listed))), listed);
    }

    #[test]
    fn one_key_walks_the_watchlist_open_focused_closed() {
        // Closed: open it.
        assert_eq!(watchlist_action(false, false), WatchlistAction::Open);
        // Open, keyboard elsewhere: bring the keyboard here.
        assert_eq!(watchlist_action(true, false), WatchlistAction::Focus);
        // Open and focused: you are done with it.
        assert_eq!(watchlist_action(true, true), WatchlistAction::Close);
    }

    #[test]
    fn pressing_it_repeatedly_cycles_rather_than_sticking() {
        // Starting closed and unfocused, three presses return to the start.
        let mut open = false;
        let mut focused = false;
        let mut seen = Vec::new();
        for _ in 0..3 {
            let action = watchlist_action(open, focused);
            seen.push(action);
            match action {
                WatchlistAction::Open => {
                    open = true;
                    focused = true;
                }
                WatchlistAction::Focus => focused = true,
                WatchlistAction::Close => {
                    open = false;
                    focused = false;
                }
            }
        }
        assert_eq!(
            seen,
            vec![WatchlistAction::Open, WatchlistAction::Close, WatchlistAction::Open],
        );
        assert!(open && focused, "back where we started");
    }
}

/// Pop a real menu up where the pointer is.
///
/// A GtkPopoverMenu built from a menu model, rather than a box of flat
/// buttons: it is what the platform draws for a context menu, it handles
/// radio items and keyboard navigation itself, and it looks like every other
/// menu on the desktop.
fn popup_menu(model: &gio::Menu, over: &impl IsA<gtk::Widget>, x: f64, y: f64) {
    let popover = gtk::PopoverMenu::from_model(Some(model));
    popover.set_parent(over);
    popover.set_has_arrow(false);
    popover.set_halign(gtk::Align::Start);
    popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
    // Clicking an item closes the popover and *then* activates its action, so
    // unparenting on close would pull the action context out from under the
    // click. Let the activation happen first.
    popover.connect_closed(|popover| {
        let popover = popover.clone();
        glib::idle_add_local_once(move || popover.unparent());
    });
    popover.popup();
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
const SETTING_TIMEFRAMES: &str = "timeframes";

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
    /// One row per indicator, under the legend.
    indicator_legend: gtk::Box,
    /// Regular or extended hours.
    session: Rc<RefCell<Session>>,
    /// One set of indicators, shared by every symbol.
    indicators: Rc<RefCell<Vec<Indicator>>>,
    bar_style: Rc<RefCell<BarStyle>>,
    /// The split, so the keyboard can show and hide the rail.
    split: adw::OverlaySplitView,
    /// The resolution strip and what is on it.
    timeframe_strip: gtk::Box,
    timeframe_buttons: RefCell<Vec<(Timeframe, gtk::ToggleButton)>>,
    timeframes: RefCell<Vec<Timeframe>>,
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
            indicator_legend: {
                let rows = gtk::Box::new(gtk::Orientation::Vertical, 0);
                rows.set_halign(gtk::Align::Start);
                rows
            },
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
            timeframe_strip: {
                let strip = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                strip.add_css_class("linked");
                strip.add_css_class("timeframe-strip");
                strip
            },
            timeframe_buttons: RefCell::new(Vec::new()),
            timeframes: RefCell::new(parse_timeframes(
                store.setting(SETTING_TIMEFRAMES).as_deref(),
            )),
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
        legend_bar.append(&readout);
        legend_bar.append(&gear);

        // The legend stack: what you are looking at, then what is drawn on it.
        let legend = gtk::Box::new(gtk::Orientation::Vertical, 0);
        legend.set_halign(gtk::Align::Start);
        legend.set_valign(gtk::Align::Start);
        legend.set_margin_start(12);
        legend.set_margin_top(6);
        legend.append(&legend_bar);
        legend.append(&this.indicator_legend);

        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&chart.area));
        overlay.add_overlay(&legend);
        split.set_content(Some(&overlay));

        let opener = this.clone();
        gear.connect_clicked(move |_| opener.open_chart_settings());

        let header = this.build_header(&split);
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(split));
        window.set_content(Some(&toolbar));

        this.install_chart_actions();
        this.chart.set_bar_style(*this.bar_style.borrow());
        let menu_owner = this.clone();
        this.chart.set_context_menu_handler(move |x, y| menu_owner.chart_menu(x, y));
        this.wire_shortcuts();
        this.wire_responses(receiver);
        this.wire_theme_polling();

        this.rebuild_indicator_legend();
        this.restore_last_symbol();
        this
    }

    fn build_header(self: &Rc<Self>, split: &adw::OverlaySplitView) -> adw::HeaderBar {
        let header = adw::HeaderBar::new();

        let this = self.clone();
        self.symbol_button.connect_clicked(move |_| this.open_search());
        header.pack_start(&self.symbol_button);
        self.rebuild_timeframes();
        header.set_title_widget(Some(&self.timeframe_strip));

        // Right-clicking the strip offers to edit it, rather than throwing a
        // modal at you for a click you may not have meant.
        let menu = gtk::GestureClick::new();
        menu.set_button(gtk::gdk::BUTTON_SECONDARY);
        let this = self.clone();
        menu.connect_pressed(move |_, _, x, y| {
            let model = gio::Menu::new();
            model.append(Some("Edit resolutions…"), Some("chart.edit-resolutions"));
            popup_menu(&model, &this.timeframe_strip, x, y);
        });
        self.timeframe_strip.add_controller(menu);

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

    /// Fill the resolution strip from the list.
    fn rebuild_timeframes(self: &Rc<Self>) {
        while let Some(child) = self.timeframe_strip.first_child() {
            self.timeframe_strip.remove(&child);
        }
        let current = *self.timeframe.borrow();
        let mut first: Option<gtk::ToggleButton> = None;
        let mut buttons = Vec::new();

        for timeframe in self.timeframes.borrow().iter().copied() {
            let button = gtk::ToggleButton::with_label(&timeframe.label());
            button.add_css_class("flat");
            button.set_tooltip_text(Some(&timeframe.description()));
            match &first {
                Some(anchor) => button.set_group(Some(anchor)),
                None => first = Some(button.clone()),
            }
            button.set_active(timeframe == current);

            let this = self.clone();
            button.connect_toggled(move |button| {
                if button.is_active() {
                    this.set_timeframe(timeframe);
                }
            });
            self.timeframe_strip.append(&button);
            buttons.push((timeframe, button));
        }
        *self.timeframe_buttons.borrow_mut() = buttons;
    }

    /// Put a resolution on the strip, in the place its length earns it.
    fn remember_timeframe(self: &Rc<Self>, timeframe: Timeframe) {
        {
            let mut timeframes = self.timeframes.borrow_mut();
            if timeframes.contains(&timeframe) {
                return;
            }
            timeframes.push(timeframe);
            timeframes.sort_by_key(|t| t.seconds());
        }
        self.store
            .set_setting(SETTING_TIMEFRAMES, &format_timeframes(&self.timeframes.borrow()));
        self.rebuild_timeframes();
    }

    /// The modal for editing the strip.
    fn edit_timeframes(self: &Rc<Self>) {
        let page = adw::PreferencesPage::new();
        let group = adw::PreferencesGroup::new();
        group.set_title("Resolutions");
        group.set_description(Some(
            "What the strip offers. Typing a resolution on the chart adds it here too.",
        ));

        let rebuild: Rc<RefCell<Option<Box<dyn Fn()>>>> = Rc::new(RefCell::new(None));

        let entry = gtk::Entry::new();
        entry.set_placeholder_text(Some("3m, 45m, 2h, 1D"));
        entry.set_valign(gtk::Align::Center);
        let add_row = adw::ActionRow::new();
        add_row.set_title("Add");
        add_row.add_suffix(&entry);
        group.add(&add_row);

        let rows: Rc<RefCell<Vec<adw::ActionRow>>> = Rc::new(RefCell::new(Vec::new()));
        let fill = {
            let this = self.clone();
            let group = group.clone();
            let rows = rows.clone();
            let rebuild = rebuild.clone();
            move || {
                for row in rows.borrow_mut().drain(..) {
                    group.remove(&row);
                }
                let listed: Vec<Timeframe> = this.timeframes.borrow().clone();
                for timeframe in listed {
                    let row = adw::ActionRow::new();
                    row.set_title(&timeframe.label());
                    row.set_subtitle(&timeframe.description());

                    let remove = gtk::Button::from_icon_name("user-trash-symbolic");
                    remove.add_css_class("flat");
                    remove.set_valign(gtk::Align::Center);
                    // The last one cannot go: a strip with nothing on it is a
                    // chart with no way back to a resolution.
                    remove.set_sensitive(this.timeframes.borrow().len() > 1);

                    let this = this.clone();
                    let rebuild = rebuild.clone();
                    remove.connect_clicked(move |_| {
                        this.timeframes.borrow_mut().retain(|t| *t != timeframe);
                        this.store.set_setting(
                            SETTING_TIMEFRAMES,
                            &format_timeframes(&this.timeframes.borrow()),
                        );
                        this.rebuild_timeframes();
                        if let Some(rebuild) = rebuild.borrow().as_ref() {
                            rebuild();
                        }
                    });
                    row.add_suffix(&remove);
                    group.add(&row);
                    rows.borrow_mut().push(row);
                }
            }
        };
        fill();
        *rebuild.borrow_mut() = Some(Box::new(fill));

        let this = self.clone();
        let rebuild_on_add = rebuild.clone();
        entry.connect_activate(move |entry| {
            let Some(timeframe) = Timeframe::parse(&entry.text()) else { return };
            this.remember_timeframe(timeframe);
            entry.set_text("");
            if let Some(rebuild) = rebuild_on_add.borrow().as_ref() {
                rebuild();
            }
        });

        page.add(&group);
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&adw::HeaderBar::new());
        toolbar.set_content(Some(&page));

        let dialog = adw::Dialog::new();
        dialog.set_title("Resolutions");
        dialog.set_content_width(420);
        dialog.set_content_height(520);
        dialog.set_child(Some(&toolbar));
        dialog.present(Some(&self.window));
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
            let shift = state.contains(gtk::gdk::ModifierType::SHIFT_MASK);

            match key {
                // Find a symbol. Ctrl+K is what modern apps use; Ctrl+F is
                // what this desktop uses for find.
                Key::k | Key::f if ctrl => {
                    this.open_search();
                    return glib::Propagation::Stop;
                }
                // One key for the rail: open it, focus it, then close it.
                Key::b if ctrl => {
                    this.toggle_watchlist();
                    return glib::Propagation::Stop;
                }
                // Back to the chart, from wherever the keyboard ended up —
                // but a dialog gets Escape first. Closing what is open is what
                // Escape means, and this controller sits on the window, above
                // everything presented into it.
                Key::Escape => {
                    if this.window.visible_dialog().is_some() {
                        return glib::Propagation::Proceed;
                    }
                    this.chart.area.grab_focus();
                    return glib::Propagation::Stop;
                }
                // Ctrl+I goes to the indicators; Ctrl+Shift+, to the chart's
                // settings, pairing with Ctrl+, for the app's.
                Key::i | Key::I if ctrl => {
                    this.open_indicators();
                    return glib::Propagation::Stop;
                }
                Key::less | Key::comma if ctrl && shift => {
                    this.open_chart_settings();
                    return glib::Propagation::Stop;
                }
                Key::comma if ctrl => {
                    this.open_preferences();
                    return glib::Propagation::Stop;
                }
                // Bare "?" as well as Ctrl+?, because it is what people try
                // first and it collides with nothing: the type-to-search path
                // only takes letters and digits.
                Key::question => {
                    this.show_shortcuts();
                    return glib::Propagation::Stop;
                }
                // Close and quit are the same thing while there is one
                // window, but both keys exist because people reach for both.
                Key::w | Key::q if ctrl => {
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
        // Indicators without a colour of their own take one from the theme's
        // palette, and that answer just changed. Repainting without asking
        // again leaves them wearing the old theme's colours.
        self.rebuild_indicator_legend();
        self.redraw_current();
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

        // The window's key controller would otherwise take Escape to move
        // focus back to the chart, leaving this open behind it.
        let popover_weak = popover.downgrade();
        let escape = gtk::EventControllerKey::new();
        escape.connect_key_pressed(move |_, key, _, _| {
            if key == gtk::gdk::Key::Escape {
                if let Some(popover) = popover_weak.upgrade() {
                    popover.popdown();
                }
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        entry.add_controller(escape);

        popover.popup();
        entry.grab_focus();
        entry.set_position(-1);
    }

    pub fn open_chart_settings(self: &Rc<Self>) {
        crate::ui::chart_settings::ChartSettings::present(self, self.store.clone());
    }

    pub fn open_indicators(self: &Rc<Self>) {
        crate::ui::chart_settings::ChartSettings::present_indicators(self, self.store.clone());
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

    /// The shortcuts, grouped and aligned.
    ///
    /// A list of rows rather than a block of text: an alert dialog centres
    /// whatever it is given, which turns two columns into a ragged mess.
    fn show_shortcuts(self: &Rc<Self>) {
        let page = adw::PreferencesPage::new();

        let sections: [(&str, &[(&str, &str)]); 3] = [
            (
                "Finding things",
                &[
                    ("Type a letter", "Find a symbol"),
                    ("Type a number", "Set the resolution"),
                    ("Ctrl+K", "Find a symbol"),
                    ("Ctrl+I", "Indicators"),
                    ("Ctrl+Shift+,", "Chart settings"),
                    ("Ctrl+,", "Preferences"),
                    ("? · Ctrl+?", "This list"),
                    ("Ctrl+W · Ctrl+Q", "Close"),
                ],
            ),
            (
                "Watchlist",
                &[
                    ("Ctrl+B", "Open, focus, then close"),
                    ("↑ ↓", "Next or previous symbol"),
                    ("Ctrl+↑ ↓", "Next or previous section"),
                    ("Delete", "Remove the symbol"),
                ],
            ),
            (
                "Chart",
                &[
                    ("← →", "Pan"),
                    ("+ −", "Zoom"),
                    ("End", "Jump to the latest bar"),
                    ("Alt+R", "Reset the view"),
                    ("Esc", "Back to the chart"),
                ],
            ),
        ];

        for (title, shortcuts) in sections {
            let group = adw::PreferencesGroup::new();
            group.set_title(title);
            for (keys, what) in shortcuts {
                let row = adw::ActionRow::new();
                row.set_title(what);
                let label = gtk::Label::new(Some(keys));
                label.add_css_class("keycap");
                label.set_valign(gtk::Align::Center);
                row.add_suffix(&label);
                group.add(&row);
            }
            page.add(&group);
        }

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&adw::HeaderBar::new());
        toolbar.set_content(Some(&page));

        let dialog = adw::Dialog::new();
        dialog.set_title("Keyboard shortcuts");
        dialog.set_content_width(480);
        dialog.set_content_height(620);
        dialog.set_child(Some(&toolbar));
        dialog.present(Some(&self.window));
    }

    /// Switch resolution from somewhere other than the strip, keeping the
    /// strip's buttons honest about what is being shown.
    fn apply_timeframe(self: &Rc<Self>, timeframe: Timeframe) {
        // A resolution you typed belongs on the strip: you asked for it once,
        // you will ask for it again.
        self.remember_timeframe(timeframe);
        self.set_timeframe(timeframe);
        for (listed, button) in self.timeframe_buttons.borrow().iter() {
            button.set_active(*listed == timeframe);
        }
    }

    fn set_timeframe(self: &Rc<Self>, timeframe: Timeframe) {
        *self.timeframe.borrow_mut() = timeframe;
        self.store.set_setting(LAST_TIMEFRAME, &timeframe.key());
        // A promoted reset period changes with the resolution, so the legend
        // has to be rewritten when the resolution does.
        self.rebuild_indicator_legend();
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
                // Indicators have to be recomputed here too. Skipping it left
                // the previous symbol's VWAP on screen until the network reply
                // arrived and forced a repaint — which looked like a slow
                // indicator and was a missing call.
                self.recompute_indicators(&instrument, timeframe, &bars);
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
        self.rebuild_indicator_legend();
        self.redraw_current();
    }

    /// The indicator rows under the legend: a dot, a name, and the two things
    /// you reach for without opening anything — hide it, or go to its settings.
    ///
    /// Deliberately faint. These sit over the drawing, and the drawing is the
    /// point; they come up to full strength when the pointer is near.
    fn rebuild_indicator_legend(self: &Rc<Self>) {
        while let Some(child) = self.indicator_legend.first_child() {
            self.indicator_legend.remove(&child);
        }
        let theme = self.theming.borrow().theme();

        let timeframe = *self.timeframe.borrow();
        for (slot, indicator) in self.indicators.borrow().iter().enumerate() {
            let id = indicator.id;
            let colour = indicator.color(&theme, slot);

            let dot = gtk::DrawingArea::new();
            dot.set_size_request(8, 8);
            dot.set_valign(gtk::Align::Center);
            let dot_colour = colour.clone();
            dot.set_draw_func(move |_, cr, width, height| {
                colors::set_source(cr, &dot_colour);
                let radius = (width.min(height) as f64) / 2.0;
                cr.arc(
                    width as f64 / 2.0,
                    height as f64 / 2.0,
                    radius,
                    0.0,
                    std::f64::consts::TAU,
                );
                let _ = cr.fill();
            });

            let label = gtk::Label::new(Some(&indicator.label_for(timeframe)));
            label.add_css_class("legend-indicator");
            label.set_xalign(0.0);
            if !indicator.visible {
                label.add_css_class("legend-indicator-hidden");
            }

            let toggle = gtk::Button::from_icon_name(if indicator.visible {
                "view-reveal-symbolic"
            } else {
                "view-conceal-symbolic"
            });
            toggle.add_css_class("flat");
            toggle.add_css_class("legend-button");
            toggle.set_tooltip_text(Some(if indicator.visible { "Hide" } else { "Show" }));
            let this = self.clone();
            toggle.connect_clicked(move |_| {
                let mut indicators = this.indicators();
                if let Some(found) = indicators.iter_mut().find(|i| i.id == id) {
                    found.visible = !found.visible;
                }
                this.set_indicators(indicators);
            });

            let settings = gtk::Button::from_icon_name("emblem-system-symbolic");
            settings.add_css_class("flat");
            settings.add_css_class("legend-button");
            settings.set_tooltip_text(Some("Settings"));
            let this = self.clone();
            settings.connect_clicked(move |_| {
                crate::ui::chart_settings::ChartSettings::present_indicator(
                    &this,
                    this.store.clone(),
                    id,
                );
            });

            let row = gtk::Box::new(gtk::Orientation::Horizontal, 5);
            row.add_css_class("legend-row");
            row.append(&dot);
            row.append(&label);
            row.append(&toggle);
            row.append(&settings);
            self.indicator_legend.append(&row);
        }
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
        let menu = gio::Menu::new();

        let bars = gio::Menu::new();
        for style in BarStyle::ALL {
            let item = gio::MenuItem::new(Some(style.label()), None);
            item.set_action_and_target_value(
                Some("chart.bar-style"),
                Some(&style.key().to_variant()),
            );
            bars.append_item(&item);
        }
        menu.append_section(None, &bars);

        let sessions = gio::Menu::new();
        for session in Session::ALL {
            let item = gio::MenuItem::new(Some(session.label()), None);
            item.set_action_and_target_value(
                Some("chart.session"),
                Some(&session.key().to_variant()),
            );
            sessions.append_item(&item);
        }
        menu.append_section(None, &sessions);

        let rest = gio::Menu::new();
        rest.append(Some("Indicators…"), Some("chart.indicators"));
        rest.append(Some("Chart settings…"), Some("chart.settings"));
        menu.append_section(None, &rest);

        popup_menu(&menu, &self.chart.area, x, y);
    }

    /// The actions the chart's menus drive.
    ///
    /// Stateful actions rather than plain ones, so the menu draws the current
    /// choice as a selected radio item without anybody building ticks by hand.
    fn install_chart_actions(self: &Rc<Self>) {
        let actions = gio::SimpleActionGroup::new();

        let bar_style = gio::SimpleAction::new_stateful(
            "bar-style",
            Some(glib::VariantTy::STRING),
            &self.bar_style().key().to_variant(),
        );
        let this = self.clone();
        bar_style.connect_activate(move |action, value| {
            let Some(key) = value.and_then(|v| v.str().map(str::to_string)) else { return };
            let Some(style) = BarStyle::from_key(&key) else { return };
            action.set_state(&key.to_variant());
            this.set_bar_style(style);
        });
        actions.add_action(&bar_style);

        let session = gio::SimpleAction::new_stateful(
            "session",
            Some(glib::VariantTy::STRING),
            &self.session.borrow().key().to_variant(),
        );
        let this = self.clone();
        session.connect_activate(move |action, value| {
            let Some(key) = value.and_then(|v| v.str().map(str::to_string)) else { return };
            let Some(chosen) = Session::from_key(&key) else { return };
            action.set_state(&key.to_variant());
            *this.session.borrow_mut() = chosen;
            this.store
                .set_setting(crate::ui::chart_settings::SETTING_SESSION, chosen.key());
            this.redraw_current();
        });
        actions.add_action(&session);

        let settings = gio::SimpleAction::new("settings", None);
        let this = self.clone();
        settings.connect_activate(move |_, _| this.open_chart_settings());
        actions.add_action(&settings);

        let indicators = gio::SimpleAction::new("indicators", None);
        let this = self.clone();
        indicators.connect_activate(move |_, _| this.open_indicators());
        actions.add_action(&indicators);

        let resolutions = gio::SimpleAction::new("edit-resolutions", None);
        let this = self.clone();
        resolutions.connect_activate(move |_, _| this.edit_timeframes());
        actions.add_action(&resolutions);

        self.window.insert_action_group("chart", Some(&actions));
    }

    /// One key for the whole watchlist, doing the obvious next thing.
    ///
    /// Closed, open it and put the keyboard on it — the reason to open a
    /// watchlist is almost always to move through it. Open but not focused,
    /// focus it. Open and focused, you are done with it, so close it.
    pub fn toggle_watchlist(self: &Rc<Self>) {
        let focused = self
            .watchlist
            .borrow()
            .as_ref()
            .map(|w| w.has_focus())
            .unwrap_or(false);

        match watchlist_action(self.split.shows_sidebar(), focused) {
            WatchlistAction::Open => {
                self.split.set_show_sidebar(true);
                self.store.set_setting_bool(SHOW_WATCHLIST, true);
                if let Some(watchlist) = self.watchlist.borrow().as_ref().cloned() {
                    // The sidebar is not realised until the frame after it is
                    // revealed, so focus has to wait for it.
                    glib::idle_add_local_once(move || watchlist.grab_focus());
                }
            }
            WatchlistAction::Focus => {
                if let Some(watchlist) = self.watchlist.borrow().as_ref() {
                    watchlist.grab_focus();
                }
            }
            WatchlistAction::Close => {
                self.split.set_show_sidebar(false);
                self.store.set_setting_bool(SHOW_WATCHLIST, false);
                self.chart.area.grab_focus();
            }
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
