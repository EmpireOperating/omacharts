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

use omacharts_engine::{Bar, BarScheme, Indicator, Theme, Timeframe};

use crate::migrations;
use rusqlite::{params, Connection, OptionalExtension};

pub struct Store {
    conn: Connection,
}

/// Symbols kept outside any section, in the watchlist that has always been
/// there. Every watchlist has a root of its own; this is the one whose row
/// predates there being more than one.
pub const ROOT_SECTION: i64 = 0;

/// What a root section's position is, in a column where every named section's
/// is zero or more. One number, below everything it is ordered against, is
/// what makes "the section that is not a section" a query rather than a rule
/// written down in two places.
const ROOT_POSITION: i64 = -1;

/// The watchlist every install has and nobody can delete.
///
/// A fixed row id rather than a name, because it can be renamed: resolving it
/// by the string "Default" would lose it the moment somebody called it
/// something else.
pub const DEFAULT_WATCHLIST: i64 = 1;

/// A named group of symbols in a watchlist. The root section has an empty
/// name and is drawn without a header.
#[derive(Clone, PartialEq, Debug)]
pub struct Section {
    pub id: i64,
    pub name: String,
    /// Sections fold away like they do in every other watchlist. The root
    /// never collapses — there is no header to click.
    pub collapsed: bool,
    /// The section holding whatever is not in a section. One per watchlist,
    /// so its id is only [`ROOT_SECTION`] in the default one.
    pub root: bool,
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
    pub fn open() -> Result<Store, migrations::Error> {
        let dir = data_dir();
        let _ = std::fs::create_dir_all(&dir);
        Store::open_at(&dir.join("omacharts.db"))
    }

    pub fn open_at(path: &Path) -> Result<Store, migrations::Error> {
        let conn = Connection::open(path)?;
        // WAL so a background fetch writing never blocks the window reading.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        let store = Store { conn };
        // Where a migration that could lose something leaves a copy first.
        // Beside the database rather than in a temp directory, because the
        // person who needs it has to be able to find it.
        store.migrate(Some(&path.with_extension("db.bak")))?;
        Ok(store)
    }

    /// A database with no file behind it: the tests, and the fallback when
    /// the real one cannot be opened. Nothing to snapshot.
    pub fn memory() -> Result<Store, migrations::Error> {
        let store = Store { conn: Connection::open_in_memory()? };
        store.migrate(None)?;
        Ok(store)
    }

    /// Bring the database up to date, then decide whether what is cached in
    /// it is still worth believing.
    ///
    /// Two different questions, kept apart deliberately: one is the *shape* of
    /// the database, which only ever moves forward and is recorded in
    /// `user_version`; the other is whether the bars already in it were
    /// fetched correctly, which has its own counter because it moves on its
    /// own schedule. A release can change either without touching the other.
    fn migrate(&self, snapshot_to: Option<&Path>) -> Result<(), migrations::Error> {
        migrations::run(&self.conn, snapshot_to)?;
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

    /// Every watchlist, in display order, as id and name.
    pub fn watchlists(&self) -> Vec<(i64, String)> {
        let Ok(mut stmt) =
            self.conn.prepare("SELECT id, name FROM watchlists ORDER BY position, id")
        else {
            return Vec::new();
        };
        let Ok(rows) = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))) else {
            return Vec::new();
        };
        rows.filter_map(Result::ok).collect()
    }

    pub fn watchlist_exists(&self, id: i64) -> bool {
        self.conn
            .query_row("SELECT 1 FROM watchlists WHERE id = ?1", params![id], |_| Ok(()))
            .optional()
            .ok()
            .flatten()
            .is_some()
    }

    /// The default watchlist, in display order.
    ///
    /// The one the bar widget shows, which is why it is the one this returns:
    /// switching the rail to a scratch list is a thing you do while looking at
    /// the app, and it has no business rewriting what sits in the system bar.
    pub fn watchlist(&self) -> Vec<Section> {
        self.watchlist_sections(DEFAULT_WATCHLIST)
    }

    /// One watchlist's sections, in display order.
    ///
    /// Sections are optional: symbols can sit at the root. The root comes back
    /// as a nameless section, present only when it holds something, so callers
    /// render one uniform list either way.
    pub fn watchlist_sections(&self, watchlist: i64) -> Vec<Section> {
        let root_id = self.root_section(watchlist);
        let mut out = Vec::new();
        let root = self.section_entries(root_id);
        if !root.is_empty() {
            out.push(Section {
                id: root_id,
                name: String::new(),
                collapsed: false,
                root: true,
                entries: root,
            });
        }
        out.extend(self.named_sections(watchlist));
        out
    }

    /// Where a symbol lands when it is put in a watchlist rather than in one
    /// of its sections.
    ///
    /// Every watchlist has one, held apart from the named sections by a
    /// position no section it is ordered against can reach. The default
    /// watchlist's is [`ROOT_SECTION`], which is the row that was there before
    /// any of this.
    pub fn root_section(&self, watchlist: i64) -> i64 {
        self.conn
            .query_row(
                "SELECT id FROM watchlist_sections
                 WHERE watchlist_id = ?1 AND position = ?2 ORDER BY id LIMIT 1",
                params![watchlist, ROOT_POSITION],
                |r| r.get(0),
            )
            .unwrap_or(ROOT_SECTION)
    }

    fn named_sections(&self, watchlist: i64) -> Vec<Section> {
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT id, name, collapsed FROM watchlist_sections
             WHERE watchlist_id = ?1 AND position <> ?2 ORDER BY position, id",
        ) else {
            return Vec::new();
        };
        let Ok(rows) = stmt.query_map(params![watchlist, ROOT_POSITION], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)? != 0))
        }) else {
            return Vec::new();
        };
        rows.filter_map(Result::ok)
            .map(|(id, name, collapsed)| Section {
                id,
                name,
                collapsed,
                root: false,
                entries: self.section_entries(id),
            })
            .collect()
    }

    /// Create an empty watchlist, with the root section it needs to hold a
    /// symbol that is not in a section.
    pub fn add_watchlist(&self, name: &str) -> Option<i64> {
        let position: i64 = self
            .conn
            .query_row("SELECT COALESCE(MAX(position), -1) + 1 FROM watchlists", [], |r| r.get(0))
            .unwrap_or(0);
        self.conn
            .execute(
                "INSERT INTO watchlists (name, position) VALUES (?1, ?2)",
                params![name, position],
            )
            .ok()?;
        let id = self.conn.last_insert_rowid();
        self.conn
            .execute(
                "INSERT INTO watchlist_sections (name, position, watchlist_id)
                 VALUES ('', ?1, ?2)",
                params![ROOT_POSITION, id],
            )
            .ok()?;
        Some(id)
    }

    pub fn rename_watchlist(&self, id: i64, name: &str) {
        let _ = self
            .conn
            .execute("UPDATE watchlists SET name = ?2 WHERE id = ?1", params![id, name]);
    }

    /// Remove a watchlist and everything in it. The default one stays: it is
    /// what the bar widget shows and what a deleted watchlist falls back to,
    /// so there has to be one that is always there.
    pub fn remove_watchlist(&self, id: i64) {
        if id == DEFAULT_WATCHLIST {
            return;
        }
        let _ = self.conn.execute(
            "DELETE FROM watchlist_entries WHERE section_id IN
             (SELECT id FROM watchlist_sections WHERE watchlist_id = ?1)",
            params![id],
        );
        let _ = self
            .conn
            .execute("DELETE FROM watchlist_sections WHERE watchlist_id = ?1", params![id]);
        let _ = self.conn.execute("DELETE FROM watchlists WHERE id = ?1", params![id]);
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

    /// Put a symbol at the root of the default watchlist, outside any section.
    pub fn add_to_root(&self, symbol: &str, suffix: Option<&str>) {
        self.add_to_section(ROOT_SECTION, symbol, suffix);
    }

    pub fn add_section(&self, watchlist: i64, name: &str) -> Option<i64> {
        let position: i64 = self
            .conn
            .query_row(
                "SELECT COALESCE(MAX(position), -1) + 1 FROM watchlist_sections
                 WHERE watchlist_id = ?1",
                params![watchlist],
                |r| r.get(0),
            )
            .unwrap_or(0);
        self.conn
            .execute(
                "INSERT INTO watchlist_sections (name, position, watchlist_id)
                 VALUES (?1, ?2, ?3)",
                params![name, position, watchlist],
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

    /// Remove a section and everything in it. A root cannot be removed —
    /// there is no header to remove it from, and its watchlist would have
    /// nowhere to put a symbol that is not in a section.
    pub fn remove_section(&self, id: i64) {
        let position: i64 = self
            .conn
            .query_row("SELECT position FROM watchlist_sections WHERE id = ?1", params![id], |r| {
                r.get(0)
            })
            .unwrap_or(ROOT_POSITION);
        if id == ROOT_SECTION || position == ROOT_POSITION {
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

    /// Move an entry into another section, landing before `before` when given
    /// and at the end otherwise.
    pub fn move_entry_to_section(
        &self,
        from: i64,
        to: i64,
        moving: &Entry,
        before: Option<&Entry>,
    ) {
        if from == to {
            if let Some(before) = before {
                self.move_entry(from, moving, before);
            }
            return;
        }
        self.remove_from_section(from, &moving.symbol, moving.suffix.as_deref());
        self.add_to_section(to, &moving.symbol, moving.suffix.as_deref());
        if let Some(before) = before {
            self.move_entry(to, moving, before);
        }
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
            if let Some(id) = self.add_section(DEFAULT_WATCHLIST, name) {
                for symbol in *symbols {
                    self.add_to_section(id, symbol, None);
                }
            }
        }
    }

    // -- indicators --------------------------------------------------------

    /// The indicators on the chart. One set, shared by every symbol — arrowing
    /// down a watchlist to compare setups only works if the chart keeps its
    /// shape.
    pub fn indicators(&self) -> Vec<Indicator> {
        match self.setting("indicators") {
            // Volume used to be drawn unconditionally. Now that it is an
            // indicator, a chart that has never been configured still gets
            // one — otherwise making it removable would remove it from
            // everybody at once.
            None => vec![Indicator::new(1, omacharts_engine::IndicatorKind::Volume)],
            Some(json) => serde_json::from_str(&json).unwrap_or_default(),
        }
    }

    pub fn set_indicators(&self, indicators: &[Indicator]) {
        if let Ok(json) = serde_json::to_string(indicators) {
            self.set_setting("indicators", &json);
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

    /// Every symbol in a watchlist, sections and all, for the tests that only
    /// care about which watchlist a symbol ended up in.
    fn symbols(sections: &[Section]) -> Vec<String> {
        sections.iter().flat_map(|s| &s.entries).map(|e| e.symbol.clone()).collect()
    }

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
        let indexes = store.add_section(DEFAULT_WATCHLIST, "Indexes").unwrap();
        let crypto = store.add_section(DEFAULT_WATCHLIST, "Crypto").unwrap();
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
        store.add_section(DEFAULT_WATCHLIST, "Crypto");
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

    /// The database on a machine that has been running this app has sections
    /// A build older than the one that last wrote the file has to stop here,
    /// where the caller can still tell this apart from a database it simply
    /// could not read. Falling back to an empty one would put a pristine
    /// watchlist on screen over the top of somebody's real one.
    #[test]
    fn a_database_from_a_newer_omacharts_is_refused_rather_than_opened() {
        let path =
            std::env::temp_dir().join(format!("omacharts-future-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);

        let store = Store::open_at(&path).unwrap();
        store.add_section(DEFAULT_WATCHLIST, "Mine").unwrap();
        drop(store);

        let ahead = Connection::open(&path).unwrap();
        ahead.pragma_update(None, "user_version", migrations::LATEST + 1).unwrap();
        drop(ahead);

        match Store::open_at(&path) {
            Err(migrations::Error::FromTheFuture { found, known }) => {
                assert_eq!((found, known), (migrations::LATEST + 1, migrations::LATEST));
            }
            other => panic!("expected a refusal, got {:?}", other.map(|_| "a store")),
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_symbol_put_in_one_watchlist_stays_out_of_the_others() {
        let store = Store::memory().unwrap();
        let scratch = store.add_watchlist("Scratch").unwrap();

        store.add_to_root("SPY", None);
        store.add_to_section(store.root_section(scratch), "BTC", None);

        assert_eq!(symbols(&store.watchlist()), vec!["SPY"]);
        assert_eq!(symbols(&store.watchlist_sections(scratch)), vec!["BTC"]);
    }

    /// Every watchlist needs somewhere to put a symbol that is not in a
    /// section, and sharing one would be the same bug as sharing the symbols.
    #[test]
    fn each_watchlist_gets_a_root_of_its_own() {
        let store = Store::memory().unwrap();
        let scratch = store.add_watchlist("Scratch").unwrap();
        assert_eq!(store.root_section(DEFAULT_WATCHLIST), ROOT_SECTION);
        assert_ne!(store.root_section(scratch), ROOT_SECTION);
    }

    #[test]
    fn the_default_watchlist_can_be_renamed_but_not_deleted() {
        let store = Store::memory().unwrap();
        store.rename_watchlist(DEFAULT_WATCHLIST, "Majors");
        store.remove_watchlist(DEFAULT_WATCHLIST);
        assert_eq!(store.watchlists(), vec![(DEFAULT_WATCHLIST, "Majors".to_string())]);
    }

    #[test]
    fn deleting_a_watchlist_takes_its_sections_and_symbols() {
        let store = Store::memory().unwrap();
        let scratch = store.add_watchlist("Scratch").unwrap();
        let section = store.add_section(scratch, "Metals").unwrap();
        store.add_to_section(section, "GC", None);

        store.remove_watchlist(scratch);
        assert!(!store.watchlist_exists(scratch));
        assert!(store.watchlist_sections(scratch).is_empty());
        assert!(store.watchlist().is_empty(), "the default one is untouched and still empty");
    }

    /// What the bar widget reads. Switching the rail to a scratch list is
    /// something you do while looking at the app; the system bar has no
    /// business changing because of it.
    #[test]
    fn the_bar_widget_reads_the_default_watchlist_whatever_else_exists() {
        let store = Store::memory().unwrap();
        store.add_to_root("SPY", None);
        let scratch = store.add_watchlist("Scratch").unwrap();
        store.add_to_section(store.root_section(scratch), "BTC", None);

        assert_eq!(symbols(&store.watchlist()), vec!["SPY"]);
    }

    #[test]
    fn sections_keep_the_order_they_were_made_in() {
        let store = Store::memory().unwrap();
        for name in ["First", "Second", "Third"] {
            store.add_section(DEFAULT_WATCHLIST, name);
        }
        let names: Vec<String> = store.watchlist().into_iter().map(|s| s.name).collect();
        assert_eq!(names, vec!["First", "Second", "Third"]);
    }

    #[test]
    fn adding_the_same_symbol_twice_is_harmless() {
        let store = Store::memory().unwrap();
        let id = store.add_section(DEFAULT_WATCHLIST, "Watchlist").unwrap();
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
        let id = store.add_section(DEFAULT_WATCHLIST, "Temp").unwrap();
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
        let id = store.add_section(DEFAULT_WATCHLIST, "Futures").unwrap();
        assert!(!store.watchlist()[0].collapsed, "new sections start open");

        store.set_section_collapsed(id, true);
        assert!(store.watchlist()[0].collapsed);

        store.set_section_collapsed(id, false);
        assert!(!store.watchlist()[0].collapsed);
    }

    #[test]
    fn sections_can_be_renamed_and_entries_removed() {
        let store = Store::memory().unwrap();
        let id = store.add_section(DEFAULT_WATCHLIST, "Old").unwrap();
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
        let id = store.add_section(DEFAULT_WATCHLIST, "W").unwrap();
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
    fn an_entry_can_move_between_sections() {
        let store = Store::memory().unwrap();
        let a = store.add_section(DEFAULT_WATCHLIST, "A").unwrap();
        let b = store.add_section(DEFAULT_WATCHLIST, "B").unwrap();
        store.add_to_section(a, "ES", None);
        store.add_to_section(a, "NQ", None);
        store.add_to_section(b, "BTC", None);
        let entry = |s: &str| Entry { symbol: s.into(), suffix: None };

        // Dropped onto BTC, so it lands above it.
        store.move_entry_to_section(a, b, &entry("ES"), Some(&entry("BTC")));

        let list = store.watchlist();
        let in_a: Vec<String> = list[0].entries.iter().map(|e| e.symbol.clone()).collect();
        let in_b: Vec<String> = list[1].entries.iter().map(|e| e.symbol.clone()).collect();
        assert_eq!(in_a, vec!["NQ"]);
        assert_eq!(in_b, vec!["ES", "BTC"]);
    }

    #[test]
    fn dropping_on_a_section_rather_than_a_row_appends() {
        let store = Store::memory().unwrap();
        let a = store.add_section(DEFAULT_WATCHLIST, "A").unwrap();
        let b = store.add_section(DEFAULT_WATCHLIST, "B").unwrap();
        store.add_to_section(a, "ES", None);
        store.add_to_section(b, "BTC", None);
        store.move_entry_to_section(a, b, &Entry { symbol: "ES".into(), suffix: None }, None);

        let in_b: Vec<String> =
            store.watchlist()[1].entries.iter().map(|e| e.symbol.clone()).collect();
        assert_eq!(in_b, vec!["BTC", "ES"]);
    }

    #[test]
    fn moving_within_a_section_still_reorders() {
        let store = Store::memory().unwrap();
        let a = store.add_section(DEFAULT_WATCHLIST, "A").unwrap();
        for symbol in ["X", "Y", "Z"] {
            store.add_to_section(a, symbol, None);
        }
        let entry = |s: &str| Entry { symbol: s.into(), suffix: None };
        store.move_entry_to_section(a, a, &entry("Z"), Some(&entry("X")));
        let order: Vec<String> =
            store.watchlist()[0].entries.iter().map(|e| e.symbol.clone()).collect();
        assert_eq!(order, vec!["Z", "X", "Y"]);
    }

    #[test]
    fn moving_onto_an_unknown_target_appends() {
        let store = Store::memory().unwrap();
        let id = store.add_section(DEFAULT_WATCHLIST, "W").unwrap();
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
        let id = store.add_section(DEFAULT_WATCHLIST, "W").unwrap();
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
    fn a_chart_that_was_never_configured_still_has_volume() {
        use omacharts_engine::IndicatorKind;
        let store = Store::memory().unwrap();
        let indicators = store.indicators();
        assert_eq!(indicators.len(), 1);
        assert_eq!(indicators[0].kind, IndicatorKind::Volume);

        // But removing it is allowed to stick: an empty list is a choice.
        store.set_indicators(&[]);
        assert!(store.indicators().is_empty());
    }

    #[test]
    fn indicators_round_trip() {
        use omacharts_engine::IndicatorKind;
        let store = Store::memory().unwrap();

        let set = vec![Indicator::new(1, IndicatorKind::Sma), Indicator::new(2, IndicatorKind::Vwap)];
        store.set_indicators(&set);
        assert_eq!(store.indicators(), set);

        store.set_indicators(&[]);
        assert!(store.indicators().is_empty());
    }

    #[test]
    fn an_unreadable_indicator_list_is_ignored_rather_than_fatal() {
        let store = Store::memory().unwrap();
        store.set_setting("indicators", "{ this is not json");
        assert!(store.indicators().is_empty());
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

