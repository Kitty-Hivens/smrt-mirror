# Public API guide

What a client author needs beyond the endpoint reference. The exact paths,
parameters and response schemas live at `/docs` (Scalar over
`/openapi.json`) on any running mirror; this page carries the semantics that
a schema cannot: flows, ordering rules, compatibility promises.

The reference deployment lives at `https://smrt.hivens.dev`; substitute your
own origin throughout. All public reads are anonymous.

## Schema versioning and forward compatibility

Wire objects carry `schema_version` (currently **2**). Clients must reject a
manifest whose major schema they do not know, and must **ignore unknown
fields** everywhere: the mirror adds optional fields without bumping the
version. Only a wire-incompatible change bumps it.

Practical consequences:

- decode tolerantly (unknown fields, unknown `source` variants, unknown
  `display.presence` values -> no badge, not an error);
- never rely on field order or on absent-vs-null distinctions beyond what the
  schema states;
- treat additive fields as optional forever (a manifest built before a field
  landed simply lacks it).

## The launcher update flow

```
GET /v1/packs                              # catalog: official published packs
GET /v1/packs/{id}                         # one summary (also: latest_* fields)
GET /v1/packs/{id}/manifest/versions       # every retained build, newest first
GET /v1/packs/{id}/manifest                # the latest build
GET /v1/packs/{id}/manifest/{version}      # a specific build
```

1. **Catalog**: `latest_pack_version` on each summary is the current pointer.
   `latest_built_at` (RFC 3339) and `latest_channel` are derived by the mirror
   from the latest manifest at read time -- render "updated X ago" and the
   channel badge from these; absence means the pack has no readable build.
2. **Did anything change?** Compare `latest_pack_version` by plain string
   equality against the installed version. For content-level change detection
   use the manifest `fingerprint`: identical fingerprint = identical
   instance, whatever the labels say.
3. **Version picker**: the versions listing's `builds[]` is newest-first by
   `date_published` and follows the Modrinth version-object naming
   (`version_number`, `version_type`, `date_published`, `changelog` --
   curator-authored release notes where given -- plus `fingerprint`,
   `mods_count`, `assets_count`). A build made from a checkpoint also carries
   `built_from`, the commit id its config came from -- absent on a CLI build,
   which builds the working config, and on anything built before a pack had
   history. `latest` names the build the latest pointer serves. Filter by
   `version_type` to hide prereleases.
   Each build also states what it targets and what it costs:
   `minecraft_version` and `loader` (`{name, version}`), so "this update moves
   you to 1.20.1" is answerable from the listing rather than by fetching every
   manifest in it, and `size_bytes`, the sum of every mod and asset the build
   lists -- optional entries included, since what one player fetches depends on
   what they enable.
4. **Ordering**, when a client must sort labels itself: numeric tuple
   comparison within a version base (`0.4.10` > `0.4.2`; lexicographic sort
   is wrong); across bases or historical labels, order by `date_published`.

## What the mirror knew about a build

A manifest may carry `checks`: what the pre-publish check found when the build
was published. `blocking` lists findings that mean the pack cannot start (a
declared hard dependency nothing satisfies, an artifact no loader present can
run, a version -- of a mod or of the loader itself -- outside a window something
declared) -- it
is non-empty only when a curator published over them, and `overridden` then says
so. `advisory` lists what was recorded rather than enforced: active conflicts,
jars the registry could not identify.

Advisory to a client in the strict sense -- nothing here changes what gets
installed, and the block is absent when the check found nothing to say. It is
worth surfacing to whoever is installing: a build that says it will not start
usually will not.

## The update dialog: what changed

```
GET /v1/packs/{id}/diff?from={installed}&to={target}   # to defaults to latest
```

The structured change summary between two builds -- what an update dialog
renders instead of guessing from file lists: `loader` / `minecraft` / `java`
bumps (absent when unchanged), `mods_added` / `mods_removed` /
`mods_updated` (matched by stable identity -- Modrinth project, curator
slug, filename -- so a re-pin that renames the jar reads as an update, with
version labels enriched from the registry where known), `mods_toggled`
(install-default flips), and the asset equivalents. `content_changed: false`
means the two builds share a fingerprint and the diff is a relabel. The diff
is the "what"; the curator's `changelog` on each build (manifest and versions
listing) is the "why" -- render both when present.

Release notes may also carry `changelog_i18n`, the same text keyed by language
tag (`{"en": "...", "ru": "..."}`). Prefer the user's language from that map;
fall back to `changelog`, which is always the note in one of the languages the
map holds and is what a client that ignores the map has always read. A tag the
curator left blank is absent from the map rather than present and empty, so a
missing key means "not written", never "written as nothing".

## Downloading an instance

For each `mods[]` / `assets[]` entry, dispatch on `source.type`:

- **`modrinth`**: resolve the actual file via Modrinth
  (`/v2/project/{project_id}/version/{version_id}`), pick the file with
  `primary: true` (fall back to `files[0]` only when nothing is marked
  primary -- Modrinth versions often ship sources/deobf jars alongside the
  installable one). Verify the downloaded bytes against the manifest's
  `sha1`; the manifest, not Modrinth, is the contract.
- **`smrt_cache`**: `source.url` points at
  `/v1/cache/{xx}/{sha1}.jar` on the mirror. Content-addressed and immutable;
  cache aggressively, dedup across packs by sha1.
- **`smrt_static`**: `source.url` points under
  `/v1/packs/{id}/static/...`. Not content-addressed; re-fetch per version
  and verify the manifest's `sha1`.

**Auth precondition**: the optional root-level `auth` block
(`{"kind": "smartycraft", "server_id": "Create"}`) names the provider the
user must be signed in with before the game spawns, and for
SmartyCraft-bound content the SC game-server id the join and session bind
to. Absent = no precondition. `kind` is an open vocabulary (`smartycraft`,
`microsoft`, `both` today); treat unknown kinds as advisory.

Install flags: `required` is enforcing (never offer a toggle); for optional
entries `default_enabled` (absent = true) is the install-time default. The
`display` block is advisory UX metadata -- names, descriptions, icons, the
`requires` tree for co-toggling, `presence` for the side badge.

**Toggle identity** across version bumps: key an optional mod's on/off state
by its Modrinth `project_id` when the source is Modrinth, else by the entry's
`slug` field when present, else by `filename`.

**Resuming a download**: files served by the mirror (`/v1/cache/...`,
`/v1/packs/{id}/static/...`) answer `Range`, so a transfer that died at 90%
asks for the rest instead of starting over. Send `Range: bytes=<got>-` and
expect `206` with `Content-Range`; a range past the end answers `416`.

Resuming safely: a cache jar is content-addressed, so its bytes cannot change
under a resume. A static file can, and `If-Range` is **not** honoured -- pair
the range with `If-Unmodified-Since: <the Last-Modified you started from>` and
a file that moved answers `412` instead of splicing two versions together.
Verify the manifest's `sha1` after assembling either way.

## Reading cheaply

Three things the whole `/v1` surface does, worth wiring into a client once:

- **Compression.** Send `Accept-Encoding: gzip` (or `br`). Manifests and
  listings are repetitive JSON and compress several-fold -- a manifest-shaped
  body lands around a seventh of its size. Jars, zips and live event streams
  are served uncompressed on purpose.
- **Conditional GET.** A JSON read carries an `ETag`. Send it back as
  `If-None-Match` and an unchanged answer costs `304 Not Modified` with no
  body -- which is the cheap way to poll. The tag is weak: it identifies the
  data, not the encoding it arrived in. Tagged means a `200` whose body was
  built whole; a streamed response, and anything past 8 MiB, is answered
  untagged rather than hashed on every request, so treat a missing `ETag` as
  "cannot revalidate this one" rather than as an error.
- **Paging.** Listings that grow without bound accept `?limit=<n>` (capped at
  500): `/v1/cache/inventory`, a pack's discussions and one discussion's
  comments, and the gated `/v1/registry/mods`, `/v1/audit`, a pack's commit log
  and your own notifications.
  Without it they answer whole, as they always have. With it, the response
  carries `Link: <...>; rel="next"` when there is more; follow that URL
  verbatim -- it repeats your filters and carries an opaque cursor. The body
  shape does not change between the paged and unpaged forms. Paging is keyset,
  not offset: rows arriving while you walk land outside the page you are
  reading rather than shifting it. A cursor pointing at a row that has since
  been merged away ends the walk; start over rather than trusting a stale one.

## Mods, files, hashes

```
GET /v1/mods/{key}      # key = numeric id | sha1:<hash> | slug
GET /v1/files/{sha1}    # hash-first: file + its release + owning mod
```

The mod page model carries identity (name, slug, modid, Modrinth project id),
the project environment flags (`client_side`/`server_side`, Modrinth
vocabulary, absent for mods without a Modrinth identity), releases with files
(loaders, MC versions, `cached` = the mirror holds the bytes), dependency
edges, and which public packs ship it. `/v1/files/{sha1}` is the Modrinth
`version_file/{hash}` analog: identify an arbitrary jar in one call.

## Icons and images

- `/v1/cache/icon/{sha1}` -- the icon embedded in a cached jar (mcmod.info
  logoFile, mods.toml logoFile, fabric icon, or a conventional root png).
  Immutable-cacheable; 404 when the jar carries none.
- Modrinth-sourced entries may carry `display.icon_url`; when absent, clients
  can resolve the project icon themselves -- and should fall back to the
  jar-embedded icon by sha1 when Modrinth is unreachable (the mirror caches
  the jars either way).
- `/v1/modrinth/icon/{project_id}` -- the icon of a Modrinth project the mirror
  indexes, fetched once and served from here. The mirror does not rehost
  Modrinth's jars, but a project icon is a few kilobytes of picture rather than
  the mod, and serving it means a page does not fetch images off someone else's
  CDN. Refreshed monthly; 404 when the project has no icon, or has one in a kind
  the mirror does not serve back.
- `/v1/users/{uid}/avatar` -- GitHub avatars proxied through the mirror, so a
  page never hands viewer IPs to a third party.

## Servers, featured, community

`/v1/servers` and `/v1/featured` are curated editorial surfaces for the
launcher's home screen. `/v1/community` lists published community packs (with
an owner byline) -- browseable, but never part of the official catalog at
`/v1/packs`.

## What is being asked of a pack

`GET /v1/packs/{id}/threads` lists what people have said about a pack: reports
(`kind: "issue"`) and offers from forks (`kind: "proposal"`), open ones by
default, `?all=true` for the settled, `?kind=` to narrow.
`GET /v1/threads/{id}` is one of them in full, with its comments;
`GET /v1/threads/{id}/diff` is what taking a proposal would do to the pack as it
stands now, in the same change rows the panel draws.

The list and the discussion take `?limit=` and name the next page in a `Link`
header, the same keyset paging as `/v1/cache/inventory` and the audit log; the
thread rides with every page of its comments, so a paging client never holds
half an answer. Without `limit` both answer whole.

All three are anonymous reads for a published or unlisted pack, and answer `404`
for a draft that is not yours -- a decision nobody can see is indistinguishable
from one nobody made. A moderated comment keeps its place in the numbering and
loses its body: `hidden: true` with no `body`. Writing (opening a thread,
commenting, deciding) is a session surface and lives under `/v1/authoring/`.

## Authenticated surfaces

Not needed by a launcher, listed for completeness: GitHub OAuth session
(`/v1/auth/github/login` -> callback -> cookie; `/v1/me`), member endpoints
(`/v1/me/...`: own packs, uploads, forks), admin authoring
(`/v1/authoring/...`, `/v1/registry/...`; bearer `SMRT_ADMIN_TOKEN` or an
allowlisted OAuth session), and a debug rung above admin for compat-affecting
registry writes. Job endpoints (`/v1/jobs/{id}`, `/v1/jobs/{id}/events` --
SSE) track builds; both are gated on the pack the job is about, at the same
`view` level as reading that pack, because a build log names the pack, its mods
and whatever the pre-publish check refused. Finished jobs keep answering the
status endpoint from persisted snapshots across restarts (a job running at a
restart reads failed, with an explicit interrupted line), while the live SSE
tail is memory-only.

A pack's history is paged the same way as the listings above:
`GET /v1/authoring/packs/{id}/commits?limit=` answers a hundred commits by
default and names the next page in `Link`, the cursor being the last commit
served -- the walk continues at that commit's parent.

`GET /v1/events` (SSE, any signed-in caller) is the mirror-wide equivalent:
what changed, as it changes, so a view listens instead of asking again on a
timer. Four event names -- `registry` (the mod index moved: a harvest ran, a
jar was named, two mods merged), `pack` (a build published, a pack deleted or
changed visibility, a discussion moved), `catalog` (the curated surfaces around
the packs: the server list, the featured selection) and `moderation` (the upload
queue moved, operators only). Each carries a small JSON body saying which, and
is a nudge rather than the data: refetch the one view that cares, and that read
is the conditional GET above, usually answered `304`. In-process, so events are
live-only -- a reconnecting client reads the world as it is and listens from
there.
