//! Indicators.
//!
//! Everything here is a pure function of bars plus parameters, which is what
//! makes it testable without a window and cheap to recompute on every repaint.
//! The renderer receives values and a style, and does no arithmetic of its own.
//!
//! Colour is not stored by default. An indicator carries a [`ColorChoice`],
//! and an unset one resolves through the active theme's palette in sequence —
//! so adding four indicators gives four colours that already work together,
//! in any theme, including one the user edited.

pub mod periods;
pub mod profile;
pub mod vwap;

use serde::{Deserialize, Serialize};

use crate::bars::Bar;
use crate::theme::{ColorChoice, Theme};

pub use periods::Reset;
pub use profile::{Profile, ProfileRow};
pub use vwap::Bands;

/// The indicators we know how to compute.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Sma,
    Ema,
    Vwap,
    VolumeProfile,
}

impl Kind {
    pub const ALL: [Kind; 4] = [Kind::Sma, Kind::Ema, Kind::Vwap, Kind::VolumeProfile];

    pub fn name(self) -> &'static str {
        match self {
            Kind::Sma => "Simple Moving Average",
            Kind::Ema => "Exponential Moving Average",
            Kind::Vwap => "VWAP",
            Kind::VolumeProfile => "Volume Profile",
        }
    }

    /// What the chart's summary shows. Short, because it sits over the chart.
    pub fn short_name(self) -> &'static str {
        match self {
            Kind::Sma => "SMA",
            Kind::Ema => "EMA",
            Kind::Vwap => "VWAP",
            Kind::VolumeProfile => "VP",
        }
    }

    /// Searched alongside the name, so "moving average" and "ma" both find
    /// both averages, and "poc" finds the profile.
    pub fn aliases(self) -> &'static [&'static str] {
        match self {
            Kind::Sma => &["sma", "ma", "moving average", "mean"],
            Kind::Ema => &["ema", "ma", "moving average", "exponential"],
            Kind::Vwap => &["vwap", "volume weighted", "average price", "bands"],
            Kind::VolumeProfile => &["volume profile", "vp", "poc", "value area", "tpo"],
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Kind::Sma => "sma",
            Kind::Ema => "ema",
            Kind::Vwap => "vwap",
            Kind::VolumeProfile => "volume_profile",
        }
    }

    pub fn default_params(self) -> Params {
        match self {
            Kind::Sma => Params::MovingAverage { period: 50 },
            Kind::Ema => Params::MovingAverage { period: 21 },
            Kind::Vwap => Params::Vwap {
                reset: Reset::Session,
                deviations: vec![1.0, 1.5, 2.0],
            },
            Kind::VolumeProfile => Params::VolumeProfile {
                reset: Reset::Session,
                rows: 48,
                value_area: 0.70,
            },
        }
    }
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Params {
    MovingAverage {
        period: usize,
    },
    Vwap {
        reset: Reset,
        /// Standard deviations the bands sit at.
        deviations: Vec<f64>,
    },
    VolumeProfile {
        reset: Reset,
        rows: usize,
        /// Fraction of volume the value area covers.
        value_area: f64,
    },
}

/// One indicator on a chart.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Indicator {
    /// Stable for the life of the chart, so the summary row and the drawing
    /// agree on which one you clicked.
    pub id: u32,
    pub kind: Kind,
    pub params: Params,
    /// Unset means "whatever the palette offers for my slot".
    pub color: Option<ColorChoice>,
    pub visible: bool,
}

impl Indicator {
    pub fn new(id: u32, kind: Kind) -> Indicator {
        Indicator { id, kind, params: kind.default_params(), color: None, visible: true }
    }

    /// How the summary row labels it: "SMA 50", "VWAP · Session".
    pub fn label(&self) -> String {
        match &self.params {
            Params::MovingAverage { period } => format!("{} {period}", self.kind.short_name()),
            Params::Vwap { reset, .. } => format!("VWAP · {}", reset.label()),
            Params::VolumeProfile { reset, .. } => format!("VP · {}", reset.label()),
        }
    }

    /// The line colour, resolved against the theme.
    ///
    /// `slot` is this indicator's position among those on the chart, which is
    /// what makes a fresh set of indicators come out in sequence rather than
    /// all wearing the accent.
    pub fn color(&self, theme: &Theme, slot: usize) -> String {
        match &self.color {
            Some(choice) => choice.resolve(theme),
            None => theme.series(slot),
        }
    }
}

/// What computing an indicator produces.
#[derive(Clone, PartialEq, Debug)]
pub enum Output {
    /// One value per bar. `None` where there is not enough history yet —
    /// never a fabricated number.
    Line(Vec<Option<f64>>),
    Bands(Bands),
    /// One profile per reset period.
    Profiles(Vec<Profile>),
}

/// Compute an indicator over `bars`.
///
/// `session_origin` is the instrument's session open, which is what makes a
/// session-reset VWAP on futures start in the evening rather than at midnight.
pub fn compute(indicator: &Indicator, bars: &[Bar], session_origin: i64) -> Output {
    match (&indicator.kind, &indicator.params) {
        (Kind::Sma, Params::MovingAverage { period }) => Output::Line(sma(bars, *period)),
        (Kind::Ema, Params::MovingAverage { period }) => Output::Line(ema(bars, *period)),
        (Kind::Vwap, Params::Vwap { reset, deviations }) => {
            Output::Bands(vwap::compute(bars, *reset, session_origin, deviations))
        }
        (Kind::VolumeProfile, Params::VolumeProfile { reset, rows, value_area }) => {
            Output::Profiles(profile::compute(bars, *reset, session_origin, *rows, *value_area))
        }
        // Params and kind are set together; a mismatch means a stored
        // indicator from a future version. Draw nothing rather than guess.
        _ => Output::Line(vec![None; bars.len()]),
    }
}

/// Simple moving average of closes.
pub fn sma(bars: &[Bar], period: usize) -> Vec<Option<f64>> {
    let mut out = vec![None; bars.len()];
    if period == 0 || bars.len() < period {
        return out;
    }
    let mut sum = 0.0;
    for (i, bar) in bars.iter().enumerate() {
        sum += bar.close;
        if i >= period {
            sum -= bars[i - period].close;
        }
        if i + 1 >= period {
            out[i] = Some(sum / period as f64);
        }
    }
    out
}

/// Exponential moving average of closes, seeded with the simple average of
/// the first `period` bars so it does not start from an arbitrary value.
pub fn ema(bars: &[Bar], period: usize) -> Vec<Option<f64>> {
    let mut out = vec![None; bars.len()];
    if period == 0 || bars.len() < period {
        return out;
    }
    let alpha = 2.0 / (period as f64 + 1.0);
    let seed: f64 = bars[..period].iter().map(|b| b.close).sum::<f64>() / period as f64;
    let mut value = seed;
    out[period - 1] = Some(value);
    for (i, bar) in bars.iter().enumerate().skip(period) {
        value = bar.close * alpha + value * (1.0 - alpha);
        out[i] = Some(value);
    }
    out
}

/// A search over the indicators, for the picker.
///
/// Same shape as the symbol search: everything is in memory, so a keystroke
/// re-runs it.
pub fn search(query: &str) -> Vec<Kind> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Kind::ALL.to_vec();
    }
    let mut scored: Vec<(i32, Kind)> = Kind::ALL
        .into_iter()
        .filter_map(|kind| {
            let name = kind.name().to_lowercase();
            let short = kind.short_name().to_lowercase();
            let score = if short == q {
                100
            } else if kind.aliases().iter().any(|a| *a == q) {
                90
            } else if short.starts_with(&q) || name.starts_with(&q) {
                70
            } else if kind.aliases().iter().any(|a| a.starts_with(&q)) {
                60
            } else if name.contains(&q) || kind.aliases().iter().any(|a| a.contains(&q)) {
                40
            } else {
                return None;
            };
            Some((score, kind))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0));
    scored.into_iter().map(|(_, kind)| kind).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bars(closes: &[f64]) -> Vec<Bar> {
        closes
            .iter()
            .enumerate()
            .map(|(i, &c)| Bar {
                ts: i as i64 * 86_400,
                open: c,
                high: c + 1.0,
                low: c - 1.0,
                close: c,
                volume: 100.0,
            })
            .collect()
    }

    #[test]
    fn sma_has_no_value_until_it_has_enough_bars() {
        let out = sma(&bars(&[1.0, 2.0, 3.0, 4.0]), 3);
        assert_eq!(out, vec![None, None, Some(2.0), Some(3.0)]);
    }

    #[test]
    fn sma_of_a_flat_series_is_the_level() {
        let out = sma(&bars(&[5.0; 10]), 4);
        assert!(out[3..].iter().all(|v| *v == Some(5.0)));
    }

    #[test]
    fn sma_handles_degenerate_inputs() {
        assert!(sma(&bars(&[1.0, 2.0]), 5).iter().all(Option::is_none));
        assert!(sma(&bars(&[1.0]), 0).iter().all(Option::is_none));
        assert!(sma(&[], 3).is_empty());
    }

    #[test]
    fn ema_is_seeded_with_the_simple_average() {
        let out = ema(&bars(&[1.0, 2.0, 3.0, 4.0]), 3);
        assert_eq!(out[0], None);
        assert_eq!(out[1], None);
        assert_eq!(out[2], Some(2.0), "seeded with the mean of the first three");

        // Then it follows: 4 * 0.5 + 2 * 0.5 = 3.
        assert_eq!(out[3], Some(3.0));
    }

    #[test]
    fn ema_tracks_a_flat_series_exactly() {
        let out = ema(&bars(&[7.0; 20]), 5);
        for value in out[4..].iter() {
            assert!((value.unwrap() - 7.0).abs() < 1e-9);
        }
    }

    #[test]
    fn ema_reacts_faster_than_sma() {
        // A step up: the exponential average should be above the simple one.
        let mut closes = vec![10.0; 30];
        closes.extend(vec![20.0; 5]);
        let series = bars(&closes);
        let fast = ema(&series, 10).last().unwrap().unwrap();
        let slow = sma(&series, 10).last().unwrap().unwrap();
        assert!(fast > slow, "ema {fast} should lead sma {slow}");
    }

    #[test]
    fn labels_say_what_the_indicator_is() {
        assert_eq!(Indicator::new(1, Kind::Sma).label(), "SMA 50");
        assert_eq!(Indicator::new(2, Kind::Ema).label(), "EMA 21");
        assert_eq!(Indicator::new(3, Kind::Vwap).label(), "VWAP · Session");
        assert_eq!(Indicator::new(4, Kind::VolumeProfile).label(), "VP · Session");
    }

    #[test]
    fn a_fresh_set_of_indicators_comes_out_in_palette_sequence() {
        let theme = &crate::theme::builtin_themes()[0];
        let colors: Vec<String> = (0..4)
            .map(|slot| Indicator::new(slot as u32, Kind::Sma).color(theme, slot))
            .collect();
        let mut unique = colors.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), 4, "four indicators, four colours: {colors:?}");
        assert_eq!(colors[0], theme.series(0));
    }

    #[test]
    fn an_explicit_colour_overrides_the_palette_slot() {
        let theme = &crate::theme::builtin_themes()[0];
        let mut indicator = Indicator::new(1, Kind::Sma);
        indicator.color = Some(ColorChoice::swatch("Rose"));
        assert_eq!(indicator.color(theme, 0), theme.swatch("Rose").unwrap().hex);
    }

    #[test]
    fn the_picker_finds_things_by_name_abbreviation_and_concept() {
        assert_eq!(search("sma")[0], Kind::Sma);
        assert_eq!(search("vwap")[0], Kind::Vwap);
        assert_eq!(search("poc")[0], Kind::VolumeProfile);
        assert_eq!(search("volume profile")[0], Kind::VolumeProfile);
        assert_eq!(search("exponential")[0], Kind::Ema);

        // "ma" is an alias of both averages and should surface them together.
        let mas = search("ma");
        assert!(mas.contains(&Kind::Sma) && mas.contains(&Kind::Ema));

        assert_eq!(search("").len(), Kind::ALL.len());
        assert!(search("zzzz").is_empty());
    }
}
