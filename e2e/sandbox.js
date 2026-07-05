// Claude Code's sandbox blocks Chromium's multi-process Mach-port IPC, so
// Chromium there must run --single-process (and skip the egress proxy).
// Everywhere else those flags crash the browser at launch. Auto-detect via
// SANDBOX_RUNTIME (set by the sandbox wrapper itself, absent in a normal
// terminal); E2E_SANDBOX=1/0 forces either mode.
function detectSandbox() {
  const forced = process.env.E2E_SANDBOX;
  if (forced != null) {
    if (forced !== "1" && forced !== "0") {
      throw new Error(`E2E_SANDBOX must be "1" or "0", got: ${forced}`);
    }
    return forced === "1";
  }
  return process.env.SANDBOX_RUNTIME != null;
}

const isSandbox = detectSandbox();

const browserArgs = [
  "--autoplay-policy=no-user-gesture-required",
  ...(isSandbox ? ["--no-proxy-server", "--single-process"] : []),
];

module.exports = { isSandbox, browserArgs };
