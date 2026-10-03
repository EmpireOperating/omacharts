//! Settings.
//!
//! Two choices sit at the top because they are the two people actually make:
//! the overall theme, and what candles look like. Everything below is the
//! same two things taken apart — every colour a theme defines is editable,
//! once the theme is yours to edit.
//!
//! Built-ins and the Omarchy theme are not editable in place. "Duplicate"
//! makes a copy you own, which is both simpler to reason about and means a
//! fiddled palette can always be abandoned by switching back.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use omacharts_engine::theme::{
    BarScheme, BarSlot, Source, Theme, UiSlot, SWATCH_NAMES, THEME_BARS_ID,
};

use crate::store::Store;
use crate::theming::Theming;
use crate::ui::colors;

pub struct Preferences;

struct Context {
    store: Rc<Store>,
    theming: Rc<RefCell<Theming>>,
    on_change: Rc<dyn Fn()>,
    page: adw::PreferencesPage,
    data_page: adw::PreferencesPage,
    groups: RefCell<Vec<adw::PreferencesGroup>>,
}

impl Preferences {
    pub fn present(
        parent: &impl IsA<gtk::Widget>,
        store: Rc<Store>,
        theming: Rc<RefCell<Theming>>,
        on_change: Rc<dyn Fn()>,
    ) {
        let dialog = adw::PreferencesDialog::new();
        dialog.set_title("Preferences");
        dialog.set_content_width(560);
        dialog.set_content_height(640);

        let page = adw::PreferencesPage::new();
        page.set_title("Appearance");
        page.set_icon_name(Some("applications-graphics-symbolic"));

        let data_page = adw::PreferencesPage::new();
        data_page.set_title("Data");
        data_page.set_icon_name(Some("folder-download-symbolic"));

        dialog.add(&page);
        dialog.add(&data_page);

        let context = Rc::new(Context {
            store,
            theming,
            on_change,
            page,
            data_page,
            groups: RefCell::new(Vec::new()),
        });
        rebuild(&context);
        build_data_page(&context);

        dialog.present(Some(parent));
    }
}

/// Clear and re-add the appearance groups.
///
/// Called whenever the selection changes, because which editors belong on the
/// page depends on whether the selected theme is editable.
fn rebuild(context: &Rc<Context>) {
    for group in context.groups.borrow_mut().drain(..) {
        context.page.remove(&group);
    }
    let mut groups = Vec::new();

    groups.push(theme_group(context));
    let theme = context.theming.borrow().theme();
    if theme.source.is_editable() {
        for name in UiSlot::GROUPS {
            groups.push(theme_colors_group(context, name));
        }
        groups.push(palette_group(context));
    }

    groups.push(bars_group(context));
    let scheme = context.theming.borrow().bar_scheme();
    if scheme.source.is_editable() {
        for name in BarSlot::GROUPS {
            groups.push(bar_colors_group(context, name));
        }
    }

    for group in &groups {
        context.page.add(group);
    }
    *context.groups.borrow_mut() = groups;
}

fn theme_group(context: &Rc<Context>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::new();
    group.set_title("Theme");
    let theming = context.theming.borrow();
    group.set_description(Some(if theming.omarchy_available() {
        "“Follow desktop” tracks your Omarchy theme as you change it. \
         The others are fixed, and stay put whatever the desktop does."
    } else {
        "How the whole app looks."
    }));

    let ids: Vec<String> = theming.themes().iter().map(|t| t.id.clone()).collect();
    // The entry that tracks the desktop has to say so. Labelled with just the
    // desktop theme's name it is indistinguishable from a fixed preset, and
    // the difference between them is the whole point: one keeps changing.
    let names: Vec<String> = theming
        .themes()
        .iter()
        .map(|t| {
            if t.id == omacharts_engine::theme::OMARCHY_ID {
                format!("Follow desktop · {}", t.name)
            } else {
                t.name.clone()
            }
        })
        .collect();
    let selected = ids.iter().position(|id| id == theming.theme_id()).unwrap_or(0);
    let source = theming.theme().source;
    drop(theming);

    let row = adw::ComboRow::new();
    row.set_title("Theme");
    row.set_model(Some(&string_list(&names)));
    row.set_selected(selected as u32);

    let ctx = context.clone();
    let choices = ids.clone();
    row.connect_selected_notify(move |row| {
        let Some(id) = choices.get(row.selected() as usize) else { return };
        ctx.theming.borrow_mut().select_theme(id, &ctx.store);
        apply(&ctx);
        rebuild(&ctx);
    });
    group.add(&row);

    group.add(&duplicate_row(context, source, true));
    group
}

fn bars_group(context: &Rc<Context>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::new();
    group.set_title("Bars");
    group.set_description(Some("Candle colours, chosen separately from the theme."));

    let theming = context.theming.borrow();
    let schemes = theming.bar_schemes();
    let ids: Vec<String> = schemes.iter().map(|s| s.id.clone()).collect();
    let names: Vec<String> = schemes
        .iter()
        .map(|s| {
            if s.id == THEME_BARS_ID {
                "Match theme".to_string()
            } else {
                s.name.clone()
            }
        })
        .collect();
    let selected = ids.iter().position(|id| id == theming.scheme_id()).unwrap_or(0);
    let source = theming.bar_scheme().source;
    drop(theming);

    let row = adw::ComboRow::new();
    row.set_title("Bar scheme");
    row.set_model(Some(&string_list(&names)));
    row.set_selected(selected as u32);

    let ctx = context.clone();
    let choices = ids.clone();
    row.connect_selected_notify(move |row| {
        let Some(id) = choices.get(row.selected() as usize) else { return };
        ctx.theming.borrow_mut().select_bar_scheme(id, &ctx.store);
        apply(&ctx);
        rebuild(&ctx);
    });
    group.add(&row);

    group.add(&duplicate_row(context, source, false));
    group
}

/// "Duplicate" for things you cannot edit, "Delete" for the ones you can.
fn duplicate_row(context: &Rc<Context>, source: Source, is_theme: bool) -> adw::ActionRow {
    let row = adw::ActionRow::new();
    if source.is_editable() {
        row.set_title("This is yours to edit");
        row.set_subtitle("Colours below are saved as you change them.");

        let delete = gtk::Button::with_label("Delete");
        delete.add_css_class("destructive-action");
        delete.set_valign(gtk::Align::Center);
        let ctx = context.clone();
        delete.connect_clicked(move |_| {
            let mut theming = ctx.theming.borrow_mut();
            if is_theme {
                let id = theming.theme().id;
                ctx.store.delete_theme(&id);
                theming.reload_custom(&ctx.store);
                let fallback = theming.themes().first().map(|t| t.id.clone());
                if let Some(id) = fallback {
                    theming.select_theme(&id, &ctx.store);
                }
            } else {
                let id = theming.bar_scheme().id;
                ctx.store.delete_bar_scheme(&id);
                theming.reload_custom(&ctx.store);
                theming.select_bar_scheme(THEME_BARS_ID, &ctx.store);
            }
            drop(theming);
            apply(&ctx);
            rebuild(&ctx);
        });
        row.add_suffix(&delete);
    } else {
        row.set_title("Make it yours");
        row.set_subtitle("Duplicate this to edit its colours.");

        let button = gtk::Button::with_label("Duplicate");
        button.set_valign(gtk::Align::Center);
        let ctx = context.clone();
        button.connect_clicked(move |_| {
            let mut theming = ctx.theming.borrow_mut();
            if is_theme {
                let base = theming.theme();
                let (id, name) = unique_name(
                    &base.name,
                    &theming.themes().iter().map(|t| t.id.clone()).collect::<Vec<_>>(),
                );
                let copy: Theme = base.duplicate(id.clone(), name);
                ctx.store.save_theme(&copy);
                theming.reload_custom(&ctx.store);
                theming.select_theme(&id, &ctx.store);
            } else {
                let base = theming.bar_scheme();
                let (id, name) = unique_name(
                    &base.name,
                    &theming.bar_schemes().iter().map(|s| s.id.clone()).collect::<Vec<_>>(),
                );
                let copy: BarScheme = base.duplicate(id.clone(), name);
                ctx.store.save_bar_scheme(&copy);
                theming.reload_custom(&ctx.store);
                theming.select_bar_scheme(&id, &ctx.store);
            }
            drop(theming);
            apply(&ctx);
            rebuild(&ctx);
        });
        row.add_suffix(&button);
    }
    row
}

fn theme_colors_group(context: &Rc<Context>, group_name: &str) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::new();
    group.set_title(group_name);

    let theme = context.theming.borrow().theme();
    for slot in UiSlot::ALL.into_iter().filter(|s| s.group() == group_name) {
        let row = adw::ActionRow::new();
        row.set_title(slot.label());

        let button = color_button(UiSlot::get(slot, &theme.ui));
        let ctx = context.clone();
        button.connect_rgba_notify(move |button| {
            let hex = colors::to_hex(&button.rgba());
            let mut theme = ctx.theming.borrow().theme();
            if !theme.source.is_editable() {
                return;
            }
            UiSlot::set(slot, &mut theme.ui, hex);
            ctx.store.save_theme(&theme);
            ctx.theming.borrow_mut().reload_custom(&ctx.store);
            apply(&ctx);
        });
        row.add_suffix(&button);
        group.add(&row);
    }
    group
}

/// The theme's indicator palette. Editing these changes every overlay that
/// chose a colour by name rather than by hex.
fn palette_group(context: &Rc<Context>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::new();
    group.set_title("Palette");
    group.set_description(Some(
        "Offered first when picking a colour for an indicator, so they stay consistent.",
    ));

    let theme = context.theming.borrow().theme();
    for name in SWATCH_NAMES {
        let Some(swatch) = theme.swatch(name) else { continue };
        let row = adw::ActionRow::new();
        row.set_title(name);

        let button = color_button(&swatch.hex);
        let ctx = context.clone();
        let name = name.to_string();
        button.connect_rgba_notify(move |button| {
            let hex = colors::to_hex(&button.rgba());
            let mut theme = ctx.theming.borrow().theme();
            if !theme.source.is_editable() {
                return;
            }
            if let Some(swatch) = theme.swatches.iter_mut().find(|s| s.name == name) {
                swatch.hex = hex;
            }
            ctx.store.save_theme(&theme);
            ctx.theming.borrow_mut().reload_custom(&ctx.store);
            apply(&ctx);
        });
        row.add_suffix(&button);
        group.add(&row);
    }
    group
}

fn bar_colors_group(context: &Rc<Context>, group_name: &str) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::new();
    group.set_title(group_name);

    let scheme = context.theming.borrow().bar_scheme();
    for slot in BarSlot::ALL.into_iter().filter(|s| s.group() == group_name) {
        let row = adw::ActionRow::new();
        row.set_title(slot.label());
        if matches!(slot, BarSlot::UpFill | BarSlot::DownFill) {
            row.set_subtitle("Fully transparent draws a hollow candle.");
        }

        let button = color_button(BarSlot::get(slot, &scheme));
        button.set_dialog(&alpha_dialog());
        let ctx = context.clone();
        button.connect_rgba_notify(move |button| {
            let rgba = button.rgba();
            // Alpha is meaningful here: a transparent body is how hollow
            // candles are expressed, so keep it rather than flattening to
            // #rrggbb.
            let hex = format!(
                "#{:02x}{:02x}{:02x}{:02x}",
                (rgba.red() * 255.0).round() as u8,
                (rgba.green() * 255.0).round() as u8,
                (rgba.blue() * 255.0).round() as u8,
                (rgba.alpha() * 255.0).round() as u8,
            );
            let mut scheme = ctx.theming.borrow().bar_scheme();
            if !scheme.source.is_editable() {
                return;
            }
            BarSlot::set(slot, &mut scheme, hex);
            ctx.store.save_bar_scheme(&scheme);
            ctx.theming.borrow_mut().reload_custom(&ctx.store);
            apply(&ctx);
        });
        row.add_suffix(&button);
        group.add(&row);
    }
    group
}

fn build_data_page(context: &Rc<Context>) {
    let group = adw::PreferencesGroup::new();
    group.set_title("Market data");
    group.set_description(Some(
        "Bars are downloaded once and kept, so charts you have opened before \
         open instantly and without a request.",
    ));

    let provider = adw::ActionRow::new();
    provider.set_title("Provider");
    provider.set_subtitle("Yahoo Finance · delayed 10 min for futures, 15 for indexes");
    group.add(&provider);

    let cache = adw::ActionRow::new();
    cache.set_title("Cached data");
    let refresh_subtitle = {
        let store = context.store.clone();
        move |row: &adw::ActionRow| {
            row.set_subtitle(&format!(
                "{} · {}",
                describe_count(store.cached_series()),
                describe_bytes(store.cache_bytes())
            ));
        }
    };
    refresh_subtitle(&cache);

    let clear = gtk::Button::with_label("Clear");
    clear.add_css_class("destructive-action");
    clear.set_valign(gtk::Align::Center);
    let ctx = context.clone();
    let row = cache.clone();
    clear.connect_clicked(move |_| {
        let _ = ctx.store.clear_market_data();
        refresh_subtitle(&row);
        (ctx.on_change)();
    });
    cache.add_suffix(&clear);
    group.add(&cache);

    context.data_page.add(&group);
}

fn apply(context: &Rc<Context>) {
    context.theming.borrow_mut().apply();
    (context.on_change)();
}

fn color_button(hex: &str) -> gtk::ColorDialogButton {
    let button = gtk::ColorDialogButton::new(Some(gtk::ColorDialog::new()));
    button.set_rgba(&colors::parse(hex));
    button.set_valign(gtk::Align::Center);
    button.add_css_class("swatch-button");
    button
}

fn alpha_dialog() -> gtk::ColorDialog {
    let dialog = gtk::ColorDialog::new();
    dialog.set_with_alpha(true);
    dialog
}

fn string_list(items: &[String]) -> gtk::StringList {
    let list = gtk::StringList::new(&[]);
    for item in items {
        list.append(item);
    }
    list
}

/// "Midnight" -> ("midnight-copy", "Midnight copy"), avoiding ids in use.
fn unique_name(base: &str, taken: &[String]) -> (String, String) {
    let slug: String = base
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    for n in 1.. {
        let (id, name) = if n == 1 {
            (format!("{slug}-copy"), format!("{base} copy"))
        } else {
            (format!("{slug}-copy-{n}"), format!("{base} copy {n}"))
        };
        if !taken.contains(&id) {
            return (id, name);
        }
    }
    unreachable!()
}

fn describe_bytes(bytes: i64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    let bytes = bytes as f64;
    if bytes >= MB {
        format!("{:.1} MB", bytes / MB)
    } else if bytes >= 1024.0 {
        format!("{:.0} kB", bytes / 1024.0)
    } else {
        format!("{bytes:.0} bytes")
    }
}

fn describe_count(series: i64) -> String {
    match series {
        0 => "nothing cached yet".to_string(),
        1 => "1 series".to_string(),
        n => format!("{n} series"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_get_names_that_are_not_taken() {
        let (id, name) = unique_name("Midnight", &[]);
        assert_eq!((id.as_str(), name.as_str()), ("midnight-copy", "Midnight copy"));

        let (id, name) = unique_name("Midnight", &["midnight-copy".into()]);
        assert_eq!((id.as_str(), name.as_str()), ("midnight-copy-2", "Midnight copy 2"));
    }

    #[test]
    fn names_with_punctuation_still_make_valid_ids() {
        let (id, _) = unique_name("Blue / Orange", &[]);
        assert!(id.chars().all(|c| c.is_alphanumeric() || c == '-'), "{id}");
    }

    #[test]
    fn sizes_read_sensibly() {
        assert_eq!(describe_bytes(0), "0 bytes");
        assert_eq!(describe_bytes(2048), "2 kB");
        assert_eq!(describe_bytes(5 * 1024 * 1024), "5.0 MB");
    }

    #[test]
    fn counts_read_sensibly() {
        assert_eq!(describe_count(0), "nothing cached yet");
        assert_eq!(describe_count(1), "1 series");
        assert_eq!(describe_count(7), "7 series");
    }
}
