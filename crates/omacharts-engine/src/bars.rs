//! Bars, timeframes, and turning one timeframe into another.

use serde::{Deserialize, Serialize};

/// One candle. `ts` is the unix second the bar opens at.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct Bar {
    pub ts: i64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum Timeframe {
    M5,
    M15,
    H1,
    H4,
    #[default]
    D1,
    W1,
}

impl Timeframe {
    pub const ALL: [Timeframe; 6] = [
        Timeframe::M5,
        Timeframe::M15,
        Timeframe::H1,
        Timeframe::H4,
        Timeframe::D1,
        Timeframe::W1,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Timeframe::M5 => "5m",
            Timeframe::M15 => "15m",
            Timeframe::H1 => "1h",
            Timeframe::H4 => "4h",
            Timeframe::D1 => "1D",
            Timeframe::W1 => "1W",
        }
    }

    /// Stable key for the cache and for settings.
    pub fn key(self) -> &'static str {
        self.label()
    }

    pub fn from_key(key: &str) -> Option<Timeframe> {
        Timeframe::ALL.into_iter().find(|t| t.key() == key)
    }

    pub fn seconds(self) -> i64 {
        match self {
            Timeframe::M5 => 300,
            Timeframe::M15 => 900,
            Timeframe::H1 => 3_600,
            Timeframe::H4 => 14_400,
            Timeframe::D1 => 86_400,
            Timeframe::W1 => 604_800,
        }
    }

    /// What we actually ask a provider for. No free feed serves 4h, and weekly
    /// is cheaper and more accurate folded from daily than fetched.
    pub fn native(self) -> Timeframe {
        match self {
            Timeframe::H4 => Timeframe::H1,
            Timeframe::W1 => Timeframe::D1,
            other => other,
        }
    }

    pub fn is_derived(self) -> bool {
        self.native() != self
    }

    pub fn is_intraday(self) -> bool {
        self.seconds() < 86_400
    }
}

/// Fold `src` into `target`.
///
/// `origin` shifts the bucket boundaries, in seconds. Futures sessions open at
/// 18:00 ET, so 4h bars bucketed from midnight disagree with every other tool
/// on the planet; passing the session offset here is what makes them line up.
///
/// Weekly folds to ISO weeks (Monday) and ignores `origin`.
pub fn resample(src: &[Bar], target: Timeframe, origin: i64) -> Vec<Bar> {
    if src.is_empty() {
        return Vec::new();
    }
    if target == Timeframe::W1 {
        return fold(src, |ts| week_start(*ts));
    }
    let step = target.seconds();
    fold(src, move |ts| (ts - origin).div_euclid(step) * step + origin)
}

fn fold(src: &[Bar], bucket_of: impl Fn(&i64) -> i64) -> Vec<Bar> {
    let mut out: Vec<Bar> = Vec::with_capacity(src.len() / 4 + 1);
    for bar in src {
        let bucket = bucket_of(&bar.ts);
        match out.last_mut() {
            Some(last) if last.ts == bucket => {
                last.high = last.high.max(bar.high);
                last.low = last.low.min(bar.low);
                last.close = bar.close;
                last.volume += bar.volume;
            }
            _ => out.push(Bar { ts: bucket, ..*bar }),
        }
    }
    out
}

/// Unix second of the Monday 00:00 UTC that `ts` falls in.
fn week_start(ts: i64) -> i64 {
    // The epoch was a Thursday, so the first Monday is four days in. Shift
    // onto that grid, bucket, shift back.
    const FIRST_MONDAY: i64 = 4 * 86_400;
    (ts - FIRST_MONDAY).div_euclid(604_800) * 604_800 + FIRST_MONDAY
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(ts: i64, o: f64, h: f64, l: f64, c: f64, v: f64) -> Bar {
        Bar { ts, open: o, high: h, low: l, close: c, volume: v }
    }

    #[test]
    fn four_hour_folds_from_hourly() {
        let src: Vec<Bar> = (0..8)
            .map(|i| bar(i * 3600, 10.0 + i as f64, 12.0 + i as f64, 9.0 + i as f64, 11.0 + i as f64, 1.0))
            .collect();
        let out = resample(&src, Timeframe::H4, 0);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].open, 10.0);
        assert_eq!(out[0].close, 14.0);
        assert_eq!(out[0].high, 15.0);
        assert_eq!(out[0].low, 9.0);
        assert_eq!(out[0].volume, 4.0);
    }

    #[test]
    fn the_session_origin_moves_the_boundaries() {
        // Two hourly bars either side of a boundary that only exists when the
        // session origin is applied.
        let src = vec![bar(0, 1.0, 1.0, 1.0, 1.0, 1.0), bar(3600, 2.0, 2.0, 2.0, 2.0, 1.0)];
        assert_eq!(resample(&src, Timeframe::H4, 0).len(), 1);
        assert_eq!(resample(&src, Timeframe::H4, 3600).len(), 2);
    }

    #[test]
    fn weekly_buckets_start_on_monday() {
        // 2024-01-04 was a Thursday; its week starts Monday 2024-01-01.
        let thursday = 1_704_326_400;
        let monday = week_start(thursday);
        assert_eq!(monday, 1_704_067_200);
        // Same bucket for every day of that week.
        assert_eq!(week_start(monday + 6 * 86_400), monday);
        assert_eq!(week_start(monday + 7 * 86_400), monday + 604_800);
    }

    #[test]
    fn empty_in_empty_out() {
        assert!(resample(&[], Timeframe::H4, 0).is_empty());
    }
}
