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

use crate::bars::{Bar, Timeframe};
use crate::provider::{Capability, Provider, ProviderError};
use crate::symbols::{Instrument, InstrumentKind};

const ENDPOINT: &str = "https://query1.finance.yahoo.com/v8/finance/chart";

/// Yahoo serves nothing without a browser-shaped agent.
const AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 \
                     (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

const CAPABILITIES: &[Capability] = &[
    Capability { timeframe: Timeframe::M5, history_days: Some(60) },
    Capability { timeframe: Timeframe::M15, history_days: Some(60) },
    Capability { timeframe: Timeframe::H1, history_days: Some(730) },
    Capability { timeframe: Timeframe::D1, history_days: None },
];

#[derive(Default)]
pub struct Yahoo {
    timeout: Option<std::time::Duration>,
}

impl Yahoo {
    pub fn new() -> Yahoo {
        Yahoo { timeout: Some(std::time::Duration::from_secs(20)) }
    }

    /// Yahoo's own spelling of a timeframe.
    fn interval(timeframe: Timeframe) -> Option<&'static str> {
        Some(match timeframe {
            Timeframe::M5 => "5m",
            Timeframe::M15 => "15m",
            Timeframe::H1 => "1h",
            Timeframe::D1 => "1d",
            // Folded from a native timeframe by the caller.
            Timeframe::H4 | Timeframe::W1 => return None,
        })
    }

    /// The whole history Yahoo will serve at this timeframe.
    fn full_range(timeframe: Timeframe) -> &'static str {
        match timeframe {
            Timeframe::M5 | Timeframe::M15 => "60d",
            Timeframe::H1 => "2y",
            _ => "max",
        }
    }

    fn url(symbol: &str, timeframe: Timeframe, since: Option<i64>) -> Result<String, ProviderError> {
        let interval = Self::interval(timeframe).ok_or_else(|| {
            ProviderError::Unsupported(format!("{} is folded, not fetched", timeframe.label()))
        })?;
        let encoded = encode(symbol);
        Ok(match since {
            // An explicit window is how a tail fetch stays small: ask for what
            // is missing, not for a range that re-sends what we already have.
            Some(from) => {
                let to = chrono::Utc::now().timestamp() + timeframe.seconds();
                format!(
                    "{ENDPOINT}/{encoded}?interval={interval}&period1={from}&period2={to}"
                )
            }
            None => format!(
                "{ENDPOINT}/{encoded}?interval={interval}&range={}",
                Self::full_range(timeframe)
            ),
        })
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

    fn bars(
        &self,
        symbol: &str,
        timeframe: Timeframe,
        since: Option<i64>,
    ) -> Result<Vec<Bar>, ProviderError> {
        let url = Self::url(symbol, timeframe, since)?;

        let mut request = ureq::get(&url).header("User-Agent", AGENT);
        if let Some(timeout) = self.timeout {
            request = request.config().timeout_global(Some(timeout)).build();
        }

        let mut response = match request.call() {
            Ok(response) => response,
            Err(ureq::Error::StatusCode(429)) => return Err(ProviderError::RateLimited),
            Err(ureq::Error::StatusCode(404)) => return Err(ProviderError::NotFound),
            Err(error) => return Err(ProviderError::Network(error.to_string())),
        };

        let body: serde_json::Value = response
            .body_mut()
            .read_json()
            .map_err(|e| ProviderError::Malformed(e.to_string()))?;

        parse_chart(&body)
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
    fn urls_encode_and_window_correctly() {
        let full = Yahoo::url("^GSPC", Timeframe::D1, None).unwrap();
        assert!(full.contains("%5EGSPC"), "{full}");
        assert!(full.contains("interval=1d") && full.contains("range=max"), "{full}");

        let tail = Yahoo::url("GC=F", Timeframe::H1, Some(1_700_000_000)).unwrap();
        assert!(tail.contains("GC%3DF"), "{tail}");
        assert!(tail.contains("period1=1700000000"), "{tail}");
        assert!(!tail.contains("range="), "{tail}");
    }

    #[test]
    fn derived_timeframes_are_never_fetched() {
        assert!(Yahoo::url("AAPL", Timeframe::H4, None).is_err());
        assert!(Yahoo::url("AAPL", Timeframe::W1, None).is_err());
        let y = Yahoo::new();
        assert!(!y.serves(Timeframe::H4));
        assert!(y.serves(Timeframe::H1));
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
    fn delays_are_disclosed_per_kind() {
        let y = Yahoo::new();
        assert_eq!(y.delay_minutes(InstrumentKind::FutureRoot), 10);
        assert_eq!(y.delay_minutes(InstrumentKind::Index), 15);
        assert_eq!(y.delay_minutes(InstrumentKind::Equity), 0);
    }
}
