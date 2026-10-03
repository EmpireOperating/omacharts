//! Choosing a theme and a bar scheme, and making the whole app wear them.
//!
//! On Omarchy the desktop theme is the default for both, and it is followed
//! live: the palette is re-read on a timer and the app repaints when it
//! changes. Everything is overridable in settings, and a custom theme is just
//! a palette somebody saved.
//!
//! "The whole app" is meant literally. The theme's colours are pushed into
//! libadwaita's own named colours, so headers, popovers, lists, dialogs and
//! the chart all come from one palette rather than the chart being a themed
//! island in a default-grey window.

use std::path::PathBuf;

use std::fmt::Write as _;

use omacharts_engine::frame::{GUTTER_WIDTH, RING_WIDTH};
use omacharts_engine::theme::{
    builtin_bar_schemes, builtin_themes, theme_bars, BarScheme, Mode, Theme,
    FALLBACK_THEME_ID, OMARCHY_ID, THEME_BARS_ID,
};
use omacharts_engine::omarchy;

use crate::store::{home, Store};
use crate::ui::colors;

pub const SETTING_THEME: &str = "theme";
pub const SETTING_BARS: &str = "bar_scheme";

pub struct Theming {
    home: PathBuf,
    /// Rebuilt from the desktop whenever its fingerprint moves.
    omarchy: Option<Theme>,
    omarchy_fingerprint: Option<String>,
    builtin_themes: Vec<Theme>,
    builtin_schemes: Vec<BarScheme>,
    custom_themes: Vec<Theme>,
    custom_schemes: Vec<BarScheme>,
    theme_id: String,
    scheme_id: String,
    provider: Option<gtk::CssProvider>,
}

impl Theming {
    pub fn load(store: &Store) -> Theming {
        let home = home();
        let omarchy = omarchy::current(&home);
        let omarchy_fingerprint = omarchy::fingerprint(&home);

        // Omarchy's theme is the default when the desktop has one, so a fresh
        // install already matches everything else on screen.
        let default_theme = if omarchy.is_some() { OMARCHY_ID } else { FALLBACK_THEME_ID };

        Theming {
            theme_id: store.setting(SETTING_THEME).unwrap_or_else(|| default_theme.to_string()),
            scheme_id: store.setting(SETTING_BARS).unwrap_or_else(|| THEME_BARS_ID.to_string()),
            omarchy,
            omarchy_fingerprint,
            builtin_themes: builtin_themes(),
            builtin_schemes: builtin_bar_schemes(),
            custom_themes: store.custom_themes(),
            custom_schemes: store.custom_bar_schemes(),
            home,
            provider: None,
        }
    }

    pub fn omarchy_available(&self) -> bool {
        self.omarchy.is_some()
    }

    /// Every theme on offer: the desktop's, then ours, then the user's.
    pub fn themes(&self) -> Vec<&Theme> {
        self.omarchy
            .iter()
            .chain(self.builtin_themes.iter())
            .chain(self.custom_themes.iter())
            .collect()
    }

    /// Every bar scheme on offer. The first is built from the active theme, so
    /// candles match the desktop without the user choosing anything.
    pub fn bar_schemes(&self) -> Vec<BarScheme> {
        let mut out = vec![theme_bars(&self.theme())];
        out.extend(self.builtin_schemes.iter().cloned());
        out.extend(self.custom_schemes.iter().cloned());
        out
    }

    /// The active theme, owned.
    ///
    /// A theme can vanish — Omarchy uninstalled, a custom one deleted from
    /// another window — so this falls back rather than refusing to draw.
    pub fn theme(&self) -> Theme {
        let themes = self.themes();
        themes
            .iter()
            .find(|t| t.id == self.theme_id)
            .or_else(|| themes.first())
            .map(|t| (*t).clone())
            .unwrap_or_else(|| self.builtin_themes[0].clone())
    }

    pub fn bar_scheme(&self) -> BarScheme {
        let schemes = self.bar_schemes();
        schemes
            .iter()
            .find(|s| s.id == self.scheme_id)
            .cloned()
            .unwrap_or_else(|| schemes[0].clone())
    }

    pub fn theme_id(&self) -> &str {
        &self.theme_id
    }

    pub fn scheme_id(&self) -> &str {
        &self.scheme_id
    }

    pub fn select_theme(&mut self, id: &str, store: &Store) {
        self.theme_id = id.to_string();
        store.set_setting(SETTING_THEME, id);
    }

    pub fn select_bar_scheme(&mut self, id: &str, store: &Store) {
        self.scheme_id = id.to_string();
        store.set_setting(SETTING_BARS, id);
    }

    pub fn reload_custom(&mut self, store: &Store) {
        self.custom_themes = store.custom_themes();
        self.custom_schemes = store.custom_bar_schemes();
    }

    /// Re-read the desktop palette. `true` when something moved and the app
    /// should restyle and repaint.
    ///
    /// Called on a timer rather than from a file watch because Omarchy swaps a
    /// symlinked directory, which is exactly where watches miss changes.
    pub fn poll_omarchy(&mut self) -> bool {
        let fingerprint = omarchy::fingerprint(&self.home);
        if fingerprint == self.omarchy_fingerprint {
            return false;
        }
        self.omarchy_fingerprint = fingerprint;
        self.omarchy = omarchy::current(&self.home);
        true
    }

    /// Push the active theme into libadwaita's named colours.
    pub fn apply(&mut self) {
        let theme = self.theme();

        adw::StyleManager::default().set_color_scheme(match theme.mode {
            Mode::Dark => adw::ColorScheme::ForceDark,
            Mode::Light => adw::ColorScheme::ForceLight,
        });

        let css = stylesheet(&theme, &self.bar_scheme());
        let provider = self.provider.get_or_insert_with(|| {
            let provider = gtk::CssProvider::new();
            if let Some(display) = gtk::gdk::Display::default() {
                gtk::style_context_add_provider_for_display(
                    &display,
                    &provider,
                    // Above the platform stylesheet, so our named colours win.
                    gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
                );
            }
            provider
        });
        provider.load_from_data(&css);
    }
}

/// The app's CSS: libadwaita's named colours, redefined from the theme, plus
/// the few classes we add ourselves.
pub fn stylesheet(theme: &Theme, scheme: &BarScheme) -> String {
    let ui = &theme.ui;
    let accent_fg = colors::readable_on(&ui.accent);
    let frame = theme.frame(scheme);

    let mut css = String::with_capacity(2048);
    let mut define = |name: &str, value: &str| {
        css.push_str(&format!("@define-color {name} {value};\n"));
    };

    // Windows and views. `view_bg_color` is the chart's own ground, so lists
    // and the chart agree.
    define("window_bg_color", &ui.surface);
    define("window_fg_color", &ui.text);
    define("view_bg_color", &ui.background);
    define("view_fg_color", &ui.text);
    define("headerbar_bg_color", &ui.surface_variant);
    define("headerbar_fg_color", &ui.text);
    define("headerbar_border_color", &ui.border);
    define("headerbar_backdrop_color", &ui.surface);
    define("sidebar_bg_color", &ui.surface);
    define("sidebar_fg_color", &ui.text);
    define("sidebar_border_color", &ui.border);
    define("sidebar_backdrop_color", &ui.surface);
    define("secondary_sidebar_bg_color", &ui.surface);
    define("secondary_sidebar_fg_color", &ui.text);
    define("card_bg_color", &ui.surface_variant);
    define("card_fg_color", &ui.text);
    define("dialog_bg_color", &ui.surface);
    define("dialog_fg_color", &ui.text);
    define("popover_bg_color", &ui.surface_variant);
    define("popover_fg_color", &ui.text);
    define("borders", &ui.border);

    // Accent. libadwaita 1.6 split the standalone accent from the filled one.
    define("accent_bg_color", &ui.accent);
    define("accent_fg_color", accent_fg);
    define("accent_color", &ui.accent);

    // Rising and falling text follows the bar scheme, so a watchlist agrees
    // with the candles beside it.
    define("omacharts_up", &scheme.up);
    define("omacharts_down", &scheme.down);

    // The frame around tiled charts. Derived, not taken: the engine explains
    // why in `frame.rs`, and `doc/themes/frames.html` shows the result on
    // every theme.
    define("omacharts_gutter", &frame.gutter);
    define("omacharts_gutter_hover", &frame.gutter_hover);
    define("omacharts_focus", &frame.focus);

    css.push_str(
        "
.change-up { color: @omacharts_up; }
.change-down { color: @omacharts_down; }
.change-flat { opacity: 0.55; }

.numeric { font-feature-settings: 'tnum'; font-variant-numeric: tabular-nums; }

/* The symbol palette: a quiet list where the row, not a border, carries the
   selection. */
.symbol-row-ticker { font-weight: 700; }
.symbol-row-name { font-size: 0.9em; opacity: 0.7; }
.symbol-kind {
  font-size: 0.75em;
  padding: 1px 6px;
  border-radius: 6px;
  background: alpha(@accent_bg_color, 0.18);
  color: @accent_color;
}

/* The readout above the chart. Tabular figures so values do not jitter as the
   crosshair moves. */
.readout { font-size: 0.85em; }
.readout-symbol { font-size: 1.05em; font-weight: 700; }
.delay-chip {
  font-size: 0.75em;
  padding: 1px 6px;
  border-radius: 6px;
  background: alpha(@card_fg_color, 0.08);
  opacity: 0.75;
}

/* Timeframe buttons: flat strip, the active one filled. */
.timeframe-strip { padding: 2px; }
.timeframe-strip button { min-width: 34px; padding: 2px 6px; }

.swatch-button { min-width: 26px; min-height: 26px; padding: 0; border-radius: 6px; }

/* GtkPopoverMenu keeps its items in a scroller, and the popover measures the
   scroller rather than the padded box around it. So a menu that fits perfectly
   is still a dozen pixels short of its own contents: it scrolls, and the last
   item sits on the rounded corner with nothing under it.

   The padding is one part of it and the one-pixel border above and below is
   the other; neither is counted. Zeroing both takes the slack away, and the
   breathing room goes on the items as margins instead, which every level does
   count. Measured through the live widget tree with the real menu — two
   submenus and three sections — requested and allocated heights now match
   exactly, so there is nothing left to scroll. */
popover.menu contents {
  padding-top: 0;
  padding-bottom: 0;
  border-top-width: 0;
  border-bottom-width: 0;
}
popover.menu modelbutton:first-child { margin-top: 6px; }
popover.menu modelbutton:last-child { margin-bottom: 6px; }

/* The link toggle is a glyph, not a button: no frame, no fill, not even when
   it is on — a chart that follows the rail says so by the icon being there at
   full strength, and one that does not says so by the icon being nearly gone.
   The only time it wears anything is under the pointer, which is the moment
   you are about to click it. */
.legend-link,
.legend-link:checked,
.legend-link:checked:not(:hover) {
  background: none;
  background-image: none;
  border: none;
  box-shadow: none;
  outline: none;
  min-width: 20px;
  min-height: 20px;
  padding: 1px;
}
.legend-link:hover {
  background: alpha(currentColor, 0.12);
  border-radius: 5px;
}

/* The symbol over the chart is a button, because clicking the name of the
   thing you are looking at to change it is the shortest route there is — but
   it should read as the title it replaced until you reach for it. */
.legend-symbol {
  background: none;
  border: none;
  box-shadow: none;
  padding: 0 4px;
  min-height: 0;
}
.legend-symbol:hover { background: alpha(currentColor, 0.12); }

/* The legend sits over the drawing, and the drawing is the point. Everything
   here stays faint until the pointer is near it. */
.legend-gear { opacity: 0.35; min-width: 22px; min-height: 22px; padding: 2px; }
.legend-gear:hover { opacity: 1; }

/* The window's own controls — the watchlist toggle and the main menu — sit
   in the top-right corner, over whatever is there, instead of on a header
   bar. A header bar is a 47px band across the whole window; it held two
   buttons, a close button the window manager already provides for, and a
   title every chart writes in its own corner, and the band cost the charts
   that height. This is the HIG's overlaid-controls pattern: controls over
   content, attached to the window's edge, quiet until the pointer is near
   them. The box is also the window's drag handle, so the blank space at its
   start is what a header bar would have offered to be grabbed by. */
.window-corner {
  padding: 3px 6px 3px 12px;
  opacity: 0.75;
  transition: opacity 120ms ease-out;
}
.window-corner:hover { opacity: 1; }
.window-corner button { min-width: 24px; min-height: 24px; padding: 3px; }

/* The corner sits over the rail when the rail is open, which is where the
   HIG puts a sidebar's menu (above the sidebar list), so the rail's column
   header steps down out from under it. The step is the corner's height: three
   pixels of padding either side of a 24px button. */
.rail-header { margin-top: 30px; }

/* A way back to a default, without shouting about it. */
.subtle-link {
  font-size: 0.85em;
  opacity: 0.6;
  padding: 2px 6px;
  min-height: 0;
}
.subtle-link:hover { opacity: 1; }
.subtle-link:disabled { opacity: 0.25; }

/* Keys read as keys. */
.keycap {
  font-size: 0.85em;
  font-feature-settings: 'tnum';
  padding: 2px 8px;
  border-radius: 6px;
  background: alpha(@card_fg_color, 0.08);
  border: 1px solid alpha(@card_fg_color, 0.10);
}

.legend-row { padding: 0 0 1px 1px; }
.legend-indicator { font-size: 0.78em; opacity: 0.75; }
.legend-indicator-hidden { opacity: 0.35; text-decoration: line-through; }
.legend-button {
  opacity: 0;
  min-width: 18px;
  min-height: 18px;
  padding: 0;
  -gtk-icon-size: 12px;
}
.legend-row:hover .legend-button { opacity: 0.55; }
.legend-button:hover { opacity: 1; }
",
    );

    // Tiled charts borrow the window manager's idiom, since an Omarchy user
    // reads it without thinking: panes sit on a gap, the focused one wears a
    // thin border in the accent, and nothing else is drawn.
    let _ = write!(
        css,
        "
/* The gap between two charts is the window surface showing through, the way
   the gap between two windows shows the desktop, and it is also the handle
   that resizes them. libadwaita gives a wide handle a hairline along each of
   its edges, as inset shadows in its own border colour, which under a
   chart's time axis reads as a hairline, a band and a second hairline: a
   double separator. The shadows go, so the handle is one band of one colour.
   It stays wide because the band is then the whole hit area; the narrow
   handle is grabbed through an invisible extension over the first pixels of
   the charts beside it, which is exactly where the focus ring is. */
paned.chart-split > separator {{
  min-width: {gutter}px;
  min-height: {gutter}px;
  margin: 0;
  padding: 0;
  border: none;
  box-shadow: none;
  background: @omacharts_gutter;
  transition: background-color 120ms ease-out;
}}
paned.chart-split > separator:hover,
paned.chart-split > separator:active {{ background-color: @omacharts_gutter_hover; }}

/* The focused pane is the one keys, menus and the symbol search act on, so
   with four charts open it has to be told apart at a glance: a ring inside
   its edge, in the accent's hue at a lightness the engine chooses so that it
   is seen the same amount on every theme and is never the colour of the
   axis, the crosshair or a candle. Every pane carries the border, in the
   chart's own colour when unfocused, so focus moving between panes changes a
   colour and not a layout. Only inside a split: a chart on its own has
   nothing to be told apart from. */
.chart-pane {{
  background-color: @view_bg_color;
  border: {ring}px solid transparent;
}}
.chart-split .chart-pane.focused {{ border-color: @omacharts_focus; }}
",
        gutter = GUTTER_WIDTH,
        ring = RING_WIDTH,
    );
    css
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stylesheet_redefines_the_platform_colours() {
        let theme = &builtin_themes()[0];
        let css = stylesheet(theme, &builtin_bar_schemes()[0]);
        for name in [
            "window_bg_color",
            "view_bg_color",
            "headerbar_bg_color",
            "card_bg_color",
            "popover_bg_color",
            "accent_bg_color",
            "borders",
        ] {
            assert!(css.contains(&format!("@define-color {name} ")), "missing {name}");
        }
        assert!(css.contains(&theme.ui.background));
        assert!(css.contains(&theme.ui.accent));
        assert!(css.contains("@define-color omacharts_up "));
        assert!(css.contains("@define-color omacharts_down "));
    }

    #[test]
    fn the_frame_is_styled_from_the_engines_colours_and_widths() {
        let theme = &builtin_themes()[0];
        let scheme = builtin_bar_schemes()[0].clone();
        let css = stylesheet(theme, &scheme);
        let frame = theme.frame(&scheme);
        for (name, value) in [
            ("omacharts_gutter", &frame.gutter),
            ("omacharts_gutter_hover", &frame.gutter_hover),
            ("omacharts_focus", &frame.focus),
        ] {
            assert!(css.contains(&format!("@define-color {name} {value};")), "missing {name}");
        }
        assert!(css.contains(&format!("min-width: {GUTTER_WIDTH}px;")));
        assert!(css.contains(&format!("border: {RING_WIDTH}px solid transparent;")));
        // Only a split shows focus; a lone chart has nothing to be told from.
        assert!(css.contains(".chart-split .chart-pane.focused { border-color: @omacharts_focus; }"));
        // And the wide handle's two hairlines are gone.
        assert!(css.contains("box-shadow: none;"));
    }

    #[test]
    fn accent_text_contrasts_with_the_accent() {
        assert_eq!(colors::readable_on("#ffffff"), "#000000");
        assert_eq!(colors::readable_on("#0d1117"), "#ffffff");
    }

    #[test]
    fn omarchy_is_the_default_when_present_and_midnight_when_not() {
        // Exercised through Theming::load against a store with no settings,
        // which is the only place the choice is made.
        let store = Store::memory().unwrap();
        let theming = Theming::load(&store);
        let expected = if omarchy::available(&home()) { OMARCHY_ID } else { FALLBACK_THEME_ID };
        assert_eq!(theming.theme_id(), expected);
        assert_eq!(theming.scheme_id(), THEME_BARS_ID);
    }

    #[test]
    fn a_missing_theme_falls_back_rather_than_panicking() {
        let store = Store::memory().unwrap();
        store.set_setting(SETTING_THEME, "deleted-by-another-window");
        let theming = Theming::load(&store);
        assert!(!theming.theme().id.is_empty());
    }

    #[test]
    fn the_theme_bar_scheme_is_always_offered_first() {
        let store = Store::memory().unwrap();
        let theming = Theming::load(&store);
        let schemes = theming.bar_schemes();
        assert_eq!(schemes[0].id, THEME_BARS_ID);
        assert!(schemes.len() > 4);
    }
}
