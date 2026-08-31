# Architecture

smrt is a self-hostable mod mirror and pack registry. It answers one question
end to end: *given a pack id, what exactly lands in a client instance, and
where does every byte come from?* Everything else -- the registry, the panel,
the harvest -- exists to keep that answer correct without a human re-deriving
it. Launchers are clients of the HTTP API; the reference deployment serves
the Nexira launcher, and the instance-archive importer exists because that is
where the first packs came from, but no component assumes either.

## Components

One repository, two binaries, one SPA:

- **`smrt`** (the service) -- axum HTTP server. Serves the public read API
  (catalog, manifests, mod pages, cached jars), the member/admin authoring
  API, the OpenAPI reference at `/docs`, and the control panel itself. Owns
  the background jobs (builds, harvest scheduling). Deployed as a single
  static binary behind nginx; systemd unit in `deploy/`.
- **`smrt-pack`** (the CLI) -- the same authoring code paths, callable from a
  shell on the box or a workstation: `bootstrap` (seed a config from an SC
  archive), `build`, `depfill`, enrichment passes, `registry` maintenance
  (harvest, stats, classify, conflicts, orphans). Ships to the VPS alongside
  the service so it can never drift behind the registry schema.
- **Panel** (`web/`) -- Svelte 5 SPA served by the service. Curator UI over
  the same HTTP API: pack editor, resolve report, build console, registry
  browser (mods -> releases -> files), moderation and audit views. Talks to
  the API with generated TypeScript bindings, so wire drift is a compile
  error.

Shared by all three:

- **Storage** (`src/storage.rs`) -- the on-disk layout under one root. Plain
  files, atomic writes, no daemon-private state: everything the mirror serves
  is inspectable with `ls` and `cat`.
- **Registry** (`src/registry/`) -- SQLite (WAL) database of mod identity:
  mods, releases, files, aliases, relations, per-jar classification. The
  *decision layer* for side/required derivation reads from here.
- **Authoring** (`src/authoring/`) -- the pipeline: config -> enrichment ->
  classification -> source resolution -> manifest. Plus the harvest (jar
  scanning + Modrinth reconciliation) and depfill (dependency auto-pull).

## Storage tree

```
/var/lib/smrt/
  registry.db                    # the mod-identity registry (SQLite, WAL)
  accounts.db                    # accounts, sessions, grants, threads, audit (SQLite)
  removed.txt                    # takedown list: sha1s that must never serve again
  jobs/<id>.json                 # job snapshots (status + log; newest 200 kept)
  featured.json                  # editorial: featured packs/servers
  servers/<id>.json              # curated server metadata
  meta/<name>.json               # cached upstream lists (Minecraft + loader versions)
  icons/<xx>/<sha1>.<ext>        # extracted mod icons, and the negative marker
  icons/modrinth/...             # proxied project icons, cached by project id
  uploads/<sha1>.jar             # member uploads awaiting moderation (staged, not served)
  cache/<xx>/<sha1>.jar          # content-addressed jar cache (xx = first two hex)
  packs/<PackId>/
    summary.json                 # the catalog card (built)
    authoring/config.json        # the curator's declaration (source of truth)
    authoring/commits/<id>.json  # a declared checkpoint: author, message, parent
    authoring/commits/<id>.snapshot.json  # the config as it stood
    authoring/commits/HEAD       # the newest commit's id
    manifests/<version>.json     # frozen builds
    manifests/latest             # symlink -> the current build
    static/...                   # mirror-hosted pack files (configs, resource packs)
  packs/u/<uid>/<PackId>/...     # community packs, same shape, namespaced by owner
```

Two files matter more than the rest: `authoring/config.json` is what a human
edits (directly or through the panel), and `manifests/<version>.json` is what
a launcher consumes. Everything between them is derived and rebuildable.

The commits beside the config are neither: a snapshot is the only record of a
state somebody declared worth keeping, and nothing else can reconstruct it once
the config has moved on. History is linear and append-only -- a restore writes
an old state forward as a new commit rather than rewinding -- so a build that
names the checkpoint it came from keeps naming the same state forever.

## Data flows

### Authoring -> publish

```
config.json --(save/PUT)--> depfill (pull missing hard deps from Modrinth/cache)
config.json --(commit)----> snapshot + HEAD (what a build is made from)
snapshot    --(build)-----> enrichment (mcmod display, inferred requires)
                            classification (registry decision layer: side/policy)
                            pre-publish check (resolve against the graph)
                            source resolution (Modrinth lookups, cache reads)
                            derive_required (seeds + hard-dep walk + invariants)
                            manifest <version>.json + summary.json + latest
```

A build is a *pure function of the config and the registry* plus network
lookups; it writes nothing until the manifest is complete. Real builds of the
same pack are serialized; dry runs (`?dry_run=true`) resolve everything and
publish nothing.

The pre-publish check (`authoring/gate.rs`) judges the resolve report. What
stops a publish is what means the pack cannot start: a declared hard dependency
nothing satisfies (a bytecode-inferred one is recorded, not enforced); an
artifact built for a loader the pack does not run with nothing present to bridge
it; a required mixin whose target the pack's own copy of the host no longer
carries; a dependency shipped outside the version window its requirer declared;
and the pinned loader build sitting outside the window a jar declares on it.
That last one needs the jar's own manifest, which for a Modrinth pin is not on
this disk, so it comes from a pass of its own (`authoring/loaderreq.rs`) that
reads each artifact once by HTTP range and remembers what it read. The rest --
active conflicts, jars the registry cannot identify -- are
recorded on the built manifest under `checks` rather than enforced. A publish
over a blocking finding needs `?override_checks=true` (`--force` on the CLI) and
is recorded three ways: the job log, the audit trail (`build.override_checks`,
with the actor), and `checks.overridden` on the manifest itself. A dry run
reports the same verdict and is never stopped by it.

### Two people in one pack

A pack being edited has a document (`authoring/packdoc.rs`): the config's own
JSON shape as a CRDT, with a short list of paths marking which strings are prose.
Editors join it empty and apply the mirror's state -- seeding a client from the
config and then applying that state would author a second value for every key,
and one whole `mods` array would silently replace the other.

Updates arrive at `POST /v1/authoring/packs/{id}/doc`, are merged, fanned out to
the pack's room (#113) and written to `config.json` once the typing stops. The
write goes through the same path a whole-config `PUT` uses, so both doors reach
one act; a `PUT` or a revert forgets the document first, since it would otherwise
put back what it still remembers. Server-owned fields never enter the document.

The document is a merge point, not a second source of truth: it is rebuilt from
the stored config whenever nobody holds it, so a restart costs the history of how
the content got there and none of the content.

### Harvest cycle

After every real build or cache upload the harvest scheduler is poked (it can
also be forced via `POST /v1/registry/harvest`). A harvest run:

1. reads every cached jar in one pass each (metadata files, bytecode graph,
   icons are extracted on demand elsewhere);
2. reconciles with Modrinth: sha1 -> version lookups, project env flags,
   declared dependencies, identity folds (slug == modid), one-time modid
   learning for re-uploads;
3. rewrites the derived registry layers (packages, inferred + modrinth
   relations, jar classifications) in one transaction. Authored/curator rows
   are precious and never clobbered.

The cycle build -> harvest -> build converges: a build downloads new jars into
the cache, the harvest learns what they are, the next build classifies them.

### Launcher update flow

Covered normatively in [api.md](api.md); in one line: catalog -> pack summary
(`latest_pack_version` / `latest_channel` / `latest_built_at`) -> versions
listing -> manifest -> per-entry download by source -> sha1 verify.

## Trust and roles

Reads are anonymous. Writes are tiered: **Member** (GitHub OAuth; owns
community packs), **Admin** (operator allowlist by GitHub uid, or the
`SMRT_ADMIN_TOKEN` bearer for headless use), **Debug** (a separate token/uid
rung above admin gating compat-affecting registry writes, e.g. authored jar
classification). Frontend role checks are rank-aware; the backend enforces
regardless.

## Design invariants

- **One home per concern.** Instance content is declared in the config,
  identity lives in the registry, presentation hints ride the manifest's
  `display` block. No fact is stored twice on purpose; read-time derivation
  is preferred over duplicated persisted state (e.g. a summary's
  `latest_built_at` is read from the latest manifest, never written).
- **Derived layers are rebuildable.** Everything the harvest writes can be
  wiped and re-derived from jars + Modrinth. Authored rows are the exception
  and are defended in SQL (`WHERE source NOT IN ('curator','authored')`).
- **The client invariant.** A client-side mod is never `required` in a built
  manifest. The build refuses to produce one (see
  [concepts.md](concepts.md)).
- **Self-contained serving.** Everything the mirror itself puts on a page comes
  from the mirror's own origin: the panel, the docs page, mod icons, GitHub
  avatars. Where the source is somebody else's, the fetch happens server-side,
  where it is cacheable and attributable and the reader's address never leaves
  the mirror.

  With one gap, stated because it is a gap and not a design: a pack card's
  `icon_url`, `banner_url` and `gallery_urls`, and any image inside
  `description_md`, are authored strings that the panel renders as they are.
  Operator packs point them at the pack's own `static/` tree, which keeps the
  invariant; a community pack's author can point them anywhere, and then every
  visitor to the public catalog fetches that host directly and tells it who
  they are. Nothing about it is enforced today -- no validation on the write,
  no proxy on the read, and no CSP on the panel that would refuse a foreign
  image. Closing it means either rejecting off-mirror URLs (which takes away
  something authors currently do) or proxying them (which makes the mirror a
  fetcher of arbitrary URLs, with the SSRF surface that implies), and neither
  has been decided.
- **Takedown-safe.** `removed.txt` blocks a sha1 from serving and from
  re-ingestion, even if the bytes are still on disk.
