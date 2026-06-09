# taskagent-web

Read-only Leptos/WASM web UI for TaskAgent.

The app connects to a TaskAgent API server through `/v1/*` REST endpoints and
`/v1/ws`. It is a static frontend: build it with Trunk and serve the generated
`dist/` directory from any static host.

## Development

Prerequisites:

- Rust with the `wasm32-unknown-unknown` target
- Trunk 0.21+
- A sibling TaskAgent checkout at `../taskagent`

```bash
sh scripts/link-oss.sh
NO_COLOR=false trunk serve --config Trunk.dev.toml
```

Open `http://127.0.0.1:5174/`.

## Build

```bash
trunk build --release
```

For a non-root deployment path, pass the public URL explicitly:

```bash
trunk build --release --public-url /web/
```

## Scope

This UI is an observability surface. It lists and inspects tasks, projects,
plans, runs, documents, relations, and realtime updates. It does not create or
mutate TaskAgent data.
