// Runs once before the e2e suite: builds the concert-web binary and a pristine
// fixture (DB + tiny ffmpeg-generated media) under e2e/.fixture/. Each test then
// copies this fixture into its own temp dir and runs its own server (see
// fixtures.js). Requires ffmpeg on PATH.

const { execFileSync } = require("child_process");
const fs = require("fs");
const path = require("path");

const REPO = path.resolve(__dirname, "..");
const FIXTURE = path.join(__dirname, ".fixture");

module.exports = async () => {
  // Compiled binary (not `cargo run`) so per-test server spawn is fast.
  execFileSync("cargo", ["build", "--locked", "--bin", "concert-web"], {
    cwd: REPO,
    stdio: "inherit",
  });

  // Rebuild the fixture from scratch each run for determinism.
  fs.rmSync(FIXTURE, { recursive: true, force: true });
  fs.mkdirSync(FIXTURE, { recursive: true });

  execFileSync(
    "cargo",
    [
      "run",
      "--locked",
      "-q",
      "--example",
      "make_test_fixture",
      "-p",
      "concert-tracker",
      "--",
      FIXTURE,
      path.join(FIXTURE, "test.db"),
    ],
    { cwd: REPO, stdio: "inherit" }
  );
};
