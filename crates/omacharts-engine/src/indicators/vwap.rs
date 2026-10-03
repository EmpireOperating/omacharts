//! Volume-weighted average price, with standard-deviation bands.
//!
//! Accumulates within a reset period and starts over at the next one. The
//! bands come from the volume-weighted variance of typical price around the
//! running VWAP, which is the usual construction and the one that makes the
//! first band mean "a normal distance from fair value today".

use crate::bars::Bar;

use super::periods::Reset;

#[derive(Clone, PartialEq, Debug, Default)]
pub struct Bands {
    pub vwap: Vec<Option<f64>>,
    /// One series per deviation, innermost first.
    pub upper: Vec<Vec<Option<f64>>>,
    pub lower: Vec<Vec<Option<f64>>>,
    pub deviations: Vec<f64>,
}

/// How one band is drawn.
///
/// Only the first band is shaded, as a single region from its lower edge to
/// its upper one — the area price spends most of its time in. The outer bands
/// are lines alone: shading those as well stacks into a wash that says less
/// than the lines do.
///
/// The first band's own edges are drawn in the chart background rather than in
/// the band colour, so the shaded area ends cleanly instead of being boxed in
/// by two lines competing with the ones further out.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct BandStyle {
    pub line_alpha: f64,
    /// Shading between this band's edges. Zero for every band but the first.
    pub fill_alpha: f64,
    pub dashed: bool,
    /// Draw the edge in the chart's background colour rather than the band's.
    pub edge_is_background: bool,
}

pub fn band_style(index: usize) -> BandStyle {
    match index {
        // The shaded band. Its edges disappear into the chart.
        0 => BandStyle {
            line_alpha: 1.0,
            fill_alpha: 0.16,
            dashed: false,
            edge_is_background: true,
        },
        // The middle band is dashed, so it reads as a marker rather than a
        // boundary.
        1 => BandStyle {
            line_alpha: 0.75,
            fill_alpha: 0.0,
            dashed: true,
            edge_is_background: false,
        },
        // Everything further out is a plain line.
        _ => BandStyle {
            line_alpha: 0.75,
            fill_alpha: 0.0,
            dashed: false,
            edge_is_background: false,
        },
    }
}

/// Typical price — the midpoint the weighting is applied to.
fn typical(bar: &Bar) -> f64 {
    (bar.high + bar.low + bar.close) / 3.0
}

pub fn compute(
    bars: &[Bar],
    reset: Reset,
    session_origin: i64,
    deviations: &[f64],
) -> Bands {
    let n = bars.len();
    let mut out = Bands {
        vwap: vec![None; n],
        upper: vec![vec![None; n]; deviations.len()],
        lower: vec![vec![None; n]; deviations.len()],
        deviations: deviations.to_vec(),
    };
    if n == 0 {
        return out;
    }

    let mut bucket = i64::MIN;
    let (mut sum_w, mut sum_wp, mut sum_wp2) = (0.0f64, 0.0f64, 0.0f64);

    for (i, bar) in bars.iter().enumerate() {
        let this_bucket = reset.bucket(bar.ts, session_origin);
        if this_bucket != bucket {
            bucket = this_bucket;
            sum_w = 0.0;
            sum_wp = 0.0;
            sum_wp2 = 0.0;
        }

        // Indexes report no volume at all. Weighting every bar equally keeps
        // the line meaningful there instead of producing nothing; with real
        // volume this has no effect.
        let weight = if bar.volume > 0.0 { bar.volume } else { 1.0 };
        let price = typical(bar);

        sum_w += weight;
        sum_wp += weight * price;
        sum_wp2 += weight * price * price;

        if sum_w <= 0.0 {
            continue;
        }
        let vwap = sum_wp / sum_w;
        out.vwap[i] = Some(vwap);

        // Floating point can push this a hair below zero on a flat period.
        let variance = (sum_wp2 / sum_w - vwap * vwap).max(0.0);
        let deviation = variance.sqrt();
        for (b, multiple) in deviations.iter().enumerate() {
            out.upper[b][i] = Some(vwap + deviation * multiple);
            out.lower[b][i] = Some(vwap - deviation * multiple);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(ts: i64, price: f64, volume: f64) -> Bar {
        Bar { ts, open: price, high: price, low: price, close: price, volume }
    }

    const DAY: i64 = 86_400;

    #[test]
    fn vwap_of_one_price_is_that_price() {
        let bars = vec![bar(0, 10.0, 5.0), bar(60, 10.0, 7.0)];
        let out = compute(&bars, Reset::Session, 0, &[1.0]);
        assert_eq!(out.vwap, vec![Some(10.0), Some(10.0)]);
        // No dispersion, so the bands sit on the line.
        assert_eq!(out.upper[0][1], Some(10.0));
        assert_eq!(out.lower[0][1], Some(10.0));
    }

    #[test]
    fn vwap_is_weighted_by_volume() {
        // 10 on 1 lot and 20 on 3 lots averages to 17.5, not 15.
        let bars = vec![bar(0, 10.0, 1.0), bar(60, 20.0, 3.0)];
        let out = compute(&bars, Reset::Session, 0, &[]);
        assert_eq!(out.vwap[1], Some(17.5));
    }

    #[test]
    fn it_starts_over_at_the_period_boundary() {
        let bars = vec![
            bar(0, 10.0, 1.0),
            bar(3600, 10.0, 1.0),
            // Next day: the previous day's prices must not carry over.
            bar(DAY, 50.0, 1.0),
        ];
        let out = compute(&bars, Reset::Session, 0, &[]);
        assert_eq!(out.vwap[1], Some(10.0));
        assert_eq!(out.vwap[2], Some(50.0), "a new session starts clean");
    }

    #[test]
    fn the_session_origin_moves_the_boundary() {
        let origin = 79_200; // 18:00 ET
        // Two bars either side of midnight belong to the same session.
        let bars = vec![bar(DAY - 600, 10.0, 1.0), bar(DAY + 600, 20.0, 1.0)];
        let same_session = compute(&bars, Reset::Session, origin, &[]);
        assert_eq!(same_session.vwap[1], Some(15.0), "one session, so it averages");

        // With no origin they are two different days.
        let split = compute(&bars, Reset::Session, 0, &[]);
        assert_eq!(split.vwap[1], Some(20.0), "a fresh day starts clean");
    }

    #[test]
    fn bands_widen_with_dispersion() {
        let bars = vec![bar(0, 10.0, 1.0), bar(60, 20.0, 1.0), bar(120, 30.0, 1.0)];
        let out = compute(&bars, Reset::Session, 0, &[1.0, 2.0]);
        let vwap = out.vwap[2].unwrap();
        let one = out.upper[0][2].unwrap();
        let two = out.upper[1][2].unwrap();
        assert!(one > vwap, "a band sits above the line");
        assert!((two - vwap) > (one - vwap), "two deviations is wider than one");
        // And symmetric.
        assert!(((vwap - out.lower[0][2].unwrap()) - (one - vwap)).abs() < 1e-9);
    }

    #[test]
    fn a_volumeless_instrument_still_gets_a_line() {
        // Indexes report no volume; equal weighting keeps VWAP meaningful.
        let bars = vec![bar(0, 10.0, 0.0), bar(60, 20.0, 0.0)];
        let out = compute(&bars, Reset::Session, 0, &[]);
        assert_eq!(out.vwap[1], Some(15.0));
    }

    #[test]
    fn variance_never_goes_negative_on_a_flat_period() {
        // Large prices with no movement are where the naive variance formula
        // goes slightly negative and the square root produces NaN.
        let bars: Vec<Bar> = (0..50).map(|i| bar(i * 60, 48_000.125, 3.0)).collect();
        let out = compute(&bars, Reset::Session, 0, &[1.0]);
        for value in out.upper[0].iter().flatten() {
            assert!(value.is_finite(), "band went non-finite: {value}");
        }
    }

    #[test]
    fn empty_input_produces_empty_output() {
        let out = compute(&[], Reset::Session, 0, &[1.0]);
        assert!(out.vwap.is_empty());
        assert_eq!(out.upper.len(), 1);
        assert!(out.upper[0].is_empty());
    }

    #[test]
    fn only_the_first_band_is_shaded() {
        assert!(band_style(0).fill_alpha > 0.0);
        for index in 1..6 {
            assert_eq!(band_style(index).fill_alpha, 0.0, "band {index} should be a line only");
        }
    }

    #[test]
    fn the_shaded_bands_edges_disappear_into_the_chart() {
        assert!(band_style(0).edge_is_background);
        assert!(!band_style(1).edge_is_background);
        assert!(!band_style(2).edge_is_background);
    }

    #[test]
    fn the_middle_band_is_dashed_and_the_outer_one_is_not() {
        assert!(!band_style(0).dashed, "the shaded band has no dashes to show");
        assert!(band_style(1).dashed);
        assert!(!band_style(2).dashed);
    }

    #[test]
    fn every_band_is_drawn_at_all() {
        for index in 0..6 {
            assert!(band_style(index).line_alpha > 0.0, "band {index}");
        }
    }
}
