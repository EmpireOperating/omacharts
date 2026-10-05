//! Catalogue contracts exercised through credential-free, display-free CLI processes.
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_HOME: AtomicUsize = AtomicUsize::new(0);
const USER_LISTINGS: &str =
    "equity\tCATTEST\tSynthetic catalogue instrument\t-\tUSD\t2\t0\tCAT-TEST\tTEST\t3\n";

struct Home(PathBuf);
impl Home {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "omacharts-catalogue-{}-{}",
            std::process::id(),
            NEXT_HOME.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(path.join(".local/share/omacharts")).unwrap();
        Self(path)
    }

    fn listings(&self, text: &str) {
        std::fs::write(self.0.join(".local/share/omacharts/listings.tsv"), text).unwrap();
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_omacharts"))
            .args(args)
            .env_clear()
            .env("HOME", &self.0)
            // The catalogue follows HOME, even with a separate XDG data home.
            .env("XDG_DATA_HOME", self.0.join("xdg-data"))
            .env("TMPDIR", &self.0)
            .env(
                "DBUS_SESSION_BUS_ADDRESS",
                "unix:path=/nonexistent-omacharts-test-bus",
            )
            .output()
            .unwrap()
    }

    fn json(&self, args: &[&str]) -> serde_json::Value {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "{args:?}: {error}: {}",
                String::from_utf8_lossy(&output.stdout)
            )
        })
    }
}
impl Drop for Home {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn symbol_lookup_honors_user_listings() {
    let home = Home::new();
    home.listings(USER_LISTINGS);
    let found = home.json(&["symbol", "show", "CATTEST", "--json"]);
    assert_eq!(found["symbol"], "CATTEST");
    assert_eq!(found["name"], "Synthetic catalogue instrument");
}

#[test]
fn user_catalogue_resolves_search_watchlist_add_and_chart_set() {
    let home = Home::new();
    home.listings(USER_LISTINGS);
    let searched = home.json(&["symbol", "search", "CATTEST", "--json"]);
    assert!(
        searched["symbols"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["symbol"] == "CATTEST")
    );
    home.json(&["watchlist", "add", "Default", "CATTEST", "--json"]);
    let list = home.json(&["watchlist", "show", "Default", "--json"]);
    assert_eq!(list["sections"][0]["symbols"][0]["symbol"], "CATTEST");
    home.json(&[
        "chartbook",
        "create",
        "Catalogue",
        "--symbol",
        "AAPL",
        "--json",
    ]);
    home.json(&[
        "chart",
        "set",
        "--book",
        "Catalogue",
        "--symbol",
        "CATTEST",
        "--json",
    ]);
    let charts = home.json(&["chart", "list", "--book", "Catalogue", "--json"]);
    assert_eq!(charts["charts"][0]["symbol"], "CATTEST");
}

#[test]
fn user_catalogue_replaces_embedded_listings_but_curated_rows_win() {
    let home = Home::new();
    home.listings(&format!(
        "{USER_LISTINGS}equity\tAAPL\tNot Apple\t-\tUSD\t2\t0\tFAKE\tNASDAQ\t9\n"
    ));
    let apple = home.json(&["symbol", "show", "AAPL", "--json"]);
    assert_eq!(apple["name"], "Apple");
    assert_eq!(apple["currency"], "USD");
    assert_eq!(apple["exchange"], "NASDAQ");
    // A user file is a replacement, not an append to embedded listings.
    let removed = home.run(&["symbol", "show", "FOXF", "--json"]);
    assert_eq!(removed.status.code(), Some(3));

    use omacharts_engine::{Provider, SearchIndex};
    let rows = format!("{USER_LISTINGS}equity\tAAPL\tNot Apple\t-\tUSD\t2\t0\tFAKE\tNASDAQ\t9\n");
    let index = SearchIndex::new(omacharts::inventory::merge(
        &rows,
        omacharts_engine::symbols::seed(),
    ));
    let provider = omacharts_engine::providers::Yahoo::new();
    let custom = index.find("CATTEST", None).unwrap();
    assert_eq!(provider.symbol_for(custom).as_deref(), Some("CAT-TEST"));
    let apple = index.find("AAPL", None).unwrap();
    assert_eq!(apple.name, "Apple");
    assert_eq!(apple.tier, 0);
    assert_eq!(apple.popularity, 9);
    assert_eq!(provider.symbol_for(apple).as_deref(), Some("AAPL"));
    assert_eq!(
        index.items().iter().filter(|i| i.symbol == "AAPL").count(),
        1
    );
}

#[test]
fn missing_or_unreadable_catalogue_falls_back_to_embedded_listings() {
    let home = Home::new();
    for unreadable in [false, true] {
        if unreadable {
            std::fs::write(home.0.join(".local/share/omacharts/listings.tsv"), [0xff]).unwrap();
        }
        assert_eq!(
            home.json(&["symbol", "show", "FOXF", "--json"])["symbol"],
            "FOXF"
        );
        assert_eq!(
            home.json(&["symbol", "show", "AAPL", "--json"])["name"],
            "Apple"
        );
        home.json(&["watchlist", "add", "Default", "FOXF", "--json"]);
        let feed = home.json(&["watchlist", "feed"]);
        assert_eq!(feed["sections"][0]["entries"][0]["symbol"], "FOXF");
    }
}

fn feed_after_removing_custom_listing(symbols: &[&str]) -> Vec<String> {
    let home = Home::new();
    home.listings(USER_LISTINGS);
    let mut args = vec!["watchlist", "add", "Default"];
    args.extend_from_slice(symbols);
    args.push("--json");
    home.json(&args);
    // An empty replacement removes CATTEST but retains curated instruments.
    home.listings("");
    let feed = home.json(&["watchlist", "feed"]);
    feed["sections"][0]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["symbol"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn feed_skips_unknown_first_entry_without_a_leading_comma() {
    assert_eq!(
        feed_after_removing_custom_listing(&["CATTEST", "AAPL", "MSFT"]),
        ["AAPL", "MSFT"]
    );
}

#[test]
fn feed_skips_unknown_middle_entry_without_losing_known_entries() {
    assert_eq!(
        feed_after_removing_custom_listing(&["AAPL", "CATTEST", "MSFT"]),
        ["AAPL", "MSFT"]
    );
}

#[test]
fn feed_with_all_entries_unknown_is_valid_empty_json() {
    assert!(feed_after_removing_custom_listing(&["CATTEST"]).is_empty());
}

#[test]
fn feed_resolves_user_catalogue_and_keeps_uncached_quotes_absent() {
    let home = Home::new();
    home.listings(USER_LISTINGS);
    home.json(&["watchlist", "add", "Default", "CATTEST", "--json"]);
    let feed = home.json(&["watchlist", "feed"]);
    let entries = feed["sections"][0]["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["symbol"], "CATTEST");
    assert_eq!(entries[0]["name"], "Synthetic catalogue instrument");
    for entry in entries {
        for field in ["last", "change", "changePct", "asOf", "spark"] {
            assert!(
                entry[field].is_null(),
                "uncached {field} must not be invented"
            );
        }
    }
}
