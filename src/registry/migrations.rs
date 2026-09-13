//! Hand-rolled, dependency-free migrations. Numbered steps applied in order and
//! tracked in `registry_meta.schema_version`. Most steps are SQL embedded at
//! compile time; a step needing imperative logic (inspect the schema, toggle
//! `foreign_keys` for a table rebuild) is a `Code` step. Adding a migration =
//! append `(n, Migration::Sql(include_str!(...)))`, or `Migration::Code(...)`
//! for the rare imperative case.

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension};

enum Migration {
    Sql(&'static str),
    /// An imperative step. Unlike `Sql` it is NOT wrapped in an outer
    /// transaction (so it can toggle `foreign_keys`, a no-op inside one), and so
    /// MUST be idempotent -- it may re-run if a later step fails before the
    /// version is recorded.
    Code(fn(&mut Connection) -> Result<()>),
}

const MIGRATIONS: &[(u32, Migration)] = &[
    (1, Migration::Sql(include_str!("schema/0001_init.sql"))),
    (
        2,
        Migration::Sql(include_str!("schema/0002_seed_loaders.sql")),
    ),
    (
        3,
        Migration::Sql(include_str!("schema/0003_pack_build_fingerprint.sql")),
    ),
    (4, Migration::Code(reconcile_target_schema)),
    (
        5,
        Migration::Sql(include_str!("schema/0005_mod_author.sql")),
    ),
    (
        6,
        Migration::Sql(include_str!("schema/0006_mod_version_modrinth.sql")),
    ),
    (
        7,
        Migration::Sql(include_str!("schema/0007_mod_release.sql")),
    ),
    (
        8,
        Migration::Sql(include_str!("schema/0008_mod_content_sig.sql")),
    ),
    (9, Migration::Code(drop_pack_provenance)),
    (
        10,
        Migration::Sql(include_str!("schema/0010_derived_graph.sql")),
    ),
    (
        11,
        Migration::Sql(include_str!("schema/0011_relation_artifact.sql")),
    ),
    (
        12,
        Migration::Sql(include_str!("schema/0012_side_class.sql")),
    ),
    (
        13,
        Migration::Sql(include_str!("schema/0013_jar_class.sql")),
    ),
    (
        14,
        Migration::Sql(include_str!("schema/0014_side_confidence.sql")),
    ),
    (
        15,
        Migration::Sql(include_str!("schema/0015_jar_class_source.sql")),
    ),
    (16, Migration::Code(widen_release_channel_vocab)),
    (
        17,
        Migration::Sql(include_str!("schema/0017_loader_bridge.sql")),
    ),
    (
        18,
        Migration::Sql(include_str!("schema/0018_loader_provides.sql")),
    ),
    (
        19,
        Migration::Sql(include_str!("schema/0019_lwjgl3ify_loader.sql")),
    ),
    (
        20,
        Migration::Sql(include_str!("schema/0020_modrinth_project_name.sql")),
    ),
    (21, Migration::Sql(include_str!("schema/0021_jar_read.sql"))),
    (22, Migration::Code(collapse_conflict_kinds)),
    (
        23,
        Migration::Sql(include_str!("schema/0023_mixin_needs.sql")),
    ),
    (
        24,
        Migration::Sql(include_str!("schema/0024_artifact_loader_req.sql")),
    ),
    (
        25,
        Migration::Sql(include_str!("schema/0025_curseforge_file.sql")),
    ),
];

/// Apply every migration newer than the recorded schema version, each in its
/// own transaction. Idempotent: a no-op when already current.
pub fn apply_pending(conn: &mut Connection) -> Result<()> {
    let current = current_version(conn)?;
    for (version, migration) in MIGRATIONS {
        if *version <= current {
            continue;
        }
        match migration {
            Migration::Sql(sql) => {
                let tx = conn.transaction().context("begin migration txn")?;
                tx.execute_batch(sql)
                    .with_context(|| format!("applying migration {version}"))?;
                // migration 1 creates registry_meta; the upsert is safe from then on.
                tx.execute(
                    "INSERT INTO registry_meta (k, v) VALUES ('schema_version', ?1)
                     ON CONFLICT(k) DO UPDATE SET v = excluded.v",
                    [version.to_string()],
                )
                .context("recording schema_version")?;
                tx.commit().context("committing migration")?;
            }
            Migration::Code(step) => {
                step(conn).with_context(|| format!("applying migration {version}"))?;
                // recorded after the (idempotent) step succeeds; a crash before
                // this just re-runs the step next start.
                conn.execute(
                    "INSERT INTO registry_meta (k, v) VALUES ('schema_version', ?1)
                     ON CONFLICT(k) DO UPDATE SET v = excluded.v",
                    [version.to_string()],
                )
                .context("recording schema_version")?;
            }
        }
    }
    Ok(())
}

/// Migration 4: reconcile a registry created from the pre-rename `0001` (shipped
/// by #18 before the target-set rework) to the current shape, without data loss.
/// Idempotent; a no-op on a DB already built from the current `0001`.
///
/// The pre-rename `mod_version` had a `target` column + `UNIQUE(mod_id, version,
/// target)` and there was no `mod_version_target` table, so the current queries
/// 500 on such a DB. This adds the child table, moves the old single target into
/// it, then rebuilds `mod_version` into the new shape (dropping the column and
/// the stale uniqueness).
fn reconcile_target_schema(conn: &mut Connection) -> Result<()> {
    // The child table is absent on the pre-rename schema; create it either way.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS mod_version_target (
           mod_version_id INTEGER NOT NULL REFERENCES mod_version(id) ON DELETE CASCADE,
           target         TEXT NOT NULL,
           PRIMARY KEY (mod_version_id, target)
         );
         CREATE INDEX IF NOT EXISTS idx_mvt_target ON mod_version_target(target);",
    )
    .context("0004: ensure mod_version_target")?;

    if !mod_version_has_target_column(conn)? {
        return Ok(()); // already the post-rename shape
    }

    // `foreign_keys` is a no-op inside a transaction, and dropping mod_version
    // would otherwise cascade-delete the rows just backfilled, so toggle it off
    // around the rebuild (SQLite's documented table-redefinition procedure).
    conn.execute_batch("PRAGMA foreign_keys = OFF")
        .context("0004: foreign_keys off")?;
    let tx = conn.transaction().context("0004: begin rebuild")?;
    tx.execute_batch(
        "INSERT OR IGNORE INTO mod_version_target (mod_version_id, target)
           SELECT id, target FROM mod_version;

         CREATE TABLE mod_version_new (
           id           INTEGER PRIMARY KEY,
           mod_id       INTEGER NOT NULL REFERENCES mods(id) ON DELETE CASCADE,
           version      TEXT NOT NULL,
           sha1         TEXT NOT NULL,
           size_bytes   INTEGER NOT NULL,
           filename     TEXT,
           mc_versions  TEXT,
           source       TEXT NOT NULL DEFAULT 'harvested',
           confidence   INTEGER NOT NULL DEFAULT 10,
           created_at   TEXT NOT NULL,
           updated_at   TEXT NOT NULL,
           CHECK (source IN ('harvested','jar-meta','modrinth','inferred','curator','authored'))
         );
         INSERT INTO mod_version_new
           (id, mod_id, version, sha1, size_bytes, filename, mc_versions,
            source, confidence, created_at, updated_at)
           SELECT id, mod_id, version, sha1, size_bytes, filename, mc_versions,
                  source, confidence, created_at, updated_at
           FROM mod_version;
         DROP TABLE mod_version;
         ALTER TABLE mod_version_new RENAME TO mod_version;
         CREATE UNIQUE INDEX idx_mv_sha1 ON mod_version(sha1);
         CREATE INDEX idx_mv_mod ON mod_version(mod_id);",
    )
    .context("0004: rebuild mod_version")?;
    tx.commit().context("0004: commit rebuild")?;
    conn.execute_batch("PRAGMA foreign_keys = ON")
        .context("0004: foreign_keys on")?;

    // the rebuild must not have orphaned any foreign key.
    let violations = {
        let mut stmt = conn.prepare("PRAGMA foreign_key_check")?;
        stmt.query_map([], |_| Ok(()))?.count()
    };
    if violations > 0 {
        bail!("0004 reconcile left {violations} foreign-key violations");
    }
    Ok(())
}

/// Migration 9: drop the retired `provenance` (sc/hivens) and `source` columns
/// from `pack`. Both were write-only -- nothing read them for behaviour -- and
/// pack authorship now lives on the storage-side pack config (owner/tier), so
/// the axis is dead. A plain `DROP COLUMN` is blocked by `CHECK(provenance IN
/// ...)`, and `pack_build` FKs into `pack`, so rebuild with `foreign_keys` off
/// around the swap (SQLite's documented table-redefinition procedure).
/// Idempotent: a no-op once `pack` no longer carries `provenance`.
fn drop_pack_provenance(conn: &mut Connection) -> Result<()> {
    if !pack_has_provenance_column(conn)? {
        return Ok(()); // already rebuilt (or a fresh DB from the current 0001)
    }

    conn.execute_batch("PRAGMA foreign_keys = OFF")
        .context("0009: foreign_keys off")?;
    let tx = conn.transaction().context("0009: begin rebuild")?;
    tx.execute_batch(
        "CREATE TABLE pack_new (
           id         TEXT PRIMARY KEY,
           created_at TEXT NOT NULL,
           updated_at TEXT NOT NULL
         );
         INSERT INTO pack_new (id, created_at, updated_at)
           SELECT id, created_at, updated_at FROM pack;
         DROP TABLE pack;
         ALTER TABLE pack_new RENAME TO pack;",
    )
    .context("0009: rebuild pack")?;
    tx.commit().context("0009: commit rebuild")?;
    conn.execute_batch("PRAGMA foreign_keys = ON")
        .context("0009: foreign_keys on")?;

    // the rebuild must not have orphaned pack_build.
    let violations = {
        let mut stmt = conn.prepare("PRAGMA foreign_key_check")?;
        stmt.query_map([], |_| Ok(()))?.count()
    };
    if violations > 0 {
        bail!("0009 pack rebuild left {violations} foreign-key violations");
    }
    Ok(())
}

/// True if `pack` still carries the retired `provenance` column.
fn pack_has_provenance_column(conn: &Connection) -> Result<bool> {
    let mut stmt = conn.prepare("PRAGMA table_info(pack)")?;
    let names = stmt
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(names.iter().any(|name| name == "provenance"))
}

/// True if `mod_version` still carries the pre-rename `target` column.
/// Migration 22: collapse `conflicts` and `breaks` into one kind with a
/// severity (#129). See `schema/0022_conflict_severity.sql` for the reasoning
/// and the table shape.
///
/// A `Code` step rather than plain SQL for the same reason 0004 and 0009 are:
/// the kind vocabulary is a CHECK constraint, SQLite cannot alter one, and a
/// table rebuild needs `foreign_keys` off around the swap -- a pragma that is a
/// no-op inside the transaction a `Sql` step would run in. `relation` carries
/// FKs into `mods` and `mod_version`, so with the pragma left on the copy is
/// checked row by row against tables the swap is in the middle of.
///
/// Idempotent: a no-op once the column exists.
fn collapse_conflict_kinds(conn: &mut Connection) -> Result<()> {
    if relation_has_severity_column(conn)? {
        return Ok(());
    }
    conn.execute_batch("PRAGMA foreign_keys = OFF")
        .context("0022: foreign_keys off")?;
    let tx = conn.transaction().context("0022: begin rebuild")?;
    tx.execute_batch(include_str!("schema/0022_conflict_severity.sql"))
        .context("0022: rebuild relation")?;
    tx.commit().context("0022: commit rebuild")?;
    conn.execute_batch("PRAGMA foreign_keys = ON")
        .context("0022: foreign_keys on")?;

    // the rebuild must not have orphaned any foreign key
    let violations = {
        let mut stmt = conn.prepare("PRAGMA foreign_key_check")?;
        stmt.query_map([], |_| Ok(()))?.count()
    };
    if violations > 0 {
        bail!("0022 rebuild left {violations} foreign-key violations");
    }
    Ok(())
}

fn relation_has_severity_column(conn: &Connection) -> Result<bool> {
    let mut stmt = conn.prepare("PRAGMA table_info(relation)")?;
    let names = stmt
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(names.iter().any(|name| name == "severity"))
}

fn mod_version_has_target_column(conn: &Connection) -> Result<bool> {
    let mut stmt = conn.prepare("PRAGMA table_info(mod_version)")?;
    let names = stmt
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(names.iter().any(|name| name == "target"))
}

/// Migration 16: widen `mod_release.channel` to the Modrinth `version_type`
/// vocabulary (release/beta/alpha/unknown), folding the legacy 'dev' value into
/// 'alpha'. A CHECK constraint cannot be altered in SQLite, so the table is
/// rebuilt (same procedure as 0004). Idempotent: a no-op once the constraint
/// admits 'alpha'.
fn widen_release_channel_vocab(conn: &mut Connection) -> Result<()> {
    let table_sql: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'mod_release'",
            [],
            |r| r.get(0),
        )
        .context("0016: read mod_release definition")?;
    if table_sql.contains("'alpha'") {
        return Ok(()); // already the widened vocabulary
    }
    // Dropping mod_release would otherwise SET NULL every mod_version.release_id
    // through the FK, so toggle enforcement off around the rebuild (SQLite's
    // documented table-redefinition procedure; the FK references the table by
    // name, so the rename makes it whole again).
    conn.execute_batch("PRAGMA foreign_keys = OFF")
        .context("0016: foreign_keys off")?;
    let tx = conn.transaction().context("0016: begin rebuild")?;
    tx.execute_batch(
        "CREATE TABLE mod_release_new (
           id             INTEGER PRIMARY KEY,
           mod_id         INTEGER NOT NULL REFERENCES mods(id) ON DELETE CASCADE,
           version_number TEXT NOT NULL,
           channel        TEXT NOT NULL DEFAULT 'unknown',
           source         TEXT NOT NULL DEFAULT 'harvested',
           confidence     INTEGER NOT NULL DEFAULT 10,
           created_at     TEXT NOT NULL,
           updated_at     TEXT NOT NULL,
           CHECK (channel IN ('release','beta','alpha','unknown')),
           CHECK (source IN ('harvested','jar-meta','modrinth','inferred','curator','authored'))
         );
         INSERT INTO mod_release_new
           SELECT id, mod_id, version_number,
                  CASE channel WHEN 'dev' THEN 'alpha' ELSE channel END,
                  source, confidence, created_at, updated_at
           FROM mod_release;
         DROP TABLE mod_release;
         ALTER TABLE mod_release_new RENAME TO mod_release;
         CREATE INDEX idx_mod_release_mod ON mod_release(mod_id);
         CREATE UNIQUE INDEX idx_mod_release_key
           ON mod_release(mod_id, version_number, channel);",
    )
    .context("0016: rebuild mod_release")?;
    tx.commit().context("0016: commit rebuild")?;
    conn.execute_batch("PRAGMA foreign_keys = ON")
        .context("0016: foreign_keys on")?;

    // the rebuild must not have orphaned any foreign key
    let violations: Option<String> = conn
        .query_row("PRAGMA foreign_key_check(mod_release)", [], |r| r.get(2))
        .optional()
        .context("0016: foreign_key_check")?;
    if let Some(v) = violations {
        bail!("0016 left a foreign key violation: {v}");
    }
    Ok(())
}

/// Recorded schema version, or 0 on a fresh database (no `registry_meta` yet).
fn current_version(conn: &Connection) -> Result<u32> {
    let has_meta: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'registry_meta'",
            [],
            |_| Ok(true),
        )
        .optional()?
        .unwrap_or(false);
    if !has_meta {
        return Ok(0);
    }
    let v: Option<String> = conn
        .query_row(
            "SELECT v FROM registry_meta WHERE k = 'schema_version'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    Ok(v.and_then(|s| s.parse().ok()).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_apply_and_are_idempotent() {
        let mut conn = Connection::open_in_memory().unwrap();
        apply_pending(&mut conn).unwrap();
        let v1 = current_version(&conn).unwrap();
        // re-running applies nothing
        apply_pending(&mut conn).unwrap();
        let v2 = current_version(&conn).unwrap();
        assert_eq!(v1, v2);
        assert_eq!(v1, MIGRATIONS.last().unwrap().0);

        let tables: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'table'
                 AND name IN ('mods','mod_alias','mod_version','mod_version_target','mod_release',
                              'mod_package','pack','pack_build','pack_build_mod','relation','loader',
                              'loader_parent','registry_meta')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tables, 13);

        let loaders: i64 = conn
            .query_row("SELECT count(*) FROM loader", [], |r| r.get(0))
            .unwrap();
        assert!(loaders >= 6);
    }

    // #129 collapsed `conflicts` and `breaks` into one kind with a severity, and
    // the stored rows carry the old vocabulary. The conversion runs once against
    // real data and cannot be re-run, so it is tested against the shape it will
    // actually meet: the pre-0022 table, with a row of every kind in it.
    #[test]
    fn conflict_severity_0022_converts_the_old_vocabulary() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE mods (id INTEGER PRIMARY KEY);
             CREATE TABLE mod_version (id INTEGER PRIMARY KEY);
             INSERT INTO mods (id) VALUES (10), (11);
             INSERT INTO mod_version (id) VALUES (100);
             CREATE TABLE relation (
               id                   INTEGER PRIMARY KEY,
               from_mod_id          INTEGER NOT NULL,
               target_modid         TEXT NOT NULL,
               target_version_range TEXT,
               kind                 TEXT NOT NULL,
               source               TEXT NOT NULL,
               confidence           INTEGER NOT NULL,
               created_at           TEXT NOT NULL,
               from_mod_version_id  INTEGER,
               CHECK (kind IN ('requires','conflicts','optional_dep','provides','recommends','breaks'))
             );
             INSERT INTO relation
               (id, from_mod_id, target_modid, kind, source, confidence, created_at, from_mod_version_id)
             VALUES
               (1, 10, 'jei',      'requires',    'jar-meta', 50, 'T0', 100),
               (2, 10, 'oldmod',   'conflicts',   'jar-meta', 50, 'T0', 100),
               (3, 10, 'grumpy',   'breaks',      'jar-meta', 50, 'T0', 100),
               (4, 11, 'optional', 'optional_dep','modrinth', 40, 'T0', NULL),
               (5, 11, 'api',      'provides',    'harvested',10, 'T0', NULL),
               (6, 11, 'nice',     'recommends',  'jar-meta', 50, 'T0', NULL);",
        )
        .unwrap();

        collapse_conflict_kinds(&mut conn).unwrap();
        // the step re-runs whole if a later migration fails before its version
        // is recorded, so running it twice must change nothing
        collapse_conflict_kinds(&mut conn).unwrap();

        let row = |id: i64| -> (String, Option<String>) {
            conn.query_row(
                "SELECT kind, severity FROM relation WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
        };
        // the hard declaration keeps its name and gains the intensity it always had
        assert_eq!(row(2), ("conflicts".into(), Some("hard".into())));
        // and the advisory one stops claiming to be the harder kind
        assert_eq!(row(3), ("conflicts".into(), Some("soft".into())));
        // everything else is untouched, and states no intensity
        assert_eq!(row(1), ("requires".into(), None));
        assert_eq!(row(4), ("optional_dep".into(), None));
        assert_eq!(row(5), ("provides".into(), None));
        assert_eq!(row(6), ("recommends".into(), None));

        // nothing was dropped: the two incompatibilities stay two rows, which is
        // what the severity in the dedupe key is for
        let n: i64 = conn
            .query_row("SELECT count(*) FROM relation", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 6);
        // and the artifact scope survived the rebuild
        let scoped: i64 = conn
            .query_row(
                "SELECT from_mod_version_id FROM relation WHERE id = 2",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(scoped, 100);

        // the vocabulary is closed now: the old name no longer parses as a kind
        assert!(super::super::model::RelKind::parse("breaks").is_none());
    }

    // The CHECK is the invariant, not a comment: a conflict without an intensity
    // and a requirement with one are both rejected by the store itself.
    #[test]
    fn severity_belongs_to_conflicts_and_only_to_them() {
        let mut conn = Connection::open_in_memory().unwrap();
        apply_pending(&mut conn).unwrap();
        conn.execute(
            "INSERT INTO mods (id, created_at, updated_at) VALUES (1, 'T0', 'T0')",
            [],
        )
        .unwrap();

        let insert = |kind: &str, severity: Option<&str>| {
            conn.execute(
                "INSERT INTO relation
                   (from_mod_id, target_modid, kind, severity, source, confidence, created_at)
                 VALUES (1, 'target', ?1, ?2, 'authored', 90, 'T0')",
                rusqlite::params![kind, severity],
            )
        };
        assert!(insert("conflicts", None).is_err());
        assert!(insert("requires", Some("hard")).is_err());
        assert!(insert("conflicts", Some("sort-of")).is_err());
        if let Err(e) = insert("conflicts", Some("hard")) {
            panic!("a hard conflict must be storable: {e}");
        }
        assert!(insert("requires", None).is_ok());
    }

    // A registry created from the pre-rename 0001 (mod_version with a `target`
    // column + UNIQUE(mod_id,version,target), no mod_version_target) must
    // reconcile losslessly and idempotently.
    #[test]
    fn reconcile_0004_migrates_stale_target_schema() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE mods (id INTEGER PRIMARY KEY);
             CREATE TABLE mod_version (
               id INTEGER PRIMARY KEY,
               mod_id INTEGER NOT NULL REFERENCES mods(id) ON DELETE CASCADE,
               version TEXT NOT NULL,
               target TEXT NOT NULL DEFAULT 'any',
               sha1 TEXT NOT NULL,
               size_bytes INTEGER NOT NULL,
               filename TEXT,
               mc_versions TEXT,
               source TEXT NOT NULL DEFAULT 'harvested',
               confidence INTEGER NOT NULL DEFAULT 10,
               created_at TEXT NOT NULL,
               updated_at TEXT NOT NULL,
               UNIQUE (mod_id, version, target)
             );
             CREATE UNIQUE INDEX idx_mv_sha1 ON mod_version(sha1);
             INSERT INTO mods (id) VALUES (1);
             INSERT INTO mod_version
               (id, mod_id, version, target, sha1, size_bytes, created_at, updated_at)
               VALUES (1, 1, '2.5', 'forge', 'sha_a', 10, 'T', 'T');",
        )
        .unwrap();
        assert!(mod_version_has_target_column(&conn).unwrap());

        reconcile_target_schema(&mut conn).unwrap();

        // the old single target moved into the child table
        let tgt: String = conn
            .query_row(
                "SELECT target FROM mod_version_target WHERE mod_version_id = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tgt, "forge");
        // the column is gone and the row data survived
        assert!(!mod_version_has_target_column(&conn).unwrap());
        let (v, s): (String, String) = conn
            .query_row(
                "SELECT version, sha1 FROM mod_version WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((v.as_str(), s.as_str()), ("2.5", "sha_a"));
        // the stale UNIQUE(mod_id,version,target) is gone: a second artifact of
        // the same mod+version inserts cleanly now
        conn.execute(
            "INSERT INTO mod_version
               (mod_id, version, sha1, size_bytes, created_at, updated_at)
             VALUES (1, '2.5', 'sha_b', 20, 'T', 'T')",
            [],
        )
        .unwrap();

        // idempotent: a second run is a no-op
        reconcile_target_schema(&mut conn).unwrap();
        assert!(!mod_version_has_target_column(&conn).unwrap());
    }

    // Migration 7 groups existing files into one 'unknown' release per
    // (mod_id, version) and links each file up to it. Two files of one mod at the
    // same number collapse to one release; a different number, or the same number
    // under a different mod, is its own release.
    #[test]
    fn migration_0007_backfills_one_release_per_mod_version_group() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE mods (id INTEGER PRIMARY KEY);
             CREATE TABLE mod_version (
               id INTEGER PRIMARY KEY,
               mod_id INTEGER NOT NULL REFERENCES mods(id),
               version TEXT NOT NULL,
               sha1 TEXT NOT NULL,
               size_bytes INTEGER NOT NULL,
               filename TEXT,
               mc_versions TEXT,
               source TEXT NOT NULL DEFAULT 'harvested',
               confidence INTEGER NOT NULL DEFAULT 10,
               created_at TEXT NOT NULL,
               updated_at TEXT NOT NULL
             );
             INSERT INTO mods (id) VALUES (1), (2);
             INSERT INTO mod_version (mod_id, version, sha1, size_bytes, created_at, updated_at)
               VALUES (1, '1.0', 'sha_a', 1, 'T', 'T'),
                      (1, '1.0', 'sha_b', 1, 'T', 'T'),
                      (1, '2.0', 'sha_c', 1, 'T', 'T'),
                      (2, '1.0', 'sha_d', 1, 'T', 'T');",
        )
        .unwrap();

        conn.execute_batch(include_str!("schema/0007_mod_release.sql"))
            .unwrap();

        let release_id = |sha: &str| -> i64 {
            conn.query_row(
                "SELECT release_id FROM mod_version WHERE sha1 = ?1",
                [sha],
                |r| r.get(0),
            )
            .unwrap()
        };

        let releases: i64 = conn
            .query_row("SELECT count(*) FROM mod_release", [], |r| r.get(0))
            .unwrap();
        assert_eq!(releases, 3, "(1,1.0) (1,2.0) (2,1.0)");
        assert_eq!(release_id("sha_a"), release_id("sha_b"), "same mod+number");
        assert_ne!(release_id("sha_a"), release_id("sha_c"), "different number");
        assert_ne!(release_id("sha_a"), release_id("sha_d"), "different mod");

        let ungrouped: i64 = conn
            .query_row(
                "SELECT count(*) FROM mod_version WHERE release_id IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(ungrouped, 0, "every file grouped");

        let (vn, ch): (String, String) = conn
            .query_row(
                "SELECT version_number, channel FROM mod_release WHERE id = ?1",
                [release_id("sha_a")],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((vn.as_str(), ch.as_str()), ("1.0", "unknown"));
    }

    // Migration 11 must not invent provenance. A mod with exactly one artifact
    // leaves no doubt which jar a derived edge came from, so it is attached; a mod
    // with several genuinely cannot be resolved after the fact, so the row stays
    // mod-level (today's semantics, no regression) until a re-harvest scopes it.
    // An authored fact was asserted about the mod and is left alone.
    #[test]
    fn migration_0011_attaches_only_unambiguous_derived_rows() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE mods (id INTEGER PRIMARY KEY);
             CREATE TABLE mod_version (id INTEGER PRIMARY KEY, mod_id INTEGER NOT NULL);
             CREATE TABLE relation (
               id                   INTEGER PRIMARY KEY,
               from_mod_id          INTEGER NOT NULL,
               target_modid         TEXT NOT NULL,
               target_version_range TEXT,
               kind                 TEXT NOT NULL,
               source               TEXT NOT NULL,
               confidence           INTEGER NOT NULL,
               created_at           TEXT NOT NULL
             );
             CREATE UNIQUE INDEX idx_rel_dedupe
               ON relation(from_mod_id, target_modid, kind, source, COALESCE(target_version_range, ''));
             INSERT INTO mods (id) VALUES (1), (2);
             -- mod 1 has one artifact; mod 2 has two
             INSERT INTO mod_version (id, mod_id) VALUES (10, 1), (20, 2), (21, 2);
             INSERT INTO relation (id, from_mod_id, target_modid, kind, source, confidence, created_at)
               VALUES (1, 1, 'lib', 'requires', 'jar-meta', 8, 'T'),
                      (2, 2, 'lib', 'requires', 'jar-meta', 8, 'T'),
                      (3, 1, 'foe', 'conflicts', 'authored', 40, 'T');",
        )
        .unwrap();

        conn.execute_batch(include_str!("schema/0011_relation_artifact.sql"))
            .unwrap();

        let scope = |id: i64| -> Option<i64> {
            conn.query_row(
                "SELECT from_mod_version_id FROM relation WHERE id = ?1",
                [id],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(scope(1), Some(10), "one artifact: no doubt, attach it");
        assert_eq!(scope(2), None, "two artifacts: which jar is unknowable now");
        assert_eq!(scope(3), None, "an authored fact is about the mod");
    }

    // A pack table from the pre-drop 0001 (provenance/source columns + a
    // pack_build child) rebuilds to the bare shape, keeps the child rows, and is
    // idempotent on a second run.
    #[test]
    fn migration_0009_drops_pack_provenance_and_keeps_builds() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE pack (
               id         TEXT PRIMARY KEY,
               provenance TEXT NOT NULL DEFAULT 'hivens',
               source     TEXT NOT NULL DEFAULT 'harvested',
               created_at TEXT NOT NULL,
               updated_at TEXT NOT NULL,
               CHECK (provenance IN ('sc','hivens'))
             );
             CREATE TABLE pack_build (
               id      INTEGER PRIMARY KEY,
               pack_id TEXT NOT NULL REFERENCES pack(id) ON DELETE CASCADE
             );
             INSERT INTO pack (id, provenance, source, created_at, updated_at)
               VALUES ('P', 'sc', 'authored', 'T', 'T');
             INSERT INTO pack_build (id, pack_id) VALUES (1, 'P');",
        )
        .unwrap();
        assert!(pack_has_provenance_column(&conn).unwrap());

        drop_pack_provenance(&mut conn).unwrap();

        assert!(!pack_has_provenance_column(&conn).unwrap());
        let id: String = conn
            .query_row("SELECT id FROM pack WHERE id = 'P'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(id, "P");
        let builds: i64 = conn
            .query_row(
                "SELECT count(*) FROM pack_build WHERE pack_id = 'P'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(builds, 1, "the pack_build child survived the rebuild");

        // idempotent: a second run is a no-op
        drop_pack_provenance(&mut conn).unwrap();
        assert!(!pack_has_provenance_column(&conn).unwrap());
    }

    #[test]
    fn widen_release_channel_folds_dev_and_admits_alpha() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE mods (id INTEGER PRIMARY KEY, created_at TEXT, updated_at TEXT);
             CREATE TABLE mod_release (
               id             INTEGER PRIMARY KEY,
               mod_id         INTEGER NOT NULL REFERENCES mods(id) ON DELETE CASCADE,
               version_number TEXT NOT NULL,
               channel        TEXT NOT NULL DEFAULT 'unknown',
               source         TEXT NOT NULL DEFAULT 'harvested',
               confidence     INTEGER NOT NULL DEFAULT 10,
               created_at     TEXT NOT NULL,
               updated_at     TEXT NOT NULL,
               CHECK (channel IN ('release','beta','dev','unknown')),
               CHECK (source IN ('harvested','jar-meta','modrinth','inferred','curator','authored'))
             );
             CREATE TABLE mod_version (
               id INTEGER PRIMARY KEY,
               release_id INTEGER REFERENCES mod_release(id) ON DELETE SET NULL
             );
             INSERT INTO mods (id) VALUES (1);
             INSERT INTO mod_release (id, mod_id, version_number, channel, created_at, updated_at)
               VALUES (7, 1, '2.3', 'dev', 'T', 'T');
             INSERT INTO mod_version (id, release_id) VALUES (1, 7);",
        )
        .unwrap();

        widen_release_channel_vocab(&mut conn).unwrap();

        // 'dev' folded to 'alpha'; ids stable so the file link survives
        let (ch, rid): (String, i64) = conn
            .query_row(
                "SELECT r.channel, v.release_id FROM mod_release r
                 JOIN mod_version v ON v.release_id = r.id WHERE r.id = 7",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(ch, "alpha");
        assert_eq!(rid, 7);

        // the widened constraint admits 'alpha' inserts
        conn.execute(
            "INSERT INTO mod_release (mod_id, version_number, channel, created_at, updated_at)
             VALUES (1, '3.0', 'alpha', 'T', 'T')",
            [],
        )
        .unwrap();

        // idempotent: a second run is a no-op
        widen_release_channel_vocab(&mut conn).unwrap();
        let n: i64 = conn
            .query_row("SELECT count(*) FROM mod_release", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 2);
    }
}
