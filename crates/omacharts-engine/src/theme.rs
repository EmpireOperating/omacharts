//! Themes, bar schemes, and the swatch palettes they carry.
//!
//! Three things are chosen independently, because people want them
//! independently:
//!
//! * a **[`Theme`]** — the whole app's look: surfaces, text, chart structure,
//!   accent. On Omarchy this follows the desktop by default.
//! * a **[`BarScheme`]** — what candles look like. Classic green and red is
//!   one option among several; monochrome is another.
//! * a **[`Swatch`]** from the active theme's palette, whenever a colour is
//!   picked for an indicator or overlay.
//!
//! Every theme ships a named palette so picking an indicator colour means
//! choosing from a handful that already look right together, rather than
//! hunting in a colour wheel. Picking from the wheel stays possible — see
//! [`ColorChoice`].

use serde::{Deserialize, Serialize};

/// Which way the platform stylesheet should lean.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    Dark,
    Light,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Mode::Dark => "Dark",
            Mode::Light => "Light",
        }
    }
}

/// Where a theme or bar scheme came from. Built-ins and the Omarchy theme can
/// be duplicated but not edited in place; custom ones can be edited and
/// deleted.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    BuiltIn,
    /// Derived from the live Omarchy palette; follows it as it changes.
    Omarchy,
    Custom,
}

impl Source {
    pub fn is_editable(self) -> bool {
        matches!(self, Source::Custom)
    }
}

// ---------------------------------------------------------------------------
// Theme
// ---------------------------------------------------------------------------

/// The app's own colours: everything that is not a candle.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct UiColors {
    pub background: String,
    pub surface: String,
    pub surface_variant: String,
    pub border: String,
    pub text: String,
    pub text_muted: String,
    pub grid: String,
    pub axis: String,
    pub crosshair: String,
    pub accent: String,
}

/// One entry in a theme's indicator palette.
///
/// Names are shared across themes on purpose: an indicator stored as "Amber"
/// stays amber-ish when the theme changes, picking up whatever that theme
/// thinks amber should be.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Swatch {
    pub name: String,
    pub hex: String,
}

impl Swatch {
    fn new(name: &str, hex: &str) -> Swatch {
        Swatch { name: name.to_string(), hex: hex.to_string() }
    }
}

/// The swatch names every shipped theme provides, in display order.
pub const SWATCH_NAMES: [&str; 8] = [
    "Blue", "Amber", "Violet", "Teal", "Rose", "Green", "Orange", "Cyan",
];

/// How a colour was chosen for an indicator or overlay.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ColorChoice {
    /// A named swatch, re-resolved against whichever theme is active.
    Swatch { name: String },
    /// An exact colour the user picked. Never re-resolved.
    Fixed { hex: String },
}

impl ColorChoice {
    pub fn swatch(name: &str) -> ColorChoice {
        ColorChoice::Swatch { name: name.to_string() }
    }

    /// Resolve against a theme, falling back to its accent if the swatch name
    /// is one this theme does not carry.
    pub fn resolve(&self, theme: &Theme) -> String {
        match self {
            ColorChoice::Fixed { hex } => hex.clone(),
            ColorChoice::Swatch { name } => theme
                .swatch(name)
                .map(|s| s.hex.clone())
                .unwrap_or_else(|| theme.ui.accent.clone()),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Theme {
    pub id: String,
    pub name: String,
    pub mode: Mode,
    pub source: Source,
    pub ui: UiColors,
    /// The theme's own indicator palette.
    pub swatches: Vec<Swatch>,
}

impl Theme {
    pub fn swatch(&self, name: &str) -> Option<&Swatch> {
        self.swatches.iter().find(|s| s.name == name)
    }

    /// The nth palette colour, wrapping. For handing successive overlays
    /// colours that do not clash.
    pub fn series(&self, n: usize) -> String {
        if self.swatches.is_empty() {
            return self.ui.accent.clone();
        }
        self.swatches[n % self.swatches.len()].hex.clone()
    }

    pub fn duplicate(&self, id: impl Into<String>, name: impl Into<String>) -> Theme {
        Theme {
            id: id.into(),
            name: name.into(),
            source: Source::Custom,
            ..self.clone()
        }
    }
}

/// One editable colour of a theme, so the settings page can be generic: it
/// walks `ALL`, groups by [`UiSlot::group`], and needs no edit when a colour
/// is added here.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UiSlot {
    Background,
    Surface,
    SurfaceVariant,
    Border,
    Text,
    TextMuted,
    Grid,
    Axis,
    Crosshair,
    Accent,
}

impl UiSlot {
    pub const ALL: [UiSlot; 10] = [
        UiSlot::Background,
        UiSlot::Surface,
        UiSlot::SurfaceVariant,
        UiSlot::Border,
        UiSlot::Text,
        UiSlot::TextMuted,
        UiSlot::Grid,
        UiSlot::Axis,
        UiSlot::Crosshair,
        UiSlot::Accent,
    ];

    pub const GROUPS: [&'static str; 4] = ["Surfaces", "Text", "Chart structure", "Accent"];

    pub fn label(self) -> &'static str {
        match self {
            UiSlot::Background => "Chart background",
            UiSlot::Surface => "Panels",
            UiSlot::SurfaceVariant => "Toolbars",
            UiSlot::Border => "Borders",
            UiSlot::Text => "Text",
            UiSlot::TextMuted => "Secondary text",
            UiSlot::Grid => "Grid lines",
            UiSlot::Axis => "Axes",
            UiSlot::Crosshair => "Crosshair",
            UiSlot::Accent => "Accent",
        }
    }

    pub fn group(self) -> &'static str {
        match self {
            UiSlot::Background | UiSlot::Surface | UiSlot::SurfaceVariant | UiSlot::Border => {
                "Surfaces"
            }
            UiSlot::Text | UiSlot::TextMuted => "Text",
            UiSlot::Grid | UiSlot::Axis | UiSlot::Crosshair => "Chart structure",
            UiSlot::Accent => "Accent",
        }
    }

    pub fn get(self, c: &UiColors) -> &str {
        match self {
            UiSlot::Background => &c.background,
            UiSlot::Surface => &c.surface,
            UiSlot::SurfaceVariant => &c.surface_variant,
            UiSlot::Border => &c.border,
            UiSlot::Text => &c.text,
            UiSlot::TextMuted => &c.text_muted,
            UiSlot::Grid => &c.grid,
            UiSlot::Axis => &c.axis,
            UiSlot::Crosshair => &c.crosshair,
            UiSlot::Accent => &c.accent,
        }
    }

    pub fn set(self, c: &mut UiColors, hex: String) {
        let field = match self {
            UiSlot::Background => &mut c.background,
            UiSlot::Surface => &mut c.surface,
            UiSlot::SurfaceVariant => &mut c.surface_variant,
            UiSlot::Border => &mut c.border,
            UiSlot::Text => &mut c.text,
            UiSlot::TextMuted => &mut c.text_muted,
            UiSlot::Grid => &mut c.grid,
            UiSlot::Axis => &mut c.axis,
            UiSlot::Crosshair => &mut c.crosshair,
            UiSlot::Accent => &mut c.accent,
        };
        *field = hex;
    }
}

// ---------------------------------------------------------------------------
// Bar scheme
// ---------------------------------------------------------------------------

/// Which way a thing moved.
///
/// The single definition of up and down in the app. Everything that colours by
/// direction — candles, volume, a watchlist's change column — asks a
/// [`BarScheme`] for the colour of a `Direction` rather than comparing numbers
/// and reaching for its own green. One definition, one palette, no drift.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Direction {
    Up,
    Down,
    Flat,
}

impl Direction {
    /// A bar's direction: a close above its open is up, equal is flat.
    pub fn of_bar(open: f64, close: f64) -> Direction {
        Direction::of_change(close - open)
    }

    /// A move's direction. Exact zero is flat; anything else commits.
    pub fn of_change(delta: f64) -> Direction {
        if delta > 0.0 {
            Direction::Up
        } else if delta < 0.0 {
            Direction::Down
        } else {
            Direction::Flat
        }
    }

    /// Stable CSS class, so widgets colour themselves the same way the chart
    /// does.
    pub fn css_class(self) -> &'static str {
        match self {
            Direction::Up => "change-up",
            Direction::Down => "change-down",
            Direction::Flat => "change-flat",
        }
    }
}

/// What candles look like, chosen independently of the theme.
///
/// Outline and body are separate so a scheme can be hollow (body == chart
/// background) or solid (body == outline) with no other machinery.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct BarScheme {
    pub id: String,
    pub name: String,
    pub source: Source,
    pub up: String,
    pub up_fill: String,
    pub down: String,
    pub down_fill: String,
    pub volume_up: String,
    pub volume_down: String,
    /// Used when open == close.
    pub neutral: String,
}

impl BarScheme {
    /// The outline colour for a direction.
    pub fn outline(&self, direction: Direction) -> &str {
        match direction {
            Direction::Up => &self.up,
            Direction::Down => &self.down,
            Direction::Flat => &self.neutral,
        }
    }

    /// The body colour for a direction. Fully transparent means hollow.
    pub fn body(&self, direction: Direction) -> &str {
        match direction {
            Direction::Up => &self.up_fill,
            Direction::Down => &self.down_fill,
            Direction::Flat => &self.neutral,
        }
    }

    /// The volume colour for a direction.
    pub fn volume(&self, direction: Direction) -> &str {
        match direction {
            Direction::Up => &self.volume_up,
            Direction::Down => &self.volume_down,
            Direction::Flat => &self.neutral,
        }
    }

    pub fn duplicate(&self, id: impl Into<String>, name: impl Into<String>) -> BarScheme {
        BarScheme {
            id: id.into(),
            name: name.into(),
            source: Source::Custom,
            ..self.clone()
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BarSlot {
    Up,
    UpFill,
    Down,
    DownFill,
    VolumeUp,
    VolumeDown,
    Neutral,
}

impl BarSlot {
    pub const ALL: [BarSlot; 7] = [
        BarSlot::Up,
        BarSlot::UpFill,
        BarSlot::Down,
        BarSlot::DownFill,
        BarSlot::VolumeUp,
        BarSlot::VolumeDown,
        BarSlot::Neutral,
    ];

    pub const GROUPS: [&'static str; 3] = ["Candles", "Volume", "Other"];

    pub fn label(self) -> &'static str {
        match self {
            BarSlot::Up => "Up outline",
            BarSlot::UpFill => "Up body",
            BarSlot::Down => "Down outline",
            BarSlot::DownFill => "Down body",
            BarSlot::VolumeUp => "Up volume",
            BarSlot::VolumeDown => "Down volume",
            BarSlot::Neutral => "Unchanged",
        }
    }

    pub fn group(self) -> &'static str {
        match self {
            BarSlot::Up | BarSlot::UpFill | BarSlot::Down | BarSlot::DownFill => "Candles",
            BarSlot::VolumeUp | BarSlot::VolumeDown => "Volume",
            BarSlot::Neutral => "Other",
        }
    }

    pub fn get(self, s: &BarScheme) -> &str {
        match self {
            BarSlot::Up => &s.up,
            BarSlot::UpFill => &s.up_fill,
            BarSlot::Down => &s.down,
            BarSlot::DownFill => &s.down_fill,
            BarSlot::VolumeUp => &s.volume_up,
            BarSlot::VolumeDown => &s.volume_down,
            BarSlot::Neutral => &s.neutral,
        }
    }

    pub fn set(self, s: &mut BarScheme, hex: String) {
        let field = match self {
            BarSlot::Up => &mut s.up,
            BarSlot::UpFill => &mut s.up_fill,
            BarSlot::Down => &mut s.down,
            BarSlot::DownFill => &mut s.down_fill,
            BarSlot::VolumeUp => &mut s.volume_up,
            BarSlot::VolumeDown => &mut s.volume_down,
            BarSlot::Neutral => &mut s.neutral,
        };
        *field = hex;
    }
}

// ---------------------------------------------------------------------------
// What we ship
// ---------------------------------------------------------------------------

/// The theme that follows the desktop. Default wherever Omarchy is present.
pub const OMARCHY_ID: &str = "omarchy";
/// The bar scheme that takes its colours from the active theme's palette.
/// Default, so candles match the desktop too.
pub const THEME_BARS_ID: &str = "theme";
/// Fallback theme when Omarchy is not installed.
pub const FALLBACK_THEME_ID: &str = "midnight";

pub fn builtin_themes() -> Vec<Theme> {
    vec![midnight(), carbon(), paper(), daylight()]
}

/// The fixed schemes. [`theme_bars`] is offered alongside these but built
/// per-theme, so it is not in the list.
pub fn builtin_bar_schemes() -> Vec<BarScheme> {
    vec![classic(), monochrome(), accessible(), hollow()]
}

fn swatches(hexes: [&str; 8]) -> Vec<Swatch> {
    SWATCH_NAMES
        .iter()
        .zip(hexes)
        .map(|(name, hex)| Swatch::new(name, hex))
        .collect()
}

/// Deep slate blue. The fallback default, and the house look.
fn midnight() -> Theme {
    Theme {
        id: FALLBACK_THEME_ID.into(),
        name: "Midnight".into(),
        mode: Mode::Dark,
        source: Source::BuiltIn,
        ui: UiColors {
            background: "#0d1117".into(),
            surface: "#151b23".into(),
            surface_variant: "#1c232c".into(),
            border: "#2a323d".into(),
            text: "#e6edf3".into(),
            text_muted: "#8b949e".into(),
            grid: "#1a2029".into(),
            axis: "#30363d".into(),
            crosshair: "#58a6ff".into(),
            accent: "#58a6ff".into(),
        },
        swatches: swatches([
            "#58a6ff", "#d29922", "#bc8cff", "#39c5cf", "#ff7b72", "#3fb950", "#ff9b50", "#76e4f7",
        ]),
    }
}

/// Neutral greys with GNOME's own accent, for an app that disappears into the
/// desktop.
fn carbon() -> Theme {
    Theme {
        id: "carbon".into(),
        name: "Carbon".into(),
        mode: Mode::Dark,
        source: Source::BuiltIn,
        ui: UiColors {
            background: "#121212".into(),
            surface: "#1a1a1a".into(),
            surface_variant: "#222222".into(),
            border: "#2e2e2e".into(),
            text: "#ededed".into(),
            text_muted: "#8f8f8f".into(),
            grid: "#1d1d1d".into(),
            axis: "#333333".into(),
            crosshair: "#d0d0d0".into(),
            accent: "#3584e4".into(),
        },
        swatches: swatches([
            "#3584e4", "#f5c211", "#9141ac", "#33d17a", "#e01b24", "#26a269", "#ff7800", "#62a0ea",
        ]),
    }
}

/// Warm light: off-white stock, ink text. Easy in daylight.
fn paper() -> Theme {
    Theme {
        id: "paper".into(),
        name: "Paper".into(),
        mode: Mode::Light,
        source: Source::BuiltIn,
        ui: UiColors {
            background: "#fbf8f3".into(),
            surface: "#f4efe7".into(),
            surface_variant: "#ebe4d8".into(),
            border: "#ddd3c3".into(),
            text: "#2d2a26".into(),
            text_muted: "#7a7268".into(),
            grid: "#efe9de".into(),
            axis: "#c9bfae".into(),
            crosshair: "#a85b2a".into(),
            accent: "#a85b2a".into(),
        },
        swatches: swatches([
            "#1f6feb", "#b8860b", "#7d3c98", "#0f766e", "#b03a2e", "#2f7d52", "#c2601c", "#1f7a8c",
        ]),
    }
}

/// Crisp light: white stock, cool neutrals.
fn daylight() -> Theme {
    Theme {
        id: "daylight".into(),
        name: "Daylight".into(),
        mode: Mode::Light,
        source: Source::BuiltIn,
        ui: UiColors {
            background: "#ffffff".into(),
            surface: "#f6f8fa".into(),
            surface_variant: "#eef1f4".into(),
            border: "#d8dee4".into(),
            text: "#1f2328".into(),
            text_muted: "#656d76".into(),
            grid: "#f0f3f6".into(),
            axis: "#c9d1d9".into(),
            crosshair: "#0969da".into(),
            accent: "#0969da".into(),
        },
        swatches: swatches([
            "#0969da", "#9a6700", "#8250df", "#1b7c83", "#cf222e", "#1a7f37", "#bc4c00", "#0a7ea4",
        ]),
    }
}

/// Candles built from the active theme's own palette.
///
/// The default, and what makes the chart match the desktop on Omarchy: the
/// theme supplies green and rose, so candles shift with it instead of staying
/// a fixed pair of hexes that clash with half the themes available.
pub fn theme_bars(theme: &Theme) -> BarScheme {
    let up = theme
        .swatch("Green")
        .map(|s| s.hex.clone())
        .unwrap_or_else(|| "#3fb950".to_string());
    let down = theme
        .swatch("Rose")
        .map(|s| s.hex.clone())
        .unwrap_or_else(|| "#f85149".to_string());
    BarScheme {
        id: THEME_BARS_ID.to_string(),
        name: "Theme".to_string(),
        source: Source::BuiltIn,
        volume_up: mix(&up, &theme.ui.background, 0.45),
        volume_down: mix(&down, &theme.ui.background, 0.45),
        up_fill: up.clone(),
        down_fill: down.clone(),
        up,
        down,
        neutral: theme.ui.text_muted.clone(),
    }
}

/// Blend `a` toward `b`. `t` of 0 is all `a`, 1 is all `b`.
///
/// Volume bars want to be the candle colour pushed most of the way into the
/// background so they read as a dimmer echo rather than a second signal.
pub fn mix(a: &str, b: &str, t: f64) -> String {
    let (Some((ar, ag, ab)), Some((br, bg, bb))) = (rgb(a), rgb(b)) else {
        return a.to_string();
    };
    let lerp = |x: u8, y: u8| (x as f64 + (y as f64 - x as f64) * t).round() as u8;
    format!("#{:02x}{:02x}{:02x}", lerp(ar, br), lerp(ag, bg), lerp(ab, bb))
}

/// `#rgb`, `#rrggbb` or `#rrggbbaa` to bytes. Alpha is dropped.
pub fn rgb(hex: &str) -> Option<(u8, u8, u8)> {
    let h = hex.trim().trim_start_matches('#');
    let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    match h.len() {
        3 => {
            let d = |i: usize| {
                u8::from_str_radix(&h[i..i + 1], 16)
                    .ok()
                    .map(|v| v * 17)
            };
            Some((d(0)?, d(1)?, d(2)?))
        }
        6 | 8 => Some((byte(0)?, byte(2)?, byte(4)?)),
        _ => None,
    }
}

/// Green up, red down. What most people expect.
fn classic() -> BarScheme {
    BarScheme {
        id: "classic".into(),
        name: "Classic".into(),
        source: Source::BuiltIn,
        up: "#3fb950".into(),
        up_fill: "#3fb950".into(),
        down: "#f85149".into(),
        down_fill: "#f85149".into(),
        volume_up: "#2d6e3b".into(),
        volume_down: "#8c322e".into(),
        neutral: "#8b949e".into(),
    }
}

/// No colour at all: light grey throughout, direction read from whether the
/// body is filled. Quiet, and it never fights the theme.
fn monochrome() -> BarScheme {
    BarScheme {
        id: "monochrome".into(),
        name: "Monochrome".into(),
        source: Source::BuiltIn,
        up: "#c9d1d9".into(),
        up_fill: "#00000000".into(),
        down: "#c9d1d9".into(),
        down_fill: "#c9d1d9".into(),
        volume_up: "#555f6a".into(),
        volume_down: "#8b949e".into(),
        neutral: "#8b949e".into(),
    }
}

/// Blue and orange rather than green and red, which the most common forms of
/// colour blindness cannot separate.
fn accessible() -> BarScheme {
    BarScheme {
        id: "accessible".into(),
        name: "Blue / Orange".into(),
        source: Source::BuiltIn,
        up: "#4493f8".into(),
        up_fill: "#4493f8".into(),
        down: "#ff9b50".into(),
        down_fill: "#ff9b50".into(),
        volume_up: "#2a5a8f".into(),
        volume_down: "#8f5a2a".into(),
        neutral: "#8b949e".into(),
    }
}

/// Outlined up, solid down — the convention that keeps charts airy.
fn hollow() -> BarScheme {
    BarScheme {
        id: "hollow".into(),
        name: "Hollow".into(),
        source: Source::BuiltIn,
        up: "#3fb950".into(),
        up_fill: "#00000000".into(),
        down: "#f85149".into(),
        down_fill: "#f85149".into(),
        volume_up: "#2d6e3b".into(),
        volume_down: "#8c322e".into(),
        neutral: "#8b949e".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_theme_carries_the_full_swatch_palette() {
        for theme in builtin_themes() {
            assert_eq!(theme.swatches.len(), SWATCH_NAMES.len(), "{}", theme.name);
            for name in SWATCH_NAMES {
                assert!(theme.swatch(name).is_some(), "{} lacks {name}", theme.name);
            }
        }
    }

    #[test]
    fn a_swatch_choice_follows_the_theme() {
        let choice = ColorChoice::swatch("Amber");
        assert_eq!(choice.resolve(&midnight()), "#d29922");
        assert_eq!(choice.resolve(&paper()), "#b8860b");
    }

    #[test]
    fn a_fixed_choice_ignores_the_theme() {
        let choice = ColorChoice::Fixed { hex: "#abcdef".into() };
        assert_eq!(choice.resolve(&midnight()), "#abcdef");
        assert_eq!(choice.resolve(&paper()), "#abcdef");
    }

    #[test]
    fn an_unknown_swatch_falls_back_to_the_accent() {
        let choice = ColorChoice::swatch("Chartreuse");
        assert_eq!(choice.resolve(&midnight()), midnight().ui.accent);
    }

    #[test]
    fn series_colours_wrap_without_panicking() {
        let theme = midnight();
        assert_eq!(theme.series(0), theme.swatches[0].hex);
        assert_eq!(theme.series(8), theme.swatches[0].hex);
    }

    #[test]
    fn slots_round_trip() {
        let mut theme = midnight();
        for slot in UiSlot::ALL {
            slot.set(&mut theme.ui, "#123456".into());
            assert_eq!(slot.get(&theme.ui), "#123456");
        }
        let mut bars = classic();
        for slot in BarSlot::ALL {
            slot.set(&mut bars, "#654321".into());
            assert_eq!(slot.get(&bars), "#654321");
        }
    }


    #[test]
    fn theme_bars_take_their_colours_from_the_theme() {
        let theme = paper();
        let bars = theme_bars(&theme);
        assert_eq!(bars.up, theme.swatch("Green").unwrap().hex);
        assert_eq!(bars.down, theme.swatch("Rose").unwrap().hex);
        assert_eq!(bars.id, THEME_BARS_ID);
    }

    #[test]
    fn volume_sits_between_the_candle_and_the_background() {
        let theme = midnight();
        let bars = theme_bars(&theme);
        assert_ne!(bars.volume_up, bars.up);
        assert_ne!(bars.volume_up, theme.ui.background);
    }

    #[test]
    fn hex_parsing_handles_every_length() {
        assert_eq!(rgb("#fff"), Some((255, 255, 255)));
        assert_eq!(rgb("#0d1117"), Some((13, 17, 23)));
        assert_eq!(rgb("#0d1117ff"), Some((13, 17, 23)));
        assert_eq!(rgb("nonsense"), None);
    }

    #[test]
    fn mixing_the_extremes_returns_the_endpoints() {
        assert_eq!(mix("#000000", "#ffffff", 0.0), "#000000");
        assert_eq!(mix("#000000", "#ffffff", 1.0), "#ffffff");
        assert_eq!(mix("#000000", "#ffffff", 0.5), "#808080");
    }

    #[test]
    fn direction_is_defined_once_and_agrees_with_itself() {
        assert_eq!(Direction::of_bar(10.0, 11.0), Direction::Up);
        assert_eq!(Direction::of_bar(11.0, 10.0), Direction::Down);
        assert_eq!(Direction::of_bar(10.0, 10.0), Direction::Flat);
        assert_eq!(Direction::of_change(0.01), Direction::Up);
        assert_eq!(Direction::of_change(-0.01), Direction::Down);
        assert_eq!(Direction::of_change(0.0), Direction::Flat);
    }

    #[test]
    fn every_scheme_answers_for_every_direction() {
        let mut schemes = builtin_bar_schemes();
        schemes.push(theme_bars(&midnight()));
        for scheme in schemes {
            for direction in [Direction::Up, Direction::Down, Direction::Flat] {
                assert!(!scheme.outline(direction).is_empty(), "{}", scheme.name);
                assert!(!scheme.body(direction).is_empty(), "{}", scheme.name);
                assert!(!scheme.volume(direction).is_empty(), "{}", scheme.name);
            }
        }
    }

    #[test]
    fn up_and_down_are_never_the_same_colour() {
        // Monochrome is the deliberate exception: it separates direction by
        // whether the body is filled, not by hue.
        for scheme in builtin_bar_schemes() {
            if scheme.id == "monochrome" {
                assert_ne!(scheme.body(Direction::Up), scheme.body(Direction::Down));
                continue;
            }
            assert_ne!(
                scheme.outline(Direction::Up),
                scheme.outline(Direction::Down),
                "{}",
                scheme.name
            );
        }
    }

    #[test]
    fn the_default_scheme_is_green_up_red_down() {
        // "Typically red/green" is what people expect, and the theme palette
        // is where those two live.
        let bars = theme_bars(&midnight());
        assert_eq!(bars.outline(Direction::Up), midnight().swatch("Green").unwrap().hex);
        assert_eq!(bars.outline(Direction::Down), midnight().swatch("Rose").unwrap().hex);
    }

    #[test]
    fn css_classes_are_stable() {
        assert_eq!(Direction::Up.css_class(), "change-up");
        assert_eq!(Direction::Down.css_class(), "change-down");
        assert_eq!(Direction::Flat.css_class(), "change-flat");
    }

    #[test]
    fn only_custom_things_are_editable() {
        assert!(!Source::BuiltIn.is_editable());
        assert!(!Source::Omarchy.is_editable());
        assert!(Source::Custom.is_editable());
    }
}
