//! The main window.
//!
//! A chart, a watchlist that can be hidden outright, and the window's own
//! controls floating over its top-right corner. The chart is the main
//! character: there is no header bar, and nothing but the drawing takes room.
//!
//! Nothing here waits on the network. Opening a symbol paints whatever the
//! cache holds immediately, asks a worker for the gap, and repaints when the
//! answer arrives.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use omacharts_engine::providers::Yahoo;
use omacharts_engine::{
    resample, BarStyle, Indicator, Instrument, Provider, Session, Timeframe,
};

use crate::loader::{Loader, Request, Response, BACKGROUND, FOREGROUND};
use crate::store::Store;
use crate::theming::Theming;
use crate::ui::chart::Drawn;
use crate::ui::pane::{ChartPane, Node};
use crate::ui::colors;
use crate::ui::preferences::Preferences;
use crate::ui::search::SymbolSearch;
use crate::ui::shortcuts;
use crate::ui::watchlist::{Quote, Watchlist, DEFAULTS};
use gtk::gio;

/// How often we look for a desktop theme change. Cheap enough to be invisible,
/// often enough to feel immediate.
const THEME_POLL_SECONDS: u32 = 2;

/// Read the stored resolution strip.
///
/// A strip that was never set gets the presets; one that was set to nothing
/// stays set to nothing. Clearing it is a choice — typing a resolution works
/// whether or not it is on the strip — and a list that refills itself the next
/// time the app starts is a setting that does not hold.
fn parse_timeframes(stored: Option<&str>) -> Vec<Timeframe> {
    let Some(stored) = stored else {
        return Timeframe::PRESETS.to_vec();
    };
    let mut listed: Vec<Timeframe> =
        stored.split(',').filter_map(|k| Timeframe::parse(k.trim())).collect();
    listed.sort_by_key(|t| t.seconds());
    listed.dedup();
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
    fn a_strip_nobody_has_set_falls_back_to_the_presets() {
        assert_eq!(parse_timeframes(None), Timeframe::PRESETS.to_vec());
    }

    /// Emptying the strip is allowed and has to stick. Typing a resolution
    /// works whether or not it is listed, so a chart with no strip at all is a
    /// usable chart — and a list that refilled itself on the next launch would
    /// be a setting that does not hold.
    #[test]
    fn a_strip_emptied_on_purpose_stays_empty() {
        assert!(parse_timeframes(Some("")).is_empty());
        assert!(parse_timeframes(Some("banana,,")).is_empty());
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
///
/// Nested rather than sliding, which is what fixes the height. Sliding
/// submenus live in one stack, and the stack is as tall as its tallest page —
/// so a four-item menu whose submenu has more rows than that opens with dead
/// space below it and a scrollbar down the side. Nested submenus fly out as
/// their own popovers, leaving each menu exactly as tall as what is in it.
fn popup_menu(model: &gio::Menu, over: &impl IsA<gtk::Widget>, x: f64, y: f64) {
    let popover = gtk::PopoverMenu::from_model_full(model, gtk::PopoverMenuFlags::NESTED);
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
    // Popped from an idle, not here. GtkPopoverMenu inserts its section
    // separators from an idle after this callback returns, and a popup surface
    // is sized once, when it is first shown — GTK never re-presents a mapped
    // popover for a resize that started inside it. Showing it now measures a
    // menu two separators short of itself, and the scroller inside every
    // GtkPopoverMenu absorbs the difference by scrolling, which is why the last
    // item sat under the bottom corner. The separator sync runs at a higher
    // idle priority than this one, so by the time it pops it is whole.
    glib::idle_add_local_once(move || popover.popup());
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
/// How wide the rail was left, in pixels.
const SETTING_SIDEBAR_WIDTH: &str = "sidebar_width";
/// Whether the bar widget has been put in the bar once already.
const SETTING_WIDGET_OFFERED: &str = "bar_widget_offered";
const DEFAULT_SIDEBAR_WIDTH: i32 = 280;
pub const SETTING_BAR_STYLE: &str = "bar_style";
pub const SETTING_SHOW_GRID: &str = "show_grid";
const SETTING_TIMEFRAMES: &str = "timeframes";
/// The whole arrangement of charts, as one stored value.
const SETTING_WORKSPACE: &str = "workspace";
/// Whether a pointer on one chart draws a line on the linked ones.
pub const SETTING_SYNC_CROSSHAIR: &str = "sync_crosshair";

/// One chart, as it is written down.
///
/// Stored together with the layout rather than as separate settings, because
/// they only mean anything together: a tree of pane ids and a list of panes
/// that disagree is a window that cannot be rebuilt.
#[derive(serde::Serialize, serde::Deserialize)]
struct StoredPane {
    id: u32,
    symbol: String,
    #[serde(default)]
    suffix: Option<String>,
    timeframe: String,
    indicators: Vec<Indicator>,
    bar_style: String,
    session: String,
    show_grid: bool,
    linked: bool,
}

/// Point a leaf at a different pane id, leaving the shape alone.
fn rename_leaf(node: &Node, from: u32, to: u32) -> Node {
    match node {
        Node::Leaf(id) if *id == from => Node::Leaf(to),
        Node::Leaf(id) => Node::Leaf(*id),
        Node::Split { horizontal, ratio, first, second } => Node::Split {
            horizontal: *horizontal,
            ratio: *ratio,
            first: Box::new(rename_leaf(first, from, to)),
            second: Box::new(rename_leaf(second, from, to)),
        },
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Workspace {
    layout: Node,
    focused: u32,
    panes: Vec<StoredPane>,
}

pub struct Window {
    pub window: adw::ApplicationWindow,
    /// Every chart on screen. One of them is focused, and that is the one the
    /// keyboard, the menus and the symbol search act on.
    panes: RefCell<Vec<Rc<ChartPane>>>,
    /// How they are arranged. Splitting divides the focused pane; closing one
    /// hands its space to its sibling.
    layout: RefCell<Node>,
    focused: Cell<u32>,
    next_pane: Cell<u32>,
    /// A save is already queued, so a drag does not queue one per pixel.
    save_pending: Cell<bool>,
    /// The chart menu's stateful actions, so they can be re-pointed at
    /// whichever chart is focused.
    linked_action: RefCell<Option<gio::SimpleAction>>,
    bar_style_action: RefCell<Option<gio::SimpleAction>>,
    session_action: RefCell<Option<gio::SimpleAction>>,
    auto_scale_action: RefCell<Option<gio::SimpleAction>>,
    /// Where the tree of charts is mounted, rebuilt whenever it changes.
    chart_host: gtk::Box,
    store: Rc<Store>,
    index: crate::inventory::Inventory,
    theming: Rc<RefCell<Theming>>,
    search: Rc<SymbolSearch>,
    watchlist: RefCell<Option<Rc<Watchlist>>>,
    provider: Rc<Yahoo>,
    loader: Loader,
    /// Folded series, keyed by cache key and the resolution shown.
    ///
    /// The bars are the chart — there is nothing else to preload — so once a
    /// symbol has been fetched, switching to it is a read and a draw. This
    /// removes even the read, which is what makes arrowing back and forth over
    /// the same few symbols feel like nothing is happening at all.
    series: Rc<RefCell<HashMap<(String, Timeframe), Rc<Vec<omacharts_engine::Bar>>>>>,
    /// The split, so the keyboard can show and hide the rail.
    ///
    /// A GtkPaned rather than an AdwOverlaySplitView: the rail is a table of
    /// numbers whose useful width depends on how many columns you have shown,
    /// and a split view has no handle to drag. Hiding it is hiding the child,
    /// which takes the handle with it.
    split: gtk::Paned,
    /// The resolution strip and what is on it.
    timeframes: RefCell<Vec<Timeframe>>,
}

impl Window {
    pub fn build(app: &adw::Application, store: Rc<Store>) -> Rc<Window> {
        store.seed_watchlist_if_empty(DEFAULTS);

        let index = crate::inventory::Inventory::curated();
        let theming = Rc::new(RefCell::new(Theming::load(&store)));
        theming.borrow_mut().apply();

        let (sender, receiver) = async_channel::unbounded::<Response>();
        let loader = Loader::new(Yahoo::new(), sender.clone());

        let window = adw::ApplicationWindow::new(app);
        // Just the name. Which symbol you are looking at is written over each
        // chart, and with several of them the title could only ever name one.
        window.set_title(Some("Omacharts"));
        window.set_default_size(1280, 800);

        let split = gtk::Paned::new(gtk::Orientation::Horizontal);
        let chart_host = gtk::Box::new(gtk::Orientation::Vertical, 0);
        chart_host.set_hexpand(true);
        chart_host.set_vexpand(true);

        let this = Rc::new(Window {
            window: window.clone(),
            panes: RefCell::new(Vec::new()),
            layout: RefCell::new(Node::leaf(1)),
            focused: Cell::new(1),
            next_pane: Cell::new(1),
            save_pending: Cell::new(false),
            linked_action: RefCell::new(None),
            bar_style_action: RefCell::new(None),
            session_action: RefCell::new(None),
            auto_scale_action: RefCell::new(None),
            chart_host: chart_host.clone(),
            store: store.clone(),
            index: index.clone(),
            theming: theming.clone(),
            search: SymbolSearch::new(index.clone()),
            watchlist: RefCell::new(None),
            provider: Rc::new(Yahoo::new()),
            loader,
            series: Rc::new(RefCell::new(HashMap::new())),
            split: split.clone(),
            timeframes: RefCell::new(parse_timeframes(
                store.setting(SETTING_TIMEFRAMES).as_deref(),
            )),
        });

        let watchlist = this.build_watchlist();
        *this.watchlist.borrow_mut() = Some(watchlist.clone());

        let split = &this.split;
        split.set_end_child(Some(&watchlist.widget));
        // The chart takes the room a wider window gives; the rail keeps the
        // width it was left at, because its columns do not get more useful.
        split.set_resize_start_child(true);
        split.set_resize_end_child(false);
        split.set_shrink_end_child(false);
        watchlist.widget.set_visible(store.setting_bool(SHOW_WATCHLIST, true));

        split.set_start_child(Some(&chart_host));

        // No header bar: the window's controls float over its top-right
        // corner, and the chart gets the row the bar used to take.
        let corner = this.build_corner(&this.split);
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(split));
        overlay.add_overlay(&corner);
        this.restore_sidebar_width();
        window.set_content(Some(&overlay));

        // The first chart. Everything else is a split of this one.
        let first = this.new_pane(
            store
                .setting(LAST_TIMEFRAME)
                .and_then(|k| Timeframe::parse(&k))
                .unwrap_or(Timeframe::days(1)),
            store.indicators(),
            store
                .setting(SETTING_BAR_STYLE)
                .and_then(|k| BarStyle::from_key(&k))
                .unwrap_or_default(),
            store
                .setting(crate::ui::chart_settings::SETTING_SESSION)
                .and_then(|k| Session::from_key(&k))
                .unwrap_or_default(),
            store.setting_bool(SETTING_SHOW_GRID, true),
            true,
        );
        *this.layout.borrow_mut() = Node::leaf(first.id);
        this.focused.set(first.id);
        this.rebuild_layout();
        // After the first pane: the chart menu's radio items are built from
        // the focused chart, and until now there was not one.
        this.install_chart_actions();

        this.wire_shortcuts();
        this.wire_responses(receiver);
        this.wire_theme_polling();

        // The widget is part of the app, so it arrives with it rather than
        // waiting to be discovered in the settings.
        let installer = this.clone();
        glib::idle_add_local_once(move || installer.offer_bar_widget());

        // The long tail of listings arrives on a thread once the window is up:
        // the names you are most likely to type are already in the curated half.
        crate::inventory::load_in_background(this.index.clone(), |_| {});

        if !this.restore_workspace() {
            this.rebuild_indicator_legend();
            this.restore_last_symbol();
        }
        this
    }

    // -- panes -------------------------------------------------------------

    /// Build a chart and wire it to this window. Not placed in the layout:
    /// the caller decides where it goes.
    #[allow(clippy::too_many_arguments)]
    fn new_pane(
        self: &Rc<Self>,
        timeframe: Timeframe,
        indicators: Vec<Indicator>,
        bar_style: BarStyle,
        session: Session,
        show_grid: bool,
        linked: bool,
    ) -> Rc<ChartPane> {
        let id = self.next_pane.get();
        self.next_pane.set(id + 1);

        let (theme, scheme) = {
            let t = self.theming.borrow();
            (t.theme(), t.bar_scheme())
        };
        let pane = ChartPane::new(
            id, theme, scheme, timeframe, indicators, bar_style, session, show_grid, linked,
        );
        pane.view.set_bar_style(bar_style);
        pane.view.set_show_grid(show_grid);

        let searcher = self.clone();
        pane.symbol_button.connect_clicked(move |_| {
            searcher.focus(id);
            searcher.open_search();
        });

        let opener = self.clone();
        pane.gear.connect_clicked(move |_| {
            opener.focus(id);
            opener.open_chart_settings();
        });

        let linker = self.clone();
        pane.link.connect_toggled(move |toggle| {
            linker.set_pane_linked(id, toggle.is_active());
        });

        // Right-clicking a strip offers to edit the list, rather than throwing
        // a modal at you for a click you may not have meant. The list is one
        // list for the window: every chart offers the same resolutions, and
        // picks its own from them.
        let editor = self.clone();
        let strip_menu = gtk::GestureClick::new();
        strip_menu.set_button(gtk::gdk::BUTTON_SECONDARY);
        let strip = pane.strip.clone();
        strip_menu.connect_pressed(move |_, _, x, y| {
            editor.focus(id);
            let model = gio::Menu::new();
            model.append(Some("Edit resolutions…"), Some("chart.edit-resolutions"));
            popup_menu(&model, &strip, x, y);
        });
        pane.strip.add_controller(strip_menu);

        let axis_owner = self.clone();
        pane.view.set_axis_menu_handler(move |x, y| {
            axis_owner.focus(id);
            axis_owner.price_axis_menu(x, y);
        });

        let menu_owner = self.clone();
        pane.view.set_context_menu_handler(move |x, y| {
            menu_owner.focus(id);
            menu_owner.chart_menu(x, y);
        });

        // Dragging a pane's edge changes the indicator, not just the drawing,
        // so the new height is stored the way any other setting of its would be.
        let resizer = self.clone();
        pane.view.set_pane_resize_handler(move |indicator_id, share| {
            let Some(pane) = resizer.pane(id) else { return };
            let mut indicators = pane.indicators.borrow().clone();
            if let Some(indicator) = indicators.iter_mut().find(|i| i.id == indicator_id) {
                match &mut indicator.params {
                    omacharts_engine::Params::Volume { height }
                    | omacharts_engine::Params::Rsi { height, .. }
                    | omacharts_engine::Params::Atr { height, .. } => *height = share,
                    _ => return,
                }
                resizer.set_indicators_of(&pane, indicators);
            }
        });

        // The × in a strip's corner takes that indicator off this chart.
        let closer = self.clone();
        pane.view.set_pane_close_handler(move |indicator_id| {
            let Some(pane) = closer.pane(id) else { return };
            let kept: Vec<Indicator> = pane
                .indicators
                .borrow()
                .iter()
                .filter(|i| i.id != indicator_id)
                .cloned()
                .collect();
            closer.set_indicators_of(&pane, kept);
        });

        // The pointer on one chart draws a line on the charts linked with it,
        // so you can read the same moment on all of them at once. Only the
        // time travels: two charts at different resolutions share no bar
        // index, and two symbols share no price.
        let echoer = self.clone();
        pane.view.set_hover_handler(move |hover| {
            echoer.echo_crosshair(id, hover.map(|h| h.bar.ts));
        });

        // Clicking anywhere on a chart focuses it, which is what makes the
        // next keystroke land where you are looking.
        let focuser = self.clone();
        let click = gtk::GestureClick::new();
        click.set_button(0);
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        click.connect_pressed(move |_, _, _, _| focuser.focus(id));
        pane.root.add_controller(click);

        self.panes.borrow_mut().push(pane.clone());
        self.rebuild_strip_of(&pane);
        pane
    }

    pub fn focused_pane(&self) -> Rc<ChartPane> {
        let id = self.focused.get();
        let panes = self.panes.borrow();
        panes
            .iter()
            .find(|p| p.id == id)
            .or_else(|| panes.first())
            .cloned()
            .expect("at least one chart")
    }

    fn pane(&self, id: u32) -> Option<Rc<ChartPane>> {
        self.panes.borrow().iter().find(|p| p.id == id).cloned()
    }

    /// Make `id` the chart the keyboard and the menus act on.
    pub fn focus(self: &Rc<Self>, id: u32) {
        if self.focused.get() == id && self.panes.borrow().iter().any(|p| p.id == id) {
            return;
        }
        self.focused.set(id);
        for pane in self.panes.borrow().iter() {
            pane.set_focused(pane.id == id);
        }
        // What is left in the header describes the focused chart, as do the
        // chart menu's radio items.
        self.sync_header();
    }

    /// The chart menu's radio items describe the focused chart, so they have
    /// to be re-pointed when the focus moves.
    fn sync_chart_actions(self: &Rc<Self>, pane: &Rc<ChartPane>) {
        if let Some(action) = self.linked_action.borrow().as_ref() {
            action.set_state(&pane.linked.get().to_variant());
        }
        if let Some(action) = self.bar_style_action.borrow().as_ref() {
            action.set_state(&pane.bar_style.get().key().to_variant());
        }
        if let Some(action) = self.session_action.borrow().as_ref() {
            action.set_state(&pane.session.get().key().to_variant());
        }
    }

    /// Point the header and the rail at the focused chart.
    fn sync_header(self: &Rc<Self>) {
        let pane = self.focused_pane();
        self.sync_timeframe_buttons();
        self.sync_chart_actions(&pane);
        if let (Some(watchlist), Some(instrument)) =
            (self.watchlist.borrow().as_ref(), pane.instrument.borrow().as_ref())
        {
            watchlist.highlight(instrument);
        }
    }

    /// Divide the focused chart in two, the new one showing the same thing.
    ///
    /// Copying the chart you split is what makes splitting useful: you get two
    /// of what you were looking at and change one of them.
    pub fn split_focused(self: &Rc<Self>, horizontal: bool) {
        let from = self.focused_pane();
        let added = self.new_pane(
            from.timeframe.get(),
            from.indicators.borrow().clone(),
            from.bar_style.get(),
            from.session.get(),
            from.show_grid.get(),
            from.linked.get(),
        );
        let next = self.layout.borrow().split(from.id, added.id, horizontal);
        *self.layout.borrow_mut() = next;
        self.rebuild_layout();
        if let Some(instrument) = from.instrument.borrow().clone() {
            self.show_in(&added, instrument);
        }
        self.focus(added.id);
        self.save_workspace();
    }

    /// Close the focused chart. The last one stays: a window with no chart in
    /// it is not a state worth being able to reach.
    pub fn close_focused(self: &Rc<Self>) {
        let id = self.focused.get();
        let Some(next) = self.layout.borrow().remove(id) else { return };
        let leaves = next.leaves();
        *self.layout.borrow_mut() = next;
        self.panes.borrow_mut().retain(|p| p.id != id);
        self.rebuild_layout();
        if let Some(first) = leaves.first() {
            self.focus(*first);
        }
        self.save_workspace();
    }

    /// Walk the focus to the next or previous chart in layout order.
    /// Move the focus to the nearest chart in a direction.
    ///
    /// By where the charts are on screen, not by where they sit in the tree.
    /// Walking the tree means the pane to the right of this one is whichever
    /// leaf happens to come next in a depth-first order — from the top left of
    /// a column beside a tall chart, that is the pane *below*, and pressing
    /// right moved down. Geometry is what the arrow key is asking about.
    pub fn focus_towards(self: &Rc<Self>, dx: i32, dy: i32) {
        let Some(from) = self.pane_rect(self.focused.get()) else { return };
        let (fx, fy) = (from.0 + from.2 / 2.0, from.1 + from.3 / 2.0);

        let mut best: Option<(f64, u32)> = None;
        for pane in self.panes.borrow().iter() {
            if pane.id == self.focused.get() {
                continue;
            }
            let Some(rect) = self.pane_rect(pane.id) else { continue };
            let (cx, cy) = (rect.0 + rect.2 / 2.0, rect.1 + rect.3 / 2.0);
            let (along, across) = if dx != 0 {
                ((cx - fx) * dx as f64, (cy - fy).abs())
            } else {
                ((cy - fy) * dy as f64, (cx - fx).abs())
            };
            // Strictly in the direction asked for, and then the nearest one,
            // with ties broken by how well the two charts line up.
            if along <= 1.0 {
                continue;
            }
            let score = along + across * 2.0;
            if best.is_none_or(|(b, _)| score < b) {
                best = Some((score, pane.id));
            }
        }
        if let Some((_, id)) = best {
            self.focus(id);
        }
    }

    /// Where a chart sits, in the coordinates of the area the charts share.
    fn pane_rect(&self, id: u32) -> Option<(f64, f64, f64, f64)> {
        let pane = self.panes.borrow().iter().find(|p| p.id == id).cloned()?;
        let (x, y) = pane.root.translate_coordinates(&self.chart_host, 0.0, 0.0)?;
        Some((x, y, pane.root.width() as f64, pane.root.height() as f64))
    }

    /// Mount the tree. Rebuilt whole rather than patched: a handful of panes,
    /// and a tree that is half old and half new is a bug nobody can see.
    fn rebuild_layout(self: &Rc<Self>) {
        while let Some(child) = self.chart_host.first_child() {
            self.chart_host.remove(&child);
        }
        for pane in self.panes.borrow().iter() {
            if let Some(parent) = pane.root.parent() {
                if let Some(paned) = parent.downcast_ref::<gtk::Paned>() {
                    if paned.start_child().as_ref() == Some(pane.root.upcast_ref()) {
                        paned.set_start_child(None::<&gtk::Widget>);
                    } else {
                        paned.set_end_child(None::<&gtk::Widget>);
                    }
                }
            }
        }
        let layout = self.layout.borrow().clone();
        let widget = self.build_node(&layout, &[]);
        self.chart_host.append(&widget);
        let focused = self.focused.get();
        for pane in self.panes.borrow().iter() {
            pane.set_focused(pane.id == focused);
        }
    }

    fn build_node(self: &Rc<Self>, node: &Node, path: &[bool]) -> gtk::Widget {
        match node {
            Node::Leaf(id) => match self.pane(*id) {
                Some(pane) => pane.root.clone().upcast(),
                None => gtk::Box::new(gtk::Orientation::Vertical, 0).upcast(),
            },
            Node::Split { horizontal, ratio, first, second } => {
                let paned = gtk::Paned::new(if *horizontal {
                    gtk::Orientation::Horizontal
                } else {
                    gtk::Orientation::Vertical
                });
                paned.add_css_class("chart-split");
                let mut down = path.to_vec();
                down.push(true);
                paned.set_start_child(Some(&self.build_node(first, &down)));
                let mut down = path.to_vec();
                down.push(false);
                paned.set_end_child(Some(&self.build_node(second, &down)));
                // Both halves grow with the window, so a split stays the split
                // you made rather than drifting as the window is resized.
                paned.set_resize_start_child(true);
                paned.set_resize_end_child(true);
                paned.set_shrink_start_child(false);
                paned.set_shrink_end_child(false);
                paned.set_wide_handle(true);

                // Put the divider back where it was left, once the split has a
                // size to measure it against — a position set before the first
                // allocation is a position against zero.
                let ratio = ratio.clamp(0.05, 0.95);
                let placed = Rc::new(Cell::new(false));
                let paned_for_place = paned.clone();
                let placed_for_map = placed.clone();
                let place = move || {
                    if placed_for_map.get() {
                        return;
                    }
                    let length = if paned_for_place.orientation() == gtk::Orientation::Horizontal {
                        paned_for_place.width()
                    } else {
                        paned_for_place.height()
                    };
                    if length > 0 {
                        placed_for_map.set(true);
                        paned_for_place.set_position((length as f64 * ratio).round() as i32);
                    }
                };
                let place_on_map = place.clone();
                paned.connect_map(move |_| place_on_map());

                let this = self.clone();
                let here = path.to_vec();
                let placed_for_move = placed.clone();
                paned.connect_position_notify(move |paned| {
                    // Only once it has been put back, or the first notify —
                    // which fires while the split still has no size — would
                    // record a ratio of nothing.
                    if !placed_for_move.get() {
                        place();
                        return;
                    }
                    let length = if paned.orientation() == gtk::Orientation::Horizontal {
                        paned.width()
                    } else {
                        paned.height()
                    };
                    if length > 0 {
                        this.record_ratio(&here, paned.position() as f64 / length as f64);
                    }
                });

                paned.upcast()
            }
        }
    }

    pub fn set_pane_linked(self: &Rc<Self>, id: u32, linked: bool) {
        let Some(pane) = self.pane(id) else { return };
        pane.set_linked(linked);
        // Joining the group adopts what the group is showing, which is what
        // "linked" means — otherwise the toggle says linked and the chart is
        // somewhere else.
        if linked {
            if let Some(instrument) = self.linked_instrument(id) {
                self.show_in(&pane, instrument);
            }
        }
        self.save_workspace();
    }

    /// Remember where a divider was dragged to.
    fn record_ratio(self: &Rc<Self>, path: &[bool], ratio: f64) {
        let next = self.layout.borrow().with_ratio(path, ratio);
        *self.layout.borrow_mut() = next;
        self.save_soon();
    }

    /// Save once the dragging stops.
    ///
    /// A handle being dragged emits a position for every pixel it crosses, and
    /// a database write per pixel is a database write per pixel. The delay is
    /// longer than the gap between two of those and shorter than anyone's
    /// patience.
    fn save_soon(self: &Rc<Self>) {
        if self.save_pending.replace(true) {
            return;
        }
        let this = self.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(400), move || {
            this.save_pending.set(false);
            this.save_workspace();
        });
    }

    /// Write the arrangement down, so a window comes back as it was left.
    fn save_workspace(self: &Rc<Self>) {
        let panes: Vec<StoredPane> = self
            .panes
            .borrow()
            .iter()
            .map(|pane| {
                let instrument = pane.instrument.borrow();
                StoredPane {
                    id: pane.id,
                    symbol: instrument.as_ref().map(|i| i.symbol.clone()).unwrap_or_default(),
                    suffix: instrument.as_ref().and_then(|i| i.suffix.clone()),
                    timeframe: pane.timeframe.get().key(),
                    indicators: pane.indicators.borrow().clone(),
                    bar_style: pane.bar_style.get().key().to_string(),
                    session: pane.session.get().key().to_string(),
                    show_grid: pane.show_grid.get(),
                    linked: pane.linked.get(),
                }
            })
            .collect();
        let workspace = Workspace {
            layout: self.layout.borrow().clone(),
            focused: self.focused.get(),
            panes,
        };
        if let Ok(json) = serde_json::to_string(&workspace) {
            self.store.set_setting(SETTING_WORKSPACE, &json);
        }
    }

    /// Put the charts back. `false` when there was nothing written down, or
    /// when what was written cannot be rebuilt — a layout naming panes that
    /// are not there is worse than starting over with one chart.
    fn restore_workspace(self: &Rc<Self>) -> bool {
        let Some(json) = self.store.setting(SETTING_WORKSPACE) else { return false };
        let Ok(workspace) = serde_json::from_str::<Workspace>(&json) else { return false };
        if workspace.panes.is_empty() {
            return false;
        }
        let wanted = workspace.layout.leaves();
        if wanted.is_empty() || wanted.iter().any(|id| !workspace.panes.iter().any(|p| p.id == *id))
        {
            return false;
        }

        self.panes.borrow_mut().clear();
        self.next_pane.set(1);
        let mut restored: Vec<(Rc<ChartPane>, StoredPane)> = Vec::new();
        for stored in workspace.panes {
            if !wanted.contains(&stored.id) {
                continue;
            }
            let pane = self.new_pane(
                Timeframe::parse(&stored.timeframe).unwrap_or(Timeframe::days(1)),
                stored.indicators.clone(),
                BarStyle::from_key(&stored.bar_style).unwrap_or_default(),
                Session::from_key(&stored.session).unwrap_or_default(),
                stored.show_grid,
                stored.linked,
            );
            restored.push((pane, stored));
        }

        // Ids are handed out afresh in layout order, so the tree is rewritten
        // to match rather than trusting ids from another run to still be free.
        let mut layout = workspace.layout.clone();
        let mut focused = restored.first().map(|(pane, _)| pane.id).unwrap_or(1);
        for (pane, stored) in &restored {
            layout = rename_leaf(&layout, stored.id, pane.id);
            if stored.id == workspace.focused {
                focused = pane.id;
            }
        }
        *self.layout.borrow_mut() = layout;
        self.focused.set(focused);
        self.rebuild_layout();

        for (pane, stored) in &restored {
            if let Some(instrument) = self.index.find(&stored.symbol, stored.suffix.as_deref()) {
                self.show_in(pane, instrument.clone());
            }
        }
        self.sync_header();
        true
    }

    /// Draw `ts` on every chart linked with `from`, and on none when it is
    /// unlinked: an unlinked chart is deliberately somewhere else.
    fn echo_crosshair(self: &Rc<Self>, from: u32, ts: Option<i64>) {
        if !self.store.setting_bool(SETTING_SYNC_CROSSHAIR, true) {
            return;
        }
        let source_linked = self.pane(from).map(|p| p.linked.get()).unwrap_or(false);
        for pane in self.panes.borrow().iter() {
            if pane.id == from {
                continue;
            }
            let show = source_linked && pane.linked.get();
            pane.view.set_echo(if show { ts } else { None });
        }
    }

    /// Stop every echo, for when the setting is switched off or the panes
    /// change under it.
    pub fn clear_echoes(self: &Rc<Self>) {
        for pane in self.panes.borrow().iter() {
            pane.view.set_echo(None);
        }
    }

    /// What the linked charts are showing, ignoring `except`.
    fn linked_instrument(&self, except: u32) -> Option<Instrument> {
        self.panes
            .borrow()
            .iter()
            .filter(|p| p.id != except && p.linked.get())
            .find_map(|p| p.instrument.borrow().clone())
    }

    /// The window's own controls, over its top-right corner: the watchlist
    /// toggle and the main menu.
    ///
    /// There is no header bar. It was a band across the whole window holding
    /// an app name, two buttons and the desktop's close button — which on a
    /// tiling desktop is furniture and everywhere else is Alt+F4 — and it cost
    /// every chart in the top row forty-seven pixels. The cluster sits over
    /// the rail when it is open, which is where the HIG puts a sidebar's menu,
    /// and over the top-right chart when it is not.
    ///
    /// A `WindowHandle`, so the cluster is also what the window is dragged by,
    /// double-clicked to maximise and right-clicked for the window menu —
    /// everything a header bar's blank space did.
    fn build_corner(self: &Rc<Self>, split: &gtk::Paned) -> gtk::WindowHandle {
        let menu = gio::Menu::new();
        shortcuts::append(&menu, "Preferences", "win.preferences");
        shortcuts::append(&menu, "Keyboard Shortcuts", "win.shortcuts");
        let menu_button = gtk::MenuButton::new();
        menu_button.set_icon_name("open-menu-symbolic");
        menu_button.set_menu_model(Some(&menu));
        menu_button.set_tooltip_text(Some("Main Menu"));
        // F10 opens the primary menu, and the header bar used to be the only
        // thing saying this menu was the primary one.
        menu_button.set_primary(true);
        menu_button.add_css_class("flat");

        let toggle = gtk::ToggleButton::new();
        toggle.set_icon_name("sidebar-show-right-symbolic");
        toggle.set_tooltip_text(Some(&shortcuts::tooltip("Watchlist", "win.watchlist")));
        toggle.add_css_class("flat");
        toggle.set_active(split.end_child().map(|rail| rail.is_visible()).unwrap_or(false));
        let split_weak = split.downgrade();
        let store = self.store.clone();
        toggle.connect_toggled(move |toggle| {
            if let Some(rail) = split_weak.upgrade().and_then(|split| split.end_child()) {
                rail.set_visible(toggle.is_active());
            }
            store.set_setting_bool(SHOW_WATCHLIST, toggle.is_active());
        });

        // Ctrl+B needs to drive the button so its pressed state stays honest.
        let action = gio::SimpleAction::new("watchlist", None);
        let toggle_weak = toggle.downgrade();
        action.connect_activate(move |_, _| {
            if let Some(toggle) = toggle_weak.upgrade() {
                toggle.set_active(!toggle.is_active());
            }
        });
        self.window.add_action(&action);

        let cluster = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        cluster.add_css_class("window-corner");
        cluster.append(&toggle);
        cluster.append(&menu_button);

        let handle = gtk::WindowHandle::new();
        handle.set_child(Some(&cluster));
        handle.set_halign(gtk::Align::End);
        handle.set_valign(gtk::Align::Start);
        handle
    }

    /// Fill the resolution strip from the list.
    /// Build every chart's resolution strip. Called when the list of
    /// resolutions changes, which is the only time the buttons differ.
    fn rebuild_timeframes(self: &Rc<Self>) {
        let panes = self.panes.borrow().clone();
        for pane in panes {
            self.rebuild_strip_of(&pane);
        }
    }

    fn rebuild_strip_of(self: &Rc<Self>, pane: &Rc<ChartPane>) {
        while let Some(child) = pane.strip.first_child() {
            pane.strip.remove(&child);
        }
        let current = pane.timeframe.get();
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

            // The strip belongs to its own chart, so clicking one focuses that
            // chart first: changing a resolution is not a reason to leave the
            // keyboard pointing somewhere else.
            let this = self.clone();
            let owner = pane.id;
            button.connect_toggled(move |button| {
                if button.is_active() {
                    this.focus(owner);
                    this.set_timeframe(timeframe);
                }
            });
            pane.strip.append(&button);
            buttons.push((timeframe, button));
        }
        *pane.buttons.borrow_mut() = buttons;
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
                    // Every one of them can go. A chart with no strip is a
                    // usable chart: typing a resolution works whether or not
                    // it is listed.
                    remove.set_sensitive(true);

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
        // Everything the application can own outright, which is also
        // what makes the menus draw the key beside the row.
        if let Some(app) = self.window.application().and_downcast::<adw::Application>() {
            shortcuts::install(&app);
        }

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
                // Ctrl+L: follow the rail, or stop following it.
                Key::l | Key::L if ctrl => {
                    let pane = this.focused_pane();
                    this.set_pane_linked(pane.id, !pane.linked.get());
                    this.sync_chart_actions(&this.focused_pane());
                    return glib::Propagation::Stop;
                }
                Key::Left if alt && !ctrl => {
                    this.focus_towards(-1, 0);
                    return glib::Propagation::Stop;
                }
                Key::Right if alt && !ctrl => {
                    this.focus_towards(1, 0);
                    return glib::Propagation::Stop;
                }
                Key::Up if alt && !ctrl => {
                    this.focus_towards(0, -1);
                    return glib::Propagation::Stop;
                }
                Key::Down if alt && !ctrl => {
                    this.focus_towards(0, 1);
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
                    this.focused_pane().view.area.grab_focus();
                    return glib::Propagation::Stop;
                }
                // Ctrl+Shift+I skips the list and opens the picker, because
                // adding one is what you usually came for. It has to be tested
                // before plain Ctrl+I or that arm swallows it.
                Key::i | Key::I if ctrl && shift => {
                    this.add_indicator();
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
                // Walk the chart without leaving it: resolutions sideways,
                // symbols up and down, whichever pane has the keyboard.
                Key::Left if ctrl && alt => {
                    this.step_timeframe(-1);
                    return glib::Propagation::Stop;
                }
                Key::Right if ctrl && alt => {
                    this.step_timeframe(1);
                    return glib::Propagation::Stop;
                }
                Key::Up if ctrl && alt => {
                    this.step_symbol(-1);
                    return glib::Propagation::Stop;
                }
                Key::Down if ctrl && alt => {
                    this.step_symbol(1);
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
                    this.focused_pane().view.pan_bars(-5);
                    glib::Propagation::Stop
                }
                (Key::Right, false) if !on_watchlist => {
                    this.focused_pane().view.pan_bars(5);
                    glib::Propagation::Stop
                }
                (Key::plus | Key::equal, false) => {
                    this.focused_pane().view.zoom(1.0 / 1.25);
                    glib::Propagation::Stop
                }
                (Key::minus, false) => {
                    this.focused_pane().view.zoom(1.25);
                    glib::Propagation::Stop
                }
                (Key::End, false) if !on_watchlist => {
                    this.focused_pane().view.go_to_latest();
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

        // Ctrl+V is paste and Ctrl+X is cut, and GTK claims both before a
        // controller on the window ever sees them — which is why the vertical
        // split did nothing while the horizontal one worked. The layout keys
        // have to be caught on the way down.
        //
        // Only when the focus is not in something you can type into, or
        // pasting a symbol into a box would split the window instead.
        let capture = gtk::EventControllerKey::new();
        capture.set_propagation_phase(gtk::PropagationPhase::Capture);
        let this = self.clone();
        capture.connect_key_pressed(move |_, key, _, state| {
            use gtk::gdk::Key;
            if !state.contains(gtk::gdk::ModifierType::CONTROL_MASK) || this.is_typing() {
                return glib::Propagation::Proceed;
            }
            match key {
                Key::v | Key::V => this.split_focused(false),
                Key::x | Key::X => this.close_focused(),
                _ => return glib::Propagation::Proceed,
            }
            glib::Propagation::Stop
        });
        self.window.add_controller(capture);
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

    /// Put the bar widget in the bar, the first time this runs on a desktop
    /// that has one.
    ///
    /// Once, and remembered. Taking the widget out is a choice, and a choice
    /// that undid itself on the next launch would not be one — so the question
    /// is recorded as asked, rather than inferred from whether the widget
    /// happens to be there.
    fn offer_bar_widget(self: &Rc<Self>) {
        let home = crate::store::home();
        if self.store.setting_bool(SETTING_WIDGET_OFFERED, false)
            || !crate::bar_plugin::available(&home)
        {
            return;
        }
        self.store.set_setting_bool(SETTING_WIDGET_OFFERED, true);
        if let Err(error) = crate::bar_plugin::install(&home) {
            eprintln!("omacharts: bar widget not installed: {error}");
        }
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
        for pane in self.panes.borrow().iter() {
            pane.view.restyle(theme.clone(), scheme.clone());
        }
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

        // Anchored to the focused chart's own resolution strip, which is what
        // it is about and where the answer will appear. Hanging it off the
        // window would put it over whichever chart happened to be underneath.
        let popover = gtk::Popover::new();
        popover.set_child(Some(&content));
        popover.set_parent(&self.focused_pane().strip);
        // Every number typed builds a fresh one, so each has to let go of the
        // strip when it closes or they pile up on it.
        popover.connect_closed(|popover| {
            let popover = popover.clone();
            glib::idle_add_local_once(move || popover.unparent());
        });

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

    /// The indicator picker, centred like every other picker in the app.
    pub fn add_indicator(self: &Rc<Self>) {
        crate::ui::chart_settings::add_indicator(self);
    }

    /// Re-fold and repaint what is on screen, after something that changes
    /// how the bars are read rather than which bars they are.
    fn redraw_current(self: &Rc<Self>) {
        self.series.borrow_mut().clear();
        let panes = self.panes.borrow().clone();
        for pane in panes {
            let instrument = pane.instrument.borrow().clone();
            if let Some(instrument) = instrument {
                self.show_in(&pane, instrument);
            }
        }
    }

    fn open_preferences(self: &Rc<Self>) {
        let this = self.clone();
        let editor = self.clone();
        Preferences::present(
            &self.window,
            self.store.clone(),
            self.theming.clone(),
            Rc::new(move || this.restyle()),
            Rc::new(move || editor.edit_timeframes()),
        );
    }

    /// The shortcuts, grouped and aligned.
    ///
    /// A list of rows rather than a block of text: an alert dialog centres
    /// whatever it is given, which turns two columns into a ragged mess.
    fn show_shortcuts(self: &Rc<Self>) {
        let page = adw::PreferencesPage::new();

        let sections: [(&str, &[(&str, &str)]); 4] = [
            (
                "Finding things",
                &[
                    ("Type a letter", "Find a symbol"),
                    ("Type a number", "Set the resolution"),
                    ("Ctrl+K", "Find a symbol"),
                    ("Ctrl+I", "Indicators"),
                    ("Ctrl+Shift+I", "Add an indicator"),
                    ("Ctrl+Shift+,", "Chart settings"),
                    ("Ctrl+,", "Preferences"),
                    ("F10", "Main menu"),
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
                "Charts",
                &[
                    ("Ctrl+H", "Split horizontally"),
                    ("Ctrl+V", "Split vertically"),
                    ("Ctrl+X", "Close this chart"),
                    ("Ctrl+L", "Link this chart to the watchlist, or unlink it"),
                    ("Alt+← → ↑ ↓", "Focus the chart that way"),
                ],
            ),
            (
                "Chart",
                &[
                    ("Ctrl+Alt+← →", "Previous or next resolution"),
                    ("Ctrl+Alt+↑ ↓", "Previous or next symbol"),
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

    /// Move along the resolution strip.
    fn step_timeframe(self: &Rc<Self>, delta: i32) {
        let listed = self.timeframes.borrow().clone();
        if listed.is_empty() {
            return;
        }
        let current = self.focused_pane().timeframe.get();
        // A resolution typed but not on the strip has no neighbours; start
        // from the nearest thing that is.
        let at = listed
            .iter()
            .position(|t| *t == current)
            .unwrap_or_else(|| {
                listed
                    .iter()
                    .position(|t| t.seconds() >= current.seconds())
                    .unwrap_or(listed.len() - 1)
            }) as i32;
        let next = (at + delta).clamp(0, listed.len() as i32 - 1) as usize;
        self.apply_timeframe(listed[next]);
    }

    /// Move through the watchlist without going to it.
    fn step_symbol(self: &Rc<Self>, delta: i32) {
        let Some(watchlist) = self.watchlist.borrow().as_ref().cloned() else { return };
        let order = watchlist.flat_order();
        if order.is_empty() {
            return;
        }
        let current = self.focused_pane().instrument.borrow().clone();
        let at = current
            .and_then(|instrument| {
                order
                    .iter()
                    .position(|i| i.symbol == instrument.symbol && i.suffix == instrument.suffix)
            })
            .map(|at| at as i32)
            .unwrap_or(if delta > 0 { -1 } else { order.len() as i32 });
        let next = (at + delta).clamp(0, order.len() as i32 - 1) as usize;
        self.show(order[next].clone());
    }

    /// Switch resolution from somewhere other than the strip, keeping the
    /// strip's buttons honest about what is being shown.
    fn apply_timeframe(self: &Rc<Self>, timeframe: Timeframe) {
        // A resolution you typed belongs on the strip: you asked for it once,
        // you will ask for it again.
        self.remember_timeframe(timeframe);
        self.set_timeframe(timeframe);
        self.sync_timeframe_buttons();
    }

    fn sync_timeframe_buttons(self: &Rc<Self>) {
        self.focused_pane().sync_strip();
    }

    /// The resolution belongs to the focused chart. Splitting a chart to put
    /// the same symbol on two resolutions is most of why you would split one.
    fn set_timeframe(self: &Rc<Self>, timeframe: Timeframe) {
        let pane = self.focused_pane();
        pane.timeframe.set(timeframe);
        self.store.set_setting(LAST_TIMEFRAME, &timeframe.key());
        // A promoted reset period changes with the resolution, so the legend
        // has to be rewritten when the resolution does.
        self.rebuild_indicator_legend();
        let instrument = pane.instrument.borrow().clone();
        if let Some(instrument) = instrument {
            self.show_in(&pane, instrument);
        }
        self.save_workspace();
    }

    /// Chart an instrument: paint from cache now, fetch the gap in the
    /// background.
    /// Chart an instrument on the focused chart, and on every chart linked
    /// with it.
    ///
    /// Linked charts are one group showing one symbol: the watchlist drives
    /// them, and a symbol picked on any of them moves the rest. An unlinked
    /// chart is parked — leaving one showing something while you go looking
    /// elsewhere is most of the reason to have split it.
    pub fn show(self: &Rc<Self>, instrument: Instrument) {
        let focused = self.focused_pane();
        self.show_in(&focused, instrument.clone());
        if focused.linked.get() {
            let others: Vec<Rc<ChartPane>> = self
                .panes
                .borrow()
                .iter()
                .filter(|p| p.id != focused.id && p.linked.get())
                .cloned()
                .collect();
            for pane in others {
                self.show_in(&pane, instrument.clone());
            }
        }
        self.save_workspace();
    }

    /// Chart an instrument on one chart: paint from cache now, fetch the gap
    /// in the background.
    fn show_in(self: &Rc<Self>, pane: &Rc<ChartPane>, instrument: Instrument) {
        let timeframe = pane.timeframe.get();
        let Some(symbol) = self.provider.symbol_for(&instrument) else {
            return;
        };
        let key = format!("{}:{symbol}", self.provider.id());
        let native = timeframe.native();

        *pane.instrument.borrow_mut() = Some(instrument.clone());
        pane.write_readout();
        if pane.id == self.focused.get() {
            self.store.set_setting(LAST_SYMBOL, &instrument.symbol);
            self.store
                .set_setting(LAST_SUFFIX, instrument.suffix.as_deref().unwrap_or(""));
        }

        // Cache first, so the chart is on screen before any request leaves.
        let memo = self.series.borrow().get(&(key.clone(), timeframe)).cloned();
        let cached_was_empty = match memo {
            Some(bars) => {
                let empty = bars.is_empty();
                // Indicators have to be recomputed here too. Skipping it left
                // the previous symbol's VWAP on screen until the network reply
                // arrived and forced a repaint — which looked like a slow
                // indicator and was a missing call.
                self.recompute_indicators(pane, &instrument, timeframe, &bars);
                pane.view.set_series(instrument.clone(), timeframe, (*bars).clone());
                empty
            }
            None => {
                let cached = self.store.load_bars(&key, native);
                let empty = cached.is_empty();
                self.paint(pane, &key, &instrument, timeframe, cached);
                empty
            }
        };
        pane.view.set_stale(false);
        pane.view.set_loading(cached_was_empty);

        // Only the focused chart drives the rail and the prefetch window: the
        // others are not where the next keystroke is going.
        if pane.id == self.focused.get() {
            if let Some(watchlist) = self.watchlist.borrow().as_ref() {
                watchlist.highlight(&instrument);
            }
            // The chart on screen overtakes everything queued behind it, and
            // the old neighbourhood is forgotten: those symbols are no longer
            // the ones a keypress away.
            self.loader.drop_prefetches();
        }
        self.loader
            .fetch(Request { key, symbol, timeframe, speculative: false }, FOREGROUND);
        if pane.id == self.focused.get() {
            self.prefetch_neighbours(&instrument, timeframe);
        }
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
        // One reply can belong to several charts: the same symbol at the same
        // resolution in two panes is one fetch and two repaints.
        let waiting: Vec<Rc<ChartPane>> = self
            .panes
            .borrow()
            .iter()
            .filter(|pane| {
                pane.timeframe.get().native() == native
                    && pane
                        .instrument
                        .borrow()
                        .as_ref()
                        .and_then(|i| self.provider.symbol_for(i))
                        .map(|symbol| format!("{}:{symbol}", self.provider.id()) == key)
                        .unwrap_or(false)
            })
            .cloned()
            .collect();

        if waiting.is_empty() {
            // Something we prefetched. Not on any chart, but the rail's change
            // column may now have numbers it did not have a moment ago.
            if let Some(watchlist) = self.watchlist.borrow().as_ref() {
                watchlist.refresh_quotes();
            }
            return;
        }

        for pane in waiting {
            let instrument = pane.instrument.borrow().clone();
            let Some(instrument) = instrument else { continue };
            self.paint(&pane, key, &instrument, pane.timeframe.get(), bars.clone());
            pane.view.set_stale(stale);
            pane.view.set_loading(false);
        }
        if let Some(watchlist) = self.watchlist.borrow().as_ref() {
            watchlist.refresh_quotes();
        }
    }

    /// Fold to the shown resolution, memoise, and draw.
    fn paint(
        self: &Rc<Self>,
        pane: &Rc<ChartPane>,
        key: &str,
        instrument: &Instrument,
        timeframe: Timeframe,
        bars: Vec<omacharts_engine::Bar>,
    ) {
        // A market that never closes opens where it left off. Nothing here is
        // specific to a provider: the repair only fires when the series it is
        // given has no bodies, which is true of Yahoo's currency pairs and
        // not of its crypto.
        let bars = if instrument.kind.is_continuous() {
            omacharts_engine::repair_continuous_opens(&bars)
        } else {
            bars
        };
        let bars = omacharts_engine::session::filter(
            &bars,
            pane.session.get(),
            instrument,
            timeframe.is_intraday(),
        );
        let bars = if timeframe.is_derived() {
            resample(&bars, timeframe, instrument.session_origin)
        } else {
            bars
        };
        self.recompute_indicators(pane, instrument, timeframe, &bars);
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
        pane.view.set_series(instrument.clone(), timeframe, (*bars).clone());
    }

    /// Run the indicators over the bars now on screen and hand them to the
    /// chart, coloured.
    ///
    /// Colour comes from the theme's palette by slot, so a set of indicators
    /// is distinguishable without anyone choosing anything — and follows the
    /// theme when it changes.
    fn recompute_indicators(
        self: &Rc<Self>,
        pane: &Rc<ChartPane>,
        instrument: &Instrument,
        timeframe: Timeframe,
        bars: &[omacharts_engine::Bar],
    ) {
        let theme = self.theming.borrow().theme();
        let all = pane.indicators.borrow().clone();
        let colors = omacharts_engine::palette_colors(&all, &theme);
        let drawn: Vec<Drawn> = all
            .iter()
            .zip(colors)
            .map(|(indicator, color)| Drawn {
                color,
                output: omacharts_engine::indicators::compute(
                    indicator,
                    bars,
                    instrument.session_origin,
                    timeframe,
                    Some(instrument.kind),
                ),
                indicator: indicator.clone(),
            })
            .collect();
        pane.view.set_indicators(drawn);
    }

    /// Replace the set of indicators and redraw.
    /// Indicators belong to one chart, so this is the focused one's.
    pub fn set_indicators(self: &Rc<Self>, indicators: Vec<Indicator>) {
        let pane = self.focused_pane();
        self.set_indicators_of(&pane, indicators);
    }

    fn set_indicators_of(self: &Rc<Self>, pane: &Rc<ChartPane>, indicators: Vec<Indicator>) {
        // Still written to the one stored set, which is what a brand new chart
        // starts from: the next split should look like the last thing you set
        // up rather than like the defaults.
        self.store.set_indicators(&indicators);
        *pane.indicators.borrow_mut() = indicators;
        self.rebuild_legend_of(pane);
        let instrument = pane.instrument.borrow().clone();
        if let Some(instrument) = instrument {
            self.series.borrow_mut().clear();
            self.show_in(pane, instrument);
        }
        self.save_workspace();
    }

    /// The indicator rows under the legend: a dot, a name, and the two things
    /// you reach for without opening anything — hide it, or go to its settings.
    ///
    /// Deliberately faint. These sit over the drawing, and the drawing is the
    /// point; they come up to full strength when the pointer is near.
    fn rebuild_indicator_legend(self: &Rc<Self>) {
        let panes = self.panes.borrow().clone();
        for pane in panes {
            self.rebuild_legend_of(&pane);
        }
    }

    fn rebuild_legend_of(self: &Rc<Self>, pane: &Rc<ChartPane>) {
        while let Some(child) = pane.indicator_legend.first_child() {
            pane.indicator_legend.remove(&child);
        }
        pane.write_readout();
        let theme = self.theming.borrow().theme();

        let timeframe = pane.timeframe.get();
        let all = pane.indicators.borrow().clone();
        let colors = omacharts_engine::palette_colors(&all, &theme);
        for (indicator, colour) in all.iter().zip(colors) {
            let id = indicator.id;

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

            // Double-clicking the name opens its settings, the same as the
            // gear beside it. The name is the bigger target and the one you
            // are already looking at.
            let open_settings = gtk::GestureClick::new();
            let this = self.clone();
            open_settings.connect_pressed(move |_, presses, _, _| {
                if presses < 2 {
                    return;
                }
                crate::ui::chart_settings::ChartSettings::present_indicator(
                    &this,
                    this.store.clone(),
                    id,
                );
            });
            label.add_controller(open_settings);

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

            let remove = gtk::Button::from_icon_name("user-trash-symbolic");
            remove.add_css_class("flat");
            remove.add_css_class("legend-button");
            remove.set_tooltip_text(Some("Remove"));
            let this = self.clone();
            let owner = pane.clone();
            remove.connect_clicked(move |_| {
                let kept: Vec<Indicator> =
                    owner.indicators.borrow().iter().filter(|i| i.id != id).cloned().collect();
                this.set_indicators_of(&owner, kept);
            });

            let row = gtk::Box::new(gtk::Orientation::Horizontal, 5);
            row.add_css_class("legend-row");
            row.append(&dot);
            row.append(&label);
            row.append(&toggle);
            row.append(&settings);
            row.append(&remove);
            pane.indicator_legend.append(&row);
        }
    }

    /// The active theme, for anything that needs to resolve a colour.
    pub fn theme(&self) -> omacharts_engine::Theme {
        self.theming.borrow().theme()
    }

    /// The focused chart's trading hours.
    pub fn session(&self) -> Session {
        self.focused_pane().session.get()
    }

    pub fn set_session(self: &Rc<Self>, session: Session) {
        self.focused_pane().session.set(session);
        self.save_workspace();
    }

    pub fn show_grid(&self) -> bool {
        self.store.setting_bool(SETTING_SHOW_GRID, true)
    }

    pub fn set_show_grid(self: &Rc<Self>, show: bool) {
        self.store.set_setting_bool(SETTING_SHOW_GRID, show);
        self.focused_pane().view.set_show_grid(show);
    }

    /// How many rows the profile with this id is drawing right now.
    pub fn profile_rows(&self, id: u32) -> Option<usize> {
        self.focused_pane().view.profile_rows(id)
    }

    pub fn bar_style(&self) -> BarStyle {
        self.focused_pane().bar_style.get()
    }

    pub fn set_bar_style(self: &Rc<Self>, style: BarStyle) {
        self.focused_pane().bar_style.set(style);
        self.store.set_setting(SETTING_BAR_STYLE, style.key());
        self.focused_pane().view.set_bar_style(style);
    }

    /// Right-clicking the chart offers the things you change most, and a way
    /// to everything else.
    fn chart_menu(self: &Rc<Self>, x: f64, y: f64) {
        let menu = gio::Menu::new();

        // Choices live behind a named item rather than loose in the menu: a
        // flat list of radio buttons makes you read every option to find out
        // what the menu is even about.
        let bars = gio::Menu::new();
        for style in BarStyle::ALL {
            let item = gio::MenuItem::new(Some(style.label()), None);
            item.set_action_and_target_value(
                Some("chart.bar-style"),
                Some(&style.key().to_variant()),
            );
            bars.append_item(&item);
        }
        menu.append_submenu(Some("Bar style"), &bars);

        let sessions = gio::Menu::new();
        for session in Session::ALL {
            let item = gio::MenuItem::new(Some(session.label()), None);
            item.set_action_and_target_value(
                Some("chart.session"),
                Some(&session.key().to_variant()),
            );
            sessions.append_item(&item);
        }
        menu.append_submenu(Some("Session"), &sessions);

        // The layout, in its own section: splitting and closing are about the
        // arrangement rather than about what this chart draws.
        let layout = gio::Menu::new();
        shortcuts::append(&layout, "Split horizontally", "chart.split-h");
        shortcuts::append(&layout, "Split vertically", "chart.split-v");
        if self.panes.borrow().len() > 1 {
            shortcuts::append(&layout, "Close chart", "chart.close");
        }
        menu.append_section(None, &layout);

        let rest = gio::Menu::new();
        shortcuts::append(&rest, "Linked to watchlist", "chart.linked");
        shortcuts::append(&rest, "Indicators…", "chart.indicators");
        shortcuts::append(&rest, "Chart settings…", "chart.settings");
        menu.append_section(None, &rest);

        // Hung off the window rather than the chart it was opened on: a menu
        // parented to one pane of a split has only that pane's height to fit
        // in, and GTK answers a menu that does not fit by making it scroll.
        let area = self.focused_pane().view.area.clone();
        let (wx, wy) = area
            .translate_coordinates(&self.window, x, y)
            .unwrap_or((x, y));
        popup_menu(&menu, &self.window, wx, wy);
    }

    /// The price axis has its own menu, and its own question: whether the
    /// scale fits itself to what is on screen.
    ///
    /// Shown as a state rather than offered as an action, because it is on
    /// unless you have dragged the axis — "Auto scale price" as a plain item
    /// looked broken, since clicking it usually asked for what was already
    /// happening. Turning it off is what lets the chart be dragged up and down.
    fn price_axis_menu(self: &Rc<Self>, x: f64, y: f64) {
        if let Some(action) = self.auto_scale_action.borrow().as_ref() {
            action.set_state(&self.focused_pane().view.price_auto().to_variant());
        }
        let menu = gio::Menu::new();
        menu.append(Some("Auto scale price"), Some("chart.auto-scale"));
        let rest = gio::Menu::new();
        shortcuts::append(&rest, "Reset chart", "chart.reset-view");
        menu.append_section(None, &rest);

        let area = self.focused_pane().view.area.clone();
        let (wx, wy) = area.translate_coordinates(&self.window, x, y).unwrap_or((x, y));
        popup_menu(&menu, &self.window, wx, wy);
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
        *self.bar_style_action.borrow_mut() = Some(bar_style);

        let session = gio::SimpleAction::new_stateful(
            "session",
            Some(glib::VariantTy::STRING),
            &self.session().key().to_variant(),
        );
        let this = self.clone();
        session.connect_activate(move |action, value| {
            let Some(key) = value.and_then(|v| v.str().map(str::to_string)) else { return };
            let Some(chosen) = Session::from_key(&key) else { return };
            action.set_state(&key.to_variant());
            this.focused_pane().session.set(chosen);
            this.store
                .set_setting(crate::ui::chart_settings::SETTING_SESSION, chosen.key());
            this.redraw_current();
        });
        actions.add_action(&session);
        *self.session_action.borrow_mut() = Some(session);

        let split_h = gio::SimpleAction::new("split-h", None);
        let this = self.clone();
        split_h.connect_activate(move |_, _| this.split_focused(true));
        actions.add_action(&split_h);

        let split_v = gio::SimpleAction::new("split-v", None);
        let this = self.clone();
        split_v.connect_activate(move |_, _| this.split_focused(false));
        actions.add_action(&split_v);

        let close = gio::SimpleAction::new("close", None);
        let this = self.clone();
        close.connect_activate(move |_, _| this.close_focused());
        actions.add_action(&close);

        let linked = gio::SimpleAction::new_stateful(
            "linked",
            None,
            &self.focused_pane().linked.get().to_variant(),
        );
        let this = self.clone();
        linked.connect_activate(move |action, _| {
            let next = !this.focused_pane().linked.get();
            action.set_state(&next.to_variant());
            this.set_pane_linked(this.focused.get(), next);
        });
        actions.add_action(&linked);
        *self.linked_action.borrow_mut() = Some(linked);

        let auto_scale = gio::SimpleAction::new_stateful(
            "auto-scale",
            None,
            &self.focused_pane().view.price_auto().to_variant(),
        );
        let this = self.clone();
        auto_scale.connect_activate(move |action, _| {
            let next = !this.focused_pane().view.price_auto();
            action.set_state(&next.to_variant());
            this.focused_pane().view.set_price_auto(next);
        });
        actions.add_action(&auto_scale);
        *self.auto_scale_action.borrow_mut() = Some(auto_scale);

        let reset_view = gio::SimpleAction::new("reset-view", None);
        let this = self.clone();
        reset_view.connect_activate(move |_, _| this.focused_pane().view.reset_view());
        actions.add_action(&reset_view);

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
    /// Is the keyboard going into a box somebody is typing in?
    fn is_typing(&self) -> bool {
        gtk::prelude::GtkWindowExt::focus(&self.window)
            .map(|widget| widget.is::<gtk::Editable>() || widget.is::<gtk::TextView>())
            .unwrap_or(false)
    }

    fn shows_sidebar(&self) -> bool {
        self.split.end_child().map(|rail| rail.is_visible()).unwrap_or(false)
    }

    fn set_show_sidebar(&self, show: bool) {
        if let Some(rail) = self.split.end_child() {
            rail.set_visible(show);
        }
    }

    /// Put the rail back at the width it was left at.
    ///
    /// Stored as a width rather than a handle position, because the position
    /// is measured from the other side and would move the rail every time the
    /// window was resized.
    fn restore_sidebar_width(self: &Rc<Self>) {
        let width = self
            .store
            .setting(SETTING_SIDEBAR_WIDTH)
            .and_then(|v| v.parse::<i32>().ok())
            .unwrap_or(DEFAULT_SIDEBAR_WIDTH)
            .clamp(160, 900);

        // Nothing is written down until the stored width has been put back.
        // Mapping reports a position measured against a window that has not
        // been laid out yet, and saving that overwrote the width every launch
        // with whatever the default happened to produce.
        let placed = Rc::new(Cell::new(false));

        let split = self.split.clone();
        let placed_for_put = placed.clone();
        let put_back = move || {
            if placed_for_put.get() {
                return;
            }
            let total = split.width();
            if total > width + 200 {
                placed_for_put.set(true);
                split.set_position(total - width);
            }
        };
        put_back();
        let on_map = put_back.clone();
        self.split.connect_map(move |_| on_map());

        let store = self.store.clone();
        self.split.connect_position_notify(move |paned| {
            if !placed.get() {
                put_back();
                return;
            }
            if paned.width() > 0 && paned.position() > 0 {
                let width = (paned.width() - paned.position()).clamp(160, 900);
                store.set_setting(SETTING_SIDEBAR_WIDTH, &width.to_string());
            }
        });
    }

    pub fn toggle_watchlist(self: &Rc<Self>) {
        let focused = self
            .watchlist
            .borrow()
            .as_ref()
            .map(|w| w.has_focus())
            .unwrap_or(false);

        match watchlist_action(self.shows_sidebar(), focused) {
            WatchlistAction::Open => {
                self.set_show_sidebar(true);
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
                self.set_show_sidebar(false);
                self.store.set_setting_bool(SHOW_WATCHLIST, false);
                self.focused_pane().view.area.grab_focus();
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

    /// Show a symbol named on the command line, if we know it.
    ///
    /// Takes the canonical symbol and the exchange suffix separately, because
    /// a display symbol cannot be split back apart: BRK.B is one symbol with a
    /// dot in it, SAN.MC is a symbol and a suffix.
    pub fn show_named(self: &Rc<Self>, symbol: &str, suffix: Option<&str>) -> bool {
        let Some(instrument) = self.index.find(symbol, suffix) else {
            return false;
        };
        self.show(instrument);
        true
    }

    pub fn indicators(&self) -> Vec<Indicator> {
        self.focused_pane().indicators.borrow().clone()
    }

    /// An id nothing on the chart is using.
    pub fn next_indicator_id(&self) -> u32 {
        self.focused_pane().indicators.borrow().iter().map(|i| i.id).max().unwrap_or(0) + 1
    }

    fn restore_last_symbol(self: &Rc<Self>) {
        let symbol = self.store.setting(LAST_SYMBOL).unwrap_or_else(|| "GSPC".to_string());
        let suffix = self.store.setting(LAST_SUFFIX).filter(|s| !s.is_empty());
        let instrument = self
            .index
            .find(&symbol, suffix.as_deref())
            .or_else(|| self.index.find("GSPC", None));
        if let Some(instrument) = instrument {
            self.show(instrument);
        }
    }
}
