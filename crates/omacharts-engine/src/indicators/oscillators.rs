//! Indicators that need their own pane: they are not prices, so they cannot
//! share the price scale.
//!
//! Both of these are Wilder's, and both are computed the way TradingView
//! computes them, because a number that disagrees with the chart everybody
//! else is looking at is worse than no number. Wilder's smoothing is an
//! exponential average with alpha `1/period` rather than `2/(period+1)`, seeded
//! with the simple average of the first `period` values — which is why an RSI
//! here matches one there bar for bar rather than drifting apart over a long
//! series.

use crate::bars::Bar;

/// Wilder's smoothing, seeded with the simple average of the first `period`.
///
/// `None` until there is enough history, never a number made up to fill the
/// gap: a chart that draws an RSI from bar one is drawing a guess.
fn wilder(values: &[f64], period: usize) -> Vec<Option<f64>> {
    let mut out = vec![None; values.len()];
    if period == 0 || values.len() < period {
        return out;
    }
    let mut average: f64 = values[..period].iter().sum::<f64>() / period as f64;
    out[period - 1] = Some(average);
    for (i, value) in values.iter().enumerate().skip(period) {
        average += (value - average) / period as f64;
        out[i] = Some(average);
    }
    out
}

/// Relative strength index, 0 to 100.
///
/// The first bar has no change to measure, so everything is shifted one place:
/// the first reading lands on bar `period`, not `period - 1`.
pub fn rsi(bars: &[Bar], period: usize) -> Vec<Option<f64>> {
    let mut out = vec![None; bars.len()];
    if period == 0 || bars.len() <= period {
        return out;
    }

    let mut gains = Vec::with_capacity(bars.len() - 1);
    let mut losses = Vec::with_capacity(bars.len() - 1);
    for pair in bars.windows(2) {
        let change = pair[1].close - pair[0].close;
        gains.push(change.max(0.0));
        losses.push((-change).max(0.0));
    }

    let average_gain = wilder(&gains, period);
    let average_loss = wilder(&losses, period);
    for i in 0..gains.len() {
        let (Some(gain), Some(loss)) = (average_gain[i], average_loss[i]) else { continue };
        // A run with no down closes at all is RSI 100, not a division by zero.
        out[i + 1] = Some(if loss <= f64::EPSILON {
            100.0
        } else {
            100.0 - 100.0 / (1.0 + gain / loss)
        });
    }
    out
}

/// Average true range, in the instrument's own price units.
pub fn atr(bars: &[Bar], period: usize) -> Vec<Option<f64>> {
    let mut out = vec![None; bars.len()];
    if period == 0 || bars.is_empty() {
        return out;
    }

    // The first bar has no previous close, so its true range is just its own
    // range. Dropping it instead would shift every later value by one.
    let mut ranges = Vec::with_capacity(bars.len());
    ranges.push(bars[0].high - bars[0].low);
    for pair in bars.windows(2) {
        let (previous, bar) = (&pair[0], &pair[1]);
        ranges.push(
            (bar.high - bar.low)
                .max((bar.high - previous.close).abs())
                .max((bar.low - previous.close).abs()),
        );
    }

    for (i, value) in wilder(&ranges, period).into_iter().enumerate() {
        out[i] = value;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bars(closes: &[f64]) -> Vec<Bar> {
        closes
            .iter()
            .enumerate()
            .map(|(i, &c)| Bar {
                ts: i as i64 * 60,
                open: c,
                high: c + 1.0,
                low: c - 1.0,
                close: c,
                volume: 100.0,
            })
            .collect()
    }

    #[test]
    fn rsi_waits_for_enough_history() {
        let series = bars(&[1.0, 2.0, 3.0, 4.0, 5.0]);
        let values = rsi(&series, 14);
        assert!(values.iter().all(Option::is_none), "no reading without a full period");

        let series = bars(&(0..30).map(|i| 100.0 + i as f64).collect::<Vec<_>>());
        let values = rsi(&series, 14);
        assert!(values[13].is_none(), "the first change is on bar 1, so the first reading is 14");
        assert!(values[14].is_some());
    }

    #[test]
    fn rsi_is_a_hundred_when_nothing_falls() {
        let series = bars(&(0..40).map(|i| 100.0 + i as f64).collect::<Vec<_>>());
        let last = rsi(&series, 14).pop().flatten().unwrap();
        assert!((last - 100.0).abs() < 1e-9, "{last}");
    }

    #[test]
    fn rsi_is_zero_when_nothing_rises() {
        let series = bars(&(0..40).map(|i| 200.0 - i as f64).collect::<Vec<_>>());
        let last = rsi(&series, 14).pop().flatten().unwrap();
        assert!(last.abs() < 1e-9, "{last}");
    }

    #[test]
    fn rsi_sits_at_the_midpoint_when_gains_and_losses_match() {
        // Alternating equal up and down closes: no side has the upper hand.
        let closes: Vec<f64> = (0..60).map(|i| if i % 2 == 0 { 100.0 } else { 101.0 }).collect();
        // Not exactly fifty: the series ends on an up bar, and Wilder's
        // average still carries that. Close to it is the claim.
        let last = rsi(&bars(&closes), 14).pop().flatten().unwrap();
        assert!((last - 50.0).abs() < 3.0, "{last}");
    }

    #[test]
    fn rsi_stays_inside_its_scale() {
        let closes: Vec<f64> =
            (0..200).map(|i| 100.0 + (i as f64 * 0.7).sin() * 12.0 + i as f64 * 0.1).collect();
        for value in rsi(&bars(&closes), 14).into_iter().flatten() {
            assert!((0.0..=100.0).contains(&value), "{value}");
        }
    }

    #[test]
    fn atr_of_a_steady_range_is_that_range() {
        // Every bar two wide, no gaps: the average true range is two.
        let series = bars(&vec![100.0; 40]);
        let last = atr(&series, 14).pop().flatten().unwrap();
        assert!((last - 2.0).abs() < 1e-9, "{last}");
    }

    #[test]
    fn atr_counts_the_gap_not_just_the_bar() {
        // Same two-wide bars, but one opens ten above the last close. True
        // range has to notice; the bar's own high minus low does not.
        let mut series = bars(&vec![100.0; 40]);
        for bar in series.iter_mut().skip(20) {
            bar.open += 10.0;
            bar.high += 10.0;
            bar.low += 10.0;
            bar.close += 10.0;
        }
        let gapped = atr(&series, 14).pop().flatten().unwrap();
        let flat = atr(&bars(&vec![100.0; 40]), 14).pop().flatten().unwrap();
        assert!(gapped > flat, "{gapped} should exceed {flat}");
    }

    #[test]
    fn atr_is_never_negative() {
        let closes: Vec<f64> = (0..120).map(|i| 50.0 + (i as f64 * 0.3).cos() * 8.0).collect();
        for value in atr(&bars(&closes), 14).into_iter().flatten() {
            assert!(value >= 0.0, "{value}");
        }
    }

    #[test]
    fn degenerate_periods_produce_nothing_rather_than_panicking() {
        let series = bars(&[1.0, 2.0, 3.0]);
        assert!(rsi(&series, 0).iter().all(Option::is_none));
        assert!(atr(&series, 0).iter().all(Option::is_none));
        assert!(rsi(&[], 14).is_empty());
        assert!(atr(&[], 14).is_empty());
    }
}
