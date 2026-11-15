-- accounts.db: persistent user identities (from GitHub OAuth) and server-side
-- sessions keyed to a user -- the multi-user auth foundation. A sign-in is a
-- `users` row; a session id maps to a user, not to a raw token. Grants and
-- user_flags may join here in later phases.

CREATE TABLE IF NOT EXISTS accounts_meta (
    k TEXT PRIMARY KEY,
    v TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS users (
    id            INTEGER PRIMARY KEY,
    github_uid    INTEGER NOT NULL UNIQUE,
    login         TEXT NOT NULL,
    role          TEXT NOT NULL DEFAULT 'member' CHECK (role IN ('member', 'admin', 'debug')),
    created_at    INTEGER NOT NULL,
    last_login_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS sessions (
    id         TEXT PRIMARY KEY,
    user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_sessions_user ON sessions(user_id);
CREATE INDEX IF NOT EXISTS idx_sessions_expires ON sessions(expires_at);

-- Retire the seeded break-glass user (github_uid = 0) that the removed token
-- login opened sessions for. A DB created before that path was dropped still
-- carries the row; delete it so its sessions cascade away and nothing is left
-- pinned to the reserved uid. A no-op on fresh DBs, which never seed it.
DELETE FROM users WHERE github_uid = 0;

-- Member jar uploads awaiting moderation. A self-hosted jar never lands in the
-- shared cache directly: it stages here as `pending`, an operator approves it
-- (jar promoted to the cache) or rejects it. `note` carries the auto-gate reason
-- or the moderator's. Provenance (an archival upload must stay a traceable
-- record): `upstream_maintainer` is who the uploader names as the jar's origin,
-- `decided_by` the moderator who accepted/rejected it. See the upload-moderation
-- policy. (upstream_maintainer/decided_by are also added to an older DB by an
-- ADD COLUMN migration in `Accounts::init`.)
CREATE TABLE IF NOT EXISTS mod_uploads (
    id         INTEGER PRIMARY KEY,
    uploader   INTEGER NOT NULL,
    pack_id    TEXT NOT NULL,
    filename   TEXT NOT NULL,
    sha1       TEXT NOT NULL,
    size_bytes INTEGER NOT NULL,
    status     TEXT NOT NULL DEFAULT 'pending'
               CHECK (status IN ('pending', 'approved', 'rejected')),
    note       TEXT,
    created_at INTEGER NOT NULL,
    decided_at INTEGER,
    upstream_maintainer TEXT,
    decided_by INTEGER
);
CREATE INDEX IF NOT EXISTS idx_uploads_status ON mod_uploads(status);
CREATE INDEX IF NOT EXISTS idx_uploads_uploader ON mod_uploads(uploader);

-- System-wide audit log: who did what, when. Every accountable operator /
-- moderator action (role changes, upload decisions, pack edits, takedowns, ...)
-- records the actor's github identity, the action, its target, and optional
-- detail. Community-mirror accountability -- a plain "who did what" trail for the
-- mirror's own operators. Local-only; never egresses.
CREATE TABLE IF NOT EXISTS audit_log (
    id          INTEGER PRIMARY KEY,
    actor_uid   INTEGER NOT NULL,
    actor_login TEXT NOT NULL,
    action      TEXT NOT NULL,
    target      TEXT,
    detail      TEXT,
    created_at  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_audit_created ON audit_log(created_at);
CREATE INDEX IF NOT EXISTS idx_audit_actor ON audit_log(actor_uid);

-- Rules-of-use acceptance, keyed by github uid. A member must accept before
-- authoring or forking community content. A separate table (not a users column)
-- so the idempotent CREATE-IF-NOT-EXISTS schema needs no ALTER migration.
CREATE TABLE IF NOT EXISTS terms_acceptance (
    github_uid  INTEGER PRIMARY KEY,
    accepted_at INTEGER NOT NULL
);

-- Per-pack access (ADR 0006). The mirror's roles answer what somebody may do to
-- the mirror; this answers what they may do to one pack, so letting a person
-- help with a single pack no longer means handing them the whole mirror.
--
-- Two answers are deliberately not rows here: the owner of a community
-- namespace (`u/<uid>/<pack>`) and an admin. Both are rules the gate knows
-- before it reads anything, so a grant is only ever the third answer. Access
-- lives here rather than in the pack's config because the config is authored by
-- clients and merged live -- a permission the restrained can rewrite is not one.
CREATE TABLE IF NOT EXISTS pack_access (
    pack_id    TEXT NOT NULL,
    github_uid INTEGER NOT NULL,
    level      TEXT NOT NULL CHECK (level IN ('view', 'edit', 'own')),
    granted_by INTEGER NOT NULL,
    granted_at INTEGER NOT NULL,
    PRIMARY KEY (pack_id, github_uid)
);
CREATE INDEX IF NOT EXISTS idx_pack_access_uid ON pack_access(github_uid);

-- Threads: everything said about a pack that is not the pack itself -- a report
-- ("mod X crashes on entry"), a topic ("will it run on 4 GB?"), or a fork
-- offered back. One table because they differ in what opens them and how they
-- settle, and in nothing else: each is somebody talking to a pack's keepers and
-- its players, each carries a discussion, each ends somewhere worth reading
-- afterwards. Separate tables would mean several of everything below, starting
-- with comments.
--
-- A proposal is a thread whose `source_pack`/`source_commit` name the state it
-- offers; an issue and a topic leave them null. `status` is the thread's own
-- vocabulary: an issue or a topic is open or closed, a proposal is open,
-- merged, declined or withdrawn.
-- Settled threads keep their rows -- "we said no in March" is what somebody
-- looks for in April.
CREATE TABLE IF NOT EXISTS pack_threads (
    id            INTEGER PRIMARY KEY,
    pack_id       TEXT NOT NULL,
    kind          TEXT NOT NULL CHECK (kind IN ('issue', 'topic', 'proposal')),
    title         TEXT NOT NULL,
    body          TEXT NOT NULL DEFAULT '',
    by_uid        INTEGER NOT NULL,
    status        TEXT NOT NULL DEFAULT 'open'
                  CHECK (status IN ('open', 'closed', 'merged', 'declined', 'withdrawn')),
    -- proposals only
    source_pack   TEXT,
    source_commit TEXT,
    merged_commit TEXT,
    created_at    INTEGER NOT NULL,
    decided_at    INTEGER,
    decided_by    INTEGER
);
CREATE INDEX IF NOT EXISTS idx_threads_pack ON pack_threads(pack_id, status);
CREATE INDEX IF NOT EXISTS idx_threads_by ON pack_threads(by_uid);

-- An account the mirror's operators have stopped, as distinct from a pack's own
-- block below. The two answer different questions and both are needed: a pack's
-- keepers decide who writes in their discussion, and the operators decide who
-- puts content on the mirror at all -- somebody whose pack was the offence
-- cannot be answered by blocking them from one discussion.
--
-- It bars writing everywhere: authoring or forking a pack, uploading a jar,
-- opening a thread, saying anything on one. Reading is untouched, as with a
-- pack block: a suspension is not a way to unpublish what somebody already
-- said. The reason is written for the person it names -- they are shown it when
-- they try to write and on their own page.
CREATE TABLE IF NOT EXISTS account_suspensions (
    github_uid INTEGER PRIMARY KEY,
    reason     TEXT,
    by_uid     INTEGER NOT NULL,
    at         INTEGER NOT NULL
);

-- Who a pack's keepers have asked to stop. Hiding a comment answers what was
-- already said; this answers the next one, which is the difference between
-- cleaning up after somebody and not hosting them. It sits beside `pack_access`
-- because it is the same question from the other side -- that list says who may
-- reach a pack, this one who may no longer write on it.
--
-- A block bars writing (a report, a proposal, a comment) and nothing else: a
-- published pack's discussion stays readable, so a block can never quietly
-- erase somebody from a record they are already part of. `reason` is written
-- for the person it names: a door that shuts with no word is one they can only
-- argue with, so it is served to them when they try to write.
CREATE TABLE IF NOT EXISTS pack_blocks (
    pack_id    TEXT NOT NULL,
    github_uid INTEGER NOT NULL,
    reason     TEXT,
    blocked_by INTEGER NOT NULL,
    blocked_at INTEGER NOT NULL,
    PRIMARY KEY (pack_id, github_uid)
);
CREATE INDEX IF NOT EXISTS idx_pack_blocks_uid ON pack_blocks(github_uid);

-- The address somebody's own notifications can be read at from outside the
-- panel. A feed reader has no session and cannot be given one, so the key in the
-- address is what identifies the account -- which makes it a secret in a URL:
-- one key per account, rotatable, and rotating invalidates whatever was handed
-- out before. It grants exactly one thing, reading that account's own list, and
-- is stored apart from the sessions so it can be rotated without touching them.
CREATE TABLE IF NOT EXISTS feed_keys (
    github_uid INTEGER PRIMARY KEY,
    key        TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL
);

-- Somebody was answered and does not know it. A discussion where a reply
-- reaches nobody is a discussion people stop reading: the report sits open
-- because its author never learned it was answered, and the pack's keepers
-- learn about a report when they happen to open the tab.
--
-- One row per event rather than a per-thread counter, so "who answered what,
-- when" survives being read: the row is marked read, not deleted. The thread is
-- the only thing referenced -- what to show is read from it, so a title edited
-- afterwards does not leave a stale copy here -- and a deleted pack takes its
-- threads and, with them, these.
CREATE TABLE IF NOT EXISTS notifications (
    id         INTEGER PRIMARY KEY,
    uid        INTEGER NOT NULL,
    -- 'comment' (somebody said something), 'opened' (a thread on a pack you
    -- keep), 'settled' (yours was closed, declined, merged or reopened)
    kind       TEXT NOT NULL CHECK (kind IN ('comment', 'opened', 'settled')),
    thread_id  INTEGER NOT NULL REFERENCES pack_threads(id) ON DELETE CASCADE,
    actor_uid  INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    read_at    INTEGER
);
CREATE INDEX IF NOT EXISTS idx_notifications_uid ON notifications(uid, read_at, id);

-- What people said on a thread. Hidden rather than deleted when moderated: the
-- fact that something was said and taken down is itself part of the record, and
-- a hole in a numbered discussion is worse than a marked gap. `hidden_by` is the
-- moderator, so the trail names who decided.
CREATE TABLE IF NOT EXISTS thread_comments (
    id         INTEGER PRIMARY KEY,
    thread_id  INTEGER NOT NULL REFERENCES pack_threads(id) ON DELETE CASCADE,
    by_uid     INTEGER NOT NULL,
    body       TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    hidden_at  INTEGER,
    hidden_by  INTEGER
);
CREATE INDEX IF NOT EXISTS idx_comments_thread ON thread_comments(thread_id, created_at);
