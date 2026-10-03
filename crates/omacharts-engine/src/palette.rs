//! The indicator palette, built from whatever a theme has to offer.
//!
//! The obvious way to get an indicator palette out of an Omarchy theme is to
//! take its sixteen terminal colours and call the blue one "Blue". It does not
//! work, and the shipped themes show why. Terminal palettes are not designed
//! to be told apart as thin lines: Lumon is six shades of one blue, Lupine's
//! yellow is blue, Hackerman's red is green, Vantablack has no hue at all, and
//! in thirteen of the twenty-two themes `cyan` and `bright_cyan` are the same
//! hex. A palette that only extracts will hand two overlays the same colour in
//! most themes and look like a box of crayons in the rest.
//!
//! So the palette is *generated*, with the theme deciding everything except
//! the one thing a name has to guarantee. From the theme come the lightness
//! band (where its own colours sit, pulled into the range that can be seen on
//! its background) and the chroma (as vivid as its own colours, within reason).
//! From the name comes the hue, and even that is borrowed from the theme
//! whenever the theme has a colour near enough to what the name means: Nord's
//! Blue is Nord's blue, Everforest's Teal is Everforest's mint. Only when the
//! theme has nothing to offer is a hue invented, and then it is the hue the
//! name promises, at the theme's own lightness and chroma, so it still looks
//! like it was chosen by the same hand.
//!
//! Everything is done in OKLCh, because that is where "same lightness" and
//! "far enough apart" mean what the eye sees. See [`Oklch`].

use std::collections::HashMap;

use crate::theme::{contrast_ratio, delta_e, hue_gap, Oklch, Swatch, SWATCH_NAMES, SWATCH_SEQUENCE};

/// What a swatch name promises.
struct Promise {
    name: &'static str,
    /// The hue the name means, in OKLCh degrees.
    hue: f64,
    /// How far above or below the palette's centre the hue naturally sits.
    ///
    /// Yellow is a light colour and blue a deep one; a palette that paints
    /// them at identical lightness looks synthetic, and worse, loses the
    /// lightness difference that keeps amber and orange apart when their hues
    /// are only forty degrees from each other.
    lift: f64,
    /// Which theme keys usually carry it, most trusted first.
    keys: &'static [&'static str],
}

/// In [`SWATCH_NAMES`] order. The hues are spread so that no two of the six
/// automatically assigned ones are closer than forty degrees, which is what
/// the separation floor needs at the quietest chroma allowed.
const PROMISES: [Promise; 8] = [
    Promise { name: "Blue", hue: 262.0, lift: -0.05, keys: &["blue", "bright_blue", "accent"] },
    Promise { name: "Amber", hue: 90.0, lift: 0.06, keys: &["yellow", "bright_yellow"] },
    Promise { name: "Violet", hue: 310.0, lift: -0.04, keys: &["magenta", "purple", "bright_magenta"] },
    Promise { name: "Teal", hue: 180.0, lift: 0.0, keys: &["cyan", "bright_cyan"] },
    Promise { name: "Rose", hue: 12.0, lift: -0.03, keys: &["red", "bright_red"] },
    Promise { name: "Green", hue: 140.0, lift: 0.02, keys: &["green", "bright_green"] },
    Promise { name: "Orange", hue: 50.0, lift: -0.01, keys: &["orange", "brown"] },
    Promise { name: "Cyan", hue: 220.0, lift: 0.03, keys: &["bright_cyan", "cyan"] },
];

/// The theme keys that are colours rather than surfaces or text. These decide
/// the band; a theme's background and foreground are not colours in the sense
/// that matters here.
const COLOUR_KEYS: [&str; 16] = [
    "red", "yellow", "orange", "green", "cyan", "blue", "magenta", "purple", "brown", "accent",
    "bright_red", "bright_yellow", "bright_green", "bright_cyan", "bright_blue", "bright_magenta",
];

/// How far a theme colour's hue may be from what a name means and still be
/// that name. Thirty degrees is a blue that leans violet, not a purple.
const HUE_TOLERANCE: f64 = 30.0;

/// Below this a colour is a tinted grey, not a hue anyone could name.
const MIN_THEME_CHROMA: f64 = 0.04;

/// The chroma band lines are painted in.
///
/// The floor is the quietest a line can be and still be told apart from its
/// neighbours by hue alone; it is what gives a greyscale theme a usable
/// palette. The ceiling stops a vivid theme like Lupine producing lines that
/// vibrate against the background.
const MIN_CHROMA: f64 = 0.09;
const MAX_CHROMA: f64 = 0.16;

/// How much a theme colour's lightness and chroma may stray from the band
/// and still be used as it is. Beyond this it is pulled in: Catppuccin's
/// yellow is cream, and a cream line on a dark chart is indistinguishable
/// from the text.
const LIGHTNESS_SLACK: f64 = 0.05;
const CHROMA_SLACK: f64 = 0.25;

/// WCAG's floor for graphics that carry meaning is 3:1. The band's centre
/// aims higher, so a hue that sits below centre still clears the floor, and
/// each final colour is then held to the floor itself.
const CENTRE_CONTRAST: f64 = 4.5;
const MIN_CONTRAST: f64 = 3.2;

/// Lines outside this range stop reading as colours: above, they are pastel
/// going on white, and sRGB cannot even hold a blue's chroma up there, so a
/// pale sky line comes out grey; below, they are mud. Independent of the
/// background. The band's centre keeps clear of both ends by the largest
/// lift, so every hue lands inside.
const LIGHTEST: f64 = 0.88;
const DARKEST: f64 = 0.36;
const MAX_LIFT: f64 = 0.06;

/// How different two lines that might cross have to look. At this distance a
/// pair of 1.5px lines is told apart at a glance; below about 0.06 they read
/// as the same line in two places.
const MIN_SEPARATION: f64 = 0.08;

/// A line must never be mistaken for the chart's furniture. The grid sits
/// next to the background, the axis is a single line at the edge and the
/// crosshair is dashed and moves, so they need less distance than a second
/// overlay does; but an overlay cannot be the near-identical colour, which is
/// what naively using the accent for both the crosshair and "blue" produces.
/// Any higher and a quiet line on a light theme cannot escape a grey axis of
/// the same lightness, and gets driven into the mud.
const MIN_FROM_FURNITURE: f64 = 0.06;

/// How far a theme's own colour may be moved in lightness before it stops
/// being that colour and the name's own hue is the better answer.
const FAITHFUL_REACH: f64 = 0.12;

/// What the lines will be drawn on and beside.
pub struct Ground<'a> {
    pub background: &'a str,
    pub grid: &'a str,
    pub axis: &'a str,
    pub crosshair: &'a str,
}

/// Build the full named palette for a theme.
pub fn generate(keys: &HashMap<String, String>, ground: &Ground) -> Vec<Swatch> {
    let colours = theme_colours(keys);
    let band = Band::of(&colours, ground.background);

    // Up and down are placed first because they are the chart's own colours,
    // not overlays: everything else must stay clear of them, never the
    // reverse. Then the automatic sequence in order, so that the first overlay
    // on a chart is the most faithful and later ones give way.
    let mut placed: Vec<(&'static str, Oklch)> = Vec::new();
    for name in ["Green", "Rose"] {
        let colour = direction_colour(promise(name), &colours, &band, &placed, ground);
        placed.push((name, colour));
    }
    for name in SWATCH_SEQUENCE {
        let colour = series_colour(promise(name), &colours, &band, &placed, ground);
        placed.push((name, colour));
    }

    SWATCH_NAMES
        .iter()
        .map(|name| {
            let colour = placed.iter().find(|(n, _)| n == name).map(|(_, c)| *c).expect("placed");
            Swatch { name: name.to_string(), hex: colour.hex() }
        })
        .collect()
}

fn promise(name: &str) -> &'static Promise {
    PROMISES.iter().find(|p| p.name == name).expect("a promise for every swatch name")
}

/// The hue a swatch name stands for, in OKLCh degrees.
///
/// This is the contract behind storing an indicator's colour by name: an
/// "Amber" saved under one theme must come back amber under every other, or
/// the name was a lie. Tests hold every theme to it.
pub fn promised_hue(name: &str) -> Option<f64> {
    PROMISES.iter().find(|p| p.name == name).map(|p| p.hue)
}

/// The theme's colours, keyed, as OKLCh. Anything that does not parse is
/// simply not offered.
fn theme_colours(keys: &HashMap<String, String>) -> Vec<(&'static str, Oklch)> {
    COLOUR_KEYS
        .iter()
        .filter_map(|key| keys.get(*key).and_then(|hex| Oklch::of(hex)).map(|c| (*key, c)))
        .collect()
}

/// Where this theme's lines live: one lightness and one chroma, from which
/// each hue departs only by its natural lift.
struct Band {
    centre: f64,
    chroma: f64,
    background: Oklch,
}

impl Band {
    fn of(colours: &[(&str, Oklch)], background: &str) -> Band {
        let background = Oklch::of(background).unwrap_or(Oklch { l: 0.1, c: 0.0, h: 0.0 });
        // Medians over every colour, greys included. A grey theme with one
        // red in it (Solitude) is a quiet theme, and taking the median of only
        // its chromatic colours would make it the loudest palette of all.
        let chroma = median(colours.iter().map(|(_, c)| c.c)).unwrap_or(0.0).clamp(MIN_CHROMA, MAX_CHROMA);
        let wanted = median(colours.iter().map(|(_, c)| c.l)).unwrap_or(0.5);
        let (lo, hi) = legible_range(background, chroma);
        Band { centre: wanted.clamp(lo.min(hi), hi), chroma, background }
    }

    fn dark(&self) -> bool {
        self.background.l < 0.5
    }

    /// The colour a name gets when the theme offers nothing for it.
    fn invent(&self, promise: &Promise) -> Oklch {
        Oklch { l: self.centre + promise.lift, c: self.chroma, h: promise.hue }
    }

    /// A theme colour pulled into the band, keeping its hue.
    fn fit(&self, colour: Oklch, promise: &Promise) -> Oklch {
        let target = self.centre + promise.lift;
        Oklch {
            l: colour.l.clamp(target - LIGHTNESS_SLACK, target + LIGHTNESS_SLACK),
            c: colour.c.clamp(self.chroma * (1.0 - CHROMA_SLACK), self.chroma * (1.0 + CHROMA_SLACK)),
            h: colour.h,
        }
    }

    /// Move `colour` away from the background by `step`; a negative step
    /// moves it toward.
    fn lift(&self, colour: Oklch, step: f64) -> Oklch {
        let l = if self.dark() { colour.l + step } else { colour.l - step };
        colour.with_lightness(l)
    }
}

/// The lightness a band's centre may have on this background: far enough
/// from it that a line at the centre has comfortable contrast, and short of
/// the point where colours stop being colours.
fn legible_range(background: Oklch, chroma: f64) -> (f64, f64) {
    let bg = background.hex();
    let clears = |l: f64| contrast_ratio(&Oklch { l, c: chroma, h: 0.0 }.hex(), &bg) >= CENTRE_CONTRAST;
    let steps = (0..=100).map(|i| i as f64 / 100.0);
    let (floor, ceiling) = (DARKEST + MAX_LIFT, LIGHTEST - MAX_LIFT);
    if background.l < 0.5 {
        let lo = steps.filter(|l| *l > background.l).find(|l| clears(*l)).unwrap_or(ceiling);
        (lo.min(ceiling), ceiling)
    } else {
        let hi = steps.rev().filter(|l| *l < background.l).find(|l| clears(*l)).unwrap_or(floor);
        (floor, hi.max(floor))
    }
}

/// The theme colour that is what this name means, if the theme has one.
///
/// Its own key first, because a theme that calls something "blue" has told us
/// what it thinks blue is. Failing that, anything whose hue is near enough.
/// Either way the colour has to be nearer this name than any other, so Lumon's
/// seven blues go to Blue and not one each to Blue, Cyan and Teal.
fn adopt(promise: &Promise, colours: &[(&str, Oklch)]) -> Option<Oklch> {
    let suits = |c: &Oklch| {
        c.c >= MIN_THEME_CHROMA
            && hue_gap(c.h, promise.hue) <= HUE_TOLERANCE
            && nearest_promise(c.h).name == promise.name
    };
    let by_key = promise
        .keys
        .iter()
        .filter_map(|key| colours.iter().find(|(k, _)| k == key).map(|(_, c)| *c))
        .find(suits);
    by_key.or_else(|| {
        colours
            .iter()
            .map(|(_, c)| *c)
            .filter(suits)
            .min_by(|a, b| hue_gap(a.h, promise.hue).total_cmp(&hue_gap(b.h, promise.hue)))
    })
}

fn nearest_promise(hue: f64) -> &'static Promise {
    PROMISES
        .iter()
        .min_by(|a, b| hue_gap(hue, a.hue).total_cmp(&hue_gap(hue, b.hue)))
        .expect("promises")
}

/// Up and down stay exactly what the theme says they are, as long as that can
/// be seen and the two can be told apart. Candles are the one place the
/// desktop's own colours must come through untouched, since a Gruvbox user
/// knows what Gruvbox green looks like.
fn direction_colour(
    promise: &Promise,
    colours: &[(&str, Oklch)],
    band: &Band,
    placed: &[(&str, Oklch)],
    ground: &Ground,
) -> Oklch {
    let ok = |c: Oklch| clear_of(c, placed) && visible(c, ground);
    let own = promise
        .keys
        .iter()
        .filter_map(|key| colours.iter().find(|(k, _)| k == key).map(|(_, c)| *c))
        .find(|c| ok(*c));
    own.unwrap_or_else(|| {
        let invented = band.invent(promise);
        settle(invented, band, 1.0, ok).unwrap_or_else(|| last_resort(invented, band, ground))
    })
}

/// An overlay colour.
///
/// The theme's own, fitted to the band, when the theme has one near enough
/// to what the name means; moved a little in lightness if that is all it
/// takes to clear the candles, the earlier overlays and the furniture. Nord's
/// blue is also Nord's accent and therefore its crosshair, and the right
/// answer is a slightly lighter Nord blue, not a different blue. Only when a
/// small move will not do does the name's own hue take over, since that hue
/// was chosen to be far from the others; and a promised hue that still
/// collides is one the theme's candles happen to wear, where lightness is
/// all that is left.
fn series_colour(
    promise: &Promise,
    colours: &[(&str, Oklch)],
    band: &Band,
    placed: &[(&str, Oklch)],
    ground: &Ground,
) -> Oklch {
    let ok = |c: Oklch| clear_of(c, placed) && visible(c, ground) && clear_of_furniture(c, ground);
    if let Some(fitted) = adopt(promise, colours).map(|c| band.fit(c, promise))
        && let Some(colour) = settle(fitted, band, FAITHFUL_REACH, ok)
    {
        return colour;
    }
    let invented = band.invent(promise);
    settle(invented, band, 1.0, ok).unwrap_or_else(|| last_resort(invented, band, ground))
}

/// Move a colour's lightness, as little as possible, until `ok` accepts it.
///
/// Smallest moves first, in both directions, never further than `reach` and
/// never past the ends of the usable range. Both directions matter: Rose
/// Pine's up-candle is a blue, and a Cyan overlay driven away from it in the
/// one direction "away from the background" allows ends up a navy no one
/// would call cyan, when a step the other way was just as clear and still
/// legible.
fn settle(colour: Oklch, band: &Band, reach: f64, ok: impl Fn(Oklch) -> bool) -> Option<Oklch> {
    if ok(colour) {
        return Some(colour);
    }
    let usable = |c: &Oklch| (DARKEST..=LIGHTEST).contains(&c.l);
    let steps = (1..).map(|n| n as f64 * 0.02).take_while(|step| *step <= reach);
    for step in steps {
        for candidate in [band.lift(colour, step), band.lift(colour, -step)] {
            if usable(&candidate) && ok(candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

/// Nothing in range works, which means the theme's candles are wearing this
/// hue at every usable lightness. Being seen matters more than being pretty:
/// the far end of the range, nudged until it is legible.
fn last_resort(colour: Oklch, band: &Band, ground: &Ground) -> Oklch {
    let mut last = colour.with_lightness(if band.dark() { LIGHTEST } else { DARKEST });
    for _ in 0..12 {
        if visible(last, ground) {
            break;
        }
        last = band.lift(last, 0.025);
    }
    last
}

fn clear_of(colour: Oklch, placed: &[(&str, Oklch)]) -> bool {
    let hex = colour.hex();
    placed.iter().all(|(_, other)| delta_e(&hex, &other.hex()) >= MIN_SEPARATION)
}

fn visible(colour: Oklch, ground: &Ground) -> bool {
    contrast_ratio(&colour.hex(), ground.background) >= MIN_CONTRAST
}

fn clear_of_furniture(colour: Oklch, ground: &Ground) -> bool {
    let hex = colour.hex();
    [ground.grid, ground.axis, ground.crosshair]
        .iter()
        .all(|f| delta_e(&hex, f) >= MIN_FROM_FURNITURE)
}

fn median(values: impl Iterator<Item = f64>) -> Option<f64> {
    let mut v: Vec<f64> = values.collect();
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    let mid = v.len() / 2;
    Some(if v.len().is_multiple_of(2) { (v[mid - 1] + v[mid]) / 2.0 } else { v[mid] })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_swatch_name_has_a_promise_and_nothing_else_does() {
        for name in SWATCH_NAMES {
            assert!(PROMISES.iter().any(|p| p.name == name), "{name} has no promise");
        }
        assert_eq!(PROMISES.len(), SWATCH_NAMES.len());
    }

    #[test]
    fn the_automatic_hues_are_never_close() {
        // Forty degrees is what the separation floor needs at MIN_CHROMA, with
        // the natural lifts doing the rest. Shrinking this would let a quiet
        // theme produce two lines nobody could tell apart.
        for (i, a) in SWATCH_SEQUENCE.iter().enumerate() {
            for b in &SWATCH_SEQUENCE[i + 1..] {
                let gap = hue_gap(promise(a).hue, promise(b).hue);
                assert!(gap >= 40.0, "{a} and {b} are {gap:.0} degrees apart");
            }
        }
    }

    #[test]
    fn a_theme_colour_near_the_promised_hue_is_adopted() {
        // Nord's blue, which is a real blue.
        let colours = vec![("blue", Oklch::of("#81a1c1").unwrap())];
        let adopted = adopt(promise("Blue"), &colours).expect("adopted");
        assert!((adopted.h - Oklch::of("#81a1c1").unwrap().h).abs() < 0.01);
    }

    #[test]
    fn a_theme_colour_with_the_wrong_hue_is_not_adopted() {
        // Lupine calls a blue "yellow". Amber must not believe it.
        let colours = vec![("yellow", Oklch::of("#026fde").unwrap())];
        assert!(adopt(promise("Amber"), &colours).is_none());
    }

    #[test]
    fn a_colour_goes_to_the_name_it_is_nearest() {
        // A sky blue sits between Cyan and Blue; only one of them may have it.
        let sky = Oklch::of("#88c0d0").unwrap();
        let colours = vec![("blue", sky)];
        let takers: Vec<&str> =
            PROMISES.iter().filter(|p| adopt(p, &colours).is_some()).map(|p| p.name).collect();
        assert_eq!(takers.len(), 1, "{takers:?}");
    }

    #[test]
    fn a_grey_is_never_adopted() {
        let colours = vec![("blue", Oklch::of("#8d8d8d").unwrap())];
        assert!(adopt(promise("Blue"), &colours).is_none());
    }

    #[test]
    fn the_band_clears_the_background() {
        let colours = vec![("blue", Oklch::of("#1a1a2a").unwrap())];
        let band = Band::of(&colours, "#000000");
        let line = band.invent(promise("Teal"));
        assert!(contrast_ratio(&line.hex(), "#000000") >= MIN_CONTRAST, "{}", line.hex());
    }

    #[test]
    fn a_greyscale_theme_still_gets_hues() {
        let keys: HashMap<String, String> = [("red", "#a4a4a4"), ("blue", "#8d8d8d"), ("green", "#b6b6b6")]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let ground = Ground { background: "#000000", grid: "#1a1a1a", axis: "#7a7a7a", crosshair: "#8d8d8d" };
        let swatches = generate(&keys, &ground);
        for a in SWATCH_SEQUENCE {
            for b in SWATCH_SEQUENCE {
                if a == b {
                    continue;
                }
                let (a, b) = (swatch(&swatches, a), swatch(&swatches, b));
                assert!(delta_e(&a, &b) >= MIN_SEPARATION, "{a} vs {b}");
            }
        }
    }

    #[test]
    fn an_empty_theme_yields_a_complete_legible_palette() {
        let ground = Ground { background: "#ffffff", grid: "#f0f0f0", axis: "#808080", crosshair: "#000000" };
        let swatches = generate(&HashMap::new(), &ground);
        assert_eq!(swatches.len(), SWATCH_NAMES.len());
        for s in &swatches {
            assert!(contrast_ratio(&s.hex, "#ffffff") >= MIN_CONTRAST, "{} {}", s.name, s.hex);
        }
    }

    #[test]
    fn a_theme_colour_that_is_also_the_crosshair_keeps_its_hue() {
        // Nord: blue and accent are the same hex, and the accent is the
        // crosshair. The overlay must still be Nord's blue, moved, not a
        // different blue.
        let keys: HashMap<String, String> = [("blue", "#81a1c1"), ("accent", "#81a1c1"), ("red", "#bf616a"), ("green", "#a3be8c")]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let ground = Ground { background: "#2e3440", grid: "#3b4252", axis: "#4c566a", crosshair: "#81a1c1" };
        let blue = swatch(&generate(&keys, &ground), "Blue");
        let (ours, theirs) = (Oklch::of(&blue).unwrap(), Oklch::of("#81a1c1").unwrap());
        assert!(hue_gap(ours.h, theirs.h) < 1.0, "{blue}");
        assert!(delta_e(&blue, "#81a1c1") >= MIN_FROM_FURNITURE, "{blue}");
    }

    #[test]
    fn median_is_the_middle_value() {
        assert_eq!(median([3.0, 1.0, 2.0].into_iter()), Some(2.0));
        assert_eq!(median([4.0, 1.0, 3.0, 2.0].into_iter()), Some(2.5));
        assert_eq!(median(std::iter::empty()), None);
    }

    fn swatch(swatches: &[Swatch], name: &str) -> String {
        swatches.iter().find(|s| s.name == name).unwrap().hex.clone()
    }
}
