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

### What taskagent-web displays (OSS boundary)

taskagent-web renders **execution results** only:

- Tasks, projects, plans, plan progress
- Run history and activity feed
- Documents and artifacts produced by runs
- Workspace graph (project/task relations)

**Upper-layer MCPBox entities** (knowledge, hypothesis, decision,
sensemaking, and similar `mcpbox_*` types) are **not displayed here.**
Those belong to a separate viewer or layer. Pull requests that add
upper-layer entity visualisations to this repository will be declined at
review.

## Host-shell integration contract

taskagent-web is designed to be embedded inside a larger host application
(e.g. a Cloud dashboard, a local switcher). The **only** supported
integration point is a JSON file served by the host at:

```
/.well-known/taskagent-shell.json
```

The viewer fetches this URL on startup. If the file is absent or returns
404 the viewer runs in standalone mode with no host chrome.

### HostShellConfig fields

All fields are optional strings. An empty or all-absent payload is treated
as standalone mode.

| Field | Type | Description |
|---|---|---|
| `home_url` | `string \| null` | URL of the host's home/dashboard page. |
| `switcher_url` | `string \| null` | URL of the host's workspace-switcher page. Preferred over `home_url` for the "Workspaces" button. |
| `current_workspace_label` | `string \| null` | Display name of the currently active workspace (max 80 chars). Shown in the nav bar. |

If both `switcher_url` and `home_url` are present, `switcher_url` takes
precedence for navigation (`primary_url` in `src/host_shell.rs:33`).

Accepted URL values: absolute `https://…`, `http://…`, or root-relative
`/…` paths. Values longer than 2 048 chars are silently discarded.

### Rules for host integration

1. **No hardcoded Cloud or SaaS URLs** inside taskagent-web source. Any
   host-specific URL must come from `/.well-known/taskagent-shell.json`
   at runtime.
2. Adding a new host integration point (button, link, label, feature flag)
   requires extending `HostShellConfig` and the JSON contract — not
   hardcoding a URL or brand name in the Rust/HTML source.
3. The OSS viewer must remain fully functional when
   `/.well-known/taskagent-shell.json` is absent.

Source: `src/host_shell.rs` (config struct and fetch logic),
`src/components/host_shell_nav.rs` (nav rendering).
