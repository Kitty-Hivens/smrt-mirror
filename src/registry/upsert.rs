//! Idempotent writers. Every write is keyed by a natural key and guards the
//! precious layer: harvested rows refresh, but `curator`/`authored` rows are
//! never clobbered (the `WHERE source NOT IN (...)` on each upsert). Call inside
//! `Registry::with_conn_mut` (a write transaction).

use super::authored;
use super::model::{RelKind, Severity, Source};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};

pub fn now_rfc3339() -> String {
    use time::OffsetDateTime;
    use time::format_description::well_known::Rfc3339;
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

/// Resolve a logical mod from any of its external keys, creating it if none
/// match, and attach any missing aliases. This is how a jar carrying both a
/// `modid` and a Modrinth `project_id` collapses to one identity, and how two
/// jars of the same mod share it. When the supplied aliases point at *different*
/// existing mods, those rows are one mod split across identities (a Modrinth
/// re-upload known by project id, the same mod known by its forge modid), so they
/// are folded into one -- see [`merge_collided_mods`] for the precious-row
/// safeguards.
pub fn upsert_mod_by_alias(conn: &Connection, aliases: &[(&str, &str)], now: &str) -> Result<i64> {
    // every distinct existing mod any of the aliases already points at
    let mut matched: Vec<i64> = Vec::new();
    for (src, key) in aliases {
        if let Some(id) = conn
            .query_row(
                "SELECT mod_id FROM mod_alias WHERE source = ?1 AND external_key = ?2",
                params![src, key],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            && !matched.contains(&id)
        {
            matched.push(id);
        }
    }
    let mod_id = if matched.is_empty() {
        conn.execute(
            "INSERT INTO mods (source, confidence, created_at, updated_at)
             VALUES ('harvested', 10, ?1, ?1)",
            params![now],
        )?;
        conn.last_insert_rowid()
    } else if matched.len() == 1 {
        matched[0]
    } else {
        // One artifact's aliases point at several mod rows: they are one mod split
        // across identities -- a Modrinth re-upload known by its project id, the
        // same mod known by its forge modid. Fold them into one so a modid-keyed
        // dependency and a project-keyed placement resolve to the same mod.
        merge_collided_mods(conn, &matched, now)?
    };
    for (src, key) in aliases {
        conn.execute(
            "INSERT INTO mod_alias (mod_id, source, external_key) VALUES (?1, ?2, ?3)
             ON CONFLICT(source, external_key) DO NOTHING",
            params![mod_id, src, key],
        )?;
    }
    Ok(mod_id)
}

/// Fold the collided identities in `matched` into one survivor and return it. An
/// operator-authored/curator row is never deleted by this automatic merge: the
/// survivor is a precious row when one is present, and when two or more precious
/// rows collide the split is left for the operator to resolve rather than guessed.
/// Otherwise the lowest id survives, for a stable outcome across harvests.
pub(crate) fn merge_collided_mods(conn: &Connection, matched: &[i64], now: &str) -> Result<i64> {
    let is_precious = |id: i64| -> Result<bool> {
        let source: String =
            conn.query_row("SELECT source FROM mods WHERE id = ?1", params![id], |r| {
                r.get(0)
            })?;
        Ok(matches!(source.as_str(), "authored" | "curator"))
    };
    let mut precious: Vec<i64> = Vec::new();
    for &id in matched {
        if is_precious(id)? {
            precious.push(id);
        }
    }
    if precious.len() >= 2 {
        return Ok(*precious.iter().min().unwrap());
    }
    let canonical = precious
        .first()
        .copied()
        .unwrap_or_else(|| *matched.iter().min().unwrap());
    for &other in matched {
        if other != canonical {
            authored::fold_mods(conn, other, canonical, now)?;
        }
    }
    Ok(canonical)
}

/// Fill a mod's human metadata (display name, Modrinth slug, author) from a
/// harvest. `COALESCE(new, existing)` per column: a jar that carries a value
/// fills a gap, a jar that lacks one never erases a value an earlier jar set.
/// Skipped for precious (`curator`/`authored`) rows. Idempotent.
pub fn set_mod_meta(
    conn: &Connection,
    mod_id: i64,
    name: Option<&str>,
    slug: Option<&str>,
    author: Option<&str>,
    now: &str,
) -> Result<()> {
    if name.is_none() && slug.is_none() && author.is_none() {
        return Ok(());
    }
    conn.execute(
        "UPDATE mods SET
           canonical_name = COALESCE(?2, canonical_name),
           slug           = COALESCE(?3, slug),
           author         = COALESCE(?4, author),
           updated_at     = ?5
         WHERE id = ?1 AND source NOT IN ('curator', 'authored')",
        params![mod_id, name, slug, author, now],
    )?;
    Ok(())
}

/// Record a Modrinth project's declared environment flags on its mod. The
/// modrinth layer is rebuildable, so a fresh value overwrites a stale one --
/// but `COALESCE(new, existing)` keeps a known flag when a harvest could not
/// reach Modrinth (both slots None short-circuits to a no-op anyway), and
/// precious rows are skipped like every other enrichment write.
pub fn set_mod_env_flags(
    conn: &Connection,
    mod_id: i64,
    client_env: Option<&str>,
    server_env: Option<&str>,
    now: &str,
) -> Result<()> {
    if client_env.is_none() && server_env.is_none() {
        return Ok(());
    }
    conn.execute(
        "UPDATE mods SET
           client_env = COALESCE(?2, client_env),
           server_env = COALESCE(?3, server_env),
           updated_at = ?4
         WHERE id = ?1 AND source NOT IN ('curator', 'authored')",
        params![mod_id, client_env, server_env, now],
    )?;
    Ok(())
}

/// Find-or-create the release (version node) grouping a mod's files that share
/// `(version_number, channel)`. Harvested releases default to channel 'unknown';
/// the authored layer sets a real channel. Idempotent -- a re-harvest reuses the
/// existing release rather than duplicating it.
pub fn upsert_release(
    conn: &Connection,
    mod_id: i64,
    version_number: &str,
    channel: &str,
    now: &str,
) -> Result<i64> {
    if let Some(id) = conn
        .query_row(
            "SELECT id FROM mod_release WHERE mod_id = ?1 AND version_number = ?2 AND channel = ?3",
            params![mod_id, version_number, channel],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
    {
        return Ok(id);
    }
    conn.execute(
        "INSERT INTO mod_release
           (mod_id, version_number, channel, source, confidence, created_at, updated_at)
         VALUES (?1, ?2, ?3, 'harvested', 10, ?4, ?4)",
        params![mod_id, version_number, channel, now],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Upsert an artifact keyed by its content hash, and (re)record its
/// compatibility `targets` (loader ids, or `any` when the set is empty).
/// Returns the `mod_version` id. A precious (`curator`/`authored`) row with this
/// sha1 is left untouched -- its columns AND its hand-set targets both survive a
/// re-harvest.
#[allow(clippy::too_many_arguments)]
pub fn upsert_mod_version(
    conn: &Connection,
    mod_id: i64,
    version: &str,
    targets: &[&str],
    sha1: &str,
    size_bytes: i64,
    filename: Option<&str>,
    mc_versions: Option<&str>,
    now: &str,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO mod_version
           (mod_id, version, sha1, size_bytes, filename, mc_versions,
            source, confidence, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'harvested', 10, ?7, ?7)
         ON CONFLICT(sha1) DO UPDATE SET
           mod_id = excluded.mod_id,
           version = excluded.version,
           size_bytes = excluded.size_bytes,
           filename = excluded.filename,
           mc_versions = excluded.mc_versions,
           updated_at = excluded.updated_at
         WHERE mod_version.source NOT IN ('curator', 'authored')",
        params![
            mod_id,
            version,
            sha1,
            size_bytes,
            filename,
            mc_versions,
            now
        ],
    )?;
    let (id, precious): (i64, bool) = conn.query_row(
        "SELECT id, source IN ('curator', 'authored') FROM mod_version WHERE sha1 = ?1",
        params![sha1],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    set_mod_version_targets(conn, id, targets, precious)?;
    // group the file under its release. Harvested files only: an authored file's
    // release is set through the authored layer, so a re-harvest must not move it.
    if !precious {
        let release_id = upsert_release(conn, mod_id, version, "unknown", now)?;
        conn.execute(
            "UPDATE mod_version SET release_id = ?2
             WHERE id = ?1 AND source NOT IN ('curator', 'authored')",
            params![id, release_id],
        )?;
    }
    Ok(id)
}

/// Re-point a harvested file to the release for its `(version, channel)`,
/// creating it if absent. No-op for a precious (authored) file, whose grouping is
/// operator-set. Call after [`upsert_mod_version`] (which sets a provisional
/// `unknown` release); harvest prunes any release this leaves empty.
pub fn set_harvested_release(
    conn: &Connection,
    sha1: &str,
    mod_id: i64,
    version: &str,
    channel: &str,
    now: &str,
) -> Result<()> {
    // absent row or a precious one -> leave it alone
    let precious: Option<bool> = conn
        .query_row(
            "SELECT source IN ('curator', 'authored') FROM mod_version WHERE sha1 = ?1",
            params![sha1],
            |r| r.get(0),
        )
        .optional()?;
    if precious != Some(false) {
        return Ok(());
    }
    let release_id = upsert_release(conn, mod_id, version, channel, now)?;
    conn.execute(
        "UPDATE mod_version SET release_id = ?2, updated_at = ?3 WHERE sha1 = ?1",
        params![sha1, release_id, now],
    )?;
    Ok(())
}

/// Delete harvested releases no file points at any more -- e.g. the provisional
/// `unknown` release [`upsert_mod_version`] creates before [`set_harvested_release`]
/// moves the file to its real (channel) release. Authored releases are preserved
/// even when empty (an operator may create one before attaching files).
pub fn prune_empty_releases(conn: &Connection) -> Result<usize> {
    Ok(conn.execute(
        "DELETE FROM mod_release
         WHERE source NOT IN ('curator', 'authored')
           AND id NOT IN (SELECT release_id FROM mod_version WHERE release_id IS NOT NULL)",
        [],
    )?)
}

/// Replace a mod_version's target set with `targets` (empty -> `any`). Skipped
/// for precious rows: an authored artifact's hand-set targets are not a
/// harvested fact and must not be reset. Idempotent for harvested rows.
fn set_mod_version_targets(
    conn: &Connection,
    mod_version_id: i64,
    targets: &[&str],
    precious: bool,
) -> Result<()> {
    if precious {
        return Ok(());
    }
    conn.execute(
        "DELETE FROM mod_version_target WHERE mod_version_id = ?1",
        params![mod_version_id],
    )?;
    let any = ["any"];
    let effective: &[&str] = if targets.is_empty() { &any } else { targets };
    for t in effective {
        conn.execute(
            "INSERT OR IGNORE INTO mod_version_target (mod_version_id, target) VALUES (?1, ?2)",
            params![mod_version_id, t],
        )?;
    }
    Ok(())
}

/// Record the Modrinth version id for an artifact (by sha1), so the panel can
/// re-add a build's Modrinth-sourced mod as a real Modrinth source rather than a
/// local-cache one. `COALESCE` so a later harvest that lost the id never erases
/// it; skipped for precious rows. No-op when there is no id.
pub fn set_mod_version_modrinth(
    conn: &Connection,
    sha1: &str,
    version_id: Option<&str>,
    now: &str,
) -> Result<()> {
    let Some(vid) = version_id else {
        return Ok(());
    };
    conn.execute(
        "UPDATE mod_version
           SET modrinth_version_id = COALESCE(?2, modrinth_version_id), updated_at = ?3
         WHERE sha1 = ?1 AND source NOT IN ('curator', 'authored')",
        params![sha1, vid, now],
    )?;
    Ok(())
}

/// Record the package prefixes a mod's jar defines, for the reference-derivation
/// index. Purely derived (harvest DELETEs the whole `mod_package` table first and
/// rebuilds), so there is no precious guard; `INSERT OR IGNORE` folds a prefix a
/// mod's several jars share.
pub fn set_mod_packages(conn: &Connection, mod_id: i64, prefixes: &[&str]) -> Result<()> {
    for p in prefixes {
        conn.execute(
            "INSERT OR IGNORE INTO mod_package (mod_id, prefix) VALUES (?1, ?2)",
            params![mod_id, p],
        )?;
    }
    Ok(())
}

/// Record a scanned jar's classification (kind + side + match policy), keyed
/// by content hash. Purely derived like `mod_package`: the harvest rewrites
/// the row each run, for every scanned jar whether or not it has an identity.
/// What one pass read out of a jar. Borrowed rather than owned: the caller
/// already holds every field, and this crosses one function boundary.
pub struct JarRead<'a> {
    pub modid: Option<&'a str>,
    pub name: Option<&'a str>,
    pub version: Option<&'a str>,
    pub loaders: &'a [String],
    pub mc: &'a [String],
    pub filename: Option<&'a str>,
}

/// Record what the harvest read out of a jar (#123). Written for every scanned
/// jar, identified or not: a jar with no mod row is exactly the one whose name
/// is otherwise lost, and it is the population this exists for.
pub fn set_jar_read(conn: &Connection, sha1: &str, read: &JarRead<'_>) -> Result<()> {
    let JarRead {
        modid,
        name,
        version,
        loaders,
        mc,
        filename,
    } = *read;
    let join = |v: &[String]| (!v.is_empty()).then(|| v.join(","));
    conn.execute(
        "INSERT INTO jar_read (sha1, modid, name, version, loaders, mc, filename)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(sha1) DO UPDATE SET
           modid    = COALESCE(excluded.modid, jar_read.modid),
           name     = COALESCE(excluded.name, jar_read.name),
           version  = COALESCE(excluded.version, jar_read.version),
           loaders  = COALESCE(excluded.loaders, jar_read.loaders),
           mc       = COALESCE(excluded.mc, jar_read.mc),
           filename = COALESCE(excluded.filename, jar_read.filename)",
        rusqlite::params![
            sha1,
            modid,
            name,
            version,
            join(loaders),
            join(mc),
            filename
        ],
    )?;
    Ok(())
}

pub fn set_jar_class(
    conn: &Connection,
    sha1: &str,
    kind: &str,
    side: Option<&str>,
    match_policy: Option<&str>,
    side_confidence: Option<&str>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO jar_class (sha1, kind, side, match_policy, side_confidence)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(sha1) DO UPDATE SET
           kind = excluded.kind,
           side = excluded.side,
           match_policy = excluded.match_policy,
           side_confidence = excluded.side_confidence
         WHERE jar_class.source != 'authored'",
        params![sha1, kind, side, match_policy, side_confidence],
    )?;
    Ok(())
}

pub fn mod_version_id_for_sha1(conn: &Connection, sha1: &str) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT id FROM mod_version WHERE sha1 = ?1",
            params![sha1],
            |r| r.get(0),
        )
        .optional()?)
}

/// Insert a sourced assertion; de-duped by the (from, target, kind, source,
/// range) unique index, so a re-harvest adds nothing. Returns whether a new row
/// was actually inserted (false when the dedupe index ignored it), so a caller
/// can count edges without over-counting duplicates across a mod's several jars.
/// Record a relation. `from_mod_version_id` scopes the edge to the artifact it was
/// derived from; `None` states it about the mod as a whole, which is what an
/// authored fact means (#48). A derived edge should always name its artifact -- the
/// jar is what actually declared the thing.
#[allow(clippy::too_many_arguments)]
pub fn upsert_relation(
    conn: &Connection,
    from_mod_id: i64,
    from_mod_version_id: Option<i64>,
    target_modid: &str,
    version_range: Option<&str>,
    kind: RelKind,
    severity: Option<Severity>,
    source: Source,
    now: &str,
) -> Result<bool> {
    upsert_relation_ranked(
        conn,
        from_mod_id,
        from_mod_version_id,
        target_modid,
        version_range,
        kind,
        severity,
        source,
        source.rank(),
        now,
    )
}

/// [`upsert_relation`] with an explicit confidence rank, for the one split the
/// source vocab does not encode: a modern loader manifest's declared deps
/// (loader-enforced, reliable) rank above 1.12-era mcmod.info ones, while both
/// stay `jar-meta` on the wire.
#[allow(clippy::too_many_arguments)]
pub fn upsert_relation_ranked(
    conn: &Connection,
    from_mod_id: i64,
    from_mod_version_id: Option<i64>,
    target_modid: &str,
    version_range: Option<&str>,
    kind: RelKind,
    severity: Option<Severity>,
    source: Source,
    rank: i64,
    now: &str,
) -> Result<bool> {
    // The severity rule lives here, once, so no caller can write a row the
    // schema would reject and none has to remember the convention. A conflict
    // whose intensity nobody stated is recorded hard: an incompatibility of
    // unknown strength is the one to surface, not the one to play down -- and
    // the reader defaults the same way (#129). Nothing else carries one.
    let severity = match kind {
        RelKind::Conflicts => Some(severity.unwrap_or(Severity::Hard).as_str()),
        _ => None,
    };
    let inserted = conn.execute(
        "INSERT OR IGNORE INTO relation
           (from_mod_id, from_mod_version_id, target_modid, target_version_range,
            kind, severity, source, confidence, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            from_mod_id,
            from_mod_version_id,
            target_modid,
            version_range,
            kind.as_str(),
            severity,
            source.as_str(),
            rank,
            now
        ],
    )?;
    Ok(inserted > 0)
}

/// Hash a class's binary name into the digest's 64-bit space.
///
/// FNV-1a: stable across builds and platforms, which a stored digest needs, and
/// not `DefaultHasher`, whose value is explicitly not guaranteed between
/// releases -- a digest written by one build and read by the next has to mean
/// the same thing.
pub fn class_hash(binary_name: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in binary_name.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Store what an artifact provides, as the sorted hashes of its class names.
/// Replaces whatever was there: purely derived, like the package index.
pub fn set_class_digest(
    conn: &Connection,
    mod_version_id: i64,
    class_names: &[String],
) -> Result<()> {
    let mut hashes: Vec<u64> = class_names.iter().map(|n| class_hash(n)).collect();
    hashes.sort_unstable();
    hashes.dedup();
    let mut blob = Vec::with_capacity(hashes.len() * 8);
    for h in hashes {
        blob.extend_from_slice(&h.to_be_bytes());
    }
    conn.execute(
        "UPDATE mod_version SET class_digest = ?1 WHERE id = ?2",
        params![blob, mod_version_id],
    )?;
    Ok(())
}

/// Store what an artifact's required mixins must resolve. Replaces the
/// artifact's rows wholesale -- a re-harvest of the same jar states the same
/// set, and a config that stopped being required must stop being recorded.
pub fn set_mixin_needs(
    conn: &Connection,
    mod_version_id: i64,
    needs: &[(String, String)],
) -> Result<()> {
    conn.execute(
        "DELETE FROM mixin_need WHERE mod_version_id = ?1",
        params![mod_version_id],
    )?;
    for (config, needed) in needs {
        conn.execute(
            "INSERT OR IGNORE INTO mixin_need (mod_version_id, config, needed)
             VALUES (?1, ?2, ?3)",
            params![mod_version_id, config, needed],
        )?;
    }
    Ok(())
}

/// Store the loader windows an artifact declares, keyed by whichever identity
/// the mirror has for it (a sha1, or `modrinth:<version_id>` for a pin whose
/// bytes it never held). Replaces the artifact's rows: the jar is immutable, so
/// a second read states the same thing, and a corrected parse must win.
///
/// An empty list is recorded as one row with no loader -- the difference
/// between "read it, it declares nothing" and "never read" is what keeps a
/// silent jar from being fetched again on every check.
pub fn set_artifact_loader_reqs(
    conn: &Connection,
    artifact_key: &str,
    reqs: &[(String, String)],
    now: &str,
) -> Result<()> {
    conn.execute(
        "DELETE FROM artifact_loader_req WHERE artifact_key = ?1",
        params![artifact_key],
    )?;
    if reqs.is_empty() {
        conn.execute(
            "INSERT INTO artifact_loader_req (artifact_key, loader, version_range, read_at)
             VALUES (?1, '', NULL, ?2)",
            params![artifact_key, now],
        )?;
        return Ok(());
    }
    for (loader, range) in reqs {
        conn.execute(
            "INSERT OR REPLACE INTO artifact_loader_req
                (artifact_key, loader, version_range, read_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![artifact_key, loader.to_ascii_lowercase(), range, now],
        )?;
    }
    Ok(())
}

pub fn upsert_pack(conn: &Connection, pack_id: &str, now: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO pack (id, created_at, updated_at)
         VALUES (?1, ?2, ?2)
         ON CONFLICT(id) DO UPDATE SET updated_at = excluded.updated_at",
        params![pack_id, now],
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn upsert_pack_build(
    conn: &Connection,
    pack_id: &str,
    pack_version: &str,
    mc_version: &str,
    loader_id: Option<&str>,
    loader_version: Option<&str>,
    java_major: Option<i64>,
    fingerprint: Option<&str>,
    is_latest: bool,
    now: &str,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO pack_build
           (pack_id, pack_version, mc_version, loader_id, loader_version, java_major,
            fingerprint, is_latest, source, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'harvested', ?9)
         ON CONFLICT(pack_id, pack_version) DO UPDATE SET
           mc_version = excluded.mc_version,
           loader_id = excluded.loader_id,
           loader_version = excluded.loader_version,
           java_major = excluded.java_major,
           fingerprint = excluded.fingerprint,
           is_latest = excluded.is_latest
         WHERE pack_build.source NOT IN ('curator', 'authored')",
        params![
            pack_id,
            pack_version,
            mc_version,
            loader_id,
            loader_version,
            java_major,
            fingerprint,
            is_latest,
            now
        ],
    )?;
    Ok(conn.query_row(
        "SELECT id FROM pack_build WHERE pack_id = ?1 AND pack_version = ?2",
        params![pack_id, pack_version],
        |r| r.get(0),
    )?)
}

pub fn link_build_mod(
    conn: &Connection,
    build_id: i64,
    mod_version_id: i64,
    filename: &str,
    required: bool,
    default_enabled: bool,
) -> Result<()> {
    conn.execute(
        "INSERT INTO pack_build_mod
           (build_id, mod_version_id, filename, required, default_enabled, source)
         VALUES (?1, ?2, ?3, ?4, ?5, 'harvested')
         ON CONFLICT(build_id, mod_version_id) DO UPDATE SET
           filename = excluded.filename,
           required = excluded.required,
           default_enabled = excluded.default_enabled",
        params![
            build_id,
            mod_version_id,
            filename,
            required,
            default_enabled
        ],
    )?;
    Ok(())
}

/// Record what CurseForge said about a jar, including when it said nothing.
///
/// A row without a `project_id` is the useful negative: asked, matched nowhere.
/// Writing it is the only way a later run can tell that from never having
/// asked, and it is what keeps the question from being put again on every
/// harvest for the same few hundred jars.
///
/// A match is final, because a jar's bytes do not change. A miss is not:
/// CurseForge builds its fingerprint index lazily and a file can be published
/// after the fact, so `asked_at` moves on every write and the caller decides
/// how stale a miss has to be before it is worth asking again.
pub fn set_curseforge_file(
    conn: &Connection,
    sha1: &str,
    fingerprint: u32,
    found: Option<&crate::authoring::curseforge::Match>,
    now: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO curseforge_file
             (sha1, fingerprint, project_id, file_id, display_name, file_name, download_url, asked_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(sha1) DO UPDATE SET
           fingerprint  = excluded.fingerprint,
           project_id   = excluded.project_id,
           file_id      = excluded.file_id,
           display_name = excluded.display_name,
           file_name    = excluded.file_name,
           download_url = excluded.download_url,
           asked_at     = excluded.asked_at",
        rusqlite::params![
            sha1,
            fingerprint,
            found.map(|m| m.project_id),
            found.map(|m| m.file_id),
            found.map(|m| m.display_name.as_str()),
            found.map(|m| m.file_name.as_str()),
            found.and_then(|m| m.download_url.as_deref()),
            now,
        ],
    )?;
    Ok(())
}
