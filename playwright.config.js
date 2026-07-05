const { defineConfig } = require("@playwright/test");
const { isSandbox } = require("./e2e/sandbox");

module.exports = defineConfig({
  testDir: "./e2e",
  // Each test boots its own concert-web (see e2e/fixtures.js), so allow a little
  // headroom over a pure in-browser test.
  timeout: 45000,
  // Chromium runs --single-process only inside the Claude Code sandbox (see
  // e2e/sandbox.js); parallel single-process instances crash under CPU
  // contention there, so serialize. Outside the sandbox each test already has
  // its own server/DB (see e2e/fixtures.js), so default parallelism is safe.
  workers: isSandbox ? 1 : undefined,
  // Builds the concert-web binary + the pristine fixture (DB + media) once.
  globalSetup: require.resolve("./e2e/global-setup.js"),
  use: {
    browserName: "chromium",
    // launchOptions are set per-test in e2e/fixtures.js (browser args are
    // environment-dependent — see e2e/sandbox.js).
  },
});
