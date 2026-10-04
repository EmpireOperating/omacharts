//! What the keys do, in one table.
//!
//! A shortcut that only exists inside a key handler is a shortcut
//! nobody finds. The menus and the tooltips should say so themselves,
//! and they should say it in the desktop's own words — "Ctrl+K" here,
//! something else on a machine set up differently — which is what
//! `gtk::accelerator_get_label` is for.
//!
//! So the table is the one place a binding is written down. The
//! application registers what it can from it, the menu items carry it
//! as the `accel` attribute GTK draws on the right of a row, the
//! tooltips append it, and the shortcuts window reads it rather than
//! repeating it.

use adw::prelude::*;
use gtk::gio;

/// One key, and what it does.
pub struct Binding {
    /// The action it activates, group and all: "win.find".
    pub action: &'static str,
    /// In GTK's accelerator syntax, most important first — the first
    /// is what menus and tooltips show.
    pub accels: &'static [&'static str],
    /// Whether the application may handle it.
    ///
    /// Registering an accelerator puts it above the focused widget,
    /// which is right for Ctrl+K and wrong for Ctrl+V: the window
    /// would split while somebody was pasting a symbol into a box. The
    /// ones marked false are handled by a controller that checks what
    /// has the keyboard first, and appear here only so that they are
    /// still written down where people look.
    pub global: bool,
}

const fn global(action: &'static str, accels: &'static [&'static str]) -> Binding {
    Binding { action, accels, global: true }
}

const fn careful(action: &'static str, accels: &'static [&'static str]) -> Binding {
    Binding { action, accels, global: false }
}

pub const BINDINGS: &[Binding] = &[
    // Nothing here can be typed into a box, so the application can own
    // them outright.
    global("win.find", &["<Ctrl>k", "<Ctrl>f"]),
    global("win.watchlist", &["<Ctrl>b"]),
    global("win.preferences", &["<Ctrl>comma"]),
    global("chart.indicators", &["<Ctrl>i"]),
    // A letter, because Shift and punctuation together is a trap. GTK
    // matches the keyval the key produces *after* modifiers, and shifting
    // the comma key does not produce a comma: it gives "<" on a US layout
    // and ";" on a Spanish one, so `<Ctrl><Shift>comma` matched on neither
    // and chart settings had no key at all. Naming the shifted keyval
    // instead only moves which layouts it is broken on. Punctuation can be
    // bound here, but only bare, the way `question` below is.
    global("chart.settings", &["<Ctrl><Shift>s"]),
    // O, not P: printing a chart is a thing this will grow, and the key
    // everything else on the desktop prints with has to still be free when it
    // does. A screenshot of the chartbook is the same key with Shift, the way
    // closing one already is.
    global("chart.screenshot", &["<Ctrl>o"]),
    global("win.screenshot", &["<Ctrl><Shift>o"]),
    global("chart.reset-view", &["<Alt>r"]),
    global("chart.split-h", &["<Ctrl>h"]),
    global("chart.maximize", &["<Ctrl>m"]),
    global("win.new-chartbook", &["<Ctrl>n"]),
    global("win.rename-chartbook", &["<Ctrl><Shift>r"]),
    // Ctrl+X closes a chart, so the chartbook holding it is the same
    // key with Shift. Safe to own outright: Shift+X is nobody's cut.
    global("win.close-chartbook", &["<Ctrl><Shift>x"]),
    // Paste and cut. The keys are the keys; what changes is whether
    // the keyboard is in something you can type into.
    careful("chart.split-v", &["<Ctrl>v"]),
    careful("chart.close", &["<Ctrl>x"]),
    // A bare key, which an entry has to see first.
    careful("win.shortcuts", &["question"]),
];

/// Hand the application everything it is safe to own.
pub fn install(app: &adw::Application) {
    for binding in BINDINGS.iter().filter(|binding| binding.global) {
        app.set_accels_for_action(binding.action, binding.accels);
    }
}

fn binding(action: &str) -> Option<&'static Binding> {
    BINDINGS.iter().find(|binding| binding.action == action)
}

/// The first accelerator for an action, in GTK's syntax.
pub fn accel(action: &str) -> Option<&'static str> {
    binding(action).and_then(|binding| binding.accels.first().copied())
}

/// The same, written the way this desktop writes it: "Ctrl+K".
pub fn label(action: &str) -> Option<String> {
    let (key, mods) = gtk::accelerator_parse(accel(action)?)?;
    let label = gtk::accelerator_get_label(key, mods);
    (!label.is_empty()).then(|| label.to_string())
}

/// A menu row that says what it does and what key does it.
///
/// The `accel` attribute is what a GTK popover menu draws down the
/// right-hand side, and it draws it whether or not the application
/// registered the key — which is the only way to show a shortcut that
/// has to be handled more carefully than an accelerator can be.
pub fn item(label: &str, action: &str) -> gio::MenuItem {
    let item = gio::MenuItem::new(Some(label), Some(action));
    if let Some(accel) = accel(action) {
        item.set_attribute_value("accel", Some(&accel.to_variant()));
    }
    item
}

/// Add one to a menu.
pub fn append(menu: &gio::Menu, label: &str, action: &str) {
    menu.append_item(&item(label, action));
}

/// "Watchlist (Ctrl+B)", for a button that has no room to say more.
pub fn tooltip(text: &str, action: &str) -> String {
    match label(action) {
        Some(key) => format!("{text} ({key})"),
        None => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_binding_is_an_accelerator_gtk_understands() {
        // Parsing one is GTK's job and GTK needs a display to say so.
        // Where there is none this checks the rest, which is still the
        // part that goes wrong.
        let parses = crate::ui::gtk_ready();

        // A typo here is a shortcut that silently does nothing, and a
        // menu row that confidently advertises it.
        for binding in BINDINGS {
            for accel in binding.accels {
                assert!(
                    !parses || gtk::accelerator_parse(*accel).is_some(),
                    "{} cannot be parsed, for {}",
                    accel,
                    binding.action
                );
            }
            assert!(!binding.accels.is_empty(), "{} has no key", binding.action);
            assert!(binding.action.contains('.'), "{} has no action group", binding.action);
        }
    }

    #[test]
    fn nothing_claims_a_key_twice() {
        let mut seen: Vec<&str> = Vec::new();
        for binding in BINDINGS {
            for accel in binding.accels {
                assert!(!seen.contains(accel), "{accel} is claimed twice");
                seen.push(accel);
            }
        }
    }

    /// Screenshots went on Ctrl+O so that Ctrl+P stays what it is everywhere
    /// else. Taking it later for anything but printing would be taking it from
    /// printing, which is the one thing it was kept for.
    #[test]
    fn the_printing_keys_are_left_alone() {
        for accel in ["<Ctrl>p", "<Ctrl><Shift>p"] {
            assert!(
                !BINDINGS.iter().any(|binding| binding.accels.contains(&accel)),
                "{accel} is reserved for printing"
            );
        }
    }

    #[test]
    fn what_is_typed_into_a_box_is_never_taken_by_the_application() {
        // The three that would break pasting, cutting, or typing a
        // question mark. If one of these is ever marked global, the
        // bug it causes is somebody else's afternoon.
        for action in ["chart.split-v", "chart.close", "win.shortcuts"] {
            let binding = binding(action).expect(action);
            assert!(!binding.global, "{action} must stay out of the accelerator table");
        }
    }
}
