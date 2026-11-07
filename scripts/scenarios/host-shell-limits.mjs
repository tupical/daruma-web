#!/usr/bin/env node
// Real scenario: a browser running the built bundle against a real
// daruma-server, with the host publishing /.well-known/daruma-shell.json
// values at and just past the documented length limits.
//
//   node scripts/scenarios/host-shell-limits.mjs <dist-dir> <daruma-server-binary> [trace-file]
//
// Build <dist-dir> with `trunk build --release --dist <dir>` and the binary
// from the vendor/oss pin. Playwright comes from TESTS_DIR, a directory whose
// node_modules holds it. Exits 1 when any check fails, so the same script
// run on a bundle of the base commit is the negative control.
//
// Limits count characters of the trimmed value (README "HostShellConfig
// fields"): 80 for current_workspace_label, 2048 for URLs.

import { spawn } from "node:child_process";
import fs from "node:fs";
import http from "node:http";
import { createRequire } from "node:module";
import net from "node:net";
import os from "node:os";
import path from "node:path";

const [distDir, serverBin, traceFile] = process.argv.slice(2);
if (!distDir || !serverBin) {
  console.error("usage: host-shell-limits.mjs <dist-dir> <daruma-server-binary> [trace-file]");
  process.exit(2);
}

const testsDir = process.env.TESTS_DIR ?? process.cwd();
const { chromium } = createRequire(path.join(testsDir, "package.json"))("playwright");

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

const work = fs.mkdtempSync(path.join(os.homedir(), ".cache", "host-shell-limits-"));
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

// ── Front: bundle under /web/, the host shell file, the rest to the server ───

let shellConfig = null;
let shellFetches = 0;
const MIME = { ".html": "text/html", ".js": "text/javascript", ".wasm": "application/wasm", ".css": "text/css" };
const front = http.createServer((req, res) => {
  const url = new URL(req.url, "http://front");
  if (url.pathname === "/.well-known/daruma-shell.json") {
    shellFetches += 1;
    res.writeHead(200, { "Content-Type": "application/json" });
    res.end(JSON.stringify(shellConfig));
    return;
  }
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

// ── One viewer per host config ───────────────────────────────────────────────

let token;
/** Loads the viewer with `config` published by the host; returns what the
 *  nav shows and, when `follow` is set, the URL the Workspaces button opens. */
async function openViewer(browser, frontPort, config, { follow = false } = {}) {
  shellConfig = config;
  const fetchesBefore = shellFetches;
  const context = await browser.newContext();
  const page = await context.newPage();
  const errors = [];
  page.on("pageerror", (error) => errors.push(String(error)));
  page.on("console", (message) => {
    if (message.text().includes("panicked")) errors.push(message.text());
  });
  await page.addInitScript((value) => localStorage.setItem("daruma_token", value), token);
  await page.goto(`http://127.0.0.1:${frontPort}/web/`);
  await page.waitForSelector(".host-shell-nav");
  await waitFor("host shell fetch", () => shellFetches > fetchesBefore);
  // The config loads asynchronously; give the signal a moment to land.
  await sleep(1500);
  const labels = await page.locator(".host-shell-nav__label").allTextContents();
  const buttons = await page.locator(".host-shell-nav button").allTextContents();
  let opened = null;
  if (follow && buttons.includes("Workspaces")) {
    const request = page.waitForRequest((req) => req.isNavigationRequest() && !req.url().includes("/web/"));
    await page.route("**/switch/**", (route) => route.fulfill({ status: 200, body: "switched" }));
    await page.getByRole("button", { name: "Workspaces" }).click();
    opened = decodeURI(new URL((await request).url()).pathname);
  }
  await context.close();
  return { labels, workspaces: buttons.includes("Workspaces"), opened, errors };
}

const chars = (value) => [...value].length;
const bytes = (value) => Buffer.byteLength(value, "utf8");

async function labelCase(browser, frontPort, scenario, label, expectKept) {
  const result = await openViewer(browser, frontPort, {
    current_workspace_label: label,
    switcher_url: "/switch/home",
  });
  const shown = result.labels[0] ?? null;
  check(scenario, expectKept ? "label shown verbatim" : "label dropped, fallback shown", shown === (expectKept ? label.trim() : "Workspace"), {
    chars: chars(label.trim()),
    bytes: bytes(label.trim()),
    shown_chars: shown === null ? null : chars(shown),
    shown_prefix: shown?.slice(0, 12) ?? null,
  });
  check(scenario, "no page errors", result.errors.length === 0, { errors: result.errors });
}

async function urlCase(browser, frontPort, scenario, url, expectKept) {
  const result = await openViewer(browser, frontPort, { switcher_url: url }, { follow: true });
  if (expectKept) {
    check(scenario, "Workspaces opens the published URL", result.opened === url, {
      chars: chars(url),
      bytes: bytes(url),
      opened_chars: result.opened === null ? null : chars(result.opened),
    });
  } else {
    // The only field is dropped, so the viewer runs standalone: no host chrome.
    check(scenario, "standalone: no label, no Workspaces", !result.workspaces && result.labels.length === 0, {
      chars: chars(url),
      bytes: bytes(url),
      labels: result.labels.length,
      workspaces: result.workspaces,
    });
  }
  check(scenario, "no page errors", result.errors.length === 0, { errors: result.errors });
}

const urlOf = (unit, total) => {
  const prefix = "/switch/";
  return prefix + unit.repeat(total - chars(prefix));
};

// ── Run ──────────────────────────────────────────────────────────────────────

let browser;
try {
  const health = await startServer();
  trace("server", { git_sha: health.git_sha, version: health.version });
  token = fs.readFileSync(path.join(agentDir, "data", "bootstrap.token"), "utf8").trim();

  const frontPort = await freePort();
  await new Promise((resolve) => front.listen(frontPort, "127.0.0.1", resolve));
  browser = await chromium.launch();

  const cases = [
    ["label-41-cyrillic", () => labelCase(browser, frontPort, "label-41-cyrillic", "Ж".repeat(41), true)],
    ["label-80-emoji-trimmed", () => labelCase(browser, frontPort, "label-80-emoji-trimmed", `  ${"😀".repeat(80)}  `, true)],
    ["label-80-ascii", () => labelCase(browser, frontPort, "label-80-ascii", "a".repeat(80), true)],
    ["label-81-cyrillic", () => labelCase(browser, frontPort, "label-81-cyrillic", "ж".repeat(81), false)],
    ["url-2048-cyrillic", () => urlCase(browser, frontPort, "url-2048-cyrillic", urlOf("ж", 2048), true)],
    ["url-2049-cyrillic", () => urlCase(browser, frontPort, "url-2049-cyrillic", urlOf("ж", 2049), false)],
    ["url-2049-ascii", () => urlCase(browser, frontPort, "url-2049-ascii", urlOf("a", 2049), false)],
    ["url-bad-scheme", () => urlCase(browser, frontPort, "url-bad-scheme", "javascript:alert(1)", false)],
  ];
  for (const [label, run] of cases) {
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
