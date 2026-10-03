//! Volume profile.
//!
//! One profile per reset period: how much volume traded at each price, the
//! price that saw the most (the point of control), and the band around it
//! that covers most of the period's volume (the value area).
//!
//! Each bar's volume is spread across every row its range touches, in
//! proportion to how much of the bar sits in each. Dropping it all on the
//! close would make the profile a histogram of closes, which is a different
//! and less useful picture.

use crate::bars::Bar;

use super::periods::Reset;

#[derive(Clone, PartialEq, Debug)]
pub struct ProfileRow {
    pub low: f64,
    pub high: f64,
    pub volume: f64,
}

#[derive(Clone, PartialEq, Debug)]
pub struct Profile {
    /// The period this profile covers.
    pub start_ts: i64,
    pub end_ts: i64,
    /// Index of the first and last bar of the period, for placing it on the
    /// chart without searching for them again.
    pub first_bar: usize,
    pub last_bar: usize,
    pub rows: Vec<ProfileRow>,
    /// Row holding the most volume.
    pub poc: usize,
    /// Inclusive row range covering `value_area` of the volume.
    pub value_area: (usize, usize),
    pub max_volume: f64,
    pub total_volume: f64,
}

impl Profile {
    pub fn poc_price(&self) -> f64 {
        let row = &self.rows[self.poc];
        (row.low + row.high) / 2.0
    }

    /// Price bounds of the value area.
    pub fn value_area_bounds(&self) -> (f64, f64) {
        (self.rows[self.value_area.0].low, self.rows[self.value_area.1].high)
    }
}

pub fn compute(
    bars: &[Bar],
    reset: Reset,
    session_origin: i64,
    rows: usize,
    value_area: f64,
) -> Vec<Profile> {
    let rows = rows.clamp(4, 400);
    let mut out = Vec::new();
    if bars.is_empty() {
        return out;
    }

    let mut start = 0usize;
    let mut bucket = reset.bucket(bars[0].ts, session_origin);

    for i in 1..=bars.len() {
        let next = bars.get(i).map(|b| reset.bucket(b.ts, session_origin));
        if next == Some(bucket) {
            continue;
        }
        if let Some(profile) = one(&bars[start..i], start, bucket, rows, value_area) {
            out.push(profile);
        }
        if let Some(next) = next {
            bucket = next;
            start = i;
        }
    }
    out
}

fn one(
    bars: &[Bar],
    offset: usize,
    start_ts: i64,
    rows: usize,
    value_area: f64,
) -> Option<Profile> {
    let (mut low, mut high) = (f64::MAX, f64::MIN);
    for bar in bars {
        low = low.min(bar.low);
        high = high.max(bar.high);
    }
    if !low.is_finite() || !high.is_finite() {
        return None;
    }
    // A period that never moved still has a profile; give it a row to live in.
    if (high - low).abs() < f64::EPSILON {
        high = low + 1.0;
    }

    let step = (high - low) / rows as f64;
    let mut volumes = vec![0.0f64; rows];

    for bar in bars {
        let weight = if bar.volume > 0.0 { bar.volume } else { 1.0 };
        let span = (bar.high - bar.low).max(f64::EPSILON);
        let first = (((bar.low - low) / step).floor() as isize).clamp(0, rows as isize - 1) as usize;
        let last = (((bar.high - low) / step).floor() as isize).clamp(0, rows as isize - 1) as usize;

        if first == last {
            volumes[first] += weight;
            continue;
        }
        // Split across the rows the bar touches, by how much of it is in each.
        for row in first..=last {
            let row_low = low + row as f64 * step;
            let row_high = row_low + step;
            let overlap = (bar.high.min(row_high) - bar.low.max(row_low)).max(0.0);
            volumes[row] += weight * overlap / span;
        }
    }

    let total: f64 = volumes.iter().sum();
    let poc = volumes
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(i, _)| i)?;
    let max_volume = volumes[poc];

    Some(Profile {
        start_ts,
        end_ts: bars.last()?.ts,
        first_bar: offset,
        last_bar: offset + bars.len() - 1,
        value_area: expand_value_area(&volumes, poc, total, value_area),
        rows: (0..rows)
            .map(|i| ProfileRow {
                low: low + i as f64 * step,
                high: low + (i + 1) as f64 * step,
                volume: volumes[i],
            })
            .collect(),
        poc,
        max_volume,
        total_volume: total,
    })
}

/// Grow outward from the point of control, always taking the busier side,
/// until the rows cover `fraction` of the volume.
fn expand_value_area(
    volumes: &[f64],
    poc: usize,
    total: f64,
    fraction: f64,
) -> (usize, usize) {
    let target = total * fraction.clamp(0.0, 1.0);
    let (mut lower, mut upper) = (poc, poc);
    let mut covered = volumes[poc];

    while covered < target && (lower > 0 || upper + 1 < volumes.len()) {
        let below = if lower > 0 { Some(volumes[lower - 1]) } else { None };
        let above = if upper + 1 < volumes.len() { Some(volumes[upper + 1]) } else { None };
        match (below, above) {
            (Some(b), Some(a)) if b > a => {
                lower -= 1;
                covered += b;
            }
            (Some(_), Some(a)) => {
                upper += 1;
                covered += a;
            }
            (Some(b), None) => {
                lower -= 1;
                covered += b;
            }
            (None, Some(a)) => {
                upper += 1;
                covered += a;
            }
            (None, None) => break,
        }
    }
    (lower, upper)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 86_400;

    fn bar(ts: i64, low: f64, high: f64, volume: f64) -> Bar {
        Bar { ts, open: low, high, low, close: high, volume }
    }

    #[test]
    fn one_profile_per_period() {
        let bars = vec![
            bar(0, 10.0, 11.0, 100.0),
            bar(3600, 10.0, 11.0, 100.0),
            bar(DAY, 20.0, 21.0, 100.0),
            bar(DAY + 3600, 20.0, 21.0, 100.0),
        ];
        let profiles = compute(&bars, Reset::Session, 0, 10, 0.7);
        assert_eq!(profiles.len(), 2);
        assert_eq!(profiles[0].first_bar, 0);
        assert_eq!(profiles[0].last_bar, 1);
        assert_eq!(profiles[1].first_bar, 2);
        assert_eq!(profiles[1].last_bar, 3);
    }

    #[test]
    fn the_point_of_control_is_where_volume_concentrated() {
        // Lots of trade at 10, a little at 20.
        let mut bars = vec![bar(0, 10.0, 10.1, 1000.0); 1];
        bars.push(bar(60, 20.0, 20.1, 1.0));
        let profile = &compute(&bars, Reset::Session, 0, 20, 0.7)[0];
        assert!(profile.poc_price() < 11.0, "poc at {}", profile.poc_price());
    }

    #[test]
    fn volume_is_spread_across_the_rows_a_bar_touches() {
        // One bar spanning the whole range: every row gets a share, and they
        // sum back to the bar's volume.
        let bars = vec![bar(0, 0.0, 10.0, 100.0)];
        let profile = &compute(&bars, Reset::Session, 0, 10, 0.7)[0];
        let total: f64 = profile.rows.iter().map(|r| r.volume).sum();
        assert!((total - 100.0).abs() < 1e-6, "{total}");
        assert!(profile.rows.iter().all(|r| r.volume > 0.0), "every row touched");
    }

    #[test]
    fn the_value_area_covers_the_requested_fraction() {
        // A peak with thin tails.
        let bars = vec![
            bar(0, 9.0, 9.1, 10.0),
            bar(60, 10.0, 10.1, 100.0),
            bar(120, 10.0, 10.1, 100.0),
            bar(180, 11.0, 11.1, 10.0),
        ];
        let profile = &compute(&bars, Reset::Session, 0, 24, 0.70)[0];
        let (lo, hi) = profile.value_area;
        let covered: f64 = profile.rows[lo..=hi].iter().map(|r| r.volume).sum();
        assert!(covered >= profile.total_volume * 0.70 - 1e-6, "{covered}");
        assert!(lo <= profile.poc && profile.poc <= hi, "the value area contains the poc");
    }

    #[test]
    fn a_full_value_area_is_the_whole_profile() {
        let bars = vec![bar(0, 9.0, 11.0, 50.0), bar(60, 9.5, 10.5, 50.0)];
        let profile = &compute(&bars, Reset::Session, 0, 12, 1.0)[0];
        assert_eq!(profile.value_area, (0, profile.rows.len() - 1));
    }

    #[test]
    fn value_area_bounds_bracket_the_poc_price() {
        let bars = vec![bar(0, 9.0, 11.0, 50.0), bar(60, 9.5, 10.5, 80.0)];
        let profile = &compute(&bars, Reset::Session, 0, 20, 0.7)[0];
        let (low, high) = profile.value_area_bounds();
        let poc = profile.poc_price();
        assert!(low <= poc && poc <= high, "{low} <= {poc} <= {high}");
    }

    #[test]
    fn a_motionless_period_still_produces_a_profile() {
        let bars = vec![bar(0, 10.0, 10.0, 5.0), bar(60, 10.0, 10.0, 5.0)];
        let profiles = compute(&bars, Reset::Session, 0, 10, 0.7);
        assert_eq!(profiles.len(), 1);
        assert!(profiles[0].max_volume > 0.0);
        assert!(profiles[0].poc_price().is_finite());
    }

    #[test]
    fn a_volumeless_instrument_still_profiles() {
        let bars = vec![bar(0, 9.0, 10.0, 0.0), bar(60, 9.5, 10.5, 0.0)];
        let profile = &compute(&bars, Reset::Session, 0, 10, 0.7)[0];
        assert!(profile.total_volume > 0.0, "bars count even without volume");
    }

    #[test]
    fn empty_input_produces_nothing() {
        assert!(compute(&[], Reset::Session, 0, 10, 0.7).is_empty());
    }

    #[test]
    fn row_counts_are_clamped_to_something_drawable() {
        let bars = vec![bar(0, 9.0, 11.0, 50.0)];
        assert_eq!(compute(&bars, Reset::Session, 0, 0, 0.7)[0].rows.len(), 4);
        assert_eq!(compute(&bars, Reset::Session, 0, 10_000, 0.7)[0].rows.len(), 400);
    }
}
