//! Render every shipped Omarchy theme's indicator palette, before and after,
//! onto one self-contained HTML page: `doc/themes/palettes.html`.
//!
//! The invariant tests say whether a palette is *acceptable*. This page is
//! for the other question, the one no threshold answers: does it look good?
//! Each theme is drawn as it appears on screen, twice: as the old derivation
//! produced it, by lifting the theme's terminal colours straight into the
//! named slots, and as the current one generates it. Beside each picture are
//! the numbers the picture is judged by, so a reader who disagrees with the
//! judgement can check it.
//!
//!     cargo run -p omacharts-engine --example palette_sheet
//!
//! writes the page in place. Pass a path to write it elsewhere.
//!
//! The old derivation is reproduced here rather than kept in the engine, so
//! the comparison stays honest after the engine has moved on: it is the exact
//! extraction-and-repair the app shipped with, including its hue rotation in
//! HSL, and its output was checked against the page the real code produced
//! before that code was removed.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use omacharts_engine::omarchy;
use omacharts_engine::theme::{
    contrast_ratio, delta_e, distance, ensure_distinct, rgb, theme_bars, Direction, Oklch, Swatch,
    Theme, SWATCH_NAMES, SWATCH_SEQUENCE,
};

/// What the generator guarantees for every automatic overlay. The page marks
/// anything below these; they are the same floors the tests hold.
const MIN_PAIR: f64 = 0.08;
const MIN_CONTRAST: f64 = 3.0;
const MIN_FROM_CANDLE: f64 = 0.08;
const MIN_FROM_FURNITURE: f64 = 0.06;

fn main() {
    let out = std::env::args().nth(1).map(PathBuf::from).unwrap_or_else(|| {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../doc/themes/palettes.html")
    });
    let themes = fixtures();
    let pairs: Vec<Comparison> = themes
        .iter()
        .map(|(name, keys, theme)| Comparison::of(name, keys, theme))
        .collect();

    let mut html = String::new();
    html.push_str(HEAD);
    intro(&mut html, pairs.len());
    summary(&mut html, &pairs);
    for pair in &pairs {
        section(&mut html, pair);
    }
    html.push_str(FOOT);
    std::fs::write(&out, html).expect("write sheet");
    eprintln!("wrote {}", out.display());
}

/// The shipped palettes, from the engine's own fixtures.
fn fixtures() -> Vec<(String, HashMap<String, String>, Theme)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/omarchy");
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("fixtures directory") {
        let path = entry.expect("entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        let text = std::fs::read_to_string(&path).expect("read fixture");
        let keys = omarchy::parse(&text);
        let theme = omarchy::derive(&keys, &name);
        out.push((name, keys, theme));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

// ---------------------------------------------------------------------------
// The old derivation, reproduced
// ---------------------------------------------------------------------------

/// The palette the app used to derive: each named slot took one terminal
/// colour, then three repairs ran in RGB. Faithful to the removed code, so the
/// "before" column is what users actually saw.
fn legacy_swatches(keys: &HashMap<String, String>, theme: &Theme) -> Vec<Swatch> {
    let pick = |names: &[&str], fallback: &str| -> String {
        names.iter().find_map(|n| keys.get(*n).cloned()).unwrap_or_else(|| fallback.to_string())
    };
    let accent = pick(&["accent", "blue", "cyan"], &theme.ui.text);
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

    let background = &theme.ui.background;
    let dark = rgb(background).map(|(r, g, b)| (r as u32 + g as u32 + b as u32) < 384) == Some(true);
    let lift = if dark { "#ffffff" } else { "#000000" };
    for swatch in swatches.iter_mut() {
        swatch.hex = ensure_distinct(&swatch.hex, background, 0.12, lift);
    }
    legacy_separate(&mut swatches, "Green", "Rose", 0.15);
    for pair in SWATCH_SEQUENCE.windows(2) {
        legacy_separate(&mut swatches, pair[0], pair[1], 0.10);
    }
    swatches
}

fn legacy_separate(swatches: &mut [Swatch], earlier: &str, later: &str, minimum: f64) {
    let Some(anchor) = swatches.iter().find(|s| s.name == earlier).map(|s| s.hex.clone()) else {
        return;
    };
    let Some(target) = swatches.iter_mut().find(|s| s.name == later) else { return };
    if distance(&target.hex, &anchor) >= minimum {
        return;
    }
    for degrees in [90.0, 150.0, 210.0, 45.0, 270.0, 120.0] {
        let rotated = rotate_hue_hsl(&target.hex, degrees);
        if distance(&rotated, &anchor) >= minimum {
            target.hex = rotated;
            return;
        }
    }
    let lightness = |hex: &str| {
        rgb(hex).map(|(r, g, b)| (r as f64 + g as f64 + b as f64) / 765.0).unwrap_or(0.5)
    };
    let toward = if lightness(&anchor) > 0.5 { "#000000" } else { "#ffffff" };
    target.hex = ensure_distinct(&target.hex, &anchor, minimum, toward);
}

fn rotate_hue_hsl(hex: &str, degrees: f64) -> String {
    let Some((r, g, b)) = rgb(hex) else { return hex.to_string() };
    let (r, g, b) = (r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let delta = max - min;
    let (h, s) = if delta.abs() < f64::EPSILON {
        (0.0, 0.0)
    } else {
        let s = delta / (1.0 - (2.0 * l - 1.0).abs()).max(f64::EPSILON);
        let h = if (max - r).abs() < f64::EPSILON {
            60.0 * (((g - b) / delta) % 6.0)
        } else if (max - g).abs() < f64::EPSILON {
            60.0 * ((b - r) / delta + 2.0)
        } else {
            60.0 * ((r - g) / delta + 4.0)
        };
        (h.rem_euclid(360.0), s.clamp(0.0, 1.0))
    };
    let h = (h + degrees).rem_euclid(360.0);
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match h as u32 {
        0..=59 => (c, x, 0.0),
        60..=119 => (x, c, 0.0),
        120..=179 => (0.0, c, x),
        180..=239 => (0.0, x, c),
        240..=299 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let byte = |v: f64| (((v + m) * 255.0).round().clamp(0.0, 255.0)) as u8;
    format!("#{:02x}{:02x}{:02x}", byte(r), byte(g), byte(b))
}

// ---------------------------------------------------------------------------
// Measurement
// ---------------------------------------------------------------------------

struct Comparison {
    name: String,
    theme: Theme,
    before: Palette,
    after: Palette,
}

impl Comparison {
    fn of(name: &str, keys: &HashMap<String, String>, theme: &Theme) -> Comparison {
        let legacy = Theme { swatches: legacy_swatches(keys, theme), ..theme.clone() };
        Comparison {
            name: name.to_string(),
            theme: theme.clone(),
            before: Palette::measure(&legacy),
            after: Palette::measure(theme),
        }
    }
}

/// One palette's six automatic overlays, with everything worth knowing about
/// each, and the candles they share the chart with.
struct Palette {
    swatches: Vec<Swatch>,
    lines: Vec<Line>,
    up: String,
    down: String,
}

struct Line {
    name: &'static str,
    hex: String,
    lch: Oklch,
    contrast: f64,
    /// The closest of the other five, and how close.
    nearest: (&'static str, f64),
    from_up: f64,
    from_down: f64,
    from_grid: f64,
    from_axis: f64,
    from_crosshair: f64,
}

impl Palette {
    fn measure(theme: &Theme) -> Palette {
        let bars = theme_bars(theme);
        let (up, down) = (bars.outline(Direction::Up), bars.outline(Direction::Down));
        let hexes: Vec<String> = (0..SWATCH_SEQUENCE.len()).map(|n| theme.series(n)).collect();
        let lines = hexes
            .iter()
            .enumerate()
            .map(|(n, hex)| {
                let nearest = hexes
                    .iter()
                    .enumerate()
                    .filter(|(m, _)| *m != n)
                    .map(|(m, other)| (SWATCH_SEQUENCE[m], delta_e(hex, other)))
                    .min_by(|a, b| a.1.total_cmp(&b.1))
                    .unwrap();
                Line {
                    name: SWATCH_SEQUENCE[n],
                    lch: Oklch::of(hex).unwrap(),
                    contrast: contrast_ratio(hex, &theme.ui.background),
                    nearest,
                    from_up: delta_e(hex, up),
                    from_down: delta_e(hex, down),
                    from_grid: delta_e(hex, &theme.ui.grid),
                    from_axis: delta_e(hex, &theme.ui.axis),
                    from_crosshair: delta_e(hex, &theme.ui.crosshair),
                    hex: hex.clone(),
                }
            })
            .collect();
        Palette { swatches: theme.swatches.clone(), lines, up: up.to_string(), down: down.to_string() }
    }

    fn min(&self, f: impl Fn(&Line) -> f64) -> f64 {
        self.lines.iter().map(f).fold(f64::MAX, f64::min)
    }

    /// How many of the guarantees this palette breaks, summed over lines.
    fn failures(&self) -> usize {
        self.lines
            .iter()
            .map(|l| {
                usize::from(l.nearest.1 < MIN_PAIR)
                    + usize::from(l.contrast < MIN_CONTRAST)
                    + usize::from(l.from_up < MIN_FROM_CANDLE || l.from_down < MIN_FROM_CANDLE)
                    + usize::from(
                        l.from_grid < MIN_FROM_FURNITURE
                            || l.from_axis < MIN_FROM_FURNITURE
                            || l.from_crosshair < MIN_FROM_FURNITURE,
                    )
            })
            .sum()
    }
}

// ---------------------------------------------------------------------------
// The page
// ---------------------------------------------------------------------------

fn intro(html: &mut String, count: usize) {
    let _ = write!(
        html,
        r#"<header>
<h1>Indicator palettes from Omarchy themes</h1>
<p class="lede">How omacharts turns a desktop theme into six colours you can tell apart on a chart, shown for all {count} themes Omarchy ships, before and after the derivation was rewritten.</p>
</header>
<section class="prose">
<h2>The problem</h2>
<p><a href="https://omarchy.org">Omarchy</a> is a Linux desktop that is themed as a whole: pick Nord or Gruvbox or Catppuccin and the terminal, the editor, the window borders and the bar all change together. Each theme is a small file of named colours, mostly the sixteen a terminal expects: red, green, yellow, blue, magenta, cyan and their bright variants, plus a background, a foreground and an accent.</p>
<p>omacharts draws market charts, and it follows that theme. The chart's background, grid, text and candles all come from the file, so the app looks like it belongs on the desktop it is running on. The hard part is the <em>indicator palette</em>: when you add a moving average, then another, then a band and an oscillator, each one needs a colour, and those colours have to do several things at once. Each must be visible as a line one or two pixels wide against the chart. Each must be distinguishable from every other, because overlays cross and run beside each other. None may look like the candles, whose green and red already mean <em>up</em> and <em>down</em>. And the six together should look like they were chosen by whoever designed the theme, not dropped on it from a box of crayons.</p>
<p>Six distinguishable colours on one background is harder than it sounds. A terminal palette was never designed for it: its colours are meant to be read as text, a few at a time, and plenty of themes do not even try to offer six hues. Lumon is six shades of one blue. Lupine's "yellow" is blue. Hackerman's "red" is green. Vantablack has no hue at all. And in thirteen of the {count} themes, <code>cyan</code> and <code>bright_cyan</code> are the same hex.</p>
<h2>Before and after</h2>
<p>The original derivation <strong>extracted</strong>: the Blue slot took the theme's <code>blue</code>, Amber took <code>yellow</code>, and so on, then nudged anything that collided. The rewrite <strong>generates</strong>. The theme still decides almost everything: how light the lines are (where its own colours sit, pulled into the range that can be seen on its background), how vivid they are (as vivid as its own colours, within a band), and, whenever it has a colour near enough to what a name means, the hue itself: Nord's Blue is Nord's blue. Only when the theme has nothing to offer for a name is a hue invented, and then it is the hue the name promises, at the theme's own lightness and chroma. All of it happens in <a href="https://bottosson.github.io/posts/oklab/">OKLCh</a>, a colour space built so that equal steps look equal, which is what makes "same lightness, different hue" mean what it says.</p>
<h2>How to read the measurements</h2>
<p>Two numbers appear throughout. <strong>ΔE</strong> is the distance between two colours in OKLab, on a scale where black to white is 1.0. About 0.02 is the smallest difference an eye can see; two thin lines closer than about 0.06 read as the same line in two places; the generator guarantees every pair of automatic overlays at least <b>{MIN_PAIR}</b>, and at least that from each candle colour, and <b>{MIN_FROM_FURNITURE}</b> from the grid, the axis and the crosshair. <strong>Contrast</strong> is the WCAG ratio against the chart background, where 3:1 is the accepted floor for graphics that carry meaning; every line is held to at least <b>{MIN_CONTRAST}:1</b>. <strong>L, C, H</strong> are a colour's OKLCh lightness (0 to 1), chroma (0 is grey; 0.09 is a quiet colour, 0.2 a vivid one) and hue in degrees.</p>
<p>In each theme below, the left panel is the old derivation and the right is the current one, drawn on the theme's real chart background with its grid, axis, crosshair and candles. Numbers below a guarantee are marked <span class="bad">like this</span>. The old derivation is shown in full, including the themes where it was already fine; the summary table is the honest version of the argument.</p>
</section>
"#
    );
}

fn summary(html: &mut String, pairs: &[Comparison]) {
    html.push_str(
        r#"<section class="prose wide"><h2>Summary</h2>
<p>For each theme: the closest two of the six automatic overlays, the lowest contrast of any of them against the chart, the closest any came to a candle colour, and how many guarantees were broken in total.</p>
<div class="scroll"><table class="summary">
<tr><th rowspan="2">Theme</th><th rowspan="2">Mode</th><th colspan="4">Before</th><th colspan="4">After</th></tr>
<tr><th>closest pair ΔE</th><th>min contrast</th><th>closest to candle ΔE</th><th>guarantees broken</th><th>closest pair ΔE</th><th>min contrast</th><th>closest to candle ΔE</th><th>guarantees broken</th></tr>
"#,
    );
    for pair in pairs {
        let cells = |p: &Palette| {
            let closest = p.min(|l| l.nearest.1);
            let contrast = p.min(|l| l.contrast);
            let candle = p.min(|l| l.from_up.min(l.from_down));
            format!(
                "{}{}{}<td>{}</td>",
                mark(closest, MIN_PAIR, 3),
                mark(contrast, MIN_CONTRAST, 2),
                mark(candle, MIN_FROM_CANDLE, 3),
                p.failures()
            )
        };
        let _ = writeln!(
            html,
            "<tr><td><a href=\"#{name}\">{name}</a></td><td>{}</td>{}{}</tr>",
            pair.theme.mode.label(),
            cells(&pair.before),
            cells(&pair.after),
            name = pair.name,
        );
    }
    html.push_str("</table></div></section>\n");
}

fn mark(value: f64, floor: f64, decimals: usize) -> String {
    let class = if value < floor { " class=\"bad\"" } else { "" };
    format!("<td{class}>{value:.decimals$}</td>")
}

fn section(html: &mut String, pair: &Comparison) {
    let _ = write!(
        html,
        r#"<section class="theme" id="{name}">
<h2>{name} <span class="mode">{mode}</span></h2>
<div class="compare">
"#,
        name = pair.name,
        mode = pair.theme.mode.label(),
    );
    panel(html, "Before", "extracted from the theme's terminal colours", &pair.theme, &pair.before);
    panel(html, "After", "generated from the theme", &pair.theme, &pair.after);
    html.push_str("</div></section>\n");
}

fn panel(html: &mut String, title: &str, subtitle: &str, theme: &Theme, palette: &Palette) {
    let ui = &theme.ui;
    let _ = write!(
        html,
        r#"<div class="panel">
<h3>{title} <span class="sub">{subtitle}</span></h3>
<p class="caption">Six overlays as 1.5px lines, with the theme's grid, axis, crosshair and candles.</p>
"#
    );
    sample(html, theme, palette);
    let _ = write!(
        html,
        r#"<p class="caption">The full named palette. Green and Rose are the candles and are never assigned automatically.</p><div class="chips" style="background:{};">"#,
        ui.background
    );
    for swatch in &palette.swatches {
        let lch = Oklch::of(&swatch.hex).unwrap();
        let _ = write!(
            html,
            r#"<div class="chip" style="color:{text}"><span style="background:{hex}"></span><div><b>{name}</b><code>{hex}</code><small>L {:.2} · C {:.3} · H {:.0}°</small></div></div>"#,
            lch.l,
            lch.c,
            lch.h,
            hex = swatch.hex,
            name = swatch.name,
            text = ui.text,
        );
    }
    html.push_str("</div>\n");
    html.push_str(
        r#"<p class="caption">Measurements for the six automatic overlays. "nearest" is the closest other overlay.</p>
<div class="scroll"><table class="metrics">
<tr><th>overlay</th><th>hex</th><th>L</th><th>C</th><th>H</th><th>contrast</th><th>nearest</th><th>ΔE</th><th>ΔE up</th><th>ΔE down</th><th>ΔE grid</th><th>ΔE axis</th><th>ΔE crosshair</th></tr>
"#,
    );
    for line in &palette.lines {
        let _ = writeln!(
            html,
            "<tr><td><i style=\"background:{hex}\"></i>{}</td><td><code>{hex}</code></td><td>{:.2}</td><td>{:.3}</td><td>{:.0}</td>{}<td>{}</td>{}{}{}{}{}{}</tr>",
            line.name,
            line.lch.l,
            line.lch.c,
            line.lch.h,
            mark(line.contrast, MIN_CONTRAST, 2),
            line.nearest.0,
            mark(line.nearest.1, MIN_PAIR, 3),
            mark(line.from_up, MIN_FROM_CANDLE, 3),
            mark(line.from_down, MIN_FROM_CANDLE, 3),
            mark(line.from_grid, MIN_FROM_FURNITURE, 3),
            mark(line.from_axis, MIN_FROM_FURNITURE, 3),
            mark(line.from_crosshair, MIN_FROM_FURNITURE, 3),
            hex = line.hex,
        );
    }
    html.push_str("</table></div></div>\n");
}

/// A miniature of the chart: grid, axis, crosshair, a run of candles, and the
/// six overlays as sine waves offset so they cross and run beside each other,
/// which is where lookalikes hide.
fn sample(html: &mut String, theme: &Theme, palette: &Palette) {
    let ui = &theme.ui;
    let (w, h) = (640.0, 200.0);
    let _ = write!(
        html,
        r#"<svg class="sample" viewBox="0 0 {w} {h}" style="background:{bg}" role="img" aria-label="Chart sample for {name}">"#,
        bg = ui.background,
        name = theme.name,
    );
    for y in (20..200).step_by(30) {
        let _ = write!(html, r#"<line x1="0" y1="{y}" x2="{w}" y2="{y}" stroke="{}"/>"#, ui.grid);
    }
    for x in (60..640).step_by(60) {
        let _ = write!(html, r#"<line x1="{x}" y1="0" x2="{x}" y2="{h}" stroke="{}"/>"#, ui.grid);
    }
    let _ = write!(html, r#"<line x1="0" y1="199" x2="{w}" y2="199" stroke="{}"/>"#, ui.axis);
    let _ = write!(
        html,
        r#"<line x1="560" y1="0" x2="560" y2="{h}" stroke="{}" stroke-dasharray="3 3"/>"#,
        ui.crosshair
    );
    let _ = write!(
        html,
        r#"<line x1="0" y1="110" x2="{w}" y2="110" stroke="{}" stroke-dasharray="3 3"/>"#,
        ui.crosshair
    );

    let candles: [(f64, f64, f64, bool); 6] = [
        (60.0, 120.0, 90.0, true),
        (40.0, 100.0, 70.0, false),
        (80.0, 150.0, 120.0, true),
        (90.0, 160.0, 130.0, false),
        (50.0, 140.0, 100.0, true),
        (30.0, 90.0, 60.0, false),
    ];
    for (i, (lo, hi, close, is_up)) in candles.iter().enumerate() {
        let x = 20.0 + i as f64 * 14.0;
        let colour = if *is_up { &palette.up } else { &palette.down };
        let (top, bottom) = if *is_up { (*close, *lo + 20.0) } else { (*lo + 20.0, *close) };
        let _ = write!(
            html,
            r#"<line x1="{x}" y1="{}" x2="{x}" y2="{}" stroke="{colour}"/>"#,
            h - hi,
            h - lo
        );
        let _ = write!(
            html,
            r#"<rect x="{}" y="{}" width="8" height="{}" fill="{colour}"/>"#,
            x - 4.0,
            h - top.max(bottom),
            (top - bottom).abs().max(1.0)
        );
    }
    for (n, line) in palette.lines.iter().enumerate() {
        let mut points = String::new();
        for step in 0..=50 {
            let x = 120.0 + step as f64 * 9.0;
            let phase = step as f64 * 0.21 + n as f64 * 0.9;
            let y = 100.0 + 20.0 * phase.sin() + (n as f64 - 2.5) * 22.0;
            let _ = write!(points, "{x:.1},{y:.1} ");
        }
        let _ = write!(
            html,
            r#"<polyline points="{points}" fill="none" stroke="{}" stroke-width="1.5"/>"#,
            line.hex
        );
        let _ = write!(
            html,
            r#"<text x="632" y="{}" fill="{}" font-size="10" text-anchor="end">{}</text>"#,
            92.0 + (n as f64 - 2.5) * 22.0,
            line.hex,
            line.name
        );
    }
    html.push_str("</svg>\n");
}

const HEAD: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Indicator palettes from Omarchy themes</title>
<style>
:root { --bg: #141518; --panel: #1b1c21; --text: #d4d4d6; --muted: #8e8f95; --rule: #2a2b31; --bad: #c47a7a; }
* { box-sizing: border-box; }
body { margin: 0; background: var(--bg); color: var(--text); font: 15px/1.6 -apple-system, "Inter", "Segoe UI", system-ui, sans-serif; }
header, .prose { max-width: 760px; margin: 0 auto; padding: 0 24px; }
.prose.wide { max-width: 1180px; }
.prose.wide p { max-width: 712px; }
header { padding-top: 72px; }
h1 { font-size: 34px; line-height: 1.15; font-weight: 600; letter-spacing: -0.01em; margin: 0 0 16px; }
.lede { font-size: 19px; color: var(--muted); margin: 0 0 48px; }
.prose h2 { font-size: 22px; font-weight: 600; margin: 40px 0 12px; }
.prose p { margin: 0 0 16px; }
.prose a { color: var(--text); text-decoration: underline; text-decoration-color: var(--muted); }
code { font-family: ui-monospace, "JetBrains Mono", Menlo, monospace; font-size: 0.92em; }
.bad { color: var(--bad); }
.scroll { overflow-x: auto; }
table { border-collapse: collapse; font-size: 13px; width: 100%; }
th, td { text-align: right; padding: 5px 10px; border-bottom: 1px solid var(--rule); white-space: nowrap; }
th { color: var(--muted); font-weight: 500; }
td:first-child, th:first-child { text-align: left; }
.summary { margin-top: 8px; }
.summary th[colspan] { text-align: center; border-bottom: none; padding-top: 12px; }
.summary td:nth-child(6), .summary th:nth-child(6), .summary tr:first-child th:nth-child(3) { border-right: 1px solid var(--rule); }
.summary a { color: var(--text); text-decoration: none; }
.theme { max-width: 1480px; margin: 72px auto 0; padding: 0 24px; }
.theme h2 { font-size: 24px; font-weight: 600; margin: 0 0 20px; padding-top: 24px; border-top: 1px solid var(--rule); }
.mode { color: var(--muted); font-weight: 400; font-size: 16px; margin-left: 8px; }
.compare { display: grid; grid-template-columns: repeat(auto-fit, minmax(560px, 1fr)); gap: 24px; }
.panel { background: var(--panel); border-radius: 8px; padding: 18px 20px 20px; }
.panel h3 { margin: 0 0 14px; font-size: 16px; font-weight: 600; }
.sub { color: var(--muted); font-weight: 400; }
.caption { color: var(--muted); font-size: 13px; margin: 14px 0 6px; }
.sample { width: 100%; height: auto; display: block; border-radius: 6px; }
.sample line { stroke-width: 1; }
.chips { display: grid; grid-template-columns: repeat(4, 1fr); gap: 10px 14px; padding: 12px 14px; border-radius: 6px; }
.chip { display: flex; gap: 8px; align-items: center; font-size: 12px; line-height: 1.3; }
.chip span { width: 26px; height: 26px; border-radius: 5px; flex: none; }
.chip div { display: flex; flex-direction: column; }
.chip code { opacity: .8; }
.chip small { opacity: .6; font-size: 10px; }
.metrics td i { display: inline-block; width: 10px; height: 10px; border-radius: 2px; margin-right: 7px; vertical-align: -1px; }
footer { max-width: 760px; margin: 96px auto 72px; padding: 0 24px; color: var(--muted); font-size: 13px; }
@media (max-width: 640px) { .compare { grid-template-columns: 1fr; } .chips { grid-template-columns: repeat(2, 1fr); } header { padding-top: 40px; } h1 { font-size: 28px; } }
</style>
</head>
<body>
"#;

const FOOT: &str = r#"<footer>
<p>Generated by <code>crates/omacharts-engine/examples/palette_sheet.rs</code> from the theme files in <code>crates/omacharts-engine/tests/fixtures/omarchy/</code>. To regenerate after a change to the derivation or a new theme: <code>cargo run -p omacharts-engine --example palette_sheet</code>. The "before" column is a faithful reproduction of the derivation this replaced; the "after" column is whatever the engine does now.</p>
</footer>
</body>
</html>
"#;
