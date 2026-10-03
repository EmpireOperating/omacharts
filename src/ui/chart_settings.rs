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

/// Where the dialog should land when it opens.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Focus {
    Chart,
    Indicators,
    Indicator(u32),
}

pub struct ChartSettings;

impl ChartSettings {
    /// The chart's own settings, on the first page.
    pub fn present(window: &Rc<Window>, store: Rc<Store>) {
        ChartSettings::open(window, store, Focus::Chart);
    }

    /// Straight to the indicators.
    pub fn present_indicators(window: &Rc<Window>, store: Rc<Store>) {
        ChartSettings::open(window, store, Focus::Indicators);
    }

    /// Straight to one indicator's panel, for the gear beside it on the chart.
    pub fn present_indicator(window: &Rc<Window>, store: Rc<Store>, id: u32) {
        ChartSettings::open(window, store, Focus::Indicator(id));
    }

    fn open(window: &Rc<Window>, store: Rc<Store>, focus: Focus) {
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
        // Wired once here rather than on every rebuild: the button outlives
        // the rows, and reconnecting it each time would stack up handlers.
        let window_for_add = window.clone();
        let refresh_for_add = Refresh::rebuilding(window, &dialog, &list);
        list.add.connect_clicked(move |_| {
            pick_indicator(&window_for_add, &refresh_for_add);
        });
        rebuild_indicators(window, &dialog, &list);

        dialog.present(Some(&window.window));
        match focus {
            Focus::Chart => {}
            Focus::Indicators => dialog.set_visible_page(&indicators_page),
            Focus::Indicator(id) => {
                dialog.set_visible_page(&indicators_page);
                open_indicator_panel_for(
                    window,
                    &Refresh::rebuilding(window, &dialog, &list),
                    id,
                    Panel::Subpage(dialog.clone()),
                );
            }
        }
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

    let window_for_style = window.clone();
    row.connect_selected_notify(move |row| {
        if let Some(style) = BarStyle::ALL.get(row.selected() as usize).copied() {
            window_for_style.set_bar_style(style);
        }
    });
    group.add(&row);

    let grid = adw::ActionRow::new();
    grid.set_title("Gridlines");
    grid.set_subtitle("The axes and their labels stay either way");
    let switch = gtk::Switch::new();
    switch.set_valign(gtk::Align::Center);
    switch.set_active(window.show_grid());
    let window_for_grid = window.clone();
    switch.connect_state_set(move |_, on| {
        window_for_grid.set_show_grid(on);
        glib::Propagation::Proceed
    });
    grid.add_suffix(&switch);
    group.add(&grid);

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
    /// The + in the group's header. Kept so the hotkey can hang the picker off
    /// the same button the pointer would have used: a popover that appears
    /// somewhere else depending on how you asked for it is two features.
    add: gtk::Button,
}

impl IndicatorList {
    fn new(page: &adw::PreferencesPage) -> IndicatorList {
        let group = adw::PreferencesGroup::new();
        group.set_title("On the chart");
        page.add(&group);

        let add = gtk::Button::from_icon_name("list-add-symbolic");
        add.add_css_class("flat");
        add.set_tooltip_text(Some("Add an indicator (Ctrl+Shift+I)"));
        add.set_valign(gtk::Align::Center);
        group.set_header_suffix(Some(&add));

        IndicatorList { group, rows: Rc::new(RefCell::new(Vec::new())), add }
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

/// Something to run when an indicator changes, so whatever is showing it
/// redraws.
///
/// A callback rather than the preferences dialog and its list, because adding
/// an indicator no longer needs either — the picker opens straight from the
/// chart — and a row that sets a line width should not have to know which of
/// the two it is serving.
#[derive(Clone)]
struct Refresh(Rc<dyn Fn()>);

impl Refresh {
    /// Nothing behind the panel to redraw.
    fn none() -> Refresh {
        Refresh(Rc::new(|| {}))
    }

    fn rebuilding(
        window: &Rc<Window>,
        dialog: &adw::PreferencesDialog,
        list: &IndicatorList,
    ) -> Refresh {
        let window = window.clone();
        let dialog = dialog.clone();
        let list = list.clone();
        Refresh(Rc::new(move || rebuild_indicators(&window, &dialog, &list)))
    }

    fn run(&self) {
        (self.0)()
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
    let refresh = Refresh::rebuilding(window, dialog, list);

    list.group.set_description(Some(if indicators.is_empty() {
        "Nothing yet."
    } else {
        "Drag to reorder. Panes stack in the same order; colours come from the theme."
    }));


    for indicator in indicators.iter() {
        list.push(&indicator_row(window, dialog, &refresh, &indicators, indicator));
    }

    if indicators.is_empty() {
        let empty = adw::ActionRow::new();
        empty.set_title("Add an indicator");
        empty.set_subtitle("Moving averages, VWAP, volume profile");
        empty.set_activatable(true);
        let window_for_empty = window.clone();
        let refresh_for_empty = refresh.clone();
        empty.connect_activated(move |_| {
            pick_indicator(&window_for_empty, &refresh_for_empty);
        });
        list.push(&empty);
    }
}

/// One line in the list: what it is, whether it is drawn, and a way in.
fn indicator_row(
    window: &Rc<Window>,
    dialog: &adw::PreferencesDialog,
    refresh: &Refresh,
    all: &[Indicator],
    indicator: &Indicator,
) -> adw::ActionRow {
    let id = indicator.id;
    let colour = drawn_colour(window, all, id);
    let row = adw::ActionRow::new();
    row.set_title(&indicator.label());
    row.set_subtitle(indicator.kind.name());
    row.set_activatable(true);

    // A dot in the indicator's own colour, so the list matches the chart.
    let swatch = gtk::DrawingArea::new();
    swatch.set_size_request(12, 12);
    swatch.set_valign(gtk::Align::Center);
    swatch.set_draw_func(move |_, cr, width, height| {
        let radius = (width.min(height) as f64) / 2.0;
        colors::set_source(cr, &colour);
        cr.arc(width as f64 / 2.0, height as f64 / 2.0, radius, 0.0, std::f64::consts::TAU);
        let _ = cr.fill();
    });
    row.add_prefix(&swatch);

    // Order is a property of the list, so its controls sit on the list side of
    // the row. It decides two things at once: which strip is stacked above
    // which under the chart, and which line is drawn over which on it.
    // The handle is prepended rather than appended, so it ends up left of the
    // swatch: order is a property of the list, and the list runs down the left
    // edge. It is there to say the row can be dragged — the drag itself works
    // anywhere on the row that a control is not already using.
    let handle = gtk::Image::from_icon_name("list-drag-handle-symbolic");
    handle.add_css_class("dim-label");
    handle.set_tooltip_text(Some("Drag to reorder"));
    row.add_prefix(&handle);
    wire_reorder(window, refresh, &row, id);

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
    let refresh_for_remove = refresh.clone();
    remove.connect_clicked(move |_| {
        let kept: Vec<Indicator> =
            window_for_remove.indicators().into_iter().filter(|i| i.id != id).collect();
        window_for_remove.set_indicators(kept);
        refresh_for_remove.run();
    });
    row.add_suffix(&remove);
    row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));

    let window_for_open = window.clone();
    let refresh_for_open = refresh.clone();
    let dialog_for_open = dialog.clone();
    row.connect_activated(move |_| {
        open_indicator_panel_for(
            &window_for_open,
            &refresh_for_open,
            id,
            Panel::Subpage(dialog_for_open.clone()),
        );
    });
    row
}

/// Let a row be picked up and dropped on another to change the order.
///
/// Dragging rather than a pair of arrows, which is what the watchlist already
/// does and what a list of half a dozen things wants: moving the bottom one to
/// the top is one gesture rather than five clicks.
///
/// The id travels as the payload rather than the position, because the list is
/// rebuilt while the drag is in flight and a position would by then mean a
/// different row.
fn wire_reorder(window: &Rc<Window>, refresh: &Refresh, row: &adw::ActionRow, id: u32) {
    let source = gtk::DragSource::new();
    source.set_actions(gtk::gdk::DragAction::MOVE);
    let payload = id.to_string();
    source.connect_prepare(move |_, _, _| {
        Some(gtk::gdk::ContentProvider::for_value(&payload.to_value()))
    });
    row.add_controller(source);

    let target = gtk::DropTarget::new(glib::Type::STRING, gtk::gdk::DragAction::MOVE);
    let window = window.clone();
    let refresh = refresh.clone();
    target.connect_drop(move |_, value, _, _| {
        let Ok(moving) = value.get::<String>().unwrap_or_default().parse::<u32>() else {
            return false;
        };
        if moving == id {
            return false;
        }
        let mut indicators = window.indicators();
        let (Some(from), Some(to)) = (
            indicators.iter().position(|i| i.id == moving),
            indicators.iter().position(|i| i.id == id),
        ) else {
            return false;
        };
        // Taken out before the destination is used, so dropping downwards
        // lands after the row you dropped on and upwards lands in its place —
        // which is what the pointer was over either way.
        let dragged = indicators.remove(from);
        indicators.insert(to, dragged);
        window.set_indicators(indicators);
        refresh.run();
        true
    });
    row.add_controller(target);
}

/// Where an indicator's settings are shown.
enum Panel {
    /// Pushed over the list it was opened from, with a Done button.
    Subpage(adw::PreferencesDialog),
    /// A dialog of its own, with one button that adds the indicator. Used by
    /// the add path, which can start from the chart with no settings window
    /// open at all, so there is nothing to push a page onto.
    Add,
}

/// One indicator's settings.
///
/// Changes apply as you make them, on the chart, whichever way this was
/// opened: you are looking at the thing you are configuring. For the add path
/// that means the indicator is already drawn while you set it up, and
/// dismissing the dialog takes it back off again — the Add button is how you
/// say you meant it.
fn open_indicator_panel_for(window: &Rc<Window>, refresh: &Refresh, id: u32, panel: Panel) {
    let indicators = window.indicators();
    let Some(indicator) = indicators.iter().find(|i| i.id == id).cloned() else {
        return;
    };
    let colour = drawn_colour(window, &indicators, id);

    let page = adw::PreferencesPage::new();
    page.add(&parameters_group(window, refresh, &indicator));
    page.add(&appearance_group(window, refresh, &colour, &indicator));
    for group in band_groups(window, refresh, &colour, &indicator) {
        page.add(&group);
    }

    let header = adw::HeaderBar::new();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&page));

    match panel {
        Panel::Subpage(dialog) => {
            header.set_show_end_title_buttons(false);
            let done = gtk::Button::with_label("Done");
            done.add_css_class("suggested-action");
            let dialog_for_done = dialog.clone();
            done.connect_clicked(move |_| {
                dialog_for_done.pop_subpage();
            });
            header.pack_end(&done);

            let subpage = adw::NavigationPage::new(&toolbar, &indicator.label());
            dialog.push_subpage(&subpage);
        }
        Panel::Add => {
            let modal = adw::Dialog::new();
            modal.set_title(&indicator.label());
            modal.set_content_width(560);
            modal.set_content_height(620);
            modal.set_child(Some(&toolbar));

            // One button, and it finishes the job. Backing out is the dialog's
            // own close, which is where everything else on the desktop puts it.
            let add = gtk::Button::with_label("Add");
            add.add_css_class("suggested-action");
            let added = Rc::new(std::cell::Cell::new(false));
            let added_on_click = added.clone();
            let modal_for_add = modal.clone();
            add.connect_clicked(move |_| {
                added_on_click.set(true);
                modal_for_add.close();
            });
            header.pack_end(&add);

            // Closed any other way — Escape, the close button, clicking out —
            // means it was never added, so it comes back off the chart.
            let window_for_close = window.clone();
            let refresh_for_close = refresh.clone();
            modal.connect_closed(move |_| {
                if added.get() {
                    return;
                }
                let kept: Vec<Indicator> =
                    window_for_close.indicators().into_iter().filter(|i| i.id != id).collect();
                window_for_close.set_indicators(kept);
                refresh_for_close.run();
            });

            modal.present(Some(&window.window));
        }
    }
}

/// The indicator's own line: colour, weight, pattern.
fn appearance_group(
    window: &Rc<Window>,
    refresh: &Refresh,
    colour: &str,
    indicator: &Indicator,
) -> adw::PreferencesGroup {
    let id = indicator.id;
    let group = adw::PreferencesGroup::new();
    group.set_title("Line");

    group.add(&colour_row(
        window,
        refresh,
        "Colour",
        colour,
        indicator.color.clone(),
        move |indicator, choice| indicator.color = choice,
        id,
    ));
    group.add(&stroke_rows(window, refresh, id, "", indicator.stroke, {
        move |indicator: &mut Indicator, stroke: Stroke| indicator.stroke = stroke
    }));
    group
}

/// Width and style, as one expander-free pair of rows.
///
/// Returns the group they belong to so callers can drop it straight in.
fn stroke_rows(
    window: &Rc<Window>,
    refresh: &Refresh,
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
    let refresh_for_width = refresh.clone();
    let apply_width = apply.clone();
    width.connect_value_notify(move |row| {
        let value = row.value();
        let apply = apply_width.clone();
        update(&window_for_width, id, move |indicator| {
            apply(indicator, Stroke { width: value, style: stroke.style });
        });
        refresh_for_width.run();
    });
    group.add(&width);

    let names: Vec<&str> = LineStyle::ALL.iter().map(|s| s.label()).collect();
    let style = adw::ComboRow::new();
    style.set_title(&format!("{prefix}Style"));
    style.set_model(Some(&gtk::StringList::new(&names)));
    style.set_selected(LineStyle::ALL.iter().position(|s| *s == stroke.style).unwrap_or(0) as u32);
    let window_for_style = window.clone();
    let refresh_for_style = refresh.clone();
    style.connect_selected_notify(move |row| {
        let Some(chosen) = LineStyle::ALL.get(row.selected() as usize).copied() else { return };
        let apply = apply.clone();
        update(&window_for_style, id, move |indicator| {
            apply(indicator, Stroke { width: stroke.width, style: chosen });
        });
        refresh_for_style.run();
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
    refresh: &Refresh,
    title: &str,
    current: &str,
    choice: Option<ColorChoice>,
    apply: impl Fn(&mut Indicator, Option<ColorChoice>) + Clone + 'static,
    id: u32,
) -> adw::ActionRow {
    let row = adw::ActionRow::new();
    row.set_title(title);

    let default = gtk::Button::with_label("Reset");
    default.add_css_class("flat");
    default.add_css_class("subtle-link");
    default.set_valign(gtk::Align::Center);
    default.set_tooltip_text(Some("Back to the colour the theme gives it"));
    default.set_sensitive(choice.is_some());

    // Only the list behind this panel is rebuilt when a colour changes, so
    // these two have to keep each other up to date — otherwise Reset stays
    // greyed out until the panel is reopened, right after the one action that
    // gives it something to undo.
    let window_for_colour = window.clone();
    let refresh_for_colour = refresh.clone();
    let apply_for_colour = apply.clone();
    let default_weak = default.downgrade();
    let button = crate::ui::palette::picker(
        &window.theme(),
        choice,
        current,
        move |picked| {
            let apply = apply_for_colour.clone();
            update(&window_for_colour, id, move |indicator| {
                apply(indicator, Some(picked.clone()))
            });
            if let Some(default) = default_weak.upgrade() {
                default.set_sensitive(true);
            }
            refresh_for_colour.run();
        },
    );

    let window_for_default = window.clone();
    let refresh_for_default = refresh.clone();
    let button_weak = button.downgrade();
    default.connect_clicked(move |default| {
        let apply = apply.clone();
        update(&window_for_default, id, move |indicator| apply(indicator, None));
        default.set_sensitive(false);
        // Show what it reverted to. The default is not a fixed colour — it is
        // whichever of the palette this indicator's siblings have left free —
        // so it has to be asked for after the change, not guessed before it.
        if let Some(button) = button_weak.upgrade() {
            let indicators = window_for_default.indicators();
            crate::ui::palette::show(&button, &drawn_colour(&window_for_default, &indicators, id));
        }
        refresh_for_default.run();
    });

    row.add_suffix(&default);
    row.add_suffix(&button);
    row
}

fn parameters_group(
    window: &Rc<Window>,
    refresh: &Refresh,
    indicator: &Indicator,
) -> adw::PreferencesGroup {
    let id = indicator.id;
    let group = adw::PreferencesGroup::new();
    group.set_title("Parameters");

    match &indicator.params {
        Params::MovingAverage { period } => {
            group.add(&spin_row(
                window,
                refresh,
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
            group.add(&reset_row(window, refresh, id, *reset));
        }
        Params::Volume { height } => {
            group.add(&pane_height_row(window, refresh, id, *height));
        }
        Params::Rsi { period, height, overbought, oversold } => {
            group.add(&spin_row(
                window, refresh, id, "Period", *period as f64, 2.0, 200.0, 1.0,
                move |indicator, value| {
                    if let Params::Rsi { period, .. } = &mut indicator.params {
                        *period = value as usize;
                    }
                },
            ));
            group.add(&spin_row(
                window, refresh, id, "Overbought", *overbought, 50.0, 100.0, 1.0,
                move |indicator, value| {
                    if let Params::Rsi { overbought, .. } = &mut indicator.params {
                        *overbought = value;
                    }
                },
            ));
            group.add(&spin_row(
                window, refresh, id, "Oversold", *oversold, 0.0, 50.0, 1.0,
                move |indicator, value| {
                    if let Params::Rsi { oversold, .. } = &mut indicator.params {
                        *oversold = value;
                    }
                },
            ));
            group.add(&pane_height_row(window, refresh, id, *height));
        }
        Params::Atr { period, height } => {
            group.add(&spin_row(
                window, refresh, id, "Period", *period as f64, 2.0, 200.0, 1.0,
                move |indicator, value| {
                    if let Params::Atr { period, .. } = &mut indicator.params {
                        *period = value as usize;
                    }
                },
            ));
            group.add(&pane_height_row(window, refresh, id, *height));
        }
        Params::VolumeProfile { reset, rows, value_area } => {
            group.add(&reset_row(window, refresh, id, *reset));
            for row in rows_rows(window, refresh, id, *rows) {
                group.add(&row);
            }
            group.add(&spin_row(
                window,
                refresh,
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
    refresh: &Refresh,
    line_colour: &str,
    indicator: &Indicator,
) -> Vec<adw::PreferencesGroup> {
    let Params::Vwap { bands, .. } = &indicator.params else { return Vec::new() };
    let id = indicator.id;
    let line_colour = line_colour.to_string();
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
        let refresh_for_show = refresh.clone();
        switch.connect_state_set(move |_, on| {
            update(&window_for_show, id, move |indicator| {
                if let Params::Vwap { bands, .. } = &mut indicator.params {
                    if let Some(band) = bands.get_mut(index) {
                        band.enabled = on;
                    }
                }
            });
            refresh_for_show.run();
            glib::Propagation::Proceed
        });
        show.add_suffix(&switch);
        group.add(&show);

        let deviations = adw::SpinRow::with_range(0.1, 6.0, 0.1);
        deviations.set_title("Standard deviations");
        deviations.set_digits(1);
        deviations.set_value(band.deviations);
        let window_for_dev = window.clone();
        let refresh_for_dev = refresh.clone();
        deviations.connect_value_notify(move |row| {
            let value = row.value();
            update(&window_for_dev, id, move |indicator| {
                if let Params::Vwap { bands, .. } = &mut indicator.params {
                    if let Some(band) = bands.get_mut(index) {
                        band.deviations = value;
                    }
                }
            });
            refresh_for_dev.run();
        });
        group.add(&deviations);

        let shaded = adw::ActionRow::new();
        shaded.set_title("Shaded");
        shaded.set_subtitle("Fill the area between this band's edges");
        let fill = gtk::Switch::new();
        fill.set_active(band.fill);
        fill.set_valign(gtk::Align::Center);
        let window_for_fill = window.clone();
        let refresh_for_fill = refresh.clone();
        fill.connect_state_set(move |_, on| {
            update(&window_for_fill, id, move |indicator| {
                if let Params::Vwap { bands, .. } = &mut indicator.params {
                    if let Some(band) = bands.get_mut(index) {
                        band.fill = on;
                    }
                }
            });
            refresh_for_fill.run();
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
            refresh,
            "Colour",
            &current,
            band.color.clone(),
            move |indicator, choice| {
                if let Params::Vwap { bands, .. } = &mut indicator.params {
                    if let Some(band) = bands.get_mut(index) {
                        band.color = choice;
                    }
                }
            },
            id,
        ));

        groups.push(group);
        groups.push(stroke_rows(window, refresh, id, "", band.stroke, {
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
    refresh: &Refresh,
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
    let refresh = refresh.clone();
    row.connect_selected_notify(move |row| {
        let Some(chosen) = Reset::ALL.get(row.selected() as usize).copied() else { return };
        update(&window, id, |indicator| match &mut indicator.params {
            Params::Vwap { reset, .. } => *reset = chosen,
            Params::VolumeProfile { reset, .. } => *reset = chosen,
            Params::MovingAverage { .. }
            | Params::Volume { .. }
            | Params::Rsi { .. }
            | Params::Atr { .. } => {}
        });
        refresh.run();
    });
    row
}

#[allow(clippy::too_many_arguments)]
fn spin_row(
    window: &Rc<Window>,
    refresh: &Refresh,
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
    let refresh = refresh.clone();
    row.connect_value_notify(move |row| {
        let value = row.value();
        update(&window, id, |indicator| apply(indicator, value));
        // The list behind this panel shows the period in its title.
        refresh.run();
    });
    row
}

/// How much of the chart an indicator's own strip takes.
///
/// One row for every indicator that has a strip, rather than one per kind: the
/// question is the same whichever of them is asking it.
fn pane_height_row(
    window: &Rc<Window>,
    refresh: &Refresh,
    id: u32,
    height: f64,
) -> adw::SpinRow {
    spin_row(
        window,
        refresh,
        id,
        "Pane height %",
        height * 100.0,
        5.0,
        60.0,
        1.0,
        move |indicator, value| {
            let share = value / 100.0;
            match &mut indicator.params {
                Params::Volume { height }
                | Params::Rsi { height, .. }
                | Params::Atr { height, .. } => *height = share,
                _ => {}
            }
        },
    )
}

/// How a volume profile is divided up: automatically, or by a number.
///
/// Two rows rather than one control, because they answer different questions.
/// The switch decides whether the instrument's own price increment sets the
/// row height — a nickel on a share, half a pip on a currency major — and the
/// spin is for the rare chart where you want a specific count instead. While
/// the switch is on the spin still shows the count being drawn, so automatic
/// is something you can see rather than take on faith.
fn rows_rows(
    window: &Rc<Window>,
    refresh: &Refresh,
    id: u32,
    rows: Option<usize>,
) -> Vec<adw::PreferencesRow> {
    let drawn = window.profile_rows(id);

    let spin = adw::SpinRow::with_range(4.0, 400.0, 1.0);
    spin.set_title("Rows");
    spin.set_value(rows.or(drawn).unwrap_or(48) as f64);
    spin.set_sensitive(rows.is_some());

    let automatic = adw::ActionRow::new();
    automatic.set_title("Automatic");
    automatic.set_subtitle("Rows as tall as the instrument's own price steps");
    let switch = gtk::Switch::new();
    switch.set_active(rows.is_none());
    switch.set_valign(gtk::Align::Center);
    automatic.add_suffix(&switch);
    automatic.set_activatable_widget(Some(&switch));

    let window_for_switch = window.clone();
    let refresh_for_switch = refresh.clone();
    let spin_weak = spin.downgrade();
    switch.connect_active_notify(move |switch| {
        let on = switch.is_active();
        // Turning it off hands over the count that was on screen a moment ago,
        // so taking control does not also change the chart.
        let taken = spin_weak.upgrade().map(|spin| spin.value() as usize).unwrap_or(48);
        update(&window_for_switch, id, move |indicator| {
            if let Params::VolumeProfile { rows, .. } = &mut indicator.params {
                *rows = if on { None } else { Some(taken) };
            }
        });
        if let Some(spin) = spin_weak.upgrade() {
            spin.set_sensitive(!on);
            if on {
                if let Some(drawn) = window_for_switch.profile_rows(id) {
                    spin.set_value(drawn as f64);
                }
            }
        }
        refresh_for_switch.run();
    });

    let window_for_spin = window.clone();
    let refresh_for_spin = refresh.clone();
    spin.connect_value_notify(move |spin| {
        let value = spin.value() as usize;
        update(&window_for_spin, id, move |indicator| {
            if let Params::VolumeProfile { rows, .. } = &mut indicator.params {
                *rows = Some(value);
            }
        });
        refresh_for_spin.run();
    });

    vec![automatic.upcast(), spin.upcast()]
}

/// The colour this indicator is actually drawn in.
///
/// Asked of the whole set rather than of the indicator, because that is where
/// the answer lives now: an automatic colour is the first one its siblings have
/// not taken, so a second moving average is a different colour from the first.
fn drawn_colour(window: &Rc<Window>, indicators: &[Indicator], id: u32) -> String {
    let colours = omacharts_engine::palette_colors(indicators, &window.theme());
    indicators
        .iter()
        .position(|i| i.id == id)
        .and_then(|at| colours.get(at).cloned())
        .unwrap_or_else(|| window.theme().ui.accent.clone())
}

/// Change one indicator in place and redraw.
fn update(window: &Rc<Window>, id: u32, change: impl Fn(&mut Indicator)) {
    let mut indicators = window.indicators();
    let Some(indicator) = indicators.iter_mut().find(|i| i.id == id) else { return };
    change(indicator);
    window.set_indicators(indicators);
}

/// Pick the kind of indicator to add.
///
/// A dialog rather than a popover hanging off whatever was clicked. It is
/// reached from a button in the settings and from a key on the chart, and the
/// key has nothing to point at — a picker that lands in the top-left corner
/// when summoned by Ctrl+Shift+I and under a button otherwise is two features
/// wearing one name. The symbol search, which has exactly the same two ways
/// in, has always been a dialog for the same reason.
pub fn add_indicator(window: &Rc<Window>) {
    pick_indicator(window, &Refresh::none());
}

fn pick_indicator(window: &Rc<Window>, refresh: &Refresh) {
    let entry = gtk::SearchEntry::new();
    entry.set_placeholder_text(Some("Indicator"));

    let rows = gtk::ListBox::new();
    rows.set_selection_mode(gtk::SelectionMode::Browse);
    rows.add_css_class("navigation-sidebar");

    let scroller = gtk::ScrolledWindow::new();
    scroller.set_child(Some(&rows));
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
    dialog.set_title("Add an indicator");
    dialog.set_content_width(420);
    dialog.set_content_height(380);
    dialog.set_child(Some(&content));

    let shown: Rc<RefCell<Vec<Kind>>> = Rc::new(RefCell::new(Vec::new()));

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

    // Up and down move the selection while focus stays in the entry, so one
    // hand never has to leave the keyboard.
    let keys = gtk::EventControllerKey::new();
    let rows_for_keys = rows.clone();
    keys.connect_key_pressed(move |_, key, _, _| {
        let delta = match key {
            gtk::gdk::Key::Down => 1,
            gtk::gdk::Key::Up => -1,
            _ => return glib::Propagation::Proceed,
        };
        let at = rows_for_keys.selected_row().map(|r| r.index()).unwrap_or(0);
        if let Some(next) = rows_for_keys.row_at_index(at + delta) {
            rows_for_keys.select_row(Some(&next));
        }
        glib::Propagation::Stop
    });
    entry.add_controller(keys);

    let window_for_pick = window.clone();
    let refresh_for_pick = refresh.clone();
    let dialog_for_pick = dialog.clone();
    rows.connect_row_activated(move |_, row| {
        let at = row.index().max(0) as usize;
        let Some(kind) = shown.borrow().get(at).copied() else { return };
        let id = window_for_pick.next_indicator_id();
        let mut indicators = window_for_pick.indicators();
        indicators.push(Indicator::new(id, kind));
        window_for_pick.set_indicators(indicators);
        refresh_for_pick.run();
        dialog_for_pick.close();
        // Straight into its settings: picking the kind and setting it up are
        // two steps, and the second is where you say whether you meant it.
        open_indicator_panel_for(&window_for_pick, &refresh_for_pick, id, Panel::Add);
    });

    // A GtkSearchEntry swallows Escape to clear itself, so closing hangs off
    // what it emits rather than off the key.
    let dialog_for_stop = dialog.clone();
    entry.connect_stop_search(move |_| {
        dialog_for_stop.close();
    });

    // And again on the dialog, for an Escape pressed with focus in the list.
    let escape = gtk::EventControllerKey::new();
    let dialog_for_escape = dialog.clone();
    escape.connect_key_pressed(move |_, key, _, _| {
        if key == gtk::gdk::Key::Escape {
            dialog_for_escape.close();
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    dialog.add_controller(escape);

    dialog.present(Some(&window.window));
    entry.grab_focus();
}

use gtk::glib;
