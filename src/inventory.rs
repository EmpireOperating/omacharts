//! The symbol inventory, and when it arrives.
//!
//! Two halves. The curated one is written by hand and compiled in: a few
//! hundred instruments carrying the tier weights that make `GC` mean gold
//! futures, the session origins that make a four-hour bar bucket correctly,
//! and every future, currency pair and crypto, none of which appear in any
//! listings feed. The generated one is every US-listed equity and ETF, built
//! from Nasdaq Trader's daily files by `tools/build_listings.py`.
//!
//! Only the curated half is ready when the window opens. The other eleven
//! thousand are parsed on a thread afterwards and swapped in, because five
//! milliseconds on the startup path is five milliseconds, and the names
//! somebody is most likely to type are in the half that is already there.
//!
//! A file in the data directory beats the compiled copy when it exists, which
//! is how the inventory can be refreshed without a new binary.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;

use omacharts_engine::{Instrument, SearchIndex};

/// Every US listing, built in CI rather than at runtime: a failed download is
/// then a red build rather than a broken app.
const LISTINGS: &str = include_str!("../crates/omacharts-engine/src/listings.tsv");

/// Where a refreshed inventory would be written.
pub fn listings_path(home: &std::path::Path) -> PathBuf {
    home.join(".local/share/omacharts/listings.tsv")
}

/// The index everything searches, replaced once the long tail has loaded.
///
/// An `Rc` inside the cell rather than the index itself, so a reader takes a
/// cheap clone and holds no borrow while it searches — a borrow held across a
/// GTK callback is a panic waiting for the moment the index is swapped.
#[derive(Clone)]
pub struct Inventory(Rc<RefCell<Rc<SearchIndex>>>);

impl Inventory {
    /// The curated half, ready immediately.
    pub fn curated() -> Inventory {
        Inventory(Rc::new(RefCell::new(Rc::new(SearchIndex::new(
            omacharts_engine::symbols::seed(),
        )))))
    }

    pub fn get(&self) -> Rc<SearchIndex> {
        self.0.borrow().clone()
    }

    pub fn len(&self) -> usize {
        self.get().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// One instrument by its neutral symbol and optional exchange suffix.
    pub fn find(&self, symbol: &str, suffix: Option<&str>) -> Option<Instrument> {
        self.get().find(symbol, suffix).cloned()
    }

    fn replace(&self, index: SearchIndex) {
        *self.0.borrow_mut() = Rc::new(index);
    }
}

/// The generated listings and the curated rows, as one list.
///
/// A curated row wins any collision, because it is the one carrying a tier, a
/// session origin and a provider override — judgements no feed can make. AAPL
/// is in both files and must come out tier 0.
///
/// But winning is not the same as arriving alone. The feed knows two things
/// about AAPL that the hand-written row does not: which venue lists it, and
/// that it is the most searched ticker there is. So a curated row inherits
/// whatever it left blank from the listing it displaces, and keeps everything
/// it filled in. Without this the two hundred symbols people actually type
/// would be the only ones with no venue and no ranking — the exact inverse of
/// what either column is for.
///
/// Only from a listing of its own kind, though. `ES` is the E-mini and also
/// Eversource Energy; `MGC` is micro gold and also a Vanguard fund. Sharing
/// four letters with a stock is not a reason to claim its exchange, and the
/// futures root would have gone out labelled NYSE.
pub fn merge(listings: &str, mut curated: Vec<Instrument>) -> Vec<Instrument> {
    let mut items = omacharts_engine::symbols::parse_seed(listings);

    let listed: HashMap<(&str, Option<&str>), &Instrument> =
        items.iter().map(|i| ((i.symbol.as_str(), i.suffix.as_deref()), i)).collect();
    for row in &mut curated {
        let inherited = listed
            .get(&(row.symbol.as_str(), row.suffix.as_deref()))
            .filter(|l| l.kind == row.kind)
            .map(|l| (l.exchange.clone(), l.popularity));
        let Some((exchange, popularity)) = inherited else {
            continue;
        };
        if row.exchange.is_none() {
            row.exchange = exchange;
        }
        if row.popularity == 0 {
            row.popularity = popularity;
        }
    }

    let won: HashSet<(&str, Option<&str>)> =
        curated.iter().map(|c| (c.symbol.as_str(), c.suffix.as_deref())).collect();
    items.retain(|l| !won.contains(&(l.symbol.as_str(), l.suffix.as_deref())));
    items.extend(curated);
    items
}

/// Parse the listings off the main thread and hand back one merged index.
pub fn load_in_background(inventory: Inventory, done: impl Fn(usize) + 'static) {
    let (sender, receiver) = async_channel::bounded::<Vec<Instrument>>(1);
    let home = crate::store::home();

    std::thread::spawn(move || {
        // A refreshed file if there is one, the compiled copy otherwise. A
        // download that never happened, or happened and went wrong, leaves the
        // app exactly as it shipped rather than without an inventory.
        let text = std::fs::read_to_string(listings_path(&home))
            .unwrap_or_else(|_| LISTINGS.to_string());
        let _ = sender.send_blocking(merge(&text, omacharts_engine::symbols::seed()));
    });

    glib::spawn_future_local(async move {
        if let Ok(items) = receiver.recv().await {
            let count = items.len();
            inventory.replace(SearchIndex::new(items));
            done(count);
        }
    });
}

use gtk::glib;

#[cfg(test)]
mod tests {
    use super::*;

    /// The generated file is written from a listings feed that knows nothing
    /// about how well known a symbol is, so a name in both files has to come
    /// back as the curated one or search stops ranking.
    #[test]
    fn a_curated_row_beats_a_generated_one() {
        let curated = omacharts_engine::symbols::seed();
        let merged = merge(LISTINGS, curated.clone());

        let aapl: Vec<&Instrument> = merged.iter().filter(|i| i.symbol == "AAPL").collect();
        assert_eq!(aapl.len(), 1, "AAPL is in both files and must appear once");
        assert_eq!(aapl[0].tier, 0, "and must keep the curated tier");

        for one in &curated {
            assert!(
                merged.iter().any(|m| m.symbol == one.symbol && m.tier == one.tier),
                "{} lost its curated row",
                one.symbol
            );
        }
    }

    /// The curated half is written by hand and carries no venue and no
    /// ranking, so without inheritance the two hundred symbols people actually
    /// type would be the only ones missing both — AAPL showing no exchange
    /// while every AAPL-derived ETF showed NASDAQ, and the most searched
    /// ticker there is scoring below a microcap.
    #[test]
    fn a_curated_row_inherits_what_it_left_blank() {
        let merged = merge(LISTINGS, omacharts_engine::symbols::seed());
        let aapl = merged.iter().find(|i| i.symbol == "AAPL").expect("no AAPL");

        assert_eq!(aapl.tier, 0, "the curated tier is the whole point and must survive");
        assert_eq!(aapl.name, "Apple", "as is the curated name");
        assert_eq!(aapl.exchange.as_deref(), Some("NASDAQ"), "venue comes from the listing");
        assert_eq!(aapl.popularity, 9, "and so does the ranking");
    }

    /// `ES` is the E-mini S&P and also Eversource Energy. One ticker, two
    /// unrelated things, and the curated row must not come out wearing the
    /// stock's exchange.
    #[test]
    fn a_curated_row_inherits_nothing_from_another_kind() {
        let merged = merge(LISTINGS, omacharts_engine::symbols::seed());
        let es = merged.iter().find(|i| i.symbol == "ES").expect("no ES");
        assert_eq!(es.kind, omacharts_engine::InstrumentKind::FutureRoot);
        assert_eq!(es.exchange, None, "a futures root does not list on NYSE");
        assert_eq!(es.popularity, 0, "nor does it borrow a utility's market cap");
    }

    #[test]
    fn the_generated_half_is_there_and_ranked_below() {
        let merged = merge(LISTINGS, omacharts_engine::symbols::seed());
        assert!(merged.len() > 10_000, "only {} instruments", merged.len());
        let generated = merged.iter().find(|i| i.symbol == "FOXF").expect("a long-tail listing");
        assert_eq!(generated.tier, 2, "the long tail must not outrank a major");
    }

    /// Futures, currencies and crypto are in no listings feed; they exist only
    /// because somebody wrote them down.
    #[test]
    fn the_curated_kinds_survive_the_merge() {
        let merged = merge(LISTINGS, omacharts_engine::symbols::seed());
        for symbol in ["GC", "EURUSD", "BTC", "GSPC"] {
            assert!(merged.iter().any(|i| i.symbol == symbol), "{symbol} went missing");
        }
    }
}
