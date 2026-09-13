//! Schema versioning. Migrations are embedded SQL files applied in order,
//! each inside its own transaction; the current version lives in
//! `schema_meta('version')`.

use rusqlite::{Connection, OptionalExtension, params};

use super::error::TraceError;

/// `(version, sql)` in ascending order. Append only.
const MIGRATIONS: &[(u32, &str)] = &[
    (1, include_str!("migrations/v001.sql")),
    (2, include_str!("migrations/v002.sql")),
    (3, include_str!("migrations/v003.sql")),
];

/// Newest schema version this build understands.
pub const SCHEMA_VERSION: u32 = MIGRATIONS[MIGRATIONS.len() - 1].0;

/// Pragmas applied to every connection.
pub(super) fn configure(conn: &Connection) -> Result<(), TraceError> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    Ok(())
}

/// Current schema version; 0 for an empty database.
pub(super) fn version(conn: &Connection) -> Result<u32, TraceError> {
    let has_meta: bool = conn.query_row(
        "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type = 'table' AND name = 'schema_meta'",
        [],
        |r| r.get(0),
    )?;
    if !has_meta {
        return Ok(0);
    }
    let v: Option<String> = conn
        .query_row(
            "SELECT value FROM schema_meta WHERE key = 'version'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    match v {
        None => Ok(0),
        Some(s) => s.parse().map_err(|_| TraceError::Invalid {
            what: "schema version",
            reason: format!("`{s}` is not a number"),
        }),
    }
}

/// Applies every migration newer than the stored version. Refuses to open a
/// database from a newer build.
pub(super) fn migrate(conn: &mut Connection) -> Result<Vec<u32>, TraceError> {
    let current = version(conn)?;
    if current > SCHEMA_VERSION {
        return Err(TraceError::SchemaTooNew {
            found: current,
            supported: SCHEMA_VERSION,
        });
    }
    let mut applied = Vec::new();
    for &(v, sql) in MIGRATIONS.iter().filter(|(v, _)| *v > current) {
        let tx = conn.transaction()?;
        apply(&tx, v, sql).map_err(|source| TraceError::Migration { version: v, source })?;
        tx.commit()?;
        tracing::info!(version = v, "trace schema migrated");
        applied.push(v);
    }
    Ok(applied)
}

fn apply(tx: &rusqlite::Transaction<'_>, version: u32, sql: &str) -> rusqlite::Result<()> {
    tx.execute_batch(sql)?;
    tx.execute(
        "INSERT INTO schema_meta(key, value) VALUES ('version', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![version.to_string()],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_are_ascending_from_one() {
        for (i, (v, _)) in MIGRATIONS.iter().enumerate() {
            assert_eq!(*v, u32::try_from(i).unwrap() + 1);
        }
    }

    #[test]
    fn fresh_database_migrates_to_latest() {
        let mut conn = Connection::open_in_memory().unwrap();
        configure(&conn).unwrap();
        assert_eq!(version(&conn).unwrap(), 0);
        assert_eq!(migrate(&mut conn).unwrap(), vec![1, 2, 3]);
        assert_eq!(version(&conn).unwrap(), SCHEMA_VERSION);
        assert!(migrate(&mut conn).unwrap().is_empty());
    }

    /// An M00 database (schema v1) with data in it migrates to the
    /// current version with every row intact, `sessions.workspace_id`
    /// NULL and the v3 columns at their defaults.
    #[test]
    fn v1_database_migrates_without_data_loss() {
        let mut conn = Connection::open_in_memory().unwrap();
        configure(&conn).unwrap();
        {
            let tx = conn.transaction().unwrap();
            apply(&tx, 1, MIGRATIONS[0].1).unwrap();
            tx.commit().unwrap();
        }
        assert_eq!(version(&conn).unwrap(), 1);
        conn.execute_batch(
            "INSERT INTO sessions(id, created_at, updated_at, title, workspace_path, config_json)
             VALUES ('s1', 't0', 't0', 'old', 'C:/old/repo', '{}');
             INSERT INTO events(id, session_id, seq, ts, kind, payload_json)
             VALUES ('e1', 's1', 1, 't0', 'session.created', '{}');
             INSERT INTO blobs(id, size, media_type, created_at) VALUES ('b1', 3, 'text/plain', 't0');",
        )
        .unwrap();

        assert_eq!(migrate(&mut conn).unwrap(), vec![2, 3]);
        assert_eq!(version(&conn).unwrap(), 3);
        let (title, path, ws): (String, String, Option<String>) = conn
            .query_row(
                "SELECT title, workspace_path, workspace_id FROM sessions WHERE id = 's1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            (title.as_str(), path.as_str(), ws),
            ("old", "C:/old/repo", None)
        );
        let counts: (i64, i64, i64, i64) = conn
            .query_row(
                "SELECT (SELECT COUNT(*) FROM events), (SELECT COUNT(*) FROM blobs),
                        (SELECT COUNT(*) FROM workspaces),
                        (SELECT message_count FROM sessions WHERE id = 's1')",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(counts, (1, 1, 0, 0));
        conn.execute_batch(
            "INSERT INTO session_fts(session_id, seq, text) VALUES ('s1', 1, 'hello world');",
        )
        .unwrap();
        let hits: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM session_fts WHERE session_fts MATCH 'hello'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hits, 1, "fts5 is compiled in");
    }

    #[test]
    fn newer_schema_is_refused() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
        conn.execute(
            "UPDATE schema_meta SET value = '99' WHERE key = 'version'",
            [],
        )
        .unwrap();
        let err = migrate(&mut conn).unwrap_err();
        assert!(matches!(
            err,
            TraceError::SchemaTooNew {
                found: 99,
                supported: SCHEMA_VERSION
            }
        ));
    }
}
