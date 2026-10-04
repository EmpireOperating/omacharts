//! Yahoo Finance, via the chart endpoint.
//!
//! `v8/finance/chart` is used for everything, quotes included — the older
//! `v7/finance/quote` now needs a cookie and crumb handshake, and this one
//! needs nothing. No key, no account, every instrument type we care about.
//!
//! Two things to respect. The data is delayed — ten minutes for futures,
//! fifteen for indexes — so the UI must say so. And the rate limiting is
//! aggressive: a single unlucky request can leave an address blocked for
//! every subsequent call. The cache above this layer is the real defence;
//! this layer's job is to ask for as little as possible and to report
//! throttling clearly rather than retrying into a wall.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::bars::{Bar, Timeframe, Unit};
use crate::provider::{Capability, Provider, ProviderError};
use crate::symbols::{Instrument, InstrumentKind};

const ENDPOINT: &str = "https://query1.finance.yahoo.com/v8/finance/chart";

/// Yahoo serves nothing without a browser-shaped agent.
const AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 \
                     (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

/// What Yahoo serves directly, and how far back each goes.
///
/// Anything else is folded from one of these by the caller, which is why a
/// three-minute chart works at all.
const CAPABILITIES: &[Capability] = &[
    // One-minute bars are kept for a month but served a week at a time, and
    // asking for the month returns an empty series rather than an error.
    Capability {
        timeframe: Timeframe::minutes(1),
        history_days: Some(30),
        max_request_days: Some(7),
    },
    Capability { timeframe: Timeframe::minutes(2), history_days: Some(60), max_request_days: None },
    Capability { timeframe: Timeframe::minutes(5), history_days: Some(60), max_request_days: None },
    Capability { timeframe: Timeframe::minutes(15), history_days: Some(60), max_request_days: None },
    Capability { timeframe: Timeframe::minutes(30), history_days: Some(60), max_request_days: None },
    Capability { timeframe: Timeframe::minutes(90), history_days: Some(60), max_request_days: None },
    Capability { timeframe: Timeframe::hours(1), history_days: Some(730), max_request_days: None },
    Capability { timeframe: Timeframe::days(1), history_days: None, max_request_days: None },
];

/// How far inside its own limit to ask.
///
/// "Within the last 60 days" turns out to mean strictly within: asking for
/// exactly sixty days of fifteen-minute bars is refused outright — not an
/// empty series, a 422 — which is how DIA ended up blank at 15m while every
/// other resolution worked. The boundary is not even consistent between
/// intervals; five-minute bars accept the same window that fifteen-minute bars
/// reject.
///
/// Six hours of margin costs nothing measurable. The oldest bars in that
/// window are overnight ones that do not exist, so the same request comes back
/// with the same count either way.
const WINDOW_MARGIN: i64 = 6 * 3_600;

/// Smallest gap between two requests the user is waiting on.
///
/// Yahoo tolerates a steady trickle and punishes bursts, and nothing here
/// needs to be fast about fetching — the cache is what makes it feel fast.
const MIN_GAP: Duration = Duration::from_millis(350);

/// Smallest gap between two requests nobody asked for.
///
/// Speculative work gets a much longer leash. Filling a watchlist is a
/// one-time cost per symbol — the bars are kept forever — so there is no
/// reason to spend the request budget quickly, and every reason not to.
const MIN_GAP_SPECULATIVE: Duration = Duration::from_millis(2_000);

/// How long to stop asking entirely after a 429, and the ceiling that repeated
/// throttling climbs to.
const COOLDOWN_START: Duration = Duration::from_secs(60);
const COOLDOWN_MAX: Duration = Duration::from_secs(900);

/// Transient failures worth one more try. Deliberately small: a chart that
/// paints from cache has nothing to gain from a third attempt.
const RETRIES: u32 = 2;

/// Request pacing, shared across every call.
struct Throttle {
    last_request: Option<Instant>,
    /// Set after a 429; until it passes we do not touch the network at all.
    cooldown_until: Option<Instant>,
    cooldown: Duration,
}

impl Throttle {
    fn new() -> Throttle {
        Throttle { last_request: None, cooldown_until: None, cooldown: COOLDOWN_START }
    }

    /// `Err` when we must not ask at all right now.
    ///
    /// Speculative work is refused for the whole cooldown *and* paced further
    /// apart the rest of the time. When Yahoo is unhappy, the thing to stop is
    /// the work nobody asked for — not the chart someone is looking at.
    fn admit(&mut self, now: Instant, speculative: bool) -> Result<Option<Duration>, ()> {
        if let Some(until) = self.cooldown_until {
            if now < until {
                return Err(());
            }
            self.cooldown_until = None;
        }
        let gap = if speculative { MIN_GAP_SPECULATIVE } else { MIN_GAP };
        let wait = self.last_request.and_then(|last| {
            let since = now.saturating_duration_since(last);
            (since < gap).then(|| gap - since)
        });
        Ok(wait)
    }

    fn record_request(&mut self, now: Instant) {
        self.last_request = Some(now);
    }

    /// Back off harder each time Yahoo says no, and relax once it says yes.
    fn record_throttled(&mut self, now: Instant) {
        self.cooldown_until = Some(now + self.cooldown);
        self.cooldown = (self.cooldown * 2).min(COOLDOWN_MAX);
    }

    fn record_success(&mut self) {
        self.cooldown = COOLDOWN_START;
    }
}

pub struct Yahoo {
    timeout: Option<Duration>,
    throttle: Mutex<Throttle>,
}

impl Default for Yahoo {
    fn default() -> Yahoo {
        Yahoo::new()
    }
}

impl Yahoo {
    pub fn new() -> Yahoo {
        Yahoo {
            timeout: Some(Duration::from_secs(20)),
            throttle: Mutex::new(Throttle::new()),
        }
    }

    /// Yahoo's own spelling of a resolution, for the ones it serves.
    fn interval(timeframe: Timeframe) -> Option<String> {
        if !CAPABILITIES.iter().any(|c| c.timeframe == timeframe) {
            // Folded from a served resolution by the caller.
            return None;
        }
        Some(match timeframe.unit {
            Unit::Minute => format!("{}m", timeframe.count),
            Unit::Hour => format!("{}h", timeframe.count),
            Unit::Day => format!("{}d", timeframe.count),
            Unit::Week => format!("{}wk", timeframe.count),
        })
    }

    /// How much history to ask for when we have none, in days.
    ///
    /// Never `range=max` for daily bars. Yahoo honours the interval only
    /// within a bounded window: ask for everything and it quietly coarsens,
    /// returning month-ends for the old part of the series and days only near
    /// the end. A chart built on that looks fine and is wrong.
    fn full_window_days(timeframe: Timeframe) -> i64 {
        let capability = CAPABILITIES.iter().find(|c| c.timeframe == timeframe);
        // Daily and coarser: two decades is more than any chart needs and well
        // inside the window Yahoo serves honestly.
        let history = capability.and_then(|c| c.history_days).map(i64::from).unwrap_or(20 * 365);
        let per_request = capability
            .and_then(|c| c.max_request_days)
            .map(i64::from)
            .unwrap_or(i64::MAX);
        history.min(per_request)
    }

    fn url(symbol: &str, timeframe: Timeframe, since: Option<i64>) -> Result<String, ProviderError> {
        let interval = Self::interval(timeframe).ok_or_else(|| {
            ProviderError::Unsupported(format!("{} is folded, not fetched", timeframe.label()))
        })?;
        let encoded = encode(symbol);
        // Always an explicit window. For a tail fetch it is how the request
        // stays small; for a first fetch it is what stops Yahoo coarsening the
        // series behind our back.
        let now = chrono::Utc::now().timestamp();
        let to = now + timeframe.seconds();
        let from = since.unwrap_or_else(|| {
            now - Self::full_window_days(timeframe) * 86_400 + WINDOW_MARGIN
        });
        Ok(format!(
            "{ENDPOINT}/{encoded}?interval={interval}&period1={from}&period2={to}"
        ))
    }
}

/// Percent-encode the handful of characters Yahoo symbols actually contain.
fn encode(symbol: &str) -> String {
    let mut out = String::with_capacity(symbol.len() + 4);
    for c in symbol.chars() {
        match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '.' | '-' | '_' | '~' => out.push(c),
            '^' => out.push_str("%5E"),
            '=' => out.push_str("%3D"),
            other => {
                let mut buf = [0u8; 4];
                for b in other.encode_utf8(&mut buf).as_bytes() {
                    out.push_str(&format!("%{b:02X}"));
                }
            }
        }
    }
    out
}

impl Yahoo {
    /// One request, no pacing and no retries.
    fn fetch(&self, url: &str) -> Result<serde_json::Value, ProviderError> {
        let mut request = ureq::get(url).header("User-Agent", AGENT);
        if let Some(timeout) = self.timeout {
            request = request.config().timeout_global(Some(timeout)).build();
        }

        let mut response = match request.call() {
            Ok(response) => response,
            Err(ureq::Error::StatusCode(429)) => return Err(ProviderError::RateLimited),
            Err(ureq::Error::StatusCode(404)) => return Err(ProviderError::NotFound),
            // Yahoo's edge returns these transiently; the caller decides
            // whether to try again.
            Err(ureq::Error::StatusCode(code)) if (500..600).contains(&code) => {
                return Err(ProviderError::Network(format!("HTTP {code}")))
            }
            Err(ureq::Error::StatusCode(code)) => {
                return Err(ProviderError::Malformed(format!("HTTP {code}")))
            }
            Err(error) => return Err(ProviderError::Network(error.to_string())),
        };

        response
            .body_mut()
            .read_json()
            .map_err(|e| ProviderError::Malformed(e.to_string()))
    }
}

impl Provider for Yahoo {
    fn id(&self) -> &'static str {
        "yahoo"
    }

    fn label(&self) -> &'static str {
        "Yahoo Finance"
    }

    fn delay_minutes(&self, kind: InstrumentKind) -> u32 {
        match kind {
            InstrumentKind::FutureRoot => 10,
            InstrumentKind::Index => 15,
            // Equities and ETFs are near real time, FX and crypto are
            // continuous. None of it is guaranteed, so claim nothing.
            _ => 0,
        }
    }

    fn symbol_for(&self, instrument: &Instrument) -> Option<String> {
        if let Some(explicit) = instrument.override_for(self.id()) {
            return Some(explicit.to_string());
        }
        Some(match instrument.kind {
            InstrumentKind::Index => format!("^{}", instrument.symbol),
            InstrumentKind::FutureRoot => format!("{}=F", instrument.symbol),
            InstrumentKind::Fx => format!("{}=X", instrument.symbol),
            InstrumentKind::Crypto => format!("{}-USD", instrument.symbol),
            InstrumentKind::Equity | InstrumentKind::Etf => {
                // Yahoo spells class shares with a hyphen: BRK.B is BRK-B.
                let base = instrument.symbol.replace('.', "-");
                match &instrument.suffix {
                    Some(suffix) => format!("{base}.{suffix}"),
                    None => base,
                }
            }
        })
    }

    fn capabilities(&self) -> &'static [Capability] {
        CAPABILITIES
    }

    /// Bars for `symbol`, paced and retried.
    ///
    /// Blocks: it sleeps to keep requests apart and between retries, so it
    /// must be called from a worker thread, never the UI thread.
    fn bars(
        &self,
        symbol: &str,
        timeframe: Timeframe,
        since: Option<i64>,
    ) -> Result<Vec<Bar>, ProviderError> {
        self.bars_paced(symbol, timeframe, since, false)
    }

    fn bars_speculative(
        &self,
        symbol: &str,
        timeframe: Timeframe,
        since: Option<i64>,
    ) -> Result<Vec<Bar>, ProviderError> {
        self.bars_paced(symbol, timeframe, since, true)
    }
}

impl Yahoo {
    fn bars_paced(
        &self,
        symbol: &str,
        timeframe: Timeframe,
        since: Option<i64>,
        speculative: bool,
    ) -> Result<Vec<Bar>, ProviderError> {
        let url = Self::url(symbol, timeframe, since)?;

        // Sitting out a cooldown is not a failure worth retrying — the caller
        // shows what it already has.
        let wait = {
            let mut throttle = self.throttle.lock().unwrap_or_else(|e| e.into_inner());
            throttle
                .admit(Instant::now(), speculative)
                .map_err(|()| ProviderError::RateLimited)?
        };
        if let Some(wait) = wait {
            std::thread::sleep(wait);
        }

        let mut attempt = 0;
        loop {
            {
                let mut throttle = self.throttle.lock().unwrap_or_else(|e| e.into_inner());
                throttle.record_request(Instant::now());
            }

            match self.fetch(&url) {
                Ok(body) => {
                    let mut throttle = self.throttle.lock().unwrap_or_else(|e| e.into_inner());
                    throttle.record_success();
                    drop(throttle);
                    return parse_chart(&body).map(|bars| collapse_days(bars, timeframe));
                }
                Err(ProviderError::RateLimited) => {
                    let mut throttle = self.throttle.lock().unwrap_or_else(|e| e.into_inner());
                    throttle.record_throttled(Instant::now());
                    return Err(ProviderError::RateLimited);
                }
                // A dropped connection or a 503 on the way through Yahoo's
                // edge is common enough to be worth one more try.
                Err(error @ (ProviderError::Network(_) | ProviderError::Malformed(_)))
                    if attempt < RETRIES =>
                {
                    let backoff = Duration::from_millis(400 << attempt);
                    attempt += 1;
                    let _ = error;
                    std::thread::sleep(backoff);
                }
                Err(error) => return Err(error),
            }
        }
    }
}

/// Pull bars out of a chart response.
///
/// Yahoo pads its arrays with nulls wherever it has no print, so a bar is only
/// real if every one of its four prices is present. Inventing a value here
/// would put a candle on the chart that never traded.
pub fn parse_chart(body: &serde_json::Value) -> Result<Vec<Bar>, ProviderError> {
    let chart = &body["chart"];
    if let Some(code) = chart["error"]["code"].as_str() {
        return Err(match code {
            "Not Found" => ProviderError::NotFound,
            other => ProviderError::Malformed(other.to_string()),
        });
    }

    let result = chart["result"]
        .get(0)
        .ok_or_else(|| ProviderError::Malformed("no result".into()))?;

    let stamps = result["timestamp"]
        .as_array()
        .ok_or_else(|| ProviderError::Malformed("no timestamps".into()))?;

    let quote = result["indicators"]["quote"]
        .get(0)
        .ok_or_else(|| ProviderError::Malformed("no quote block".into()))?;

    let column = |name: &str| quote[name].as_array().cloned().unwrap_or_default();
    let (open, high, low, close, volume) = (
        column("open"),
        column("high"),
        column("low"),
        column("close"),
        column("volume"),
    );

    let mut bars = Vec::with_capacity(stamps.len());
    for (i, stamp) in stamps.iter().enumerate() {
        let Some(ts) = stamp.as_i64() else { continue };
        let at = |series: &Vec<serde_json::Value>| series.get(i).and_then(|v| v.as_f64());
        let (Some(o), Some(h), Some(l), Some(c)) = (at(&open), at(&high), at(&low), at(&close))
        else {
            continue;
        };
        bars.push(Bar { ts, open: o, high: h, low: l, close: c, volume: at(&volume).unwrap_or(0.0) });
    }

    // Yahoo is ordered oldest first, but nothing promises it.
    bars.sort_by_key(|b| b.ts);
    bars.dedup_by_key(|b| b.ts);
    Ok(bars)
}

/// Fold bars that land on the same day into one, for daily and coarser.
///
/// Yahoo appends a partial bar for the session in progress, stamped with the
/// current time rather than the session's open. On a daily series that is a
/// second bar for today, and anything comparing the last two closes — a
/// change column, say — reads a change of exactly zero.
fn collapse_days(bars: Vec<Bar>, timeframe: Timeframe) -> Vec<Bar> {
    if !matches!(timeframe.unit, Unit::Day | Unit::Week) || bars.len() < 2 {
        return bars;
    }
    let day_of = |ts: i64| ts.div_euclid(86_400);
    let mut out: Vec<Bar> = Vec::with_capacity(bars.len());
    for bar in bars {
        match out.last_mut() {
            // The later bar is the more complete one, but the day's extremes
            // belong to the day, not to whichever slice arrived last.
            Some(last) if day_of(last.ts) == day_of(bar.ts) => {
                last.high = last.high.max(bar.high);
                last.low = last.low.min(bar.low);
                last.close = bar.close;
                last.volume = last.volume.max(bar.volume);
            }
            _ => out.push(bar),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instrument(kind: InstrumentKind, symbol: &str) -> Instrument {
        Instrument {
            symbol: symbol.to_string(),
            name: String::new(),
            kind,
            suffix: None,
            currency: None,
            tier: 0,
            session_origin: 0,
            overrides: Vec::new(),
            exchange: None,
            popularity: 0,
        }
    }

    #[test]
    fn templates_produce_yahoo_symbology() {
        let y = Yahoo::new();
        let cases = [
            (InstrumentKind::Index, "GSPC", "^GSPC"),
            (InstrumentKind::FutureRoot, "GC", "GC=F"),
            (InstrumentKind::Fx, "EURUSD", "EURUSD=X"),
            (InstrumentKind::Crypto, "BTC", "BTC-USD"),
            (InstrumentKind::Equity, "AAPL", "AAPL"),
            (InstrumentKind::Equity, "BRK.B", "BRK-B"),
        ];
        for (kind, symbol, expected) in cases {
            assert_eq!(y.symbol_for(&instrument(kind, symbol)).unwrap(), expected);
        }
    }

    #[test]
    fn a_suffix_is_appended_for_foreign_listings() {
        let y = Yahoo::new();
        let mut inst = instrument(InstrumentKind::Equity, "SAN");
        inst.suffix = Some("MC".into());
        assert_eq!(y.symbol_for(&inst).unwrap(), "SAN.MC");
    }

    #[test]
    fn an_override_beats_the_template() {
        let y = Yahoo::new();
        let mut inst = instrument(InstrumentKind::Index, "DXY");
        inst.overrides = vec![("yahoo".into(), "DX-Y.NYB".into())];
        assert_eq!(y.symbol_for(&inst).unwrap(), "DX-Y.NYB");
    }

    #[test]
    fn urls_encode_and_always_use_an_explicit_window() {
        let full = Yahoo::url("^GSPC", Timeframe::days(1), None).unwrap();
        assert!(full.contains("%5EGSPC"), "{full}");
        assert!(full.contains("interval=1d"), "{full}");
        // Never range=max: it makes Yahoo coarsen a daily series into
        // month-ends without saying so.
        assert!(!full.contains("range="), "{full}");
        assert!(full.contains("period1=") && full.contains("period2="), "{full}");

        let tail = Yahoo::url("GC=F", Timeframe::hours(1), Some(1_700_000_000)).unwrap();
        assert!(tail.contains("GC%3DF"), "{tail}");
        assert!(tail.contains("period1=1700000000"), "{tail}");
    }

    #[test]
    fn intraday_asks_for_less_history_than_daily() {
        assert!(
            Yahoo::full_window_days(Timeframe::minutes(5))
                < Yahoo::full_window_days(Timeframe::days(1))
        );
        assert_eq!(Yahoo::full_window_days(Timeframe::minutes(5)), 60);
    }

    /// Yahoo reads its own limit as strictly inside, and not even the same way
    /// for every interval: sixty days of fifteen-minute bars is a 422 while
    /// sixty days of five-minute bars is fine. Asking for the exact window is
    /// how DIA went blank at 15m with every other resolution working.
    #[test]
    fn a_full_window_stops_short_of_the_limit_it_is_allowed() {
        for timeframe in
            [Timeframe::minutes(5), Timeframe::minutes(15), Timeframe::minutes(30)]
        {
            let url = Yahoo::url("DIA", timeframe, None).unwrap();
            let from: i64 = url
                .split("period1=")
                .nth(1)
                .and_then(|rest| rest.split('&').next())
                .and_then(|v| v.parse().ok())
                .expect("period1");
            let asked = chrono::Utc::now().timestamp() - from;
            let limit = Yahoo::full_window_days(timeframe) * 86_400;
            assert!(asked < limit, "{} asked for the whole window", timeframe.label());
            // And not so far short that a day of history is thrown away.
            assert!(asked > limit - 86_400, "{} gave up too much", timeframe.label());
        }
    }

    #[test]
    fn a_request_never_exceeds_the_per_request_cap() {
        // One-minute bars exist for a month but are served a week at a time.
        // Asking for the month hands back an empty series, which is how a
        // three-minute chart ended up saying "no data for this symbol".
        assert_eq!(Yahoo::full_window_days(Timeframe::minutes(1)), 7);

        let url = Yahoo::url("AAPL", Timeframe::minutes(1), None).unwrap();
        let from: i64 = url
            .split("period1=")
            .nth(1)
            .and_then(|rest| rest.split('&').next())
            .and_then(|v| v.parse().ok())
            .expect("period1");
        let days = (chrono::Utc::now().timestamp() - from) / 86_400;
        assert!((6..=8).contains(&days), "asked for {days} days of one-minute bars");
    }

    #[test]
    fn every_capability_can_be_asked_for_in_one_request() {
        for capability in CAPABILITIES {
            let days = Yahoo::full_window_days(capability.timeframe);
            if let Some(cap) = capability.max_request_days {
                assert!(days <= cap as i64, "{:?}", capability.timeframe);
            }
        }
    }

    #[test]
    fn a_partial_bar_for_today_does_not_become_a_second_day() {
        let bar = |ts, close, volume| Bar {
            ts,
            open: close,
            high: close + 1.0,
            low: close - 1.0,
            close,
            volume,
        };
        // Yesterday, today's session bar, and today's partial update.
        let day = 86_400;
        let bars = vec![
            bar(10 * day, 100.0, 500.0),
            bar(11 * day, 110.0, 400.0),
            bar(11 * day + 50_000, 112.0, 450.0),
        ];
        let out = collapse_days(bars, Timeframe::days(1));
        assert_eq!(out.len(), 2, "today must be one bar");
        assert_eq!(out[1].close, 112.0, "the later close wins");
        assert_eq!(out[1].high, 113.0, "the day keeps its extreme");
        assert_eq!(out[1].volume, 450.0);

        // And the change between the last two closes is no longer zero.
        assert_ne!(out[1].close, out[0].close);
    }

    #[test]
    fn intraday_bars_are_left_alone() {
        let bar = |ts| Bar { ts, open: 1.0, high: 1.0, low: 1.0, close: 1.0, volume: 1.0 };
        let bars = vec![bar(0), bar(300), bar(600)];
        assert_eq!(collapse_days(bars.clone(), Timeframe::minutes(5)).len(), 3);
        // But a daily series with three stamps on one day is one bar.
        assert_eq!(collapse_days(bars, Timeframe::days(1)).len(), 1);
    }

    #[test]
    fn derived_timeframes_are_never_fetched() {
        assert!(Yahoo::url("AAPL", Timeframe::hours(4), None).is_err());
        assert!(Yahoo::url("AAPL", Timeframe::weeks(1), None).is_err());
        let y = Yahoo::new();
        assert!(!y.serves(Timeframe::hours(4)));
        assert!(y.serves(Timeframe::hours(1)));
    }

    #[test]
    fn nulls_are_skipped_rather_than_invented() {
        let body: serde_json::Value = serde_json::from_str(
            r#"{"chart":{"error":null,"result":[{
                 "timestamp":[100,200,300],
                 "indicators":{"quote":[{
                   "open":[1.0,null,3.0],
                   "high":[2.0,null,4.0],
                   "low":[0.5,null,2.5],
                   "close":[1.5,null,3.5],
                   "volume":[10,null,30]}]}}]}}"#,
        )
        .unwrap();
        let bars = parse_chart(&body).unwrap();
        assert_eq!(bars.len(), 2);
        assert_eq!(bars[0].ts, 100);
        assert_eq!(bars[1].ts, 300);
        assert_eq!(bars[1].close, 3.5);
    }

    #[test]
    fn a_missing_volume_is_zero_not_a_dropped_bar() {
        let body: serde_json::Value = serde_json::from_str(
            r#"{"chart":{"result":[{"timestamp":[100],"indicators":{"quote":[{
                 "open":[1.0],"high":[2.0],"low":[0.5],"close":[1.5]}]}}]}}"#,
        )
        .unwrap();
        let bars = parse_chart(&body).unwrap();
        assert_eq!(bars.len(), 1);
        assert_eq!(bars[0].volume, 0.0);
    }

    #[test]
    fn an_error_payload_becomes_not_found() {
        let body: serde_json::Value =
            serde_json::from_str(r#"{"chart":{"error":{"code":"Not Found"},"result":null}}"#)
                .unwrap();
        assert!(matches!(parse_chart(&body), Err(ProviderError::NotFound)));
    }

    #[test]
    fn requests_are_kept_apart() {
        let mut throttle = Throttle::new();
        let t0 = Instant::now();

        assert_eq!(throttle.admit(t0, false), Ok(None), "first request waits for nothing");
        throttle.record_request(t0);

        // Straight away: told to wait out the rest of the gap.
        let wait = throttle.admit(t0, false).unwrap().expect("should wait");
        assert!(wait <= MIN_GAP && wait > Duration::ZERO, "{wait:?}");

        // Once the gap has passed: no wait.
        assert_eq!(throttle.admit(t0 + MIN_GAP, false).unwrap(), None);
    }

    #[test]
    fn speculative_work_is_paced_far_further_apart() {
        let mut throttle = Throttle::new();
        let t0 = Instant::now();
        throttle.record_request(t0);

        // The chart someone is waiting on may go again after the short gap.
        assert_eq!(throttle.admit(t0 + MIN_GAP, false).unwrap(), None);
        // Work nobody asked for waits much longer.
        let wait = throttle.admit(t0 + MIN_GAP, true).unwrap().expect("should wait");
        assert!(wait > Duration::from_millis(500), "{wait:?}");
        assert_eq!(throttle.admit(t0 + MIN_GAP_SPECULATIVE, true).unwrap(), None);
    }

    #[test]
    fn a_429_stops_us_asking_at_all() {
        let mut throttle = Throttle::new();
        let t0 = Instant::now();
        throttle.record_throttled(t0);

        assert_eq!(throttle.admit(t0, false), Err(()), "inside the cooldown");
        assert_eq!(
            throttle.admit(t0 + COOLDOWN_START + Duration::from_secs(1), false),
            Ok(None),
            "cooldown expired"
        );
    }

    #[test]
    fn repeated_throttling_backs_off_and_success_relaxes_it() {
        let mut throttle = Throttle::new();
        let t0 = Instant::now();

        throttle.record_throttled(t0);
        assert_eq!(throttle.cooldown, COOLDOWN_START * 2);
        throttle.record_throttled(t0);
        assert_eq!(throttle.cooldown, COOLDOWN_START * 4);

        throttle.record_success();
        assert_eq!(throttle.cooldown, COOLDOWN_START);
    }

    #[test]
    fn the_backoff_has_a_ceiling() {
        let mut throttle = Throttle::new();
        let t0 = Instant::now();
        for _ in 0..20 {
            throttle.record_throttled(t0);
        }
        assert_eq!(throttle.cooldown, COOLDOWN_MAX);
    }

    #[test]
    fn delays_are_disclosed_per_kind() {
        let y = Yahoo::new();
        assert_eq!(y.delay_minutes(InstrumentKind::FutureRoot), 10);
        assert_eq!(y.delay_minutes(InstrumentKind::Index), 15);
        assert_eq!(y.delay_minutes(InstrumentKind::Equity), 0);
    }
}
