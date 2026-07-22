// Per-test isolation: every test gets its own copy of the pristine fixture
// (DB + media) in a temp dir and its own concert-web on an ephemeral port, so
// tests can mutate state (delete tracks, toggle likes) without interfering and
// never touch the real concerts.db. Import { test, expect } from here instead
// of from @playwright/test.

const base = require("@playwright/test");
const { chromium } = base;
const { spawn } = require("child_process");
const fs = require("fs");
const os = require("os");
const path = require("path");

const { browserArgs: BROWSER_ARGS } = require("./sandbox");

const REPO = path.resolve(__dirname, "..");
const FIXTURE = path.join(__dirname, ".fixture");
const BIN = path.join(REPO, "target", "debug", "concert-web");
const SERVER_START_TIMEOUT_MS = 20_000;
const READINESS_ATTEMPTS = 50;
const READINESS_DELAY_MS = 100;
const READINESS_REQUEST_TIMEOUT_MS = 1_000;

const ServerState = Object.freeze({
  STARTING: "starting",
  READINESS: "readiness",
  RUNNING: "running",
  EXPECTED_STOP: "expected-stop",
  UNEXPECTED_EXIT: "unexpected-exit",
  STOPPED: "stopped",
});

function serverDiagnostics(server, summary) {
  const exit =
    server.exitCode !== null || server.signalCode !== null
      ? `\nexit code: ${server.exitCode ?? "none"}; signal: ${server.signalCode ?? "none"}`
      : "";
  const processError = server.processError
    ? `\nprocess error: ${server.processError.message}`
    : "";
  return new Error(
    `${summary}\nstate: ${server.state}${exit}${processError}\nworkdir: ${server.tmp}\nstdout:\n${server.stdout || "(empty)"}\nstderr:\n${server.stderr || "(empty)"}`
  );
}

async function waitForReadiness(server) {
  let lastResult = "no request attempted";
  for (let attempt = 1; attempt <= READINESS_ATTEMPTS; attempt++) {
    if (server.exited) {
      throw serverDiagnostics(
        server,
        `server exited while waiting for ${server.baseURL}/`
      );
    }
    try {
      const response = await fetch(`${server.baseURL}/`, {
        signal: AbortSignal.timeout(READINESS_REQUEST_TIMEOUT_MS),
      });
      lastResult = `HTTP ${response.status}`;
      await response.body?.cancel();
      if (response.ok) return;
    } catch (error) {
      lastResult = error instanceof Error ? error.message : String(error);
    }
    await new Promise((resolve) => setTimeout(resolve, READINESS_DELAY_MS));
  }
  throw serverDiagnostics(
    server,
    `server readiness failed after ${READINESS_ATTEMPTS} attempts; last result: ${lastResult}`
  );
}

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
      // Stub splitter (real executable, no mock): "splits" by copying the
      // full-concert file to one playable file per set-list song, so the
      // automated split-on-play flow is testable end to end. Requires
      // --splitter cli (#141 made the in-process library adapter the
      // default, which has no --splitter-bin seam) so the subprocess is the
      // stub rather than the real splitter.
      "--splitter",
      "cli",
      "--splitter-bin",
      path.join(__dirname, "stub-splitter.js"),
    ],
    { stdio: ["ignore", "pipe", "pipe"] }
  );

  // child.exitCode stays null for signal-killed processes (signalCode is set
  // instead), so retain both values and an explicit lifecycle state.
  const server = {
    child,
    tmp,
    baseURL: null,
    exited: false,
    exitCode: null,
    signalCode: null,
    processError: null,
    spawnFailed: false,
    unexpectedProcessError: false,
    state: ServerState.STARTING,
    stdout: "",
    stderr: "",
  };
  child.stdout.on("data", (data) => (server.stdout += data.toString()));
  child.stderr.on("data", (data) => (server.stderr += data.toString()));
  child.on("exit", (code, signal) => {
    server.exited = true;
    server.exitCode = code;
    server.signalCode = signal;
    if (server.state === ServerState.RUNNING) {
      server.state = ServerState.UNEXPECTED_EXIT;
    } else if (server.state === ServerState.EXPECTED_STOP) {
      server.state = ServerState.STOPPED;
    }
  });
  child.on("error", (error) => {
    server.processError = error;
    if (child.pid === undefined) {
      server.spawnFailed = true;
      server.exited = true;
    } else if (server.state === ServerState.RUNNING) {
      server.unexpectedProcessError = true;
    }
  });

  try {
    const port = await new Promise((resolve, reject) => {
      const cleanupStartupListeners = () => {
        clearTimeout(timer);
        child.stdout.removeListener("data", onData);
        child.removeListener("exit", onExit);
        child.removeListener("error", onError);
      };
      const onData = () => {
        const match = server.stdout.match(
          /Listening on http:\/\/127\.0\.0\.1:(\d+)/
        );
        if (match) {
          cleanupStartupListeners();
          resolve(parseInt(match[1], 10));
        }
      };
      const onExit = () => {
        cleanupStartupListeners();
        reject(serverDiagnostics(server, "server exited before listening"));
      };
      const onError = () => {
        cleanupStartupListeners();
        reject(serverDiagnostics(server, "server process failed to start"));
      };
      const timer = setTimeout(
        () => {
          cleanupStartupListeners();
          reject(serverDiagnostics(server, "server start timed out"));
        },
        SERVER_START_TIMEOUT_MS
      );
      child.stdout.on("data", onData);
      child.once("exit", onExit);
      child.once("error", onError);
    });

    server.baseURL = `http://127.0.0.1:${port}`;
    server.state = ServerState.READINESS;
    await waitForReadiness(server);
    if (server.exited) {
      throw serverDiagnostics(server, "server exited after readiness");
    }
    server.state = ServerState.RUNNING;
    return server;
  } catch (error) {
    try {
      await killChild(server);
    } catch (killError) {
      throw new AggregateError(
        [error, killError],
        "server startup failed and the child could not be stopped"
      );
    } finally {
      if (server.exited || server.spawnFailed) cleanup(server);
    }
    throw error;
  }
}

function killChild(server) {
  return new Promise((resolve, reject) => {
    if (server.exited || server.spawnFailed) return resolve();
    server.state = ServerState.EXPECTED_STOP;
    const cleanupListeners = () => {
      server.child.removeListener("exit", onExit);
      server.child.removeListener("error", onError);
    };
    const onExit = () => {
      cleanupListeners();
      resolve();
    };
    const onError = () => {
      cleanupListeners();
      reject(serverDiagnostics(server, "server process reported a stop error"));
    };
    server.child.once("exit", onExit);
    server.child.once("error", onError);
    try {
      if (!server.child.kill("SIGKILL")) {
        cleanupListeners();
        reject(serverDiagnostics(server, "failed to signal server process"));
      }
    } catch (error) {
      server.processError = error;
      cleanupListeners();
      reject(serverDiagnostics(server, "failed to signal server process"));
    }
  });
}

async function stopServer(server) {
  if (!server || !server.child) return;
  try {
    await killChild(server);
  } finally {
    // Removing a live child's workdir can corrupt its state. A stop failure
    // therefore preserves the directory and reports its path for diagnosis.
    if (server.exited || server.spawnFailed) cleanup(server);
  }
}

function cleanup(server) {
  if (server && server.tmp) {
    fs.rmSync(server.tmp, { recursive: true, force: true });
  }
}

const test = base.test.extend({
  // Per-test browser. The built-in `browser` fixture is worker-scoped and its
  // scope can't be changed, so we use a private fixture and override `context`
  // and `page` to flow through it. In the sandbox, Chromium's --single-process
  // mode can crash during browserContext cleanup, killing the shared worker
  // browser and making every subsequent test fail; per-test launch isolates
  // those crashes. Outside the sandbox a per-test launch is unnecessary but
  // cheap, so it's kept as the one code path for both modes rather than
  // branching back to the worker-scoped fixture.
  _ownBrowser: [
    async ({}, use) => {
      const browser = await chromium.launch({ args: BROWSER_ARGS });
      await use(browser);
      await browser.close().catch(() => {});
    },
    { scope: "test" },
  ],
  context: async ({ _ownBrowser, baseURL }, use) => {
    const ctx = await _ownBrowser.newContext({ baseURL });
    await use(ctx);
    await ctx.close().catch(() => {});
  },
  page: async ({ context }, use) => {
    const page = await context.newPage();
    await use(page);
  },
  // Test-scoped server; teardown removes the temp dir after confirmed exit.
  // A failed stop preserves it because deleting a live child's workdir is unsafe.
  _server: [
    async ({}, use, testInfo) => {
      const server = await startServer();
      try {
        await use(server);
      } finally {
        let stopError = null;
        try {
          await stopServer(server);
        } catch (error) {
          stopError = error;
        }
        if (
          server.state === ServerState.UNEXPECTED_EXIT ||
          server.unexpectedProcessError
        ) {
          await testInfo.attach("concert-web-stdout", {
            body: server.stdout,
            contentType: "text/plain",
          });
          await testInfo.attach("concert-web-stderr", {
            body: server.stderr,
            contentType: "text/plain",
          });
          const unexpectedError = serverDiagnostics(
            server,
            "concert-web failed unexpectedly during the test"
          );
          if (stopError) {
            throw new AggregateError(
              [unexpectedError, stopError],
              "concert-web failed during the test and could not be stopped"
            );
          }
          throw unexpectedError;
        }
        if (stopError) throw stopError;
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

// Reveal a listing card's track list: visibility is pure CSS (:hover on the
// card) and the list HTML is fetched on first hover. Hover near the top-left
// corner so the pointer stays inside the card while the picture shrinks to
// its banner strip (the card height itself never changes).
async function openTracks(page, concertId) {
  await page.hover(`#concert-${concertId}`, { position: { x: 20, y: 20 } });
  await page.waitForSelector(
    `#concert-${concertId} .card-tracks-box ol.track-list`
  );
}

const expect = base.expect;
module.exports = { test, expect, openTracks };
