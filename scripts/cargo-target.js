// Where cargo puts build artifacts for this workspace, following the same
// overrides cargo itself does, so a gate that spawns a cargo-built binary still
// finds it when the build went somewhere other than <repo>/target:
//
//   CARGO_TARGET_DIR    the target directory (a relative value resolves against
//                       the repository root, which is the cwd every gate script
//                       invokes cargo from)
//   CARGO_BUILD_TARGET  when set, cargo nests the artifacts under the triple
//
// This is what keeps `just test-hurl` (scripts/hurl-test.js) and
// `npx playwright test` (e2e/fixtures.js) working when the build went somewhere
// other than `<repo>/target` — a cross-compiled `--target`, or a
// `CARGO_TARGET_DIR` set by hand.

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
