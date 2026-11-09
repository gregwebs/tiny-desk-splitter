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

const { debugBin } = require("./cargo-target");

const REPO = path.resolve(__dirname, "..");
const BIN = debugBin("concert-web");
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
  // `BIN` follows CARGO_TARGET_DIR / CARGO_BUILD_TARGET (scripts/cargo-target.js).
  // Say so explicitly rather than failing later with a bare ENOENT from spawn.
  if (!fs.existsSync(BIN)) {
    throw new Error(
      `cargo build succeeded but ${BIN} does not exist — check CARGO_TARGET_DIR and CARGO_BUILD_TARGET`
    );
  }
}

// Spawn concert-web against a fresh scratch DB/workdir with both the app and
// Test Control API on ephemeral ports (--port 0 --test-control-port 0), and
// resolve once both "Listening on ..." lines have been printed. On any
// startup failure (timeout, early exit, spawn error) the caller is
// responsible for reaching the partially-started server via `activeServer`
// and cleaning it up — this function only rejects, it never leaves dangling
// listeners on the child.
//
// Everything one run creates lives under a single scratch root: the DB, the
// workdir, and the archive location handed to Hurl as `{{archive_location}}`.
// That last one has to be outside the workdir — archiving renames a concert's
// directory out of the workdir — and it has to be unique per run. Archiving
// renames `<workdir>/<album>` onto `<archive location>/<album>`, so a leftover
// directory at a fixed path makes the next run fail with ENOTEMPTY (issue
// #193); a per-run path means an aborted run can never poison a later one.
function startServer() {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "tds-hurl-"));
  const workdir = path.join(root, "workdir");
  const archiveLocation = path.join(root, "archive");
  const dbPath = path.join(root, "test.db");
  // Hand the root to `activeServer` before doing anything that can throw, so a
  // failed setup still reaches the caller's `cleanupServer` and removes it
  // rather than leaking a scratch directory.
  const server = { child: null, root, stdout: "", stderr: "", exited: false };
  activeServer = server;
  fs.mkdirSync(workdir);
  fs.mkdirSync(archiveLocation);
  const child = spawn(
    BIN,
    [
      "--db",
      dbPath,
      "--workdir",
      workdir,
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

  server.child = child;
  child.stdout.on("data", (data) => (server.stdout += data.toString()));
  child.stderr.on("data", (data) => (server.stderr += data.toString()));
  child.on("exit", () => (server.exited = true));
  // A child that never started (spawn error, e.g. ENOENT) emits `error` and
  // never `exit`, so `exited` has to cover it too — `stopServer` and every
  // cleanup path key off it.
  child.on("error", () => (server.exited = true));

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
          archiveLocation,
        });
      }
    };
    const onExit = () => {
      cleanup();
      reject(serverDiagnostics(server, "server exited before listening"));
    };
    const onError = (error) => {
      cleanup();
      // Set before rejecting: the `error` listener above has already run (or is
      // about to), so cleanup must not wait for an `exit` that cannot come.
      server.exited = true;
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
    `${summary}\nscratch root: ${server.root}\nstdout:\n${server.stdout || "(empty)"}\nstderr:\n${server.stderr || "(empty)"}`
  );
}

// Signals the child and resolves only once it has actually exited (SIGTERM,
// escalating to SIGKILL after STOP_GRACE_MS) — never deletes the scratch
// directory out from under a still-running process. A server whose child was
// never attached (setup threw between ownership of the root and the spawn) has
// nothing to stop, and a child that never started (spawn error, e.g. ENOENT)
// emits `error` instead of `exit`, so both events settle the wait.
function stopServer(server) {
  return new Promise((resolve) => {
    if (!server || !server.child || server.exited) {
      resolve();
      return;
    }
    const child = server.child;
    const escalate = setTimeout(() => {
      child.kill("SIGKILL");
    }, STOP_GRACE_MS);
    const settle = () => {
      clearTimeout(escalate);
      resolve();
    };
    child.once("exit", settle);
    child.once("error", settle);
    if (child.pid !== undefined) child.kill("SIGTERM");
  });
}

async function cleanupServer(server) {
  if (!server) return;
  await stopServer(server);
  // The whole scratch root — DB, workdir, and archive location — so an aborted
  // or Ctrl-C'd run leaves nothing behind for a later run to trip over.
  fs.rmSync(server.root, { recursive: true, force: true });
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

function runHurl(glob, { appPort, testControlPort, archiveLocation }) {
  console.log(`[hurl-test] running hurl --glob '${glob}'...`);
  const result = spawnSync(
    "hurl",
    [
      "--test",
      // All Hurl files in this suite share one concert-web process, one
      // scratch DB, and test.reset for isolation between files. --test
      // parallelizes across files by default, which would let a later file's
      // test.reset or seed calls race a still-running earlier file against
      // that shared state. --jobs 1 keeps files sequential.
      "--jobs",
      "1",
      "--glob",
      glob,
      "--variable",
      `base_url=http://127.0.0.1:${appPort}`,
      "--variable",
      `test_control_url=http://127.0.0.1:${testControlPort}`,
      // Per-run archive scratch location (see startServer). A Hurl file that
      // needs the media to leave the workdir reads this rather than inventing
      // a fixed /tmp path.
      "--variable",
      `archive_location=${archiveLocation}`,
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

  const { server, appPort, testControlPort, archiveLocation } = started;
  console.log(
    `[hurl-test] app on http://127.0.0.1:${appPort}, test control on http://127.0.0.1:${testControlPort}`
  );

  let exitCode;
  try {
    exitCode = runHurl(glob, { appPort, testControlPort, archiveLocation });
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
