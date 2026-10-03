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

pub mod oscillators;
pub mod periods;
pub mod profile;
pub mod vwap;

use serde::{Deserialize, Serialize};

use crate::bars::{Bar, Timeframe};
use crate::theme::{ColorChoice, Theme, SWATCH_SEQUENCE};

pub use periods::Reset;
pub use profile::{Profile, ProfileRow};
pub use vwap::Bands;

/// How a line is drawn.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LineStyle {
    #[default]
    Solid,
    Dashed,
    Dotted,
}

impl LineStyle {
    pub const ALL: [LineStyle; 3] = [LineStyle::Solid, LineStyle::Dashed, LineStyle::Dotted];

    pub fn label(self) -> &'static str {
        match self {
            LineStyle::Solid => "Solid",
            LineStyle::Dashed => "Dashed",
            LineStyle::Dotted => "Dotted",
        }
    }

    /// The dash pattern, scaled to the line's width so a thick dashed line
    /// does not come out as a row of squares.
    pub fn dashes(self, width: f64) -> Vec<f64> {
        let unit = width.max(1.0);
        match self {
            LineStyle::Solid => Vec::new(),
            LineStyle::Dashed => vec![unit * 3.0, unit * 3.0],
            LineStyle::Dotted => vec![unit, unit * 2.0],
        }
    }
}

/// A line's weight and pattern.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct Stroke {
    /// Zero means the line is not drawn at all, which is how a shaded band
    /// gets a clean edge without borrowing the background colour.
    pub width: f64,
    pub style: LineStyle,
}

impl Default for Stroke {
    fn default() -> Stroke {
        Stroke { width: 1.5, style: LineStyle::Solid }
    }
}

impl Stroke {
    pub const fn new(width: f64, style: LineStyle) -> Stroke {
        Stroke { width, style }
    }

    /// Nothing to draw.
    pub fn is_hidden(&self) -> bool {
        self.width <= 0.0
    }
}

/// The indicators we know how to compute.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Sma,
    Ema,
    Vwap,
    VolumeProfile,
    Volume,
    Rsi,
    Atr,
}

impl Kind {
    pub const ALL: [Kind; 7] = [
        Kind::Volume,
        Kind::Sma,
        Kind::Ema,
        Kind::Vwap,
        Kind::VolumeProfile,
        Kind::Rsi,
        Kind::Atr,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Kind::Sma => "Simple Moving Average",
            Kind::Ema => "Exponential Moving Average",
            Kind::Vwap => "VWAP",
            Kind::VolumeProfile => "Volume Profile",
            Kind::Volume => "Volume",
            Kind::Rsi => "Relative Strength Index",
            Kind::Atr => "Average True Range",
        }
    }

    /// What the chart's summary shows. Short, because it sits over the chart.
    pub fn short_name(self) -> &'static str {
        match self {
            Kind::Sma => "SMA",
            Kind::Ema => "EMA",
            Kind::Vwap => "VWAP",
            Kind::VolumeProfile => "VP",
            Kind::Volume => "Vol",
            Kind::Rsi => "RSI",
            Kind::Atr => "ATR",
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
            Kind::Volume => &["volume", "vol", "turnover"],
            Kind::Rsi => &["rsi", "relative strength", "oscillator", "momentum", "overbought"],
            Kind::Atr => &["atr", "average true range", "volatility", "range", "stop"],
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Kind::Sma => "sma",
            Kind::Ema => "ema",
            Kind::Vwap => "vwap",
            Kind::VolumeProfile => "volume_profile",
            Kind::Volume => "volume",
            Kind::Rsi => "rsi",
            Kind::Atr => "atr",
        }
    }

    pub fn default_params(self) -> Params {
        match self {
            Kind::Sma => Params::MovingAverage { period: 50 },
            Kind::Ema => Params::MovingAverage { period: 21 },
            Kind::Vwap => Params::Vwap { reset: Reset::Session, bands: vwap::default_bands() },
            Kind::VolumeProfile => Params::VolumeProfile {
                reset: Reset::Session,
                rows: None,
                value_area: 0.70,
            },
            Kind::Volume => Params::Volume { height: 0.18 },
            // Fourteen bars, overbought at seventy, oversold at thirty: the
            // numbers Wilder published and the ones every other chart shows,
            // so a level somebody is watching elsewhere is the same level here.
            Kind::Rsi => Params::Rsi {
                period: 14,
                height: 0.16,
                overbought: 70.0,
                oversold: 30.0,
            },
            Kind::Atr => Params::Atr { period: 14, height: 0.16 },
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
        /// Three bands, off until asked for.
        bands: Vec<vwap::Band>,
    },
    VolumeProfile {
        reset: Reset,
        /// How many rows each period is divided into, or `None` to let the
        /// instrument decide: a row height snapped to a round multiple of its
        /// own quote increment. A stored number is honoured as it always was,
        /// so a profile somebody tuned by hand stays tuned.
        #[serde(default)]
        rows: Option<usize>,
        /// Fraction of volume the value area covers.
        value_area: f64,
    },
    Volume {
        /// How much of the chart's height the pane takes.
        height: f64,
    },
    Rsi {
        period: usize,
        height: f64,
        overbought: f64,
        oversold: f64,
    },
    Atr {
        period: usize,
        height: f64,
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
    #[serde(default)]
    pub stroke: Stroke,
    pub visible: bool,
}

impl Indicator {
    pub fn new(id: u32, kind: Kind) -> Indicator {
        Indicator {
            id,
            kind,
            params: kind.default_params(),
            color: None,
            stroke: Stroke::default(),
            visible: true,
        }
    }

    /// How the summary row labels it: "SMA 50", "VWAP · Session".
    ///
    /// The stored settings, which is what the settings page should show.
    pub fn label(&self) -> String {
        self.describe(None)
    }

    /// What it is actually doing on this chart.
    ///
    /// A reset period too fine for the timeframe is promoted before computing,
    /// so a session VWAP on a weekly chart is really a quarterly one. The
    /// legend has to say the promoted period or it is describing a chart that
    /// is not on screen.
    pub fn label_for(&self, timeframe: Timeframe) -> String {
        self.describe(Some(timeframe))
    }

    fn describe(&self, timeframe: Option<Timeframe>) -> String {
        let effective = |reset: Reset| match timeframe {
            Some(timeframe) => reset.effective_for(timeframe),
            None => reset,
        };
        match &self.params {
            Params::MovingAverage { period } => format!("{} {period}", self.kind.short_name()),
            Params::Vwap { reset, .. } => format!("VWAP · {}", effective(*reset).label()),
            Params::VolumeProfile { reset, .. } => format!("VP · {}", effective(*reset).label()),
            Params::Volume { .. } => "Volume".to_string(),
            Params::Rsi { period, .. } | Params::Atr { period, .. } => {
                format!("{} {period}", self.kind.short_name())
            }
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

/// The colour each of these indicators draws in.
///
/// Returned for the whole set at once, because avoiding a collision is a
/// property of the set and not of any one member: a second moving average has
/// to know what the first one took. Walking them one at a time is how you end
/// up with two amber lines and no way to tell which is the fifty.
///
/// Three rules:
///
/// * **A colour somebody chose is never moved.** Those are taken first, so an
///   automatic one gives way to a pinned one rather than the other way round.
/// * **Automatic ones take the first sequence colour still free.** Past six
///   they repeat, because the palette has six and a chart with seven overlays
///   has worse problems than a repeated hue.
/// * **Oldest first, by id.** So adding an indicator cannot repaint the ones
///   already on the chart, and neither can reordering them.
pub fn palette_colors(indicators: &[Indicator], theme: &Theme) -> Vec<String> {
    let mut by_age: Vec<usize> = (0..indicators.len()).collect();
    by_age.sort_by_key(|&i| indicators[i].id);

    let mut taken: Vec<String> = indicators
        .iter()
        .filter_map(|indicator| indicator.color.as_ref())
        .map(|choice| choice.resolve(theme))
        .collect();

    let mut out = vec![String::new(); indicators.len()];
    for i in by_age {
        let indicator = &indicators[i];
        if let Some(choice) = &indicator.color {
            out[i] = choice.resolve(theme);
            continue;
        }
        let free = (0..SWATCH_SEQUENCE.len())
            .map(|n| theme.series(n))
            .find(|hex| !taken.contains(hex));
        let colour = free.unwrap_or_else(|| theme.series(taken.len()));
        taken.push(colour.clone());
        out[i] = colour;
    }
    out
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
    /// Volume per bar, with the share of the chart its pane takes.
    Volume { values: Vec<f64>, height: f64 },
    /// A series drawn in its own strip under the price, on its own scale.
    Pane(Pane),
}

/// An indicator that cannot share the price scale, and so gets a strip of its
/// own under the chart.
#[derive(Clone, PartialEq, Debug)]
pub struct Pane {
    pub values: Vec<Option<f64>>,
    /// Share of the chart's height this strip takes.
    pub height: f64,
    /// The scale it always uses, or `None` to fit whatever is on screen.
    ///
    /// An RSI is always 0 to 100 — half its meaning is where the line sits
    /// between them — while an ATR is a price distance with no natural
    /// ceiling, and fitting it is the only way to see its shape.
    pub bounds: Option<(f64, f64)>,
    /// Horizontal reference lines.
    pub guides: Vec<f64>,
    /// The pair of guides worth shading between, if any.
    pub band: Option<(f64, f64)>,
}

impl Output {
    /// The share of the chart's height this indicator wants for its own strip,
    /// if it needs one at all.
    pub fn pane_height(&self) -> Option<f64> {
        match self {
            Output::Volume { height, .. } => Some(*height),
            Output::Pane(pane) => Some(pane.height),
            _ => None,
        }
    }

    /// Resize the strip, for dragging its edge. The computed series is left
    /// alone — only the box it is drawn in changes, so there is nothing to
    /// recompute while the hand is moving.
    pub fn set_pane_height(&mut self, share: f64) {
        let share = share.clamp(0.05, 0.6);
        match self {
            Output::Volume { height, .. } => *height = share,
            Output::Pane(pane) => pane.height = share,
            _ => {}
        }
    }
}

/// Compute an indicator over `bars`.
///
/// `session_origin` is the instrument's session open, which is what makes a
/// session-reset VWAP on futures start in the evening rather than at midnight.
/// `timeframe` lets a reset period that is too fine for the chart be promoted
/// rather than drawn as nonsense.
pub fn compute(
    indicator: &Indicator,
    bars: &[Bar],
    session_origin: i64,
    timeframe: Timeframe,
    kind: Option<crate::symbols::InstrumentKind>,
) -> Output {
    match (&indicator.kind, &indicator.params) {
        (Kind::Sma, Params::MovingAverage { period }) => Output::Line(sma(bars, *period)),
        (Kind::Ema, Params::MovingAverage { period }) => Output::Line(ema(bars, *period)),
        (Kind::Vwap, Params::Vwap { reset, bands }) => Output::Bands(vwap::compute(
            bars,
            reset.effective_for(timeframe),
            session_origin,
            bands,
        )),
        (Kind::Rsi, Params::Rsi { period, height, overbought, oversold }) => Output::Pane(Pane {
            values: oscillators::rsi(bars, *period),
            height: height.clamp(0.05, 0.6),
            bounds: Some((0.0, 100.0)),
            guides: vec![*oversold, 50.0, *overbought],
            band: Some((*oversold, *overbought)),
        }),
        (Kind::Atr, Params::Atr { period, height }) => Output::Pane(Pane {
            values: oscillators::atr(bars, *period),
            height: height.clamp(0.05, 0.6),
            bounds: None,
            guides: Vec::new(),
            band: None,
        }),
        (Kind::Volume, Params::Volume { height }) => Output::Volume {
            values: bars.iter().map(|bar| bar.volume).collect(),
            height: height.clamp(0.05, 0.6),
        },
        (Kind::VolumeProfile, Params::VolumeProfile { reset, rows, value_area }) => {
            Output::Profiles(profile::compute(
                bars,
                reset.effective_for(timeframe),
                session_origin,
                *rows,
                *value_area,
                kind,
            ))
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
    fn volume_is_an_indicator_like_any_other() {
        let volume = Indicator::new(1, Kind::Volume);
        assert_eq!(volume.label(), "Volume");
        assert_eq!(volume.kind.short_name(), "Vol");
        assert!(volume.visible);

        let series = bars(&[1.0, 2.0, 3.0]);
        match compute(&volume, &series, 0, Timeframe::days(1), None) {
            Output::Volume { values, height } => {
                assert_eq!(values, vec![100.0, 100.0, 100.0]);
                assert!((0.05..=0.6).contains(&height));
            }
            other => panic!("expected a volume pane, got {other:?}"),
        }
    }

    #[test]
    fn an_absurd_pane_height_is_brought_back_into_range() {
        let mut volume = Indicator::new(1, Kind::Volume);
        volume.params = Params::Volume { height: 5.0 };
        match compute(&volume, &bars(&[1.0, 2.0]), 0, Timeframe::days(1), None) {
            Output::Volume { height, .. } => assert_eq!(height, 0.6),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_picker_offers_volume_first() {
        // It is the one almost every chart wants, so it leads the list.
        assert_eq!(Kind::ALL[0], Kind::Volume);
        assert_eq!(search("vol")[0], Kind::Volume);
    }

    #[test]
    fn labels_say_what_the_indicator_is() {
        assert_eq!(Indicator::new(1, Kind::Sma).label(), "SMA 50");
        assert_eq!(Indicator::new(2, Kind::Ema).label(), "EMA 21");
        assert_eq!(Indicator::new(3, Kind::Vwap).label(), "VWAP · Session");
        assert_eq!(Indicator::new(4, Kind::VolumeProfile).label(), "VP · Session");
    }

    #[test]
    fn the_legend_names_the_period_actually_in_use() {
        let vwap = Indicator::new(1, Kind::Vwap);
        // Stored as a session reset, and that is what settings should show.
        assert_eq!(vwap.label(), "VWAP · Session");
        // But a session is one bar on a weekly chart, so it is promoted — and
        // saying "Session" there would describe a chart nobody is looking at.
        assert_eq!(vwap.label_for(Timeframe::weeks(1)), "VWAP · Quarter");
        assert_eq!(vwap.label_for(Timeframe::days(1)), "VWAP · Month");
        // Intraday, nothing is promoted and the two agree.
        assert_eq!(vwap.label_for(Timeframe::minutes(5)), "VWAP · Session");
    }

    #[test]
    fn a_moving_average_reads_the_same_everywhere() {
        let sma = Indicator::new(1, Kind::Sma);
        assert_eq!(sma.label(), "SMA 50");
        assert_eq!(sma.label_for(Timeframe::weeks(1)), "SMA 50");
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

    /// Five of the same indicator, which is the case the sequence exists for:
    /// a second moving average that comes out the colour of the first is two
    /// lines you cannot tell apart.
    #[test]
    fn repeating_an_indicator_takes_the_next_colour_along() {
        let theme = &crate::theme::builtin_themes()[0];
        let set: Vec<Indicator> = (1..=5).map(|id| Indicator::new(id, Kind::Sma)).collect();
        let colors = palette_colors(&set, theme);
        let mut unique = colors.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), 5, "five averages, five colours: {colors:?}");
        assert_eq!(colors[0], theme.series(0), "the first still starts the sequence");
    }

    #[test]
    fn a_colour_somebody_chose_is_never_handed_to_anybody_else() {
        let theme = &crate::theme::builtin_themes()[0];
        // The second indicator is pinned to the colour the first would have
        // taken automatically. The first has to move, not the pinned one.
        let mut set: Vec<Indicator> = (1..=3).map(|id| Indicator::new(id, Kind::Ema)).collect();
        let first = theme.series(0);
        set[1].color = Some(ColorChoice::Fixed { hex: first.clone() });

        let colors = palette_colors(&set, theme);
        assert_eq!(colors[1], first, "the pinned one keeps what it was given");
        assert_ne!(colors[0], first, "the automatic one gives way");
        assert_ne!(colors[2], first);
        assert_ne!(colors[0], colors[2]);
    }

    #[test]
    fn adding_or_reordering_does_not_repaint_what_is_already_there() {
        let theme = &crate::theme::builtin_themes()[0];
        let mut set: Vec<Indicator> = (1..=3).map(|id| Indicator::new(id, Kind::Sma)).collect();
        let before = palette_colors(&set, theme);

        set.push(Indicator::new(4, Kind::Vwap));
        let after_adding = palette_colors(&set, theme);
        assert_eq!(&after_adding[..3], &before[..], "the three already drawn keep their colours");

        // Same indicators, listed the other way round: colours follow the
        // indicator, not the row it happens to sit in.
        set.reverse();
        let reordered = palette_colors(&set, theme);
        for (indicator, colour) in set.iter().zip(&reordered) {
            let was = &after_adding[(indicator.id - 1) as usize];
            assert_eq!(colour, was, "indicator {} changed colour on reorder", indicator.id);
        }
    }

    #[test]
    fn past_the_palette_colours_repeat_rather_than_running_out() {
        let theme = &crate::theme::builtin_themes()[0];
        let set: Vec<Indicator> = (1..=9).map(|id| Indicator::new(id, Kind::Sma)).collect();
        let colors = palette_colors(&set, theme);
        assert_eq!(colors.len(), 9);
        assert!(colors.iter().all(|c| !c.is_empty()), "every one gets a colour");
    }

    #[test]
    fn computing_every_indicator_is_cheap_enough_to_do_on_every_repaint() {
        // Two years of hourly bars, which is the most this app ever holds.
        let series: Vec<Bar> = (0..12_000)
            .map(|i| {
                let price = 100.0 + (i as f64 / 50.0).sin() * 5.0;
                Bar {
                    ts: i as i64 * 3_600,
                    open: price,
                    high: price + 0.5,
                    low: price - 0.5,
                    close: price + 0.1,
                    volume: 1_000.0,
                }
            })
            .collect();

        let indicators: Vec<Indicator> =
            Kind::ALL.into_iter().enumerate().map(|(i, k)| Indicator::new(i as u32, k)).collect();

        let start = std::time::Instant::now();
        let rounds = 20;
        for _ in 0..rounds {
            for indicator in &indicators {
                let _ = compute(indicator, &series, 0, Timeframe::hours(1), None);
            }
        }
        let per_repaint = start.elapsed() / rounds;

        // Caching these would mean storing and invalidating derived values to
        // save a few milliseconds a repaint. The budget here is deliberately
        // loose; it exists to catch an indicator that becomes quadratic.
        assert!(
            per_repaint < std::time::Duration::from_millis(50),
            "all four indicators over 12k bars took {per_repaint:?}"
        );
        eprintln!("all indicators over 12k bars: {per_repaint:?} per repaint");
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
