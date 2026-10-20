# daruma-web

Leptos/WASM web UI for Daruma — an observability viewer with one write path:
the per-project settings toggles (see [Scope](#scope)).

The app connects to a Daruma API server through `/v1/*` REST endpoints and
`/v1/ws`. It is a static frontend: build it with Trunk and serve the generated
`dist/` directory from any static host.

## Development

Prerequisites:

- Rust with the `wasm32-unknown-unknown` target
- Trunk 0.21+

```bash
# vendor/oss is a git submodule of github.com/tupical/daruma
sh scripts/link-oss.sh        # wraps: git submodule update --init vendor/oss
env -u http_proxy -u https_proxy -u HTTP_PROXY -u HTTPS_PROXY \
  NO_COLOR=false trunk serve --config Trunk.dev.toml
```

Open `http://127.0.0.1:5174/`.

Drop the proxy variables: Trunk's `[[proxy]]` backend calls honour `HTTP_PROXY`
but not glob `no_proxy` entries like `127.*`, so with a local proxy running
every `/v1/*` request leaves the machine and comes back as somebody else's
`404`. It surfaces in the browser console as `[ws] catch-up failed: HTTP 404`
with an nginx error page in the body — a dev-environment artifact, not an app
bug.

## Build

```bash
trunk build --release
```

For a non-root deployment path, pass the public URL explicitly:

```bash
trunk build --release --public-url /web/
```

## Sign-in

The viewer sends a bearer token with every `/v1/*` request and keeps it in the
browser's `localStorage` (`daruma_token`). Without a stored token it decides
how to get one before redirecting anywhere (`src/main.rs`, `src/auth.rs`):

1. It reads the API server's OAuth metadata: `GET
   /.well-known/oauth-protected-resource` on its own origin (RFC 9728) names
   the authorization server, whose `/.well-known/oauth-authorization-server`
   (RFC 8414) gives the authorize, token and registration endpoints. No OAuth
   URL is compiled into the bundle.
2. If both documents are there, it runs Authorization Code + PKCE (S256):
   registers itself as a client, sends the browser to the authorization
   endpoint and exchanges the code on return. The redirect URI is the page's
   origin plus `/web/`, the bundle's `public_url`.
3. If the server publishes no metadata (404, 401/403, or a non-JSON page), the
   viewer shows a sign-in screen instead. If discovery or sign-in fails, the
   screen shows the error.

**Hosted (MCPBox).** The cloud API publishes both metadata documents, so a
fresh browser goes straight to the MCPBox consent page.

**Self-hosted (OSS daruma-server).** The OSS server has no OAuth endpoints, so
the first visit lands on the sign-in screen. Bootstrap the initial token
through the URL once:

```text
https://<your-host>/web/?token=<token>
```

The token is stored in `localStorage` and reused on later visits. On first
start daruma-server prints a long-lived admin token and writes it to
`<data_dir>/bootstrap.token` (see the
[daruma quick start](https://github.com/tupical/daruma#quick-start)). The
token also stays in that first URL and in browser history, so prefer a
token with only the capabilities the viewer needs. Serve the bundle and the
API from one origin, as in production: the viewer calls `/v1/*`, `/v1/ws` and
the metadata on its own origin.

## Scope

This UI is an observability surface. It lists and inspects tasks, projects,
plans, runs, documents, relations, and realtime updates. It does not create,
edit or delete tasks, plans, runs, documents or relations.

The one exception is project settings. With a single project selected, the
right-rail **Settings** panel shows the "Interview" and "Human Log"
auto-append toggles; flipping one sends
`PATCH /v1/projects/{id}/settings` and changes that setting on the server for
every client (`src/components/project_settings_panel.rs`,
`update_project_settings` in `src/api.rs`). The server requires the
`ProjectWrite` capability for that request. With a read-only token the toggle
reverts and the panel shows "Not authorized"; the rest of the UI is
unaffected.

The other non-`GET` requests do not touch tracker data:
`POST /v1/relations/query` is a bulk read; the dynamic client registration
(registers this app as an OAuth client on the server) and token requests are
the sign-in flow (see [Sign-in](#sign-in), `src/auth.rs`).

### What daruma-web displays (OSS boundary)

daruma-web renders **execution results** only:

- Tasks, projects, plans, plan progress
- Run history and activity feed
- Documents and artifacts produced by runs
- Workspace graph (project/task relations)

**Upper-layer SaaS entities** (knowledge, hypothesis, decision,
sensemaking, and similar SaaS-layer types) are **not displayed here.**
Those belong to a separate viewer or layer. Pull requests that add
upper-layer entity visualisations to this repository will be declined at
review.

## Host-shell integration contract

daruma-web is designed to be embedded inside a larger host application
(e.g. a Cloud dashboard, a local switcher). The **only** supported
integration point is a JSON file served by the host at:

```
/.well-known/daruma-shell.json
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

1. **No hardcoded Cloud or SaaS URLs** inside daruma-web source. Any
   host-specific URL must come from `/.well-known/daruma-shell.json`
   at runtime.
2. Adding a new host integration point (button, link, label, feature flag)
   requires extending `HostShellConfig` and the JSON contract — not
   hardcoding a URL or brand name in the Rust/HTML source.
3. The OSS viewer must remain fully functional when
   `/.well-known/daruma-shell.json` is absent.

Source: `src/host_shell.rs` (config struct and fetch logic),
`src/components/host_shell_nav.rs` (nav rendering).
