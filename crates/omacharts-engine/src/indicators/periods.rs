//! Reset periods.
//!
//! VWAP and a volume profile both mean "since the start of the current
//! something". This is that something: a function from a timestamp to the
//! bucket it belongs to, so both indicators agree on where a period begins.

use chrono::{Datelike, TimeZone, Utc};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Reset {
    /// The trading session. For futures that is the 18:00 ET open, which is
    /// why `session_origin` is threaded through.
    #[default]
    Session,
    Week,
    Month,
    Quarter,
    Year,
}

impl Reset {
    pub const ALL: [Reset; 5] =
        [Reset::Session, Reset::Week, Reset::Month, Reset::Quarter, Reset::Year];

    pub fn label(self) -> &'static str {
        match self {
            Reset::Session => "Session",
            Reset::Week => "Week",
            Reset::Month => "Month",
            Reset::Quarter => "Quarter",
            Reset::Year => "Year",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Reset::Session => "session",
            Reset::Week => "week",
            Reset::Month => "month",
            Reset::Quarter => "quarter",
            Reset::Year => "year",
        }
    }

    pub fn from_key(key: &str) -> Option<Reset> {
        Reset::ALL.into_iter().find(|r| r.key() == key)
    }

    /// The start of the period `ts` falls in, as a unix second.
    ///
    /// Buckets are the identity of a period; two bars share a period exactly
    /// when this returns the same value for both.
    pub fn bucket(self, ts: i64, session_origin: i64) -> i64 {
        const DAY: i64 = 86_400;
        match self {
            // Shift onto the session's own grid before taking the day, so a
            // futures session that opens at 18:00 ET counts as one period
            // rather than two halves of two calendar days.
            Reset::Session => (ts - session_origin).div_euclid(DAY) * DAY + session_origin,
            Reset::Week => {
                // The epoch was a Thursday; the first Monday is four days in.
                const FIRST_MONDAY: i64 = 4 * DAY;
                (ts - FIRST_MONDAY).div_euclid(7 * DAY) * (7 * DAY) + FIRST_MONDAY
            }
            Reset::Month => start_of(ts, |d| (d.year(), d.month(), 1)),
            Reset::Quarter => {
                start_of(ts, |d| (d.year(), (d.month() - 1) / 3 * 3 + 1, 1))
            }
            Reset::Year => start_of(ts, |d| (d.year(), 1, 1)),
        }
    }
}

/// Truncate a timestamp to the (year, month, day) the closure names.
fn start_of(ts: i64, parts: impl Fn(&chrono::DateTime<Utc>) -> (i32, u32, u32)) -> i64 {
    let Some(dt) = Utc.timestamp_opt(ts, 0).single() else {
        return ts;
    };
    let (year, month, day) = parts(&dt);
    Utc.with_ymd_and_hms(year, month, day, 0, 0, 0)
        .single()
        .map(|d| d.timestamp())
        .unwrap_or(ts)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2024-02-14 12:00 UTC, a Wednesday.
    const WEDNESDAY: i64 = 1_707_912_000;

    #[test]
    fn a_session_is_a_day_on_the_sessions_own_grid() {
        let midnight = Reset::Session.bucket(WEDNESDAY, 0);
        assert_eq!(midnight, 1_707_868_800, "2024-02-14 00:00 UTC");

        // With an 18:00 ET origin the same instant belongs to the session that
        // opened the previous evening.
        let origin = 79_200; // 22:00 UTC
        let session = Reset::Session.bucket(WEDNESDAY, origin);
        assert!(session < midnight, "session starts before calendar midnight");
        assert_eq!(midnight - session, 86_400 - origin);
    }

    #[test]
    fn bars_either_side_of_the_session_open_are_different_periods() {
        let origin = 79_200;
        let before = Reset::Session.bucket(1_707_868_800 + origin - 60, origin);
        let after = Reset::Session.bucket(1_707_868_800 + origin + 60, origin);
        assert_ne!(before, after);
        assert_eq!(after - before, 86_400);
    }

    #[test]
    fn weeks_start_on_monday() {
        let monday = Reset::Week.bucket(WEDNESDAY, 0);
        assert_eq!(monday, 1_707_696_000, "2024-02-12 00:00 UTC");
        // Every day of that week agrees.
        for day in 0..7 {
            assert_eq!(Reset::Week.bucket(monday + day * 86_400, 0), monday);
        }
        assert_ne!(Reset::Week.bucket(monday + 7 * 86_400, 0), monday);
    }

    #[test]
    fn months_quarters_and_years_truncate() {
        assert_eq!(Reset::Month.bucket(WEDNESDAY, 0), 1_706_745_600, "2024-02-01");
        assert_eq!(Reset::Quarter.bucket(WEDNESDAY, 0), 1_704_067_200, "2024-01-01");
        assert_eq!(Reset::Year.bucket(WEDNESDAY, 0), 1_704_067_200, "2024-01-01");
    }

    #[test]
    fn quarters_land_on_the_right_months() {
        // One day in each quarter of 2024.
        let cases = [
            (1_707_912_000_i64, "2024-01-01", 1_704_067_200_i64), // Feb -> Q1
            (1_715_000_000, "2024-04-01", 1_711_929_600),          // May -> Q2
            (1_722_000_000, "2024-07-01", 1_719_792_000),          // Jul -> Q3
            (1_730_000_000, "2024-10-01", 1_727_740_800),          // Oct -> Q4
        ];
        for (ts, label, expected) in cases {
            assert_eq!(Reset::Quarter.bucket(ts, 0), expected, "{label}");
        }
    }

    #[test]
    fn keys_round_trip() {
        for reset in Reset::ALL {
            assert_eq!(Reset::from_key(reset.key()), Some(reset));
        }
        assert_eq!(Reset::from_key("fortnight"), None);
    }
}
