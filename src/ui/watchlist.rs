//! The watchlist rail.
//!
//! One `ListBox` holding every section's header and every symbol, rather than
//! a list per section. That is what lets the arrow keys run from the bottom of
//! one section into the top of the next without the user noticing there was a
//! boundary — which is the whole point of a watchlist you navigate rather than
//! click.
//!
//! Prices shown here come from the cache only. The rail never issues a
//! request; the window prefetches around the selection instead.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use omacharts_engine::{Instrument, SearchIndex};

use crate::store::{Entry, Store, ROOT_SECTION};
use crate::ui::search::SymbolSearch;

/// What a new install starts with, so the rail is never an empty column.
pub const DEFAULTS: &[(&str, &[&str])] = &[
    ("Indexes", &["GSPC", "NDX", "DJI", "VIX"]),
    ("US Stocks", &["NVDA", "AAPL", "MSFT", "AMZN", "META", "GOOGL", "TSLA", "AMD"]),
    ("Futures", &["ES", "NQ", "GC", "CL"]),
    ("Currencies", &["EURUSD", "GBPUSD", "USDJPY", "AUDUSD", "USDCAD", "USDCHF"]),
    ("Crypto", &["BTC", "ETH", "SOL"]),
];

/// The last close and the move onto it, when the cache holds enough to say.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Quote {
    pub last: f64,
    pub change: f64,
    pub change_pct: f64,
}

pub type QuoteLookup = Rc<dyn Fn(&Instrument) -> Option<Quote>>;

/// A column of the rail.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Column {
    Symbol,
    Last,
    Change,
    ChangePct,
}

impl Column {
    pub const ALL: [Column; 4] = [Column::Symbol, Column::Last, Column::Change, Column::ChangePct];
    /// What a fresh install shows.
    pub const DEFAULT: [Column; 3] = [Column::Symbol, Column::Change, Column::ChangePct];

    pub fn label(self) -> &'static str {
        match self {
            Column::Symbol => "Symbol",
            Column::Last => "Last",
            Column::Change => "Chg",
            Column::ChangePct => "Chg%",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Column::Symbol => "symbol",
            Column::Last => "last",
            Column::Change => "change",
            Column::ChangePct => "change_pct",
        }
    }

    pub fn from_key(key: &str) -> Option<Column> {
        Column::ALL.into_iter().find(|c| c.key() == key)
    }

    fn width_chars(self) -> i32 {
        match self {
            Column::Symbol => 0,
            Column::Last => 9,
            Column::Change => 8,
            Column::ChangePct => 7,
        }
    }
}

/// Parse the stored column list, falling back to the default.
///
/// The symbol column is always present and always first: a row without it is
/// not a row, and no amount of configuration should let you hide what you are
/// looking at.
pub fn parse_columns(stored: Option<&str>) -> Vec<Column> {
    let mut columns: Vec<Column> = stored
        .map(|s| s.split(',').filter_map(|k| Column::from_key(k.trim())).collect())
        .unwrap_or_default();
    if columns.is_empty() {
        columns = Column::DEFAULT.to_vec();
    }
    columns.retain(|c| *c != Column::Symbol);
    let mut out = vec![Column::Symbol];
    out.extend(columns);
    out
}

pub fn columns_to_string(columns: &[Column]) -> String {
    columns.iter().map(|c| c.key()).collect::<Vec<_>>().join(",")
}

/// What a row in the list is.
#[derive(Clone)]
enum RowKind {
    Header { section_id: i64 },
    Entry {
        section_id: i64,
        entry: Entry,
        instrument: Instrument,
        /// The value labels, so a new quote can be written straight into them.
        cells: Vec<(Column, gtk::Label)>,
    },
}

pub struct Watchlist {
    pub widget: gtk::Box,
    list: gtk::ListBox,
    header: gtk::Box,
    store: Rc<Store>,
    index: Rc<SearchIndex>,
    quote: QuoteLookup,
    search: Rc<SymbolSearch>,
    on_pick: Rc<dyn Fn(Instrument)>,
    /// Parallel to the list's rows.
    rows: RefCell<Vec<RowKind>>,
    columns: RefCell<Vec<Column>>,
    /// Set while we are selecting a row ourselves, so rebuilding does not
    /// re-load the chart.
    quiet: Cell<bool>,
}

const SETTING_COLUMNS: &str = "watchlist_columns";

impl Watchlist {
    pub fn new(
        store: Rc<Store>,
        index: Rc<SearchIndex>,
        search: Rc<SymbolSearch>,
        quote: QuoteLookup,
        on_pick: impl Fn(Instrument) + 'static,
    ) -> Rc<Watchlist> {
        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::Single);
        list.add_css_class("navigation-sidebar");

        let scroller = gtk::ScrolledWindow::new();
        scroller.set_child(Some(&list));
        scroller.set_vexpand(true);
        scroller.set_hscrollbar_policy(gtk::PolicyType::Never);

        let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        header.set_margin_start(12);
        header.set_margin_end(10);
        header.set_margin_top(6);
        header.set_margin_bottom(4);

        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        widget.set_size_request(248, -1);
        widget.append(&header);
        widget.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        widget.append(&scroller);

        let columns = parse_columns(store.setting(SETTING_COLUMNS).as_deref());

        let watchlist = Rc::new(Watchlist {
            widget,
            list,
            header,
            store,
            index,
            quote,
            search,
            on_pick: Rc::new(on_pick),
            rows: RefCell::new(Vec::new()),
            columns: RefCell::new(columns),
            quiet: Cell::new(false),
        });

        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        actions.set_halign(gtk::Align::Center);
        actions.set_margin_top(4);
        actions.set_margin_bottom(4);

        let add_symbol = gtk::Button::from_icon_name("list-add-symbolic");
        add_symbol.set_tooltip_text(Some("Add a symbol"));
        add_symbol.add_css_class("flat");
        let this = watchlist.clone();
        add_symbol.connect_clicked(move |_| this.add_symbol_to(ROOT_SECTION));
        actions.append(&add_symbol);

        let add_section = gtk::Button::from_icon_name("folder-new-symbolic");
        add_section.set_tooltip_text(Some("New section"));
        add_section.add_css_class("flat");
        let this = watchlist.clone();
        add_section.connect_clicked(move |button| this.prompt_new_section(button));
        actions.append(&add_section);

        watchlist.widget.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        watchlist.widget.append(&actions);

        watchlist.wire_selection();
        watchlist.wire_keys();
        watchlist.rebuild();
        watchlist
    }

    /// Put the keyboard on the rail, landing on something selected so the
    /// arrows have somewhere to go from.
    pub fn grab_focus(self: &Rc<Self>) {
        let row = self.list.selected_row().or_else(|| {
            (0..)
                .map_while(|i| self.list.row_at_index(i))
                .find(|row| row.is_selectable() && row.is_visible())
        });
        if let Some(row) = row {
            self.list.select_row(Some(&row));
            row.grab_focus();
        } else {
            self.list.grab_focus();
        }
    }

    /// Does the keyboard currently live here?
    pub fn has_focus(&self) -> bool {
        self.list.focus_child().is_some() || self.list.has_focus()
    }

    pub fn columns(&self) -> Vec<Column> {
        self.columns.borrow().clone()
    }

    pub fn set_columns(self: &Rc<Self>, columns: Vec<Column>) {
        let columns = parse_columns(Some(&columns_to_string(&columns)));
        self.store.set_setting(SETTING_COLUMNS, &columns_to_string(&columns));
        *self.columns.borrow_mut() = columns;
        self.rebuild();
    }

    /// Every symbol on the rail, in the order the arrow keys walk them.
    ///
    /// Collapsed sections are excluded: what you cannot see, you cannot arrow
    /// onto, and prefetching it would spend the request budget on rows that
    /// are not there.
    pub fn flat_order(&self) -> Vec<Instrument> {
        self.rows
            .borrow()
            .iter()
            .enumerate()
            .filter_map(|(at, kind)| match kind {
                RowKind::Entry { instrument, .. } => {
                    let visible = self
                        .list
                        .row_at_index(at as i32)
                        .map(|row| row.is_visible())
                        .unwrap_or(false);
                    visible.then(|| instrument.clone())
                }
                RowKind::Header { .. } => None,
            })
            .collect()
    }

    /// Highlight a symbol without loading it again.
    pub fn highlight(self: &Rc<Self>, instrument: &Instrument) {
        let target = self.rows.borrow().iter().position(|kind| match kind {
            RowKind::Entry { instrument: i, .. } => {
                i.symbol == instrument.symbol && i.suffix == instrument.suffix
            }
            RowKind::Header { .. } => false,
        });
        let Some(at) = target else { return };
        let Some(row) = self.list.row_at_index(at as i32) else { return };
        self.quiet.set(true);
        self.list.select_row(Some(&row));
        self.quiet.set(false);
    }

    fn wire_selection(self: &Rc<Self>) {
        let this = self.clone();
        self.list.connect_row_selected(move |_, row| {
            if this.quiet.get() {
                return;
            }
            let Some(row) = row else { return };
            let at = row.index().max(0) as usize;
            let picked = match this.rows.borrow().get(at) {
                Some(RowKind::Entry { instrument, .. }) => Some(instrument.clone()),
                _ => None,
            };
            if let Some(instrument) = picked {
                (this.on_pick)(instrument);
            }
        });
    }

    /// Plain arrows move a symbol at a time — the list does that itself.
    /// Ctrl with an arrow jumps a whole section, for a long rail.
    fn wire_keys(self: &Rc<Self>) {
        let keys = gtk::EventControllerKey::new();
        let this = self.clone();
        keys.connect_key_pressed(move |_, key, _, state| {
            let ctrl = state.contains(gtk::gdk::ModifierType::CONTROL_MASK);
            if !ctrl {
                return glib::Propagation::Proceed;
            }
            let forward = match key {
                gtk::gdk::Key::Down => true,
                gtk::gdk::Key::Up => false,
                _ => return glib::Propagation::Proceed,
            };
            this.jump_section(forward);
            glib::Propagation::Stop
        });
        self.list.add_controller(keys);

        // Delete takes the highlighted symbol off the rail.
        let keys = gtk::EventControllerKey::new();
        let this = self.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            if !matches!(key, gtk::gdk::Key::Delete | gtk::gdk::Key::KP_Delete) {
                return glib::Propagation::Proceed;
            }
            this.remove_selected();
            glib::Propagation::Stop
        });
        self.list.add_controller(keys);
    }

    /// Remove whatever is highlighted, and leave the highlight where it was so
    /// Delete can be pressed again.
    pub fn remove_selected(self: &Rc<Self>) {
        let at = self.list.selected_row().map(|r| r.index().max(0) as usize);
        let Some(at) = at else { return };
        let target = match self.rows.borrow().get(at) {
            Some(RowKind::Entry { section_id, entry, .. }) => Some((*section_id, entry.clone())),
            _ => None,
        };
        let Some((section_id, entry)) = target else { return };
        self.remove_entry(section_id, &entry);

        // Land on the row that took its place, or the one above if it was last.
        let next = self
            .list
            .row_at_index(at as i32)
            .or_else(|| self.list.row_at_index(at as i32 - 1));
        if let Some(row) = next {
            if row.is_selectable() {
                self.list.select_row(Some(&row));
            }
        }
    }

    fn remove_entry(self: &Rc<Self>, section_id: i64, entry: &Entry) {
        self.store.remove_from_section(section_id, &entry.symbol, entry.suffix.as_deref());
        self.rebuild();
    }

    /// Select the first symbol of the next or previous section.
    fn jump_section(self: &Rc<Self>, forward: bool) {
        let current = self.list.selected_row().map(|r| r.index()).unwrap_or(0).max(0) as usize;
        let rows = self.rows.borrow();

        // Which section are we in now?
        let current_section = rows[..=current.min(rows.len().saturating_sub(1))]
            .iter()
            .rev()
            .find_map(|kind| match kind {
                RowKind::Entry { section_id, .. } | RowKind::Header { section_id } => {
                    Some(*section_id)
                }
            });

        let candidates: Vec<usize> = rows
            .iter()
            .enumerate()
            .filter_map(|(at, kind)| match kind {
                RowKind::Entry { section_id, .. } if Some(*section_id) != current_section => {
                    Some(at)
                }
                _ => None,
            })
            .collect();
        drop(rows);

        let target = if forward {
            candidates.into_iter().find(|at| *at > current)
        } else {
            candidates.into_iter().filter(|at| *at < current).next_back()
        };
        if let Some(at) = target {
            if let Some(row) = self.list.row_at_index(at as i32) {
                self.list.select_row(Some(&row));
                row.grab_focus();
            }
        }
    }

    /// Rebuild the whole rail. A few dozen rows, so there is nothing to gain
    /// from patching it in place.
    pub fn rebuild(self: &Rc<Self>) {
        let selected = self.list.selected_row().map(|r| r.index());

        self.quiet.set(true);
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        while let Some(child) = self.header.first_child() {
            self.header.remove(&child);
        }

        for column in self.columns.borrow().iter() {
            let label = gtk::Label::new(Some(column.label()));
            label.add_css_class("dim-label");
            label.add_css_class("caption");
            if *column == Column::Symbol {
                label.set_xalign(0.0);
                label.set_hexpand(true);
            } else {
                label.set_xalign(1.0);
                label.set_width_chars(column.width_chars());
            }
            self.header.append(&label);
        }

        let mut kinds = Vec::new();
        for section in self.store.watchlist() {
            if section.id != ROOT_SECTION {
                self.list.append(&self.section_header(section.id, &section.name, section.collapsed));
                kinds.push(RowKind::Header { section_id: section.id });
            }
            for entry in &section.entries {
                let Some(instrument) = self.index.find(&entry.symbol, entry.suffix.as_deref()) else {
                    continue;
                };
                let (row, cells) = self.entry_row(section.id, entry, instrument);
                row.set_visible(!section.collapsed);
                self.list.append(&row);
                kinds.push(RowKind::Entry {
                    section_id: section.id,
                    entry: entry.clone(),
                    instrument: instrument.clone(),
                    cells,
                });
            }
        }
        *self.rows.borrow_mut() = kinds;

        if let Some(index) = selected {
            if let Some(row) = self.list.row_at_index(index) {
                self.list.select_row(Some(&row));
            }
        }
        self.quiet.set(false);
    }

    /// Section title, a disclosure arrow, and its controls. Double-clicking the
    /// title renames it in place.
    fn section_header(
        self: &Rc<Self>,
        id: i64,
        name: &str,
        collapsed: bool,
    ) -> gtk::ListBoxRow {
        let arrow = gtk::Button::from_icon_name(if collapsed {
            "pan-end-symbolic"
        } else {
            "pan-down-symbolic"
        });
        arrow.add_css_class("flat");
        arrow.set_valign(gtk::Align::Center);
        let this = self.clone();
        arrow.connect_clicked(move |_| {
            this.store.set_section_collapsed(id, !collapsed);
            this.rebuild();
        });

        let label = gtk::Label::new(Some(name));
        label.set_xalign(0.0);
        label.set_hexpand(true);
        label.add_css_class("dim-label");
        label.add_css_class("caption-heading");
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);

        let stack = gtk::Stack::new();
        stack.set_hexpand(true);
        stack.add_named(&label, Some("label"));
        let rename = gtk::Entry::new();
        rename.set_text(name);
        stack.add_named(&rename, Some("entry"));
        stack.set_visible_child_name("label");

        let click = gtk::GestureClick::new();
        let stack_weak = stack.downgrade();
        let rename_weak = rename.downgrade();
        click.connect_pressed(move |_, presses, _, _| {
            if presses < 2 {
                return;
            }
            if let (Some(stack), Some(rename)) = (stack_weak.upgrade(), rename_weak.upgrade()) {
                stack.set_visible_child_name("entry");
                rename.grab_focus();
                rename.select_region(0, -1);
            }
        });
        label.add_controller(click);

        let this = self.clone();
        rename.connect_activate(move |entry| {
            let text = entry.text().trim().to_string();
            if !text.is_empty() {
                this.store.rename_section(id, &text);
            }
            this.rebuild();
        });
        let focus = gtk::EventControllerFocus::new();
        let stack_weak = stack.downgrade();
        focus.connect_leave(move |_| {
            if let Some(stack) = stack_weak.upgrade() {
                stack.set_visible_child_name("label");
            }
        });
        rename.add_controller(focus);

        let header = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        header.set_margin_start(4);
        header.set_margin_end(10);
        header.set_margin_top(6);
        header.append(&arrow);
        header.append(&stack);

        let row = gtk::ListBoxRow::new();
        row.set_child(Some(&header));
        // Headers are scenery: the arrow keys walk past them.
        row.set_selectable(false);
        row.set_activatable(false);

        // Dropping on a header puts the symbol in that section, at the end.
        // Without this there is no way to move something into a collapsed or
        // empty section.
        let target = gtk::DropTarget::new(glib::Type::STRING, gtk::gdk::DragAction::MOVE);
        let this = self.clone();
        target.connect_drop(move |_, value, _, _| {
            let Some((from, moving)) = parse_drag(value) else { return false };
            this.store.move_entry_to_section(from, id, &moving, None);
            this.rebuild();
            true
        });
        row.add_controller(target);

        // Everything you can do to a section lives behind a right-click. A
        // delete button sitting on every header all the time is both noise and
        // an invitation to lose a section by accident.
        let menu = gtk::GestureClick::new();
        menu.set_button(gtk::gdk::BUTTON_SECONDARY);
        let this = self.clone();
        let section_name = name.to_string();
        let row_weak = row.downgrade();
        menu.connect_pressed(move |_, _, x, y| {
            let Some(row) = row_weak.upgrade() else { return };
            this.section_menu(&row, id, &section_name, x, y);
        });
        row.add_controller(menu);
        row
    }

    /// The section context menu.
    fn section_menu(
        self: &Rc<Self>,
        anchor: &gtk::ListBoxRow,
        id: i64,
        name: &str,
        x: f64,
        y: f64,
    ) {
        let items = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let popover = gtk::Popover::new();
        popover.set_child(Some(&items));
        popover.set_parent(anchor);
        popover.set_has_arrow(false);
        popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));

        let add = menu_item("Add symbol…");
        let this = self.clone();
        let popover_weak = popover.downgrade();
        add.connect_clicked(move |_| {
            if let Some(p) = popover_weak.upgrade() {
                p.popdown();
            }
            this.add_symbol_to(id);
        });
        items.append(&add);

        let remove = menu_item("Remove section…");
        remove.add_css_class("destructive-action");
        let this = self.clone();
        let popover_weak = popover.downgrade();
        let name = name.to_string();
        let anchor = anchor.clone();
        remove.connect_clicked(move |_| {
            if let Some(p) = popover_weak.upgrade() {
                p.popdown();
            }
            this.confirm_remove_section(&anchor, id, &name);
        });
        items.append(&remove);

        popover.popup();
    }

    fn entry_row(
        self: &Rc<Self>,
        section_id: i64,
        entry: &Entry,
        instrument: &Instrument,
    ) -> (gtk::ListBoxRow, Vec<(Column, gtk::Label)>) {
        let quote = (self.quote)(instrument);
        let row_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        row_box.set_margin_top(4);
        row_box.set_margin_bottom(4);
        row_box.set_margin_start(12);
        row_box.set_margin_end(10);

        let mut cells = Vec::new();
        for column in self.columns.borrow().iter() {
            let label = self.cell(*column, instrument, quote);
            row_box.append(&label);
            if *column != Column::Symbol {
                cells.push((*column, label));
            }
        }

        let row = gtk::ListBoxRow::new();
        row.set_child(Some(&row_box));
        row.set_activatable(true);
        row.set_tooltip_text(Some(&instrument.name));

        self.wire_row_removal(&row, section_id, entry);
        self.wire_row_reorder(&row, section_id, entry);
        (row, cells)
    }

    /// Write new numbers into the rows that are already there.
    ///
    /// Prefetching means quotes arrive constantly, and rebuilding the rail for
    /// each one tears down every widget — which loses the selection, steals
    /// focus mid-keypress, and flickers. Only the values change, so only the
    /// values are written.
    pub fn refresh_quotes(&self) {
        for kind in self.rows.borrow().iter() {
            let RowKind::Entry { instrument, cells, .. } = kind else { continue };
            let quote = (self.quote)(instrument);
            for (column, label) in cells {
                write_cell(label, *column, quote);
            }
        }
    }

    fn cell(&self, column: Column, instrument: &Instrument, quote: Option<Quote>) -> gtk::Label {
        let label = gtk::Label::new(None);
        if column == Column::Symbol {
            label.set_text(&instrument.display_symbol());
            label.set_xalign(0.0);
            label.set_hexpand(true);
            label.add_css_class("symbol-row-ticker");
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            return label;
        }
        label.add_css_class("numeric");
        label.add_css_class("caption");
        label.set_xalign(1.0);
        label.set_width_chars(column.width_chars());
        write_cell(&label, column, quote);
        label
    }

    /// Right-click offers removal, rather than removing on the click itself.
    fn wire_row_removal(self: &Rc<Self>, row: &gtk::ListBoxRow, section_id: i64, entry: &Entry) {
        let click = gtk::GestureClick::new();
        click.set_button(gtk::gdk::BUTTON_SECONDARY);

        let this = self.clone();
        let entry = entry.clone();
        let row_weak = row.downgrade();
        click.connect_pressed(move |_, _, x, y| {
            let Some(row) = row_weak.upgrade() else { return };
            let items = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let popover = gtk::Popover::new();
            popover.set_child(Some(&items));
            popover.set_parent(&row);
            popover.set_has_arrow(false);
            popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));

            let remove = menu_item("Remove");
            remove.add_css_class("destructive-action");
            let this = this.clone();
            let entry = entry.clone();
            let popover_weak = popover.downgrade();
            remove.connect_clicked(move |_| {
                if let Some(p) = popover_weak.upgrade() {
                    p.popdown();
                }
                this.remove_entry(section_id, &entry);
            });
            items.append(&remove);
            popover.popup();
        });
        row.add_controller(click);
    }

    /// Drag a row onto another to put it there, within its section.
    fn wire_row_reorder(self: &Rc<Self>, row: &gtk::ListBoxRow, section_id: i64, entry: &Entry) {
        let payload = format!(
            "{section_id}\t{}\t{}",
            entry.symbol,
            entry.suffix.clone().unwrap_or_default()
        );

        let source = gtk::DragSource::new();
        source.set_actions(gtk::gdk::DragAction::MOVE);
        let dragged = payload.clone();
        source.connect_prepare(move |_, _, _| {
            Some(gtk::gdk::ContentProvider::for_value(&dragged.to_value()))
        });
        row.add_controller(source);

        let target = gtk::DropTarget::new(glib::Type::STRING, gtk::gdk::DragAction::MOVE);
        let this = self.clone();
        let onto = entry.clone();
        target.connect_drop(move |_, value, _, _| {
            let Some((from, moving)) = parse_drag(value) else { return false };
            if from == section_id && moving == onto {
                return false;
            }
            this.store.move_entry_to_section(from, section_id, &moving, Some(&onto));
            this.rebuild();
            true
        });
        row.add_controller(target);
    }

    /// Adding uses the same picker as everywhere else.
    pub fn add_symbol_to(self: &Rc<Self>, section_id: i64) {
        let title = if section_id == ROOT_SECTION {
            "Add to watchlist".to_string()
        } else {
            let name = self
                .store
                .watchlist()
                .into_iter()
                .find(|s| s.id == section_id)
                .map(|s| s.name)
                .unwrap_or_default();
            format!("Add to {name}")
        };
        let this = self.clone();
        self.search.present(&self.widget, &title, move |instrument| {
            this.store.add_to_section(section_id, &instrument.symbol, instrument.suffix.as_deref());
            this.rebuild();
        });
    }

    fn prompt_new_section(self: &Rc<Self>, anchor: &gtk::Button) {
        let entry = gtk::Entry::new();
        entry.set_placeholder_text(Some("Section name"));

        let popover = gtk::Popover::new();
        popover.set_child(Some(&entry));
        popover.set_parent(anchor);

        let this = self.clone();
        let popover_weak = popover.downgrade();
        entry.connect_activate(move |entry| {
            let name = entry.text().trim().to_string();
            if !name.is_empty() {
                this.store.add_section(&name);
                this.rebuild();
            }
            if let Some(popover) = popover_weak.upgrade() {
                popover.popdown();
            }
        });

        popover.popup();
        entry.grab_focus();
    }

    /// Removing a section takes its symbols with it, so it asks first.
    fn confirm_remove_section(
        self: &Rc<Self>,
        anchor: &impl IsA<gtk::Widget>,
        id: i64,
        name: &str,
    ) {
        let count = self
            .store
            .watchlist()
            .into_iter()
            .find(|s| s.id == id)
            .map(|s| s.entries.len())
            .unwrap_or(0);

        let body = match count {
            0 => format!("Remove “{name}”?"),
            1 => format!("Remove “{name}” and the symbol in it?"),
            n => format!("Remove “{name}” and the {n} symbols in it?"),
        };

        let dialog = adw::AlertDialog::new(Some("Remove section"), Some(&body));
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("remove", "Remove");
        dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");

        let this = self.clone();
        dialog.connect_response(None, move |_, response| {
            if response == "remove" {
                this.store.remove_section(id);
                this.rebuild();
            }
        });
        dialog.present(Some(anchor));
    }
}

/// Unpack a dragged row: which section it came from, and which entry it is.
fn parse_drag(value: &glib::Value) -> Option<(i64, Entry)> {
    let text = value.get::<String>().ok()?;
    let parts: Vec<&str> = text.split('\t').collect();
    if parts.len() != 3 {
        return None;
    }
    Some((
        parts[0].parse().ok()?,
        Entry {
            symbol: parts[1].to_string(),
            suffix: (!parts[2].is_empty()).then(|| parts[2].to_string()),
        },
    ))
}

/// Put a quote into one value label, direction colouring included.
fn write_cell(label: &gtk::Label, column: Column, quote: Option<Quote>) {
    for class in ["change-up", "change-down", "change-flat", "dim-label"] {
        label.remove_css_class(class);
    }
    let Some(q) = quote else {
        label.set_text(if column == Column::ChangePct { "" } else { "–" });
        label.add_css_class("dim-label");
        return;
    };
    match column {
        Column::Last => label.set_text(&format!("{:.*}", decimals_for(q.last), q.last)),
        Column::Change => label.set_text(&format!("{:+.*}", decimals_for(q.last), q.change)),
        Column::ChangePct => label.set_text(&format!("{:+.2}%", q.change_pct)),
        Column::Symbol => {}
    }
    if column != Column::Last {
        // Direction is defined once, in the engine; this just wears it.
        label.add_css_class(omacharts_engine::Direction::of_change(q.change).css_class());
    }
}

/// How many decimals a price of this size deserves.
///
/// Two is right for a stock and absurd for a currency pair: EURUSD moving
/// 0.0008 rounds to "+0.00", which reads as "nothing happened" when the
/// percent column says otherwise.
fn decimals_for(price: f64) -> usize {
    match price.abs() {
        p if p >= 20.0 => 2,
        p if p >= 1.0 => 4,
        _ => 6,
    }
}

/// A flat row in one of the little context menus.
fn menu_item(label: &str) -> gtk::Button {
    let button = gtk::Button::with_label(label);
    button.add_css_class("flat");
    button.set_halign(gtk::Align::Fill);
    if let Some(child) = button.child().and_downcast::<gtk::Label>() {
        child.set_xalign(0.0);
    }
    button
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_symbol_column_is_always_first_and_never_hidden() {
        assert_eq!(parse_columns(Some("change,change_pct"))[0], Column::Symbol);
        assert_eq!(parse_columns(Some("last"))[0], Column::Symbol);
        // Even if someone stores a list without it.
        let columns = parse_columns(Some("change_pct,change"));
        assert_eq!(columns, vec![Column::Symbol, Column::ChangePct, Column::Change]);
    }

    #[test]
    fn column_order_is_respected() {
        let columns = parse_columns(Some("symbol,change_pct,last,change"));
        assert_eq!(
            columns,
            vec![Column::Symbol, Column::ChangePct, Column::Last, Column::Change]
        );
    }

    #[test]
    fn an_empty_or_unreadable_setting_falls_back_to_the_default() {
        assert_eq!(parse_columns(None), Column::DEFAULT.to_vec());
        assert_eq!(parse_columns(Some("")), Column::DEFAULT.to_vec());
        assert_eq!(parse_columns(Some("nonsense,rubbish")), Column::DEFAULT.to_vec());
    }

    #[test]
    fn columns_round_trip_through_settings() {
        let columns = vec![Column::Symbol, Column::Last, Column::ChangePct];
        let stored = columns_to_string(&columns);
        assert_eq!(parse_columns(Some(&stored)), columns);
    }

    #[test]
    fn prices_get_the_decimals_they_deserve() {
        assert_eq!(decimals_for(7722.72), 2, "an index");
        assert_eq!(decimals_for(233.95), 2, "a stock");
        assert_eq!(decimals_for(84908.12), 2, "bitcoin");
        assert_eq!(decimals_for(1.1734), 4, "a currency pair");
        assert_eq!(decimals_for(0.00042), 6, "a small-cap crypto");
    }

    #[test]
    fn a_currency_move_does_not_round_away_to_nothing() {
        let change = 0.0008_f64;
        let formatted = format!("{:+.*}", decimals_for(1.1734), change);
        assert_ne!(formatted, "+0.00");
        assert_eq!(formatted, "+0.0008");
    }

    #[test]
    fn the_default_is_symbol_change_and_percent() {
        assert_eq!(
            Column::DEFAULT.to_vec(),
            vec![Column::Symbol, Column::Change, Column::ChangePct]
        );
    }
}
