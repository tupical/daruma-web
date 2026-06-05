<!--
module:
  name:     taskagent-web
  kind:     client
  status:   shipped
  contract: /v1/* + WS
  owner:    clients
See module.toml for the machine-readable manifest.
-->

# taskagent-web

Leptos/WASM client — the canonical read-only browser viewer for TaskAgent OSS.
It talks to the core only through the public `/v1/*` REST API and the `/v1/ws`
WebSocket; no shared in-process state and no write/admin controls.

This is a **standalone repository**, extracted from the OSS monorepo (formerly
`apps/web-leptos`). Release builds are intended to pin the OSS crates to a
TaskAgent git tag; local development may use `vendor/oss`, a symlink to a
sibling TaskAgent checkout. The OSS `taskagent-server` is now a bare API + MCP
backend and no longer bundles or serves this UI.

## Layout

```
taskagent-web/
├── Cargo.toml          # standalone workspace + crate (OSS core deps)
├── Trunk.toml          # asset pipeline config
├── module.toml         # module manifest
├── index.html          # bootstrap HTML for the WASM bundle
├── style.css           # global styles
├── scripts/link-oss.sh # creates vendor/oss → ../taskagent
├── src/
│   ├── main.rs         # Trunk entry — mounts <App/>
│   ├── api.rs          # gloo-net HTTP client against /v1/*
│   ├── ws.rs           # WebSocket client + reconnect/resync
│   └── components/     # task list, plans panel, project/doc viewers
├── vendor/oss          # local dev symlink → ../taskagent (not committed)
└── dist/               # build output (gitignored)
```

## Build

Requires:

- A sibling OSS checkout at `../taskagent` (or `TASKAGENT_OSS_ROOT`).
- Rust toolchain matching `rust-toolchain.toml`.
- `wasm32-unknown-unknown` target: `rustup target add wasm32-unknown-unknown`.
- [Trunk](https://trunkrs.dev/) 0.21+: `cargo install --locked trunk`.

```bash
sh scripts/link-oss.sh    # one-time: vendor/oss → ../taskagent
trunk build --release
```

The WASM target needs `chrono` with the `wasmbind` feature (declared in
`Cargo.toml`). Without it, applying live WS `TaskUpdated` / `PlanUpdated`
events panics with `time not implemented on this platform`.

Production builds should set `--public-url` to the URL path where the static
bundle will be mounted. The default in `Trunk.toml` is `/web/`:

```bash
trunk build --release --public-url /web/
```

Do **not** deploy `dist/` after `trunk serve` — that leaves
`{{__TRUNK_*}}` live-reload scripts and development asset URLs in the bundle.

Output lands in `dist/` — `index.html`, the hashed CSS, and the hashed
`taskagent-web-*.{js,wasm}`.

For development with hot reload:

```bash
# Terminal 1 — API (sibling taskagent checkout)
cd ../taskagent && just server
# SQLite: ~/.agents/taskagent/data/ (see local-dev-data.md)

# Terminal 2 — UI (this repo)
NO_COLOR=false trunk serve --config Trunk.dev.toml
```

**Database:** `~/.agents/taskagent/data/taskagent.sqlite` — set only via
`TASKAGENT_DATA_DIR` if you need a non-default copy. See
`../taskagent/docs/guides/local-dev-data.md` and `scripts/dev-stack.sh`.

Open the UI at **`http://127.0.0.1:5174/`** when using `Trunk.dev.toml`
(`trunk serve --config Trunk.dev.toml`). Production builds keep
`public_url = /web/`; `./scripts/dev-stack.sh print-url` prints a dev URL
with the bootstrap token.

The API requires an explicit `status` query on `GET /v1/tasks` and
`GET /v1/plans`; this client passes `status=all` so every status group
renders in the viewer.

## Serve

The server no longer serves the UI. Deploy the `dist/` bundle behind any static
host and point it at the API server's `/v1/*` + `/v1/ws`.

## OSS core dependency

The intended production dependency model is a pinned TaskAgent OSS git tag, for
example `taskagent-v0.1.0`. Until the first immutable public tag is cut, this
checkout keeps `vendor/oss` path dependencies as the local development override.
The module manifest records both the intended source and the dev override.

## Read-only scope

The OSS web UI is an observability surface only. It can list and inspect tasks,
projects, plans, runs, documents, relations, and realtime WS updates. It does
not create tasks, complete tasks, edit documents, mutate plans, call AI parse
endpoints, mint MCP tokens, or download MCP binaries. Those workflows belong to
MCP/CLI/desktop/embed clients.

## Module contract

This client consumes:

- **REST** — `/v1/tasks`, `/v1/projects`, `/v1/plans`, `/v1/runs`,
  `/v1/documents`, `/v1/comments`, `/v1/relations/query`, `/v1/healthz`.
- **WS** — `/v1/ws` subprotocol `taskagent.v1`; subscribes to
  `Tasks`, `Comments`, `Plans`, `Runs`, `Presence`, optional
  `AgentStatus` channels.
- **Auth** — bearer token in `Authorization:` header for REST,
  same token via `Sec-WebSocket-Protocol` for WS. Token is stored in
  `localStorage` under `taskagent_token`.

Capabilities the UI actually exercises are declared in
`module.toml [capabilities]`. CI should verify the crate does not import from
internal runtime modules: domain types come from `taskagent-domain` directly,
wire shapes from `taskagent-api-dto`.

## Tests

WASM/UI tests are not run on every push (no headless browser in CI
today). Smoke verification is manual: after `trunk build --release`,
open the dev server and exercise the golden path: select a project,
inspect tasks/plans/documents, expand a task, and see WS updates arrive
without any write-capability token.
