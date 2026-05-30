// Per-test isolation: every test gets its own copy of the pristine fixture
// (DB + media) in a temp dir and its own concert-web on an ephemeral port, so
// tests can mutate state (delete tracks, toggle likes) without interfering and
// never touch the real concerts.db. Import { test, expect } from here instead
// of from @playwright/test.

const base = require("@playwright/test");
const { spawn } = require("child_process");
const fs = require("fs");
const os = require("os");
const path = require("path");

const REPO = path.resolve(__dirname, "..");
const FIXTURE = path.join(__dirname, ".fixture");
const BIN = path.join(REPO, "target", "debug", "concert-web");

// Spawn a concert-web bound to a copy of the fixture; resolve once it's ready.
async function startServer() {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "tds-e2e-"));
  fs.cpSync(FIXTURE, tmp, { recursive: true });

  const child = spawn(
    BIN,
    [
      "--db",
      path.join(tmp, "test.db"),
      "--workdir",
      tmp,
      "--port",
      "0",
      // No-op opener so the watch/Open buttons never launch a real player.
      "--open-cmd",
      "true",
    ],
    { stdio: ["ignore", "pipe", "pipe"] }
  );

  // child.exitCode stays null for signal-killed processes (signalCode is set
  // instead), so track exit explicitly to avoid hanging teardown.
  const server = { child, tmp, exited: false };
  child.on("exit", () => (server.exited = true));

  let out = "";
  child.stdout.on("data", (d) => (out += d.toString()));
  child.stderr.on("data", (d) => (out += d.toString()));

  const port = await new Promise((resolve, reject) => {
    const timer = setTimeout(
      () => reject(new Error(`server start timed out:\n${out}`)),
      20000
    );
    child.stdout.on("data", () => {
      const m = out.match(/Listening on http:\/\/127\.0\.0\.1:(\d+)/);
      if (m) {
        clearTimeout(timer);
        resolve(parseInt(m[1], 10));
      }
    });
    child.on("exit", (code) => {
      clearTimeout(timer);
      reject(new Error(`server exited (${code}) before ready:\n${out}`));
    });
  });

  const baseURL = `http://127.0.0.1:${port}`;
  // Readiness poll — the listener is bound when the port prints, but give the
  // router a moment to start answering.
  for (let i = 0; i < 50; i++) {
    try {
      const r = await fetch(`${baseURL}/`);
      if (r.ok) break;
    } catch (_) {
      /* not up yet */
    }
    await new Promise((r) => setTimeout(r, 100));
  }

  server.baseURL = baseURL;
  return server;
}

function killChild(server) {
  return new Promise((resolve) => {
    if (server.exited) return resolve();
    server.child.on("exit", () => resolve());
    server.child.kill("SIGKILL");
  });
}

async function stopServer(server) {
  if (!server || !server.child) return;
  await killChild(server);
  cleanup(server);
}

function cleanup(server) {
  if (server && server.tmp) {
    fs.rmSync(server.tmp, { recursive: true, force: true });
  }
}

const test = base.test.extend({
  // Worker/test-scoped server; tears down (and removes the temp dir) even on
  // test failure.
  _server: [
    async ({}, use) => {
      const server = await startServer();
      try {
        await use(server);
      } finally {
        await stopServer(server);
      }
    },
    { auto: false },
  ],
  // Point every navigation/request at this test's own server.
  baseURL: async ({ _server }, use) => {
    await use(_server.baseURL);
  },
  // Kill this test's server mid-test to induce a *real* network failure (used by
  // the failing-like / failing-delete cases instead of a mock). Already-buffered
  // media keeps playing; only new fetches fail. Teardown still cleans up.
  killServer: async ({ _server }, use) => {
    await use(() => killChild(_server));
  },
});

const expect = base.expect;
module.exports = { test, expect };
