// Where cargo puts build artifacts for this workspace, following the same
// overrides cargo itself does, so a gate that spawns a cargo-built binary still
// finds it when the build went somewhere other than <repo>/target:
//
//   CARGO_TARGET_DIR    the target directory (a relative value resolves against
//                       the repository root, which is the cwd every gate script
//                       invokes cargo from)
//   CARGO_BUILD_TARGET  when set, cargo nests the artifacts under the triple
//
// This is what makes the small-machine recipe in Dockerfile.dev — build into a
// scratch target dir with
// `CARGO_PROFILE_DEV_DEBUG=0 CARGO_TARGET_DIR=/tmp/td` — work for
// `just test-hurl` (scripts/hurl-test.js) and `npx playwright test`
// (e2e/fixtures.js), not only for `cargo build` itself.

const path = require("path");

const REPO = path.resolve(__dirname, "..");

function targetDir() {
  const configured = process.env.CARGO_TARGET_DIR;
  return configured
    ? path.resolve(REPO, configured)
    : path.join(REPO, "target");
}

/** Path to a debug binary built from this workspace, e.g. `debugBin("concert-web")`. */
function debugBin(name) {
  return path.join(
    targetDir(),
    process.env.CARGO_BUILD_TARGET || "",
    "debug",
    name
  );
}

module.exports = { targetDir, debugBin };
