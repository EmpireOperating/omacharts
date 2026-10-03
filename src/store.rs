//! The local database: settings, cached bars, and themes the user saved.
//!
//! One file, `$XDG_DATA_HOME/omacharts/omacharts.db`, and it is pure app
//! data — never in the repo, safe to delete, rebuilt on demand.
//!
//! Bars are stored columnar: one row per (symbol, timeframe) holding parallel
//! blobs of timestamps and prices. A row per bar would mean tens of thousands
//! of rows per series and a decode per row; this way a chart is one query and
//! one memcpy. Series are small enough — two years of hourly bars is about
//! 12,000 — that rewriting the whole row on a tail fetch costs less than
//! managing chunks would.

use std::path::{Path, PathBuf};

use omacharts_engine::{Bar, BarScheme, Theme, Timeframe};
use rusqlite::{params, Connection, OptionalExtension};

pub struct Store {
    conn: Connection,
}

/// Symbols kept outside any section live under this id, which deliberately
/// has no row in `watchlist_sections`.
pub const ROOT_SECTION: i64 = 0;

/// A named group of symbols in the watchlist. The root section has an empty
/// name and is drawn without a header.
#[derive(Clone, PartialEq, Debug)]
pub struct Section {
    pub id: i64,
    pub name: String,
    /// Sections fold away like they do in every other watchlist. The root
    /// never collapses — there is no header to click.
    pub collapsed: bool,
    pub entries: Vec<Entry>,
}

/// A watchlist entry, stored canonically so it survives a provider change.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Entry {
    pub symbol: String,
    pub suffix: Option<String>,
}

/// What we already have for one series, so a fetch can ask only for the gap.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Coverage {
    pub first_ts: i64,
    pub last_ts: i64,
    pub count: usize,
    pub fetched_at: i64,
}

impl Store {
    pub fn open() -> rusqlite::Result<Store> {
        let dir = data_dir();
        let _ = std::fs::create_dir_all(&dir);
        Store::open_at(&dir.join("omacharts.db"))
    }

    pub fn open_at(path: &Path) -> rusqlite::Result<Store> {
        let conn = Connection::open(path)?;
        // WAL so a background fetch writing never blocks the window reading.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        let store = Store { conn };
        store.migrate()?;
        Ok(store)
    }

    pub fn memory() -> rusqlite::Result<Store> {
        let store = Store { conn: Connection::open_in_memory()? };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> rusqlite::Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS bar_series (
                 key        TEXT NOT NULL,
                 interval   TEXT NOT NULL,
                 first_ts   INTEGER NOT NULL,
                 last_ts    INTEGER NOT NULL,
                 count      INTEGER NOT NULL,
                 fetched_at INTEGER NOT NULL,
                 ts         BLOB NOT NULL,
                 open       BLOB NOT NULL,
                 high       BLOB NOT NULL,
                 low        BLOB NOT NULL,
                 close      BLOB NOT NULL,
                 volume     BLOB NOT NULL,
                 PRIMARY KEY (key, interval)
             );
             CREATE TABLE IF NOT EXISTS custom_themes (
                 id TEXT PRIMARY KEY, json TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS custom_bar_schemes (
                 id TEXT PRIMARY KEY, json TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS watchlist_sections (
                 id       INTEGER PRIMARY KEY AUTOINCREMENT,
                 name     TEXT NOT NULL,
                 position INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS watchlist_entries (
                 section_id INTEGER NOT NULL
                     REFERENCES watchlist_sections(id) ON DELETE CASCADE,
                 symbol     TEXT NOT NULL,
                 suffix     TEXT NOT NULL DEFAULT '',
                 position   INTEGER NOT NULL,
                 PRIMARY KEY (section_id, symbol, suffix)
             );",
        )?;
        // This SQLite enforces foreign keys, so the root needs a real row to
        // hang entries off. It is filtered out of the named sections; callers
        // only ever see it as the nameless first section.
        self.conn.execute(
            "INSERT OR IGNORE INTO watchlist_sections (id, name, position) VALUES (?1, '', -1)",
            params![ROOT_SECTION],
        )?;
        // Added after the first release; ignored when it already exists.
        let _ = self
            .conn
            .execute("ALTER TABLE watchlist_sections ADD COLUMN collapsed INTEGER NOT NULL DEFAULT 0", []);
        self.conn.execute(
            "INSERT OR IGNORE INTO meta (key, value) VALUES ('schema_version', '1')",
            [],
        )?;
        self.invalidate_stale_cache()?;
        Ok(())
    }

    /// Throw away cached bars written by a version that fetched them wrongly.
    ///
    /// Bumped when the *content* of the cache stops being trustworthy rather
    /// than when its shape changes — version 2 is the daily series fetched
    /// with `range=max`, which Yahoo silently coarsened into month-ends. Those
    /// bars look perfectly valid, so nothing else would ever notice them.
    fn invalidate_stale_cache(&self) -> rusqlite::Result<()> {
        const CACHE_VERSION: i64 = 2;
        let stored: i64 = self
            .conn
            .query_row("SELECT value FROM meta WHERE key = 'cache_version'", [], |r| {
                r.get::<_, String>(0)
            })
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        if stored == CACHE_VERSION {
            return Ok(());
        }
        self.conn.execute("DELETE FROM bar_series", [])?;
        self.conn.execute(
            "INSERT INTO meta (key, value) VALUES ('cache_version', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![CACHE_VERSION.to_string()],
        )?;
        Ok(())
    }

    // -- settings ----------------------------------------------------------

    pub fn setting(&self, key: &str) -> Option<String> {
        self.conn
            .query_row("SELECT value FROM settings WHERE key = ?1", params![key], |r| r.get(0))
            .optional()
            .ok()
            .flatten()
    }

    pub fn set_setting(&self, key: &str, value: &str) {
        let _ = self.conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        );
    }

    pub fn setting_bool(&self, key: &str, default: bool) -> bool {
        match self.setting(key).as_deref() {
            Some("1") => true,
            Some("0") => false,
            _ => default,
        }
    }

    pub fn set_setting_bool(&self, key: &str, value: bool) {
        self.set_setting(key, if value { "1" } else { "0" });
    }

    // -- bars --------------------------------------------------------------

    pub fn coverage(&self, key: &str, timeframe: Timeframe) -> Option<Coverage> {
        self.conn
            .query_row(
                "SELECT first_ts, last_ts, count, fetched_at FROM bar_series
                 WHERE key = ?1 AND interval = ?2",
                params![key, timeframe.key()],
                |r| {
                    Ok(Coverage {
                        first_ts: r.get(0)?,
                        last_ts: r.get(1)?,
                        count: r.get::<_, i64>(2)? as usize,
                        fetched_at: r.get(3)?,
                    })
                },
            )
            .optional()
            .ok()
            .flatten()
    }

    pub fn load_bars(&self, key: &str, timeframe: Timeframe) -> Vec<Bar> {
        let row: Option<(Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>)> = self
            .conn
            .query_row(
                "SELECT ts, open, high, low, close, volume FROM bar_series
                 WHERE key = ?1 AND interval = ?2",
                params![key, timeframe.key()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
            )
            .optional()
            .ok()
            .flatten();

        let Some((ts, open, high, low, close, volume)) = row else {
            return Vec::new();
        };
        let stamps = decode_i64(&ts);
        let (o, h, l, c, v) = (
            decode_f64(&open),
            decode_f64(&high),
            decode_f64(&low),
            decode_f64(&close),
            decode_f64(&volume),
        );
        let n = stamps.len().min(o.len()).min(h.len()).min(l.len()).min(c.len());
        (0..n)
            .map(|i| Bar {
                ts: stamps[i],
                open: o[i],
                high: h[i],
                low: l[i],
                close: c[i],
                volume: v.get(i).copied().unwrap_or(0.0),
            })
            .collect()
    }

    /// Merge `fresh` into whatever is stored.
    ///
    /// Completed bars are immutable, but the last bar we stored may have been
    /// forming when we saw it, so a bar arriving with a timestamp we already
    /// hold always wins. Returns the merged series.
    pub fn merge_bars(&self, key: &str, timeframe: Timeframe, fresh: &[Bar]) -> Vec<Bar> {
        let mut bars = self.load_bars(key, timeframe);
        if fresh.is_empty() {
            return bars;
        }
        if bars.is_empty() {
            bars = fresh.to_vec();
        } else {
            for bar in fresh {
                match bars.binary_search_by_key(&bar.ts, |b| b.ts) {
                    Ok(at) => bars[at] = *bar,
                    Err(at) => bars.insert(at, *bar),
                }
            }
        }
        self.write_bars(key, timeframe, &bars);
        bars
    }

    /// Replace a series wholesale. Used by [`Self::merge_bars`], and when an
    /// overlap mismatch means the cached history can no longer be trusted.
    pub fn write_bars(&self, key: &str, timeframe: Timeframe, bars: &[Bar]) {
        if bars.is_empty() {
            let _ = self.conn.execute(
                "DELETE FROM bar_series WHERE key = ?1 AND interval = ?2",
                params![key, timeframe.key()],
            );
            return;
        }
        let ts: Vec<i64> = bars.iter().map(|b| b.ts).collect();
        let col = |f: fn(&Bar) -> f64| encode_f64(&bars.iter().map(f).collect::<Vec<_>>());
        let _ = self.conn.execute(
            "INSERT INTO bar_series
                 (key, interval, first_ts, last_ts, count, fetched_at,
                  ts, open, high, low, close, volume)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(key, interval) DO UPDATE SET
                 first_ts = excluded.first_ts, last_ts = excluded.last_ts,
                 count = excluded.count, fetched_at = excluded.fetched_at,
                 ts = excluded.ts, open = excluded.open, high = excluded.high,
                 low = excluded.low, close = excluded.close,
                 volume = excluded.volume",
            params![
                key,
                timeframe.key(),
                ts.first().copied().unwrap_or(0),
                ts.last().copied().unwrap_or(0),
                bars.len() as i64,
                chrono::Utc::now().timestamp(),
                encode_i64(&ts),
                col(|b| b.open),
                col(|b| b.high),
                col(|b| b.low),
                col(|b| b.close),
                col(|b| b.volume),
            ],
        );
    }

    pub fn drop_series(&self, key: &str, timeframe: Timeframe) {
        let _ = self.conn.execute(
            "DELETE FROM bar_series WHERE key = ?1 AND interval = ?2",
            params![key, timeframe.key()],
        );
    }

    /// Approximate bytes the cached bars occupy.
    pub fn cache_bytes(&self) -> i64 {
        self.conn
            .query_row(
                "SELECT COALESCE(SUM(LENGTH(ts) + LENGTH(open) + LENGTH(high)
                        + LENGTH(low) + LENGTH(close) + LENGTH(volume)), 0)
                 FROM bar_series",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0)
    }

    pub fn cached_series(&self) -> i64 {
        self.conn
            .query_row("SELECT COUNT(*) FROM bar_series", [], |r| r.get(0))
            .unwrap_or(0)
    }

    /// Settings and saved themes survive; only market data goes.
    pub fn clear_market_data(&self) -> rusqlite::Result<()> {
        self.conn.execute("DELETE FROM bar_series", [])?;
        self.conn.execute_batch("VACUUM")?;
        Ok(())
    }

    // -- watchlist ---------------------------------------------------------

    /// The watchlist, in display order.
    ///
    /// Sections are optional: symbols can sit at the root. The root comes back
    /// as a nameless section with id [`ROOT_SECTION`], present only when it
    /// holds something, so callers render one uniform list either way.
    pub fn watchlist(&self) -> Vec<Section> {
        let mut out = Vec::new();
        let root = self.section_entries(ROOT_SECTION);
        if !root.is_empty() {
            out.push(Section {
                id: ROOT_SECTION,
                name: String::new(),
                collapsed: false,
                entries: root,
            });
        }
        out.extend(self.named_sections());
        out
    }

    fn named_sections(&self) -> Vec<Section> {
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT id, name, collapsed FROM watchlist_sections
             WHERE id <> ?1 ORDER BY position, id",
        ) else {
            return Vec::new();
        };
        let Ok(rows) = stmt.query_map(params![ROOT_SECTION], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)? != 0))
        }) else {
            return Vec::new();
        };
        rows.filter_map(Result::ok)
            .map(|(id, name, collapsed)| Section {
                id,
                name,
                collapsed,
                entries: self.section_entries(id),
            })
            .collect()
    }

    pub fn set_section_collapsed(&self, id: i64, collapsed: bool) {
        let _ = self.conn.execute(
            "UPDATE watchlist_sections SET collapsed = ?2 WHERE id = ?1",
            params![id, collapsed as i64],
        );
    }

    fn section_entries(&self, section_id: i64) -> Vec<Entry> {
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT symbol, suffix FROM watchlist_entries
             WHERE section_id = ?1 ORDER BY position, symbol",
        ) else {
            return Vec::new();
        };
        let Ok(rows) = stmt.query_map(params![section_id], |r| {
            Ok(Entry {
                symbol: r.get(0)?,
                suffix: {
                    let s: String = r.get(1)?;
                    if s.is_empty() { None } else { Some(s) }
                },
            })
        }) else {
            return Vec::new();
        };
        rows.filter_map(Result::ok).collect()
    }

    /// Put a symbol at the root, outside any section.
    pub fn add_to_root(&self, symbol: &str, suffix: Option<&str>) {
        self.add_to_section(ROOT_SECTION, symbol, suffix);
    }

    pub fn add_section(&self, name: &str) -> Option<i64> {
        let position: i64 = self
            .conn
            .query_row("SELECT COALESCE(MAX(position), -1) + 1 FROM watchlist_sections", [], |r| {
                r.get(0)
            })
            .unwrap_or(0);
        self.conn
            .execute(
                "INSERT INTO watchlist_sections (name, position) VALUES (?1, ?2)",
                params![name, position],
            )
            .ok()?;
        Some(self.conn.last_insert_rowid())
    }

    pub fn rename_section(&self, id: i64, name: &str) {
        let _ = self.conn.execute(
            "UPDATE watchlist_sections SET name = ?2 WHERE id = ?1",
            params![id, name],
        );
    }

    /// Remove a section and everything in it. The root cannot be removed —
    /// there is no header to remove it from.
    pub fn remove_section(&self, id: i64) {
        if id == ROOT_SECTION {
            return;
        }
        let _ = self
            .conn
            .execute("DELETE FROM watchlist_entries WHERE section_id = ?1", params![id]);
        let _ = self
            .conn
            .execute("DELETE FROM watchlist_sections WHERE id = ?1", params![id]);
    }

    pub fn add_to_section(&self, section_id: i64, symbol: &str, suffix: Option<&str>) {
        let position: i64 = self
            .conn
            .query_row(
                "SELECT COALESCE(MAX(position), -1) + 1 FROM watchlist_entries
                 WHERE section_id = ?1",
                params![section_id],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let _ = self.conn.execute(
            "INSERT OR IGNORE INTO watchlist_entries (section_id, symbol, suffix, position)
             VALUES (?1, ?2, ?3, ?4)",
            params![section_id, symbol, suffix.unwrap_or(""), position],
        );
    }

    pub fn remove_from_section(&self, section_id: i64, symbol: &str, suffix: Option<&str>) {
        let _ = self.conn.execute(
            "DELETE FROM watchlist_entries
             WHERE section_id = ?1 AND symbol = ?2 AND suffix = ?3",
            params![section_id, symbol, suffix.unwrap_or("")],
        );
    }

    /// Persist a new order for one section's entries.
    ///
    /// Positions are rewritten from the list given, so the caller only has to
    /// know the order it wants, not what the old positions were.
    pub fn reorder_entries(&self, section_id: i64, ordered: &[Entry]) {
        for (position, entry) in ordered.iter().enumerate() {
            let _ = self.conn.execute(
                "UPDATE watchlist_entries SET position = ?4
                 WHERE section_id = ?1 AND symbol = ?2 AND suffix = ?3",
                params![
                    section_id,
                    entry.symbol,
                    entry.suffix.clone().unwrap_or_default(),
                    position as i64
                ],
            );
        }
    }

    /// Move one entry so it sits where `before` currently sits.
    pub fn move_entry(&self, section_id: i64, moving: &Entry, before: &Entry) {
        let mut entries = self.section_entries(section_id);
        let Some(from) = entries.iter().position(|e| e == moving) else { return };
        let entry = entries.remove(from);
        let to = entries.iter().position(|e| e == before).unwrap_or(entries.len());
        entries.insert(to, entry);
        self.reorder_entries(section_id, &entries);
    }

    /// Give a new install something to look at rather than an empty rail.
    pub fn seed_watchlist_if_empty(&self, defaults: &[(&str, &[&str])]) {
        let existing: i64 = self
            .conn
            .query_row(
                "SELECT (SELECT COUNT(*) FROM watchlist_sections WHERE id <> ?1)
                      + (SELECT COUNT(*) FROM watchlist_entries)",
                params![ROOT_SECTION],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if existing > 0 {
            return;
        }
        for (name, symbols) in defaults {
            if let Some(id) = self.add_section(name) {
                for symbol in *symbols {
                    self.add_to_section(id, symbol, None);
                }
            }
        }
    }

    // -- saved themes ------------------------------------------------------

    pub fn custom_themes(&self) -> Vec<Theme> {
        self.json_rows("SELECT json FROM custom_themes ORDER BY id")
    }

    pub fn save_theme(&self, theme: &Theme) {
        if let Ok(json) = serde_json::to_string(theme) {
            let _ = self.conn.execute(
                "INSERT INTO custom_themes (id, json) VALUES (?1, ?2)
                 ON CONFLICT(id) DO UPDATE SET json = excluded.json",
                params![theme.id, json],
            );
        }
    }

    pub fn delete_theme(&self, id: &str) {
        let _ = self.conn.execute("DELETE FROM custom_themes WHERE id = ?1", params![id]);
    }

    pub fn custom_bar_schemes(&self) -> Vec<BarScheme> {
        self.json_rows("SELECT json FROM custom_bar_schemes ORDER BY id")
    }

    pub fn save_bar_scheme(&self, scheme: &BarScheme) {
        if let Ok(json) = serde_json::to_string(scheme) {
            let _ = self.conn.execute(
                "INSERT INTO custom_bar_schemes (id, json) VALUES (?1, ?2)
                 ON CONFLICT(id) DO UPDATE SET json = excluded.json",
                params![scheme.id, json],
            );
        }
    }

    pub fn delete_bar_scheme(&self, id: &str) {
        let _ = self
            .conn
            .execute("DELETE FROM custom_bar_schemes WHERE id = ?1", params![id]);
    }

    /// A row of JSON that fails to parse is skipped rather than fatal: a theme
    /// saved by a newer version must not stop the app opening.
    fn json_rows<T: serde::de::DeserializeOwned>(&self, sql: &str) -> Vec<T> {
        let Ok(mut stmt) = self.conn.prepare(sql) else {
            return Vec::new();
        };
        let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) else {
            return Vec::new();
        };
        rows.filter_map(Result::ok)
            .filter_map(|json| serde_json::from_str(&json).ok())
            .collect()
    }
}

/// `$XDG_DATA_HOME/omacharts`, falling back to `~/.local/share/omacharts`.
pub fn data_dir() -> PathBuf {
    if let Some(base) = std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(base).join("omacharts");
    }
    home().join(".local/share/omacharts")
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

fn encode_i64(values: &[i64]) -> Vec<u8> {
    let mut out = Vec::with_capacity(values.len() * 8);
    for v in values {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

fn encode_f64(values: &[f64]) -> Vec<u8> {
    let mut out = Vec::with_capacity(values.len() * 8);
    for v in values {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

fn decode_i64(bytes: &[u8]) -> Vec<i64> {
    bytes
        .chunks_exact(8)
        .map(|c| i64::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

fn decode_f64(bytes: &[u8]) -> Vec<f64> {
    bytes
        .chunks_exact(8)
        .map(|c| f64::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(ts: i64, close: f64) -> Bar {
        Bar { ts, open: close - 1.0, high: close + 1.0, low: close - 2.0, close, volume: 100.0 }
    }

    #[test]
    fn bars_round_trip() {
        let store = Store::memory().unwrap();
        let bars = vec![bar(100, 10.0), bar(200, 11.0), bar(300, 12.0)];
        store.write_bars("yahoo:ES=F", Timeframe::days(1), &bars);

        let back = store.load_bars("yahoo:ES=F", Timeframe::days(1));
        assert_eq!(back, bars);

        let coverage = store.coverage("yahoo:ES=F", Timeframe::days(1)).unwrap();
        assert_eq!((coverage.first_ts, coverage.last_ts, coverage.count), (100, 300, 3));
    }

    #[test]
    fn a_tail_fetch_overwrites_the_provisional_last_bar() {
        let store = Store::memory().unwrap();
        store.write_bars("k", Timeframe::hours(1), &[bar(100, 10.0), bar(200, 11.0)]);

        // 200 was still forming when we stored it; 300 is new.
        let merged = store.merge_bars("k", Timeframe::hours(1), &[bar(200, 99.0), bar(300, 12.0)]);
        assert_eq!(merged.len(), 3);
        assert_eq!(merged[1].close, 99.0);
        assert_eq!(merged[2].ts, 300);
    }

    #[test]
    fn backfill_extends_without_duplicating() {
        let store = Store::memory().unwrap();
        store.write_bars("k", Timeframe::days(1), &[bar(300, 12.0)]);
        let merged = store.merge_bars("k", Timeframe::days(1), &[bar(100, 10.0), bar(200, 11.0)]);
        assert_eq!(merged.iter().map(|b| b.ts).collect::<Vec<_>>(), vec![100, 200, 300]);
    }

    #[test]
    fn timeframes_do_not_collide() {
        let store = Store::memory().unwrap();
        store.write_bars("k", Timeframe::days(1), &[bar(100, 1.0)]);
        store.write_bars("k", Timeframe::hours(1), &[bar(100, 2.0), bar(200, 3.0)]);
        assert_eq!(store.load_bars("k", Timeframe::days(1)).len(), 1);
        assert_eq!(store.load_bars("k", Timeframe::hours(1)).len(), 2);
    }

    #[test]
    fn clearing_market_data_keeps_settings_and_themes() {
        let store = Store::memory().unwrap();
        store.set_setting("theme", "midnight");
        store.save_theme(&omacharts_engine::theme::builtin_themes()[0].duplicate("mine", "Mine"));
        store.write_bars("k", Timeframe::days(1), &[bar(100, 1.0)]);
        assert!(store.cache_bytes() > 0);

        store.clear_market_data().unwrap();

        assert_eq!(store.cache_bytes(), 0);
        assert_eq!(store.cached_series(), 0);
        assert_eq!(store.setting("theme").as_deref(), Some("midnight"));
        assert_eq!(store.custom_themes().len(), 1);
    }

    #[test]
    fn the_watchlist_round_trips() {
        let store = Store::memory().unwrap();
        let indexes = store.add_section("Indexes").unwrap();
        let crypto = store.add_section("Crypto").unwrap();
        store.add_to_section(indexes, "GSPC", None);
        store.add_to_section(indexes, "NDX", None);
        store.add_to_section(crypto, "BTC", None);
        store.add_to_section(indexes, "SAN", Some("MC"));

        let list = store.watchlist();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].name, "Indexes");
        assert_eq!(list[0].entries.len(), 3);
        assert_eq!(list[1].name, "Crypto");
        assert_eq!(list[1].entries[0].symbol, "BTC");
        assert!(list[0].entries.iter().any(|e| e.suffix.as_deref() == Some("MC")));
    }

    #[test]
    fn symbols_can_live_at_the_root_without_a_section() {
        let store = Store::memory().unwrap();
        store.add_to_root("ES", None);
        store.add_to_root("GC", None);

        let list = store.watchlist();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, ROOT_SECTION);
        assert!(list[0].name.is_empty(), "the root has no name");
        assert_eq!(list[0].entries.len(), 2);
    }

    #[test]
    fn the_root_comes_before_named_sections_and_only_when_used() {
        let store = Store::memory().unwrap();
        store.add_section("Crypto");
        assert_eq!(store.watchlist().len(), 1, "an empty root is not shown");

        store.add_to_root("ES", None);
        let list = store.watchlist();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, ROOT_SECTION);
        assert_eq!(list[1].name, "Crypto");
    }

    #[test]
    fn the_root_reorders_like_any_section() {
        let store = Store::memory().unwrap();
        for symbol in ["A", "B", "C"] {
            store.add_to_root(symbol, None);
        }
        let entry = |s: &str| Entry { symbol: s.into(), suffix: None };
        store.move_entry(ROOT_SECTION, &entry("C"), &entry("A"));
        let order: Vec<String> =
            store.watchlist()[0].entries.iter().map(|e| e.symbol.clone()).collect();
        assert_eq!(order, vec!["C", "A", "B"]);
    }

    #[test]
    fn sections_keep_the_order_they_were_made_in() {
        let store = Store::memory().unwrap();
        for name in ["First", "Second", "Third"] {
            store.add_section(name);
        }
        let names: Vec<String> = store.watchlist().into_iter().map(|s| s.name).collect();
        assert_eq!(names, vec!["First", "Second", "Third"]);
    }

    #[test]
    fn adding_the_same_symbol_twice_is_harmless() {
        let store = Store::memory().unwrap();
        let id = store.add_section("Watchlist").unwrap();
        store.add_to_section(id, "ES", None);
        store.add_to_section(id, "ES", None);
        assert_eq!(store.watchlist()[0].entries.len(), 1);
    }

    #[test]
    fn the_root_cannot_be_removed() {
        let store = Store::memory().unwrap();
        store.add_to_root("ES", None);
        store.remove_section(ROOT_SECTION);
        assert_eq!(store.watchlist()[0].entries.len(), 1, "the root survives");
    }

    #[test]
    fn removing_a_section_takes_its_entries() {
        let store = Store::memory().unwrap();
        let id = store.add_section("Temp").unwrap();
        store.add_to_section(id, "ES", None);
        store.remove_section(id);
        assert!(store.watchlist().is_empty());
        let count: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM watchlist_entries", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn sections_remember_being_collapsed() {
        let store = Store::memory().unwrap();
        let id = store.add_section("Futures").unwrap();
        assert!(!store.watchlist()[0].collapsed, "new sections start open");

        store.set_section_collapsed(id, true);
        assert!(store.watchlist()[0].collapsed);

        store.set_section_collapsed(id, false);
        assert!(!store.watchlist()[0].collapsed);
    }

    #[test]
    fn sections_can_be_renamed_and_entries_removed() {
        let store = Store::memory().unwrap();
        let id = store.add_section("Old").unwrap();
        store.add_to_section(id, "ES", None);
        store.add_to_section(id, "NQ", None);
        store.rename_section(id, "New");
        store.remove_from_section(id, "ES", None);

        let list = store.watchlist();
        assert_eq!(list[0].name, "New");
        assert_eq!(list[0].entries.len(), 1);
        assert_eq!(list[0].entries[0].symbol, "NQ");
    }

    #[test]
    fn seeding_only_happens_once() {
        let store = Store::memory().unwrap();
        let defaults: &[(&str, &[&str])] = &[("Indexes", &["GSPC"]), ("Crypto", &["BTC"])];
        store.seed_watchlist_if_empty(defaults);
        store.seed_watchlist_if_empty(defaults);
        assert_eq!(store.watchlist().len(), 2);
    }

    #[test]
    fn entries_can_be_reordered() {
        let store = Store::memory().unwrap();
        let id = store.add_section("W").unwrap();
        for symbol in ["A", "B", "C"] {
            store.add_to_section(id, symbol, None);
        }
        let entry = |s: &str| Entry { symbol: s.into(), suffix: None };

        // Drag C above A.
        store.move_entry(id, &entry("C"), &entry("A"));
        let order: Vec<String> =
            store.watchlist()[0].entries.iter().map(|e| e.symbol.clone()).collect();
        assert_eq!(order, vec!["C", "A", "B"]);

        // And back to the end.
        store.reorder_entries(id, &[entry("A"), entry("B"), entry("C")]);
        let order: Vec<String> =
            store.watchlist()[0].entries.iter().map(|e| e.symbol.clone()).collect();
        assert_eq!(order, vec!["A", "B", "C"]);
    }

    #[test]
    fn moving_onto_an_unknown_target_appends() {
        let store = Store::memory().unwrap();
        let id = store.add_section("W").unwrap();
        for symbol in ["A", "B"] {
            store.add_to_section(id, symbol, None);
        }
        let entry = |s: &str| Entry { symbol: s.into(), suffix: None };
        store.move_entry(id, &entry("A"), &entry("GONE"));
        let order: Vec<String> =
            store.watchlist()[0].entries.iter().map(|e| e.symbol.clone()).collect();
        assert_eq!(order, vec!["B", "A"]);
    }

    #[test]
    fn moving_something_that_is_not_there_is_a_no_op() {
        let store = Store::memory().unwrap();
        let id = store.add_section("W").unwrap();
        store.add_to_section(id, "A", None);
        let entry = |s: &str| Entry { symbol: s.into(), suffix: None };
        store.move_entry(id, &entry("ZZ"), &entry("A"));
        assert_eq!(store.watchlist()[0].entries.len(), 1);
    }

    #[test]
    fn a_cache_written_by_an_older_version_is_discarded_once() {
        let file = std::env::temp_dir().join(format!("omacharts-cache-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&file);

        {
            let store = Store::open_at(&file).unwrap();
            store.write_bars("k", Timeframe::days(1), &[bar(100, 1.0)]);
            store.set_setting("keep", "me");
            // Pretend it was written before the fix.
            store
                .conn
                .execute("UPDATE meta SET value = '1' WHERE key = 'cache_version'", [])
                .unwrap();
        }

        let store = Store::open_at(&file).unwrap();
        assert_eq!(store.cached_series(), 0, "stale bars are dropped");
        assert_eq!(store.setting("keep").as_deref(), Some("me"), "settings survive");

        // And a second open does not drop anything again.
        store.write_bars("k", Timeframe::days(1), &[bar(100, 1.0)]);
        drop(store);
        let store = Store::open_at(&file).unwrap();
        assert_eq!(store.cached_series(), 1);

        let _ = std::fs::remove_file(&file);
    }

    #[test]
    fn settings_round_trip() {
        let store = Store::memory().unwrap();
        assert_eq!(store.setting("missing"), None);
        assert!(store.setting_bool("auto", true));
        store.set_setting_bool("auto", false);
        assert!(!store.setting_bool("auto", true));
    }

    #[test]
    fn saved_themes_round_trip_and_delete() {
        let store = Store::memory().unwrap();
        let mine = omacharts_engine::theme::builtin_themes()[0].duplicate("mine", "Mine");
        store.save_theme(&mine);
        assert_eq!(store.custom_themes(), vec![mine.clone()]);
        store.delete_theme("mine");
        assert!(store.custom_themes().is_empty());
    }

    #[test]
    fn an_empty_write_removes_the_series() {
        let store = Store::memory().unwrap();
        store.write_bars("k", Timeframe::days(1), &[bar(100, 1.0)]);
        store.write_bars("k", Timeframe::days(1), &[]);
        assert!(store.coverage("k", Timeframe::days(1)).is_none());
    }
}

