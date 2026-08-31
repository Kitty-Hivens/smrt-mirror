//! Embedded SQLite handle for the mod registry. rusqlite is synchronous, so the
//! connection sits behind a `std::sync::Mutex` and every caller runs its closure
//! inside `tokio::task::spawn_blocking` (same idiom as the unzip/build paths).

use anyhow::{Context, Result};
use rusqlite::{Connection, params};
use std::path::Path;
use std::sync::{Arc, Mutex};

use super::model::ModrinthProjectName;
use super::{authored, migrations, queries, upsert};

pub struct Registry {
    conn: Mutex<Connection>,
}

impl Registry {
    /// Open (creating if absent) the registry DB at `path`, set pragmas, and
    /// apply pending migrations. Synchronous; called once at startup.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let conn = Connection::open(path)
            .with_context(|| format!("opening registry db at {}", path.display()))?;
        Self::init(conn)
    }

    /// In-memory registry, for tests.
    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory().context("opening in-memory registry")?)
    }

    fn init(mut conn: Connection) -> Result<Self> {
        // WAL lets the readers proceed alongside the single harvest writer; the
        // busy_timeout absorbs the brief write lock. (WAL is a no-op on :memory:.)
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA foreign_keys = ON;
             PRAGMA busy_timeout = 5000;",
        )
        .context("setting registry pragmas")?;
        migrations::apply_pending(&mut conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Run a read against the connection. Call inside `spawn_blocking`.
    ///
    /// From an `async fn`, call [`read`](Self::read) instead -- it is this with
    /// the hop the rule above asks for.
    pub fn with_conn<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let guard = self.conn.lock().expect("registry mutex poisoned");
        f(&guard)
    }

    /// Run a read off the async runtime -- the door for an `async fn`.
    ///
    /// The rule at the top of this module is not decoration. The connection
    /// sits behind a plain mutex that the harvest holds for the whole of its
    /// write transaction, so a read called straight from async code does not
    /// merely do file I/O on a runtime worker: it can park that worker until
    /// the harvest is done. A mirror has as many workers as cores, and the
    /// paths that read the registry from async code -- saving a config,
    /// previewing dependencies, searching for a mod -- are the ones a person
    /// triggers repeatedly while typing.
    pub async fn read<T, F>(self: &Arc<Self>, f: F) -> Result<T>
    where
        F: FnOnce(&Connection) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let me = self.clone();
        tokio::task::spawn_blocking(move || me.with_conn(f))
            .await
            .context("registry read task")?
    }

    /// Run a write/transaction needing `&mut Connection`. Call inside
    /// `spawn_blocking`.
    pub fn with_conn_mut<T>(&self, f: impl FnOnce(&mut Connection) -> Result<T>) -> Result<T> {
        let mut guard = self.conn.lock().expect("registry mutex poisoned");
        f(&mut guard)
    }

    /// Run `f` inside a single transaction, committing on `Ok`. `f` receives a
    /// `&Connection` (a `&Transaction` deref-coerces), so the upsert helpers
    /// take part in one atomic write. Call inside `spawn_blocking`.
    pub fn with_txn<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let mut guard = self.conn.lock().expect("registry mutex poisoned");
        let tx = guard.transaction().context("begin registry txn")?;
        let out = f(&tx)?;
        tx.commit().context("commit registry txn")?;
        Ok(out)
    }

    // ── authored moderation API (the precious layer; CLI + admin HTTP) ──────

    /// Add or remove a mutual authored conflict between two mods (by modid).
    /// Both mods must already be in the registry (harvest first).
    pub fn set_conflict(&self, a_modid: &str, b_modid: &str, remove: bool) -> Result<()> {
        let now = upsert::now_rfc3339();
        self.with_txn(|c| {
            let a = queries::mod_id_for_alias(c, "modid", a_modid)?.ok_or_else(|| {
                anyhow::anyhow!("mod '{a_modid}' not in registry (harvest first)")
            })?;
            let b = queries::mod_id_for_alias(c, "modid", b_modid)?.ok_or_else(|| {
                anyhow::anyhow!("mod '{b_modid}' not in registry (harvest first)")
            })?;
            authored::set_authored_conflict(c, a, a_modid, b, b_modid, &now, remove)
        })
    }

    /// Assign a cached jar its mod + release + facets (the authoring door).
    /// Everything lands as `source='authored'`. Returns the `mod_version` id.
    pub fn author_file(&self, id: &authored::FileIdentity<'_>) -> Result<i64> {
        let now = upsert::now_rfc3339();
        self.with_txn(|c| authored::author_file_identity(c, id, &now))
    }

    /// Rename a mod (authored).
    pub fn rename_mod(&self, mod_id: i64, name: Option<&str>, slug: Option<&str>) -> Result<()> {
        let now = upsert::now_rfc3339();
        self.with_txn(|c| authored::rename_mod(c, mod_id, name, slug, &now))
    }

    /// Fill the Modrinth-name display cache (idempotent upsert). Pure display
    /// data behind the graph's external `modrinth:<id>` leaves -- moves no
    /// compatibility fact, safe to drop and refill.
    pub fn cache_modrinth_names(&self, entries: &[ModrinthProjectName]) -> Result<()> {
        if entries.is_empty() {
            return Ok(());
        }
        let now = upsert::now_rfc3339();
        self.with_txn(|c| {
            for e in entries {
                c.execute(
                    "INSERT INTO modrinth_project_name (project_id, title, slug, fetched_at)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(project_id) DO UPDATE SET
                         title = excluded.title, slug = excluded.slug, fetched_at = excluded.fetched_at",
                    params![e.id, e.title, e.slug, now],
                )?;
            }
            Ok(())
        })
    }

    /// Merge the `from` mod identity into `into` (authored). Repoints all of
    /// `from`'s aliases/releases/files/relations onto `into`, then drops `from`.
    pub fn merge_mods(&self, from_mod_id: i64, into_mod_id: i64) -> Result<()> {
        let now = upsert::now_rfc3339();
        self.with_txn(|c| authored::merge_mods(c, from_mod_id, into_mod_id, &now))
    }

    /// Author or remove a single graph edge (the node editor). Add writes an
    /// `authored` relation; remove drops only the authored row (a harvested fact
    /// reappears on the next harvest, so there is nothing to "un-assert").
    pub fn author_relation(
        &self,
        from_mod_id: i64,
        target_modid: &str,
        kind: super::model::RelKind,
        severity: Option<super::model::Severity>,
        remove: bool,
    ) -> Result<()> {
        let now = upsert::now_rfc3339();
        self.with_txn(|c| {
            if remove {
                authored::remove_authored_relation(c, from_mod_id, target_modid, kind)?;
            } else {
                authored::add_authored_relation(
                    c,
                    from_mod_id,
                    target_modid,
                    kind,
                    severity,
                    &now,
                )?;
            }
            Ok(())
        })
    }

    /// Set (or clear) an operator-asserted jar classification (authored;
    /// refused for Modrinth-identified mods -- their environment flags stay
    /// authoritative).
    pub fn author_jar_class(
        &self,
        sha1: &str,
        kind: &str,
        side: Option<&str>,
        match_policy: Option<&str>,
        remove: bool,
    ) -> Result<()> {
        self.with_txn(|c| {
            authored::set_authored_jar_class(c, sha1, kind, side, match_policy, remove)
        })
    }

    /// Edit a release's version number and/or channel (authored).
    pub fn edit_release(
        &self,
        release_id: i64,
        version_number: Option<&str>,
        channel: Option<&str>,
    ) -> Result<()> {
        let now = upsert::now_rfc3339();
        self.with_txn(|c| authored::edit_release(c, release_id, version_number, channel, &now))
    }

    /// Snapshot the whole DB to `dest` via `VACUUM INTO` (a single-file backup
    /// of the precious authored rows). Blocking.
    pub fn backup_into(&self, dest: &Path) -> Result<()> {
        let guard = self.conn.lock().expect("registry mutex poisoned");
        guard
            .execute("VACUUM INTO ?1", params![dest.to_string_lossy().as_ref()])
            .with_context(|| format!("VACUUM INTO {}", dest.display()))?;
        Ok(())
    }
}
