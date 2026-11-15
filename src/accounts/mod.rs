//! Persistent accounts store (embedded SQLite): user identities from GitHub
//! OAuth and server-side sessions keyed to a user. The multi-user auth
//! foundation for the ladder in the multi-user issue -- a sign-in is a `users`
//! row and a session id maps to a user, not to a raw token. Same connection
//! idiom as the registry: a `Mutex<Connection>` run inside `spawn_blocking`.

use anyhow::{Context, Result};
use rand::RngCore;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use ts_rs::TS;
use utoipa::ToSchema;

const SCHEMA: &str = include_str!("schema.sql");
const SESSION_TTL: Duration = Duration::from_secs(86_400);
/// The `mod_uploads` columns `upload_from_row` reads, in its index order. One
/// source so the several upload SELECTs cannot drift from the row mapper.
const UPLOAD_COLS: &str = "id, uploader, pack_id, filename, sha1, size_bytes, status, note, created_at, \
     upstream_maintainer, decided_by";
/// Reserved uid for the synthetic machine-bearer admin (the `Bearer` token path
/// in the http layer). It is never persisted as a `users` row; the guards below
/// keep uid 0 unassignable so it can't collide with a real GitHub account.
const BREAK_GLASS_UID: i64 = 0;

/// The panel's authorization tiers, ordered low -> high: **declaration order is
/// the rank** (`Member < Admin < Debug`), so `role >= Role::Admin` is the admin
/// gate and `role >= Role::Debug` the debug gate. `Debug` is a rung ABOVE admin
/// (#39), not a flag: a guard on the compat-affecting authoring that would
/// otherwise let a casual operator corrupt the derivation graph. `member` is the
/// default on sign-in; `admin`/`debug` come from the operator allowlists.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Role {
    Member,
    Admin,
    Debug,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Member => "member",
            Role::Admin => "admin",
            Role::Debug => "debug",
        }
    }

    fn from_db(s: &str) -> Role {
        match s {
            "debug" => Role::Debug,
            "admin" => Role::Admin,
            _ => Role::Member,
        }
    }
}

/// Who is behind a request, resolved from the session's user row and attached to
/// the request by the auth middleware.
#[derive(Clone, Debug)]
pub struct Identity {
    /// GitHub numeric uid; 0 for the break-glass admin token.
    pub uid: i64,
    pub login: String,
    pub role: Role,
}

impl Identity {
    /// May this caller manage a resource owned by `owner_uid`? True for the owner
    /// themselves or for any admin-and-up role. The ownership gate for member-
    /// authored packs.
    pub fn owns_or_admin(&self, owner_uid: i64) -> bool {
        self.uid == owner_uid || self.role >= Role::Admin
    }
}

/// What somebody may do to one pack (ADR 0006). Declaration order is the rank
/// (`View < Edit < Own`), so a gate asks `level >= PackLevel::Edit` the way the
/// mirror-wide gate asks `role >= Role::Admin`.
///
/// The owner of a community namespace and an admin are never stored as a level:
/// they are what the gate knows before it reads. A stored level is only ever the
/// third answer -- somebody who is neither.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "bindings/")]
pub enum PackLevel {
    /// Read a draft, its history and its reports.
    View,
    /// Write the config, commit, build.
    Edit,
    /// Also grant and revoke access, change visibility, delete.
    Own,
}

impl PackLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            PackLevel::View => "view",
            PackLevel::Edit => "edit",
            PackLevel::Own => "own",
        }
    }

    pub fn parse(s: &str) -> Option<PackLevel> {
        match s {
            "view" => Some(PackLevel::View),
            "edit" => Some(PackLevel::Edit),
            "own" => Some(PackLevel::Own),
            _ => None,
        }
    }
}

/// One person's access to one pack, as the access list shows it.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct PackGrant {
    #[ts(type = "number")]
    pub github_uid: i64,
    /// The login as it was last seen signing in; absent for a uid granted access
    /// before its owner ever signed in here.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub login: Option<String>,
    pub level: PackLevel,
    #[ts(type = "number")]
    pub granted_by: i64,
    /// Who decided it, by name where the mirror knows it. A bare uid in a list
    /// of who is answerable for an access tells the reader nothing.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub granted_by_login: Option<String>,
    #[ts(type = "number")]
    pub granted_at: i64,
}

/// Somebody a pack's keepers have stopped from writing on it.
///
/// The reason is written for the person it names: a door that shuts with no
/// word is one they can only argue with, while a refusal that says why is one
/// they can answer or accept. It is shown to them when they try to write, and
/// to whoever moderates the pack.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct PackBlock {
    #[ts(type = "number")]
    pub github_uid: i64,
    /// Absent for a uid that has never signed in here.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub login: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reason: Option<String>,
    #[ts(type = "number")]
    pub blocked_by: i64,
    /// Who decided it, by name where the mirror knows it.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub blocked_by_login: Option<String>,
    #[ts(type = "number")]
    pub blocked_at: i64,
}

/// One thing that happened to somebody's discussion, as their own list shows it.
///
/// It carries the thread's own words rather than a copy made when the event
/// happened: a title edited afterwards must not leave a stale line in somebody's
/// list saying something the thread no longer says.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct Notification {
    #[ts(type = "number")]
    pub id: i64,
    /// `comment` | `opened` | `settled`.
    pub kind: String,
    #[ts(type = "number")]
    pub thread_id: i64,
    pub pack_id: String,
    pub title: String,
    /// The thread's standing now, which is what the reader is about to open.
    pub status: String,
    #[ts(type = "number")]
    pub actor_uid: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub actor_login: Option<String>,
    #[ts(type = "number")]
    pub created_at: i64,
    pub read: bool,
}

/// Something said about a pack: a report, a topic, or a fork offered back.
///
/// One shape for all three because they differ only in what opens them and how
/// they settle. A proposal names the state it offers (`source_pack` /
/// `source_commit`), which is a commit rather than "whatever that fork says
/// today", so what a reviewer reads cannot move while they read it. An issue
/// reports something wrong and a topic is anything else somebody wants to talk
/// about (a question, an idea, how a pack plays); both leave those empty.
#[derive(Debug, Clone, Serialize, TS, ToSchema)]
#[ts(export, export_to = "bindings/")]
pub struct Thread {
    #[ts(type = "number")]
    pub id: i64,
    pub pack_id: String,
    /// `issue` | `topic` | `proposal`.
    pub kind: String,
    pub title: String,
    pub body: String,
    #[ts(type = "number")]
    pub by_uid: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub by_login: Option<String>,
    /// An issue or a topic is `open` or `closed`; a proposal is `open`,
    /// `merged`, `declined` or `withdrawn`.
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub source_pack: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub source_commit: Option<String>,
    /// What a merge wrote into the target, so a settled proposal points at what
    /// it became and not only at what it asked for.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub merged_commit: Option<String>,
    #[ts(type = "number")]
    pub created_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub decided_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub decided_by: Option<i64>,
    /// How many comments are on it, so a list can say so without reading them.
    #[ts(type = "number")]
    pub comments: i64,
}

/// One thing said on a thread.
///
/// A moderated comment is hidden, not deleted: that something was said and
/// taken down is itself part of the record, and a hole in a discussion reads
/// worse than a marked gap. The body of a hidden comment never leaves the
/// mirror -- the reader is told there was one and who took it down.
#[derive(Debug, Clone, Serialize, TS, ToSchema)]
#[ts(export, export_to = "bindings/")]
pub struct ThreadComment {
    #[ts(type = "number")]
    pub id: i64,
    #[ts(type = "number")]
    pub by_uid: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub by_login: Option<String>,
    /// Absent when the comment is hidden.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub body: Option<String>,
    pub hidden: bool,
    #[ts(type = "number")]
    pub created_at: i64,
}

/// One thread and everything said on it.
#[derive(Debug, Clone, Serialize, TS, ToSchema)]
#[ts(export, export_to = "bindings/")]
pub struct ThreadView {
    pub thread: Thread,
    pub comments: Vec<ThreadComment>,
}

/// A registered user, for the operator's user-management view.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct UserRow {
    #[ts(type = "number")]
    pub github_uid: i64,
    pub login: String,
    pub role: String,
    #[ts(type = "number")]
    pub created_at: i64,
    #[ts(type = "number")]
    pub last_login_at: i64,
    /// Present while the account is stopped from putting anything on the mirror.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub suspension: Option<Suspension>,
}

/// An account stopped by the mirror's operators, and why.
///
/// The reason is written for the person it names: they read it when they try to
/// write and on their own page, which is what makes it something they can
/// answer rather than a door that shuts silently.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct Suspension {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reason: Option<String>,
    #[ts(type = "number")]
    pub by_uid: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub by_login: Option<String>,
    #[ts(type = "number")]
    pub at: i64,
}

/// A member jar upload in the moderation queue.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct UploadRow {
    #[ts(type = "number")]
    pub id: i64,
    #[ts(type = "number")]
    pub uploader: i64,
    pub pack_id: String,
    pub filename: String,
    pub sha1: String,
    #[ts(type = "number")]
    pub size_bytes: i64,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub note: Option<String>,
    #[ts(type = "number")]
    pub created_at: i64,
    /// Who the uploader named as the jar's upstream origin (archival provenance).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub upstream_maintainer: Option<String>,
    /// GitHub uid of the moderator who decided this upload; absent while pending.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub decided_by: Option<i64>,
}

/// One entry in the system-wide audit log: who did what, when.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct AuditRow {
    #[ts(type = "number")]
    pub id: i64,
    #[ts(type = "number")]
    pub actor_uid: i64,
    pub actor_login: String,
    pub action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub detail: Option<String>,
    #[ts(type = "number")]
    pub created_at: i64,
}

pub struct Accounts {
    conn: Mutex<Connection>,
}

impl Accounts {
    /// Open (creating if absent) the accounts DB at `path`, set pragmas, and
    /// apply the schema. Synchronous; called once at startup.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let conn = Connection::open(path)
            .with_context(|| format!("opening accounts db at {}", path.display()))?;
        Self::init(conn)
    }

    /// In-memory accounts store, for tests.
    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory().context("opening in-memory accounts")?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA foreign_keys = ON;
             PRAGMA busy_timeout = 5000;",
        )
        .context("setting accounts pragmas")?;
        conn.execute_batch(SCHEMA)
            .context("applying accounts schema")?;
        widen_role_check(&conn).context("widening users.role check")?;
        widen_thread_kinds(&conn).context("widening pack_threads.kind check")?;
        // provenance columns on a DB that predates them (#44)
        ensure_column(&conn, "mod_uploads", "upstream_maintainer", "TEXT")?;
        ensure_column(&conn, "mod_uploads", "decided_by", "INTEGER")?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Upsert the signed-in GitHub user and open a session for them, returning
    /// the opaque session id for the cookie. `forced_role` comes from the operator
    /// allowlists (admin or debug) and is authoritative -- it sets the role on the
    /// record and re-asserts it on every login. `None` seeds a plain member on
    /// first sight, then leaves the role alone so an operator's UI promotion
    /// sticks across the user's later logins. A returning user keeps their row;
    /// login and last-login refresh. Blocking; wrap in `spawn_blocking`.
    pub fn sign_in_github(
        &self,
        github_uid: i64,
        login: &str,
        forced_role: Option<Role>,
    ) -> Result<String> {
        let now = unix_now();
        let mut guard = self.conn.lock().expect("accounts mutex poisoned");
        let tx = guard.transaction().context("begin sign-in txn")?;
        match forced_role {
            Some(role) => tx.execute(
                "INSERT INTO users (github_uid, login, role, created_at, last_login_at)
                 VALUES (?1, ?2, ?4, ?3, ?3)
                 ON CONFLICT(github_uid) DO UPDATE SET
                   login = excluded.login, role = ?4, last_login_at = excluded.last_login_at",
                params![github_uid, login, now, role.as_str()],
            ),
            None => tx.execute(
                "INSERT INTO users (github_uid, login, role, created_at, last_login_at)
                 VALUES (?1, ?2, 'member', ?3, ?3)
                 ON CONFLICT(github_uid) DO UPDATE SET
                   login = excluded.login, last_login_at = excluded.last_login_at",
                params![github_uid, login, now],
            ),
        }
        .context("upsert user")?;
        let user_id: i64 = tx
            .query_row(
                "SELECT id FROM users WHERE github_uid = ?1",
                params![github_uid],
                |r| r.get(0),
            )
            .context("read user id")?;
        let sid = insert_session(&tx, user_id, now)?;
        tx.commit().context("commit sign-in")?;
        Ok(sid)
    }

    /// The identity behind a session id, if the session exists and has not
    /// expired. A lapsed session is deleted on read so the table self-prunes.
    /// Blocking; wrap in `spawn_blocking`.
    pub fn session_identity(&self, session_id: &str) -> Result<Option<Identity>> {
        let now = unix_now();
        let guard = self.conn.lock().expect("accounts mutex poisoned");
        let row = guard
            .query_row(
                "SELECT u.github_uid, u.login, u.role, s.expires_at
                 FROM sessions s JOIN users u ON u.id = s.user_id
                 WHERE s.id = ?1",
                params![session_id],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, i64>(3)?,
                    ))
                },
            )
            .optional()
            .context("read session")?;
        match row {
            Some((uid, login, role, expires_at)) if expires_at > now => Ok(Some(Identity {
                uid,
                login,
                role: Role::from_db(&role),
            })),
            Some(_) => {
                guard.execute("DELETE FROM sessions WHERE id = ?1", params![session_id])?;
                Ok(None)
            }
            None => Ok(None),
        }
    }

    /// What `github_uid` was granted on `pack_id`, or `None` when nothing was.
    ///
    /// Answers the stored third case only: the caller's gate decides ownership
    /// and the admin rung before asking. Blocking; wrap in `spawn_blocking`.
    pub fn pack_access_level(&self, pack_id: &str, github_uid: i64) -> Result<Option<PackLevel>> {
        let guard = self.conn.lock().expect("accounts mutex poisoned");
        let level: Option<String> = guard
            .query_row(
                "SELECT level FROM pack_access WHERE pack_id = ?1 AND github_uid = ?2",
                params![pack_id, github_uid],
                |r| r.get(0),
            )
            .optional()
            .context("read pack access")?;
        Ok(level.as_deref().and_then(PackLevel::parse))
    }

    /// Grant (or move) somebody's access to a pack. Re-granting overwrites the
    /// level and re-stamps who decided it and when, so the list always says who
    /// is answerable for the access as it stands rather than as it began.
    ///
    /// Refuses the reserved uid 0, which is the synthetic break-glass identity
    /// and never a person. Blocking; wrap in `spawn_blocking`.
    pub fn grant_pack_access(
        &self,
        pack_id: &str,
        github_uid: i64,
        level: PackLevel,
        granted_by: i64,
    ) -> Result<()> {
        if github_uid == BREAK_GLASS_UID {
            anyhow::bail!("uid 0 is reserved");
        }
        let guard = self.conn.lock().expect("accounts mutex poisoned");
        guard.execute(
            "INSERT INTO pack_access (pack_id, github_uid, level, granted_by, granted_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(pack_id, github_uid) DO UPDATE SET
                 level = excluded.level,
                 granted_by = excluded.granted_by,
                 granted_at = excluded.granted_at",
            params![pack_id, github_uid, level.as_str(), granted_by, unix_now()],
        )?;
        Ok(())
    }

    /// Take somebody's access away. `false` when they had none, so a caller can
    /// tell a revocation from a no-op. Blocking; wrap in `spawn_blocking`.
    pub fn revoke_pack_access(&self, pack_id: &str, github_uid: i64) -> Result<bool> {
        let guard = self.conn.lock().expect("accounts mutex poisoned");
        let gone = guard.execute(
            "DELETE FROM pack_access WHERE pack_id = ?1 AND github_uid = ?2",
            params![pack_id, github_uid],
        )?;
        Ok(gone > 0)
    }

    /// Everyone granted access to a pack, highest level first. The login is
    /// joined from `users` and is absent for a uid that has never signed in --
    /// access can be granted ahead of a first login, and the list says so rather
    /// than inventing a name. Blocking; wrap in `spawn_blocking`.
    pub fn list_pack_access(&self, pack_id: &str) -> Result<Vec<PackGrant>> {
        let guard = self.conn.lock().expect("accounts mutex poisoned");
        let mut stmt = guard.prepare(
            "SELECT a.github_uid, u.login, a.level, a.granted_by, g.login, a.granted_at
             FROM pack_access a
             LEFT JOIN users u ON u.github_uid = a.github_uid
             LEFT JOIN users g ON g.github_uid = a.granted_by
             WHERE a.pack_id = ?1
             ORDER BY CASE a.level WHEN 'own' THEN 0 WHEN 'edit' THEN 1 ELSE 2 END,
                      a.granted_at",
        )?;
        let rows = stmt
            .query_map(params![pack_id], |r| {
                Ok(PackGrant {
                    github_uid: r.get(0)?,
                    login: r.get(1)?,
                    level: r
                        .get::<_, String>(2)
                        .ok()
                        .as_deref()
                        .and_then(PackLevel::parse)
                        .unwrap_or(PackLevel::View),
                    granted_by: r.get(3)?,
                    granted_by_login: r.get(4)?,
                    granted_at: r.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Every pack somebody was granted access to, for their own listing.
    /// Blocking; wrap in `spawn_blocking`.
    pub fn packs_granted_to(&self, github_uid: i64) -> Result<Vec<(String, PackLevel)>> {
        let guard = self.conn.lock().expect("accounts mutex poisoned");
        let mut stmt = guard.prepare(
            "SELECT pack_id, level FROM pack_access WHERE github_uid = ?1 ORDER BY pack_id",
        )?;
        let rows = stmt
            .query_map(params![github_uid], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)
                        .ok()
                        .as_deref()
                        .and_then(PackLevel::parse)
                        .unwrap_or(PackLevel::View),
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Forget a deleted pack's access list, so a pack id minted again later does
    /// not inherit who could reach the one before it.
    pub fn forget_pack_access(&self, pack_id: &str) -> Result<()> {
        let guard = self.conn.lock().expect("accounts mutex poisoned");
        guard.execute(
            "DELETE FROM pack_access WHERE pack_id = ?1",
            params![pack_id],
        )?;
        Ok(())
    }

    /// Who somebody is by uid, for a decision about a third person rather than
    /// about the caller. `None` for a uid that has never signed in here -- which
    /// is a real case (access can be granted ahead of a first login), so the
    /// caller decides what an unknown person counts as. Blocking; wrap in
    /// `spawn_blocking`.
    pub fn identity_of(&self, github_uid: i64) -> Result<Option<Identity>> {
        let guard = self.conn.lock().expect("accounts mutex poisoned");
        let row: Option<(String, String)> = guard
            .query_row(
                "SELECT login, role FROM users WHERE github_uid = ?1",
                params![github_uid],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .context("read user by uid")?;
        Ok(row.map(|(login, role)| Identity {
            uid: github_uid,
            login,
            role: Role::from_db(&role),
        }))
    }

    /// Stop somebody writing on a pack, or re-stamp an existing block with a
    /// fresh reason and decider. Refuses the reserved break-glass uid, which is
    /// never a person. Blocking; wrap in `spawn_blocking`.
    pub fn block_from_pack(
        &self,
        pack_id: &str,
        github_uid: i64,
        reason: Option<&str>,
        blocked_by: i64,
    ) -> Result<()> {
        if github_uid == BREAK_GLASS_UID {
            anyhow::bail!("uid 0 is reserved");
        }
        let guard = self.conn.lock().expect("accounts mutex poisoned");
        guard.execute(
            "INSERT INTO pack_blocks (pack_id, github_uid, reason, blocked_by, blocked_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(pack_id, github_uid) DO UPDATE SET
                 reason = excluded.reason,
                 blocked_by = excluded.blocked_by,
                 blocked_at = excluded.blocked_at",
            params![pack_id, github_uid, reason, blocked_by, unix_now()],
        )?;
        Ok(())
    }

    /// Let somebody write here again. `false` when they were not blocked.
    pub fn unblock_from_pack(&self, pack_id: &str, github_uid: i64) -> Result<bool> {
        let guard = self.conn.lock().expect("accounts mutex poisoned");
        let gone = guard.execute(
            "DELETE FROM pack_blocks WHERE pack_id = ?1 AND github_uid = ?2",
            params![pack_id, github_uid],
        )?;
        Ok(gone > 0)
    }

    /// This person's block on this pack, with the reason it names, or `None`.
    /// A store that cannot be read blocks nobody -- the caller treats the error
    /// as its own.
    pub fn block_on(&self, pack_id: &str, github_uid: i64) -> Result<Option<PackBlock>> {
        let guard = self.conn.lock().expect("accounts mutex poisoned");
        guard
            .query_row(
                "SELECT b.github_uid, u.login, b.reason, b.blocked_by, d.login, b.blocked_at
                 FROM pack_blocks b
                 LEFT JOIN users u ON u.github_uid = b.github_uid
                 LEFT JOIN users d ON d.github_uid = b.blocked_by
                 WHERE b.pack_id = ?1 AND b.github_uid = ?2",
                params![pack_id, github_uid],
                |r| {
                    Ok(PackBlock {
                        github_uid: r.get(0)?,
                        login: r.get(1)?,
                        reason: r.get(2)?,
                        blocked_by: r.get(3)?,
                        blocked_by_login: r.get(4)?,
                        blocked_at: r.get(5)?,
                    })
                },
            )
            .optional()
            .context("read pack block")
    }

    /// Everyone a pack has blocked, newest first. Blocking; wrap in
    /// `spawn_blocking`.
    pub fn list_pack_blocks(&self, pack_id: &str) -> Result<Vec<PackBlock>> {
        let guard = self.conn.lock().expect("accounts mutex poisoned");
        let mut stmt = guard.prepare(
            "SELECT b.github_uid, u.login, b.reason, b.blocked_by, d.login, b.blocked_at
             FROM pack_blocks b
             LEFT JOIN users u ON u.github_uid = b.github_uid
             LEFT JOIN users d ON d.github_uid = b.blocked_by
             WHERE b.pack_id = ?1 ORDER BY b.blocked_at DESC",
        )?;
        let rows = stmt
            .query_map(params![pack_id], |r| {
                Ok(PackBlock {
                    github_uid: r.get(0)?,
                    login: r.get(1)?,
                    reason: r.get(2)?,
                    blocked_by: r.get(3)?,
                    blocked_by_login: r.get(4)?,
                    blocked_at: r.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Forget a deleted pack's blocks, for the same reason its access list is
    /// forgotten: a pack id minted again later starts with nobody's history.
    pub fn forget_pack_blocks(&self, pack_id: &str) -> Result<()> {
        let guard = self.conn.lock().expect("accounts mutex poisoned");
        guard.execute(
            "DELETE FROM pack_blocks WHERE pack_id = ?1",
            params![pack_id],
        )?;
        Ok(())
    }

    // ── being told ──────────────────────────────────────────────────────────

    /// Tell each of `uids` that something happened on a thread. The actor is
    /// never told about their own act, and a uid of 0 is the mirror's own hand
    /// rather than a person with a list to read. Blocking; wrap in
    /// `spawn_blocking`.
    pub fn notify(&self, uids: &[i64], kind: &str, thread_id: i64, actor_uid: i64) -> Result<()> {
        if !matches!(kind, "comment" | "opened" | "settled") {
            anyhow::bail!("unknown notification kind {kind:?}");
        }
        let mut told: Vec<i64> = uids
            .iter()
            .copied()
            .filter(|u| *u != actor_uid && *u != BREAK_GLASS_UID)
            .collect();
        told.sort_unstable();
        told.dedup();
        if told.is_empty() {
            return Ok(());
        }
        let guard = self.conn.lock().expect("accounts mutex poisoned");
        let now = unix_now();
        let mut stmt = guard.prepare(
            "INSERT INTO notifications (uid, kind, thread_id, actor_uid, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;
        for uid in told {
            stmt.execute(params![uid, kind, thread_id, actor_uid, now])?;
        }
        Ok(())
    }

    /// Stop an account from putting anything on the mirror, or re-stamp an
    /// existing suspension with a fresh reason and decider. Refuses the reserved
    /// break-glass uid. Blocking; wrap in `spawn_blocking`.
    pub fn suspend_account(&self, github_uid: i64, reason: Option<&str>, by: i64) -> Result<()> {
        if github_uid == BREAK_GLASS_UID {
            anyhow::bail!("uid 0 is reserved");
        }
        let guard = self.conn.lock().expect("accounts mutex poisoned");
        guard.execute(
            "INSERT INTO account_suspensions (github_uid, reason, by_uid, at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(github_uid) DO UPDATE SET
                 reason = excluded.reason,
                 by_uid = excluded.by_uid,
                 at = excluded.at",
            params![github_uid, reason, by, unix_now()],
        )?;
        Ok(())
    }

    /// Let an account back. `false` when it was not suspended.
    pub fn lift_suspension(&self, github_uid: i64) -> Result<bool> {
        let guard = self.conn.lock().expect("accounts mutex poisoned");
        let gone = guard.execute(
            "DELETE FROM account_suspensions WHERE github_uid = ?1",
            params![github_uid],
        )?;
        Ok(gone > 0)
    }

    /// This account's suspension, with the reason it names, or `None`.
    pub fn suspension_of(&self, github_uid: i64) -> Result<Option<Suspension>> {
        let guard = self.conn.lock().expect("accounts mutex poisoned");
        guard
            .query_row(
                "SELECT s.reason, s.by_uid, u.login, s.at
                 FROM account_suspensions s LEFT JOIN users u ON u.github_uid = s.by_uid
                 WHERE s.github_uid = ?1",
                params![github_uid],
                |r| {
                    Ok(Suspension {
                        reason: r.get(0)?,
                        by_uid: r.get(1)?,
                        by_login: r.get(2)?,
                        at: r.get(3)?,
                    })
                },
            )
            .optional()
            .context("read account suspension")
    }

    /// The mirror's operators, for the packs whose keepers are exactly that: an
    /// official pack has no namespace owner, so admin is who answers for it.
    pub fn operator_uids(&self) -> Result<Vec<i64>> {
        let guard = self.conn.lock().expect("accounts mutex poisoned");
        let mut stmt =
            guard.prepare("SELECT github_uid FROM users WHERE role IN ('admin', 'debug')")?;
        let rows = stmt
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Everyone who has said something on a thread, for telling them it moved.
    pub fn speakers_on(&self, thread_id: i64) -> Result<Vec<i64>> {
        let guard = self.conn.lock().expect("accounts mutex poisoned");
        let mut stmt =
            guard.prepare("SELECT DISTINCT by_uid FROM thread_comments WHERE thread_id = ?1")?;
        let rows = stmt
            .query_map(params![thread_id], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// The address key for somebody's own feed, minting one if they have none.
    /// Blocking; wrap in `spawn_blocking`.
    pub fn feed_key(&self, github_uid: i64) -> Result<String> {
        if github_uid == BREAK_GLASS_UID {
            anyhow::bail!("uid 0 is reserved");
        }
        let guard = self.conn.lock().expect("accounts mutex poisoned");
        let existing: Option<String> = guard
            .query_row(
                "SELECT key FROM feed_keys WHERE github_uid = ?1",
                params![github_uid],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(key) = existing {
            return Ok(key);
        }
        let key = random_token();
        guard.execute(
            "INSERT INTO feed_keys (github_uid, key, created_at) VALUES (?1, ?2, ?3)",
            params![github_uid, key, unix_now()],
        )?;
        Ok(key)
    }

    /// Mint a new key, retiring whatever was handed out before.
    pub fn rotate_feed_key(&self, github_uid: i64) -> Result<String> {
        {
            let guard = self.conn.lock().expect("accounts mutex poisoned");
            guard.execute(
                "DELETE FROM feed_keys WHERE github_uid = ?1",
                params![github_uid],
            )?;
        }
        self.feed_key(github_uid)
    }

    /// Whose feed a key names, or `None` for a key nobody holds.
    pub fn uid_for_feed_key(&self, key: &str) -> Result<Option<i64>> {
        let guard = self.conn.lock().expect("accounts mutex poisoned");
        guard
            .query_row(
                "SELECT github_uid FROM feed_keys WHERE key = ?1",
                params![key],
                |r| r.get(0),
            )
            .optional()
            .context("read feed key")
    }

    /// Forget what somebody was told about one pack. Called when their access to
    /// it is taken away: a notification carries the thread's title, read live, so
    /// a list left behind would keep showing a private discussion to somebody who
    /// may no longer read it.
    pub fn forget_notifications_about(&self, pack_id: &str, uid: i64) -> Result<()> {
        let guard = self.conn.lock().expect("accounts mutex poisoned");
        guard.execute(
            "DELETE FROM notifications WHERE uid = ?2 AND thread_id IN
                 (SELECT id FROM pack_threads WHERE pack_id = ?1)",
            params![pack_id, uid],
        )?;
        Ok(())
    }

    /// Somebody's list, newest first. `unread_only` is what a badge counts and
    /// what a reader usually wants; `after` is the last id of the previous page
    /// -- the id is the order here, a notification being only ever appended --
    /// and `limit` bounds a list that only grows.
    pub fn notifications_for(
        &self,
        uid: i64,
        unread_only: bool,
        after: Option<i64>,
        limit: Option<usize>,
    ) -> Result<Vec<Notification>> {
        let guard = self.conn.lock().expect("accounts mutex poisoned");
        let sql = format!(
            "SELECT n.id, n.kind, n.thread_id, t.pack_id, t.title, t.status,
                    n.actor_uid, u.login, n.created_at, n.read_at
             FROM notifications n
             JOIN pack_threads t ON t.id = n.thread_id
             LEFT JOIN users u ON u.github_uid = n.actor_uid
             WHERE n.uid = ?1{}{} ORDER BY n.id DESC{}",
            if unread_only {
                " AND n.read_at IS NULL"
            } else {
                ""
            },
            // newest first, so a page continues below the last id served
            if after.is_some() {
                " AND n.id < ?2"
            } else {
                ""
            },
            match limit {
                Some(n) => format!(" LIMIT {n}"),
                None => String::new(),
            }
        );
        let mut stmt = guard.prepare(&sql)?;
        let bound: Vec<Box<dyn rusqlite::ToSql>> = match after {
            Some(id) => vec![Box::new(uid), Box::new(id)],
            None => vec![Box::new(uid)],
        };
        let refs: Vec<&dyn rusqlite::ToSql> = bound.iter().map(|b| b.as_ref()).collect();
        let rows = stmt
            .query_map(refs.as_slice(), |r| {
                let read_at: Option<i64> = r.get(9)?;
                Ok(Notification {
                    id: r.get(0)?,
                    kind: r.get(1)?,
                    thread_id: r.get(2)?,
                    pack_id: r.get(3)?,
                    title: r.get(4)?,
                    status: r.get(5)?,
                    actor_uid: r.get(6)?,
                    actor_login: r.get(7)?,
                    created_at: r.get(8)?,
                    read: read_at.is_some(),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// How many somebody has not read yet.
    pub fn unread_count(&self, uid: i64) -> Result<i64> {
        let guard = self.conn.lock().expect("accounts mutex poisoned");
        Ok(guard.query_row(
            "SELECT COUNT(*) FROM notifications WHERE uid = ?1 AND read_at IS NULL",
            params![uid],
            |r| r.get(0),
        )?)
    }

    /// Mark one of somebody's notifications read, or all of them. Scoped to the
    /// owner: an id is not a capability, so marking somebody else's read is not
    /// something an id can do.
    pub fn mark_read(&self, uid: i64, id: Option<i64>) -> Result<()> {
        let guard = self.conn.lock().expect("accounts mutex poisoned");
        match id {
            Some(id) => guard.execute(
                "UPDATE notifications SET read_at = ?3
                 WHERE uid = ?1 AND id = ?2 AND read_at IS NULL",
                params![uid, id, unix_now()],
            )?,
            None => guard.execute(
                "UPDATE notifications SET read_at = ?2 WHERE uid = ?1 AND read_at IS NULL",
                params![uid, unix_now()],
            )?,
        };
        Ok(())
    }

    /// Open a thread on a pack and return its id. `source` is `None` for an
    /// issue or a topic and the offered `(pack, commit)` for a proposal.
    /// Blocking; wrap in `spawn_blocking`.
    pub fn open_thread(
        &self,
        pack_id: &str,
        kind: &str,
        title: &str,
        body: &str,
        by_uid: i64,
        source: Option<(&str, &str)>,
    ) -> Result<i64> {
        if !matches!(kind, "issue" | "topic" | "proposal") {
            anyhow::bail!("unknown thread kind {kind:?}");
        }
        let (source_pack, source_commit) = match source {
            Some((p, c)) => (Some(p), Some(c)),
            None => (None, None),
        };
        let guard = self.conn.lock().expect("accounts mutex poisoned");
        guard.execute(
            "INSERT INTO pack_threads
                 (pack_id, kind, title, body, by_uid, status, source_pack, source_commit, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 'open', ?6, ?7, ?8)",
            params![pack_id, kind, title, body, by_uid, source_pack, source_commit, unix_now()],
        )?;
        Ok(guard.last_insert_rowid())
    }

    /// One thread by id. Blocking; wrap in `spawn_blocking`.
    pub fn thread(&self, id: i64) -> Result<Option<Thread>> {
        let guard = self.conn.lock().expect("accounts mutex poisoned");
        guard
            .query_row(
                &format!(
                    "SELECT {THREAD_COLS} FROM pack_threads t \
                          LEFT JOIN users u ON u.github_uid = t.by_uid WHERE t.id = ?1"
                ),
                params![id],
                thread_from_row,
            )
            .optional()
            .context("read thread")
    }

    /// Threads on a pack, newest first. `kind` narrows to issues or proposals;
    /// `open_only` is what a reader wants by default. `after` is the
    /// `(created_at, id)` of the last row of the previous page and `limit` how
    /// many to read; both absent answers the whole list, as it did before it
    /// could be paged. Blocking.
    ///
    /// The order breaks ties by id rather than leaving two threads opened in the
    /// same second to the database's discretion: a cursor into an order that is
    /// not total can skip a row or serve it twice.
    pub fn threads_for(
        &self,
        pack_id: &str,
        kind: Option<&str>,
        open_only: bool,
        after: Option<(i64, i64)>,
        limit: Option<usize>,
    ) -> Result<Vec<Thread>> {
        let guard = self.conn.lock().expect("accounts mutex poisoned");
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(pack_id.to_string())];
        let mut where_more = String::new();
        if let Some(k) = kind {
            args.push(Box::new(k.to_string()));
            where_more.push_str(&format!(" AND t.kind = ?{}", args.len()));
        }
        if open_only {
            where_more.push_str(" AND t.status = 'open'");
        }
        if let Some((at, id)) = after {
            args.push(Box::new(at));
            args.push(Box::new(id));
            where_more.push_str(&format!(
                " AND (t.created_at < ?{} OR (t.created_at = ?{} AND t.id < ?{}))",
                args.len() - 1,
                args.len() - 1,
                args.len()
            ));
        }
        let sql = format!(
            "SELECT {THREAD_COLS} FROM pack_threads t \
             LEFT JOIN users u ON u.github_uid = t.by_uid \
             WHERE t.pack_id = ?1{where_more} ORDER BY t.created_at DESC, t.id DESC{}",
            match limit {
                Some(n) => format!(" LIMIT {n}"),
                None => String::new(),
            }
        );
        let mut stmt = guard.prepare(&sql)?;
        let bound: Vec<&dyn rusqlite::ToSql> = args.iter().map(|a| a.as_ref()).collect();
        let rows = stmt
            .query_map(bound.as_slice(), thread_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Threads somebody opened, newest first. Blocking.
    pub fn threads_by(&self, by_uid: i64) -> Result<Vec<Thread>> {
        let guard = self.conn.lock().expect("accounts mutex poisoned");
        let mut stmt = guard.prepare(&format!(
            "SELECT {THREAD_COLS} FROM pack_threads t \
             LEFT JOIN users u ON u.github_uid = t.by_uid \
             WHERE t.by_uid = ?1 ORDER BY t.created_at DESC"
        ))?;
        let rows = stmt
            .query_map(params![by_uid], thread_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Settle a thread. Only an open one settles, so two people deciding at once
    /// cannot both win: the second write matches nothing and answers `false`.
    pub fn settle_thread(
        &self,
        id: i64,
        status: &str,
        decided_by: i64,
        merged_commit: Option<&str>,
    ) -> Result<bool> {
        if !matches!(status, "closed" | "merged" | "declined" | "withdrawn") {
            anyhow::bail!("unknown thread status {status:?}");
        }
        let guard = self.conn.lock().expect("accounts mutex poisoned");
        let n = guard.execute(
            "UPDATE pack_threads
             SET status = ?2, decided_at = ?3, decided_by = ?4, merged_commit = ?5
             WHERE id = ?1 AND status = 'open'",
            params![id, status, unix_now(), decided_by, merged_commit],
        )?;
        Ok(n > 0)
    }

    /// Reopen a closed issue or topic. Proposals do not reopen: their offer was
    /// a commit, and offering again is a new proposal rather than a resurrected
    /// one.
    pub fn reopen_thread(&self, id: i64) -> Result<bool> {
        let guard = self.conn.lock().expect("accounts mutex poisoned");
        let n = guard.execute(
            "UPDATE pack_threads SET status = 'open', decided_at = NULL, decided_by = NULL
             WHERE id = ?1 AND kind IN ('issue', 'topic') AND status = 'closed'",
            params![id],
        )?;
        Ok(n > 0)
    }

    /// Say something on a thread. Blocking; wrap in `spawn_blocking`.
    pub fn comment(&self, thread_id: i64, by_uid: i64, body: &str) -> Result<i64> {
        let guard = self.conn.lock().expect("accounts mutex poisoned");
        guard.execute(
            "INSERT INTO thread_comments (thread_id, by_uid, body, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![thread_id, by_uid, body, unix_now()],
        )?;
        Ok(guard.last_insert_rowid())
    }

    /// How many of these somebody has written since `since`. The limit is
    /// counted from the rows themselves rather than from a table of counters:
    /// there is nothing to keep in sync, and a restart does not hand anyone a
    /// fresh allowance.
    pub fn recent_by(&self, table: &str, by_uid: i64, since: i64) -> Result<i64> {
        let sql = match table {
            "threads" => "SELECT COUNT(*) FROM pack_threads WHERE by_uid = ?1 AND created_at > ?2",
            "comments" => {
                "SELECT COUNT(*) FROM thread_comments WHERE by_uid = ?1 AND created_at > ?2"
            }
            other => anyhow::bail!("no rate window for {other}"),
        };
        let guard = self.conn.lock().expect("accounts mutex poisoned");
        Ok(guard.query_row(sql, params![by_uid, since], |r| r.get(0))?)
    }

    /// The unix second, for a caller composing a rate window.
    pub fn now(&self) -> i64 {
        unix_now()
    }

    /// A thread's discussion, oldest first. A hidden comment keeps its place and
    /// loses its body: the mirror never serves what a moderator took down.
    ///
    /// `after` is the last comment id of the previous page -- the id is the order
    /// here, since a comment is only ever appended -- and `limit` how many to
    /// read. Both absent answers the whole discussion.
    pub fn comments_on(
        &self,
        thread_id: i64,
        after: Option<i64>,
        limit: Option<usize>,
    ) -> Result<Vec<ThreadComment>> {
        let guard = self.conn.lock().expect("accounts mutex poisoned");
        let mut stmt = guard.prepare(&format!(
            "SELECT c.id, c.by_uid, u.login, c.body, c.hidden_at, c.created_at
             FROM thread_comments c LEFT JOIN users u ON u.github_uid = c.by_uid
             WHERE c.thread_id = ?1 AND c.id > ?2 ORDER BY c.id{}",
            match limit {
                Some(n) => format!(" LIMIT {n}"),
                None => String::new(),
            }
        ))?;
        let rows = stmt
            .query_map(params![thread_id, after.unwrap_or(0)], comment_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// One comment, as a reader would see it -- for the write that has just
    /// added it and wants to answer with the row rather than with the id.
    pub fn comment_by_id(&self, id: i64) -> Result<Option<ThreadComment>> {
        let guard = self.conn.lock().expect("accounts mutex poisoned");
        guard
            .query_row(
                "SELECT c.id, c.by_uid, u.login, c.body, c.hidden_at, c.created_at
                 FROM thread_comments c LEFT JOIN users u ON u.github_uid = c.by_uid
                 WHERE c.id = ?1",
                params![id],
                comment_from_row,
            )
            .optional()
            .context("read comment")
    }

    /// Take a comment down, or put it back. Hiding keeps the row so the gap in
    /// the discussion is marked rather than silent.
    pub fn set_comment_hidden(&self, id: i64, hidden: bool, by: i64) -> Result<bool> {
        let guard = self.conn.lock().expect("accounts mutex poisoned");
        let n = if hidden {
            guard.execute(
                "UPDATE thread_comments SET hidden_at = ?2, hidden_by = ?3 WHERE id = ?1",
                params![id, unix_now(), by],
            )?
        } else {
            guard.execute(
                "UPDATE thread_comments SET hidden_at = NULL, hidden_by = NULL WHERE id = ?1",
                params![id],
            )?
        };
        Ok(n > 0)
    }

    /// The thread a comment belongs to, for gating a moderation call.
    pub fn thread_of_comment(&self, comment_id: i64) -> Result<Option<i64>> {
        let guard = self.conn.lock().expect("accounts mutex poisoned");
        guard
            .query_row(
                "SELECT thread_id FROM thread_comments WHERE id = ?1",
                params![comment_id],
                |r| r.get(0),
            )
            .optional()
            .context("read comment thread")
    }

    /// Drop a deleted pack's threads, from either side of a proposal.
    pub fn forget_pack_threads(&self, pack_id: &str) -> Result<()> {
        let guard = self.conn.lock().expect("accounts mutex poisoned");
        guard.execute(
            "DELETE FROM pack_threads WHERE pack_id = ?1 OR source_pack = ?1",
            params![pack_id],
        )?;
        Ok(())
    }

    /// Drop a session (logout). Blocking; wrap in `spawn_blocking`.
    pub fn delete_session(&self, session_id: &str) -> Result<()> {
        let guard = self.conn.lock().expect("accounts mutex poisoned");
        guard.execute("DELETE FROM sessions WHERE id = ?1", params![session_id])?;
        Ok(())
    }

    /// Every registered user except the reserved uid 0, newest login first.
    /// Blocking; wrap in `spawn_blocking`.
    pub fn list_users(&self) -> Result<Vec<UserRow>> {
        let guard = self.conn.lock().expect("accounts mutex poisoned");
        let mut stmt = guard.prepare(
            "SELECT u.github_uid, u.login, u.role, u.created_at, u.last_login_at,
                    s.reason, s.by_uid, b.login, s.at
             FROM users u
             LEFT JOIN account_suspensions s ON s.github_uid = u.github_uid
             LEFT JOIN users b ON b.github_uid = s.by_uid
             WHERE u.github_uid != 0
             ORDER BY u.last_login_at DESC",
        )?;
        let rows = stmt
            .query_map([], |r| {
                let at: Option<i64> = r.get(8)?;
                Ok(UserRow {
                    github_uid: r.get(0)?,
                    login: r.get(1)?,
                    role: r.get(2)?,
                    created_at: r.get(3)?,
                    last_login_at: r.get(4)?,
                    suspension: match at {
                        Some(at) => Some(Suspension {
                            reason: r.get(5)?,
                            by_uid: r.get(6)?,
                            by_login: r.get(7)?,
                            at,
                        }),
                        None => None,
                    },
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Set a user's role by GitHub uid. Refuses the reserved uid 0 and an unknown
    /// role. An allowlisted uid re-promotes to admin on its next login
    /// regardless, so demoting one here only holds until they sign in again.
    /// Blocking; wrap in `spawn_blocking`.
    pub fn set_role(&self, github_uid: i64, role: &str) -> Result<()> {
        if github_uid == BREAK_GLASS_UID {
            anyhow::bail!("cannot change the reserved uid 0");
        }
        if role != "member" && role != "admin" && role != "debug" {
            anyhow::bail!("invalid role '{role}'");
        }
        let guard = self.conn.lock().expect("accounts mutex poisoned");
        let n = guard.execute(
            "UPDATE users SET role = ?2 WHERE github_uid = ?1",
            params![github_uid, role],
        )?;
        if n == 0 {
            anyhow::bail!("no user with uid {github_uid}");
        }
        Ok(())
    }

    // ── moderation queue (member jar uploads) ───────────────────────────────

    /// Enqueue a pending member upload; returns its id. `upstream_maintainer` is
    /// the uploader-named origin of the jar, kept for archival provenance.
    /// Blocking.
    pub fn enqueue_upload(
        &self,
        uploader: i64,
        pack_id: &str,
        filename: &str,
        sha1: &str,
        size_bytes: i64,
        upstream_maintainer: Option<&str>,
    ) -> Result<i64> {
        let now = unix_now();
        let guard = self.conn.lock().expect("accounts mutex poisoned");
        guard.execute(
            "INSERT INTO mod_uploads
               (uploader, pack_id, filename, sha1, size_bytes, status, created_at, upstream_maintainer)
             VALUES (?1, ?2, ?3, ?4, ?5, 'pending', ?6, ?7)",
            params![uploader, pack_id, filename, sha1, size_bytes, now, upstream_maintainer],
        )?;
        Ok(guard.last_insert_rowid())
    }

    /// Pending uploads, oldest first -- the operator's moderation queue. Blocking.
    pub fn list_pending_uploads(&self) -> Result<Vec<UploadRow>> {
        let guard = self.conn.lock().expect("accounts mutex poisoned");
        let mut stmt = guard.prepare(&format!(
            "SELECT {UPLOAD_COLS} FROM mod_uploads WHERE status = 'pending' ORDER BY created_at ASC"
        ))?;
        let rows = stmt
            .query_map([], upload_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// A member's own uploads (any status), newest first. Blocking.
    pub fn list_user_uploads(&self, uploader: i64) -> Result<Vec<UploadRow>> {
        let guard = self.conn.lock().expect("accounts mutex poisoned");
        let mut stmt = guard.prepare(&format!(
            "SELECT {UPLOAD_COLS} FROM mod_uploads WHERE uploader = ?1 ORDER BY created_at DESC"
        ))?;
        let rows = stmt
            .query_map(params![uploader], upload_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// One upload by id. Blocking.
    pub fn get_upload(&self, id: i64) -> Result<Option<UploadRow>> {
        let guard = self.conn.lock().expect("accounts mutex poisoned");
        guard
            .query_row(
                &format!("SELECT {UPLOAD_COLS} FROM mod_uploads WHERE id = ?1"),
                params![id],
                upload_from_row,
            )
            .optional()
            .context("read upload")
    }

    /// Decide a pending upload: `approved` or `rejected`, with an optional note
    /// and the deciding moderator's uid (kept for accountability). Blocking.
    pub fn set_upload_status(
        &self,
        id: i64,
        status: &str,
        note: Option<&str>,
        decided_by: Option<i64>,
    ) -> Result<()> {
        if status != "approved" && status != "rejected" {
            anyhow::bail!("invalid upload status '{status}'");
        }
        let now = unix_now();
        let guard = self.conn.lock().expect("accounts mutex poisoned");
        let n = guard.execute(
            "UPDATE mod_uploads
               SET status = ?2, note = ?3, decided_at = ?4, decided_by = ?5
             WHERE id = ?1",
            params![id, status, note, now, decided_by],
        )?;
        if n == 0 {
            anyhow::bail!("no upload with id {id}");
        }
        Ok(())
    }

    // ── rules-of-use acceptance ─────────────────────────────────────────────

    /// Record that a user has accepted the rules of use. Idempotent. Blocking.
    pub fn accept_terms(&self, uid: i64) -> Result<()> {
        let now = unix_now();
        let guard = self.conn.lock().expect("accounts mutex poisoned");
        guard.execute(
            "INSERT OR REPLACE INTO terms_acceptance (github_uid, accepted_at) VALUES (?1, ?2)",
            params![uid, now],
        )?;
        Ok(())
    }

    /// Whether a user has accepted the rules of use. Blocking.
    pub fn terms_accepted(&self, uid: i64) -> Result<bool> {
        let guard = self.conn.lock().expect("accounts mutex poisoned");
        let accepted = guard
            .query_row(
                "SELECT 1 FROM terms_acceptance WHERE github_uid = ?1",
                params![uid],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        Ok(accepted)
    }

    // ── audit log ────────────────────────────────────────────────────────────

    /// Append one entry to the system-wide audit log. Blocking. Callers treat a
    /// failure as non-fatal to the audited action -- the action already happened;
    /// a lost entry is logged, not raised.
    pub fn record_audit(
        &self,
        actor_uid: i64,
        actor_login: &str,
        action: &str,
        target: Option<&str>,
        detail: Option<&str>,
    ) -> Result<()> {
        let now = unix_now();
        let guard = self.conn.lock().expect("accounts mutex poisoned");
        guard.execute(
            "INSERT INTO audit_log (actor_uid, actor_login, action, target, detail, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![actor_uid, actor_login, action, target, detail, now],
        )?;
        Ok(())
    }

    /// Audit entries newest first, capped at `limit`, starting after `before`.
    ///
    /// The trail only grows, so reading further back is reading older ids: the
    /// id is the sort key and the cursor at once, and a page holds still even
    /// while new entries land on top of it. Blocking.
    pub fn list_audit(&self, limit: i64, before: Option<i64>) -> Result<Vec<AuditRow>> {
        let guard = self.conn.lock().expect("accounts mutex poisoned");
        let mut stmt = guard.prepare(
            "SELECT id, actor_uid, actor_login, action, target, detail, created_at
             FROM audit_log WHERE (?2 IS NULL OR id < ?2) ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt
            .query_map(params![limit, before], |r| {
                Ok(AuditRow {
                    id: r.get(0)?,
                    actor_uid: r.get(1)?,
                    actor_login: r.get(2)?,
                    action: r.get(3)?,
                    target: r.get(4)?,
                    detail: r.get(5)?,
                    created_at: r.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}

/// The `pack_threads` columns `thread_from_row` reads, in its index order, with
/// the opener's login and the comment count joined on. One source so the several
/// thread SELECTs cannot drift from the row mapper.
const THREAD_COLS: &str = "t.id, t.pack_id, t.kind, t.title, t.body, t.by_uid, u.login, \
     t.status, t.source_pack, t.source_commit, t.merged_commit, t.created_at, t.decided_at, \
     t.decided_by, (SELECT COUNT(*) FROM thread_comments c WHERE c.thread_id = t.id)";

fn comment_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ThreadComment> {
    let hidden: Option<i64> = r.get(4)?;
    Ok(ThreadComment {
        id: r.get(0)?,
        by_uid: r.get(1)?,
        by_login: r.get(2)?,
        body: if hidden.is_some() {
            None
        } else {
            Some(r.get(3)?)
        },
        hidden: hidden.is_some(),
        created_at: r.get(5)?,
    })
}

fn thread_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Thread> {
    Ok(Thread {
        id: r.get(0)?,
        pack_id: r.get(1)?,
        kind: r.get(2)?,
        title: r.get(3)?,
        body: r.get(4)?,
        by_uid: r.get(5)?,
        by_login: r.get(6)?,
        status: r.get(7)?,
        source_pack: r.get(8)?,
        source_commit: r.get(9)?,
        merged_commit: r.get(10)?,
        created_at: r.get(11)?,
        decided_at: r.get(12)?,
        decided_by: r.get(13)?,
        comments: r.get(14)?,
    })
}

/// Add a nullable column to an existing table when it is absent -- the ADD
/// COLUMN counterpart to `CREATE TABLE IF NOT EXISTS`, so a schema addition
/// reaches a DB that predates it. `table`/`column`/`decl` are code constants,
/// never user input, so interpolating them is safe. Idempotent.
fn ensure_column(conn: &Connection, table: &str, column: &str, decl: &str) -> Result<()> {
    let present = conn
        .prepare(&format!("PRAGMA table_info({table})"))?
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<String>>>()?
        .iter()
        .any(|name| name == column);
    if !present {
        conn.execute(
            &format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"),
            [],
        )?;
    }
    Ok(())
}

/// Widen the `users.role` CHECK to admit the debug rung (#39). `CREATE TABLE IF
/// NOT EXISTS` cannot alter a table that predates the change, so an existing DB
/// keeps the old two-value CHECK and would reject a `debug` write. When the
/// stored DDL still lacks it, rebuild the table (SQLite's supported path for a
/// CHECK change) with ids preserved so the `sessions` foreign key stays valid.
/// Idempotent and a no-op on a fresh DB, whose schema already carries the rung.
fn widen_role_check(conn: &Connection) -> Result<()> {
    let ddl: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'users'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    let needs_rebuild = ddl.is_some_and(|sql| sql.contains("CHECK") && !sql.contains("'debug'"));
    if !needs_rebuild {
        return Ok(());
    }
    // foreign_keys must be toggled outside a transaction; off so dropping the old
    // users table does not disturb sessions rows, whose ids we carry over intact.
    conn.execute_batch(
        "PRAGMA foreign_keys = OFF;
         BEGIN;
         CREATE TABLE users_new (
             id            INTEGER PRIMARY KEY,
             github_uid    INTEGER NOT NULL UNIQUE,
             login         TEXT NOT NULL,
             role          TEXT NOT NULL DEFAULT 'member' CHECK (role IN ('member', 'admin', 'debug')),
             created_at    INTEGER NOT NULL,
             last_login_at INTEGER NOT NULL
         );
         INSERT INTO users_new (id, github_uid, login, role, created_at, last_login_at)
           SELECT id, github_uid, login, role, created_at, last_login_at FROM users;
         DROP TABLE users;
         ALTER TABLE users_new RENAME TO users;
         COMMIT;
         PRAGMA foreign_keys = ON;",
    )?;
    Ok(())
}

/// Admit the topic kind to `pack_threads.kind`. A DB made before topics keeps the
/// two-kind CHECK, which `CREATE TABLE IF NOT EXISTS` cannot alter and which
/// would refuse every topic opened. When the stored DDL lacks it, rebuild the
/// table with ids preserved, so the comments and notifications that reference a
/// thread by id still find it, and put its indexes back. Idempotent, and a no-op
/// on a fresh DB, whose schema already admits topics.
fn widen_thread_kinds(conn: &Connection) -> Result<()> {
    let ddl: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'pack_threads'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if !ddl.is_some_and(|sql| !sql.contains("'topic'")) {
        return Ok(());
    }
    // foreign_keys is toggled outside a transaction, and off so dropping the old
    // table leaves the comments and notifications that point at it alone
    conn.execute_batch("PRAGMA foreign_keys = OFF;")?;
    let rebuilt = conn.execute_batch(
        "BEGIN;
         CREATE TABLE pack_threads_new (
             id            INTEGER PRIMARY KEY,
             pack_id       TEXT NOT NULL,
             kind          TEXT NOT NULL CHECK (kind IN ('issue', 'topic', 'proposal')),
             title         TEXT NOT NULL,
             body          TEXT NOT NULL DEFAULT '',
             by_uid        INTEGER NOT NULL,
             status        TEXT NOT NULL DEFAULT 'open'
                           CHECK (status IN ('open', 'closed', 'merged', 'declined', 'withdrawn')),
             source_pack   TEXT,
             source_commit TEXT,
             merged_commit TEXT,
             created_at    INTEGER NOT NULL,
             decided_at    INTEGER,
             decided_by    INTEGER
         );
         INSERT INTO pack_threads_new
             (id, pack_id, kind, title, body, by_uid, status, source_pack, source_commit,
              merged_commit, created_at, decided_at, decided_by)
           SELECT id, pack_id, kind, title, body, by_uid, status, source_pack, source_commit,
                  merged_commit, created_at, decided_at, decided_by
           FROM pack_threads;
         DROP TABLE pack_threads;
         ALTER TABLE pack_threads_new RENAME TO pack_threads;
         CREATE INDEX IF NOT EXISTS idx_threads_pack ON pack_threads(pack_id, status);
         CREATE INDEX IF NOT EXISTS idx_threads_by ON pack_threads(by_uid);
         COMMIT;",
    );
    if let Err(e) = rebuilt {
        let _ = conn.execute_batch("ROLLBACK;");
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        return Err(e.into());
    }
    // every comment and notification must still name a thread that exists
    let dangling = conn
        .prepare("PRAGMA foreign_key_check")?
        .query_map([], |_| Ok(()))?
        .count();
    conn.execute_batch("PRAGMA foreign_keys = ON;")?;
    if dangling > 0 {
        anyhow::bail!("{dangling} rows lost their thread in the rebuild");
    }
    Ok(())
}

fn upload_from_row(r: &rusqlite::Row) -> rusqlite::Result<UploadRow> {
    Ok(UploadRow {
        id: r.get(0)?,
        uploader: r.get(1)?,
        pack_id: r.get(2)?,
        filename: r.get(3)?,
        sha1: r.get(4)?,
        size_bytes: r.get(5)?,
        status: r.get(6)?,
        note: r.get(7)?,
        created_at: r.get(8)?,
        upstream_maintainer: r.get(9)?,
        decided_by: r.get(10)?,
    })
}

fn insert_session(conn: &Connection, user_id: i64, now: i64) -> Result<String> {
    let sid = random_token();
    let expires = now + SESSION_TTL.as_secs() as i64;
    conn.execute(
        "INSERT INTO sessions (id, user_id, created_at, expires_at) VALUES (?1, ?2, ?3, ?4)",
        params![sid, user_id, now, expires],
    )
    .context("insert session")?;
    Ok(sid)
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// A 256-bit random token, hex-encoded: opaque session ids and OAuth `state`
/// nonces. Sourced from the OS CSPRNG so it is unguessable.
pub fn random_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_thread_is_opened_read_and_settled_once() {
        let a = Accounts::open_in_memory().unwrap();
        a.sign_in_github(42, "helper", None).unwrap();
        let id = a
            .open_thread("Create", "issue", "sodium crashes", "on entry", 42, None)
            .unwrap();

        let t = a.thread(id).unwrap().expect("thread reads back");
        assert_eq!(t.status, "open");
        assert_eq!(t.kind, "issue");
        assert_eq!(t.by_login.as_deref(), Some("helper"));
        assert_eq!(t.comments, 0);

        // two people closing at once: only the first decides it
        assert!(a.settle_thread(id, "closed", 7, None).unwrap());
        assert!(!a.settle_thread(id, "closed", 9, None).unwrap());
        // and a closed issue reopens, where a proposal would not
        assert!(a.reopen_thread(id).unwrap());
        assert_eq!(a.thread(id).unwrap().unwrap().status, "open");
    }

    #[test]
    fn a_proposal_carries_what_it_offers_and_does_not_reopen() {
        let a = Accounts::open_in_memory().unwrap();
        let id = a
            .open_thread(
                "Create",
                "proposal",
                "add sodium",
                "",
                42,
                Some(("u/42/Create", &"a".repeat(40))),
            )
            .unwrap();
        let t = a.thread(id).unwrap().unwrap();
        assert_eq!(t.source_pack.as_deref(), Some("u/42/Create"));

        assert!(
            a.settle_thread(id, "merged", 7, Some(&"b".repeat(40)))
                .unwrap()
        );
        assert_eq!(
            a.thread(id).unwrap().unwrap().merged_commit.as_deref(),
            Some(&*"b".repeat(40))
        );
        // offering again is a new proposal, not a resurrected one
        assert!(!a.reopen_thread(id).unwrap());
    }

    #[test]
    fn a_moderated_comment_keeps_its_place_and_loses_its_words() {
        let a = Accounts::open_in_memory().unwrap();
        a.sign_in_github(42, "helper", None).unwrap();
        let t = a.open_thread("Create", "issue", "x", "", 42, None).unwrap();
        let c1 = a.comment(t, 42, "a fair point").unwrap();
        let c2 = a.comment(t, 42, "an unfair one").unwrap();

        assert_eq!(a.thread(t).unwrap().unwrap().comments, 2);
        assert!(a.set_comment_hidden(c2, true, 1).unwrap());

        let rows = a.comments_on(t, None, None).unwrap();
        assert_eq!(rows.len(), 2, "a hidden comment keeps its place");
        assert_eq!(rows[0].id, c1);
        assert_eq!(rows[0].body.as_deref(), Some("a fair point"));
        assert!(rows[1].hidden);
        assert_eq!(
            rows[1].body, None,
            "the mirror never serves what was taken down"
        );

        // and it can be put back
        assert!(a.set_comment_hidden(c2, false, 1).unwrap());
        assert_eq!(
            a.comments_on(t, None, None).unwrap()[1].body.as_deref(),
            Some("an unfair one")
        );
    }

    #[test]
    fn a_deleted_pack_forgets_threads_from_either_side() {
        let a = Accounts::open_in_memory().unwrap();
        a.open_thread(
            "Create",
            "proposal",
            "x",
            "",
            42,
            Some(("u/42/fork", &"a".repeat(40))),
        )
        .unwrap();
        a.open_thread("u/42/fork", "issue", "y", "", 1, None)
            .unwrap();
        a.forget_pack_threads("u/42/fork").unwrap();
        assert!(
            a.threads_for("Create", None, false, None, None)
                .unwrap()
                .is_empty()
        );
        assert!(
            a.threads_for("u/42/fork", None, false, None, None)
                .unwrap()
                .is_empty()
        );
    }

    // A page is "everything after this row", and the row it names is the sort
    // key -- which for a list of threads is a pair, because two threads opened
    // in the same second are otherwise in no defined order and a cursor into an
    // undefined order skips rows or repeats them.
    #[test]
    fn a_long_discussion_is_read_a_page_at_a_time() {
        let a = Accounts::open_in_memory().unwrap();
        let t = a.open_thread("Create", "issue", "x", "", 42, None).unwrap();
        let ids: Vec<i64> = (0..5)
            .map(|i| a.comment(t, 42, &format!("{i}")).unwrap())
            .collect();

        let first = a.comments_on(t, None, Some(2)).unwrap();
        assert_eq!(first.iter().map(|c| c.id).collect::<Vec<_>>(), ids[..2]);
        let second = a.comments_on(t, Some(first[1].id), Some(2)).unwrap();
        assert_eq!(second.iter().map(|c| c.id).collect::<Vec<_>>(), ids[2..4]);
        let last = a.comments_on(t, Some(second[1].id), Some(2)).unwrap();
        assert_eq!(last.iter().map(|c| c.id).collect::<Vec<_>>(), ids[4..]);
        assert!(a.comments_on(t, Some(ids[4]), Some(2)).unwrap().is_empty());
        // and asking for no page is still the whole discussion
        assert_eq!(a.comments_on(t, None, None).unwrap().len(), 5);
    }

    #[test]
    fn a_pack_with_many_threads_pages_by_when_they_were_opened() {
        let a = Accounts::open_in_memory().unwrap();
        // all opened within the same second, which is the case the pair exists
        // for: without the id in the cursor the second page could repeat one
        let ids: Vec<i64> = (0..4)
            .map(|i| {
                a.open_thread("Create", "issue", &format!("t{i}"), "", 42, None)
                    .unwrap()
            })
            .collect();

        let page = a.threads_for("Create", None, false, None, Some(2)).unwrap();
        assert_eq!(
            page.iter().map(|t| t.id).collect::<Vec<_>>(),
            vec![ids[3], ids[2]],
            "newest first"
        );
        let last = &page[1];
        let rest = a
            .threads_for(
                "Create",
                None,
                false,
                Some((last.created_at, last.id)),
                Some(2),
            )
            .unwrap();
        assert_eq!(
            rest.iter().map(|t| t.id).collect::<Vec<_>>(),
            vec![ids[1], ids[0]]
        );
        assert_eq!(
            a.threads_for("Create", None, false, None, None)
                .unwrap()
                .len(),
            4
        );
    }

    #[test]
    fn a_topic_is_talked_about_closed_and_reopened_like_a_report() {
        let a = Accounts::open_in_memory().unwrap();
        a.sign_in_github(42, "player", None).unwrap();
        let id = a
            .open_thread("Create", "topic", "will it run on 4 GB?", "", 42, None)
            .unwrap();
        let t = a.thread(id).unwrap().unwrap();
        assert_eq!(t.kind, "topic");
        assert_eq!(t.source_pack, None);
        a.comment(id, 42, "asking for a friend").unwrap();
        assert!(a.settle_thread(id, "closed", 7, None).unwrap());
        assert!(a.reopen_thread(id).unwrap());
        assert_eq!(a.thread(id).unwrap().unwrap().status, "open");
    }

    #[test]
    fn thread_kind_migration_admits_topics_and_keeps_what_was_said() {
        // a DB made before topics: the two-kind CHECK refuses one
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE pack_threads (
                 id INTEGER PRIMARY KEY, pack_id TEXT NOT NULL,
                 kind TEXT NOT NULL CHECK (kind IN ('issue', 'proposal')),
                 title TEXT NOT NULL, body TEXT NOT NULL DEFAULT '', by_uid INTEGER NOT NULL,
                 status TEXT NOT NULL DEFAULT 'open'
                     CHECK (status IN ('open', 'closed', 'merged', 'declined', 'withdrawn')),
                 source_pack TEXT, source_commit TEXT, merged_commit TEXT,
                 created_at INTEGER NOT NULL, decided_at INTEGER, decided_by INTEGER);
             CREATE INDEX idx_threads_pack ON pack_threads(pack_id, status);
             CREATE INDEX idx_threads_by ON pack_threads(by_uid);
             CREATE TABLE thread_comments (
                 id INTEGER PRIMARY KEY,
                 thread_id INTEGER NOT NULL REFERENCES pack_threads(id) ON DELETE CASCADE,
                 by_uid INTEGER NOT NULL, body TEXT NOT NULL, created_at INTEGER NOT NULL,
                 hidden_at INTEGER, hidden_by INTEGER);
             CREATE TABLE notifications (
                 id INTEGER PRIMARY KEY, uid INTEGER NOT NULL, kind TEXT NOT NULL,
                 thread_id INTEGER NOT NULL REFERENCES pack_threads(id) ON DELETE CASCADE,
                 actor_uid INTEGER NOT NULL, created_at INTEGER NOT NULL, read_at INTEGER);
             INSERT INTO pack_threads (id, pack_id, kind, title, by_uid, created_at)
               VALUES (7, 'Create', 'issue', 'it crashes', 42, 1);
             INSERT INTO thread_comments (thread_id, by_uid, body, created_at)
               VALUES (7, 43, 'same here', 2);
             INSERT INTO notifications (uid, kind, thread_id, actor_uid, created_at)
               VALUES (42, 'comment', 7, 43, 2);",
        )
        .unwrap();
        assert!(
            conn.execute(
                "INSERT INTO pack_threads (pack_id, kind, title, by_uid, created_at)
                 VALUES ('Create', 'topic', 'x', 1, 3)",
                []
            )
            .is_err(),
            "the legacy CHECK refuses a topic"
        );

        widen_thread_kinds(&conn).unwrap();

        conn.execute(
            "INSERT INTO pack_threads (pack_id, kind, title, by_uid, created_at)
             VALUES ('Create', 'topic', 'x', 1, 3)",
            [],
        )
        .unwrap();
        // the old thread kept its id, and what pointed at it still does
        let (title, said, told): (String, i64, i64) = conn
            .query_row(
                "SELECT t.title,
                        (SELECT COUNT(*) FROM thread_comments WHERE thread_id = t.id),
                        (SELECT COUNT(*) FROM notifications WHERE thread_id = t.id)
                 FROM pack_threads t WHERE t.id = 7",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!((title.as_str(), said, told), ("it crashes", 1, 1));
        let indexes: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'index' AND name IN ('idx_threads_pack', 'idx_threads_by')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(indexes, 2, "the indexes came back with the table");
        // the foreign keys are enforced again: a cascade still reaches the rows
        conn.execute("DELETE FROM pack_threads WHERE id = 7", [])
            .unwrap();
        let left: i64 = conn
            .query_row("SELECT COUNT(*) FROM thread_comments", [], |r| r.get(0))
            .unwrap();
        assert_eq!(left, 0);

        // a second run is a no-op
        widen_thread_kinds(&conn).unwrap();
    }

    #[test]
    fn a_thread_cannot_be_opened_or_settled_into_a_state_nobody_defined() {
        let a = Accounts::open_in_memory().unwrap();
        assert!(a.open_thread("Create", "rant", "x", "", 42, None).is_err());
        let id = a.open_thread("Create", "issue", "x", "", 42, None).unwrap();
        assert!(a.settle_thread(id, "eaten", 1, None).is_err());
    }

    #[test]
    fn access_is_granted_moved_and_taken_away() {
        let a = Accounts::open_in_memory().unwrap();
        assert_eq!(a.pack_access_level("Create", 42).unwrap(), None);

        a.grant_pack_access("Create", 42, PackLevel::View, 1)
            .unwrap();
        assert_eq!(
            a.pack_access_level("Create", 42).unwrap(),
            Some(PackLevel::View)
        );

        // re-granting moves the level rather than adding a second row, and
        // re-stamps who is answerable for the access as it stands
        a.grant_pack_access("Create", 42, PackLevel::Edit, 7)
            .unwrap();
        assert_eq!(
            a.pack_access_level("Create", 42).unwrap(),
            Some(PackLevel::Edit)
        );
        let list = a.list_pack_access("Create").unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].granted_by, 7);

        assert!(a.revoke_pack_access("Create", 42).unwrap());
        assert!(
            !a.revoke_pack_access("Create", 42).unwrap(),
            "second revoke is a no-op"
        );
        assert_eq!(a.pack_access_level("Create", 42).unwrap(), None);
    }

    #[test]
    fn access_is_scoped_to_one_pack_and_names_who_it_can() {
        let a = Accounts::open_in_memory().unwrap();
        a.sign_in_github(42, "octocat", None).unwrap();
        a.grant_pack_access("Create", 42, PackLevel::Edit, 1)
            .unwrap();
        a.grant_pack_access("Create", 99, PackLevel::Own, 1)
            .unwrap();

        // a grant on one pack says nothing about another
        assert_eq!(a.pack_access_level("Industrial", 42).unwrap(), None);
        assert_eq!(
            a.packs_granted_to(42).unwrap(),
            vec![("Create".to_string(), PackLevel::Edit)]
        );

        // highest level first; a uid that never signed in has no login to show
        let list = a.list_pack_access("Create").unwrap();
        assert_eq!(list[0].github_uid, 99);
        assert_eq!(list[0].login, None);
        assert_eq!(list[1].login.as_deref(), Some("octocat"));
    }

    #[test]
    fn a_deleted_pack_does_not_bequeath_its_access_list() {
        // ids are re-mintable: a new pack under an old name must not inherit
        // whoever could reach the one before it
        let a = Accounts::open_in_memory().unwrap();
        a.grant_pack_access("Create", 42, PackLevel::Edit, 1)
            .unwrap();
        a.forget_pack_access("Create").unwrap();
        assert_eq!(a.pack_access_level("Create", 42).unwrap(), None);
    }

    #[test]
    fn the_reserved_uid_is_not_a_person_to_grant_to() {
        let a = Accounts::open_in_memory().unwrap();
        assert!(a.grant_pack_access("Create", 0, PackLevel::Own, 1).is_err());
    }

    #[test]
    fn github_sign_in_persists_user_and_resolves_session() {
        let a = Accounts::open_in_memory().unwrap();
        let sid = a.sign_in_github(42, "octocat", None).unwrap();
        let id = a.session_identity(&sid).unwrap().expect("session resolves");
        assert_eq!(id.uid, 42);
        assert_eq!(id.login, "octocat");
        assert_eq!(id.role, Role::Member);

        // a second sign-in reuses the row (no duplicate), and the allowlist can
        // promote the same uid to admin
        let sid2 = a
            .sign_in_github(42, "octocat-renamed", Some(Role::Admin))
            .unwrap();
        let id2 = a.session_identity(&sid2).unwrap().unwrap();
        assert_eq!(id2.login, "octocat-renamed");
        assert_eq!(id2.role, Role::Admin);
        let users: i64 = a
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM users WHERE github_uid = 42",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(users, 1);
    }

    #[test]
    fn deleted_session_stops_resolving() {
        let a = Accounts::open_in_memory().unwrap();
        let sid = a.sign_in_github(7, "x", Some(Role::Admin)).unwrap();
        a.delete_session(&sid).unwrap();
        assert!(a.session_identity(&sid).unwrap().is_none());
    }

    #[test]
    fn upload_keeps_provenance_and_the_decider() {
        let a = Accounts::open_in_memory().unwrap();
        let id = a
            .enqueue_upload(
                42,
                "u/42/p",
                "thaumcraft.jar",
                &"a".repeat(40),
                100,
                Some("the upstream"),
            )
            .unwrap();
        let row = a.get_upload(id).unwrap().unwrap();
        assert_eq!(row.status, "pending");
        assert_eq!(row.upstream_maintainer.as_deref(), Some("the upstream"));
        assert_eq!(row.decided_by, None); // undecided while pending

        a.set_upload_status(id, "approved", None, Some(7)).unwrap();
        let row = a.get_upload(id).unwrap().unwrap();
        assert_eq!(row.status, "approved");
        assert_eq!(row.decided_by, Some(7));
        // the origin the uploader named survives the decision
        assert_eq!(row.upstream_maintainer.as_deref(), Some("the upstream"));
    }

    #[test]
    fn ensure_column_adds_missing_and_is_idempotent() {
        // a mod_uploads created before the provenance columns
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE mod_uploads (
                 id INTEGER PRIMARY KEY, uploader INTEGER NOT NULL, pack_id TEXT NOT NULL,
                 filename TEXT NOT NULL, sha1 TEXT NOT NULL, size_bytes INTEGER NOT NULL,
                 status TEXT NOT NULL DEFAULT 'pending', note TEXT, created_at INTEGER NOT NULL,
                 decided_at INTEGER);",
        )
        .unwrap();
        ensure_column(&conn, "mod_uploads", "upstream_maintainer", "TEXT").unwrap();
        ensure_column(&conn, "mod_uploads", "decided_by", "INTEGER").unwrap();
        // a second run is a no-op (column already there)
        ensure_column(&conn, "mod_uploads", "decided_by", "INTEGER").unwrap();
        conn.execute(
            "INSERT INTO mod_uploads
               (uploader, pack_id, filename, sha1, size_bytes, status, created_at,
                upstream_maintainer, decided_by)
             VALUES (1, 'p', 'f.jar', 'sha', 1, 'pending', 0, 'up', 9)",
            [],
        )
        .unwrap();
        let (m, d): (String, i64) = conn
            .query_row(
                "SELECT upstream_maintainer, decided_by FROM mod_uploads",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(m, "up");
        assert_eq!(d, 9);
    }

    #[test]
    fn debug_rung_outranks_admin_and_is_grantable() {
        let a = Accounts::open_in_memory().unwrap();
        // allowlisted as debug -> the top rung, re-asserted on login
        let sid = a.sign_in_github(3, "t", Some(Role::Debug)).unwrap();
        let id = a.session_identity(&sid).unwrap().unwrap();
        assert_eq!(id.role, Role::Debug);
        assert!(id.role > Role::Admin);

        // a member can be granted debug through the UI path
        a.sign_in_github(11, "helper", None).unwrap();
        a.set_role(11, "debug").unwrap();
        let sid2 = a.sign_in_github(11, "helper", None).unwrap();
        assert_eq!(
            a.session_identity(&sid2).unwrap().unwrap().role,
            Role::Debug
        );

        // an unknown role is still rejected
        assert!(a.set_role(11, "root").is_err());
    }

    #[test]
    fn ui_promotion_sticks_but_allowlist_stays_authoritative() {
        let a = Accounts::open_in_memory().unwrap();
        // a non-allowlisted member is promoted via the UI, then signs in again
        a.sign_in_github(5, "m", None).unwrap();
        a.set_role(5, "admin").unwrap();
        let sid = a.sign_in_github(5, "m", None).unwrap();
        assert_eq!(a.session_identity(&sid).unwrap().unwrap().role, Role::Admin);

        // an allowlisted user re-promotes to admin on login even after a demote
        a.sign_in_github(9, "op", Some(Role::Admin)).unwrap();
        a.set_role(9, "member").unwrap();
        let sid2 = a.sign_in_github(9, "op", Some(Role::Admin)).unwrap();
        assert_eq!(
            a.session_identity(&sid2).unwrap().unwrap().role,
            Role::Admin
        );

        // list excludes the reserved uid 0
        let users = a.list_users().unwrap();
        assert_eq!(users.len(), 2);
        assert!(users.iter().all(|u| u.github_uid != 0));

        // the reserved uid 0 is untouchable
        assert!(a.set_role(0, "member").is_err());
    }

    #[test]
    fn role_check_migration_admits_debug_on_a_legacy_db() {
        // a DB created before the debug rung: the two-value CHECK rejects 'debug'
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE users (
                 id INTEGER PRIMARY KEY, github_uid INTEGER NOT NULL UNIQUE, login TEXT NOT NULL,
                 role TEXT NOT NULL DEFAULT 'member' CHECK (role IN ('member', 'admin')),
                 created_at INTEGER NOT NULL, last_login_at INTEGER NOT NULL);
             CREATE TABLE sessions (id TEXT PRIMARY KEY,
                 user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                 created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL);
             INSERT INTO users (github_uid, login, role, created_at, last_login_at)
               VALUES (5, 'x', 'admin', 0, 0);
             INSERT INTO sessions VALUES ('s', 1, 0, 0);",
        )
        .unwrap();
        assert!(
            conn.execute("UPDATE users SET role = 'debug' WHERE github_uid = 5", [])
                .is_err(),
            "legacy CHECK should reject debug"
        );

        widen_role_check(&conn).unwrap();

        // after the rebuild debug is accepted and the row + its session survived
        conn.execute("UPDATE users SET role = 'debug' WHERE github_uid = 5", [])
            .unwrap();
        let role: String = conn
            .query_row("SELECT role FROM users WHERE github_uid = 5", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(role, "debug");
        let sessions: i64 = conn
            .query_row("SELECT count(*) FROM sessions WHERE user_id = 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(sessions, 1);

        // second run is a no-op (DDL already carries the rung)
        widen_role_check(&conn).unwrap();
    }

    #[test]
    fn audit_log_records_and_lists_newest_first() {
        let a = Accounts::open_in_memory().unwrap();
        a.record_audit(42, "octocat", "role.set", Some("7"), Some("admin"))
            .unwrap();
        a.record_audit(42, "octocat", "upload.approve", Some("deadbeef"), None)
            .unwrap();
        let rows = a.list_audit(10, None).unwrap();
        assert_eq!(rows.len(), 2);
        // newest first
        assert_eq!(rows[0].action, "upload.approve");
        assert_eq!(rows[0].target.as_deref(), Some("deadbeef"));
        assert_eq!(rows[0].detail, None);
        assert_eq!(rows[1].action, "role.set");
        assert_eq!(rows[1].actor_login, "octocat");
        assert_eq!(rows[1].detail.as_deref(), Some("admin"));
    }

    // The trail is read backwards a page at a time, and an entry landing while
    // someone reads must not shift the page under them -- which is why the
    // cursor is an id rather than an offset.
    #[test]
    fn the_trail_reads_back_a_page_at_a_time() {
        let a = Accounts::open_in_memory().unwrap();
        for i in 0..5 {
            a.record_audit(42, "octocat", &format!("act.{i}"), None, None)
                .unwrap();
        }
        let first = a.list_audit(2, None).unwrap();
        assert_eq!(
            first.iter().map(|r| r.action.as_str()).collect::<Vec<_>>(),
            ["act.4", "act.3"]
        );

        a.record_audit(42, "octocat", "act.new", None, None)
            .unwrap();

        let second = a.list_audit(2, Some(first[1].id)).unwrap();
        assert_eq!(
            second.iter().map(|r| r.action.as_str()).collect::<Vec<_>>(),
            ["act.2", "act.1"],
            "the newcomer lands on top of the trail, not into the page being read"
        );
        let third = a.list_audit(2, Some(second[1].id)).unwrap();
        assert_eq!(
            third.iter().map(|r| r.action.as_str()).collect::<Vec<_>>(),
            ["act.0"]
        );
        assert!(a.list_audit(2, Some(third[0].id)).unwrap().is_empty());
    }
}
