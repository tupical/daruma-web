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

Leptos/WASM client — the canonical browser UI for TaskAgent. It talks to the
core only through the public `/v1/*` REST API and the `/v1/ws` WebSocket; no
shared in-process state.

This is a **standalone repository**, extracted from the OSS monorepo (formerly
`apps/web-leptos`). It consumes the OSS crates read-only via `vendor/oss`, a
symlink to a sibling TaskAgent checkout (the same pattern `taskagent-cloud`
uses). The OSS `taskagent-server` is now a bare API + MCP backend and no longer
bundles or serves this UI.

## Layout

```
taskagent-web/
├── Cargo.toml          # standalone workspace + crate (vendor/oss path deps)
├── Trunk.toml          # asset pipeline config
├── module.toml         # module manifest
├── index.html          # bootstrap HTML for the WASM bundle
├── style.css           # global styles
├── scripts/link-oss.sh # creates vendor/oss → ../taskagent
├── src/
│   ├── main.rs         # Trunk entry — mounts <App/>
│   ├── api.rs          # gloo-net HTTP client against /v1/*
│   ├── ws.rs           # WebSocket client + reconnect/resync
│   └── components/     # task list, plans panel, composer, etc.
├── vendor/oss          # symlink → ../taskagent (not committed)
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

Output lands in `dist/` — `index.html`, the hashed CSS, and the hashed
`taskagent-web-*.{js,wasm}`.

For development with hot reload:

```bash
trunk serve   # proxies /v1/* and /v1/ws to 127.0.0.1:8080 (see Trunk.toml)
```

## Serve

The server no longer serves the UI. Deploy the `dist/` bundle behind any static
host and point it at the API server's `/v1/*` + `/v1/ws`. On TaskAgent Cloud the
bundle is built with `--public-url /app/` and baked into the `cloud-server`
image (the cloud Dockerfile builds this repo via its own `vendor` wiring).

## Settings — MCP token & binary

Open **Settings** (gear icon in the header):

1. **Generate MCP token** — mints a `ta_svc_*` token; the secret is shown once.
2. **Download taskagent-mcp** — Windows (`.exe`) or Linux binary from
   `GET /v1/downloads/taskagent-mcp/{platform}` (bundled in the server image).
3. **Copy mcp.json snippet** — paste into `.cursor/mcp.json` with your binary path.

Requires a bearer token with `token:write` to mint MCP tokens (bootstrap/admin PAT).

## Module contract

This client consumes:

- **REST** — `/v1/tasks`, `/v1/projects`, `/v1/plans`, `/v1/runs`,
  `/v1/documents`, `/v1/comments`, `/v1/ai/*`, `/v1/healthz`.
- **WS** — `/v1/ws` subprotocol `taskagent.v1`; subscribes to
  `Tasks`, `Comments`, `Plans`, `Runs`, `Presence`, optional
  `AgentStatus` channels.
- **Auth** — bearer token in `Authorization:` header for REST,
  same token via `Sec-WebSocket-Protocol` for WS. Token is stored in
  `localStorage` under `taskagent:token`.

Capabilities the UI actually exercises are declared in
`module.toml [capabilities]`. The audit-grep CI step
(§3.4 W4.1) verifies the crate does not import from internal runtime
modules — domain types come from `taskagent-domain` directly, wire
shapes from `taskagent-api-dto`.

## Tests

WASM/UI tests are not run on every push (no headless browser in CI
today). Smoke verification is manual: after `trunk build --release`,
open the dev server and exercise the golden path (create task,
complete task, open plan, attach task to plan, see WS update).
