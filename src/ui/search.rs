//! Find a symbol by typing.
//!
//! The whole inventory is already in memory, so every keystroke re-runs the
//! search and rebuilds the list. No debounce, no spinner, no network — which
//! is the point: it should feel like the Omarchy launcher, not like a web
//! autocomplete.
//!
//! One instance serves every place a symbol is picked — charting it, adding it
//! to a watchlist section — by swapping the handler at presentation time.
//! Identical behaviour everywhere, and the index is only built once.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use omacharts_engine::{Instrument, SearchIndex};

/// Rows beyond this are noise — nobody scrolls a symbol picker.
const LIMIT: usize = 24;

type Handler = Rc<RefCell<Option<Box<dyn Fn(Instrument)>>>>;

pub struct SymbolSearch {
    dialog: adw::Dialog,
    list: gtk::ListBox,
    entry: gtk::SearchEntry,
    index: Rc<SearchIndex>,
    /// Instruments on display, parallel to the list rows.
    shown: Rc<RefCell<Vec<Instrument>>>,
    handler: Handler,
}

impl SymbolSearch {
    pub fn new(index: Rc<SearchIndex>) -> Rc<SymbolSearch> {
        let entry = gtk::SearchEntry::new();
        entry.set_placeholder_text(Some("Symbol or name"));
        entry.set_hexpand(true);

        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::Browse);
        list.add_css_class("navigation-sidebar");

        let scroller = gtk::ScrolledWindow::new();
        scroller.set_child(Some(&list));
        scroller.set_vexpand(true);
        scroller.set_hscrollbar_policy(gtk::PolicyType::Never);

        let header = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        header.set_margin_top(10);
        header.set_margin_bottom(10);
        header.set_margin_start(12);
        header.set_margin_end(12);
        header.append(&entry);

        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&header);
        content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        content.append(&scroller);

        let dialog = adw::Dialog::new();
        dialog.set_content_width(520);
        dialog.set_content_height(430);
        dialog.set_child(Some(&content));

        let search = Rc::new(SymbolSearch {
            dialog,
            list,
            entry,
            index,
            shown: Rc::new(RefCell::new(Vec::new())),
            handler: Rc::new(RefCell::new(None)),
        });
        search.wire();
        search.populate("");
        search
    }

    fn wire(self: &Rc<Self>) {
        // Typing. Every keystroke re-runs the search against memory, which is
        // cheap enough that debouncing would only add latency.
        let (list, index, shown) = (self.list.clone(), self.index.clone(), self.shown.clone());
        self.entry.connect_search_changed(move |entry| {
            repopulate(&list, &index, &shown, &entry.text());
        });

        // Enter takes the highlighted row, so an exact ticker never needs the
        // arrow keys or the mouse.
        let list = self.list.clone();
        self.entry.connect_activate(move |_| {
            if let Some(row) = list.selected_row() {
                row.activate();
            }
        });

        // A GtkSearchEntry handles Escape itself: it emits stop-search and
        // clears the box, swallowing the key before anything above it sees it.
        // So closing has to hang off that, not off the key.
        let dialog = self.dialog.clone();
        self.entry.connect_stop_search(move |_| {
            dialog.close();
        });

        // And again on the dialog, for an Escape pressed while focus is in the
        // list rather than the entry.
        let escape = gtk::EventControllerKey::new();
        let dialog = self.dialog.clone();
        escape.connect_key_pressed(move |_, key, _, _| {
            if key == gtk::gdk::Key::Escape {
                dialog.close();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        self.dialog.add_controller(escape);

        // Up and down move the selection while focus stays in the entry.
        let keys = gtk::EventControllerKey::new();
        let list = self.list.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            let rows = row_count(&list);
            if rows == 0 {
                return glib::Propagation::Proceed;
            }
            let current = list.selected_row().map(|r| r.index()).unwrap_or(0);
            let next = match key {
                gtk::gdk::Key::Down => (current + 1).min(rows - 1),
                gtk::gdk::Key::Up => (current - 1).max(0),
                _ => return glib::Propagation::Proceed,
            };
            if let Some(row) = list.row_at_index(next) {
                list.select_row(Some(&row));
            }
            glib::Propagation::Stop
        });
        self.entry.add_controller(keys);

        let shown = self.shown.clone();
        let dialog = self.dialog.clone();
        let handler = self.handler.clone();
        self.list.connect_row_activated(move |_, row| {
            let index = row.index().max(0) as usize;
            let picked = shown.borrow().get(index).cloned();
            if let Some(instrument) = picked {
                dialog.close();
                if let Some(handler) = handler.borrow().as_ref() {
                    handler(instrument);
                }
            }
        });
    }

    fn populate(&self, query: &str) {
        repopulate(&self.list, &self.index, &self.shown, query);
    }

    /// Open the picker. `title` says what picking will do.
    pub fn present(
        &self,
        parent: &impl IsA<gtk::Widget>,
        title: &str,
        on_pick: impl Fn(Instrument) + 'static,
    ) {
        self.present_with(parent, title, "", on_pick);
    }

    /// Open it already carrying a query.
    ///
    /// Typing a letter on the chart opens this with that letter in the box, so
    /// the keystroke that summoned the picker is not lost — which is what
    /// makes "just start typing" feel like one gesture instead of two.
    pub fn present_with(
        &self,
        parent: &impl IsA<gtk::Widget>,
        title: &str,
        query: &str,
        on_pick: impl Fn(Instrument) + 'static,
    ) {
        *self.handler.borrow_mut() = Some(Box::new(on_pick));
        self.dialog.set_title(title);
        self.entry.set_text(query);
        self.entry.set_position(-1);
        self.populate(query);
        self.dialog.present(Some(parent));
        self.entry.grab_focus();
        self.entry.set_position(-1);
    }
}

/// Re-run the search and rebuild the rows, keeping `shown` parallel to them.
fn repopulate(
    list: &gtk::ListBox,
    index: &SearchIndex,
    shown: &RefCell<Vec<Instrument>>,
    query: &str,
) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }

    let hits = index.search(query, LIMIT);
    let mut rows = Vec::with_capacity(hits.len());
    for hit in &hits {
        let Some(instrument) = index.get(hit.index) else { continue };
        list.append(&row_for(instrument));
        rows.push(instrument.clone());
    }
    *shown.borrow_mut() = rows;

    // Always leave the best match highlighted, so Enter is enough.
    if let Some(first) = list.row_at_index(0) {
        list.select_row(Some(&first));
    }
}

/// Ticker, name, and what kind of thing it is. Nothing else — a picker is for
/// picking.
fn row_for(instrument: &Instrument) -> gtk::ListBoxRow {
    let ticker = gtk::Label::new(Some(&instrument.display_symbol()));
    ticker.add_css_class("symbol-row-ticker");
    ticker.set_xalign(0.0);
    ticker.set_width_chars(9);

    let name = gtk::Label::new(Some(&instrument.name));
    name.add_css_class("symbol-row-name");
    name.set_xalign(0.0);
    name.set_hexpand(true);
    name.set_ellipsize(gtk::pango::EllipsizeMode::End);

    let kind = gtk::Label::new(Some(instrument.kind.label()));
    kind.add_css_class("symbol-kind");
    kind.set_valign(gtk::Align::Center);

    let row_box = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    row_box.set_margin_top(7);
    row_box.set_margin_bottom(7);
    row_box.set_margin_start(10);
    row_box.set_margin_end(10);
    row_box.append(&ticker);
    row_box.append(&name);
    row_box.append(&kind);

    let row = gtk::ListBoxRow::new();
    row.set_child(Some(&row_box));
    row
}

fn row_count(list: &gtk::ListBox) -> i32 {
    let mut n = 0;
    let mut child = list.first_child();
    while let Some(widget) = child {
        n += 1;
        child = widget.next_sibling();
    }
    n
}
