//! The command line.
//!
//! One job: hand the watchlist to something that is not the app. The Omarchy
//! bar widget shells out to this, which is what lets the bar keep working
//! after the window is closed — the alternative is a widget that talks to a
//! running app and goes blank the moment you quit it.

use std::rc::Rc;

use omacharts_engine::providers::Yahoo;
use omacharts_engine::{Instrument, Provider, SearchIndex, Timeframe};

use crate::store::{Store, ROOT_SECTION};

/// How many closes the row's sparkline gets.
///
/// Enough to show a shape, few enough that a watchlist of forty stays a small
/// payload: the widget re-reads this every couple of minutes.
const SPARK_POINTS: usize = 30;

/// How stale a quote may be before a `--refresh` run goes and gets it.
const STALE_AFTER_SECONDS: i64 = 15 * 60;

/// Most symbols one invocation will fetch.
///
/// The bar calls this on a timer, and a run that tries to refresh forty
/// symbols would spend minutes inside the provider's pacing and overlap the
/// next run. Whatever is stalest goes first, so a few runs cover everything.
const MAX_REFRESH: usize = 6;

pub struct Quote {
    pub last: f64,
    pub change: f64,
    pub change_pct: f64,
    pub as_of: i64,
}

/// Read the cached daily bars for an instrument and work out its move.
pub fn quote(store: &Store, provider: &Yahoo, instrument: &Instrument) -> Option<Quote> {
    let key = cache_key(provider, instrument)?;
    let bars = store.load_bars(&key, Timeframe::days(1));
    let (previous, last) = (bars.get(bars.len().checked_sub(2)?)?, bars.last()?);
    let change = last.close - previous.close;
    Some(Quote {
        last: last.close,
        change,
        change_pct: if previous.close == 0.0 { 0.0 } else { change / previous.close * 100.0 },
        as_of: last.ts,
    })
}

pub fn cache_key(provider: &Yahoo, instrument: &Instrument) -> Option<String> {
    provider.symbol_for(instrument).map(|symbol| format!("{}:{symbol}", provider.id()))
}

/// Fetch daily bars for the stalest symbols that need them.
///
/// Speculative, so the provider paces it apart and refuses outright while it
/// is being throttled: a bar widget must never cost the app its rate limit.
fn refresh(store: &Store, provider: &Yahoo, instruments: &[Instrument]) {
    let now = chrono::Utc::now().timestamp();
    let daily = Timeframe::days(1);

    let mut candidates: Vec<(i64, &Instrument, String)> = instruments
        .iter()
        .filter_map(|instrument| {
            let key = cache_key(provider, instrument)?;
            let fetched_at = store.coverage(&key, daily).map(|c| c.fetched_at).unwrap_or(0);
            (now - fetched_at > STALE_AFTER_SECONDS).then_some((fetched_at, instrument, key))
        })
        .collect();
    candidates.sort_by_key(|(fetched_at, _, _)| *fetched_at);

    for (_, instrument, key) in candidates.into_iter().take(MAX_REFRESH) {
        let Some(symbol) = provider.symbol_for(instrument) else { continue };
        let since = store.coverage(&key, daily).map(|c| c.last_ts - 5 * daily.seconds());
        if let Ok(bars) = provider.bars_speculative(&symbol, daily, since) {
            if !bars.is_empty() {
                store.merge_bars(&key, daily, &bars);
            }
        }
    }
}

/// The watchlist as JSON, for the bar widget.
pub fn watchlist_json(refresh_first: bool) -> String {
    let Ok(store) = Store::open() else {
        return r#"{"sections":[],"error":"could not open the database"}"#.to_string();
    };
    let index = Rc::new(SearchIndex::new(omacharts_engine::symbols::seed()));
    let provider = Yahoo::new();

    let sections = store.watchlist();
    if refresh_first {
        let instruments: Vec<Instrument> = sections
            .iter()
            .flat_map(|section| section.entries.iter())
            .filter_map(|entry| index.find(&entry.symbol, entry.suffix.as_deref()).cloned())
            .collect();
        refresh(&store, &provider, &instruments);
    }

    let mut out = String::from("{\"sections\":[");
    for (s, section) in sections.iter().enumerate() {
        if s > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "{{\"name\":{},\"root\":{},\"entries\":[",
            json_string(&section.name),
            section.id == ROOT_SECTION
        ));
        for (e, entry) in section.entries.iter().enumerate() {
            let Some(instrument) = index.find(&entry.symbol, entry.suffix.as_deref()) else {
                continue;
            };
            if e > 0 {
                out.push(',');
            }
            out.push_str(&entry_json(&store, &provider, instrument));
        }
        out.push_str("]}");
    }
    out.push_str(&format!(
        "],\"colors\":{},\"updatedAt\":{}}}",
        colors_json(),
        chrono::Utc::now().timestamp()
    ));
    out
}

/// The direction colours, derived exactly as the app derives them.
///
/// Sent with the data rather than hardcoded in the widget, so the bar and the
/// window cannot disagree about what up looks like — and so changing the
/// desktop theme moves both.
fn colors_json() -> String {
    let home = crate::store::home();
    let theme = omacharts_engine::omarchy::current(&home)
        .unwrap_or_else(|| omacharts_engine::theme::builtin_themes()[0].clone());
    let bars = omacharts_engine::theme_bars(&theme);
    use omacharts_engine::Direction;
    format!(
        "{{\"up\":{},\"down\":{},\"flat\":{},\"foreground\":{}}}",
        json_string(bars.outline(Direction::Up)),
        json_string(bars.outline(Direction::Down)),
        json_string(bars.outline(Direction::Flat)),
        json_string(&theme.ui.text),
    )
}

fn entry_json(store: &Store, provider: &Yahoo, instrument: &Instrument) -> String {
    let mut fields = format!(
        "{{\"symbol\":{},\"display\":{},\"name\":{},\"kind\":{}",
        json_string(&instrument.symbol),
        json_string(&instrument.display_symbol()),
        json_string(&instrument.name),
        json_string(instrument.kind.label()),
    );
    if let Some(key) = cache_key(provider, instrument) {
        let bars = store.load_bars(&key, Timeframe::days(1));
        let tail = &bars[bars.len().saturating_sub(SPARK_POINTS)..];
        if tail.len() >= 2 {
            let points: Vec<String> =
                tail.iter().map(|bar| format!("{:.6}", bar.close)).collect();
            fields.push_str(&format!(",\"spark\":[{}]", points.join(",")));
        }
    }
    match quote(store, provider, instrument) {
        // A quote we do not have is absent rather than zero. Zero is a price.
        Some(q) => fields.push_str(&format!(
            ",\"last\":{:.6},\"change\":{:.6},\"changePct\":{:.4},\"asOf\":{}",
            q.last, q.change, q.change_pct, q.as_of
        )),
        None => fields.push_str(",\"last\":null,\"change\":null,\"changePct\":null,\"asOf\":null"),
    }
    fields.push('}');
    fields
}

/// Minimal JSON string escaping — enough for symbols and section names.
fn json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

pub const USAGE: &str = "\
omacharts — market charts

    omacharts                      open the app
    omacharts watchlist [--refresh]
                                   print the watchlist as JSON
    omacharts --help               this
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_payload_carries_the_themes_direction_colours() {
        let parsed: serde_json::Value = serde_json::from_str(&colors_json()).unwrap();
        for key in ["up", "down", "flat", "foreground"] {
            let value = parsed[key].as_str().unwrap_or_default();
            assert!(value.starts_with('#') && value.len() >= 7, "{key}: {value:?}");
        }
        assert_ne!(parsed["up"], parsed["down"], "up and down must differ");
    }

    #[test]
    fn strings_are_escaped() {
        assert_eq!(json_string("plain"), "\"plain\"");
        assert_eq!(json_string("say \"hi\""), "\"say \\\"hi\\\"\"");
        assert_eq!(json_string("back\\slash"), "\"back\\\\slash\"");
        assert_eq!(json_string("two\nlines"), "\"two\\nlines\"");
        assert_eq!(json_string("bell\u{7}"), "\"bell\\u0007\"");
    }

    #[test]
    fn a_section_name_with_quotes_does_not_break_the_json() {
        let store = Store::memory().unwrap();
        let id = store.add_section("My \"best\" picks").unwrap();
        store.add_to_section(id, "ES", None);

        let name = json_string(&store.watchlist()[0].name);
        let parsed: serde_json::Value = serde_json::from_str(&name).unwrap();
        assert_eq!(parsed.as_str().unwrap(), "My \"best\" picks");
    }

    #[test]
    fn an_entry_without_bars_reports_no_quote_rather_than_zero() {
        let store = Store::memory().unwrap();
        let provider = Yahoo::new();
        let index = SearchIndex::new(omacharts_engine::symbols::seed());
        let instrument = index.find("ES", None).unwrap();

        let json = entry_json(&store, &provider, instrument);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(parsed["last"].is_null(), "a missing price must not read as 0");
        assert_eq!(parsed["symbol"], "ES");
        assert_eq!(parsed["kind"], "Futures");
    }

    #[test]
    fn a_row_carries_a_short_series_for_its_sparkline() {
        let store = Store::memory().unwrap();
        let provider = Yahoo::new();
        let index = SearchIndex::new(omacharts_engine::symbols::seed());
        let instrument = index.find("ES", None).unwrap();
        let key = cache_key(&provider, instrument).unwrap();

        // More history than the sparkline wants, so it has to take the tail.
        let bars: Vec<omacharts_engine::Bar> = (0..100)
            .map(|i| omacharts_engine::Bar {
                ts: (i + 1) * 86_400,
                open: i as f64,
                high: i as f64,
                low: i as f64,
                close: i as f64,
                volume: 1.0,
            })
            .collect();
        store.write_bars(&key, Timeframe::days(1), &bars);

        let parsed: serde_json::Value =
            serde_json::from_str(&entry_json(&store, &provider, instrument)).unwrap();
        let spark = parsed["spark"].as_array().unwrap();
        assert_eq!(spark.len(), SPARK_POINTS);
        assert_eq!(spark.last().unwrap().as_f64().unwrap(), 99.0, "the tail, not the start");
    }

    #[test]
    fn a_row_with_nothing_to_draw_has_no_series() {
        let store = Store::memory().unwrap();
        let provider = Yahoo::new();
        let index = SearchIndex::new(omacharts_engine::symbols::seed());
        let instrument = index.find("ES", None).unwrap();
        let parsed: serde_json::Value =
            serde_json::from_str(&entry_json(&store, &provider, instrument)).unwrap();
        assert!(parsed["spark"].is_null(), "an absent series beats an empty one");
    }

    #[test]
    fn an_entry_with_bars_reports_its_move() {
        let store = Store::memory().unwrap();
        let provider = Yahoo::new();
        let index = SearchIndex::new(omacharts_engine::symbols::seed());
        let instrument = index.find("ES", None).unwrap();
        let key = cache_key(&provider, instrument).unwrap();

        let bar = |ts, close| omacharts_engine::Bar {
            ts,
            open: close,
            high: close,
            low: close,
            close,
            volume: 1.0,
        };
        store.write_bars(&key, Timeframe::days(1), &[bar(86_400, 100.0), bar(172_800, 110.0)]);

        let parsed: serde_json::Value =
            serde_json::from_str(&entry_json(&store, &provider, instrument)).unwrap();
        assert_eq!(parsed["last"].as_f64().unwrap(), 110.0);
        assert_eq!(parsed["change"].as_f64().unwrap(), 10.0);
        assert!((parsed["changePct"].as_f64().unwrap() - 10.0).abs() < 1e-6);
    }

    #[test]
    fn one_bar_is_not_enough_for_a_change() {
        let store = Store::memory().unwrap();
        let provider = Yahoo::new();
        let index = SearchIndex::new(omacharts_engine::symbols::seed());
        let instrument = index.find("ES", None).unwrap();
        let key = cache_key(&provider, instrument).unwrap();
        store.write_bars(
            &key,
            Timeframe::days(1),
            &[omacharts_engine::Bar {
                ts: 86_400,
                open: 100.0,
                high: 100.0,
                low: 100.0,
                close: 100.0,
                volume: 1.0,
            }],
        );
        assert!(quote(&store, &provider, instrument).is_none());
    }
}
