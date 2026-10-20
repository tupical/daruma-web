# REVIEW.md — what matters when reviewing daruma-web

Only what is specific to this repository. The review procedure (branches,
merge order, the `REVIEW_RESULT` marker, round cap, PRE-EXISTING, the two
operating modes invariant, the real-scenario requirement) lives in the
reviewer prompt `~/.hermes/scripts/mcpbox_review.sh` and is not repeated here.
The user-facing contract is [README.md](README.md): Scope, OSS boundary,
Sign-in, Host-shell integration contract.

## What "broken" means here

This project has no REVIEW REJECT yet; the classes below come from its filed
defects and `fix(...)` commits. A blocker is a defect this diff introduces from
one of these classes, or an acceptance criterion with no line in the diff.

1. **The WASM module dies.** Any panic kills the whole app until reload
   (`RuntimeError: unreachable`). Known sources:
   - a future started with plain `spawn_local` that reads a component signal
     after an `.await`: a route change disposes the owner mid-fetch (`46bbf26`).
     Route-scoped fetches use `leptos::task::spawn_local_scoped_with_cancellation`;
     the rationale is in `src/components/task_list.rs:309`;
   - one component under several route patterns: each pattern is its own
     route match, so switching between them tears the subtree down. `/` is
     canonicalised to `/app/all` for this reason (`src/app.rs:86`, `2fadc9f`);
   - integer underflow in states right after connect (`seq < last` in the
     status bar, `2fadc9f`);
   - `std::time` on `wasm32-unknown-unknown`: `chrono` without `wasmbind`, or a
     domain helper that calls `now()` (`3ddfc7e`).

   Check: for every new `spawn_local`, does the future touch a signal after an
   await? For every new `path!` in `src/app.rs`, which component does it mount?
2. **The viewer and the pinned server disagree.** Wire types come from
   `vendor/oss` (`daruma-api-dto`, `daruma-events`, `daruma-domain`). Server
   validation is not in those types. Examples: the `status` filter on
   `/v1/tasks` and `/v1/plans` changed twice — it became mandatory
   (`src/api.rs:28`, `53e0c73`), then a newer server answered `400` to `all`
   and the "All" filter loaded nothing; every `/v1/*` request needs the
   `X-Daruma-Plugin-Contract` header (`81bcfd2`); a `vendor/oss` bump with a
   new field or variant broke an exhaustive match, so the repo did not compile
   (`6181ccb`). WS catch-up also replays old events: pre-migration
   `project_created` payloads with an empty slug overwrote the loaded slug
   (`e7eb588`). Check a changed request against its route in
   `vendor/oss/apps/server/src/routes/`. A new event handler must accept
   payloads written before the field existed.
3. **It writes where it promises to read.** The only write path is the project
   settings toggles (`PATCH /v1/projects/{id}/settings`). The other non-`GET`
   calls are `POST /v1/relations/query` (bulk read) and OAuth register/token
   (README, Scope). A new `post_json`, `patch_json` or
   `Request::{post,put,patch,delete}` in `src/` without a matching README Scope
   change and a capability in `module.toml` is a blocker.
4. **MCPBox leaks into the OSS viewer.** No cloud, SaaS, cabinet or `mcpbox`
   terms and no hardcoded hosts in `src/`. Host URLs come only from
   `/.well-known/daruma-shell.json` (`src/host_shell.rs`) or from server OAuth
   metadata (`src/auth.rs`). Upper-layer entities (knowledge, hypothesis,
   decision, sensemaking) are not displayed here. Example: hardcoded MCPBox
   OAuth URLs made self-hosting unreachable (`712b923` fixed it).
   `scripts/check-oss-boundary.sh` fails on `origin/main` because of test
   fixtures in `src/auth.rs`, `src/base.rs` and `src/projects_ctx.rs`. Count
   only the lines the diff adds:
   `git diff -U0 origin/main...HEAD -- src | grep '^+' | grep -iP '\bcloud\b|\bsaas\b|\bcabinet\b|mcpbox|"https://'`.
   The script allows `//` comment lines; this filter does not, so drop those.
5. **Navigation leaves the mount base.** The bundle is served under `/web/`
   (`Trunk.toml` `public_url`); the base comes from `mount_base()`
   (`src/base.rs:24`) and goes into `<Router base=…>` (`src/app.rs:21`). In-app
   navigation uses `use_navigate`. `window.location().assign` to a root path
   drops the base, and `/graph`, `/activity` and others returned 404. Only
   two calls leave the app: the host-shell link (`src/components/host_shell_nav.rs:149`)
   and the OAuth redirect (`src/auth.rs:177`). `/app` is a route, not a base
   (`2072805`).
6. **The live view goes stale or flickers.** Views update from WS events
   applied in place. A full refetch on every event made the list flicker and
   jump. A new list or panel that loads only on mount and never reacts to its
   events, or a resource keyed on an event counter, is a blocker when the
   criterion is about live data.
7. **The README lies.** It claimed the viewer never mutates data while the
   settings toggles sent `PATCH`, and it promised a self-hosted setup that the
   startup path could not reach. A diff that changes behaviour described in
   README updates README in the same diff. Check every README claim it touches
   against `path:line`.
8. **The check proves nothing.** A browser run against a stale `daruma-server`
   binary: the first live-update verification failed only because the server
   was three weeks older than its sources. A run through the Trunk dev proxy
   with `HTTP_PROXY` set: an nginx `404` in `[ws] catch-up failed` is the
   proxy, not the app (README, Development). Native `cargo test` does not hit
   wasm-only panics (class 1) or routing under `/web/` (class 5). For those,
   the evidence is a browser run of the built `dist/` against a server built
   from the `vendor/oss` pin.

## Not a blocker

- Style, naming, "could be cleaner" — at most **3 remarks** per pass, in one
  comment after the blockers. Drop the rest.
- Clippy warnings and `rustfmt` drift in files the diff does not touch;
  `origin/main` already has dozens under `--target wasm32-unknown-unknown`.
- Boundary-check hits that already exist on `origin/main` (class 4).
- A defect visible on `origin/main` before this diff — PRE-EXISTING, per the
  reviewer prompt.

## Paths to skip

- `vendor/oss/` — submodule of `tupical/daruma`. Review only the gitlink:
  which SHA, and is it on daruma `origin/main`. Its content is reviewed there.
- `target/`, `dist/`, `Cargo.lock` — build output, gitignored.
- `style.css` — check only that classes the diff uses exist; no style review.

## Focused checks

- `cargo test -j 4 --bin daruma-web <filter>` — native unit tests.
- `cargo clippy -j 4 --target wasm32-unknown-unknown --bin daruma-web` — the
  real target; read diagnostics only for touched files.
- `NO_COLOR=false trunk build --release --dist /tmp/<dir>` — an inherited
  `NO_COLOR=1` stops Trunk before it builds.
- The boundary check, filtered to added lines (class 4).

## Evidence

Every blocker carries `path:line` on the branch under review, a reproduction
(URL and click path, request, or command) and the criterion it breaks. A
guess from a function or file name is not a blocker: open the code and find
the line. Without `path:line` the remark goes to comments, not blockers.

## Summary

The second line of the verdict (right after `REVIEW ACCEPT`/`REVIEW REJECT`)
starts with a count: `Blockers: N`. With none, say so: `Blockers: 0 — nothing
to fix.` Then the blockers in order, then at most three remarks.
