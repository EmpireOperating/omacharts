//! The instrument inventory and the search over it.
//!
//! Search never touches a database. The whole inventory is loaded once into a
//! flat vector with lowercased keys and first-character buckets; a query scans
//! a few hundred candidates, not the whole universe. The budget is under a
//! millisecond so that search-as-you-type is always safe.
//!
//! Ranking matters more than the index. Typing `GC` must surface gold futures,
//! not some microcap that happens to share the letters — which is what `tier`
//! is for.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstrumentKind {
    Index,
    FutureRoot,
    Equity,
    Etf,
    Fx,
    Crypto,
}

impl InstrumentKind {
    pub fn label(self) -> &'static str {
        match self {
            InstrumentKind::Index => "Index",
            InstrumentKind::FutureRoot => "Futures",
            InstrumentKind::Equity => "Stock",
            InstrumentKind::Etf => "ETF",
            InstrumentKind::Fx => "FX",
            InstrumentKind::Crypto => "Crypto",
        }
    }

    /// Does this trade around the clock?
    ///
    /// A market that never closes has no auction open — today's open is
    /// yesterday's close — which matters anywhere a bar's open is treated as
    /// a separate price rather than a continuation.
    pub fn is_continuous(self) -> bool {
        matches!(self, InstrumentKind::Fx | InstrumentKind::Crypto)
    }

    pub fn from_key(key: &str) -> Option<InstrumentKind> {
        Some(match key {
            "index" => InstrumentKind::Index,
            "future_root" => InstrumentKind::FutureRoot,
            "equity" => InstrumentKind::Equity,
            "etf" => InstrumentKind::Etf,
            "fx" => InstrumentKind::Fx,
            "crypto" => InstrumentKind::Crypto,
            _ => return None,
        })
    }
}

/// One tradeable thing, in nobody's symbology in particular.
///
/// `symbol` is canonical and neutral: `GSPC`, not `^GSPC`; `ES`, not `ES=F`.
/// Turning it into a provider's spelling is the provider's job.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Instrument {
    pub symbol: String,
    pub name: String,
    pub kind: InstrumentKind,
    /// Yahoo-style exchange suffix for non-US listings: `L`, `DE`, `MC`…
    pub suffix: Option<String>,
    pub currency: Option<String>,
    /// 0 is a curated major, 3 is excluded from search.
    pub tier: u8,
    /// Seconds past midnight UTC that this instrument's session opens. Drives
    /// session-aware resampling; 0 for anything that trades a normal day.
    pub session_origin: i64,
    /// Per-provider spellings that no template produces.
    pub overrides: Vec<(String, String)>,
    /// Where it trades, when the source knew. Unset for anything whose venue
    /// can be worked out from its suffix or its kind.
    pub exchange: Option<String>,
}

impl Instrument {
    pub fn override_for(&self, adapter: &str) -> Option<&str> {
        self.overrides
            .iter()
            .find(|(a, _)| a == adapter)
            .map(|(_, s)| s.as_str())
    }

    /// What the UI shows as the instrument's ticker.
    /// Where it trades, in the form people say out loud.
    ///
    /// Taken from the listing when the source gave one, and otherwise worked
    /// out: a Yahoo suffix names a venue, and everything with no suffix and no
    /// listing behind it is one of the handful of kinds that has an obvious
    /// home.
    pub fn exchange_label(&self) -> Option<&str> {
        if let Some(named) = self.exchange.as_deref() {
            return Some(named);
        }
        if let Some(suffix) = self.suffix.as_deref() {
            return Some(match suffix {
                "MC" => "BME",
                "L" => "LSE",
                "DE" => "XETRA",
                "PA" => "Euronext",
                "SW" => "SIX",
                "T" => "TSE",
                "TO" => "TSX",
                "AS" => "Euronext",
                "MI" => "Borsa Italiana",
                "HK" => "HKEX",
                other => other,
            });
        }
        match self.kind {
            InstrumentKind::Fx => Some("FX"),
            InstrumentKind::Crypto => Some("Crypto"),
            _ => None,
        }
    }

    pub fn display_symbol(&self) -> String {
        match &self.suffix {
            Some(s) => format!("{}.{}", self.symbol, s),
            None => self.symbol.clone(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SearchHit {
    pub index: usize,
    pub score: i32,
}

/// Lowercased keys and first-character buckets over an instrument list.
pub struct SearchIndex {
    items: Vec<Instrument>,
    symbol_lc: Vec<String>,
    name_lc: Vec<String>,
    /// First character of the symbol, and of every word of the name, to the
    /// items it could match.
    buckets: HashMap<char, Vec<u32>>,
}

impl SearchIndex {
    pub fn new(items: Vec<Instrument>) -> SearchIndex {
        let mut symbol_lc = Vec::with_capacity(items.len());
        let mut name_lc = Vec::with_capacity(items.len());
        let mut buckets: HashMap<char, Vec<u32>> = HashMap::new();

        for (i, item) in items.iter().enumerate() {
            let sym = item.symbol.to_lowercase();
            let name = item.name.to_lowercase();

            let mut firsts: Vec<char> = Vec::new();
            if let Some(c) = sym.chars().next() {
                firsts.push(c);
            }
            for word in name.split(|c: char| !c.is_alphanumeric()) {
                if let Some(c) = word.chars().next() {
                    firsts.push(c);
                }
            }
            firsts.sort_unstable();
            firsts.dedup();
            for c in firsts {
                buckets.entry(c).or_default().push(i as u32);
            }

            symbol_lc.push(sym);
            name_lc.push(name);
        }

        SearchIndex { items, symbol_lc, name_lc, buckets }
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn get(&self, index: usize) -> Option<&Instrument> {
        self.items.get(index)
    }

    pub fn items(&self) -> &[Instrument] {
        &self.items
    }

    /// Exact lookup by canonical symbol and suffix.
    ///
    /// The watchlist stores what the user picked, not an index position, so it
    /// survives the inventory growing or being regenerated.
    pub fn find(&self, symbol: &str, suffix: Option<&str>) -> Option<&Instrument> {
        self.items
            .iter()
            .find(|i| i.symbol == symbol && i.suffix.as_deref() == suffix)
    }

    /// The curated majors, for an empty search field.
    pub fn featured(&self, limit: usize) -> Vec<SearchHit> {
        let mut hits: Vec<SearchHit> = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, i)| i.tier == 0)
            .map(|(index, _)| SearchHit { index, score: 0 })
            .collect();
        hits.truncate(limit);
        hits
    }

    /// Best matches for `query`, best first.
    pub fn search(&self, query: &str, limit: usize) -> Vec<SearchHit> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return self.featured(limit);
        }
        let Some(first) = q.chars().next() else {
            return Vec::new();
        };
        let Some(candidates) = self.buckets.get(&first) else {
            return Vec::new();
        };

        let mut hits: Vec<SearchHit> = Vec::new();
        for &i in candidates {
            let i = i as usize;
            if self.items[i].tier >= 3 {
                continue;
            }
            if let Some(score) = self.score(i, &q) {
                hits.push(SearchHit { index: i, score });
            }
        }
        hits.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then_with(|| self.symbol_lc[a.index].len().cmp(&self.symbol_lc[b.index].len()))
                .then_with(|| self.symbol_lc[a.index].cmp(&self.symbol_lc[b.index]))
        });
        hits.truncate(limit);
        hits
    }

    /// `None` when the query does not match at all.
    ///
    /// The tiers are what make this feel right: an exact ticker always wins,
    /// but between two equally good textual matches the one people actually
    /// meant — an index, a front-month future, a mega-cap — comes first.

    fn score(&self, i: usize, q: &str) -> Option<i32> {
        let symbol = &self.symbol_lc[i];
        let name = &self.name_lc[i];
        let item = &self.items[i];

        let textual = if symbol == q {
            1000
        } else if symbol.starts_with(q) {
            700 - (symbol.len() - q.len()).min(20) as i32 * 4
        } else if name.split(|c: char| !c.is_alphanumeric()).any(|w| w == q) {
            500
        } else if name.starts_with(q) {
            450
        } else if symbol.contains(q) {
            320
        } else if name
            .split(|c: char| !c.is_alphanumeric())
            .any(|w| w.starts_with(q))
        {
            300
        } else if name.contains(q) {
            180
        } else {
            return None;
        };

        let tier_weight = match item.tier {
            0 => 120,
            1 => 60,
            _ => 0,
        };
        // Indexes and futures are what this app is for; nudge them up when the
        // textual match is otherwise a tie.
        let kind_weight = match item.kind {
            InstrumentKind::Index | InstrumentKind::FutureRoot => 30,
            InstrumentKind::Etf => 12,
            _ => 0,
        };
        Some(textual + tier_weight + kind_weight)
    }
}

/// The curated inventory shipped in the binary.
///
/// Enough to use the app the moment it is installed, and the thing search is
/// tuned against. The generated database widens this to every US listing
/// without changing anything here.
pub fn seed() -> Vec<Instrument> {
    parse_seed(include_str!("seed.tsv"))
}

/// `kind<TAB>symbol<TAB>name<TAB>suffix<TAB>currency<TAB>tier<TAB>session_origin<TAB>yahoo_override`
pub fn parse_seed(text: &str) -> Vec<Instrument> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 6 {
            continue;
        }
        let Some(kind) = InstrumentKind::from_key(f[0]) else {
            continue;
        };
        let blank = |s: &str| if s.is_empty() || s == "-" { None } else { Some(s.to_string()) };
        out.push(Instrument {
            kind,
            symbol: f[1].to_string(),
            name: f[2].to_string(),
            suffix: blank(f[3]),
            currency: blank(f[4]),
            tier: f[5].parse().unwrap_or(2),
            session_origin: f.get(6).and_then(|v| v.parse().ok()).unwrap_or(0),
            overrides: match f.get(7).copied().and_then(blank) {
                Some(sym) => vec![("yahoo".to_string(), sym)],
                None => Vec::new(),
            },
            exchange: f.get(8).copied().and_then(blank),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index() -> SearchIndex {
        SearchIndex::new(seed())
    }

    #[test]
    fn the_seed_parses() {
        let items = seed();
        assert!(items.len() > 150, "seed looks short: {}", items.len());
        assert!(items.iter().any(|i| i.symbol == "GSPC"));
        assert!(items.iter().any(|i| i.symbol == "ES"));
    }

    #[test]
    fn gc_finds_gold_futures_first() {
        let idx = index();
        let hits = idx.search("gc", 5);
        let first = idx.get(hits[0].index).unwrap();
        assert_eq!(first.symbol, "GC", "got {first:?}");
        assert_eq!(first.kind, InstrumentKind::FutureRoot);
    }

    #[test]
    fn an_exact_ticker_wins() {
        let idx = index();
        for ticker in ["aapl", "es", "spy", "vix"] {
            let hits = idx.search(ticker, 5);
            assert!(!hits.is_empty(), "no hits for {ticker}");
            assert_eq!(
                idx.get(hits[0].index).unwrap().symbol.to_lowercase(),
                ticker,
                "wrong winner for {ticker}"
            );
        }
    }

    #[test]
    fn names_are_searchable() {
        let idx = index();
        let hits = idx.search("apple", 5);
        assert_eq!(idx.get(hits[0].index).unwrap().symbol, "AAPL");

        let hits = idx.search("gold", 5);
        assert!(!hits.is_empty());
    }

    #[test]
    fn an_empty_query_shows_the_majors() {
        let idx = index();
        let hits = idx.search("", 10);
        assert_eq!(hits.len(), 10);
        assert!(hits.iter().all(|h| idx.get(h.index).unwrap().tier == 0));
    }

    #[test]
    fn well_known_tickers_beat_obscure_ones() {
        let idx = index();
        // Each of these is typed constantly and must win its prefix outright.
        for (query, expected) in [
            ("gc", "GC"),     // gold futures, not a microcap sharing the letters
            ("es", "ES"),     // E-mini S&P
            ("nq", "NQ"),
            ("cl", "CL"),
            ("sp", "SPY"),    // the ETF people mean when they type "sp"
            ("vi", "VIX"),
            ("bt", "BTC"),
            ("eur", "EURUSD"),
        ] {
            let hits = idx.search(query, 5);
            assert!(!hits.is_empty(), "no hits for {query}");
            assert_eq!(
                idx.get(hits[0].index).unwrap().symbol,
                expected,
                "{query} should surface {expected}, got {:?}",
                hits.iter().map(|h| &idx.get(h.index).unwrap().symbol).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn full_names_are_searchable_too() {
        let idx = index();
        for (query, expected) in [
            ("gold", "GC"),
            ("nvidia", "NVDA"),
            ("bitcoin", "BTC"),
            ("crude", "CL"),
            ("santander", "SAN"),
            ("nasdaq 100", "NDX"),
            ("volatility", "VIX"),
        ] {
            let hits = idx.search(query, 8);
            assert!(!hits.is_empty(), "no hits for {query}");
            let symbols: Vec<&str> =
                hits.iter().map(|h| idx.get(h.index).unwrap().symbol.as_str()).collect();
            assert!(symbols.contains(&expected), "{query} -> {symbols:?}, wanted {expected}");
        }
    }

    #[test]
    fn a_tier_zero_match_outranks_a_better_textual_tier_two_match() {
        // "micro" prefixes several tier-2 futures; the point is only that
        // ranking never puts an obscure instrument above a curated major when
        // the textual quality is comparable.
        let idx = index();
        let hits = idx.search("s", 10);
        let top: Vec<u8> = hits.iter().take(3).map(|h| idx.get(h.index).unwrap().tier).collect();
        assert!(top.iter().all(|&t| t <= 1), "top of 's' was tiers {top:?}");
    }

    #[test]
    fn search_is_fast_enough_to_run_on_every_keystroke() {
        let idx = index();
        let queries = ["a", "ap", "app", "appl", "g", "gc", "gol", "e", "es", "spy"];
        let start = std::time::Instant::now();
        let rounds = 200;
        for _ in 0..rounds {
            for q in queries {
                let _ = idx.search(q, 20);
            }
        }
        let per_query = start.elapsed() / (rounds * queries.len() as u32);
        // The budget is a millisecond; anything near it means the index
        // regressed into a full scan.
        assert!(per_query < std::time::Duration::from_micros(500), "{per_query:?} per query");
    }

    #[test]
    fn only_the_round_the_clock_markets_are_continuous() {
        assert!(InstrumentKind::Fx.is_continuous());
        assert!(InstrumentKind::Crypto.is_continuous());
        for kind in [
            InstrumentKind::Equity,
            InstrumentKind::Etf,
            InstrumentKind::Index,
            InstrumentKind::FutureRoot,
        ] {
            assert!(!kind.is_continuous(), "{kind:?}");
        }
    }

    #[test]
    fn exact_lookup_distinguishes_listings() {
        let idx = index();
        assert_eq!(idx.find("AAPL", None).unwrap().name, "Apple");
        assert_eq!(idx.find("SAN", Some("MC")).unwrap().name, "Banco Santander");
        // The Madrid listing must not answer a lookup for a US one.
        assert!(idx.find("SAN", None).is_none());
        assert!(idx.find("NOPE", None).is_none());
    }

    #[test]
    fn nonsense_matches_nothing() {
        assert!(index().search("zzzzqq", 5).is_empty());
    }
}
