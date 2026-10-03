//! The watchlist rail.
//!
//! A column on the right that can be hidden outright. Symbols can sit at the
//! root or inside sections you name; sections are optional. Click to chart,
//! drag to reorder, double-click a section title to rename it.
//!
//! Prices shown here come from the cache only. The rail never issues a
//! request — a sidebar that quietly fetches twenty quotes on every repaint is
//! precisely how you get rate limited.

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

pub struct Watchlist {
    pub widget: gtk::Box,
    body: gtk::Box,
    store: Rc<Store>,
    index: Rc<SearchIndex>,
    quote: QuoteLookup,
    search: Rc<SymbolSearch>,
    on_pick: Rc<dyn Fn(Instrument)>,
}

impl Watchlist {
    pub fn new(
        store: Rc<Store>,
        index: Rc<SearchIndex>,
        search: Rc<SymbolSearch>,
        quote: QuoteLookup,
        on_pick: impl Fn(Instrument) + 'static,
    ) -> Rc<Watchlist> {
        let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        body.set_margin_bottom(8);

        let scroller = gtk::ScrolledWindow::new();
        scroller.set_child(Some(&body));
        scroller.set_vexpand(true);
        scroller.set_hscrollbar_policy(gtk::PolicyType::Never);

        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        widget.set_size_request(236, -1);
        widget.append(&scroller);

        let watchlist = Rc::new(Watchlist {
            widget,
            body,
            store,
            index,
            quote,
            search,
            on_pick: Rc::new(on_pick),
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

        watchlist.rebuild();
        watchlist
    }

    /// Rebuild the whole rail. A few dozen rows, so there is nothing to gain
    /// from patching it in place.
    pub fn rebuild(self: &Rc<Self>) {
        while let Some(child) = self.body.first_child() {
            self.body.remove(&child);
        }
        for section in self.store.watchlist() {
            // The root has no name and no header.
            if section.id != ROOT_SECTION {
                self.body.append(&self.section_header(section.id, &section.name));
            } else {
                let spacer = gtk::Box::new(gtk::Orientation::Vertical, 0);
                spacer.set_margin_top(4);
                self.body.append(&spacer);
            }

            let list = gtk::ListBox::new();
            list.set_selection_mode(gtk::SelectionMode::None);
            list.add_css_class("navigation-sidebar");
            for entry in &section.entries {
                if let Some(row) = self.entry_row(section.id, entry) {
                    list.append(&row);
                }
            }
            self.body.append(&list);
        }
    }

    /// Section title, a button to add to it, and its menu. Double-clicking the
    /// title renames it in place.
    fn section_header(self: &Rc<Self>, id: i64, name: &str) -> gtk::Box {
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
        rename.add_css_class("caption-heading");
        stack.add_named(&rename, Some("entry"));
        stack.set_visible_child_name("label");

        // Double-click to rename.
        let click = gtk::GestureClick::new();
        click.set_button(gtk::gdk::BUTTON_PRIMARY);
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
        let stack_weak = stack.downgrade();
        rename.connect_activate(move |entry| {
            let text = entry.text().trim().to_string();
            if !text.is_empty() {
                this.store.rename_section(id, &text);
            }
            if let Some(stack) = stack_weak.upgrade() {
                stack.set_visible_child_name("label");
            }
            this.rebuild();
        });

        // Leaving the entry without confirming abandons the edit.
        let focus = gtk::EventControllerFocus::new();
        let stack_weak = stack.downgrade();
        focus.connect_leave(move |_| {
            if let Some(stack) = stack_weak.upgrade() {
                stack.set_visible_child_name("label");
            }
        });
        rename.add_controller(focus);

        let add = gtk::Button::from_icon_name("list-add-symbolic");
        add.add_css_class("flat");
        add.set_tooltip_text(Some("Add a symbol to this section"));
        add.set_valign(gtk::Align::Center);
        let this = self.clone();
        add.connect_clicked(move |_| this.add_symbol_to(id));

        let remove = gtk::Button::from_icon_name("user-trash-symbolic");
        remove.add_css_class("flat");
        remove.set_tooltip_text(Some("Remove this section"));
        remove.set_valign(gtk::Align::Center);
        let this = self.clone();
        let section_name = name.to_string();
        remove.connect_clicked(move |button| this.confirm_remove_section(button, id, &section_name));

        let header = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        header.set_margin_start(12);
        header.set_margin_end(6);
        header.set_margin_top(10);
        header.append(&stack);
        header.append(&add);
        header.append(&remove);
        header
    }

    /// Ticker, change, and change percent. Nothing else by default.
    fn entry_row(self: &Rc<Self>, section_id: i64, entry: &Entry) -> Option<gtk::ListBoxRow> {
        let instrument = self.index.find(&entry.symbol, entry.suffix.as_deref())?.clone();

        let ticker = gtk::Label::new(Some(&instrument.display_symbol()));
        ticker.set_xalign(0.0);
        ticker.set_hexpand(true);
        ticker.add_css_class("symbol-row-ticker");
        ticker.set_ellipsize(gtk::pango::EllipsizeMode::End);

        let change = gtk::Label::new(None);
        change.add_css_class("numeric");
        change.add_css_class("caption");
        change.set_xalign(1.0);
        change.set_width_chars(8);

        let percent = gtk::Label::new(None);
        percent.add_css_class("numeric");
        percent.add_css_class("caption");
        percent.set_xalign(1.0);
        percent.set_width_chars(7);

        match (self.quote)(&instrument) {
            Some(quote) => {
                let class = if quote.change > 0.0 {
                    "change-up"
                } else if quote.change < 0.0 {
                    "change-down"
                } else {
                    "change-flat"
                };
                change.set_text(&format!("{:+.2}", quote.change));
                percent.set_text(&format!("{:+.2}%", quote.change_pct));
                change.add_css_class(class);
                percent.add_css_class(class);
                change.set_tooltip_text(Some(&format!("Last {:.2}", quote.last)));
            }
            // Never charted, so never fetched. Blank beats a fake zero.
            None => {
                change.set_text("–");
                percent.set_text("");
                change.add_css_class("dim-label");
            }
        }

        let row_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        row_box.set_margin_top(4);
        row_box.set_margin_bottom(4);
        row_box.set_margin_start(12);
        row_box.set_margin_end(10);
        row_box.append(&ticker);
        row_box.append(&change);
        row_box.append(&percent);

        let row = gtk::ListBoxRow::new();
        row.set_child(Some(&row_box));
        row.set_activatable(true);
        row.set_tooltip_text(Some(&instrument.name));

        let on_pick = self.on_pick.clone();
        let picked = instrument.clone();
        row.connect_activate(move |_| on_pick(picked.clone()));

        self.wire_row_removal(&row, section_id, entry);
        self.wire_row_reorder(&row, section_id, entry);
        Some(row)
    }

    /// Right-click offers removal, rather than removing on the click itself.
    fn wire_row_removal(self: &Rc<Self>, row: &gtk::ListBoxRow, section_id: i64, entry: &Entry) {
        let click = gtk::GestureClick::new();
        click.set_button(gtk::gdk::BUTTON_SECONDARY);

        let this = self.clone();
        let entry = entry.clone();
        let row_weak = row.downgrade();
        click.connect_pressed(move |_, _, _, _| {
            let Some(row) = row_weak.upgrade() else { return };
            let button = gtk::Button::with_label("Remove");
            button.add_css_class("destructive-action");
            button.add_css_class("flat");

            let popover = gtk::Popover::new();
            popover.set_child(Some(&button));
            popover.set_parent(&row);

            let this = this.clone();
            let entry = entry.clone();
            let popover_weak = popover.downgrade();
            button.connect_clicked(move |_| {
                this.store.remove_from_section(section_id, &entry.symbol, entry.suffix.as_deref());
                if let Some(popover) = popover_weak.upgrade() {
                    popover.popdown();
                }
                this.rebuild();
            });
            popover.popup();
        });
        row.add_controller(click);
    }

    /// Drag a row onto another to put it there. Reordering is within a section
    /// — moving between sections is a remove and an add, which the menus
    /// already do more legibly.
    fn wire_row_reorder(self: &Rc<Self>, row: &gtk::ListBoxRow, section_id: i64, entry: &Entry) {
        let payload = format!("{section_id}\t{}\t{}", entry.symbol, entry.suffix.clone().unwrap_or_default());

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
            let Ok(text) = value.get::<String>() else {
                return false;
            };
            let parts: Vec<&str> = text.split('\t').collect();
            if parts.len() != 3 {
                return false;
            }
            // Only within the same section.
            if parts[0].parse::<i64>().ok() != Some(section_id) {
                return false;
            }
            let moving = Entry {
                symbol: parts[1].to_string(),
                suffix: (!parts[2].is_empty()).then(|| parts[2].to_string()),
            };
            if moving == onto {
                return false;
            }
            this.store.move_entry(section_id, &moving, &onto);
            this.rebuild();
            true
        });
        row.add_controller(target);
    }

    /// Adding uses the same picker as everywhere else.
    fn add_symbol_to(self: &Rc<Self>, section_id: i64) {
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
            this.store.add_to_section(
                section_id,
                &instrument.symbol,
                instrument.suffix.as_deref(),
            );
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
        anchor: &gtk::Button,
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
