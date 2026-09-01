# Development

## Layout

```
src/
  main.rs, lib.rs, config.rs, state.rs, storage.rs, jobs.rs
  domain/      # wire types: manifest, pack, version+channel, side, server
  http/        # axum routers: public, panel, auth, member, jobs, registry (admin), apidoc
  registry/    # SQLite: migrations, schema/, queries, upsert, classify, authored, model
  authoring/   # pipeline: harvest, build, resolve, depfill, modmeta, bytecode,
               # classfile, curator, sources, modrinth client, bootstrap, jardiff
  accounts/    # sessions + roles (GitHub OAuth)
  bin/smrt-pack.rs
web/           # Svelte 5 panel; src/lib/bindings/ are generated
docs/          # this documentation
deploy/        # systemd unit, nginx conf, emergency deploy script
testdata/      # side-label corpus definitions + fetch/baseline tooling
examples/      # dev probes (parse_bench, evidence_dump, meta_probe)
```

## Gates

Everything below must pass before a change is done. This is the set CI runs,
verbatim, and `main` auto-deploys, so a red gate is a broken deploy:

```
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
TS_RS_EXPORT_DIR=web/src/lib cargo test --all-features
cd web
pnpm install --frozen-lockfile
pnpm check      # svelte-check
pnpm checks     # the panel's own logic suite (scripts/panel-checks.mjs)
pnpm build
```

pnpm, not npm: the lockfile is `web/pnpm-lock.yaml` and there is no
`package-lock.json` for npm to honour, so an `npm install` here resolves a
different tree than the one CI builds.

## Versioning

There is one version, and git is its source: `build.rs` stamps
`SMRT_BUILD_VERSION` as `<year of the HEAD commit>.<commit height>` --
`2026.388`. It is what `/v1/health` returns, what the panel footer shows,
and what `smrt-pack --version` prints. The `Cargo.toml` number is inert;
don't bump it and don't read it.

The height comes from `git rev-list --count HEAD`, so a build needs the full
history: CI checks out with `fetch-depth: 0`. A shallow clone (or no git at
all) stamps `unknown` rather than a small number that would pass for a real
version.

## TypeScript bindings

Wire structs derive `ts_rs::TS`; `cargo test` (with `TS_RS_EXPORT_DIR`)
regenerates `web/src/lib/bindings/*.ts`. The bindings are committed --
a wire change that breaks the panel fails `pnpm check` / `pnpm build`
instead of failing at runtime. Add `#[derive(TS)]` + `#[ts(export, ...)]` to
any new wire type, and `utoipa::ToSchema` if it appears in a documented
response.

## OpenAPI

`/openapi.json` is generated from `#[utoipa::path]` annotations on handlers
plus the `components(schemas(...))` list in `src/http/apidoc.rs`. A new
public endpoint is not done until it is annotated and registered there --
the docs page is the contract surface, and unannotated endpoints are
invisible to client authors.

## Registry migrations

Hand-rolled and numbered (`src/registry/migrations.rs`), tracked in
`registry_meta.schema_version`, applied at service start. Plain steps are SQL
files under `registry/schema/`; a step that must inspect state or toggle
`foreign_keys` for a table rebuild is a `Code` step and MUST be idempotent
(it re-runs if a later step fails before the version records).

Hard-won rules:

- **grep the schema for CHECK constraints before changing any stored
  vocabulary** -- a new enum value that the constraint does not admit kills
  every write path that touches the table;
- SQLite cannot alter a CHECK: rebuild the table (create new -> copy with
  explicit ids -> drop -> rename -> recreate indexes) with `PRAGMA
  foreign_keys = OFF` around it and a `foreign_key_check` after;
- migrations run on the deployed box at restart -- test them against a copy
  of the production database, not only against fresh in-memory schemas.

## The corpus harness

The side/policy classifier is guarded by a labelled corpus
(`testdata/side-labels.toml`, jars fetched by `testdata/corpus/fetch.py`):

```
SMRT_CORPUS_DIR=<dir> cargo test --test corpus_classify -- --ignored
```

Acceptance bars: zero client-labelled `must_match` verdicts; >=90% agreement
for the full cascade and for bytecode-only. Re-run it (plus
`testdata/corpus/baseline.py` for the diff) before touching classifier
signals, edge grading, or metadata extraction; `docs/side-required-audit.md`
records the accepted baseline and why each disagreement is tolerated.

## Driving the panel headless

`web/scripts/` holds eight scripts that open the panel in a real browser: the
screenshot sweeps (`shot`, `branding-shot`, `structured-shot`, `picker-shot`,
`preview-shot`) and the two end-to-end checks (`pack-editor-check` drives the
whole authoring loop to a published build; `upload-check` drives a cache-jar
upload). They share `scripts/lib/harness.mjs`, which is the only place that
knows how to launch a browser and sign in -- six of them used to carry their own
copy, and five of those still typed a token into a form that has answered `410`
since sign-in became GitHub OAuth.

They are run by hand, not by `pnpm`, and they need a browser and a session:

```
cd web
FIREFOX=/usr/bin/firefox BASE=http://127.0.0.1:9000 SESSION=<cookie> OUT=/tmp/shots \
  node scripts/shot.mjs
```

`SESSION` is the `smrt_session` cookie: sign in to the panel in a real browser
and copy it out of DevTools (Application > Cookies -- it is HttpOnly, so
`document.cookie` will not show it). Against a throwaway local mirror, minting
one directly is quicker and needs no OAuth app:

```
sqlite3 <storage>/accounts.db "
  INSERT INTO users (github_uid, login, role, created_at, last_login_at)
  VALUES (1, 'local-operator', 'admin', strftime('%s','now'), strftime('%s','now'));
  INSERT INTO sessions (id, user_id, created_at, expires_at)
  VALUES ('localdev', last_insert_rowid(), strftime('%s','now'), strftime('%s','now') + 86400);
  INSERT INTO terms_acceptance (github_uid, accepted_at) VALUES (1, strftime('%s','now'));"
```

Then `SESSION=localdev`. Never on a mirror anyone else uses: it is a session
nobody signed in for.

The scripts drive the panel in English (the harness sets `smrt.locale` before
the app loads) so the labels they click are deterministic. The locale switch is
on the login page and in Settings; there is none in the shell, which is what a
script clicking for one used to miss silently -- and then navigate a Russian
rail by English labels, screenshotting whatever happened to be on screen.

## Local run against real data

```
SMRT_STORAGE_DIR=<replica of /var/lib/smrt> SMRT_BIND_ADDR=127.0.0.1:9777 \
  cargo run --bin smrt
```

A storage replica (rsync of the production tree) gives the full panel +
registry experience locally. `smrt-pack reconstruct-config` can rebuild a
missing `authoring/config.json` from a published manifest. Dev probes:
`cargo run --example meta_probe -- <jar>` dumps what the metadata extractor
sees in one jar; `evidence_dump` does the same for the bytecode classifier.
