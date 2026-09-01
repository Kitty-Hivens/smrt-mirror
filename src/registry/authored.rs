//! Authored (precious) writers: the manual-moderation layer. Every row written
//! here carries `source = 'authored'`, which the harvest's never-clobber guard
//! (`WHERE source NOT IN ('curator','authored')`) preserves across re-harvests.
//! Driven through the `Registry` methods (used by the CLI + admin HTTP).

use super::model::{RelKind, Severity, Source};
use anyhow::{Result, bail};
use rusqlite::{Connection, OptionalExtension, params};

/// The valid `mod_release.channel` values (mirrors the schema CHECK).
pub const CHANNELS: &[&str] = &["release", "beta", "alpha", "unknown"];

fn valid_channel(ch: &str) -> bool {
    CHANNELS.contains(&ch)
}

/// The refusal, listing what would have been accepted. Built from [`CHANNELS`]
/// rather than written out, because the written-out version drifted: it went on
/// offering `dev` for two releases after migration 0016 folded that channel into
/// `alpha`, so the one message an operator sees when a channel is refused named
/// a channel that no longer exists and omitted the one they wanted.
fn bad_channel(ch: &str) -> anyhow::Error {
    anyhow::anyhow!("channel must be one of {}, got {ch:?}", CHANNELS.join("/"))
}

/// Record an operator-asserted relation. De-duped against an identical authored
/// row by the unique index; coexists with rows of other sources (precedence
/// resolves later).
///
/// Mod-level (no artifact scope): an operator asserting that two mods do not get
/// along is stating it about the mods, not about one build of one of them.
/// Delegates rather than writing its own INSERT, so the rule about which kinds
/// carry a severity lives in exactly one place (#129).
pub fn add_authored_relation(
    conn: &Connection,
    from_mod_id: i64,
    target_modid: &str,
    kind: RelKind,
    severity: Option<Severity>,
    now: &str,
) -> Result<()> {
    super::upsert::upsert_relation(
        conn,
        from_mod_id,
        None,
        target_modid,
        None,
        kind,
        severity,
        Source::Authored,
        now,
    )?;
    Ok(())
}

/// Remove an authored relation. Only `authored` rows -- a harvested fact would
/// just reappear on the next harvest (open-world; there is no "not" assertion).
pub fn remove_authored_relation(
    conn: &Connection,
    from_mod_id: i64,
    target_modid: &str,
    kind: RelKind,
) -> Result<usize> {
    Ok(conn.execute(
        "DELETE FROM relation
         WHERE from_mod_id = ?1 AND target_modid = ?2 AND kind = ?3 AND source = 'authored'",
        params![from_mod_id, target_modid, kind.as_str()],
    )?)
}

/// The valid `jar_class.kind` values.
pub const JAR_KINDS: &[&str] = &["mod", "coremod", "library"];

/// Set (or clear) an operator-asserted jar classification -- the debug-rung
/// escape hatch for a jar the classifier cannot decide or decides with a
/// low-confidence heuristic. Precious: the harvest's jar_class refresh skips
/// authored rows. Refused for a Modrinth-identified mod: the project's
/// environment flags stay authoritative and are not hand-overridable (the
/// per-mod best-source cascade). `side_confidence` is stored `high` so the
/// client invariant treats the assertion as solid.
pub fn set_authored_jar_class(
    conn: &Connection,
    sha1: &str,
    kind: &str,
    side: Option<&str>,
    match_policy: Option<&str>,
    remove: bool,
) -> Result<()> {
    if remove {
        // drop only the authored row; a scanned jar regains its derived row on
        // the next harvest
        conn.execute(
            "DELETE FROM jar_class WHERE sha1 = ?1 AND source = 'authored'",
            params![sha1],
        )?;
        return Ok(());
    }
    if !JAR_KINDS.contains(&kind) {
        bail!("kind must be one of {JAR_KINDS:?}");
    }
    if let Some(s) = side
        && crate::domain::SideClass::parse(s).is_none()
    {
        bail!("side must be one of client | server | both");
    }
    if let Some(p) = match_policy
        && crate::domain::MatchPolicy::parse(p).is_none()
    {
        bail!("match_policy must be one of must_match | tolerant");
    }
    if side == Some("client") && match_policy == Some("must_match") {
        bail!(
            "client + must_match is contradictory: a client-side mod never requires itself on the server"
        );
    }
    // the non-Modrinth guard: a jar whose mod carries a Modrinth identity is
    // classified by the project environment flags, not by hand
    let modrinth: Option<String> = conn
        .query_row(
            "SELECT a.external_key FROM mod_version mv
             JOIN mod_alias a ON a.mod_id = mv.mod_id AND a.source = 'modrinth'
             WHERE mv.sha1 = ?1 LIMIT 1",
            params![sha1],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(pid) = modrinth {
        bail!(
            "jar's mod is Modrinth-identified (project {pid}); its environment flags are authoritative -- fix them upstream instead"
        );
    }
    let confidence = side.map(|_| "high");
    conn.execute(
        "INSERT INTO jar_class (sha1, kind, side, match_policy, side_confidence, source)
         VALUES (?1, ?2, ?3, ?4, ?5, 'authored')
         ON CONFLICT(sha1) DO UPDATE SET
           kind = excluded.kind,
           side = excluded.side,
           match_policy = excluded.match_policy,
           side_confidence = excluded.side_confidence,
           source = 'authored'",
        params![sha1, kind, side, match_policy, confidence],
    )?;
    Ok(())
}

/// Add or remove a mutual authored conflict between two mods (the launcher
/// treats conflicts as bidirectional, so both directions are written).
#[allow(clippy::too_many_arguments)]
pub fn set_authored_conflict(
    conn: &Connection,
    a_mod_id: i64,
    a_modid: &str,
    b_mod_id: i64,
    b_modid: &str,
    now: &str,
    remove: bool,
) -> Result<()> {
    if remove {
        remove_authored_relation(conn, a_mod_id, b_modid, RelKind::Conflicts)?;
        remove_authored_relation(conn, b_mod_id, a_modid, RelKind::Conflicts)?;
    } else {
        // an operator writing "these two do not get along" means the hard one
        add_authored_relation(
            conn,
            a_mod_id,
            b_modid,
            RelKind::Conflicts,
            Some(Severity::Hard),
            now,
        )?;
        add_authored_relation(
            conn,
            b_mod_id,
            a_modid,
            RelKind::Conflicts,
            Some(Severity::Hard),
            now,
        )?;
    }
    Ok(())
}

/// Merge one mod identity into another: repoint every alias, release, file, and
/// relation from `from_mod_id` onto `into_mod_id`, then drop the now-empty source
/// mod. For when a mod ended up under two `mods` rows (a modid-harvested jar and
/// a Modrinth-identified one that failed to collapse, or two hand-created rows).
///
/// Release-key collision: `mod_release` is unique on `(mod_id, version_number,
/// channel)`, so when both mods carry a release with the same key, the source
/// release's files are folded into the target's release rather than violating it.
/// Relation and package rows dedupe onto the target (the unique index ignores a
/// duplicate; any left behind by that ignore is dropped). The survivor is marked
/// `authored` so the merge decision is precious. Rejects `from == into` and an
/// unknown id. Idempotent only in that a re-run with the (now gone) source id
/// fails the existence check.
pub fn merge_mods(conn: &Connection, from_mod_id: i64, into_mod_id: i64, now: &str) -> Result<()> {
    if from_mod_id == into_mod_id {
        bail!("cannot merge a mod into itself");
    }
    let exists = |id: i64| -> Result<bool> {
        Ok(conn
            .query_row("SELECT 1 FROM mods WHERE id = ?1", params![id], |_| Ok(()))
            .optional()?
            .is_some())
    };
    if !exists(from_mod_id)? {
        bail!("source mod {from_mod_id} not in registry");
    }
    if !exists(into_mod_id)? {
        bail!("target mod {into_mod_id} not in registry");
    }

    fold_mods(conn, from_mod_id, into_mod_id, now)?;
    // mark the survivor precious so the deliberate merge decision is not undone by
    // a later harvest.
    conn.execute(
        "UPDATE mods SET source = 'authored', updated_at = ?2 WHERE id = ?1",
        params![into_mod_id, now],
    )?;
    Ok(())
}

/// The identity-repointing core of a merge: fold every row owned by `from_mod_id`
/// onto `into_mod_id` (colliding releases merged, not violated) and delete the
/// emptied source, without touching the survivor's `source`. Shared by the
/// operator [`merge_mods`] (which then marks the survivor precious) and the
/// harvest's automatic collision merge (which keeps it harvest-managed). A
/// `from == into` call is a no-op.
pub fn fold_mods(conn: &Connection, from_mod_id: i64, into_mod_id: i64, now: &str) -> Result<()> {
    if from_mod_id == into_mod_id {
        return Ok(());
    }

    // 1. releases: fold a colliding (version_number, channel) into the target's
    //    release, repoint the rest.
    let from_releases: Vec<(i64, String, String)> = {
        let mut stmt =
            conn.prepare("SELECT id, version_number, channel FROM mod_release WHERE mod_id = ?1")?;
        stmt.query_map(params![from_mod_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    };
    for (rid, vnum, chan) in from_releases {
        let target: Option<i64> = conn
            .query_row(
                "SELECT id FROM mod_release
                 WHERE mod_id = ?1 AND version_number = ?2 AND channel = ?3",
                params![into_mod_id, vnum, chan],
                |r| r.get(0),
            )
            .optional()?;
        match target {
            Some(tid) => {
                conn.execute(
                    "UPDATE mod_version SET release_id = ?2 WHERE release_id = ?1",
                    params![rid, tid],
                )?;
                conn.execute("DELETE FROM mod_release WHERE id = ?1", params![rid])?;
            }
            None => {
                conn.execute(
                    "UPDATE mod_release SET mod_id = ?2, updated_at = ?3 WHERE id = ?1",
                    params![rid, into_mod_id, now],
                )?;
            }
        }
    }

    // 2. files, 3. aliases -- both keyed by content/external key, no collision.
    conn.execute(
        "UPDATE mod_version SET mod_id = ?2, updated_at = ?3 WHERE mod_id = ?1",
        params![from_mod_id, into_mod_id, now],
    )?;
    conn.execute(
        "UPDATE mod_alias SET mod_id = ?2 WHERE mod_id = ?1",
        params![from_mod_id, into_mod_id],
    )?;

    // 4. outgoing relations + 5. package prefixes: dedupe onto the target, drop
    //    any the unique index refused to move.
    conn.execute(
        "UPDATE OR IGNORE relation SET from_mod_id = ?2 WHERE from_mod_id = ?1",
        params![from_mod_id, into_mod_id],
    )?;
    conn.execute(
        "DELETE FROM relation WHERE from_mod_id = ?1",
        params![from_mod_id],
    )?;
    conn.execute(
        "UPDATE OR IGNORE mod_package SET mod_id = ?2 WHERE mod_id = ?1",
        params![from_mod_id, into_mod_id],
    )?;
    conn.execute(
        "DELETE FROM mod_package WHERE mod_id = ?1",
        params![from_mod_id],
    )?;

    // 6. drop the emptied source.
    conn.execute("DELETE FROM mods WHERE id = ?1", params![from_mod_id])?;
    Ok(())
}

// ── mod / release / file identity (the authoring door) ───────────────────────

/// Which mod a file belongs to: an existing surrogate id the operator picked, or
/// a new authored identity to create with this display name.
pub enum ModRef<'a> {
    Existing(i64),
    New { name: &'a str },
}

/// The full identity an operator sets for one cached jar (by sha1): which mod it
/// is, which release (version_number + channel) it belongs to, and the file's own
/// loader + Minecraft-version facets. Everything written is `source='authored'`,
/// so a re-harvest never overwrites it (the never-clobber guard).
pub struct FileIdentity<'a> {
    pub sha1: &'a str,
    pub size_bytes: i64,
    pub filename: Option<&'a str>,
    pub mod_ref: ModRef<'a>,
    pub version_number: &'a str,
    pub channel: &'a str,
    /// Loader ids the jar suits; empty falls back to `any`.
    pub loaders: &'a [String],
    pub mc_versions: &'a [String],
}

/// Assign a cached jar its mod + release + facets as an operator decision. Creates
/// the mod (when `New`) and the release (by `mod_id, version_number, channel`) if
/// absent, then upserts the file as `authored` and (re)sets its loader targets.
/// Returns the `mod_version` id. This IS the authored write, so it overwrites even
/// a prior authored row for the same sha1.
pub fn author_file_identity(conn: &Connection, id: &FileIdentity, now: &str) -> Result<i64> {
    if id.version_number.trim().is_empty() {
        bail!("version_number must not be empty");
    }
    if !valid_channel(id.channel) {
        return Err(bad_channel(id.channel));
    }

    let mod_id = match id.mod_ref {
        ModRef::Existing(mid) => {
            let exists = conn
                .query_row("SELECT 1 FROM mods WHERE id = ?1", params![mid], |_| Ok(()))
                .optional()?
                .is_some();
            if !exists {
                bail!("mod #{mid} does not exist");
            }
            mid
        }
        ModRef::New { name } => {
            let name = name.trim();
            if name.is_empty() {
                bail!("new mod name must not be empty");
            }
            conn.execute(
                "INSERT INTO mods (slug, canonical_name, source, confidence, created_at, updated_at)
                 VALUES (NULL, ?1, 'authored', ?2, ?3, ?3)",
                params![name, Source::Authored.rank(), now],
            )?;
            conn.last_insert_rowid()
        }
    };

    let release_id = match conn
        .query_row(
            "SELECT id FROM mod_release WHERE mod_id = ?1 AND version_number = ?2 AND channel = ?3",
            params![mod_id, id.version_number, id.channel],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
    {
        Some(rid) => rid,
        None => {
            conn.execute(
                "INSERT INTO mod_release
                   (mod_id, version_number, channel, source, confidence, created_at, updated_at)
                 VALUES (?1, ?2, ?3, 'authored', ?4, ?5, ?5)",
                params![
                    mod_id,
                    id.version_number,
                    id.channel,
                    Source::Authored.rank(),
                    now
                ],
            )?;
            conn.last_insert_rowid()
        }
    };

    let mc = if id.mc_versions.is_empty() {
        None
    } else {
        Some(serde_json::to_string(id.mc_versions)?)
    };
    conn.execute(
        "INSERT INTO mod_version
           (mod_id, version, sha1, size_bytes, filename, mc_versions, release_id,
            source, confidence, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'authored', ?8, ?9, ?9)
         ON CONFLICT(sha1) DO UPDATE SET
           mod_id = excluded.mod_id,
           version = excluded.version,
           size_bytes = excluded.size_bytes,
           filename = COALESCE(excluded.filename, mod_version.filename),
           mc_versions = excluded.mc_versions,
           release_id = excluded.release_id,
           source = 'authored',
           confidence = excluded.confidence,
           updated_at = excluded.updated_at",
        params![
            mod_id,
            id.version_number,
            id.sha1,
            id.size_bytes,
            id.filename,
            mc,
            release_id,
            Source::Authored.rank(),
            now
        ],
    )?;
    let mv_id: i64 = conn.query_row(
        "SELECT id FROM mod_version WHERE sha1 = ?1",
        params![id.sha1],
        |r| r.get(0),
    )?;

    conn.execute(
        "DELETE FROM mod_version_target WHERE mod_version_id = ?1",
        params![mv_id],
    )?;
    let any = [String::from("any")];
    let effective: &[String] = if id.loaders.is_empty() {
        &any
    } else {
        id.loaders
    };
    for t in effective {
        conn.execute(
            "INSERT OR IGNORE INTO mod_version_target (mod_version_id, target) VALUES (?1, ?2)",
            params![mv_id, t],
        )?;
    }
    Ok(mv_id)
}

/// Rename a mod (canonical name and/or Modrinth slug) as an operator decision,
/// stamping `source='authored'` so a re-harvest's `set_mod_meta` won't revert it.
pub fn rename_mod(
    conn: &Connection,
    mod_id: i64,
    name: Option<&str>,
    slug: Option<&str>,
    now: &str,
) -> Result<()> {
    if name.is_none() && slug.is_none() {
        return Ok(());
    }
    let n = conn.execute(
        "UPDATE mods SET
           canonical_name = COALESCE(?2, canonical_name),
           slug           = COALESCE(?3, slug),
           source = 'authored', updated_at = ?4
         WHERE id = ?1",
        params![mod_id, name, slug, now],
    )?;
    if n == 0 {
        bail!("mod #{mod_id} does not exist");
    }
    Ok(())
}

/// Edit a release's version number and/or channel as an operator decision
/// (`source='authored'`). Errors if the change collides with a sibling release
/// (the `mod_id, version_number, channel` uniqueness).
pub fn edit_release(
    conn: &Connection,
    release_id: i64,
    version_number: Option<&str>,
    channel: Option<&str>,
    now: &str,
) -> Result<()> {
    if let Some(ch) = channel
        && !valid_channel(ch)
    {
        return Err(bad_channel(ch));
    }
    if version_number.is_none() && channel.is_none() {
        return Ok(());
    }
    let n = conn.execute(
        "UPDATE mod_release SET
           version_number = COALESCE(?2, version_number),
           channel        = COALESCE(?3, channel),
           source = 'authored', updated_at = ?4
         WHERE id = ?1",
        params![release_id, version_number, channel, now],
    )?;
    if n == 0 {
        bail!("release #{release_id} does not exist");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::registry::model::{RelKind, Source};
    use crate::registry::{Registry, queries, upsert};
    use rusqlite::{OptionalExtension, params};

    #[test]
    fn authored_facts_survive_reharvest() {
        let r = Registry::open_in_memory().unwrap();
        // harvest two mods + a pack (source='harvested')
        r.with_txn(|c| {
            let a = upsert::upsert_mod_by_alias(c, &[("modid", "amod")], "T0")?;
            upsert::upsert_mod_version(c, a, "1", &["forge"], "sha_a", 1, None, None, "T0")?;
            let b = upsert::upsert_mod_by_alias(c, &[("modid", "bmod")], "T0")?;
            upsert::upsert_mod_version(c, b, "1", &["forge"], "sha_b", 1, None, None, "T0")?;
            upsert::upsert_pack(c, "P", "T0")?;
            Ok(())
        })
        .unwrap();

        // operator moderation: an authored conflict never declared upstream
        r.set_conflict("amod", "bmod", false).unwrap();

        // a re-harvest (source='harvested') must not revert it
        r.with_txn(|c| upsert::upsert_pack(c, "P", "T2")).unwrap();

        r.with_conn(|c| {
            let conflicts: i64 = c.query_row(
                "SELECT count(*) FROM relation WHERE kind='conflicts' AND source='authored'",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(conflicts, 2, "mutual authored conflict, both directions");
            Ok(())
        })
        .unwrap();

        // an unknown mod is rejected, not silently ignored
        assert!(r.set_conflict("amod", "ghost", false).is_err());

        // removal clears both directions
        r.set_conflict("amod", "bmod", true).unwrap();
        r.with_conn(|c| {
            let n: i64 = c.query_row(
                "SELECT count(*) FROM relation WHERE kind='conflicts' AND source='authored'",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(n, 0);
            // sanity: the alias resolution the methods rely on
            assert!(queries::mod_id_for_alias(c, "modid", "amod")?.is_some());
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn merge_folds_identities_and_colliding_releases() {
        let r = Registry::open_in_memory().unwrap();
        // the same mod split in two rows: a modid-harvested one and a
        // Modrinth-identified one that failed to collapse
        let (a, b) = r
            .with_txn(|c| {
                let a = upsert::upsert_mod_by_alias(c, &[("modid", "foo")], "T0")?;
                upsert::upsert_mod_version(c, a, "1.0", &["forge"], "sha_a", 1, None, None, "T0")?;
                upsert::upsert_relation(
                    c,
                    a,
                    None,
                    "bar",
                    None,
                    RelKind::Requires,
                    None,
                    Source::Inferred,
                    "T0",
                )?;
                let b = upsert::upsert_mod_by_alias(c, &[("modrinth", "proj")], "T0")?;
                // 1.0 collides on (version_number, channel) with a's release
                upsert::upsert_mod_version(c, b, "1.0", &["fabric"], "sha_b", 1, None, None, "T0")?;
                upsert::upsert_mod_version(c, b, "2.0", &["fabric"], "sha_c", 1, None, None, "T0")?;
                Ok((a, b))
            })
            .unwrap();

        r.merge_mods(a, b).unwrap();

        r.with_conn(|c| {
            // source gone; both aliases resolve to the survivor
            assert!(
                c.query_row("SELECT 1 FROM mods WHERE id = ?1", params![a], |_| Ok(()))
                    .optional()?
                    .is_none()
            );
            assert_eq!(queries::mod_id_for_alias(c, "modid", "foo")?, Some(b));
            assert_eq!(queries::mod_id_for_alias(c, "modrinth", "proj")?, Some(b));
            // all three files under the survivor
            let files: i64 = c.query_row(
                "SELECT count(*) FROM mod_version WHERE mod_id = ?1",
                params![b],
                |r| r.get(0),
            )?;
            assert_eq!(files, 3);
            // the colliding 1.0/unknown release is folded, not duplicated
            let rel_10: i64 = c.query_row(
                "SELECT count(*) FROM mod_release
                 WHERE mod_id = ?1 AND version_number = '1.0' AND channel = 'unknown'",
                params![b],
                |r| r.get(0),
            )?;
            assert_eq!(rel_10, 1, "colliding release folded");
            let files_10: i64 = c.query_row(
                "SELECT count(*) FROM mod_version mv JOIN mod_release r ON r.id = mv.release_id
                 WHERE r.mod_id = ?1 AND r.version_number = '1.0'",
                params![b],
                |r| r.get(0),
            )?;
            assert_eq!(files_10, 2, "both 1.0 files grouped under the survivor");
            // outgoing relation repointed off the dropped source
            let moved: i64 = c.query_row(
                "SELECT count(*) FROM relation WHERE from_mod_id = ?1 AND target_modid = 'bar'",
                params![b],
                |r| r.get(0),
            )?;
            assert_eq!(moved, 1);
            let orphaned: i64 = c.query_row(
                "SELECT count(*) FROM relation WHERE from_mod_id = ?1",
                params![a],
                |r| r.get(0),
            )?;
            assert_eq!(orphaned, 0);
            // survivor marked precious
            let src: String =
                c.query_row("SELECT source FROM mods WHERE id = ?1", params![b], |r| {
                    r.get(0)
                })?;
            assert_eq!(src, "authored");
            Ok(())
        })
        .unwrap();

        // self-merge and an unknown id are rejected
        assert!(r.merge_mods(b, b).is_err());
        assert!(r.merge_mods(9999, b).is_err());
    }

    #[test]
    fn upsert_auto_folds_a_modid_and_project_split() {
        // The mirror ships IC2 from a Modrinth re-upload (known by project id) while
        // a separate row carries the same mod by its forge modid. A later seed whose
        // jar bridges both identities must fold the two rows into one, so a modid dep
        // and a project placement resolve to the same mod.
        let r = Registry::open_in_memory().unwrap();
        let survivor = r
            .with_txn(|c| {
                let a = upsert::upsert_mod_by_alias(c, &[("modid", "ic2")], "T0")?;
                upsert::upsert_mod_version(c, a, "2.8", &["forge"], "sha_a", 1, None, None, "T0")?;
                let b = upsert::upsert_mod_by_alias(c, &[("modrinth", "wTncj5gs")], "T0")?;
                upsert::upsert_mod_version(c, b, "2.8", &["forge"], "sha_b", 1, None, None, "T0")?;
                // the bridging seed carries both identities at once
                upsert::upsert_mod_by_alias(c, &[("modid", "ic2"), ("modrinth", "wTncj5gs")], "T1")
            })
            .unwrap();

        r.with_conn(|c| {
            assert_eq!(
                queries::mod_id_for_alias(c, "modid", "ic2")?,
                Some(survivor)
            );
            assert_eq!(
                queries::mod_id_for_alias(c, "modrinth", "wTncj5gs")?,
                Some(survivor)
            );
            let mods: i64 = c.query_row("SELECT count(*) FROM mods", [], |r| r.get(0))?;
            assert_eq!(mods, 1, "the split collapsed to one mod");
            let files: i64 = c.query_row(
                "SELECT count(*) FROM mod_version WHERE mod_id = ?1",
                params![survivor],
                |r| r.get(0),
            )?;
            assert_eq!(files, 2, "both artifacts under the survivor");
            let src: String = c.query_row(
                "SELECT source FROM mods WHERE id = ?1",
                params![survivor],
                |r| r.get(0),
            )?;
            assert_eq!(
                src, "harvested",
                "an automatic merge stays harvest-managed, not marked precious"
            );
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn selector_resolves_modid_case_insensitively() {
        // a dependency names the mod in display case (`JEI`) while its forge modid
        // is `jei`; the selector must still resolve, and only for modids -- a
        // Modrinth project id stays case-sensitive.
        let r = Registry::open_in_memory().unwrap();
        let jei = r
            .with_txn(|c| upsert::upsert_mod_by_alias(c, &[("modid", "jei")], "T0"))
            .unwrap();
        r.with_conn(|c| {
            assert_eq!(queries::mod_id_for_selector(c, "JEI")?, Some(jei));
            assert_eq!(queries::mod_id_for_selector(c, "jei")?, Some(jei));
            assert_eq!(
                queries::mod_id_for_selector(c, "JeI@[4.16,)")?,
                Some(jei),
                "the version window is still stripped before the lookup"
            );
            assert_eq!(
                queries::mod_id_for_selector(c, "modrinth:JEI")?,
                None,
                "a modrinth project id is not case-folded"
            );
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn upsert_auto_merge_keeps_a_precious_row() {
        // An operator-authored identity must never be deleted by the automatic
        // collision merge: it is the survivor, the harvested row folds into it.
        let r = Registry::open_in_memory().unwrap();
        let (harvested, authored) = r
            .with_txn(|c| {
                let a = upsert::upsert_mod_by_alias(c, &[("modid", "ic2")], "T0")?;
                upsert::upsert_mod_version(c, a, "2.8", &["forge"], "sha_a", 1, None, None, "T0")?;
                let b = upsert::upsert_mod_by_alias(c, &[("modrinth", "proj")], "T0")?;
                c.execute(
                    "UPDATE mods SET source = 'authored' WHERE id = ?1",
                    params![b],
                )?;
                Ok((a, b))
            })
            .unwrap();

        let survivor = r
            .with_txn(|c| {
                upsert::upsert_mod_by_alias(c, &[("modid", "ic2"), ("modrinth", "proj")], "T1")
            })
            .unwrap();
        assert_eq!(survivor, authored, "the precious row survives");

        r.with_conn(|c| {
            assert!(
                c.query_row(
                    "SELECT 1 FROM mods WHERE id = ?1",
                    params![harvested],
                    |_| Ok(())
                )
                .optional()?
                .is_none(),
                "the harvested row was folded away"
            );
            assert_eq!(
                queries::mod_id_for_alias(c, "modid", "ic2")?,
                Some(authored)
            );
            let src: String = c.query_row(
                "SELECT source FROM mods WHERE id = ?1",
                params![authored],
                |r| r.get(0),
            )?;
            assert_eq!(src, "authored", "the survivor keeps its precious source");
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn author_file_identity_creates_grouped_authored_and_survives_reharvest() {
        use super::{FileIdentity, ModRef, author_file_identity};
        let r = Registry::open_in_memory().unwrap();

        // label a loose cached jar: brand-new mod, a dev release, forge/1.7.10
        let mv_id = r
            .with_txn(|c| {
                author_file_identity(
                    c,
                    &FileIdentity {
                        sha1: "sha_new",
                        size_bytes: 123,
                        filename: Some("cool.jar"),
                        mod_ref: ModRef::New { name: "Cool Mod" },
                        version_number: "2.3",
                        channel: "alpha",
                        loaders: &["forge".to_string()],
                        mc_versions: &["1.7.10".to_string()],
                    },
                    "T0",
                )
            })
            .unwrap();

        r.with_conn(|c| {
            let (src, ver, rel): (String, String, i64) = c.query_row(
                "SELECT source, version, release_id FROM mod_version WHERE sha1 = 'sha_new'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
            assert_eq!(src, "authored");
            assert_eq!(ver, "2.3");
            let (vn, ch, msrc): (String, String, String) = c.query_row(
                "SELECT mr.version_number, mr.channel, m.source
                 FROM mod_release mr JOIN mods m ON m.id = mr.mod_id WHERE mr.id = ?1",
                [rel],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
            assert_eq!(
                (vn.as_str(), ch.as_str(), msrc.as_str()),
                ("2.3", "alpha", "authored")
            );
            let t: String = c.query_row(
                "SELECT target FROM mod_version_target WHERE mod_version_id = ?1",
                [mv_id],
                |r| r.get(0),
            )?;
            assert_eq!(t, "forge");
            Ok(())
        })
        .unwrap();

        // a re-harvest reporting a different mod/loader must not touch the file
        r.with_txn(|c| {
            let mid = queries::mod_id_for_sha1(c, "sha_new")?.unwrap();
            upsert::upsert_mod_version(
                c,
                mid,
                "HARVEST",
                &["fabric"],
                "sha_new",
                999,
                Some("x.jar"),
                None,
                "T1",
            )
        })
        .unwrap();
        r.with_conn(|c| {
            let ver: String = c.query_row(
                "SELECT version FROM mod_version WHERE sha1 = 'sha_new'",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(ver, "2.3", "authored file survives a re-harvest");
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn author_file_identity_reuses_release_and_can_relabel() {
        use super::{FileIdentity, ModRef, author_file_identity};
        let r = Registry::open_in_memory().unwrap();
        // two files under the same mod + version + channel share one release
        let mk = |sha: &'static str, mod_ref| FileIdentity {
            sha1: sha,
            size_bytes: 1,
            filename: None,
            mod_ref,
            version_number: "1.0",
            channel: "release",
            loaders: &[],
            mc_versions: &[],
        };
        let mid = r
            .with_txn(|c| {
                let a = author_file_identity(c, &mk("sha_a", ModRef::New { name: "M" }), "T0")?;
                let owner: i64 =
                    c.query_row("SELECT mod_id FROM mod_version WHERE id = ?1", [a], |r| {
                        r.get(0)
                    })?;
                author_file_identity(c, &mk("sha_b", ModRef::Existing(owner)), "T0")?;
                Ok(owner)
            })
            .unwrap();
        r.with_conn(|c| {
            let releases: i64 = c.query_row(
                "SELECT count(*) FROM mod_release WHERE mod_id = ?1",
                [mid],
                |r| r.get(0),
            )?;
            assert_eq!(releases, 1, "same (mod, version, channel) -> one release");
            // an empty loader set records 'any'
            let anyc: i64 = c.query_row(
                "SELECT count(*) FROM mod_version_target t JOIN mod_version mv ON mv.id = t.mod_version_id
                 WHERE mv.sha1 = 'sha_a' AND t.target = 'any'",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(anyc, 1);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn edit_release_and_rename_mod_are_authored() {
        use super::{FileIdentity, ModRef, author_file_identity, edit_release, rename_mod};
        let r = Registry::open_in_memory().unwrap();
        let (mid, rel) = r
            .with_txn(|c| {
                let mv = author_file_identity(
                    c,
                    &FileIdentity {
                        sha1: "sha_x",
                        size_bytes: 1,
                        filename: None,
                        mod_ref: ModRef::New { name: "X" },
                        version_number: "0.1",
                        channel: "unknown",
                        loaders: &[],
                        mc_versions: &[],
                    },
                    "T0",
                )?;
                let (mid, rel): (i64, i64) = c.query_row(
                    "SELECT mod_id, release_id FROM mod_version WHERE id = ?1",
                    [mv],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                Ok((mid, rel))
            })
            .unwrap();

        r.with_txn(|c| edit_release(c, rel, Some("0.2"), Some("beta"), "T1"))
            .unwrap();
        r.with_txn(|c| rename_mod(c, mid, Some("Xenon"), None, "T1"))
            .unwrap();
        r.with_conn(|c| {
            let (vn, ch): (String, String) = c.query_row(
                "SELECT version_number, channel FROM mod_release WHERE id = ?1",
                [rel],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            assert_eq!((vn.as_str(), ch.as_str()), ("0.2", "beta"));
            let (name, src): (String, String) = c.query_row(
                "SELECT canonical_name, source FROM mods WHERE id = ?1",
                [mid],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            assert_eq!((name.as_str(), src.as_str()), ("Xenon", "authored"));
            Ok(())
        })
        .unwrap();

        // an invalid channel is rejected
        assert!(
            r.with_txn(|c| edit_release(c, rel, None, Some("nightly"), "T2"))
                .is_err()
        );
    }

    #[test]
    fn backup_writes_a_readable_copy() {
        let dir = tempfile::tempdir().unwrap();
        let r = Registry::open(dir.path().join("registry.db")).unwrap();
        r.with_txn(|c| upsert::upsert_mod_by_alias(c, &[("modid", "x")], "T0").map(|_| ()))
            .unwrap();
        let backup = dir.path().join("backup.db");
        r.backup_into(&backup).unwrap();

        let restored = Registry::open(&backup).unwrap();
        let n: i64 = restored
            .with_conn(|c| Ok(c.query_row("SELECT count(*) FROM mods", [], |r| r.get(0))?))
            .unwrap();
        assert_eq!(n, 1);
    }
}
