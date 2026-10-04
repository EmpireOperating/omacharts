//! Schema migrations, and the one number that says which have run.
//!
//! The database is one file on one machine, with no server to coordinate an
//! upgrade and nobody to run a command by hand: whatever shape it is in when
//! the app opens it, the app has to carry it forward on its own. So every
//! change to the schema is a numbered step here, applied in order, and
//! `PRAGMA user_version` records how far a given file has got.
//!
//! `user_version` rather than a row in a table, because it lives in the
//! database header: nothing that empties a table can lose it, it costs no
//! query to read, and it is written inside the same transaction as the change
//! it describes.
//!
//! Three rules hold this together, and all three matter more than they look:
//!
//! 1. **A version that has shipped is frozen.** Never renumber, never edit the
//!    SQL of a step somebody's database has already run — their file will not
//!    be visited again, so an edit changes what new installs get and nothing
//!    else, and the two drift apart silently.
//! 2. **One transaction per step, bumping the version inside it.** A machine
//!    that loses power mid-upgrade comes back on the old version with the old
//!    shape, which is a state the next launch knows how to fix. Half-applied
//!    is the one state nothing can fix.
//! 3. **Steps talk to the schema, not to the app.** A step that calls
//!    `Store::set_indicators` breaks on the day that method changes shape,
//!    years after the step was written and long after anyone remembers why it
//!    exists. The SQL of the day is frozen in amber here on purpose.

use std::path::Path;

use rusqlite::{params, Connection};

/// How far the migrations go. A database claiming more than this was written
/// by a newer Omacharts than the one opening it.
pub const LATEST: i32 = 4;

/// What went wrong before the database was usable.
#[derive(Debug)]
pub enum Error {
    Sqlite(rusqlite::Error),
    /// Opened by an older build than the one that last wrote it.
    ///
    /// Worth its own variant rather than a generic failure, because the right
    /// response is the opposite of the usual one: a database we cannot read is
    /// a reason to carry on with an empty one, and a database from the future
    /// is a reason to stop and say so. It is intact, it is the user's, and a
    /// build that quietly started them a fresh watchlist beside it would look
    /// exactly like having lost everything.
    FromTheFuture { found: i32, known: i32 },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Sqlite(error) => write!(f, "{error}"),
            Error::FromTheFuture { found, known } => write!(
                f,
                "this database was written by a newer Omacharts \
                 (its schema is version {found}, this build knows {known}). \
                 Upgrade Omacharts, or move the file aside to start fresh."
            ),
        }
    }
}

impl std::error::Error for Error {}

impl From<rusqlite::Error> for Error {
    fn from(error: rusqlite::Error) -> Error {
        Error::Sqlite(error)
    }
}

/// What a step does, which is not always expressible in SQL.
pub enum Step {
    Sql(&'static str),
    /// Reads data out, decides something about it, writes it back. Takes the
    /// open transaction so its work commits or rolls back with the version.
    Rust(fn(&Connection) -> rusqlite::Result<()>),
}

pub struct Migration {
    pub version: i32,
    /// For the error message when this is the step that failed. A number alone
    /// sends whoever is reading the log back to the source to find out what
    /// was being attempted.
    pub name: &'static str,
    /// Whether to snapshot the file before running this.
    ///
    /// Adding a table or a column cannot lose anything, so the overwhelming
    /// majority of steps say no. Anything that rewrites or drops says yes: the
    /// watchlists in here are built by hand over months and exist nowhere
    /// else.
    pub risky: bool,
    pub step: Step,
}

/// Everything that has ever been true of this schema.
pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "the shape Omacharts shipped with",
        risky: false,
        // The only step allowed to say IF NOT EXISTS, and it is load-bearing
        // rather than defensive: every database that existed before there were
        // migrations reads as version 0, and is otherwise indistinguishable
        // from an empty file. Written this way it adopts those — doing nothing
        // at all to a database that already has this shape — and builds the
        // same thing from nothing for a fresh install. Later steps must not
        // copy the idiom; from version 2 on, the version number is the truth
        // and a step that cannot fail is a step that cannot be checked.
        step: Step::Sql(
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
                 id        INTEGER PRIMARY KEY AUTOINCREMENT,
                 name      TEXT NOT NULL,
                 position  INTEGER NOT NULL,
                 collapsed INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE IF NOT EXISTS watchlist_entries (
                 section_id INTEGER NOT NULL
                     REFERENCES watchlist_sections(id) ON DELETE CASCADE,
                 symbol     TEXT NOT NULL,
                 suffix     TEXT NOT NULL DEFAULT '',
                 position   INTEGER NOT NULL,
                 PRIMARY KEY (section_id, symbol, suffix)
             );
             -- Foreign keys are enforced here, so the symbols that are in no
             -- section still need a section to hang off. It is filtered out of
             -- the named ones; callers only ever see it as the nameless first.
             INSERT OR IGNORE INTO watchlist_sections (id, name, position)
                 VALUES (0, '', -1);",
        ),
    },
    Migration {
        version: 2,
        name: "collapsed sections",
        risky: false,
        // Shipped as a bare ALTER whose error was swallowed on every launch
        // after the first, so a database adopted at version 1 may already have
        // the column. Asking first is how that is told apart from a fresh
        // install, now that swallowing the answer is no longer the plan.
        step: Step::Rust(|conn| {
            if has_column(conn, "watchlist_sections", "collapsed")? {
                return Ok(());
            }
            conn.execute_batch(
                "ALTER TABLE watchlist_sections
                     ADD COLUMN collapsed INTEGER NOT NULL DEFAULT 0",
            )
        }),
    },
    Migration {
        version: 3,
        name: "volume becomes an indicator",
        risky: false,
        // Volume used to be drawn unconditionally. Turning it into something
        // you can remove would otherwise have removed it from every chart that
        // had ever been configured, so those get one added back — once, so
        // that taking it off afterwards sticks.
        //
        // The flag this used to be guarded by is still honoured. A database
        // that ran the old one-shot and then had volume deliberately removed
        // must not be handed a second one on the way through here.
        step: Step::Rust(|conn| {
            const FLAG: &str = "volume_indicator_adopted";
            let adopted: Option<String> = conn
                .query_row("SELECT value FROM settings WHERE key = ?1", params![FLAG], |r| r.get(0))
                .ok();
            if adopted.is_some() {
                return Ok(());
            }
            set_setting(conn, FLAG, "1")?;

            // Never configured at all: the default already includes volume.
            let Some(json) = setting(conn, "indicators")? else { return Ok(()) };
            let Ok(mut indicators) =
                serde_json::from_str::<Vec<omacharts_engine::Indicator>>(&json)
            else {
                return Ok(());
            };
            if indicators.iter().any(|i| i.kind == omacharts_engine::IndicatorKind::Volume) {
                return Ok(());
            }
            let id = indicators.iter().map(|i| i.id).max().unwrap_or(0) + 1;
            indicators.insert(
                0,
                omacharts_engine::Indicator::new(id, omacharts_engine::IndicatorKind::Volume),
            );
            let Ok(json) = serde_json::to_string(&indicators) else { return Ok(()) };
            set_setting(conn, "indicators", &json)
        }),
    },
    Migration {
        version: 4,
        name: "more than one watchlist",
        risky: false,
        // The last step allowed to adopt a shape it finds already there, and
        // for the same reason version 1 is: this shipped as a bare CREATE and
        // a swallowed ALTER before there were migrations, so a database can
        // have it with nothing recording that it does. After this, every step
        // starts from a version number that means what it says.
        //
        // Nothing is moved. Sections written when there was only one watchlist
        // name none at all, and the column default fills them in as SQLite
        // adds it — so the migration is the default, and a watchlist somebody
        // built over months is never rewritten to be preserved.
        step: Step::Rust(|conn| {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS watchlists (
                     id       INTEGER PRIMARY KEY AUTOINCREMENT,
                     name     TEXT NOT NULL,
                     position INTEGER NOT NULL
                 );
                 INSERT OR IGNORE INTO watchlists (id, name, position)
                     VALUES (1, 'Default', 0);",
            )?;
            if has_column(conn, "watchlist_sections", "watchlist_id")? {
                return Ok(());
            }
            // Spelled out rather than interpolated from `DEFAULT_WATCHLIST`: a
            // step that has run on somebody's database can never run there
            // again, so reading a constant here would mean this migration
            // quietly meant something different for them than for the next
            // install. `the_default_watchlist_is_the_one_sections_fall_into`
            // is what keeps the literal and the constant honest instead.
            conn.execute_batch(
                "ALTER TABLE watchlist_sections
                     ADD COLUMN watchlist_id INTEGER NOT NULL DEFAULT 1",
            )
        }),
    },
];

/// Bring a database up to [`LATEST`], or say why it cannot be.
///
/// `snapshot_to` is where to leave a copy before anything destructive runs;
/// `None` for a database with no file behind it, which is every test and the
/// in-memory fallback.
pub fn run(conn: &Connection, snapshot_to: Option<&Path>) -> Result<(), Error> {
    apply(conn, MIGRATIONS, LATEST, snapshot_to)
}

/// What [`run`] does, over any list — so the awkward cases can be tested
/// without inventing a schema change to do it with.
fn apply(
    conn: &Connection,
    migrations: &[Migration],
    latest: i32,
    snapshot_to: Option<&Path>,
) -> Result<(), Error> {
    let from = version(conn)?;
    if from > latest {
        return Err(Error::FromTheFuture { found: from, known: latest });
    }

    let pending: Vec<&Migration> = migrations.iter().filter(|m| m.version > from).collect();
    if pending.is_empty() {
        return Ok(());
    }
    if let Some(path) = snapshot_to.filter(|_| pending.iter().any(|m| m.risky)) {
        snapshot(conn, path)?;
    }

    for migration in pending {
        // One transaction, holding both the change and the record of it. The
        // version moving is what makes the change final; if the step fails,
        // neither happened.
        let tx = conn.unchecked_transaction()?;
        match &migration.step {
            Step::Sql(sql) => tx.execute_batch(sql)?,
            Step::Rust(run) => run(&tx)?,
        }
        tx.pragma_update(None, "user_version", migration.version)?;
        tx.commit()?;
    }
    Ok(())
}

pub fn version(conn: &Connection) -> rusqlite::Result<i32> {
    conn.pragma_query_value(None, "user_version", |row| row.get(0))
}

/// Put a consistent copy of the database beside it, to go back to.
///
/// `VACUUM INTO` rather than copying the file: the database is in WAL mode, so
/// the file on its own is not the whole story, and a copy taken while a
/// checkpoint is outstanding is a copy missing the most recent thing the user
/// did. This takes the snapshot through SQLite, which knows about the WAL.
fn snapshot(conn: &Connection, path: &Path) -> rusqlite::Result<()> {
    // VACUUM INTO refuses to overwrite, and a backup from a previous upgrade
    // has already done its job — the file it was protecting is long since
    // migrated.
    let _ = std::fs::remove_file(path);
    conn.execute("VACUUM INTO ?1", params![path.to_string_lossy()])?;
    Ok(())
}

fn has_column(conn: &Connection, table: &str, column: &str) -> rusqlite::Result<bool> {
    let mut statement = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        if row.get::<_, String>(1)? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn setting(conn: &Connection, key: &str) -> rusqlite::Result<Option<String>> {
    use rusqlite::OptionalExtension;
    conn.query_row("SELECT value FROM settings WHERE key = ?1", params![key], |r| r.get(0))
        .optional()
}

fn set_setting(conn: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The watchlist tables as they were before any of this existed: no
    /// `collapsed`, no version stamped anywhere. This is the shape sitting on
    /// the disk of everyone who has run Omacharts so far, and the one thing
    /// the whole scheme has to get right.
    const BEFORE_MIGRATIONS: &str = "
        CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
        CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
        CREATE TABLE custom_themes (id TEXT PRIMARY KEY, json TEXT NOT NULL);
        CREATE TABLE custom_bar_schemes (id TEXT PRIMARY KEY, json TEXT NOT NULL);
        CREATE TABLE bar_series (
            key TEXT NOT NULL, interval TEXT NOT NULL,
            first_ts INTEGER NOT NULL, last_ts INTEGER NOT NULL,
            count INTEGER NOT NULL, fetched_at INTEGER NOT NULL,
            ts BLOB NOT NULL, open BLOB NOT NULL, high BLOB NOT NULL,
            low BLOB NOT NULL, close BLOB NOT NULL, volume BLOB NOT NULL,
            PRIMARY KEY (key, interval)
        );
        CREATE TABLE watchlist_sections (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            position INTEGER NOT NULL
        );
        CREATE TABLE watchlist_entries (
            section_id INTEGER NOT NULL
                REFERENCES watchlist_sections(id) ON DELETE CASCADE,
            symbol TEXT NOT NULL,
            suffix TEXT NOT NULL DEFAULT '',
            position INTEGER NOT NULL,
            PRIMARY KEY (section_id, symbol, suffix)
        );
        INSERT INTO watchlist_sections (id, name, position) VALUES (0, '', -1);
        INSERT INTO meta (key, value) VALUES ('schema_version', '1');
    ";

    /// A database from before the migration system, with a watchlist somebody
    /// built by hand in it.
    fn legacy() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(BEFORE_MIGRATIONS).unwrap();
        conn.execute_batch(
            "INSERT INTO watchlist_sections (id, name, position) VALUES (7, 'Futures', 0);
             INSERT INTO watchlist_entries (section_id, symbol, suffix, position)
                 VALUES (7, 'ES', '', 0), (7, 'GC', '', 1), (0, 'SPY', '', 0);",
        )
        .unwrap();
        conn
    }

    fn symbols(conn: &Connection) -> Vec<(i64, String)> {
        let mut statement = conn
            .prepare("SELECT section_id, symbol FROM watchlist_entries ORDER BY section_id, position")
            .unwrap();
        statement.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(Result::unwrap).collect()
    }

    fn sections(conn: &Connection) -> Vec<(i64, String, i64)> {
        let mut statement = conn
            .prepare("SELECT id, name, position FROM watchlist_sections ORDER BY id")
            .unwrap();
        statement
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    #[test]
    fn a_database_from_before_the_migrations_keeps_every_symbol() {
        let conn = legacy();
        let before = symbols(&conn);
        run(&conn, None).unwrap();

        assert_eq!(version(&conn).unwrap(), LATEST);
        assert_eq!(symbols(&conn), before, "a watchlist went through the upgrade and changed");
        assert!(has_column(&conn, "watchlist_sections", "collapsed").unwrap());
    }

    #[test]
    fn a_fresh_database_runs_the_same_steps_as_an_old_one() {
        // One code path, exercised by everybody: the alternative is a create
        // script that drifts away from the sum of the migrations, and nobody
        // finds out until an upgraded database behaves differently from a new
        // one for reasons no test covers.
        let fresh = Connection::open_in_memory().unwrap();
        run(&fresh, None).unwrap();
        let upgraded = legacy();
        run(&upgraded, None).unwrap();

        assert_eq!(schema(&fresh), schema(&upgraded));
    }

    fn schema(conn: &Connection) -> Vec<String> {
        let mut statement = conn
            .prepare(
                "SELECT m.name || ':' || group_concat(c.name)
                 FROM sqlite_master m
                 JOIN pragma_table_info(m.name) c
                 WHERE m.type = 'table' AND m.name NOT LIKE 'sqlite_%'
                 GROUP BY m.name ORDER BY m.name",
            )
            .unwrap();
        statement.query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect()
    }

    #[test]
    fn migrating_twice_changes_nothing_the_second_time() {
        let conn = legacy();
        run(&conn, None).unwrap();
        let after_once = (version(&conn).unwrap(), schema(&conn), symbols(&conn));
        run(&conn, None).unwrap();

        assert_eq!((version(&conn).unwrap(), schema(&conn), symbols(&conn)), after_once);
    }

    #[test]
    fn a_database_from_the_future_is_refused_rather_than_opened() {
        // It is intact and it is theirs. Starting them a blank one beside it
        // looks exactly like having lost the lot.
        let conn = Connection::open_in_memory().unwrap();
        run(&conn, None).unwrap();
        conn.pragma_update(None, "user_version", LATEST + 1).unwrap();

        match run(&conn, None) {
            Err(Error::FromTheFuture { found, known }) => {
                assert_eq!((found, known), (LATEST + 1, LATEST));
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_step_that_fails_leaves_the_version_where_it_was() {
        // The state nothing can recover from is half-applied, so the version
        // and the change it describes share a transaction.
        const BROKEN: &[Migration] = &[
            Migration {
                version: 1,
                name: "fine",
                risky: false,
                step: Step::Sql("CREATE TABLE kept (id INTEGER PRIMARY KEY);"),
            },
            Migration {
                version: 2,
                name: "throws half way",
                risky: false,
                step: Step::Sql(
                    "CREATE TABLE gone (id INTEGER PRIMARY KEY);
                     INSERT INTO nonexistent (id) VALUES (1);",
                ),
            },
        ];

        let conn = Connection::open_in_memory().unwrap();
        assert!(apply(&conn, BROKEN, 2, None).is_err());

        assert_eq!(version(&conn).unwrap(), 1, "the failed step moved the version");
        assert!(table_exists(&conn, "kept"), "the step that succeeded was rolled back too");
        assert!(!table_exists(&conn, "gone"), "half of the failed step survived");
    }

    fn table_exists(conn: &Connection, name: &str) -> bool {
        conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            params![name],
            |r| r.get::<_, i64>(0),
        )
        .unwrap()
            > 0
    }

    /// Serialised through the real type rather than written out by hand, so
    /// the fixture cannot quietly stop matching what the app would store.
    fn only_an_sma() -> String {
        let sma = omacharts_engine::Indicator::new(1, omacharts_engine::IndicatorKind::Sma);
        serde_json::to_string(&vec![sma]).unwrap()
    }

    #[test]
    fn a_chart_configured_before_volume_was_an_indicator_is_given_one() {
        let conn = legacy();
        set_setting(&conn, "indicators", &only_an_sma()).unwrap();
        run(&conn, None).unwrap();

        let json = setting(&conn, "indicators").unwrap().unwrap();
        let indicators: Vec<omacharts_engine::Indicator> = serde_json::from_str(&json).unwrap();
        assert!(indicators.iter().any(|i| i.kind == omacharts_engine::IndicatorKind::Volume));
    }

    #[test]
    fn a_chart_that_already_dropped_volume_does_not_get_it_back() {
        // The old one-shot set a flag when it ran. Somebody who took volume
        // off afterwards meant it, and the upgrade must not quietly undo that.
        let conn = legacy();
        set_setting(&conn, "volume_indicator_adopted", "1").unwrap();
        set_setting(&conn, "indicators", &only_an_sma()).unwrap();
        run(&conn, None).unwrap();

        let json = setting(&conn, "indicators").unwrap().unwrap();
        let indicators: Vec<omacharts_engine::Indicator> = serde_json::from_str(&json).unwrap();
        assert!(indicators.iter().all(|i| i.kind != omacharts_engine::IndicatorKind::Volume));
    }

    #[test]
    fn a_risky_step_leaves_a_copy_of_the_file_behind() {
        const RISKY: &[Migration] = &[Migration {
            version: 1,
            name: "rewrites something",
            risky: true,
            step: Step::Sql("CREATE TABLE after (id INTEGER PRIMARY KEY);"),
        }];

        let dir = std::env::temp_dir().join(format!("omacharts-migrate-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let (db, backup) = (dir.join("omacharts.db"), dir.join("omacharts.db.bak"));
        let _ = std::fs::remove_file(&db);
        let _ = std::fs::remove_file(&backup);

        let conn = Connection::open(&db).unwrap();
        conn.execute_batch("CREATE TABLE before (id INTEGER PRIMARY KEY);").unwrap();
        apply(&conn, RISKY, 1, Some(&backup)).unwrap();

        let restored = Connection::open(&backup).unwrap();
        assert!(table_exists(&restored, "before"), "the snapshot missed what was there");
        assert!(!table_exists(&restored, "after"), "the snapshot was taken too late to help");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nothing_is_snapshotted_for_a_step_that_cannot_lose_anything() {
        // Adding a table copies nothing, and the database is tens of
        // megabytes of cached bars: a snapshot on every launch that happens to
        // add a column is a cost paid forever for no risk avoided.
        const SAFE: &[Migration] = &[Migration {
            version: 1,
            name: "adds a table",
            risky: false,
            step: Step::Sql("CREATE TABLE added (id INTEGER PRIMARY KEY);"),
        }];

        let dir = std::env::temp_dir().join(format!("omacharts-nosnap-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let (db, backup) = (dir.join("omacharts.db"), dir.join("omacharts.db.bak"));
        let _ = std::fs::remove_file(&db);
        let _ = std::fs::remove_file(&backup);

        let conn = Connection::open(&db).unwrap();
        apply(&conn, SAFE, 1, Some(&backup)).unwrap();

        assert!(!backup.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Somebody's sections and symbols, written when there was only one
    /// watchlist, have to come back exactly — under the watchlist that has
    /// always been there. Losing somebody's symbols to a schema change is not
    /// a thing that gets a second chance.
    #[test]
    fn a_database_written_before_watchlists_were_plural_keeps_everything() {
        let conn = legacy();
        let (before, grouped) = (symbols(&conn), sections(&conn));
        run(&conn, None).unwrap();

        assert_eq!(symbols(&conn), before, "a symbol moved or vanished");
        assert_eq!(sections(&conn), grouped, "a section was renamed or reordered");
        let named: Vec<(i64, String)> = {
            let mut statement =
                conn.prepare("SELECT id, name FROM watchlists ORDER BY position").unwrap();
            statement
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .map(Result::unwrap)
                .collect()
        };
        assert_eq!(named, vec![(crate::store::DEFAULT_WATCHLIST, "Default".to_string())]);
    }

    /// The column default is what performs the migration, so the literal in
    /// the ALTER and the constant the rest of the app resolves the permanent
    /// watchlist by have to be the same number. They are written in two files
    /// and nothing but this connects them.
    #[test]
    fn the_default_watchlist_is_the_one_sections_fall_into() {
        let conn = legacy();
        run(&conn, None).unwrap();

        let mut statement =
            conn.prepare("SELECT DISTINCT watchlist_id FROM watchlist_sections").unwrap();
        let owners: Vec<i64> =
            statement.query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect();
        assert_eq!(owners, vec![crate::store::DEFAULT_WATCHLIST]);
    }

    /// Watchlists shipped as a bare CREATE and a swallowed ALTER before there
    /// were migrations, so a database can already have the shape with nothing
    /// recording that it does — which is exactly the state the first machine
    /// to run this is in.
    #[test]
    fn a_database_that_already_has_watchlists_is_adopted_rather_than_rebuilt() {
        let conn = legacy();
        conn.execute_batch(
            "CREATE TABLE watchlists (
                 id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, position INTEGER NOT NULL
             );
             INSERT INTO watchlists (id, name, position) VALUES (1, 'Renamed by hand', 0);
             ALTER TABLE watchlist_sections ADD COLUMN watchlist_id INTEGER NOT NULL DEFAULT 1;",
        )
        .unwrap();
        let before = symbols(&conn);
        run(&conn, None).unwrap();

        assert_eq!(version(&conn).unwrap(), LATEST);
        assert_eq!(symbols(&conn), before);
        let name: String = conn
            .query_row("SELECT name FROM watchlists WHERE id = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(name, "Renamed by hand", "adoption overwrote a watchlist that was already there");
    }

    #[test]
    fn the_versions_are_consecutive_and_end_at_latest() {
        // A gap or a repeat means a database can stop somewhere no step will
        // ever pick it up from again.
        for (i, migration) in MIGRATIONS.iter().enumerate() {
            assert_eq!(migration.version, i as i32 + 1, "{} is misnumbered", migration.name);
        }
        assert_eq!(MIGRATIONS.last().map(|m| m.version), Some(LATEST));
    }
}
