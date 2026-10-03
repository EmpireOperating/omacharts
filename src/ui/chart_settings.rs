//! Settings that belong to the chart rather than the app.
//!
//! Reached from the gear beside the legend, because that is where you are
//! looking when you want them. Two pages: how the bars are read, and what is
//! drawn on top of them.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use omacharts_engine::indicators::{self, Kind, LineStyle, Params, Reset, Stroke};
use omacharts_engine::theme::ColorChoice;
use omacharts_engine::{BarStyle, Indicator, Session};

use crate::store::Store;
use crate::ui::colors;
use crate::ui::window::Window;

pub const SETTING_SESSION: &str = "chart_session";

pub struct ChartSettings;

impl ChartSettings {
    pub fn present(window: &Rc<Window>, store: Rc<Store>) {
        ChartSettings::open(window, store, None);
    }

    fn open(window: &Rc<Window>, store: Rc<Store>, focus: Option<u32>) {
        let dialog = adw::PreferencesDialog::new();
        dialog.set_title("Chart");
        dialog.set_content_width(560);
        dialog.set_content_height(620);

        let chart_page = adw::PreferencesPage::new();
        chart_page.set_title("Chart");
        chart_page.set_icon_name(Some("preferences-system-symbolic"));
        chart_page.add(&bars_group(window));
        chart_page.add(&session_group(window, &store));
        dialog.add(&chart_page);

        let indicators_page = adw::PreferencesPage::new();
        indicators_page.set_title("Indicators");
        indicators_page.set_icon_name(Some("view-list-symbolic"));
        dialog.add(&indicators_page);
        let list = IndicatorList::new(&indicators_page);
        rebuild_indicators(window, &dialog, &list);

        dialog.present(Some(&window.window));
        if let Some(id) = focus {
            open_indicator_panel(window, &dialog, &list, id);
        }
    }

    /// Open straight onto one indicator's panel, for the gear beside it on the
    /// chart.
    pub fn present_indicator(window: &Rc<Window>, store: Rc<Store>, id: u32) {
        ChartSettings::open(window, store, Some(id));
    }
}

fn bars_group(window: &Rc<Window>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::new();
    group.set_title("Bars");

    let names: Vec<&str> = BarStyle::ALL.iter().map(|s| s.label()).collect();
    let row = adw::ComboRow::new();
    row.set_title("Style");
    row.set_model(Some(&gtk::StringList::new(&names)));
    row.set_selected(
        BarStyle::ALL.iter().position(|s| *s == window.bar_style()).unwrap_or(0) as u32
    );

    let window = window.clone();
    row.connect_selected_notify(move |row| {
        if let Some(style) = BarStyle::ALL.get(row.selected() as usize).copied() {
            window.set_bar_style(style);
        }
    });
    group.add(&row);
    group
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

/// The one group the indicator list lives in, and the rows currently in it.
///
/// Tracked explicitly because a preferences page does not hand back the groups
/// you added — its own children are a scroller and a clamp — so walking them
/// looking for things to remove finds nothing, and every rebuild quietly
/// appends another copy of the list.
#[derive(Clone)]
pub struct IndicatorList {
    group: adw::PreferencesGroup,
    rows: Rc<RefCell<Vec<gtk::Widget>>>,
}

impl IndicatorList {
    fn new(page: &adw::PreferencesPage) -> IndicatorList {
        let group = adw::PreferencesGroup::new();
        group.set_title("On the chart");
        page.add(&group);
        IndicatorList { group, rows: Rc::new(RefCell::new(Vec::new())) }
    }

    fn clear(&self) {
        for row in self.rows.borrow_mut().drain(..) {
            self.group.remove(&row);
        }
    }

    fn push(&self, row: &impl IsA<gtk::Widget>) {
        self.group.add(row);
        self.rows.borrow_mut().push(row.clone().upcast());
    }
}

/// Rebuild the list of indicators.
///
/// A list, and only a list: each row says what the indicator is and lets you
/// turn it off or take it away. Everything else lives on its own panel, because
/// a column of expanders holding every parameter of every indicator is a shape
/// you cannot read.
fn rebuild_indicators(
    window: &Rc<Window>,
    dialog: &adw::PreferencesDialog,
    list: &IndicatorList,
) {
    list.clear();
    let indicators = window.indicators();

    list.group.set_description(Some(if indicators.is_empty() {
        "Nothing yet."
    } else {
        "Drawn in order, each taking the next colour from the theme."
    }));

    let add = gtk::Button::from_icon_name("list-add-symbolic");
    add.add_css_class("flat");
    add.set_tooltip_text(Some("Add an indicator"));
    add.set_valign(gtk::Align::Center);
    let window_for_add = window.clone();
    let dialog_for_add = dialog.clone();
    let list_for_add = list.clone();
    add.connect_clicked(move |button| {
        pick_indicator(button, &window_for_add, &dialog_for_add, &list_for_add);
    });
    list.group.set_header_suffix(Some(&add));

    for (slot, indicator) in indicators.iter().enumerate() {
        list.push(&indicator_row(window, dialog, list, slot, indicator));
    }

    if indicators.is_empty() {
        let empty = adw::ActionRow::new();
        empty.set_title("Add an indicator");
        empty.set_subtitle("Moving averages, VWAP, volume profile");
        empty.set_activatable(true);
        let window_for_empty = window.clone();
        let dialog_for_empty = dialog.clone();
        let list_for_empty = list.clone();
        empty.connect_activated(move |row| {
            pick_indicator(row, &window_for_empty, &dialog_for_empty, &list_for_empty);
        });
        list.push(&empty);
    }
}

/// One line in the list: what it is, whether it is drawn, and a way in.
fn indicator_row(
    window: &Rc<Window>,
    dialog: &adw::PreferencesDialog,
    list: &IndicatorList,
    slot: usize,
    indicator: &Indicator,
) -> adw::ActionRow {
    let id = indicator.id;
    let row = adw::ActionRow::new();
    row.set_title(&indicator.label());
    row.set_subtitle(indicator.kind.name());
    row.set_activatable(true);

    // A dot in the indicator's own colour, so the list matches the chart.
    let swatch = gtk::DrawingArea::new();
    swatch.set_size_request(12, 12);
    swatch.set_valign(gtk::Align::Center);
    let colour = indicator.color(&window.theme(), slot);
    swatch.set_draw_func(move |_, cr, width, height| {
        let radius = (width.min(height) as f64) / 2.0;
        colors::set_source(cr, &colour);
        cr.arc(width as f64 / 2.0, height as f64 / 2.0, radius, 0.0, std::f64::consts::TAU);
        let _ = cr.fill();
    });
    row.add_prefix(&swatch);

    let visible = gtk::Switch::new();
    visible.set_active(indicator.visible);
    visible.set_valign(gtk::Align::Center);
    visible.set_tooltip_text(Some("Show on the chart"));
    let window_for_visible = window.clone();
    visible.connect_state_set(move |_, state| {
        update(&window_for_visible, id, |i| i.visible = state);
        glib::Propagation::Proceed
    });
    row.add_suffix(&visible);

    let remove = gtk::Button::from_icon_name("user-trash-symbolic");
    remove.add_css_class("flat");
    remove.set_valign(gtk::Align::Center);
    remove.set_tooltip_text(Some("Remove"));
    let window_for_remove = window.clone();
    let dialog_for_remove = dialog.clone();
    let list_for_remove = list.clone();
    remove.connect_clicked(move |_| {
        let kept: Vec<Indicator> =
            window_for_remove.indicators().into_iter().filter(|i| i.id != id).collect();
        window_for_remove.set_indicators(kept);
        rebuild_indicators(&window_for_remove, &dialog_for_remove, &list_for_remove);
    });
    row.add_suffix(&remove);
    row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));

    let window_for_open = window.clone();
    let dialog_for_open = dialog.clone();
    let list_for_open = list.clone();
    row.connect_activated(move |_| {
        open_indicator_panel(&window_for_open, &dialog_for_open, &list_for_open, id);
    });
    row
}

/// The indicator's own panel, pushed over the list.
fn open_indicator_panel(
    window: &Rc<Window>,
    dialog: &adw::PreferencesDialog,
    list: &IndicatorList,
    id: u32,
) {
    let indicators = window.indicators();
    let Some((slot, indicator)) =
        indicators.iter().enumerate().find(|(_, i)| i.id == id).map(|(s, i)| (s, i.clone()))
    else {
        return;
    };

    let page = adw::PreferencesPage::new();
    page.add(&parameters_group(window, dialog, list, &indicator));
    page.add(&appearance_group(window, dialog, list, slot, &indicator));
    for group in band_groups(window, dialog, list, slot, &indicator) {
        page.add(&group);
    }

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&page));

    let subpage = adw::NavigationPage::new(&toolbar, &indicator.label());
    dialog.push_subpage(&subpage);
}

/// The indicator's own line: colour, weight, pattern.
fn appearance_group(
    window: &Rc<Window>,
    dialog: &adw::PreferencesDialog,
    list: &IndicatorList,
    slot: usize,
    indicator: &Indicator,
) -> adw::PreferencesGroup {
    let id = indicator.id;
    let group = adw::PreferencesGroup::new();
    group.set_title("Line");

    group.add(&colour_row(
        window,
        dialog,
        list,
        "Colour",
        &indicator.color(&window.theme(), slot),
        indicator.color.is_none(),
        move |indicator, hex| {
            indicator.color = hex.map(|hex| ColorChoice::Fixed { hex });
        },
        id,
    ));
    group.add(&stroke_rows(window, dialog, list, id, "", indicator.stroke, {
        move |indicator: &mut Indicator, stroke: Stroke| indicator.stroke = stroke
    }));
    group
}

/// Width and style, as one expander-free pair of rows.
///
/// Returns the group they belong to so callers can drop it straight in.
fn stroke_rows(
    window: &Rc<Window>,
    dialog: &adw::PreferencesDialog,
    list: &IndicatorList,
    id: u32,
    prefix: &str,
    stroke: Stroke,
    apply: impl Fn(&mut Indicator, Stroke) + Clone + 'static,
) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::new();

    let width = adw::SpinRow::with_range(0.0, 5.0, 0.5);
    width.set_title(&format!("{prefix}Width"));
    width.set_subtitle("Zero draws no line");
    width.set_digits(1);
    width.set_value(stroke.width);
    let window_for_width = window.clone();
    let dialog_for_width = dialog.clone();
    let list_for_width = list.clone();
    let apply_width = apply.clone();
    width.connect_value_notify(move |row| {
        let value = row.value();
        let apply = apply_width.clone();
        update(&window_for_width, id, move |indicator| {
            apply(indicator, Stroke { width: value, style: stroke.style });
        });
        rebuild_indicators(&window_for_width, &dialog_for_width, &list_for_width);
    });
    group.add(&width);

    let names: Vec<&str> = LineStyle::ALL.iter().map(|s| s.label()).collect();
    let style = adw::ComboRow::new();
    style.set_title(&format!("{prefix}Style"));
    style.set_model(Some(&gtk::StringList::new(&names)));
    style.set_selected(LineStyle::ALL.iter().position(|s| *s == stroke.style).unwrap_or(0) as u32);
    let window_for_style = window.clone();
    let dialog_for_style = dialog.clone();
    let list_for_style = list.clone();
    style.connect_selected_notify(move |row| {
        let Some(chosen) = LineStyle::ALL.get(row.selected() as usize).copied() else { return };
        let apply = apply.clone();
        update(&window_for_style, id, move |indicator| {
            apply(indicator, Stroke { width: stroke.width, style: chosen });
        });
        rebuild_indicators(&window_for_style, &dialog_for_style, &list_for_style);
    });
    group.add(&style);
    group
}

/// A colour row: a quiet way back to the default, then the swatch.
///
/// Reset is enabled whenever a colour is pinned — not only when the hex
/// differs from the theme's. Pinning the same colour still stops the indicator
/// following the theme, so there is still something to undo.
#[allow(clippy::too_many_arguments)]
fn colour_row(
    window: &Rc<Window>,
    dialog: &adw::PreferencesDialog,
    list: &IndicatorList,
    title: &str,
    current: &str,
    is_default: bool,
    apply: impl Fn(&mut Indicator, Option<String>) + Clone + 'static,
    id: u32,
) -> adw::ActionRow {
    let row = adw::ActionRow::new();
    row.set_title(title);

    let button = gtk::ColorDialogButton::new(Some(gtk::ColorDialog::new()));
    button.set_rgba(&colors::parse(current));
    button.set_valign(gtk::Align::Center);
    button.add_css_class("swatch-button");

    let default = gtk::Button::with_label("Reset");
    default.add_css_class("flat");
    default.add_css_class("subtle-link");
    default.set_valign(gtk::Align::Center);
    default.set_tooltip_text(Some("Back to the colour the theme gives it"));
    default.set_sensitive(!is_default);

    // Only the list behind this panel is rebuilt when a colour changes, so
    // these two have to keep each other up to date — otherwise Reset stays
    // greyed out until the panel is reopened, right after the one action that
    // gives it something to undo.
    let window_for_colour = window.clone();
    let dialog_for_colour = dialog.clone();
    let list_for_colour = list.clone();
    let apply_for_colour = apply.clone();
    let default_weak = default.downgrade();
    button.connect_rgba_notify(move |button| {
        let hex = colors::to_hex(&button.rgba());
        let apply = apply_for_colour.clone();
        update(&window_for_colour, id, move |indicator| apply(indicator, Some(hex.clone())));
        if let Some(default) = default_weak.upgrade() {
            default.set_sensitive(true);
        }
        rebuild_indicators(&window_for_colour, &dialog_for_colour, &list_for_colour);
    });

    let window_for_default = window.clone();
    let dialog_for_default = dialog.clone();
    let list_for_default = list.clone();
    let button_weak = button.downgrade();
    default.connect_clicked(move |default| {
        let apply = apply.clone();
        update(&window_for_default, id, move |indicator| apply(indicator, None));
        default.set_sensitive(false);

        // Show the colour it reverted to, so the swatch is not left displaying
        // the one that was just discarded.
        if let Some(button) = button_weak.upgrade() {
            let indicators = window_for_default.indicators();
            if let Some(slot) = indicators.iter().position(|i| i.id == id) {
                let reverted = indicators[slot].color(&window_for_default.theme(), slot);
                button.set_rgba(&colors::parse(&reverted));
            }
        }
        rebuild_indicators(&window_for_default, &dialog_for_default, &list_for_default);
    });

    row.add_suffix(&default);
    row.add_suffix(&button);
    row
}

fn parameters_group(
    window: &Rc<Window>,
    dialog: &adw::PreferencesDialog,
    list: &IndicatorList,
    indicator: &Indicator,
) -> adw::PreferencesGroup {
    let id = indicator.id;
    let group = adw::PreferencesGroup::new();
    group.set_title("Parameters");

    match &indicator.params {
        Params::MovingAverage { period } => {
            group.add(&spin_row(
                window,
                dialog,
                list,
                id,
                "Period",
                *period as f64,
                1.0,
                500.0,
                1.0,
                move |indicator, value| {
                    indicator.params = Params::MovingAverage { period: value as usize };
                },
            ));
        }
        Params::Vwap { reset, .. } => {
            group.add(&reset_row(window, dialog, list, id, *reset));
        }
        Params::VolumeProfile { reset, rows, value_area } => {
            group.add(&reset_row(window, dialog, list, id, *reset));
            group.add(&spin_row(
                window,
                dialog,
                list,
                id,
                "Rows",
                *rows as f64,
                4.0,
                400.0,
                1.0,
                move |indicator, value| {
                    if let Params::VolumeProfile { rows, .. } = &mut indicator.params {
                        *rows = value as usize;
                    }
                },
            ));
            group.add(&spin_row(
                window,
                dialog,
                list,
                id,
                "Value area %",
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

/// One group per band, because every band has the same half-dozen choices and
/// burying them in expanders only hides which are on.
fn band_groups(
    window: &Rc<Window>,
    dialog: &adw::PreferencesDialog,
    list: &IndicatorList,
    slot: usize,
    indicator: &Indicator,
) -> Vec<adw::PreferencesGroup> {
    let Params::Vwap { bands, .. } = &indicator.params else { return Vec::new() };
    let id = indicator.id;
    let line_colour = indicator.color(&window.theme(), slot);
    let mut groups = Vec::new();

    for (index, band) in bands.iter().enumerate() {
        let group = adw::PreferencesGroup::new();
        group.set_title(&format!("Band {}", index + 1));

        let show = adw::ActionRow::new();
        show.set_title("Show");
        let switch = gtk::Switch::new();
        switch.set_active(band.enabled);
        switch.set_valign(gtk::Align::Center);
        let window_for_show = window.clone();
        let dialog_for_show = dialog.clone();
        let list_for_show = list.clone();
        switch.connect_state_set(move |_, on| {
            update(&window_for_show, id, move |indicator| {
                if let Params::Vwap { bands, .. } = &mut indicator.params {
                    if let Some(band) = bands.get_mut(index) {
                        band.enabled = on;
                    }
                }
            });
            rebuild_indicators(&window_for_show, &dialog_for_show, &list_for_show);
            glib::Propagation::Proceed
        });
        show.add_suffix(&switch);
        group.add(&show);

        let deviations = adw::SpinRow::with_range(0.1, 6.0, 0.1);
        deviations.set_title("Standard deviations");
        deviations.set_digits(1);
        deviations.set_value(band.deviations);
        let window_for_dev = window.clone();
        let dialog_for_dev = dialog.clone();
        let list_for_dev = list.clone();
        deviations.connect_value_notify(move |row| {
            let value = row.value();
            update(&window_for_dev, id, move |indicator| {
                if let Params::Vwap { bands, .. } = &mut indicator.params {
                    if let Some(band) = bands.get_mut(index) {
                        band.deviations = value;
                    }
                }
            });
            rebuild_indicators(&window_for_dev, &dialog_for_dev, &list_for_dev);
        });
        group.add(&deviations);

        let shaded = adw::ActionRow::new();
        shaded.set_title("Shaded");
        shaded.set_subtitle("Fill the area between this band's edges");
        let fill = gtk::Switch::new();
        fill.set_active(band.fill);
        fill.set_valign(gtk::Align::Center);
        let window_for_fill = window.clone();
        let dialog_for_fill = dialog.clone();
        let list_for_fill = list.clone();
        fill.connect_state_set(move |_, on| {
            update(&window_for_fill, id, move |indicator| {
                if let Params::Vwap { bands, .. } = &mut indicator.params {
                    if let Some(band) = bands.get_mut(index) {
                        band.fill = on;
                    }
                }
            });
            rebuild_indicators(&window_for_fill, &dialog_for_fill, &list_for_fill);
            glib::Propagation::Proceed
        });
        shaded.add_suffix(&fill);
        group.add(&shaded);

        let current = band
            .color
            .as_ref()
            .map(|c| c.resolve(&window.theme()))
            .unwrap_or_else(|| line_colour.clone());
        group.add(&colour_row(
            window,
            dialog,
            list,
            "Colour",
            &current,
            band.color.is_none(),
            move |indicator, hex| {
                if let Params::Vwap { bands, .. } = &mut indicator.params {
                    if let Some(band) = bands.get_mut(index) {
                        band.color = hex.map(|hex| ColorChoice::Fixed { hex });
                    }
                }
            },
            id,
        ));

        groups.push(group);
        groups.push(stroke_rows(window, dialog, list, id, "", band.stroke, {
            move |indicator: &mut Indicator, stroke: Stroke| {
                if let Params::Vwap { bands, .. } = &mut indicator.params {
                    if let Some(band) = bands.get_mut(index) {
                        band.stroke = stroke;
                    }
                }
            }
        }));
    }
    groups
}

fn reset_row(
    window: &Rc<Window>,
    dialog: &adw::PreferencesDialog,
    list: &IndicatorList,
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
    let dialog = dialog.clone();
    let list = list.clone();
    row.connect_selected_notify(move |row| {
        let Some(chosen) = Reset::ALL.get(row.selected() as usize).copied() else { return };
        update(&window, id, |indicator| match &mut indicator.params {
            Params::Vwap { reset, .. } => *reset = chosen,
            Params::VolumeProfile { reset, .. } => *reset = chosen,
            Params::MovingAverage { .. } => {}
        });
        rebuild_indicators(&window, &dialog, &list);
    });
    row
}

#[allow(clippy::too_many_arguments)]
fn spin_row(
    window: &Rc<Window>,
    dialog: &adw::PreferencesDialog,
    list: &IndicatorList,
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
    let dialog = dialog.clone();
    let list = list.clone();
    row.connect_value_notify(move |row| {
        let value = row.value();
        update(&window, id, |indicator| apply(indicator, value));
        // The list behind this panel shows the period in its title.
        rebuild_indicators(&window, &dialog, &list);
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
fn pick_indicator(
    anchor: &impl IsA<gtk::Widget>,
    window: &Rc<Window>,
    dialog: &adw::PreferencesDialog,
    list: &IndicatorList,
) {
    let entry = gtk::SearchEntry::new();
    entry.set_placeholder_text(Some("Indicator"));

    let rows = gtk::ListBox::new();
    rows.set_selection_mode(gtk::SelectionMode::Browse);
    rows.add_css_class("navigation-sidebar");

    let scroller = gtk::ScrolledWindow::new();
    scroller.set_child(Some(&rows));
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
        let rows = rows.clone();
        let shown = shown.clone();
        move |query: &str| {
            while let Some(child) = rows.first_child() {
                rows.remove(&child);
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
                rows.append(&row);
            }
            *shown.borrow_mut() = kinds;
            if let Some(first) = rows.row_at_index(0) {
                rows.select_row(Some(&first));
            }
        }
    };
    fill("");

    let fill_on_type = fill.clone();
    entry.connect_search_changed(move |entry| fill_on_type(&entry.text()));

    let rows_for_enter = rows.clone();
    entry.connect_activate(move |_| {
        if let Some(row) = rows_for_enter.selected_row() {
            row.activate();
        }
    });

    let window = window.clone();
    let dialog = dialog.clone();
    let indicator_list = list.clone();
    let popover_weak = popover.downgrade();
    rows.connect_row_activated(move |_, row| {
        let at = row.index().max(0) as usize;
        let Some(kind) = shown.borrow().get(at).copied() else { return };
        let id = window.next_indicator_id();
        let mut indicators = window.indicators();
        indicators.push(Indicator::new(id, kind));
        window.set_indicators(indicators);
        rebuild_indicators(&window, &dialog, &indicator_list);
        if let Some(popover) = popover_weak.upgrade() {
            popover.popdown();
        }
        // Straight into its settings: you added it to set it up.
        open_indicator_panel(&window, &dialog, &indicator_list, id);
    });

    // A GtkSearchEntry swallows Escape to clear itself, so closing hangs off
    // what it emits rather than off the key.
    let popover_weak = popover.downgrade();
    entry.connect_stop_search(move |_| {
        if let Some(popover) = popover_weak.upgrade() {
            popover.popdown();
        }
    });

    popover.popup();
    entry.grab_focus();
}

use gtk::glib;
