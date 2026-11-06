#!/usr/bin/env node
// Real scenario: a browser running the built bundle against a real
// daruma-server, with HTTP catch-up failures injected between the two.
//
//   node scripts/scenarios/ws-backlog.mjs <dist-dir> <daruma-server-binary> [trace-file]
//
// Build <dist-dir> with `trunk build --release --dist <dir>` and the binary
// from the vendor/oss pin. Playwright comes from TESTS_DIR, a directory whose
// node_modules holds it. Exits 1 when any check fails, so the same script
// run on a bundle of the base commit is the negative control.
//
// The event log is seeded so one page holds tasks, the next holds only events
// of a channel the viewer does not subscribe to (an empty filtered page), and
// the tail is what the activity feed renders: the last 200 entries.

import { spawn } from "node:child_process";
import { randomUUID } from "node:crypto";
import fs from "node:fs";
import http from "node:http";
import { createRequire } from "node:module";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { DatabaseSync } from "node:sqlite";

const [distDir, serverBin, traceFile] = process.argv.slice(2);
if (!distDir || !serverBin) {
  console.error("usage: ws-backlog.mjs <dist-dir> <daruma-server-binary> [trace-file]");
  process.exit(2);
}

const testsDir = process.env.TESTS_DIR ?? process.cwd();
const { chromium } = createRequire(path.join(testsDir, "package.json"))("playwright");

const HEAD_TASKS = 1000;
const HIDDEN_RULES = 2100;
const TAIL_TASKS = 300;
const CONTRACT = { "X-Daruma-Plugin-Contract": "1" };

const lines = [];
function trace(kind, data = {}) {
  const line = JSON.stringify({ kind, ...data });
  lines.push(line);
  console.log(line);
}

let failed = 0;
function check(scenario, name, ok, detail = {}) {
  if (!ok) failed += 1;
  trace("check", { scenario, name, ok, ...detail });
}

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

async function waitFor(what, predicate, timeoutMs = 20000) {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const value = await predicate();
    if (value) return value;
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`);
    await sleep(50);
  }
}

const freePort = () =>
  new Promise((resolve) => {
    const probe = net.createServer().listen(0, "127.0.0.1", () => {
      const { port } = probe.address();
      probe.close(() => resolve(port));
    });
  });

// ── Server ───────────────────────────────────────────────────────────────────

const work = fs.mkdtempSync(path.join(os.homedir(), ".cache", "ws-backlog-"));
const agentDir = path.join(work, "agent");
fs.mkdirSync(agentDir);
const serverPort = await freePort();
const tlsPort = await freePort();
const serverLog = fs.openSync(path.join(work, "server.log"), "a");

let serverProc;
function startServer() {
  serverProc = spawn(serverBin, [], {
    stdio: ["ignore", serverLog, serverLog],
    env: {
      PATH: process.env.PATH,
      HOME: work,
      DARUMA_AGENT_DIR: agentDir,
      DARUMA_PORT: String(serverPort),
      DARUMA_TLS_PORT: String(tlsPort),
      DARUMA_MDNS_DISABLE: "1",
    },
  });
  return waitFor(
    "server health",
    () =>
      fetch(`http://127.0.0.1:${serverPort}/v1/healthz`)
        .then((res) => res.ok && res.json())
        .catch(() => false),
    60000,
  );
}

async function stopServer() {
  const exited = new Promise((resolve) => serverProc.once("exit", resolve));
  serverProc.kill("SIGTERM");
  await exited;
}

let token;
async function command(body, actor) {
  const res = await fetch(`http://127.0.0.1:${serverPort}/v1/commands`, {
    method: "POST",
    headers: {
      Authorization: `Bearer ${token}`,
      "Content-Type": "application/json",
      ...CONTRACT,
    },
    body: JSON.stringify({ command: body, actor }),
  });
  const reply = await res.text();
  if (!res.ok) throw new Error(`${body.type}: ${res.status} ${reply}`);
  return JSON.parse(reply);
}

let projectId;
async function createTask(title) {
  await command({
    type: "materialize_plan",
    plan: {
      title,
      project_id: projectId,
      owner: { kind: "user" },
      source_brief: "ws-backlog scenario",
      source_ref: `scenario:${title}`,
    },
    tasks: [{ title }],
  });
}

function seedLog() {
  const db = new DatabaseSync(path.join(agentDir, "data", "daruma.sqlite"));
  const template = db.prepare("SELECT * FROM events WHERE kind = 'task_created' LIMIT 1").get();
  // table_info leaves generated columns out.
  const columns = db
    .prepare("PRAGMA table_info(events)")
    .all()
    .map((column) => column.name)
    .filter((column) => column !== "seq");
  const insert = db.prepare(
    `INSERT INTO events (${columns.join(",")}) VALUES (${columns.map(() => "?").join(",")})`,
  );
  const actorBase = JSON.parse(template.actor_json);
  const titles = [];
  // Two alternating actors keep the feed from collapsing rows into bursts.
  const row = (kind, payload, index) => {
    const actor = { ...actorBase, name: `seed-${index % 2}` };
    const values = {
      ...template,
      event_id: randomUUID(),
      kind,
      actor_json: JSON.stringify(actor),
      payload_json: JSON.stringify(payload),
    };
    insert.run(...columns.map((column) => values[column]));
  };
  const taskPayload = JSON.parse(template.payload_json);
  const task = (index) => {
    const title = `seed-${String(index).padStart(5, "0")}`;
    titles.push(title);
    row(
      "task_created",
      { ...taskPayload, task: { ...taskPayload.task, id: randomUUID(), title } },
      index,
    );
  };
  const rule = (index) =>
    row(
      "rule_fired",
      {
        type: "rule_fired",
        rule_id: randomUUID(),
        rule_key: "seed.hidden",
        decision: "warning",
        trigger: "task.before_complete",
        actor: actorBase,
        message: "seed",
        at: template.occurred_at,
      },
      index,
    );
  db.exec("BEGIN");
  for (let i = 0; i < HEAD_TASKS; i += 1) task(i);
  for (let i = 0; i < HIDDEN_RULES; i += 1) rule(i);
  for (let i = 0; i < TAIL_TASKS; i += 1) task(HEAD_TASKS + i);
  db.exec("COMMIT");
  const head = db.prepare("SELECT MAX(seq) AS seq FROM events").get().seq;
  db.close();
  return { titles, head };
}

// ── Front: the bundle under /web/, everything else proxied to the server ─────

const MIME = { ".html": "text/html", ".js": "text/javascript", ".wasm": "application/wasm", ".css": "text/css" };
const front = http.createServer((req, res) => {
  const url = new URL(req.url, "http://front");
  if (url.pathname.startsWith("/web/")) {
    const relative = path.normalize(url.pathname.slice("/web/".length));
    const file = path.join(distDir, relative);
    const body = fs.existsSync(file) && fs.statSync(file).isFile() ? file : path.join(distDir, "index.html");
    res.writeHead(200, { "Content-Type": MIME[path.extname(body)] ?? "application/octet-stream" });
    fs.createReadStream(body).pipe(res);
    return;
  }
  const upstream = http.request(
    { host: "127.0.0.1", port: serverPort, path: req.url, method: req.method, headers: req.headers },
    (reply) => {
      res.writeHead(reply.statusCode, reply.headers);
      reply.pipe(res);
    },
  );
  upstream.on("error", () => res.destroy());
  req.pipe(upstream);
});
front.on("upgrade", (req, socket, head) => {
  const upstream = net.connect(serverPort, "127.0.0.1", () => {
    const headers = Object.entries(req.headers).map(([key, value]) => `${key}: ${value}`);
    upstream.write(`GET ${req.url} HTTP/1.1\r\n${headers.join("\r\n")}\r\n\r\n`);
    upstream.write(head);
    upstream.pipe(socket);
    socket.pipe(upstream);
  });
  upstream.on("error", () => socket.destroy());
  socket.on("error", () => upstream.destroy());
});

// ── One browser session with injected faults ─────────────────────────────────

/** Fresh browser profile. `http(call, since)` may return a status to fail with;
 *  `onClientFrame` returning false swallows a frame; `onServerFrame` may await. */
async function openViewer(browser, frontPort, { http: failHttp, onClientFrame, onServerFrame }) {
  const context = await browser.newContext();
  const page = await context.newPage();
  const seen = { httpSince: [], subscribes: [], snapshots: [], events: [], errors: [], sockets: [] };

  page.on("pageerror", (error) => seen.errors.push(String(error)));
  page.on("console", (message) => {
    if (message.text().includes("panicked")) seen.errors.push(message.text());
  });

  await page.addInitScript((value) => localStorage.setItem("daruma_token", value), token);
  await page.route("**/v1/events?*", async (route) => {
    const since = Number(new URL(route.request().url()).searchParams.get("since"));
    seen.httpSince.push(since);
    const status = failHttp?.(seen.httpSince.length, since);
    if (status) await route.fulfill({ status, body: "injected failure" });
    else await route.continue();
  });
  await page.routeWebSocket(/\/v1\/ws/, (client) => {
    const server = client.connectToServer();
    const socket = { client, server };
    seen.sockets.push(socket);
    let chain = Promise.resolve();
    client.onMessage((message) => {
      const frame = JSON.parse(message);
      if (frame.type === "subscribe") seen.subscribes.push(frame.since_seq);
      if (onClientFrame?.(frame, socket) !== false) server.send(message);
    });
    server.onMessage((message) => {
      const frame = JSON.parse(message);
      if (frame.type === "snapshot") {
        seen.snapshots.push({
          since_seq: frame.since_seq,
          events: frame.events.length,
          has_more: frame.has_more,
          next_seq: frame.next_seq,
        });
      }
      if (frame.type === "event") seen.events.push(frame.envelope.seq);
      chain = chain.then(async () => {
        await onServerFrame?.(frame, socket);
        client.send(message);
      });
    });
    client.onClose(() => server.close());
    server.onClose(() => client.close());
  });

  await page.goto(`http://127.0.0.1:${frontPort}/web/activity`);
  return { context, page, seen };
}

const badge = (page) =>
  page
    .locator(".conn-badge")
    .first()
    .textContent({ timeout: 1000 })
    .catch(() => null);

const waitLive = (page) => waitFor("live badge", async () => (await badge(page)) === "live");

async function feedTitles(page) {
  const rows = await page.locator(".feed-entry__summary").allTextContents();
  return rows.flatMap((row) => row.match(/created (?:task|project) "(.+)"/)?.slice(1) ?? []);
}

async function checkFeed(scenario, page, allTitles) {
  await sleep(500);
  const rendered = await feedTitles(page);
  const expected = allTitles.slice(-rendered.length);
  const bursts = await page.locator(".feed-entry--burst").count();
  check(scenario, "feed shows the last events exactly once and in order", rendered.length >= 150 && bursts === 0 && rendered.join() === expected.join(), {
    rendered: rendered.length,
    bursts,
    first: rendered[0],
    last: rendered.at(-1),
    missing: expected.filter((title) => !rendered.includes(title)).length,
    unique: new Set(rendered).size,
  });
}

// ── Scenarios ────────────────────────────────────────────────────────────────

let liveCounter = 0;
async function commitLive(allTitles) {
  liveCounter += 1;
  const title = `live-${liveCounter}`;
  // One event per commit under alternating actors, so no burst hides a row.
  await command(
    { type: "create_project", title },
    { kind: "user", name: `live-actor-${liveCounter % 2}` },
  );
  allTitles.push(title);
  return title;
}

/** The trace from the defect report: first HTTP attempt fails, one Snapshot
 *  page, a live event, then the socket dies before the backlog closes. */
async function httpFailureThenReconnect(browser, frontPort, allTitles) {
  const name = "http-failure-live-event-reconnect";
  let firstSnapshot = false;
  let subscribes = 0;
  let released = false;
  const viewer = await openViewer(browser, frontPort, {
    http: (call) => (call === 1 ? 503 : null),
    // The next page request never reaches the server, so the backlog stays open.
    onClientFrame: (frame) => {
      if (frame.type === "subscribe") subscribes += 1;
      return subscribes <= 1 || released;
    },
    onServerFrame: (frame) => {
      if (frame.type === "snapshot") firstSnapshot = true;
    },
  });
  const { page, seen } = viewer;

  await waitFor("first Snapshot", () => firstSnapshot);
  await waitFor("Snapshot applied", async () => (await feedTitles(page)).length > 0);
  const live = await commitLive(allTitles);
  const liveSeq = await waitFor("live frame", () => seen.events.at(-1));
  await sleep(300);
  trace("scenario", { scenario: name, step: "live event delivered mid-backlog", live, liveSeq, badge: await badge(page) });
  check(name, "not Live while the backlog is open", (await badge(page)) !== "live", { badge: await badge(page) });

  released = true;
  const [socket] = seen.sockets;
  socket.client.close();
  socket.server.close();

  await waitFor("second socket", () => seen.sockets.length >= 2);
  await waitLive(page);
  trace("scenario", { scenario: name, httpSince: seen.httpSince, subscribes: seen.subscribes, snapshots: seen.snapshots });
  const firstPageEnd = seen.snapshots[0].next_seq;
  check(name, "reconnect resumes at the page end, not at the live event", seen.httpSince[1] === firstPageEnd, {
    resumed_from: seen.httpSince[1],
    first_page_end: firstPageEnd,
    live_seq: liveSeq,
  });
  await checkFeed(name, page, allTitles);
  check(name, "no wasm panic", seen.errors.length === 0, { errors: seen.errors });
  await viewer.context.close();
}

/** HTTP stays down: the whole history comes over the socket, through an empty
 *  filtered page, with a live event landing during pagination. */
async function pagingOverSocket(browser, frontPort, allTitles, head) {
  const name = "http-down-paging-over-socket";
  let openGate;
  const gate = new Promise((resolve) => (openGate = resolve));
  let snapshots = 0;
  const viewer = await openViewer(browser, frontPort, {
    http: () => 503,
    onServerFrame: async (frame) => {
      if (frame.type === "snapshot") {
        snapshots += 1;
        if (snapshots === 2) await gate;
      }
    },
  });
  const { page, seen } = viewer;

  await waitFor("second page held", () => snapshots === 2);
  const live = await commitLive(allTitles);
  await sleep(300);
  const midBadge = await badge(page);
  trace("scenario", { scenario: name, step: "page held back", live, badge: midBadge });
  check(name, "catching up between pages", midBadge !== "live", { badge: midBadge });
  openGate();

  await waitLive(page);
  await sleep(300);
  trace("scenario", { scenario: name, httpSince: seen.httpSince.length, subscribes: seen.subscribes, snapshots: seen.snapshots });
  const empty = seen.snapshots.find((page) => page.events === 0 && page.has_more);
  check(name, "empty filtered page continued the pagination", Boolean(empty), { empty });
  const closing = seen.snapshots.at(-1);
  check(name, "last page has has_more=false and passes the live event", closing?.has_more === false && closing.next_seq > head, { closing });
  check(name, "every page is requested from the previous page end", seen.subscribes.slice(1).every((since, i) => since === seen.snapshots[i].next_seq), {
    subscribes: seen.subscribes,
  });
  await checkFeed(name, page, allTitles);
  check(name, "no wasm panic", seen.errors.length === 0, { errors: seen.errors });
  await viewer.context.close();
}

async function errorAfterGoodHttpPage(browser, frontPort, allTitles) {
  const name = "http-error-after-good-page";
  const viewer = await openViewer(browser, frontPort, { http: (call) => (call === 2 ? 503 : null) });
  const { page, seen } = viewer;

  await waitLive(page);
  trace("scenario", { scenario: name, httpSince: seen.httpSince, subscribes: seen.subscribes, snapshots: seen.snapshots });
  check(name, "Subscribe continues from the last good HTTP page", seen.subscribes[0] === seen.httpSince[1], {
    http_since: seen.httpSince,
    subscribe: seen.subscribes[0],
  });
  await checkFeed(name, page, allTitles);
  check(name, "no wasm panic", seen.errors.length === 0, { errors: seen.errors });
  await viewer.context.close();
}

async function ordinaryStartupAndReconnect(browser, frontPort, allTitles) {
  const name = "ordinary-startup-and-reconnect";
  const viewer = await openViewer(browser, frontPort, {});
  const { page, seen } = viewer;

  await waitLive(page);
  check(name, "one Subscribe after a complete HTTP catch-up", seen.subscribes.length === 1, {
    http_pages: seen.httpSince.length,
    subscribes: seen.subscribes,
    snapshots: seen.snapshots,
  });
  const live = await commitLive(allTitles);
  await waitFor("live row", async () => (await feedTitles(page)).includes(live));
  await checkFeed(`${name}/live`, page, allTitles);

  const liveSeq = seen.events.at(-1);
  const [socket] = seen.sockets;
  socket.client.close();
  socket.server.close();
  const offline = await commitLive(allTitles);
  await waitFor("second socket", () => seen.sockets.length >= 2);
  await waitLive(page);
  await waitFor("offline row", async () => (await feedTitles(page)).includes(offline));
  trace("scenario", { scenario: name, httpSince: seen.httpSince, subscribes: seen.subscribes, snapshots: seen.snapshots });
  check(name, "reconnect continues after the last live event", seen.httpSince.includes(liveSeq), { live_seq: liveSeq });
  await checkFeed(`${name}/reconnect`, page, allTitles);
  check(name, "no wasm panic", seen.errors.length === 0, { errors: seen.errors });
  await viewer.context.close();
}

// ── Run ──────────────────────────────────────────────────────────────────────

let browser;
try {
  const health = await startServer();
  trace("server", { git_sha: health.git_sha, version: health.version });
  token = fs.readFileSync(path.join(agentDir, "data", "bootstrap.token"), "utf8").trim();
  const created = await command({ type: "create_project", title: "ws-backlog" });
  projectId = created.data[0].payload.project.id;
  await createTask("template");
  await stopServer();
  const { titles, head } = seedLog();
  const allTitles = ["template", ...titles];
  await startServer();
  trace("seed", { head, tasks: allTitles.length, hidden_rules: HIDDEN_RULES });

  const frontPort = await freePort();
  await new Promise((resolve) => front.listen(frontPort, "127.0.0.1", resolve));
  browser = await chromium.launch();

  const scenarios = [
    ["ordinary", () => ordinaryStartupAndReconnect(browser, frontPort, allTitles)],
    ["http-page-error", () => errorAfterGoodHttpPage(browser, frontPort, allTitles)],
    ["paging", () => pagingOverSocket(browser, frontPort, allTitles, head)],
    ["defect-trace", () => httpFailureThenReconnect(browser, frontPort, allTitles)],
  ];
  for (const [label, run] of scenarios) {
    await run().catch((error) => {
      failed += 1;
      trace("scenario-error", { scenario: label, message: error.message });
    });
  }
} finally {
  await browser?.close();
  front.close();
  serverProc?.kill("SIGTERM");
  fs.rmSync(work, { recursive: true, force: true });
}

trace("result", { failed });
if (traceFile) fs.writeFileSync(traceFile, `${lines.join("\n")}\n`);
process.exit(failed ? 1 : 0);
