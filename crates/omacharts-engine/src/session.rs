//! Regular and extended trading hours.
//!
//! A free feed hands back every bar it has, including the thin overnight ones.
//! Those matter sometimes and ruin a chart the rest of the time: a handful of
//! trades at 3am stretch the price scale and leave the session everyone
//! actually traded squashed into a corner.
//!
//! So the chart can ask for regular hours only. The window is the US cash
//! session in New York, which is what "RTH" means for everything this app
//! charts — US equities, the index futures that track them, and the indexes
//! themselves. Instruments that genuinely trade around the clock are left
//! alone, because filtering them would only throw data away.

use chrono::{Datelike, TimeZone, Timelike, Weekday};
use chrono_tz::America::New_York;
use serde::{Deserialize, Serialize};

use crate::bars::Bar;
use crate::symbols::{Instrument, InstrumentKind};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Session {
    /// Everything the provider sends, overnight included.
    #[default]
    Extended,
    /// The cash session only.
    Regular,
}

impl Session {
    pub const ALL: [Session; 2] = [Session::Extended, Session::Regular];

    pub fn label(self) -> &'static str {
        match self {
            Session::Extended => "Extended hours",
            Session::Regular => "Regular hours only",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Session::Extended => "extended",
            Session::Regular => "regular",
        }
    }

    pub fn from_key(key: &str) -> Option<Session> {
        Session::ALL.into_iter().find(|s| s.key() == key)
    }
}

/// Minutes past midnight, New York time, that the cash session runs between.
const OPEN: u32 = 9 * 60 + 30;
const CLOSE: u32 = 16 * 60;

/// Does restricting to regular hours mean anything for this instrument?
///
/// FX and crypto have no cash session; neither does anything listed outside
/// the US, whose hours are not New York's.
pub fn has_regular_hours(instrument: &Instrument) -> bool {
    if instrument.suffix.is_some() {
        return false;
    }
    matches!(
        instrument.kind,
        InstrumentKind::Equity
            | InstrumentKind::Etf
            | InstrumentKind::Index
            | InstrumentKind::FutureRoot
    )
}

/// Keep only the bars inside the cash session.
///
/// A no-op for daily and coarser bars — one bar already is a session — and for
/// instruments with no cash session to speak of.
pub fn filter(bars: &[Bar], session: Session, instrument: &Instrument, intraday: bool) -> Vec<Bar> {
    if session == Session::Extended || !intraday || !has_regular_hours(instrument) {
        return bars.to_vec();
    }
    bars.iter().copied().filter(|bar| in_regular_hours(bar.ts)).collect()
}

/// Is this instant inside the New York cash session on a weekday?
///
/// Timezone-aware rather than a fixed offset, because the session keeps its
/// local hours across daylight saving while its UTC offset moves.
pub fn in_regular_hours(ts: i64) -> bool {
    let Some(local) = New_York.timestamp_opt(ts, 0).single() else {
        return false;
    };
    if matches!(local.weekday(), Weekday::Sat | Weekday::Sun) {
        return false;
    }
    let minutes = local.hour() * 60 + local.minute();
    // The bar stamped at the close belongs to the next session, not this one.
    (OPEN..CLOSE).contains(&minutes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instrument(kind: InstrumentKind, suffix: Option<&str>) -> Instrument {
        Instrument {
            symbol: "X".into(),
            name: "X".into(),
            kind,
            suffix: suffix.map(str::to_string),
            currency: None,
            tier: 0,
            session_origin: 0,
            overrides: Vec::new(),
        }
    }

    fn bar(ts: i64) -> Bar {
        Bar { ts, open: 1.0, high: 1.0, low: 1.0, close: 1.0, volume: 1.0 }
    }

    /// 2024-03-13 was a Wednesday, in US daylight time (UTC-4).
    fn wednesday_at(hour: u32, minute: u32) -> i64 {
        New_York
            .with_ymd_and_hms(2024, 3, 13, hour, minute, 0)
            .single()
            .unwrap()
            .timestamp()
    }

    /// 2024-01-10, a Wednesday in standard time (UTC-5).
    fn winter_wednesday_at(hour: u32, minute: u32) -> i64 {
        New_York
            .with_ymd_and_hms(2024, 1, 10, hour, minute, 0)
            .single()
            .unwrap()
            .timestamp()
    }

    #[test]
    fn the_cash_session_runs_from_the_open_to_the_close() {
        assert!(!in_regular_hours(wednesday_at(9, 29)), "before the bell");
        assert!(in_regular_hours(wednesday_at(9, 30)), "the open");
        assert!(in_regular_hours(wednesday_at(12, 0)));
        assert!(in_regular_hours(wednesday_at(15, 59)));
        assert!(!in_regular_hours(wednesday_at(16, 0)), "the close belongs to the next session");
        assert!(!in_regular_hours(wednesday_at(3, 0)), "overnight");
    }

    #[test]
    fn the_session_keeps_its_local_hours_across_daylight_saving() {
        // Same wall-clock times, different UTC offsets.
        assert!(in_regular_hours(winter_wednesday_at(9, 30)));
        assert!(!in_regular_hours(winter_wednesday_at(9, 29)));
        assert!(in_regular_hours(winter_wednesday_at(15, 59)));
        // And the two dates really are on different offsets.
        assert_ne!(
            wednesday_at(9, 30) % 86_400,
            winter_wednesday_at(9, 30) % 86_400,
            "the fixture dates should straddle the change"
        );
    }

    #[test]
    fn weekends_are_not_the_cash_session() {
        let saturday = New_York.with_ymd_and_hms(2024, 3, 16, 12, 0, 0).single().unwrap();
        assert!(!in_regular_hours(saturday.timestamp()));
    }

    #[test]
    fn filtering_keeps_only_the_session() {
        let bars = vec![
            bar(wednesday_at(4, 0)),
            bar(wednesday_at(9, 30)),
            bar(wednesday_at(12, 0)),
            bar(wednesday_at(18, 0)),
        ];
        let stock = instrument(InstrumentKind::Equity, None);
        let kept = filter(&bars, Session::Regular, &stock, true);
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0].ts, wednesday_at(9, 30));
    }

    #[test]
    fn extended_hours_keeps_everything() {
        let bars: Vec<Bar> = (0..24).map(|h| bar(wednesday_at(h, 0))).collect();
        let stock = instrument(InstrumentKind::Equity, None);
        assert_eq!(filter(&bars, Session::Extended, &stock, true).len(), bars.len());
    }

    #[test]
    fn daily_bars_are_never_filtered() {
        // One daily bar already is a session; dropping it for being stamped
        // outside 09:30 would empty the chart.
        let bars = vec![bar(wednesday_at(0, 0)), bar(wednesday_at(0, 0) + 86_400)];
        let stock = instrument(InstrumentKind::Equity, None);
        assert_eq!(filter(&bars, Session::Regular, &stock, false).len(), 2);
    }

    #[test]
    fn instruments_without_a_cash_session_are_left_alone() {
        let bars: Vec<Bar> = (0..24).map(|h| bar(wednesday_at(h, 0))).collect();
        for kind in [InstrumentKind::Fx, InstrumentKind::Crypto] {
            let it = instrument(kind, None);
            assert!(!has_regular_hours(&it));
            assert_eq!(filter(&bars, Session::Regular, &it, true).len(), bars.len());
        }
        // Nor does a foreign listing, whose hours are not New York's.
        let madrid = instrument(InstrumentKind::Equity, Some("MC"));
        assert!(!has_regular_hours(&madrid));
        assert_eq!(filter(&bars, Session::Regular, &madrid, true).len(), bars.len());
    }

    #[test]
    fn keys_round_trip() {
        for session in Session::ALL {
            assert_eq!(Session::from_key(session.key()), Some(session));
        }
        assert_eq!(Session::from_key("nonsense"), None);
    }
}
