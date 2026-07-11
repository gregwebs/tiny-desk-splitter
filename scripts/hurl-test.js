#!/usr/bin/env node
// Builds the test-control concert-web binary, starts it against a scratch
// DB/workdir with both the app and Test Control API on ephemeral ports, runs
// the Hurl suite against it, and tears everything down. See
// docs/change/2026-07-11-hurl-web-integration-tests.md and hurl/README.md.
//
// Usage: node scripts/hurl-test.js [--glob <pattern>]
// Default glob: hurl/*.hurl

const { spawn, spawnSync } = require("child_process");
const fs = require("fs");
const os = require("os");
const path = require("path");

const REPO = path.resolve(__dirname, "..");
const BIN = path.join(REPO, "target", "debug", "concert-web");
const SERVER_START_TIMEOUT_MS = 20_000;
// How long to wait for a graceful SIGTERM exit before escalating to SIGKILL.
// concert-web's own graceful-shutdown drain is capped at 2s (SHUTDOWN_GRACE
// in bin/concert_web.rs); this leaves headroom above that.
const STOP_GRACE_MS = 5_000;

const APP_LISTEN_RE = /^Listening on http:\/\/127\.0\.0\.1:(\d+)/m;
const TEST_CONTROL_LISTEN_RE =
  /^Test control listening on http:\/\/127\.0\.0\.1:(\d+)/m;

// Tracks whatever server is currently live so the SIGINT/SIGTERM handlers
// below can clean it up no matter where main() is suspended when the signal
// arrives (e.g. mid-`await startServer()`, or blocked inside the synchronous
// `spawnSync("hurl", ...)` call).
let activeServer = null;

function parseArgs(argv) {
  let glob = "hurl/*.hurl";
  for (let i = 0; i < argv.length; i++) {
    if (argv[i] === "--glob") {
      glob = argv[i + 1];
      i++;
    }
  }
  return { glob };
}

function buildBinary() {
  console.log("[hurl-test] building concert-web (--features test-control)...");
  const result = spawnSync(
    "cargo",
    ["build", "--bin", "concert-web", "--features", "test-control"],
    { cwd: REPO, stdio: "inherit" }
  );
  if (result.status !== 0) {
    throw new Error(`cargo build failed (exit ${result.status})`);
  }
}

// Spawn concert-web against a fresh scratch DB/workdir with both the app and
// Test Control API on ephemeral ports (--port 0 --test-control-port 0), and
// resolve once both "Listening on ..." lines have been printed. On any
// startup failure (timeout, early exit, spawn error) the caller is
// responsible for reaching the partially-started server via `activeServer`
// and cleaning it up — this function only rejects, it never leaves dangling
// listeners on the child.
function startServer() {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "tds-hurl-"));
  const child = spawn(
    BIN,
    [
      "--db",
      path.join(tmp, "test.db"),
      "--workdir",
      tmp,
      "--port",
      "0",
      "--test-control-port",
      "0",
      // No-op opener: this suite never exercises watch/Open.
      "--open-cmd",
      "true",
    ],
    { stdio: ["ignore", "pipe", "pipe"] }
  );

  const server = { child, tmp, stdout: "", stderr: "", exited: false };
  activeServer = server;
  child.stdout.on("data", (data) => (server.stdout += data.toString()));
  child.stderr.on("data", (data) => (server.stderr += data.toString()));
  child.on("exit", () => (server.exited = true));

  return new Promise((resolve, reject) => {
    const onData = () => {
      const appMatch = server.stdout.match(APP_LISTEN_RE);
      const tcMatch = server.stdout.match(TEST_CONTROL_LISTEN_RE);
      if (appMatch && tcMatch) {
        cleanup();
        resolve({
          server,
          appPort: parseInt(appMatch[1], 10),
          testControlPort: parseInt(tcMatch[1], 10),
        });
      }
    };
    const onExit = () => {
      cleanup();
      reject(serverDiagnostics(server, "server exited before listening"));
    };
    const onError = (error) => {
      cleanup();
      reject(
        new Error(
          `server process failed to start: ${error.message}\n${serverDiagnostics(server, "").message}`
        )
      );
    };
    const timer = setTimeout(() => {
      cleanup();
      reject(serverDiagnostics(server, "server start timed out"));
    }, SERVER_START_TIMEOUT_MS);
    const cleanup = () => {
      clearTimeout(timer);
      child.stdout.removeListener("data", onData);
      child.removeListener("exit", onExit);
      child.removeListener("error", onError);
    };
    child.stdout.on("data", onData);
    child.once("exit", onExit);
    child.once("error", onError);
  });
}

function serverDiagnostics(server, summary) {
  return new Error(
    `${summary}\nworkdir: ${server.tmp}\nstdout:\n${server.stdout || "(empty)"}\nstderr:\n${server.stderr || "(empty)"}`
  );
}

// Signals the child and resolves only once it has actually exited (SIGTERM,
// escalating to SIGKILL after STOP_GRACE_MS) — never deletes the scratch
// directory out from under a still-running process.
function stopServer(server) {
  return new Promise((resolve) => {
    if (!server || server.exited) {
      resolve();
      return;
    }
    const escalate = setTimeout(() => {
      server.child.kill("SIGKILL");
    }, STOP_GRACE_MS);
    server.child.once("exit", () => {
      clearTimeout(escalate);
      resolve();
    });
    server.child.kill("SIGTERM");
  });
}

async function cleanupServer(server) {
  if (!server) return;
  await stopServer(server);
  fs.rmSync(server.tmp, { recursive: true, force: true });
  if (activeServer === server) activeServer = null;
}

// Ctrl-C (SIGINT) or an external SIGTERM would otherwise kill this process
// immediately (Node's default disposition for both), skipping every
// `finally` cleanup path below and leaking the concert-web child plus its
// scratch DB/workdir. These handlers make cleanup run on that path too.
function installSignalHandlers() {
  const onSignal = (signal, exitCode) => {
    console.error(`\n[hurl-test] received ${signal}, cleaning up...`);
    cleanupServer(activeServer).finally(() => process.exit(exitCode));
  };
  process.on("SIGINT", () => onSignal("SIGINT", 130));
  process.on("SIGTERM", () => onSignal("SIGTERM", 143));
}

function runHurl(glob, appPort, testControlPort) {
  console.log(`[hurl-test] running hurl --glob '${glob}'...`);
  const result = spawnSync(
    "hurl",
    [
      "--test",
      "--glob",
      glob,
      "--variable",
      `base_url=http://127.0.0.1:${appPort}`,
      "--variable",
      `test_control_url=http://127.0.0.1:${testControlPort}`,
    ],
    { cwd: REPO, stdio: "inherit" }
  );
  return result.status ?? 1;
}

async function main() {
  installSignalHandlers();
  const { glob } = parseArgs(process.argv.slice(2));

  const binResult = spawnSync("which", ["hurl"], { stdio: "ignore" });
  if (binResult.status !== 0) {
    console.error(
      "[hurl-test] `hurl` is not installed or not on PATH. See https://hurl.dev/docs/installation.html"
    );
    process.exit(1);
  }

  buildBinary();

  let started;
  try {
    started = await startServer();
  } catch (error) {
    console.error(`[hurl-test] ${error.message}`);
    await cleanupServer(activeServer);
    process.exit(1);
    return;
  }

  const { server, appPort, testControlPort } = started;
  console.log(
    `[hurl-test] app on http://127.0.0.1:${appPort}, test control on http://127.0.0.1:${testControlPort}`
  );

  let exitCode;
  try {
    exitCode = runHurl(glob, appPort, testControlPort);
  } finally {
    await cleanupServer(server);
  }

  if (exitCode !== 0) {
    console.error("[hurl-test] hurl failed; server output for diagnosis:");
    console.error(`stdout:\n${server.stdout || "(empty)"}`);
    console.error(`stderr:\n${server.stderr || "(empty)"}`);
  }
  process.exit(exitCode);
}

main();
