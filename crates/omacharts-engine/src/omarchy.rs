//! The theme that follows the desktop.
//!
//! Omarchy writes a flat semantic palette to
//! `~/.local/state/omarchy/current/theme/colors.toml` and the theme's name to
//! `theme.name` beside it, rewriting both whenever the desktop theme changes.
//! We read those two files and map them onto a [`Theme`] — surfaces, text,
//! chart structure and the full indicator palette, since an Omarchy theme
//! already ships sixteen colours chosen to sit together.
//!
//! Polling beats inotify here: Omarchy swaps a symlinked directory, which is
//! exactly the case where watching a path either misses the change or needs
//! re-arming. Two `stat` calls on a timer cost nothing and never miss.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::theme::{
    distance, ensure_distinct, mix, rotate_hue, Mode, Source, Swatch, Theme, UiColors,
    OMARCHY_ID, SWATCH_NAMES, SWATCH_SEQUENCE,
};

const COLORS: &str = ".local/state/omarchy/current/theme/colors.toml";
const NAME: &str = ".local/state/omarchy/current/theme.name";

pub fn colors_path(home: &Path) -> PathBuf {
    home.join(COLORS)
}

pub fn name_path(home: &Path) -> PathBuf {
    home.join(NAME)
}

/// Is Omarchy present on this machine at all? Decides whether the Omarchy
/// theme is offered, and whether it is the default.
pub fn available(home: &Path) -> bool {
    colors_path(home).is_file()
}

/// A cheap value that changes whenever the desktop theme does, so the app can
/// poll on a timer and only rebuild when something actually moved.
pub fn fingerprint(home: &Path) -> Option<String> {
    let name = std::fs::read_to_string(name_path(home)).ok()?;
    let colors = std::fs::read_to_string(colors_path(home)).ok()?;
    Some(format!("{}:{:x}", name.trim(), hash(&colors)))
}

/// FNV-1a. Not cryptographic — it only has to change when the file does.
fn hash(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

/// Read the live Omarchy palette and map it onto a theme.
pub fn current(home: &Path) -> Option<Theme> {
    let text = std::fs::read_to_string(colors_path(home)).ok()?;
    let name = std::fs::read_to_string(name_path(home))
        .ok()
        .map(|n| pretty_name(n.trim()))
        .unwrap_or_else(|| "Omarchy".to_string());
    Some(derive(&parse(&text), &name))
}

/// `key = "value"`, one per line; comments and blanks ignored.
///
/// The file is flat, so a short parser beats a TOML dependency.
pub fn parse(text: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim().trim_matches('"').trim_matches('\'').trim();
        if value.is_empty() {
            continue;
        }
        out.insert(key.trim().to_string(), value.to_string());
    }
    out
}

/// "everforest" -> "Everforest", "tokyo-night" -> "Tokyo Night".
fn pretty_name(raw: &str) -> String {
    let words: Vec<String> = raw
        .split(['-', '_', ' '])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect();
    if words.is_empty() {
        "Omarchy".to_string()
    } else {
        words.join(" ")
    }
}

/// How far off the background a grid line has to sit to be a grid line.
const MIN_GRID: f64 = 0.015;
/// How far a panel has to sit off the chart to read as a panel.
const MIN_SURFACE: f64 = 0.012;
/// How far apart up and down have to be to mean opposite things.
const MIN_DIRECTION: f64 = 0.15;
/// How far an overlay colour has to be from the chart to be seen on it.
const MIN_OVERLAY: f64 = 0.12;
/// How far consecutive overlay colours have to be from each other.
const MIN_BETWEEN_OVERLAYS: f64 = 0.10;

/// Make a derived palette usable, whatever the theme handed us.
///
/// Omarchy themes are designed for terminals, not charts, and a few of the
/// shipped ones break assumptions a chart depends on. Hackerman is green on
/// green, so its "red" and "green" are the same colour and up would look like
/// down. Lumon's blue and yellow are both blue, so two overlays would be
/// indistinguishable. Rather than hand-writing a palette for each theme — which
/// would stop working the moment anyone installs a new one — the derivation
/// repairs what it is given: nudged for contrast, hue-rotated for separation,
/// and otherwise left exactly as the theme intended.
fn repair_palette(swatches: &mut [Swatch], background: &str) {
    // Every colour has to be visible on the chart at all.
    for swatch in swatches.iter_mut() {
        let lift = if crate::theme::rgb(background).map(|(r, g, b)| {
            (r as u32 + g as u32 + b as u32) < 384
        }) == Some(true)
        {
            "#ffffff"
        } else {
            "#000000"
        };
        swatch.hex = ensure_distinct(&swatch.hex, background, MIN_OVERLAY, lift);
    }

    // Up and down must not be the same colour, or the chart lies.
    separate(swatches, "Green", "Rose", MIN_DIRECTION);

    // Consecutive overlays are the ones that end up on top of each other.
    for pair in SWATCH_SEQUENCE.windows(2) {
        separate(swatches, pair[0], pair[1], MIN_BETWEEN_OVERLAYS);
    }
}

/// Rotate `later` away from `earlier` until they can be told apart.
fn separate(swatches: &mut [Swatch], earlier: &str, later: &str, minimum: f64) {
    let Some(anchor) = swatches.iter().find(|s| s.name == earlier).map(|s| s.hex.clone()) else {
        return;
    };
    let Some(target) = swatches.iter_mut().find(|s| s.name == later) else {
        return;
    };
    if distance(&target.hex, &anchor) >= minimum {
        return;
    }
    // Quarter turns first: a big move is more likely to land somewhere the
    // palette does not already occupy.
    for degrees in [90.0, 150.0, 210.0, 45.0, 270.0, 120.0] {
        let rotated = rotate_hue(&target.hex, degrees);
        if distance(&rotated, &anchor) >= minimum {
            target.hex = rotated;
            return;
        }
    }
    // A greyscale theme has no hue to rotate, so separate by lightness
    // instead — away from the anchor, toward whichever end has more room.
    let lightness = |hex: &str| {
        crate::theme::rgb(hex)
            .map(|(r, g, b)| (r as f64 + g as f64 + b as f64) / 765.0)
            .unwrap_or(0.5)
    };
    let toward = if lightness(&anchor) > 0.5 { "#000000" } else { "#ffffff" };
    target.hex = ensure_distinct(&target.hex, &anchor, minimum, toward);
}

/// Map the semantic keys onto our theme.
///
/// Omarchy guarantees only a core set of keys, so every lookup carries a
/// fallback chain ending in a literal. A theme that omits
/// `lighter_background` still produces a readable chart rather than a void.
pub fn derive(keys: &HashMap<String, String>, name: &str) -> Theme {
    let mode = match keys.get("mode").or_else(|| keys.get("theme_type")) {
        Some(m) if m.eq_ignore_ascii_case("light") => Mode::Light,
        _ => Mode::Dark,
    };
    let dark = mode == Mode::Dark;

    let pick = |names: &[&str], fallback: &str| -> String {
        for n in names {
            if let Some(v) = keys.get(*n) {
                return v.clone();
            }
        }
        fallback.to_string()
    };

    let background = pick(&["background"], if dark { "#101010" } else { "#ffffff" });
    let text = pick(&["foreground"], if dark { "#e6e6e6" } else { "#1f1f1f" });
    let muted = pick(&["muted", "selection"], &text);
    let accent = pick(&["accent", "blue", "cyan"], &text);

    // Secondary text has to read as secondary. Some themes put their
    // "light_foreground" further from the background than the foreground
    // itself, which inverts the hierarchy; derive it instead when that
    // happens.
    let text_muted = {
        let picked = pick(&["light_foreground", "dark_foreground"], &muted);
        let contrast = |colour: &str| distance(colour, &background);
        if contrast(&picked) >= contrast(&text) || contrast(&picked) < 0.12 {
            mix(&text, &background, 0.42)
        } else {
            picked
        }
    };

    // A panel the same colour as the chart is not a panel.
    let surface = ensure_distinct(
        &pick(&["lighter_background", "dark_background"], &background),
        &background,
        MIN_SURFACE,
        &text,
    );

    // Omarchy's sixteen colours are already chosen to sit together, which is
    // exactly what an indicator palette needs.
    let hexes = [
        pick(&["blue", "bright_blue"], &accent),
        pick(&["yellow", "bright_yellow"], "#d29922"),
        pick(&["magenta", "purple", "bright_magenta"], "#bc8cff"),
        pick(&["cyan", "bright_cyan"], "#39c5cf"),
        pick(&["red", "bright_red"], "#f85149"),
        pick(&["green", "bright_green"], "#3fb950"),
        pick(&["orange", "brown"], "#ff9b50"),
        pick(&["bright_cyan", "cyan"], "#76e4f7"),
    ];
    let mut swatches: Vec<Swatch> = SWATCH_NAMES
        .iter()
        .zip(hexes)
        .map(|(name, hex)| Swatch { name: name.to_string(), hex })
        .collect();
    repair_palette(&mut swatches, &background);

    Theme {
        id: OMARCHY_ID.to_string(),
        name: name.to_string(),
        mode,
        source: Source::Omarchy,
        ui: UiColors {
            surface_variant: ensure_distinct(
                &pick(&["selection", "lighter_background"], &surface),
                &background,
                MIN_SURFACE,
                &text,
            ),
            surface,
            border: muted.clone(),
            text_muted,
            // The grid must sit just off the background. `lighter_background`
            // is where Omarchy themes usually put it, but not every theme has
            // the key and some set it to the background itself, so the result
            // is nudged until it is actually visible.
            grid: ensure_distinct(
                &pick(&["lighter_background", "selection"], &background),
                &background,
                MIN_GRID,
                &text,
            ),
            axis: muted,
            crosshair: accent.clone(),
            background,
            text,
            accent,
        },
        swatches,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::theme_bars;

    const EVERFOREST: &str = r##"
mode = "dark"
accent = "#7fbbb3"
selection = "#3d484d"
muted = "#475258"
background = "#2d353b"
dark_background = "#21272c"
lighter_background = "#343f44"
foreground = "#d3c6aa"
light_foreground = "#9da9a0"
red = "#e67e80"
yellow = "#dbbc7f"
orange = "#e09d7f"
green = "#a7c080"
cyan = "#83c092"
blue = "#7fbbb3"
magenta = "#d699b6"
"##;

    #[test]
    fn parses_the_real_file_shape() {
        let keys = parse(EVERFOREST);
        assert_eq!(keys.get("accent").unwrap(), "#7fbbb3");
        assert_eq!(keys.get("mode").unwrap(), "dark");
        assert_eq!(keys.len(), 16);
    }

    #[test]
    fn derives_a_complete_theme() {
        let theme = derive(&parse(EVERFOREST), "Everforest");
        assert_eq!(theme.name, "Everforest");
        assert_eq!(theme.mode, Mode::Dark);
        assert_eq!(theme.source, Source::Omarchy);
        assert_eq!(theme.ui.background, "#2d353b");
        assert_eq!(theme.ui.text, "#d3c6aa");
        assert_eq!(theme.ui.grid, "#343f44");
        assert_eq!(theme.ui.crosshair, "#7fbbb3");
        for name in SWATCH_NAMES {
            assert!(theme.swatch(name).is_some(), "missing {name}");
        }
        assert_eq!(theme.swatch("Green").unwrap().hex, "#a7c080");
        assert_eq!(theme.swatch("Rose").unwrap().hex, "#e67e80");
    }

    #[test]
    fn candles_follow_the_desktop() {
        let theme = derive(&parse(EVERFOREST), "Everforest");
        let bars = theme_bars(&theme);
        assert_eq!(bars.up, "#a7c080");
        assert_eq!(bars.down, "#e67e80");
    }

    #[test]
    fn a_sparse_theme_still_yields_every_colour() {
        let theme = derive(&parse("mode = \"light\"\nbackground = \"#ffffff\"\n"), "Bare");
        assert_eq!(theme.mode, Mode::Light);
        assert_eq!(theme.ui.background, "#ffffff");
        assert!(!theme.ui.text.is_empty());
        assert!(!theme.ui.grid.is_empty());
        assert_eq!(theme.swatches.len(), SWATCH_NAMES.len());
        assert!(theme.swatches.iter().all(|s| !s.hex.is_empty()));
    }

    #[test]
    fn light_mode_is_detected() {
        assert_eq!(derive(&parse("mode = \"light\""), "x").mode, Mode::Light);
        assert_eq!(derive(&parse("theme_type = \"light\""), "x").mode, Mode::Light);
        assert_eq!(derive(&parse(""), "x").mode, Mode::Dark);
    }

    #[test]
    fn names_are_prettified() {
        assert_eq!(pretty_name("tokyo-night"), "Tokyo Night");
        assert_eq!(pretty_name("everforest"), "Everforest");
        assert_eq!(pretty_name(""), "Omarchy");
    }

    #[test]
    fn the_fingerprint_moves_with_the_content() {
        assert_ne!(hash("a"), hash("b"));
        assert_eq!(hash(EVERFOREST), hash(EVERFOREST));
    }
}
