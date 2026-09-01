# Operations

Running the mirror and curating its content.

## Environment

| Variable | Default | Purpose |
|---|---|---|
| `SMRT_BIND_ADDR` | `127.0.0.1:9000` | TCP bind address (nginx terminates TLS in front). |
| `SMRT_STORAGE_DIR` | `/var/lib/smrt` | Storage root (see the tree in [architecture.md](architecture.md)). |
| `SMRT_MIRROR_BASE` | `http://127.0.0.1:9000` | Public base URL baked into manifest source URLs. Set to your real origin on any public deployment. |
| `SMRT_ADMIN_TOKEN` | none | Bearer for headless admin calls; admin routes refuse without a valid identity. |
| `SMRT_OPERATOR_UID` | `0` | GitHub uid that owns operator-authored packs (and backfills ownership on packs predating the field). |
| `SMRT_GITHUB_CLIENT_ID` / `SMRT_GITHUB_CLIENT_SECRET` | none | GitHub OAuth app for panel sign-in. Absent = OAuth login disabled. |
| `SMRT_ADMIN_GITHUB_UIDS` | empty | Comma-separated GitHub uids granted Admin on sign-in. |
| `SMRT_DEBUG_TOKEN` / `SMRT_DEBUG_GITHUB_UIDS` | none | The Debug rung above Admin: gates compat-affecting registry writes (authored classification, forced overrides). Leave unset in production unless needed. |
| `SMRT_COOKIE_SECURE` | `true` | Set `false` only for plain-HTTP local dev. |
| `RUST_LOG` | `smrt=info` | tracing filter. |

Production config lives in `/etc/smrt/env` (systemd `EnvironmentFile`).

## Deploy

Push to `main` deploys: CI runs the gates, builds both binaries, ships them
via SSH (`smrt` restarts the service; `smrt-pack` is replaced in place -- it
opens the same registry and must never drift behind the schema), and probes
`/v1/health` until it answers. `deploy/` holds the systemd unit, nginx conf,
and the emergency local-deploy script for when Actions is down.

Two operational consequences of a deploy:

- **a restart kills running builds** -- the job dies with the process, and at
  the next start its persisted snapshot is marked failed with an
  "interrupted by service restart" line, so pollers learn the truth. Finished
  job ids keep answering from `jobs/<id>.json` snapshots across restarts
  (newest 200 kept). Reading one is gated on the pack it is about, at the same
  level as reading that pack: a build log names the pack, its mods and whatever
  the pre-publish check refused. Still: do not start long server-side jobs while
  a deploy is in flight.
- **migrations run at service start** (`registry_meta.schema_version`
  gates them). A failed migration keeps the old schema and refuses further
  steps; fix forward.

### What a request may cost in memory

Every write route that takes a file buffers the whole body in RAM before the
handler runs, so the body ceiling is a memory ceiling and is sized per route:

- **8 GiB** -- `POST .../bootstrap` and `POST .../validate`, the two routes that
  take a whole Minecraft instance archive in one request. Bootstrap copies the
  buffered body once more before unpacking, so a request near the limit holds
  twice this for its lifetime. Both are the ceiling nginx is raised to match
  (`client_max_body_size`); the smaller of the two wins, so raising one alone
  just moves the wall.
- **512 MiB** -- anything carrying one file: a cache jar, a pack static asset,
  a member's upload.
- **8 MiB** -- build requests and document sync, which are only ever JSON or a
  CRDT update.

The archive routes are the ones to keep in mind when sizing the box: a
concurrent pair of bootstraps is tens of gigabytes of resident memory, and
nothing throttles them beyond the admin gate on bootstrap.

Back up `registry.db` before risky curation:

```
smrt-pack registry backup --storage /var/lib/smrt --out registry-$(date +%F).db
```

## The authoring workflow

Day to day, everything happens in the panel; the CLI mirrors it for scripting.

1. **Declare** mods in the pack editor (Modrinth picker, mirror cache picker,
   or GitHub-release ingest which lands the jar in the cache). Set
   `default_enabled` per mod; set the pack's `version` base (`MAJOR.MINOR`).
2. **Save** -- the server runs depfill: missing hard dependencies are pulled
   in (Modrinth first, mirror cache second), the resolved `requires` graph is
   recorded. Depfill failure (an upstream outage) is non-fatal: the config
   saves as-is and the resolve report shows what is missing.
3. **Review the resolve report**: unresolved sources, missing dependencies,
   loader mismatches, side advisories (server-side mods, coremods,
   side disagreements between Modrinth and bytecode), curator suggestions
   from `Recommends` edges.
4. **Commit** -- a build is made from a checkpoint, not from what is on
   screen, so a publish with uncommitted work is refused (`409`, naming the
   count). The commit box lists what it is about to record -- arrivals,
   departures, re-pins with both version numbers, renames, install defaults,
   assets, the pack's own fields -- and offers a first line for the message.
   Server-filled metadata (the requires graph, presence, depfill's own rows) is
   never a change, and neither are the server-controlled fields. The panel's
   build button commits and builds in one press when the pack is dirty.
5. **Build** -- pick a channel (default beta; release is deliberate),
   optionally pin an explicit version. The build classifies, derives
   required-ness, resolves every source, and publishes manifest + summary +
   latest pointer. Dry-run first when in doubt: same resolution, nothing
   published.
6. **Converge** freshly added Modrinth mods: the first build downloads their
   jars and pokes the harvest; the next build waits for that harvest to
   settle and classifies them fully (the job log shows "waiting for the
   registry harvest to settle" when it does). Newly pulled dependencies of
   new mods may need one more save -> build cycle.

CLI equivalents: `smrt-pack bootstrap | validate | depfill | build
--channel ... | enrich-mcmod | infer-requires | upload-static |
reconstruct-config`. The CLI writes the config directly and builds the working
state, so a script commits nothing and is refused nothing.

Bootstrap seeds a pack; it does not re-seed one. Over the API it refuses a pack
that already has a config (`409`), because it writes a whole config from what it
finds in the archive and would otherwise replace every curated decision in the
existing one. To rebuild a pack from a fresh archive, delete it first or
bootstrap under another id. The CLI's `bootstrap` writes to a file you name and
is yours to point wherever you like.

### The pack card

What the catalogue shows: an icon, a banner, gallery shots and a CommonMark
description, stored on the config as `pack_meta` and carried onto the built
`summary.json`.

The pictures belong to the pack. Drop one on the editor's Branding tab -- it is
cropped and written into the pack's own static tree as `_pack/icon.png` -- and
the card's field fills in with that path. The build resolves it against
`SMRT_MIRROR_BASE`, so what a launcher or the public catalogue reads is always a
URL it can fetch, and the config keeps a path that survives the mirror changing
domain.

A field may also hold a full `http(s)://` address, which travels to the card
unchanged. It is worth knowing what that means before using it: an image is not
a link. Nobody clicks it -- every browser that opens the catalogue fetches it
automatically, so the host it points at learns the address of everyone who
looked at the pack. A pack's own file costs nothing and leaks nothing; someone
else's CDN is a third party on a public page. Nothing enforces this either way
(see the note in [architecture.md](architecture.md)).

### Who may reach a pack

Access is a grant on one pack, not a rung on the mirror (ADR 0006). Three
levels: `view` reads a draft, its history and its reports; `edit` writes the
config, commits and builds; `own` also hands out and takes away access, changes
visibility, and deletes.

Two answers are never rows in the list and never need one: the owner of a
community namespace (`u/<uid>/<pack>`) owns their pack because the id says so,
and an admin owns every pack because that is what the rung means. A grant is
only ever the third answer -- somebody who is neither, which is what letting one
person help with one pack means without handing them the mirror.

`GET .../access` lists the grants, `POST .../access` (`{github_uid, level}`)
grants or moves one, `DELETE .../access/{uid}` takes it away; the last two need
`own`. Grants are keyed by GitHub uid rather than login, because a login is
its owner's to change. Every grant and revocation lands in the audit log, and
a deleted pack forgets its list so a re-minted id inherits nobody.

`GET .../access/mine` answers what the caller may do here (`{"level":"edit"}`,
or `{}` for nobody in particular). It exists so a client asks the gate instead
of deriving the answer from the pack id and the caller's role -- a derivation
that is right for the owner and the admin and wrong for exactly the person a
grant was written for.

### Discussions: reports and proposals

Everything said about a pack that is not the pack itself is a thread on it: an
`issue` (a report -- "mod X crashes on entry") or a `proposal` (a fork offered
back, naming the commit it offers). One shape for both, because they differ in
what opens them and how they settle and in nothing else, and because a
discussion belongs to both.

Reading is as public as the pack, and it is the only read on the public router
that is not about pack content: `GET /v1/packs/{id}/threads`,
`GET /v1/threads/{id}` and `GET /v1/threads/{id}/diff` answer without a session
for a published pack, and stay private for a draft. A decision nobody can see is
indistinguishable from one nobody made, which is the whole reason the read is
public. There is no second, authenticated copy of these reads: one home, so the
panel and a stranger with a link are looking at the same answer.

Writing needs a session. Anyone signed in may open an issue on a published pack
and join any discussion they can read; `edit` on the pack closes, declines,
merges and moderates; the person who opened a thread may withdraw or close it
themselves. Comments are hidden rather than deleted -- that something was said
and taken down is part of the record, and the mirror stops serving the body
while keeping the gap visible with who took it down. Closed issues reopen;
proposals do not, because their offer was a commit and offering again is a new
proposal.

Being answered reaches people. A comment tells everybody already in the
discussion -- whoever opened it and whoever has spoken -- plus the pack's
keepers; opening a thread tells the keepers; a decision (closed, declined,
merged, withdrawn, reopened) tells everybody who was in it. Keepers here are the
same people the gate would let act: whoever the namespace belongs to, or the
mirror's operators for an official pack, plus anybody granted `edit` or `own`.
Nobody is ever told about their own act. `GET /v1/me/notifications` reads the
caller's own list (`?unread=true`, `?limit=`) and answers `{unread, rows}` --
the count is the whole list, the rows are a slice of it -- and
`POST /v1/me/notifications/read` (`{id?}`) marks one or all read. It pages like
the rest: `?limit=` and a `Link` to the next, the cursor being the last id
served. Outside the panel, `GET /v1/feed.atom?key=<key>` is the same list as Atom: one
address per account, minted on first ask at `GET /v1/me/feed-key` and retired by
`POST` to the same path. A feed reader has no session and cannot be given one,
so the address is the credential -- treat it as a password, and rotate it if it
gets out. Nothing leaves this machine to deliver it: no address of anybody's is
stored, and no third party is asked.

The `unread` count is the whole list rather than the page, because a
badge and a page are different numbers and deriving one from the other would be
a lie on any list longer than a page. A notification
carries no copy of the thread: the title and status are read from it live, so an
edited title never leaves a stale line behind, and taking somebody's access to a
pack away forgets what they were told about it.

Long discussions are read a page at a time. `GET /v1/packs/{id}/threads` and
`GET /v1/threads/{id}` take `?limit=` and answer with a `Link` naming the next
page, keyset-style like the rest of the mirror's listings: for the list the
cursor is `(created_at, id)`, for a discussion it is the last comment's id.
Without `limit` both answer whole, as they did before they could be paged. The
thread itself rides with every page of its comments, so following the `Link`
never leaves a reader holding half an answer.

Writing has a ceiling and a stop. The ceiling is a rate window counted from the
rows themselves -- twenty comments or five threads per ten minutes per account,
set where a person never notices it and a script does, and a restart hands
nobody a fresh allowance. The stop is a block: `POST .../packs/{id}/blocks`
(`{github_uid, reason?}`) refuses that person's next report, proposal or comment
on this pack, `DELETE .../packs/{id}/blocks/{uid}` lifts it, and
`GET .../packs/{id}/blocks` lists who is on it -- all at `edit`, because
blocking and hiding a comment are the same job. A block never touches reading,
so it cannot be used to erase somebody from a record they are already in, and
the gate refuses to block anybody who keeps the pack. Both decisions are audit
entries.

A pack's block cannot answer everything. Somebody whose *pack* was the offence
is not answered by being kept out of one discussion, so the mirror's operators
have their own stop: `POST /v1/users/{uid}/suspension` (`{reason?}`) bars that
account from putting anything on the mirror -- authoring or forking a pack,
uploading a jar, opening a thread, saying anything on one -- and
`DELETE /v1/users/{uid}/suspension` lifts it. It is enforced in the access gate
itself (at `edit` and above) rather than at each of the forty writes behind it,
and it touches reading nowhere: a suspension is not a way to unpublish what
somebody already made. An operator cannot be suspended; take the rung away
first if that is really the intent. The account sees it on `/v1/me`, so the
panel says so in a standing bar rather than by refusing a control that looked
usable, and both decisions are audit entries.

The reason is for the person blocked or suspended. A refused write answers `403` with it in
the message, and `GET .../access/mine` carries it as `suspended` so the panel
can say so instead of offering a reply box that cannot work. Write it as
something worth reading: it is what they will see. It is stored as one line --
whitespace collapses -- and bounded at 300 characters, because it rides in every
refusal that account gets and sits in a row of a list; the long version of an
argument belongs on the thread.

### Proposing a change to somebody else's pack

A fork can be offered back. `POST .../packs/{target}/proposals`
(`{source_pack, source_commit?, message}`) opens the request: the proposer needs
`edit` on the fork they are offering and `view` on the target -- or nothing at
all beyond a session when the target is published, which is what makes an
unsolicited proposal possible.

What is offered is a **commit**, not "whatever that fork says today", so what a
reviewer reads cannot move while they read it. `GET /v1/threads/{id}/diff`
answers what taking it would do to the target **as it stands now** rather than
against the fork's parent: a review answers "what happens to my pack if I take
this", and that question moves as the pack moves. It is as readable as the
thread, which means offering a commit publishes its content to the target's
readers -- a fork that is not ready to be read is not ready to be proposed.

`POST .../threads/{id}/merge` (needs `edit` on the target) writes the offered
authored content in as an ordinary commit, so what a merge did is readable
afterwards by the same history everything else uses. Ownership does not travel:
the pack id, owner, tier, visibility and `fork_of` stay the target's, and a
proposal that could move them would be a rename away from a takeover.
`POST .../threads/{id}/close` says no; the proposer hitting the same endpoint
withdraws instead. Settled proposals keep their row -- "we said no in
March" is what somebody looks for in April.

Two reviewers pressing merge at once get one decision and one commit. What
makes that true is the pack lock and a re-read of the thread inside it, not the
one-time settling write on its own: the settle is the last thing a merge does,
so a merge that trusted it alone had already written the config and the commit
by the time it learned the decision was made. The loser is refused before
writing anything.

### History

A commit is a snapshot, an author, a message and a parent, content-addressed
and append-only (#122). In the panel it is two surfaces on purpose: declaring a
checkpoint sits beside the build button, because a build is made from a commit
and the same sentence serves both acts, while the list of past checkpoints opens
as a dock over whatever tab is up -- it is consulted while working rather than
worked in. What it is worth doing with:

- **Read one**: `GET .../commits/{id}` (metadata, plus the versions built from
  it) and `GET .../commits/{id}/diff` -- what the commit recorded, against its
  parent. `?against=<id>` compares two checkpoints; `?against=live` asks what a
  restore of it would do to the working state, which is what the panel shows
  before it asks. In the panel a commit has an address of its own
  (`/packs/<id>/commit/<sha>`).
- **Build an old one**: `?from_commit=<id>` on the build, or "build this" in
  the history. It skips the uncommitted check entirely -- the state being built
  is the commit, whatever is in the editor.
- **Put one back**: restore writes it forward as a new commit rather than
  rewinding, so nothing that was ever declared stops being true. Reverting to a
  published build (`config/revert`) does the same and records its own
  checkpoint.

A build made from a checkpoint records it (`built_from` on the manifest and in
the versions listing), which is what makes "which state is 0.1.31" a question
with an answer. A CLI build names none -- it builds the working state -- and
neither does a dry run.

The log itself is paged: `GET .../commits?limit=` answers a hundred by default,
`Link` names the next page, and the cursor is the last commit served, so the
walk continues at its parent.

### Two people in one pack

`GET .../config` answers with an `ETag` -- a revision of the config's authored
content. A save that sends it back as `If-Match` is applied only while the
stored config still matches; otherwise it is refused with 409 and nothing is
written. The panel saves this way, so a second editor is told their base is
stale instead of overwriting the first, and offers the two ways out: take the
stored version, or save over it (which re-reads the current revision and
writes against that, so it stays a conditional write).

The revision covers what a client authors and nothing else -- publishing a
pack, or depfill appending a pulled dependency, does not invalidate an edit
someone has in flight. A request that sends no `If-Match` writes
unconditionally, which is what the CLI and any script does.

## Harvest

Runs after every real build and cache upload (poked), or on demand:
`POST /v1/registry/harvest` (admin; returns `{running, last_report}`),
`GET /v1/registry/harvest/status`, or `smrt-pack registry harvest`. A run
re-reads every cached jar, reconciles Modrinth identity (sha1 lookups, env
flags incl. a backfill for aliases that predate the env columns, declared
deps, slug==modid folds, one-time modid learning for re-uploads), and
rewrites the derived registry layers. Idempotent; authored rows survive.

Modrinth outages degrade, not break: each metadata call has a hard deadline,
the filtered version listing falls back to unfiltered, 429s are absorbed
once, and failed enrichment legs log a warning and skip. Re-run the harvest
after the outage; everything self-heals.

## Registry curation

The panel's Mods section is the curation surface:

- **Identity** -- assign an unidentified cached jar to a mod (new or
  existing) with version/channel/loaders/MC (`authored` source, precious).
- **Classification** -- the Debug-gated escape hatch for jars whose
  side/policy the automatics get wrong: panel, `PUT
  /v1/registry/files/{sha1}/class`, or `smrt-pack registry classify --sha1
  ... --side ... --policy ...`. Refused for Modrinth-identified mods (their
  env flags win) and for the inconsistent client+must_match pair.
- **Relations** -- authored dependency edges override derived ones
  (e.g. downgrading a false inferred hard edge to optional).
- **Repack provenance** -- a file whose sha1 Modrinth confirms shows
  `Modrinth`; a self-hosted sibling under the same mod shows `repack?` with a
  by-request class-level diff. Nothing is auto-merged or hidden.
- **Takedown** -- removes the jar from serving and records the sha1 in
  `removed.txt`, which also blocks re-ingestion. Manifests referencing it
  must be rebuilt; the moderation policy is to self-host only genuine
  archival jars in the first place.

## Failure modes worth knowing

- **Modrinth partial outages** are the common weather: some endpoints answer,
  others hang or 500. Depfill may pull nothing (config stays as-is), env
  backfills lag, icons vanish from the panel until the CDN returns. All of it
  converges on the next healthy harvest + build.
- **Builds wait out the harvest**: a build started while a harvest is running
  (or poked and pending) waits for it to settle before classifying, capped at
  five minutes -- past the cap it proceeds against current state with a log
  line, so a busy harvester can never starve builds.
- **Restart mid-build**: the job dies with the process; its snapshot reads
  failed/interrupted after the restart, and the pack lock dies with it too.
  Re-trigger the build; manifests are only written complete.
