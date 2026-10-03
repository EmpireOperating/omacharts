//! Settings that belong to the chart rather than the app.
//!
//! Reached from the gear beside the legend, because that is where you are
//! looking when you want them. Two pages: how the bars are read, and what is
//! drawn on top of them.

use std::rc::Rc;

use adw::prelude::*;
use omacharts_engine::indicators::{self, Kind, Params, Reset};
use omacharts_engine::theme::ColorChoice;
use omacharts_engine::{Indicator, Session};

use crate::store::Store;
use crate::ui::colors;
use crate::ui::window::Window;

pub const SETTING_SESSION: &str = "chart_session";

pub struct ChartSettings;

impl ChartSettings {
    pub fn present(window: &Rc<Window>, store: Rc<Store>) {
        let dialog = adw::PreferencesDialog::new();
        dialog.set_title("Chart");
        dialog.set_content_width(560);
        dialog.set_content_height(620);

        let chart_page = adw::PreferencesPage::new();
        chart_page.set_title("Chart");
        chart_page.set_icon_name(Some("preferences-system-symbolic"));
        chart_page.add(&session_group(window, &store));
        dialog.add(&chart_page);

        let indicators_page = adw::PreferencesPage::new();
        indicators_page.set_title("Indicators");
        indicators_page.set_icon_name(Some("view-list-symbolic"));
        dialog.add(&indicators_page);
        rebuild_indicators(window, &indicators_page);

        dialog.present(Some(&window.window));
    }

    /// Open straight onto the indicators, for the button on the chart.
    pub fn present_indicators(window: &Rc<Window>, store: Rc<Store>) {
        ChartSettings::present(window, store);
    }
}

fn session_group(window: &Rc<Window>, store: &Rc<Store>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::new();
    group.set_title("Session");
    group.set_description(Some(
        "Overnight trade is thin, and a few prints at 3am stretch the price scale \
         enough to squash the session everyone actually traded.",
    ));

    let names: Vec<&str> = Session::ALL.iter().map(|s| s.label()).collect();
    let session = window.session();

    let row = adw::ComboRow::new();
    row.set_title("Hours");
    row.set_subtitle("Ignored for FX, crypto and foreign listings, which have no cash session.");
    row.set_model(Some(&gtk::StringList::new(&names)));
    row.set_selected(Session::ALL.iter().position(|s| *s == *session.borrow()).unwrap_or(0) as u32);

    let window = window.clone();
    let store = store.clone();
    row.connect_selected_notify(move |row| {
        let Some(chosen) = Session::ALL.get(row.selected() as usize).copied() else { return };
        *window.session().borrow_mut() = chosen;
        store.set_setting(SETTING_SESSION, chosen.key());
        window.refresh();
    });

    group.add(&row);
    group
}

/// Clear and re-add the indicator groups.
///
/// One group per indicator, because each has its own parameters and its own
/// colour, and a flat list of every field from every indicator would be
/// unreadable.
fn rebuild_indicators(window: &Rc<Window>, page: &adw::PreferencesPage) {
    let mut child = page.first_child();
    while let Some(widget) = child {
        let next = widget.next_sibling();
        if let Some(group) = widget.downcast_ref::<adw::PreferencesGroup>() {
            page.remove(group);
        }
        child = next;
    }

    let indicators = window.indicators();

    let add_group = adw::PreferencesGroup::new();
    add_group.set_title("Indicators");
    add_group.set_description(Some(if indicators.is_empty() {
        "Nothing on the chart yet."
    } else {
        "Drawn in the order they were added, each taking the next colour from the theme."
    }));

    let add = gtk::Button::with_label("Add indicator…");
    add.add_css_class("suggested-action");
    add.set_valign(gtk::Align::Center);
    let window_for_add = window.clone();
    let page_for_add = page.clone();
    add.connect_clicked(move |button| {
        pick_indicator(button, &window_for_add, &page_for_add);
    });
    let add_row = adw::ActionRow::new();
    add_row.set_title("Add");
    add_row.add_suffix(&add);
    add_group.add(&add_row);
    page.add(&add_group);

    for (slot, indicator) in indicators.iter().enumerate() {
        page.add(&indicator_group(window, page, slot, indicator));
    }
}

fn indicator_group(
    window: &Rc<Window>,
    page: &adw::PreferencesPage,
    slot: usize,
    indicator: &Indicator,
) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::new();
    group.set_title(&indicator.label());
    group.set_description(Some(indicator.kind.name()));

    // Shown / removed.
    let header = adw::ActionRow::new();
    header.set_title("Shown");
    let visible = gtk::Switch::new();
    visible.set_active(indicator.visible);
    visible.set_valign(gtk::Align::Center);
    let id = indicator.id;
    let window_for_visible = window.clone();
    visible.connect_state_set(move |_, state| {
        update(&window_for_visible, id, |i| i.visible = state);
        glib::Propagation::Proceed
    });
    header.add_suffix(&visible);

    let remove = gtk::Button::from_icon_name("user-trash-symbolic");
    remove.add_css_class("flat");
    remove.set_valign(gtk::Align::Center);
    remove.set_tooltip_text(Some("Remove"));
    let window_for_remove = window.clone();
    let page_for_remove = page.clone();
    remove.connect_clicked(move |_| {
        let kept: Vec<Indicator> =
            window_for_remove.indicators().into_iter().filter(|i| i.id != id).collect();
        window_for_remove.set_indicators(kept);
        rebuild_indicators(&window_for_remove, &page_for_remove);
    });
    header.add_suffix(&remove);
    group.add(&header);

    // Colour: the palette by default, a fixed colour once chosen.
    let colour_row = adw::ActionRow::new();
    colour_row.set_title("Colour");
    colour_row.set_subtitle(if indicator.color.is_none() {
        "From the theme palette"
    } else {
        "Chosen"
    });
    let theme = window.theme();
    let button = gtk::ColorDialogButton::new(Some(gtk::ColorDialog::new()));
    button.set_rgba(&colors::parse(&indicator.color(&theme, slot)));
    button.set_valign(gtk::Align::Center);
    button.add_css_class("swatch-button");
    let window_for_colour = window.clone();
    button.connect_rgba_notify(move |button| {
        let hex = colors::to_hex(&button.rgba());
        update(&window_for_colour, id, |i| {
            i.color = Some(ColorChoice::Fixed { hex: hex.clone() })
        });
    });
    colour_row.add_suffix(&button);

    let reset = gtk::Button::with_label("Use palette");
    reset.add_css_class("flat");
    reset.set_valign(gtk::Align::Center);
    let window_for_reset = window.clone();
    let page_for_reset = page.clone();
    reset.connect_clicked(move |_| {
        update(&window_for_reset, id, |i| i.color = None);
        rebuild_indicators(&window_for_reset, &page_for_reset);
    });
    colour_row.add_suffix(&reset);
    group.add(&colour_row);

    // Parameters.
    match &indicator.params {
        Params::MovingAverage { period } => {
            group.add(&spin_row(window, page, id, "Period", *period as f64, 1.0, 500.0, 1.0, {
                move |indicator, value| {
                    indicator.params = Params::MovingAverage { period: value as usize };
                }
            }));
        }
        Params::Vwap { reset, deviations } => {
            group.add(&reset_row(window, page, id, *reset));
            for (band, multiple) in deviations.iter().enumerate() {
                group.add(&spin_row(
                    window,
                    page,
                    id,
                    &format!("Band {}", band + 1),
                    *multiple,
                    0.1,
                    6.0,
                    0.1,
                    move |indicator, value| {
                        if let Params::Vwap { deviations, .. } = &mut indicator.params {
                            while deviations.len() <= band {
                                deviations.push(value);
                            }
                            deviations[band] = value;
                        }
                    },
                ));
            }
        }
        Params::VolumeProfile { reset, rows, value_area } => {
            group.add(&reset_row(window, page, id, *reset));
            group.add(&spin_row(window, page, id, "Rows", *rows as f64, 4.0, 400.0, 1.0, {
                move |indicator, value| {
                    if let Params::VolumeProfile { rows, .. } = &mut indicator.params {
                        *rows = value as usize;
                    }
                }
            }));
            group.add(&spin_row(
                window,
                page,
                id,
                "Value area",
                *value_area * 100.0,
                10.0,
                100.0,
                1.0,
                move |indicator, value| {
                    if let Params::VolumeProfile { value_area, .. } = &mut indicator.params {
                        *value_area = value / 100.0;
                    }
                },
            ));
        }
    }
    group
}

fn reset_row(
    window: &Rc<Window>,
    page: &adw::PreferencesPage,
    id: u32,
    current: Reset,
) -> adw::ComboRow {
    let names: Vec<&str> = Reset::ALL.iter().map(|r| r.label()).collect();
    let row = adw::ComboRow::new();
    row.set_title("Reset every");
    row.set_subtitle("Promoted automatically when the chart is too coarse for it.");
    row.set_model(Some(&gtk::StringList::new(&names)));
    row.set_selected(Reset::ALL.iter().position(|r| *r == current).unwrap_or(0) as u32);

    let window = window.clone();
    let page = page.clone();
    row.connect_selected_notify(move |row| {
        let Some(chosen) = Reset::ALL.get(row.selected() as usize).copied() else { return };
        update(&window, id, |indicator| match &mut indicator.params {
            Params::Vwap { reset, .. } => *reset = chosen,
            Params::VolumeProfile { reset, .. } => *reset = chosen,
            Params::MovingAverage { .. } => {}
        });
        rebuild_indicators(&window, &page);
    });
    row
}

#[allow(clippy::too_many_arguments)]
fn spin_row(
    window: &Rc<Window>,
    page: &adw::PreferencesPage,
    id: u32,
    title: &str,
    value: f64,
    min: f64,
    max: f64,
    step: f64,
    apply: impl Fn(&mut Indicator, f64) + 'static,
) -> adw::SpinRow {
    let row = adw::SpinRow::with_range(min, max, step);
    row.set_title(title);
    row.set_value(value);
    if step < 1.0 {
        row.set_digits(1);
    }

    let window = window.clone();
    let page = page.clone();
    row.connect_value_notify(move |row| {
        let value = row.value();
        update(&window, id, |indicator| apply(indicator, value));
        // The group's title carries the period, so it has to follow.
        rebuild_indicators(&window, &page);
    });
    row
}

/// Change one indicator in place and redraw.
fn update(window: &Rc<Window>, id: u32, change: impl Fn(&mut Indicator)) {
    let mut indicators = window.indicators();
    let Some(indicator) = indicators.iter_mut().find(|i| i.id == id) else { return };
    change(indicator);
    window.set_indicators(indicators);
}

/// The indicator picker: the same type-and-it-narrows as the symbol search.
fn pick_indicator(anchor: &gtk::Button, window: &Rc<Window>, page: &adw::PreferencesPage) {
    let entry = gtk::SearchEntry::new();
    entry.set_placeholder_text(Some("Indicator"));

    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::Browse);
    list.add_css_class("navigation-sidebar");

    let scroller = gtk::ScrolledWindow::new();
    scroller.set_child(Some(&list));
    scroller.set_min_content_height(220);
    scroller.set_min_content_width(300);
    scroller.set_hscrollbar_policy(gtk::PolicyType::Never);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
    content.append(&entry);
    content.append(&scroller);

    let popover = gtk::Popover::new();
    popover.set_child(Some(&content));
    popover.set_parent(anchor);

    let shown: Rc<std::cell::RefCell<Vec<Kind>>> = Rc::new(std::cell::RefCell::new(Vec::new()));

    let fill = {
        let list = list.clone();
        let shown = shown.clone();
        move |query: &str| {
            while let Some(child) = list.first_child() {
                list.remove(&child);
            }
            let kinds = indicators::search(query);
            for kind in &kinds {
                let name = gtk::Label::new(Some(kind.name()));
                name.set_xalign(0.0);
                name.set_hexpand(true);
                let short = gtk::Label::new(Some(kind.short_name()));
                short.add_css_class("symbol-kind");
                short.set_valign(gtk::Align::Center);

                let row_box = gtk::Box::new(gtk::Orientation::Horizontal, 10);
                row_box.set_margin_top(7);
                row_box.set_margin_bottom(7);
                row_box.set_margin_start(10);
                row_box.set_margin_end(10);
                row_box.append(&name);
                row_box.append(&short);

                let row = gtk::ListBoxRow::new();
                row.set_child(Some(&row_box));
                list.append(&row);
            }
            *shown.borrow_mut() = kinds;
            if let Some(first) = list.row_at_index(0) {
                list.select_row(Some(&first));
            }
        }
    };
    fill("");

    let fill_on_type = fill.clone();
    entry.connect_search_changed(move |entry| fill_on_type(&entry.text()));

    let list_for_enter = list.clone();
    entry.connect_activate(move |_| {
        if let Some(row) = list_for_enter.selected_row() {
            row.activate();
        }
    });

    let window = window.clone();
    let page = page.clone();
    let popover_weak = popover.downgrade();
    list.connect_row_activated(move |_, row| {
        let at = row.index().max(0) as usize;
        let Some(kind) = shown.borrow().get(at).copied() else { return };
        let mut indicators = window.indicators();
        indicators.push(Indicator::new(window.next_indicator_id(), kind));
        window.set_indicators(indicators);
        rebuild_indicators(&window, &page);
        if let Some(popover) = popover_weak.upgrade() {
            popover.popdown();
        }
    });

    popover.popup();
    entry.grab_focus();
}

use gtk::glib;
