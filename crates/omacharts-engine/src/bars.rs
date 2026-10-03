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

/// The unit a resolution is counted in.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Unit {
    Minute,
    Hour,
    Day,
    Week,
}

/// A chart resolution: a count of a unit.
///
/// Not an enum of presets, because people type resolutions. "3m" is a perfectly
/// reasonable thing to want and no fixed list will ever contain everyone's.
/// The strip in the header is just [`Timeframe::PRESETS`]; anything parseable
/// works.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct Timeframe {
    pub count: u32,
    pub unit: Unit,
}

impl Default for Timeframe {
    fn default() -> Timeframe {
        Timeframe::days(1)
    }
}

impl Timeframe {
    pub const fn minutes(count: u32) -> Timeframe {
        Timeframe { count, unit: Unit::Minute }
    }
    pub const fn hours(count: u32) -> Timeframe {
        Timeframe { count, unit: Unit::Hour }
    }
    pub const fn days(count: u32) -> Timeframe {
        Timeframe { count, unit: Unit::Day }
    }
    pub const fn weeks(count: u32) -> Timeframe {
        Timeframe { count, unit: Unit::Week }
    }

    /// What the header strip offers. Everything else is typed.
    pub const PRESETS: [Timeframe; 6] = [
        Timeframe::minutes(5),
        Timeframe::minutes(15),
        Timeframe::hours(1),
        Timeframe::hours(4),
        Timeframe::days(1),
        Timeframe::weeks(1),
    ];

    /// The intervals a provider is expected to serve directly. Anything else
    /// is folded from one of these.
    const SERVED_MINUTES: [u32; 7] = [1, 2, 5, 15, 30, 60, 90];

    pub fn seconds(self) -> i64 {
        let unit = match self.unit {
            Unit::Minute => 60,
            Unit::Hour => 3_600,
            Unit::Day => 86_400,
            Unit::Week => 604_800,
        };
        unit * self.count.max(1) as i64
    }

    /// "5m", "4h", "1D", "1W" — what you type and what is stored.
    pub fn key(self) -> String {
        let suffix = match self.unit {
            Unit::Minute => 'm',
            Unit::Hour => 'h',
            Unit::Day => 'D',
            Unit::Week => 'W',
        };
        format!("{}{suffix}", self.count)
    }

    pub fn label(self) -> String {
        self.key()
    }

    /// Spelled out, for confirming what was typed.
    ///
    /// Reads back the *normalised* resolution, so typing "240" says "4 hours"
    /// — which is both the honest answer and the thing worth knowing before
    /// pressing Enter.
    pub fn description(self) -> String {
        let count = self.count.max(1);
        let unit = match self.unit {
            Unit::Minute => "minute",
            Unit::Hour => "hour",
            Unit::Day => "day",
            Unit::Week => "week",
        };
        if count == 1 {
            format!("1 {unit}")
        } else {
            format!("{count} {unit}s")
        }
    }

    /// Parse what someone typed.
    ///
    /// A bare number means minutes, the way every charting package has worked
    /// since the 80s: "15" is fifteen minutes, "240" is four hours. A suffix
    /// is honoured when given, and case does not matter except that both "d"
    /// and "D" mean days.
    pub fn parse(text: &str) -> Option<Timeframe> {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        let (digits, suffix): (String, String) =
            text.chars().partition(|c| c.is_ascii_digit());
        let suffix = suffix.trim().to_lowercase();

        // "D" and "W" on their own mean one of them.
        let count: u32 = if digits.is_empty() { 1 } else { digits.parse().ok()? };
        if count == 0 {
            return None;
        }

        let unit = match suffix.as_str() {
            "" => Unit::Minute,
            "m" | "min" | "mins" | "minute" | "minutes" => Unit::Minute,
            "h" | "hr" | "hrs" | "hour" | "hours" => Unit::Hour,
            "d" | "day" | "days" => Unit::Day,
            "w" | "wk" | "week" | "weeks" => Unit::Week,
            _ => return None,
        };
        Some(Timeframe { count, unit }.normalised())
    }

    /// Collapse a resolution onto the largest unit that expresses it exactly,
    /// so "60m" and "1h" are the same thing rather than two chart states.
    pub fn normalised(self) -> Timeframe {
        match self.unit {
            Unit::Minute if self.count % 1440 == 0 => Timeframe::days(self.count / 1440),
            Unit::Minute if self.count % 60 == 0 => Timeframe::hours(self.count / 60),
            Unit::Hour if self.count % 24 == 0 => Timeframe::days(self.count / 24),
            Unit::Day if self.count % 7 == 0 => Timeframe::weeks(self.count / 7),
            _ => self,
        }
    }

    /// The resolution to actually fetch, which this one folds from.
    ///
    /// The largest served interval that divides this one evenly — folding
    /// needs whole bars, and asking for a finer interval than necessary burns
    /// history limits and rate limit for nothing.
    pub fn native(self) -> Timeframe {
        match self.unit {
            Unit::Minute => {
                if Timeframe::SERVED_MINUTES.contains(&self.count) {
                    return self;
                }
                let base = Timeframe::SERVED_MINUTES
                    .into_iter()
                    .rev()
                    .find(|m| self.count % m == 0)
                    .unwrap_or(1);
                Timeframe::minutes(base)
            }
            Unit::Hour => {
                if self.count == 1 {
                    self
                } else {
                    Timeframe::hours(1)
                }
            }
            Unit::Day | Unit::Week => {
                if self.unit == Unit::Day && self.count == 1 {
                    self
                } else {
                    Timeframe::days(1)
                }
            }
        }
    }

    pub fn is_derived(self) -> bool {
        self.native() != self
    }

    pub fn is_intraday(self) -> bool {
        matches!(self.unit, Unit::Minute | Unit::Hour)
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
    if target.unit == Unit::Week {
        let weeks = target.count.max(1) as i64;
        return fold(src, move |ts| {
            let start = week_start(*ts);
            // Multi-week buckets count from the epoch's first Monday so they
            // are stable rather than depending on where the data begins.
            const FIRST_MONDAY: i64 = 4 * 86_400;
            (start - FIRST_MONDAY).div_euclid(weeks * 604_800) * (weeks * 604_800) + FIRST_MONDAY
        });
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
        let out = resample(&src, Timeframe::hours(4), 0);
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
        assert_eq!(resample(&src, Timeframe::hours(4), 0).len(), 1);
        assert_eq!(resample(&src, Timeframe::hours(4), 3600).len(), 2);
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
    fn resolutions_parse_the_way_people_type_them() {
        assert_eq!(Timeframe::parse("3"), Some(Timeframe::minutes(3)));
        assert_eq!(Timeframe::parse("3m"), Some(Timeframe::minutes(3)));
        assert_eq!(Timeframe::parse("15"), Some(Timeframe::minutes(15)));
        assert_eq!(Timeframe::parse("4h"), Some(Timeframe::hours(4)));
        assert_eq!(Timeframe::parse("1D"), Some(Timeframe::days(1)));
        assert_eq!(Timeframe::parse("d"), Some(Timeframe::days(1)));
        assert_eq!(Timeframe::parse("W"), Some(Timeframe::weeks(1)));
        assert_eq!(Timeframe::parse(" 30 min "), Some(Timeframe::minutes(30)));

        assert_eq!(Timeframe::parse(""), None);
        assert_eq!(Timeframe::parse("0"), None);
        assert_eq!(Timeframe::parse("banana"), None);
    }

    #[test]
    fn equivalent_resolutions_collapse_to_one() {
        // "60" and "1h" must not be two different chart states.
        assert_eq!(Timeframe::parse("60").unwrap(), Timeframe::hours(1));
        assert_eq!(Timeframe::parse("240").unwrap(), Timeframe::hours(4));
        assert_eq!(Timeframe::parse("1440").unwrap(), Timeframe::days(1));
        assert_eq!(Timeframe::parse("24h").unwrap(), Timeframe::days(1));
        assert_eq!(Timeframe::parse("7d").unwrap(), Timeframe::weeks(1));
        // But 90 minutes has no larger exact unit.
        assert_eq!(Timeframe::parse("90").unwrap(), Timeframe::minutes(90));
    }

    #[test]
    fn keys_round_trip_through_parse() {
        for timeframe in Timeframe::PRESETS {
            assert_eq!(Timeframe::parse(&timeframe.key()), Some(timeframe));
        }
        for odd in [Timeframe::minutes(3), Timeframe::minutes(7), Timeframe::days(3)] {
            assert_eq!(Timeframe::parse(&odd.key()), Some(odd));
        }
    }

    #[test]
    fn a_custom_resolution_folds_from_something_the_provider_serves() {
        // 3m divides by 1 only, so it folds from one-minute bars.
        assert_eq!(Timeframe::minutes(3).native(), Timeframe::minutes(1));
        // 10m folds from 5m, which is cheaper than asking for 1m.
        assert_eq!(Timeframe::minutes(10).native(), Timeframe::minutes(5));
        assert_eq!(Timeframe::minutes(45).native(), Timeframe::minutes(15));
        // Served intervals are fetched directly.
        assert_eq!(Timeframe::minutes(5).native(), Timeframe::minutes(5));
        assert_eq!(Timeframe::hours(1).native(), Timeframe::hours(1));
        assert_eq!(Timeframe::days(1).native(), Timeframe::days(1));
        // And everything coarse folds from daily.
        assert_eq!(Timeframe::hours(4).native(), Timeframe::hours(1));
        assert_eq!(Timeframe::weeks(1).native(), Timeframe::days(1));
        assert_eq!(Timeframe::days(3).native(), Timeframe::days(1));
    }

    #[test]
    fn the_native_resolution_always_divides_the_one_asked_for() {
        for count in 1..=240u32 {
            let timeframe = Timeframe::minutes(count).normalised();
            let native = timeframe.native();
            assert_eq!(
                timeframe.seconds() % native.seconds(),
                0,
                "{} does not fold from {}",
                timeframe.key(),
                native.key()
            );
        }
    }

    #[test]
    fn resolutions_read_back_in_words() {
        assert_eq!(Timeframe::minutes(1).description(), "1 minute");
        assert_eq!(Timeframe::minutes(3).description(), "3 minutes");
        assert_eq!(Timeframe::hours(1).description(), "1 hour");
        assert_eq!(Timeframe::hours(4).description(), "4 hours");
        assert_eq!(Timeframe::days(1).description(), "1 day");
        assert_eq!(Timeframe::weeks(2).description(), "2 weeks");
    }

    #[test]
    fn what_you_type_reads_back_normalised() {
        // Typing 240 is typing four hours, and should say so.
        assert_eq!(Timeframe::parse("240").unwrap().description(), "4 hours");
        assert_eq!(Timeframe::parse("60").unwrap().description(), "1 hour");
        assert_eq!(Timeframe::parse("3").unwrap().description(), "3 minutes");
        assert_eq!(Timeframe::parse("7d").unwrap().description(), "1 week");
    }

    #[test]
    fn intraday_is_about_the_unit_not_the_length() {
        assert!(Timeframe::minutes(3).is_intraday());
        assert!(Timeframe::hours(12).is_intraday());
        assert!(!Timeframe::days(1).is_intraday());
        assert!(!Timeframe::weeks(1).is_intraday());
    }

    #[test]
    fn empty_in_empty_out() {
        assert!(resample(&[], Timeframe::hours(4), 0).is_empty());
    }
}
