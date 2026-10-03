//! Every theme Omarchy ships must produce a chart you can read.
//!
//! The Omarchy theme is derived at runtime from the desktop's own
//! `colors.toml`, so it works with themes that do not exist yet. The risk in
//! that is a palette we never looked at deriving something illegible — a grid
//! indistinguishable from the background, two overlay colours nobody can tell
//! apart.
//!
//! So every shipped palette is a fixture here, and the invariants are the same
//! ones our own themes are held to. A new Omarchy release adding a theme means
//! dropping its `colors.toml` in beside these.

use std::collections::HashMap;
use std::path::Path;

use omacharts_engine::theme::{theme_bars, Direction, SWATCH_SEQUENCE};
use omacharts_engine::{omarchy, Theme};

fn fixtures() -> Vec<(String, Theme)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/omarchy");
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("fixtures directory") {
        let path = entry.expect("entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        let text = std::fs::read_to_string(&path).expect("read fixture");
        let keys: HashMap<String, String> = omarchy::parse(&text);
        out.push((name.clone(), omarchy::derive(&keys, &name)));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    assert!(out.len() >= 20, "expected the shipped themes, found {}", out.len());
    out
}

/// Euclidean distance in RGB, normalised to 0..1.
fn distance(a: &str, b: &str) -> f64 {
    let parse = |hex: &str| -> (f64, f64, f64) {
        let h = hex.trim_start_matches('#');
        let byte = |i: usize| {
            u8::from_str_radix(h.get(i..i + 2).unwrap_or("00"), 16).unwrap_or(0) as f64 / 255.0
        };
        (byte(0), byte(2), byte(4))
    };
    let (ar, ag, ab) = parse(a);
    let (br, bg, bb) = parse(b);
    (((ar - br).powi(2) + (ag - bg).powi(2) + (ab - bb).powi(2)) / 3.0).sqrt()
}

#[test]
fn every_shipped_theme_derives_a_complete_palette() {
    for (name, theme) in fixtures() {
        assert!(!theme.ui.background.is_empty(), "{name}: no background");
        assert!(!theme.ui.text.is_empty(), "{name}: no text");
        assert_eq!(theme.swatches.len(), 8, "{name}: incomplete palette");
        for swatch in &theme.swatches {
            assert!(
                swatch.hex.starts_with('#') && swatch.hex.len() >= 7,
                "{name}/{}: {:?} is not a colour",
                swatch.name,
                swatch.hex
            );
        }
    }
}

#[test]
fn text_is_readable_on_the_background_in_every_theme() {
    for (name, theme) in fixtures() {
        let d = distance(&theme.ui.text, &theme.ui.background);
        assert!(d > 0.25, "{name}: text {} on {} ({d:.3})", theme.ui.text, theme.ui.background);
    }
}

#[test]
fn the_grid_is_visible_but_not_loud_in_every_theme() {
    for (name, theme) in fixtures() {
        let from_background = distance(&theme.ui.grid, &theme.ui.background);
        let text_contrast = distance(&theme.ui.text, &theme.ui.background);
        assert!(
            from_background > 0.004,
            "{name}: grid {} is invisible on {} ({from_background:.4})",
            theme.ui.grid,
            theme.ui.background
        );
        assert!(
            from_background < text_contrast,
            "{name}: grid competes with the text ({from_background:.3} vs {text_contrast:.3})",
        );
    }
}

#[test]
fn up_and_down_are_distinguishable_in_every_theme() {
    for (name, theme) in fixtures() {
        let bars = theme_bars(&theme);
        let up = bars.outline(Direction::Up);
        let down = bars.outline(Direction::Down);
        let d = distance(up, down);
        assert!(d > 0.1, "{name}: up {up} and down {down} are too close ({d:.3})");

        // And both must be visible against the chart.
        for (label, colour) in [("up", up), ("down", down)] {
            let against = distance(colour, &theme.ui.background);
            assert!(
                against > 0.1,
                "{name}: {label} {colour} vanishes into {} ({against:.3})",
                theme.ui.background
            );
        }
    }
}

#[test]
fn overlay_colours_are_legible_and_separable_in_every_theme() {
    for (name, theme) in fixtures() {
        for slot in 0..SWATCH_SEQUENCE.len() {
            let colour = theme.series(slot);
            let against = distance(&colour, &theme.ui.background);
            assert!(
                against > 0.1,
                "{name}: overlay {slot} ({colour}) vanishes into {} ({against:.3})",
                theme.ui.background
            );
        }
        for slot in 0..SWATCH_SEQUENCE.len() - 1 {
            let (a, b) = (theme.series(slot), theme.series(slot + 1));
            let d = distance(&a, &b);
            assert!(d > 0.08, "{name}: overlays {slot} and {} are too close: {a} vs {b} ({d:.3})", slot + 1);
        }
    }
}

#[test]
fn fills_sit_between_their_line_and_the_background_in_every_theme() {
    for (name, theme) in fixtures() {
        let line = theme.series(0);
        let fill = theme.fill_for(&line);
        let to_background = distance(&fill, &theme.ui.background);
        let line_to_background = distance(&line, &theme.ui.background);
        assert!(
            to_background < line_to_background,
            "{name}: the fill is not quieter than its line"
        );
        assert!(to_background > 0.002, "{name}: the fill is invisible");
    }
}

#[test]
fn light_themes_are_detected_as_light() {
    let themes = fixtures();
    let light: Vec<&String> = themes
        .iter()
        .filter(|(_, t)| t.mode == omacharts_engine::Mode::Light)
        .map(|(name, _)| name)
        .collect();
    // The shipped light themes, by name. If this list changes, the detection
    // is what to check first.
    for expected in ["catppuccin-latte", "flexoki-light", "white"] {
        assert!(
            light.iter().any(|n| n.as_str() == expected),
            "{expected} should be detected as light; got {light:?}"
        );
    }
    assert!(light.len() < themes.len(), "not every theme is light");
}
