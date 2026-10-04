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
use crate::store::{Store, DEFAULT_WATCHLIST};
use crate::theming::Theming;
use crate::ui::chart::{Drawn, Echo};
use crate::ui::pane::{self, ChartPane, Node};
use omacharts_engine::link;
use omacharts_engine::LinkGroup;
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

    /// What the setting held before there were chartbooks: one arrangement,
    /// with no name and nothing around it.
    const ONE_ARRANGEMENT: &str = r#"{
        "layout": {"split": {"horizontal": true, "ratio": 0.4,
                             "first": {"leaf": 1}, "second": {"leaf": 2}}},
        "focused": 2,
        "panes": [
            {"id": 1, "symbol": "SPY", "suffix": null, "timeframe": "1D",
             "indicators": [], "bar_style": "candle", "session": "extended",
             "show_grid": true, "linked": true},
            {"id": 2, "symbol": "QQQ", "suffix": null, "timeframe": "5m",
             "indicators": [], "bar_style": "candle", "session": "extended",
             "show_grid": true, "linked": false}
        ]
    }"#;

    /// Anybody upgrading has one of these in their database. Reading it as
    /// the single book it describes is the difference between keeping the
    /// layout they left and being handed an empty window.
    /// Every chart stored before there were groups says `"linked": true`, and
    /// nothing rewrites that file — it is the live layout, read once. A chart
    /// that was linked has to come back in the group that looks and behaves
    /// like the only link there used to be.
    #[test]
    fn a_chart_linked_before_there_were_groups_joins_the_first_one() {
        let workspace = parse_workspace(ONE_ARRANGEMENT).expect("the old shape should parse");
        let panes = &workspace.books[0].panes;
        assert_eq!(panes[0].linked, LinkGroup::Group(1), "linked meant the neutral group");
        assert_eq!(panes[1].linked, LinkGroup::None, "and unlinked still means nobody");
    }

    #[test]
    fn a_group_survives_being_written_down() {
        let json = r#"{"id": 1, "symbol": "SPY", "suffix": null, "timeframe": "1D",
             "indicators": [], "bar_style": "candle", "session": "extended",
             "show_grid": true, "linked": 4}"#;
        let pane: StoredPane = serde_json::from_str(json).expect("a numbered group should parse");
        assert_eq!(pane.linked, LinkGroup::Group(4));
        let back: StoredPane =
            serde_json::from_str(&serde_json::to_string(&pane).unwrap()).unwrap();
        assert_eq!(back.linked, LinkGroup::Group(4));
    }

    /// The menu marks the row this chart is in, so the number it carries and
    /// the number the action is told have to be the same number.
    #[test]
    fn a_groups_menu_target_is_its_number() {
        assert_eq!(group_state(LinkGroup::None).get::<i32>(), Some(0));
        assert_eq!(group_state(LinkGroup::Group(7)).get::<i32>(), Some(7));
    }

    /// A click in the bar widget hands back a canonical ticker and a suffix,
    /// and looks for a list holding that pair. Matching on the ticker alone
    /// would find SAP in Frankfurt from a click on SAP in New York.
    #[test]
    fn a_list_holds_a_symbol_only_with_the_venue_it_was_stored_under() {
        let store = Store::memory().expect("an in-memory store");
        store.add_to_root("SAP", Some("DE"));

        assert!(list_holds(&store, DEFAULT_WATCHLIST, "SAP", Some("DE")));
        assert!(!list_holds(&store, DEFAULT_WATCHLIST, "SAP", None), "a different listing");
        assert!(!list_holds(&store, DEFAULT_WATCHLIST, "SAP.DE", None), "not the display form");
    }

    #[test]
    fn a_workspace_written_before_chartbooks_comes_back_as_one() {
        let workspace = parse_workspace(ONE_ARRANGEMENT).expect("the old shape should parse");
        assert_eq!(workspace.books.len(), 1);
        assert_eq!(workspace.active, 0);

        let book = &workspace.books[0];
        assert_eq!(book.focused, 2, "which chart had the keyboard survives");
        assert_eq!(book.panes.len(), 2);
        assert_eq!(book.name, "", "nothing named it, so it is still unnamed");
        // The shape, dividers and all: a layout that comes back centred is
        // not the layout anybody left.
        assert_eq!(book.layout.leaves(), vec![1, 2]);
        assert!(matches!(book.layout, Node::Split { ratio, .. } if (ratio - 0.4).abs() < 1e-9));
    }

    #[test]
    fn several_books_round_trip_through_the_setting() {
        let one = parse_workspace(ONE_ARRANGEMENT).unwrap().books.remove(0);
        let two = Chartbook {
            name: "Energy".to_string(),
            layout: Node::leaf(7),
            focused: 7,
            panes: vec![StoredPane {
                id: 7,
                symbol: "CL".to_string(),
                suffix: None,
                timeframe: "1h".to_string(),
                indicators: Vec::new(),
                bar_style: "candle".to_string(),
                session: "extended".to_string(),
                show_grid: true,
                linked: LinkGroup::None,
            }],
            watchlist: Some(4),
            sidebar_shown: Some(false),
            sidebar_width: Some(330),
        };
        let json =
            serde_json::to_string(&Workspace { books: vec![one, two], active: 1 }).unwrap();

        let back = parse_workspace(&json).expect("the new shape should parse");
        assert_eq!(back.active, 1);
        assert_eq!(back.books.len(), 2);
        assert_eq!(back.books[0].panes.len(), 2, "the first book kept its split");
        assert_eq!(back.books[1].name, "Energy");
        assert_eq!(back.books[1].layout.leaves(), vec![7]);
    }

    /// A book is what you were looking at, and half of that is the list
    /// beside the charts. Coming back to an arrangement of energy charts
    /// next to yesterday's list is the same surprise as coming back to the
    /// wrong charts.
    #[test]
    fn a_book_remembers_the_rail_it_was_left_with() {
        let one = parse_workspace(ONE_ARRANGEMENT).unwrap().books.remove(0);
        let json = serde_json::to_string(&Workspace { books: vec![one], active: 0 }).unwrap();
        let mut book = parse_workspace(&json).unwrap().books.remove(0);
        book.watchlist = Some(4);
        book.sidebar_shown = Some(false);
        book.sidebar_width = Some(330);

        let json = serde_json::to_string(&Workspace { books: vec![book], active: 0 }).unwrap();
        let back = &parse_workspace(&json).expect("a book with a rail should parse").books[0];
        assert_eq!(back.watchlist, Some(4), "which list it was showing");
        assert_eq!(back.sidebar_shown, Some(false), "whether it was open at all");
        assert_eq!(back.sidebar_width, Some(330), "and how wide");
    }

    /// Every book written before the rail belonged to one says nothing about
    /// it, and has to come back saying nothing rather than failing to come
    /// back — a missing field is not a reason to lose somebody's charts.
    #[test]
    fn a_book_written_before_the_rail_belonged_to_it_still_loads() {
        let workspace = parse_workspace(ONE_ARRANGEMENT).expect("the old shape should parse");
        let book = &workspace.books[0];
        assert_eq!(book.panes.len(), 2, "the charts come back either way");
        // None rather than a default, so the fallback can be the window's own
        // settings rather than a number invented here.
        assert_eq!(book.watchlist, None);
        assert_eq!(book.sidebar_shown, None);
        assert_eq!(book.sidebar_width, None);
    }

    /// The two shapes have to be told apart by what is in them, because both
    /// arrive as the same string from the same setting.
    #[test]
    fn neither_stored_shape_can_be_read_as_the_other() {
        let new = serde_json::to_string(&Workspace {
            books: vec![parse_workspace(ONE_ARRANGEMENT).unwrap().books.remove(0)],
            active: 0,
        })
        .unwrap();
        assert!(serde_json::from_str::<Chartbook>(&new).is_err(), "no layout at the top");
        assert!(serde_json::from_str::<Workspace>(ONE_ARRANGEMENT).is_err(), "no books");
    }

    /// A window with nothing written down, and one with something unreadable,
    /// both have to end up with a chart rather than with half of one.
    #[test]
    fn nonsense_in_the_setting_restores_nothing() {
        assert!(parse_workspace("").is_none());
        assert!(parse_workspace("{}").is_none());
        assert!(parse_workspace(r#"{"books": [], "active": 0}"#).is_none());
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
/// A group's colour as a small filled circle, or an empty space the same size
/// for the group that has no colour — so the labels line up down the popover
/// however many of the rows are coloured.
fn swatch_dot(colour: Option<String>) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::new();
    area.set_content_width(12);
    area.set_content_height(12);
    area.set_valign(gtk::Align::Center);
    area.set_draw_func(move |_, cr, width, height| {
        let Some(hex) = colour.as_deref() else { return };
        crate::ui::colors::set_source(cr, hex);
        let r = (width.min(height) as f64) / 2.0 - 1.5;
        cr.arc(width as f64 / 2.0, height as f64 / 2.0, r, 0.0, std::f64::consts::TAU);
        let _ = cr.fill();
    });
    area
}

/// Does this watchlist hold that symbol?
///
/// Matched on the canonical ticker and the exchange suffix together, never on
/// what either of them is displayed as: BRK.B is one symbol with a dot in it
/// and SAN.MC is a symbol and a suffix, and a comparison that cannot tell
/// those apart finds the wrong one abroad.
fn list_holds(store: &Store, watchlist: i64, symbol: &str, suffix: Option<&str>) -> bool {
    store.watchlist_sections(watchlist).iter().any(|section| {
        section
            .entries
            .iter()
            .any(|entry| entry.symbol == symbol && entry.suffix.as_deref() == suffix)
    })
}

/// A group as a menu action's target: the number, and zero for unlinked.
fn group_state(group: LinkGroup) -> glib::Variant {
    (group.number().unwrap_or(0) as i32).to_variant()
}

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
/// How narrow and how wide the rail may be dragged. Below the first its
/// columns stop fitting; above the second it is taking room from the chart
/// without showing anything more.
const MIN_SIDEBAR_WIDTH: i32 = 160;
const MAX_SIDEBAR_WIDTH: i32 = 900;
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
#[derive(Clone, serde::Serialize, serde::Deserialize)]
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
    linked: LinkGroup,
}

/// One arrangement of charts, saved under a name.
///
/// A chartbook is the unit you switch between: a tree, the charts in it, and
/// which of them had the keyboard. Everything about a chart that is worth
/// keeping is already in `StoredPane`, so a book is that list plus the shape
/// they were in.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct Chartbook {
    #[serde(default)]
    name: String,
    layout: Node,
    focused: u32,
    panes: Vec<StoredPane>,
    /// The rail, as this book left it: which watchlist it was showing,
    /// whether it was open at all, and how wide.
    ///
    /// Part of the book rather than of the window, because a book is what you
    /// were looking at and the list beside the charts is half of that — an
    /// arrangement of energy charts wants the energy list, and finding
    /// yesterday's list beside it is the same surprise as finding the wrong
    /// charts.
    ///
    /// `None` in a book written before the rail belonged to one. Those fall
    /// back to the window's settings, which is where the choice used to live
    /// and is still where a brand new book takes its from.
    #[serde(default)]
    watchlist: Option<i64>,
    #[serde(default)]
    sidebar_shown: Option<bool>,
    #[serde(default)]
    sidebar_width: Option<i32>,
}

/// Everything the window had open, as one stored value.
///
/// `Workspace` used to be a single arrangement, which is what `Chartbook` is
/// now. A window holds several and shows one.
#[derive(serde::Serialize, serde::Deserialize)]
struct Workspace {
    books: Vec<Chartbook>,
    active: usize,
}

/// What a chartbook nobody has renamed is called.
///
/// By where it sits rather than when it was made, so the names in the strip
/// always read 1, 2, 3 and the arrow keys walk them in the order they are
/// written. A book keeps an empty name until somebody types one.
fn default_book_name(ordinal: usize) -> String {
    format!("Chartbook {ordinal}")
}

/// Read what was written down, in whichever shape it was written.
///
/// The setting held a single arrangement before there were chartbooks to hold
/// several, and anybody upgrading has one of those in their database. Reading
/// it as the one book it describes is the difference between keeping the
/// layout they left and being handed an empty window.
///
/// The two shapes cannot be confused for each other: the old one has no
/// `books`, and the new one has no `layout`.
fn parse_workspace(json: &str) -> Option<Workspace> {
    serde_json::from_str::<Workspace>(json)
        .ok()
        .filter(|workspace| !workspace.books.is_empty())
        .or_else(|| {
            let book = serde_json::from_str::<Chartbook>(json).ok()?;
            Some(Workspace { books: vec![book], active: 0 })
        })
}

/// How wide the window's corner controls are, for the one time a chart's own
/// corner has to step around them before either has been allocated.
const CORNER_WIDTH: i32 = 78;

pub struct Window {
    pub window: adw::ApplicationWindow,
    /// Every chart on screen. One of them is focused, and that is the one the
    /// keyboard, the menus and the symbol search act on.
    panes: RefCell<Vec<Rc<ChartPane>>>,
    /// How they are arranged. Splitting divides the focused pane; closing one
    /// hands its space to its sibling.
    layout: RefCell<Node>,
    focused: Cell<u32>,
    /// The chart that has the window to itself, if one does.
    ///
    /// Kept beside the tree rather than in it: maximizing is a way of looking
    /// at an arrangement, not a change to it, so the arrangement it came from
    /// is still there to go back to exactly. It is also why this is not
    /// written down — a window that came back maximized would look like a
    /// window that had lost its other charts.
    maximized: Cell<Option<u32>>,
    /// Every chartbook, as it is written down, including the one on screen.
    ///
    /// Only one book is ever built into widgets: `panes` and `layout` above
    /// are the live form of whichever is active, and the rest sit here as
    /// data. Switching writes the live one back into this list and builds the
    /// next, which is the same work `restore_workspace` does on launch and so
    /// has only one way to be wrong.
    books: RefCell<Vec<Chartbook>>,
    active: Cell<usize>,
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
    /// The row of chartbook tabs under the charts, empty and hidden until
    /// there is a second book to switch to.
    book_strip: gtk::Box,
    /// The window's own controls, floating over the top-right corner. Held on
    /// to because a chart's maximize button is in that corner too, and has to
    /// step aside when the rail is closed and the two would be in one place.
    corner: RefCell<Option<gtk::WindowHandle>>,
    /// The corner's watchlist button. Held because Ctrl+B decides what to do
    /// and the button is what does it: showing the rail through the button is
    /// what keeps its pressed state honest and the corner clearance correct.
    watchlist_toggle: RefCell<Option<gtk::ToggleButton>>,
    /// How wide the rail should be.
    ///
    /// Kept here rather than measured off the handle when it is wanted,
    /// because the handle only means anything once the window has been laid
    /// out — and a chartbook gets written down at moments when it has not.
    sidebar_width: Cell<i32>,
    /// A width has been put back, so what the handle says from here on is
    /// somebody's choice rather than a guess from a window with no size yet.
    sidebar_placed: Cell<bool>,
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

        // Its own widget rather than something inside `chart_host`, which is
        // emptied and refilled every time the tree changes.
        let book_strip = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        book_strip.add_css_class("chartbook-strip");
        book_strip.set_visible(false);

        let this = Rc::new(Window {
            window: window.clone(),
            panes: RefCell::new(Vec::new()),
            layout: RefCell::new(Node::leaf(1)),
            focused: Cell::new(1),
            maximized: Cell::new(None),
            books: RefCell::new(Vec::new()),
            active: Cell::new(0),
            next_pane: Cell::new(1),
            save_pending: Cell::new(false),
            linked_action: RefCell::new(None),
            bar_style_action: RefCell::new(None),
            session_action: RefCell::new(None),
            auto_scale_action: RefCell::new(None),
            chart_host: chart_host.clone(),
            book_strip: book_strip.clone(),
            corner: RefCell::new(None),
            watchlist_toggle: RefCell::new(None),
            sidebar_width: Cell::new(DEFAULT_SIDEBAR_WIDTH),
            sidebar_placed: Cell::new(false),
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
        // Which list the rail is showing is part of the chartbook, so every
        // way of changing it has to write the book down. One hook, because a
        // second route to the same state is a second route that can forget.
        let saver = this.clone();
        let rail = watchlist.clone();
        watchlist.connect_switched(move || {
            // A different list is a different group: the chain has to say what
            // the list in front of you drives, not what the last one did.
            let colours = saver.clone();
            rail.adopt_link_group(move |group| colours.link_colour(group));
            saver.save_soon();
        });
        *this.watchlist.borrow_mut() = Some(watchlist.clone());

        watchlist.link_button().set_popover(Some(&this.link_popover(|window, group| {
            let colour = window.link_colour(group);
            if let Some(rail) = window.watchlist.borrow().as_ref() {
                rail.set_link_group(group, colour);
            }
            window.save_workspace();
        })));
        let colours = this.clone();
        watchlist.adopt_link_group(move |group| colours.link_colour(group));

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
        *this.corner.borrow_mut() = Some(corner.clone());
        // The strip runs under the whole window, rail included. A row of tabs
        // stopping where the rail began would read as part of the chart area,
        // and a chartbook is not: the rail belongs to one as much as the
        // charts do.
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        split.set_vexpand(true);
        root.append(split);
        root.append(&book_strip);

        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&root));
        overlay.add_overlay(&corner);
        this.watch_sidebar_width();
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
            LinkGroup::Group(1),
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
            this.restore_last_symbol();
        }
        // A window that restored nothing still has a chartbook: the one it
        // just built. Everything below here can assume there is always one.
        if this.books.borrow().is_empty() {
            let book = this.capture_book(String::new());
            this.books.borrow_mut().push(book);
            this.active.set(0);
        }
        this.rebuild_book_strip();
        // Either way. The legend was only built on the path that did not
        // restore anything, so a window that came back with its charts came
        // back without the names of what was drawn on them.
        this.rebuild_indicator_legend();
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
        linked: LinkGroup,
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

        // Clicking another chart's corner means that chart: the focus moves
        // there first, and then it grows.
        let expander = self.clone();
        pane.expand.connect_clicked(move |_| {
            expander.focus(id);
            expander.toggle_maximized();
        });

        // The chain opens the list of groups rather than toggling one, now
        // that there are nine of them and "off" is a tenth answer.
        pane.link.set_popover(Some(&self.link_popover(move |window, group| {
            window.focus(id);
            window.set_pane_link_group(id, group);
        })));
        pane.set_link_group(linked, self.link_colour(linked));

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

        // The pointer on one chart draws a crosshair on the charts linked
        // with it, so you can read the same moment and the same price on all
        // of them at once. Both halves travel as values rather than as
        // pixels: charts at different resolutions share no bar index, and
        // charts at different zooms share no y.
        let echoer = self.clone();
        pane.view.set_hover_handler(move |hover| {
            echoer.echo_crosshair(id, hover.map(|h| Echo { ts: h.bar.ts, price: h.price }));
        });

        // Clicking anywhere on a chart focuses it, which is what makes the
        // next keystroke land where you are looking.
        //
        // The keyboard has to come with it, and GTK does not move it for a
        // drawing area the way it would for a button. Without this the ring
        // moved to the chart you clicked while the keyboard stayed wherever
        // it was — which made Ctrl+B close the rail you had just clicked
        // away from, because the rail still held the keyboard and Ctrl+B
        // reads that to decide between focusing and closing.
        let focuser = self.clone();
        let click = gtk::GestureClick::new();
        click.set_button(0);
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        click.connect_pressed(move |_, _, _, _| {
            focuser.focus(id);
            if let Some(pane) = focuser.pane(id) {
                pane.view.area.grab_focus();
            }
        });
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
        // Maximized, the focus keys still walk the arrangement — they just
        // walk it one chart at a time, with the window showing whichever one
        // the focus has reached. Stepping out of the maximized chart to look
        // at its neighbour is the whole reason to press them while one chart
        // is filling the window.
        if self.maximized.get().is_some_and(|open| open != id) && self.pane(id).is_some() {
            self.maximized.set(Some(id));
            self.rebuild_layout();
        }
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
            action.set_state(&group_state(pane.linked.get()));
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
        // Dividing a chart that is filling the window has to show you what it
        // divided into, so this is where maximizing ends.
        self.maximized.set(None);
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
        if self.maximized.get() == Some(id) {
            self.maximized.set(None);
        }
        let leaves = next.leaves();
        *self.layout.borrow_mut() = next;
        self.panes.borrow_mut().retain(|p| p.id != id);
        self.rebuild_layout();
        if let Some(first) = leaves.first() {
            self.focus(*first);
        }
        self.save_workspace();
    }

    /// Give the focused chart the whole window, or hand the window back.
    ///
    /// The tree is left alone: what changes is how much of it is mounted. So
    /// restoring is exact, down to where every divider was.
    pub fn toggle_maximized(self: &Rc<Self>) {
        if self.layout.borrow().leaves().len() < 2 {
            return;
        }
        let id = self.focused.get();
        let next = (self.maximized.get() != Some(id)).then_some(id);
        self.maximized.set(next);
        self.rebuild_layout();
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
        // Maximized, every chart but one is off the widget tree and has no
        // allocation to ask about — so the arrangement is measured from the
        // arrangement itself. Otherwise the real allocations are the truth,
        // gutters and all.
        let rects = self.maximized.get().map(|_| self.layout_rects());
        let rect = |id: u32| match &rects {
            Some(rects) => rects.get(&id).copied(),
            None => self.pane_rect(id),
        };
        let Some(from) = rect(self.focused.get()) else { return };
        let (fx, fy) = (from.0 + from.2 / 2.0, from.1 + from.3 / 2.0);

        let mut best: Option<(f64, u32)> = None;
        for pane in self.panes.borrow().iter() {
            if pane.id == self.focused.get() {
                continue;
            }
            let Some(rect) = rect(pane.id) else { continue };
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

    /// Where every chart would sit, worked out from the tree rather than from
    /// the widgets — which is the only way to ask while one of them is
    /// filling the window and the rest are off the screen entirely.
    fn layout_rects(&self) -> HashMap<u32, (f64, f64, f64, f64)> {
        fn fill(node: &Node, x: f64, y: f64, w: f64, h: f64, out: &mut HashMap<u32, (f64, f64, f64, f64)>) {
            match node {
                Node::Leaf(id) => {
                    out.insert(*id, (x, y, w, h));
                }
                Node::Split { horizontal, ratio, first, second } => {
                    let ratio = ratio.clamp(0.05, 0.95);
                    if *horizontal {
                        fill(first, x, y, w * ratio, h, out);
                        fill(second, x + w * ratio, y, w * (1.0 - ratio), h, out);
                    } else {
                        fill(first, x, y, w, h * ratio, out);
                        fill(second, x, y + h * ratio, w, h * (1.0 - ratio), out);
                    }
                }
            }
        }
        let mut out = HashMap::new();
        let width = self.chart_host.width().max(1) as f64;
        let height = self.chart_host.height().max(1) as f64;
        fill(&self.layout.borrow(), 0.0, 0.0, width, height, &mut out);
        out
    }

    /// What is actually mounted: the whole tree, or the one chart that has
    /// been given the window.
    fn mounted(&self) -> Node {
        match self.maximized.get() {
            Some(id) if self.pane(id).is_some() && self.layout.borrow().leaves().len() > 1 => {
                Node::Leaf(id)
            }
            _ => self.layout.borrow().clone(),
        }
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
        let layout = self.mounted();
        let widget = self.build_node(&layout, &[]);
        self.chart_host.append(&widget);
        let focused = self.focused.get();
        // One chart has nothing to be maximized away from, so the corner is
        // not offered at all until there is a second — and nothing is drawn
        // as maximized either, which is what `mounted` has already decided.
        let several = self.layout.borrow().leaves().len() > 1;
        let maximized = several.then(|| self.maximized.get()).flatten();
        for pane in self.panes.borrow().iter() {
            pane.set_focused(pane.id == focused);
            pane.set_expandable(several);
            pane.set_maximized(maximized == Some(pane.id));
        }
        self.sync_corner_clearance();
    }

    /// Keep a chart's maximize button out from under the window's own corner.
    ///
    /// They want the same few pixels, and only sometimes: the window's
    /// controls sit over the rail while the rail is open, and drop onto the
    /// top-right chart when it is closed. So the chart under them steps its
    /// button aside by exactly their width, and every other chart keeps the
    /// corner it had.
    fn sync_corner_clearance(self: &Rc<Self>) {
        let railed = self
            .split
            .end_child()
            .map(|rail| rail.is_visible())
            .unwrap_or(false);
        let clearance = match railed {
            true => 0,
            false => self
                .corner
                .borrow()
                .as_ref()
                .map(|corner| corner.width())
                .filter(|width| *width > 0)
                .unwrap_or(CORNER_WIDTH),
        };
        // The rects tile the area exactly, so the chart under the window's
        // corner is the one — the only one — holding its top right pixel.
        let rects = self.layout_rects();
        let right = self.chart_host.width().max(1) as f64;
        let topmost = self.mounted().leaves().into_iter().find(|id| {
            rects
                .get(id)
                .is_some_and(|(x, y, w, _)| *y <= 0.5 && x + w >= right - 0.5)
        });
        for pane in self.panes.borrow().iter() {
            let margin = if Some(pane.id) == topmost { clearance } else { 0 };
            pane.set_corner_clearance(margin);
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

    pub fn set_pane_link_group(self: &Rc<Self>, id: u32, group: LinkGroup) {
        let Some(pane) = self.pane(id) else { return };
        pane.set_link_group(group, self.link_colour(group));
        // Joining a group adopts what the group is showing, which is what
        // joining means — otherwise the chain says group 3 and the chart is
        // somewhere else.
        if let Some(instrument) = self.group_instrument(group, id) {
            self.show_in(&pane, instrument);
        }
        self.sync_chart_actions(&pane);
        self.save_workspace();
    }

    /// What a group's badge is painted in, against the theme in force now.
    ///
    /// Resolved on demand rather than stored, because the number is the group
    /// and the colour is only this theme's answer about it.
    fn link_colour(&self, group: LinkGroup) -> Option<String> {
        group.colour(&self.theming.borrow().theme())
    }

    /// The list of groups, as a popover. `choose` is handed the group the row
    /// stands for; which chart or rail it applies to is the caller's business.
    fn link_popover(
        self: &Rc<Self>,
        choose: impl Fn(&Rc<Self>, LinkGroup) + 'static,
    ) -> gtk::Popover {
        let popover = gtk::Popover::new();
        popover.set_has_arrow(false);
        let menu = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let choose = Rc::new(choose);
        for group in link::ALL {
            let row = gtk::Button::new();
            row.add_css_class("flat");
            row.add_css_class("link-row");
            let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            line.append(&swatch_dot(self.link_colour(group)));
            let label = gtk::Label::new(Some(group.label()));
            label.set_xalign(0.0);
            label.set_hexpand(true);
            line.append(&label);
            row.set_child(Some(&line));

            let this = self.clone();
            let choose = choose.clone();
            let popover_weak = popover.downgrade();
            row.connect_clicked(move |_| {
                if let Some(popover) = popover_weak.upgrade() {
                    popover.popdown();
                }
                choose(&this, group);
            });
            menu.append(&row);
        }
        popover.set_child(Some(&menu));
        popover
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

    /// The charts on screen, written down as a chartbook.
    ///
    /// The name is the one thing nothing on screen carries, so it is handed
    /// in by whoever already knows it. Everything else, the rail included, is
    /// read off the window as it stands.
    fn capture_book(&self, name: String) -> Chartbook {
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
        Chartbook {
            name,
            layout: self.layout.borrow().clone(),
            focused: self.focused.get(),
            panes,
            watchlist: self.watchlist.borrow().as_ref().map(|rail| rail.active_watchlist()),
            sidebar_shown: Some(self.shows_sidebar()),
            sidebar_width: Some(self.sidebar_width.get()),
        }
    }

    /// Write every chartbook down, so a window comes back as it was left.
    ///
    /// The book on screen is the only one that can have changed, so it is
    /// folded back into the list first. Everything else is already data.
    fn save_workspace(self: &Rc<Self>) {
        {
            let mut books = self.books.borrow_mut();
            let active = self.active.get().min(books.len().saturating_sub(1));
            let name = books.get(active).map(|book| book.name.clone()).unwrap_or_default();
            let live = self.capture_book(name);
            match books.get_mut(active) {
                Some(book) => *book = live,
                None => books.push(live),
            }
            self.active.set(active);
        }
        let workspace =
            Workspace { books: self.books.borrow().clone(), active: self.active.get() };
        if let Ok(json) = serde_json::to_string(&workspace) {
            self.store.set_setting(SETTING_WORKSPACE, &json);
        }
    }

    /// Put the chartbooks back. `false` when there was nothing written down,
    /// or when what was written cannot be rebuilt.
    fn restore_workspace(self: &Rc<Self>) -> bool {
        let Some(json) = self.store.setting(SETTING_WORKSPACE) else { return false };
        let Some(workspace) = parse_workspace(&json) else { return false };
        let active = workspace.active.min(workspace.books.len() - 1);
        *self.books.borrow_mut() = workspace.books;
        self.active.set(active);
        if !self.materialise_book(active) {
            // Nothing partial: a window that could not rebuild what it was
            // given starts over with one chart rather than some of one.
            self.books.borrow_mut().clear();
            return false;
        }
        true
    }

    /// Build a chartbook into widgets, replacing whatever is on screen.
    ///
    /// `false` when it cannot be rebuilt — a layout naming panes that are not
    /// there is worse than starting over with one chart.
    fn materialise_book(self: &Rc<Self>, index: usize) -> bool {
        let Some(book) = self.books.borrow().get(index).cloned() else { return false };
        if book.panes.is_empty() {
            return false;
        }
        // A build before this one wrote arrangements whose every quadrant named
        // the same pane, so a stored tree is not trusted to be one. Repeated
        // leaves come out before anything is built from them, which turns an
        // unmountable grid back into the chart it actually described.
        let Some(book_layout) = book.layout.deduped() else { return false };
        // Taken before the panes are consumed below, and applied after the
        // charts are up: showing the rail moves a chart's own corner out from
        // under the window's controls, which needs the charts to exist.
        let rail = (book.watchlist, book.sidebar_shown, book.sidebar_width);
        let wanted = book_layout.leaves();
        if wanted.is_empty() || wanted.iter().any(|id| !book.panes.iter().any(|p| p.id == *id)) {
            return false;
        }

        self.panes.borrow_mut().clear();
        // Filling the window is a way of looking at one arrangement, so it
        // does not survive being shown a different one.
        self.maximized.set(None);
        let mut restored: Vec<(Rc<ChartPane>, StoredPane)> = Vec::new();
        for stored in book.panes {
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
        // The counter is never wound back, because the books that are not on
        // screen still name panes by the ids they were built with — which is
        // exactly why the whole tree is relabelled at once. The new ids overlap
        // the stored ones, so renaming them one at a time would rename leaves
        // an earlier step had just written.
        let ids: HashMap<u32, u32> =
            restored.iter().map(|(pane, stored)| (stored.id, pane.id)).collect();
        let mut focused = restored.first().map(|(pane, _)| pane.id).unwrap_or(1);
        for (pane, stored) in &restored {
            if stored.id == book.focused {
                focused = pane.id;
            }
        }
        *self.layout.borrow_mut() = book_layout.relabel(&ids);
        self.focused.set(focused);
        self.rebuild_layout();

        for (pane, stored) in &restored {
            if let Some(instrument) = self.index.find(&stored.symbol, stored.suffix.as_deref()) {
                self.show_in(pane, instrument.clone());
            }
        }
        self.sync_header();
        self.rebuild_indicator_legend();
        self.apply_sidebar(rail.0, rail.1, rail.2);
        true
    }

    /// Put the rail back the way a chartbook left it.
    ///
    /// A book that says nothing about the rail was written before the rail
    /// belonged to one, and takes the window's settings — the same ones a
    /// window with nothing written down at all opens with.
    fn apply_sidebar(
        self: &Rc<Self>,
        watchlist: Option<i64>,
        shown: Option<bool>,
        width: Option<i32>,
    ) {
        let rail = self.watchlist.borrow().as_ref().cloned();
        if let (Some(rail), Some(id)) = (rail, watchlist) {
            rail.set_active_watchlist(id);
        }
        self.set_rail_shown(shown.unwrap_or_else(|| self.store.setting_bool(SHOW_WATCHLIST, true)));
        self.want_sidebar_width(width.unwrap_or_else(|| self.stored_sidebar_width()));
    }

    // -- chartbooks --------------------------------------------------------

    /// What a book is called in the strip. Unnamed books are called by where
    /// they sit, so the row always reads in order.
    fn book_label(&self, index: usize) -> String {
        match self.books.borrow().get(index) {
            Some(book) if !book.name.is_empty() => book.name.clone(),
            _ => default_book_name(index + 1),
        }
    }

    /// Throw away the charts on screen and start again with one.
    ///
    /// For a brand new chartbook, and for the one case a stored book cannot
    /// be rebuilt: either way the window has to end up with a chart in it.
    fn mount_single_chart(self: &Rc<Self>, instrument: Option<Instrument>) {
        // Built like the chart you were looking at, and like the first chart
        // of a fresh install when there was not one — the window can reach
        // here with nothing on screen to copy.
        let from = self.panes.borrow().iter().find(|p| p.id == self.focused.get()).cloned();
        let store = &self.store;
        let timeframe = from.as_ref().map(|p| p.timeframe.get()).unwrap_or_else(|| {
            store
                .setting(LAST_TIMEFRAME)
                .and_then(|key| Timeframe::parse(&key))
                .unwrap_or(Timeframe::days(1))
        });
        let bar_style = from.as_ref().map(|p| p.bar_style.get()).unwrap_or_else(|| {
            store
                .setting(SETTING_BAR_STYLE)
                .and_then(|key| BarStyle::from_key(&key))
                .unwrap_or_default()
        });
        let session = from.as_ref().map(|p| p.session.get()).unwrap_or_else(|| {
            store
                .setting(crate::ui::chart_settings::SETTING_SESSION)
                .and_then(|key| Session::from_key(&key))
                .unwrap_or_default()
        });
        let show_grid = from
            .as_ref()
            .map(|p| p.show_grid.get())
            .unwrap_or_else(|| store.setting_bool(SETTING_SHOW_GRID, true));
        self.panes.borrow_mut().clear();
        self.maximized.set(None);
        // A new book opens on the indicators you have chosen as your default,
        // not on whatever the last chart happened to be carrying: a fresh
        // arrangement is a fresh start, which is the reason to open one.
        let pane =
            self.new_pane(
            timeframe,
            self.store.indicators(),
            bar_style,
            session,
            show_grid,
            LinkGroup::Group(1),
        );
        *self.layout.borrow_mut() = Node::leaf(pane.id);
        self.focused.set(pane.id);
        self.rebuild_layout();
        if let Some(instrument) = instrument {
            self.show_in(&pane, instrument);
        }
        self.sync_header();
        self.rebuild_indicator_legend();
    }

    /// Open a new chartbook, showing one chart on whatever you were looking
    /// at.
    ///
    /// One chart rather than a copy of the arrangement you were in: splitting
    /// is the thing that duplicates a chart in this app, and a new book you
    /// have to dismantle before you can use it is not a new book.
    /// Ask what the new chartbook is called, and which list sits beside it.
    ///
    /// A dialog rather than the inline tab the strip uses for renaming,
    /// because the strip is not there yet: going from one book to two is the
    /// moment it appears, and a caret arriving in a twenty-one pixel row at
    /// the bottom edge of the window is easy to miss entirely.
    pub fn new_chartbook(self: &Rc<Self>) {
        let proposed = default_book_name(self.books.borrow().len() + 1);
        let lists = self
            .watchlist
            .borrow()
            .as_ref()
            .map(|rail| rail.watchlists())
            .unwrap_or_default();

        let name = adw::EntryRow::new();
        name.set_title("Name");
        name.set_text(&proposed);

        let chosen = adw::ComboRow::new();
        chosen.set_title("Watchlist");
        let names: Vec<&str> = lists.iter().map(|(_, name)| name.as_str()).collect();
        chosen.set_model(Some(&gtk::StringList::new(&names)));
        let at = lists.iter().position(|(id, _)| *id == DEFAULT_WATCHLIST).unwrap_or(0);
        chosen.set_selected(at as u32);

        let fresh = adw::SwitchRow::new();
        fresh.set_title("Create new watchlist");
        fresh.set_subtitle("An empty one, named after the chartbook");

        // Disabled rather than hidden while a new list is being made: a
        // control that greys out still says what the other answer would have
        // been, and one that vanishes leaves you wondering what moved.
        let picker = chosen.clone();
        fresh.connect_active_notify(move |switch| picker.set_sensitive(!switch.is_active()));

        let rows = adw::PreferencesGroup::new();
        rows.add(&name);
        rows.add(&chosen);
        rows.add(&fresh);

        let dialog = adw::AlertDialog::new(Some("New chartbook"), None);
        dialog.set_extra_child(Some(&rows));
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("create", "Create");
        dialog.set_response_appearance("create", adw::ResponseAppearance::Suggested);
        // Creating is not destroying, so Return may finish it — unlike the
        // alerts that delete something, which default to Cancel on purpose.
        dialog.set_default_response(Some("create"));
        dialog.set_close_response("cancel");

        // Finishing from the keyboard, by the one route that creates a
        // chartbook. The binding has no emitter for this signal, so the signal
        // is emitted by name — and the dialog is then closed by hand, because
        // only a real response does that for us. A button press arrives at the
        // same handler having already closed itself.
        let finish: Rc<dyn Fn()> = {
            let dialog = dialog.clone();
            Rc::new(move || {
                dialog.emit_by_name::<()>("response", &[&"create"]);
                dialog.close();
            })
        };

        let committing = finish.clone();
        crate::ui::dialogs::commit_on_ctrl_enter(&dialog, move || committing());

        // An EntryRow keeps Return for itself and emits this instead, so the
        // dialog's default response never hears the key that was meant for
        // it. Typing a name and pressing Return is one gesture and should
        // finish one thing.
        let typing = finish.clone();
        name.connect_entry_activated(move |_| typing());

        let this = self.clone();
        // Three ways in and one chartbook out: the key paths emit the response
        // and then close, and a close carries its own response behind it.
        let done = Cell::new(false);
        dialog.connect_response(None, move |_, response| {
            if response != "create" || done.replace(true) {
                return;
            }
            let typed = name.text().trim().to_string();
            let label = if typed.is_empty() { proposed.clone() } else { typed };
            let watchlist = if fresh.is_active() {
                // Made only now, so cancelling leaves no list behind that
                // nothing points at.
                match this.store.add_watchlist(&label) {
                    Some(id) => Some(id),
                    None => return,
                }
            } else {
                lists.get(chosen.selected() as usize).map(|(id, _)| *id)
            };
            this.open_chartbook(&label, watchlist);
        });
        dialog.present(Some(&self.window));
    }

    fn open_chartbook(self: &Rc<Self>, name: &str, watchlist: Option<i64>) {
        self.save_workspace();
        let instrument = self.focused_pane().instrument.borrow().clone();
        let at = self.books.borrow().len();
        // A placeholder, so that `active` names a book that exists before
        // anything else looks. Saving at the end writes the real one.
        self.books.borrow_mut().push(Chartbook {
            name: if name == default_book_name(at + 1) { String::new() } else { name.to_string() },
            layout: Node::leaf(0),
            focused: 0,
            panes: Vec::new(),
            watchlist,
            sidebar_shown: None,
            sidebar_width: None,
        });
        self.active.set(at);
        self.mount_single_chart(instrument);
        // Through the one path a book's rail always arrives by, so a list
        // chosen here and a list restored later cannot disagree.
        self.apply_sidebar(watchlist, None, None);
        self.rebuild_book_strip();
        self.save_workspace();
    }

    /// Ask before taking a chartbook and its charts away.
    ///
    /// The only one does not go: there would be nothing left to show, the
    /// same guard closing the last chart already keeps.
    pub fn close_chartbook(self: &Rc<Self>) {
        if self.books.borrow().len() < 2 {
            return;
        }
        let index = self.active.get();
        let charts = self.layout.borrow().leaves().len();
        let body = format!(
            "{} and its {} will be removed. This cannot be undone.",
            self.book_label(index),
            match charts {
                1 => "chart".to_string(),
                n => format!("{n} charts"),
            },
        );

        let dialog = adw::AlertDialog::new(Some("Remove chartbook"), Some(&body));
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("remove", "Remove");
        dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");

        let this = self.clone();
        dialog.connect_response(None, move |_, response| {
            if response == "remove" {
                this.remove_chartbook(index);
            }
        });
        dialog.present(Some(&self.window));
    }

    fn remove_chartbook(self: &Rc<Self>, index: usize) {
        if self.books.borrow().len() < 2 {
            return;
        }
        // The book that was asked about may not be the one on screen any more.
        if index != self.active.get() {
            self.activate_book(index);
        }
        let index = self.active.get();
        self.books.borrow_mut().remove(index);
        // The one that slid into its place, or the new last one.
        let next = index.min(self.books.borrow().len() - 1);
        self.active.set(next);
        if !self.materialise_book(next) {
            self.mount_single_chart(None);
        }
        self.rebuild_book_strip();
        self.save_workspace();
    }

    /// Show a different chartbook, writing the one on screen back first.
    pub fn activate_book(self: &Rc<Self>, index: usize) {
        if index >= self.books.borrow().len() || index == self.active.get() {
            return;
        }
        self.save_workspace();
        self.active.set(index);
        if !self.materialise_book(index) {
            self.mount_single_chart(None);
        }
        self.rebuild_book_strip();
        self.save_workspace();
    }

    /// Walk to the chartbook before or after this one, round the ends.
    pub fn step_chartbook(self: &Rc<Self>, delta: i32) {
        let count = self.books.borrow().len();
        if count < 2 {
            return;
        }
        let next = (self.active.get() as i32 + delta).rem_euclid(count as i32) as usize;
        self.activate_book(next);
    }

    /// Give a chartbook a name of its own, or take it back to its number.
    fn rename_book(self: &Rc<Self>, index: usize, name: &str) {
        let name = name.trim();
        {
            let mut books = self.books.borrow_mut();
            let Some(book) = books.get_mut(index) else { return };
            // Typing the name it already shows is not naming it, so an
            // emptied box hands it back to its position rather than leaving
            // a book with a name that stops matching where it sits.
            book.name = if name.is_empty() || name == default_book_name(index + 1) {
                String::new()
            } else {
                name.to_string()
            };
        }
        self.rebuild_book_strip();
        self.save_workspace();
    }

    /// Draw the row of chartbook tabs, or no row at all.
    fn rebuild_book_strip(self: &Rc<Self>) {
        while let Some(child) = self.book_strip.first_child() {
            self.book_strip.remove(&child);
        }
        let count = self.books.borrow().len();
        // One chartbook is how the window has always looked, and a strip
        // naming the only thing there is would be a row of furniture saying
        // nothing. It arrives with the second book and leaves with it.
        self.book_strip.set_visible(count > 1);
        if count < 2 {
            return;
        }
        for index in 0..count {
            self.book_strip.append(&self.build_book_tab(index));
        }
    }

    /// One tab: click to switch, double-click to rename.
    fn build_book_tab(self: &Rc<Self>, index: usize) -> gtk::Widget {
        let tab = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        tab.add_css_class("chartbook-tab");
        if index == self.active.get() {
            tab.add_css_class("active");
        }
        let label = gtk::Label::new(Some(&self.book_label(index)));
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.set_max_width_chars(18);
        tab.append(&label);

        let click = gtk::GestureClick::new();
        let this = self.clone();
        click.connect_pressed(move |_, presses, _, _| {
            // The single press that precedes a double is harmless: it
            // switches to the book you are about to rename, which is where
            // you wanted to be anyway.
            if presses >= 2 {
                this.begin_rename(index);
            } else {
                this.activate_book(index);
            }
        });
        tab.add_controller(click);

        // Right-clicking a tab switches to it first, then offers the menu for
        // it. The app already settles this ambiguity the same way — clicking
        // another chart's corner focuses that chart before maximizing it —
        // and it is what keeps the accelerators the rows advertise honest,
        // since both of them act on the book that is open.
        let menu = gtk::GestureClick::new();
        menu.set_button(gtk::gdk::BUTTON_SECONDARY);
        let this = self.clone();
        menu.connect_pressed(move |_, _, x, y| {
            this.activate_book(index);
            // Found in the strip rather than captured: switching books redrew
            // it, and the tab this gesture is attached to is no longer the
            // tab on screen.
            let Some(tab) = this.book_tab(this.active.get()) else { return };
            let model = gio::Menu::new();
            shortcuts::append(&model, "Rename", "win.rename-chartbook");
            shortcuts::append(&model, "Remove", "win.close-chartbook");
            popup_menu(&model, &tab, x, y);
        });
        tab.add_controller(menu);
        tab.upcast()
    }

    /// The tab sitting at `index` in the strip right now.
    fn book_tab(&self, index: usize) -> Option<gtk::Box> {
        let mut child = self.book_strip.first_child()?;
        for _ in 0..index {
            child = child.next_sibling()?;
        }
        child.downcast::<gtk::Box>().ok()
    }

    /// Put the chartbook on screen into the same editable tab a double-click
    /// opens, so there is one rename rather than two that have to agree.
    ///
    /// Which book that is, is read now rather than captured when the action
    /// was wired: the active one moves, and this area has already produced
    /// one bug from holding on to something that had stopped being true.
    ///
    /// A single chartbook has no strip and therefore no tab, and this does
    /// nothing — the same bargain the window already strikes with Maximize
    /// and Close chart. Offering a dialog for that one case would make it a
    /// different interaction from the ordinary one, which is the thing
    /// sharing the inline edit is for.
    fn rename_active_book(self: &Rc<Self>) {
        self.begin_rename(self.active.get());
    }

    /// Swap a tab's name for a box to type a new one in.
    ///
    /// Found in the strip rather than handed in, because the press that opens
    /// a rename has already switched books, and switching redraws the strip —
    /// the tab the gesture was attached to is not the tab on screen.
    fn begin_rename(self: &Rc<Self>, index: usize) {
        let Some(tab) = self.book_tab(index) else { return };
        let Some(label) = tab.first_child().and_downcast::<gtk::Label>() else { return };
        let entry = gtk::Entry::new();
        entry.add_css_class("chartbook-rename");
        entry.set_text(&self.book_label(index));
        entry.set_width_chars(12);
        entry.set_max_width_chars(18);
        tab.remove(&label);
        tab.append(&entry);
        entry.grab_focus();
        entry.select_region(0, -1);

        // Enter commits, and so does clicking away — but only one of them
        // does, because committing twice would rebuild the strip underneath
        // the box that is still handing in its text.
        let done = Rc::new(Cell::new(false));
        let commit = {
            let this = self.clone();
            let done = done.clone();
            move |entry: &gtk::Entry| {
                if done.replace(true) {
                    return;
                }
                this.rename_book(index, &entry.text());
            }
        };

        let on_activate = commit.clone();
        entry.connect_activate(move |entry| on_activate(entry));

        let focus = gtk::EventControllerFocus::new();
        let entry_weak = entry.downgrade();
        let on_leave = commit.clone();
        focus.connect_leave(move |_| {
            if let Some(entry) = entry_weak.upgrade() {
                on_leave(&entry);
            }
        });
        entry.add_controller(focus);

        // Escape puts the name back, which means leaving the book alone and
        // drawing the strip again.
        let keys = gtk::EventControllerKey::new();
        let this = self.clone();
        let done_on_escape = done.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            if key != gtk::gdk::Key::Escape {
                return glib::Propagation::Proceed;
            }
            done_on_escape.set(true);
            this.rebuild_book_strip();
            glib::Propagation::Stop
        });
        entry.add_controller(keys);
    }

    /// Draw `ts` on every chart linked with `from`, and on none when it is
    /// unlinked: an unlinked chart is deliberately somewhere else.
    fn echo_crosshair(self: &Rc<Self>, from: u32, echo: Option<Echo>) {
        if !self.store.setting_bool(SETTING_SYNC_CROSSHAIR, true) {
            return;
        }
        let source = self.pane(from).map(|p| p.linked.get()).unwrap_or_default();
        for pane in self.panes.borrow().iter() {
            if pane.id == from {
                continue;
            }
            // Same group, or no line: a chart in another group is tracking a
            // different symbol, and a crosshair from this one would be
            // pointing at a bar that is not the bar under the pointer.
            let show = source.is_linked() && pane.linked.get() == source;
            pane.view.set_echo(if show { echo } else { None });
        }
    }

    /// Stop every echo, for when the setting is switched off or the panes
    /// change under it.
    pub fn clear_echoes(self: &Rc<Self>) {
        for pane in self.panes.borrow().iter() {
            pane.view.set_echo(None);
        }
    }

    /// What a group is already showing, ignoring `except`. `None` for the
    /// unlinked group, which is not a pool and has nothing to agree on.
    fn group_instrument(&self, group: LinkGroup, except: u32) -> Option<Instrument> {
        if !group.is_linked() {
            return None;
        }
        self.panes
            .borrow()
            .iter()
            .filter(|p| p.id != except && p.linked.get() == group)
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
        let books = gio::Menu::new();
        shortcuts::append(&books, "New chartbook", "win.new-chartbook");
        menu.append_section(None, &books);
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
        let this = self.clone();
        toggle.connect_toggled(move |toggle| {
            if let Some(rail) = split_weak.upgrade().and_then(|split| split.end_child()) {
                rail.set_visible(toggle.is_active());
            }
            // Also the window's own, which is what seeds the next chartbook
            // and what a window with nothing written down opens with.
            store.set_setting_bool(SHOW_WATCHLIST, toggle.is_active());
            // Closing the rail drops these controls onto the top-right chart,
            // where its own corner is.
            this.sync_corner_clearance();
            this.save_soon();
        });

        *self.watchlist_toggle.borrow_mut() = Some(toggle.clone());

        // Ctrl+B is not a toggle. A rail you can see but cannot drive with the
        // arrow keys is a rail you still have to reach for the mouse to use, so
        // the first press puts the keyboard in it and only the second puts it
        // away. `toggle_watchlist` holds that decision.
        let action = gio::SimpleAction::new("watchlist", None);
        let this = self.clone();
        action.connect_activate(move |_, _| this.toggle_watchlist());
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
            move |instrument| this.show_from_rail(instrument),
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

        let new_book = gio::SimpleAction::new("new-chartbook", None);
        let this = self.clone();
        new_book.connect_activate(move |_, _| this.new_chartbook());
        self.window.add_action(&new_book);

        let rename_book = gio::SimpleAction::new("rename-chartbook", None);
        let this = self.clone();
        rename_book.connect_activate(move |_, _| this.rename_active_book());
        self.window.add_action(&rename_book);

        let close_book = gio::SimpleAction::new("close-chartbook", None);
        let this = self.clone();
        close_book.connect_activate(move |_, _| this.close_chartbook());
        self.window.add_action(&close_book);

        let keys = gtk::EventControllerKey::new();
        let this = self.clone();
        keys.connect_key_pressed(move |_, key, _, state| {
            use gtk::gdk::Key;
            let ctrl = state.contains(gtk::gdk::ModifierType::CONTROL_MASK);
            let alt = state.contains(gtk::gdk::ModifierType::ALT_MASK);
            let shift = state.contains(gtk::gdk::ModifierType::SHIFT_MASK);

            match key {
                // Ctrl+L: follow the rail, or stop following it.
                // Ctrl+L: in or out of the neutral group, which is the one a
                // chart that was simply "linked" has always been in. Picking
                // one of the other eight is a thing you do by looking.
                Key::l | Key::L if ctrl => {
                    let pane = this.focused_pane();
                    let next = match pane.linked.get() {
                        LinkGroup::None => LinkGroup::Group(1),
                        _ => LinkGroup::None,
                    };
                    this.set_pane_link_group(pane.id, next);
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
                // Ctrl+W closes the chartbook, and closes the window when
                // that was the only one — which is what it has always done,
                // and what every tabbed application does with the last tab.
                // Ctrl+Q skips the question and quits.
                Key::q if ctrl => {
                    this.window.close();
                    return glib::Propagation::Stop;
                }
                // Walk the chart without leaving it: resolutions sideways,
                // symbols up and down, whichever pane has the keyboard.
                //
                // Sideways takes Shift as well, so that the modifier a window
                // manager is most likely to have already claimed is not the
                // one standing between you and the next resolution.
                Key::Left if ctrl && alt && shift => {
                    this.step_timeframe(-1);
                    return glib::Propagation::Stop;
                }
                Key::Right if ctrl && alt && shift => {
                    this.step_timeframe(1);
                    return glib::Propagation::Stop;
                }
                // And sideways without Shift walks the chartbooks, which is
                // the larger of the two things sideways could mean: past the
                // edge of this arrangement and into the next.
                Key::Left if ctrl && alt => {
                    this.step_chartbook(-1);
                    return glib::Propagation::Stop;
                }
                Key::Right if ctrl && alt => {
                    this.step_chartbook(1);
                    return glib::Propagation::Stop;
                }
                // Up and down with Shift walks the watchlists, but only with
                // the keyboard in the rail: it is the one binding in this
                // grid that is not global, and over a chart these keys belong
                // to whatever else wants them rather than to a list you are
                // not looking at.
                Key::Up | Key::Down if ctrl && alt && shift => {
                    if !this.rotate_watchlist(if key == Key::Up { -1 } else { 1 }) {
                        return glib::Propagation::Proceed;
                    }
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
            // The group is the number and the colour is the theme's answer
            // about it, which has just changed.
            let group = pane.linked.get();
            pane.set_link_group(group, group.colour(&theme));
        }
        if let Some(rail) = self.watchlist.borrow().as_ref() {
            let group = rail.link_group();
            rail.set_link_group(group, group.colour(&theme));
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

        let sections: [(&str, &[(&str, &str)]); 5] = [
            (
                "Finding things",
                &[
                    ("Type a letter", "Find a symbol"),
                    ("Type a number", "Set the resolution"),
                    ("Ctrl+K", "Find a symbol"),
                    ("Ctrl+I", "Indicators"),
                    ("Ctrl+Shift+I", "Add an indicator"),
                    ("Ctrl+Shift+S", "Chart settings"),
                    ("Ctrl+,", "Preferences"),
                    ("F10", "Main menu"),
                    ("? · Ctrl+?", "This list"),
                    ("Ctrl+Q", "Quit"),
                ],
            ),
            (
                "Chartbooks",
                &[
                    ("Ctrl+N", "New chartbook"),
                    ("Ctrl+Shift+R", "Rename this chartbook"),
                    ("Ctrl+Shift+X", "Remove this chartbook"),
                    ("Ctrl+Alt+← →", "Previous or next chartbook"),
                    ("Double-click a tab", "Rename it"),
                    ("Right-click a tab", "Rename or remove it"),
                ],
            ),
            (
                "Watchlist",
                &[
                    ("Ctrl+B", "Open, focus, then close"),
                    ("↑ ↓", "Next or previous symbol"),
                    ("Ctrl+↑ ↓", "Next or previous section"),
                    // The one binding in this grid that is not global, so the
                    // row says where it works: pressed over a chart it does
                    // nothing, and nothing is hard to ask a question about.
                    ("Ctrl+Alt+Shift+↑ ↓", "Previous or next watchlist, in the sidebar"),
                    ("Delete", "Remove the symbol"),
                ],
            ),
            (
                "Charts",
                &[
                    ("Ctrl+H", "Split horizontally"),
                    ("Ctrl+V", "Split vertically"),
                    ("Ctrl+X", "Close this chart"),
                    ("Ctrl+M", "Give this chart the window, or put it back"),
                    ("Ctrl+L", "Link this chart to the watchlist, or unlink it"),
                    ("Alt+← → ↑ ↓", "Focus the chart that way"),
                ],
            ),
            (
                "Chart",
                &[
                    ("Ctrl+Alt+Shift+← →", "Previous or next resolution"),
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
        self.spread(instrument, focused.linked.get(), Some(focused.id));
    }

    /// Chart what the rail just picked.
    ///
    /// The chart you are looking at always moves — that is what clicking a
    /// symbol in the list has always meant — and so does every chart in the
    /// rail's own group, wherever it is.
    pub fn show_from_rail(self: &Rc<Self>, instrument: Instrument) {
        let group =
            self.watchlist.borrow().as_ref().map(|rail| rail.link_group()).unwrap_or_default();
        self.spread(instrument, group, Some(self.focused.get()));
    }

    /// Put `instrument` on `seed`, on every live chart in `group`, and on
    /// every chart in `group` that belongs to a chartbook which is not on
    /// screen.
    ///
    /// The books that are away are data rather than widgets, so they are
    /// written to directly. Skipping them would make a group mean "the charts
    /// in this group I can currently see", which is a rule nobody could
    /// predict from one chartbook away.
    fn spread(self: &Rc<Self>, instrument: Instrument, group: LinkGroup, seed: Option<u32>) {
        if let Some(pane) = seed.and_then(|id| self.pane(id)) {
            self.show_in(&pane, instrument.clone());
        }
        if group.is_linked() {
            let others: Vec<Rc<ChartPane>> = self
                .panes
                .borrow()
                .iter()
                .filter(|p| Some(p.id) != seed && p.linked.get() == group)
                .cloned()
                .collect();
            for pane in others {
                self.show_in(&pane, instrument.clone());
            }
            self.spread_to_stored_books(&instrument, group);
        }
        self.save_workspace();
    }

    /// Move a group's charts in the books that are not on screen.
    ///
    /// The active book is skipped: its charts are the live ones, and saving
    /// writes them back over whatever was stored anyway.
    fn spread_to_stored_books(&self, instrument: &Instrument, group: LinkGroup) {
        let active = self.active.get();
        for (index, book) in self.books.borrow_mut().iter_mut().enumerate() {
            if index == active {
                continue;
            }
            for pane in book.panes.iter_mut().filter(|p| p.linked == group) {
                pane.symbol = instrument.symbol.clone();
                pane.suffix = instrument.suffix.clone();
            }
        }
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
        // Both only mean anything with something else on screen: there is
        // nothing to close down to, and nothing to grow over.
        if self.panes.borrow().len() > 1 {
            let open = self.maximized.get() == Some(self.focused.get());
            let what = if open { pane::RESTORE } else { pane::MAXIMIZE };
            shortcuts::append(&layout, what, "chart.maximize");
            shortcuts::append(&layout, "Close chart", "chart.close");
        }
        menu.append_section(None, &layout);

        let rest = gio::Menu::new();
        let groups = gio::Menu::new();
        for group in link::ALL {
            let item = gio::MenuItem::new(Some(group.label()), None);
            item.set_action_and_target_value(
                Some("chart.link-group"),
                Some(&group_state(group)),
            );
            groups.append_item(&item);
        }
        rest.append_submenu(Some("Link group"), &groups);
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

        let maximize = gio::SimpleAction::new("maximize", None);
        let this = self.clone();
        maximize.connect_activate(move |_, _| this.toggle_maximized());
        actions.add_action(&maximize);

        let close = gio::SimpleAction::new("close", None);
        let this = self.clone();
        close.connect_activate(move |_, _| this.close_focused());
        actions.add_action(&close);

        // Stateful over the group number, so the menu draws a radio mark
        // beside the one this chart is in rather than a tick that can only
        // say linked or not.
        let linked = gio::SimpleAction::new_stateful(
            "link-group",
            Some(glib::VariantTy::INT32),
            &group_state(self.focused_pane().linked.get()),
        );
        let this = self.clone();
        linked.connect_activate(move |action, target| {
            let Some(number) = target.and_then(|t| t.get::<i32>()) else { return };
            action.set_state(&number.to_variant());
            this.set_pane_link_group(this.focused.get(), LinkGroup::numbered(number.max(0) as u8));
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

    /// Is the rail on screen?
    ///
    /// Asked of the corner button rather than of the rail itself, because a
    /// widget reports itself invisible until every ancestor is on screen —
    /// and a chartbook is written down before the window has been shown,
    /// which recorded the rail as closed on every launch. The button knows
    /// from the moment it is built, and it is already the one thing that
    /// opens and closes the rail.
    fn shows_sidebar(&self) -> bool {
        match self.watchlist_toggle.borrow().as_ref() {
            Some(toggle) => toggle.is_active(),
            None => self.split.end_child().map(|rail| rail.is_visible()).unwrap_or(false),
        }
    }

    fn set_show_sidebar(&self, show: bool) {
        if let Some(rail) = self.split.end_child() {
            rail.set_visible(show);
        }
    }

    /// Show or hide the rail the way a click does.
    ///
    /// Through the corner button rather than around it: the button's handler
    /// is what remembers the choice and what moves a chart's own corner out
    /// from under the window's controls. Setting the rail directly would leave
    /// the button looking unpressed over an open rail.
    fn set_rail_shown(&self, shown: bool) {
        match self.watchlist_toggle.borrow().as_ref() {
            Some(toggle) => toggle.set_active(shown),
            None => self.set_show_sidebar(shown),
        }
    }

    /// The width the window was last left at, for a book that has no opinion
    /// and for a window with nothing written down at all.
    fn stored_sidebar_width(&self) -> i32 {
        self.store
            .setting(SETTING_SIDEBAR_WIDTH)
            .and_then(|v| v.parse::<i32>().ok())
            .unwrap_or(DEFAULT_SIDEBAR_WIDTH)
            .clamp(MIN_SIDEBAR_WIDTH, MAX_SIDEBAR_WIDTH)
    }

    /// Ask for a rail this wide — now if the window has a size, and otherwise
    /// the moment it gets one.
    fn want_sidebar_width(self: &Rc<Self>, width: i32) {
        self.sidebar_width.set(width.clamp(MIN_SIDEBAR_WIDTH, MAX_SIDEBAR_WIDTH));
        self.place_sidebar();
    }

    /// Move the handle to where the wanted width puts it.
    ///
    /// A width rather than a handle position, because the position is
    /// measured from the other side and would move the rail every time the
    /// window was resized.
    fn place_sidebar(&self) {
        let total = self.split.width();
        let width = self.sidebar_width.get();
        if total > width + 200 {
            self.sidebar_placed.set(true);
            self.split.set_position(total - width);
        }
    }

    /// Follow the handle, and keep what it is dragged to.
    ///
    /// Nothing is recorded until a width has been put back once. A window
    /// that has not been laid out reports a position measured against no
    /// width at all, and recording that overwrote the rail every launch with
    /// whatever the default happened to produce.
    fn watch_sidebar_width(self: &Rc<Self>) {
        self.sidebar_width.set(self.stored_sidebar_width());
        self.place_sidebar();

        let this = self.clone();
        self.split.connect_map(move |_| {
            if !this.sidebar_placed.get() {
                this.place_sidebar();
            }
        });

        let this = self.clone();
        self.split.connect_position_notify(move |paned| {
            if !this.sidebar_placed.get() {
                this.place_sidebar();
                return;
            }
            if paned.width() <= 0 || paned.position() <= 0 {
                return;
            }
            let width =
                (paned.width() - paned.position()).clamp(MIN_SIDEBAR_WIDTH, MAX_SIDEBAR_WIDTH);
            if width == this.sidebar_width.get() {
                return;
            }
            this.sidebar_width.set(width);
            // Also the window's own, which is what seeds the next chartbook
            // and what a window with nothing written down opens at.
            this.store.set_setting(SETTING_SIDEBAR_WIDTH, &width.to_string());
            this.save_soon();
        });
    }

    /// Walk the rail to the watchlist before or after the one it is showing.
    ///
    /// `false` when the keyboard is not in the rail, so the key falls through
    /// to whatever else wants it. Matching and then doing nothing would eat
    /// Ctrl+Alt+Shift and the arrows everywhere in the app, which is worse
    /// than not having the binding at all.
    fn rotate_watchlist(self: &Rc<Self>, delta: i32) -> bool {
        let rail = self.watchlist.borrow().as_ref().cloned();
        let Some(rail) = rail.filter(|rail| rail.has_focus()) else { return false };
        rail.rotate_watchlist(delta);
        true
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
                self.set_rail_shown(true);
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
                self.set_rail_shown(false);
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
    /// Selected in the first chartbook whose watchlist holds it, rather than
    /// forced onto whichever chart happens to be focused: the bar widget is a
    /// way back into what you were already watching, and a click there that
    /// rewrote the chart in front of you would be a different feature.
    ///
    /// `false` when no list holds it, which leaves the caller to do the one
    /// thing that is still right — put the window up.
    pub fn show_named(self: &Rc<Self>, symbol: &str, suffix: Option<&str>) -> bool {
        let Some(instrument) = self.index.find(symbol, suffix) else {
            return false;
        };

        // The book you are in wins any tie. "The first where it is available"
        // says nothing about two books holding the same symbol, and being
        // moved out of what you were looking at because another book sorts
        // earlier would be the wrong half of that silence.
        let here = self.watchlist.borrow().as_ref().map(|rail| rail.active_watchlist());
        if here.is_some_and(|id| self.list_holds(id, symbol, suffix)) {
            return self.pick_in_rail(&instrument);
        }

        let elsewhere = {
            let books = self.books.borrow();
            books.iter().enumerate().find_map(|(index, book)| {
                let id = book.watchlist?;
                self.list_holds(id, symbol, suffix).then_some(index)
            })
        };
        let Some(index) = elsewhere else { return false };
        self.activate_book(index);
        // After the switch, never before: selecting looks through the rows the
        // rail is showing, and until it has been rebuilt those are the last
        // book's rows.
        self.pick_in_rail(&instrument)
    }

    fn list_holds(&self, watchlist: i64, symbol: &str, suffix: Option<&str>) -> bool {
        list_holds(&self.store, watchlist, symbol, suffix)
    }

    fn pick_in_rail(self: &Rc<Self>, instrument: &Instrument) -> bool {
        let rail = self.watchlist.borrow().as_ref().cloned();
        rail.is_some_and(|rail| rail.pick(instrument))
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
