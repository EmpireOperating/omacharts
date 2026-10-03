//! Settings that belong to the chart rather than the app.
//!
//! Reached from the gear beside the legend, because that is where you are
//! looking when you want them.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use omacharts_engine::Session;

use crate::store::Store;

pub const SETTING_SESSION: &str = "chart_session";

pub struct ChartSettings;

impl ChartSettings {
    pub fn present(
        parent: &impl IsA<gtk::Widget>,
        store: Rc<Store>,
        session: Rc<RefCell<Session>>,
        on_change: Rc<dyn Fn()>,
    ) {
        let dialog = adw::PreferencesDialog::new();
        dialog.set_title("Chart");
        dialog.set_content_width(480);

        let page = adw::PreferencesPage::new();
        let group = adw::PreferencesGroup::new();
        group.set_title("Session");
        group.set_description(Some(
            "Overnight trade is thin, and a few prints at 3am stretch the price \
             scale enough to squash the session everyone actually traded.",
        ));

        let names: Vec<&str> = Session::ALL.iter().map(|s| s.label()).collect();
        let list = gtk::StringList::new(&names);

        let row = adw::ComboRow::new();
        row.set_title("Hours");
        row.set_subtitle("Ignored for FX, crypto and foreign listings, which have no cash session.");
        row.set_model(Some(&list));
        row.set_selected(
            Session::ALL.iter().position(|s| *s == *session.borrow()).unwrap_or(0) as u32,
        );
        row.connect_selected_notify(move |row| {
            let Some(chosen) = Session::ALL.get(row.selected() as usize).copied() else {
                return;
            };
            *session.borrow_mut() = chosen;
            store.set_setting(SETTING_SESSION, chosen.key());
            on_change();
        });

        group.add(&row);
        page.add(&group);
        dialog.add(&page);
        dialog.present(Some(parent));
    }
}
