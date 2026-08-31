# smrt

A self-hostable Minecraft mod mirror and pack registry.

One binary serves per-pack manifests (every mod and asset with an explicit
source and verified hash), a Modrinth-shaped mod registry over everything it
caches, curated server metadata, and self-hosted jars for mods unreachable
through Modrinth -- plus the control panel to curate all of it. Launchers are
clients of the HTTP API; the reference deployment backs the
[Nexira](https://github.com/Kitty-Hivens/Nexira) launcher for the SmartyCraft
server family, but nothing in the mirror is tied to either -- point it at
your own packs and run your own.

What it does today:

- **Pack authoring and publishing** -- a curator declares a pack once
  (Modrinth / mirror-cache / static sources per entry); the mirror resolves,
  classifies, derives required-ness, and publishes frozen, reproducible
  manifests with Modrinth-style versioning (plain `base.counter` numbers,
  stored release/beta/alpha channel).
- **A mod-identity registry** -- Modrinth-shaped (mod -> release -> file)
  over everything cached, filled by an automated harvest: jar metadata,
  bytecode analysis, Modrinth reconciliation, dependency graphs, side/policy
  classification with a hard client-safety invariant.
- **A control panel** -- Svelte SPA served by the mirror itself: pack editor
  with dependency auto-fill and resolve reports, build console, registry
  browser and curation, moderation and audit. GitHub OAuth with tiered roles.
- **A dependency-aware wire format** -- per-entry display metadata, requires
  graph, presence classes, content fingerprints; documented live at `/docs`.

## Documentation

- [docs/](docs/README.md) -- architecture, concepts, the public API guide,
  operations, development. Start with
  [docs/architecture.md](docs/architecture.md).
- `/docs` on a running mirror -- the generated endpoint reference
  (Scalar over `/openapi.json`).

## Building

Requires Rust 1.85+ (edition 2024), and Node with pnpm for the panel.

The panel is built first: the release binary embeds `web/dist`, so building
cargo first would bake whatever was there before (or nothing).

```
cd web && pnpm install --frozen-lockfile && pnpm build && cd ..
cargo build --release --bin smrt --bin smrt-pack
```

## Running locally

```
SMRT_ADMIN_TOKEN=$(openssl rand -base64 32) cargo run --bin smrt
curl http://127.0.0.1:9000/v1/health
```

The full environment reference is in
[docs/operations.md](docs/operations.md).

## Deployment

`deploy/` holds the systemd + nginx walkthrough
([deploy/README.md](deploy/README.md)). Push to `main` deploys via GitHub
Actions: the gates run, both binaries ship over SSH, the service restarts and
is health-probed. The workflow uses three repository secrets:

| Secret | What |
|---|---|
| `SMRT_DEPLOY_HOST` | `user@host[:port]` -- the SSH target. |
| `SMRT_DEPLOY_SSH_KEY` | PEM-encoded private key (ed25519), CI-only. |
| `SMRT_DEPLOY_KNOWN_HOSTS` | `ssh-keyscan -H <host>` output -- pinned host identity. |

One-time setup, run locally on a trusted workstation (NOT inside CI logs):

```
ssh-keygen -t ed25519 -f ./smrt-ci-deploy -N '' -C 'smrt-ci-deploy@github'
ssh-copy-id -i ./smrt-ci-deploy.pub root@<host>
ssh-keyscan -H <host> > ./smrt-ci-known-hosts
gh secret set SMRT_DEPLOY_HOST        --body 'root@<host>'
gh secret set SMRT_DEPLOY_SSH_KEY     --body "$(cat ./smrt-ci-deploy)"
gh secret set SMRT_DEPLOY_KNOWN_HOSTS --body "$(cat ./smrt-ci-known-hosts)"
shred -u ./smrt-ci-deploy ./smrt-ci-deploy.pub ./smrt-ci-known-hosts
```

The `main` branch deploy is additionally gated through a GitHub Environment
named `production`; a required reviewer there turns every deploy into a
manual approval. `deploy/deploy.sh` remains the emergency local-deploy
fallback.

## License

Apache License 2.0. See [LICENSE](LICENSE).
