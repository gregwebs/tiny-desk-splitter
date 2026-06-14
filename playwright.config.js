const { defineConfig } = require("@playwright/test");

module.exports = defineConfig({
  testDir: "./e2e",
  // Each test boots its own concert-web (see e2e/fixtures.js), so allow a little
  // headroom over a pure in-browser test.
  timeout: 45000,
  // Chromium runs --single-process in the sandbox (see use.launchOptions /
  // e2e/fixtures.js); parallel single-process instances crash under CPU contention.
  // Serialize to keep the suite deterministic. Determinism > wall-clock here.
  workers: 1,
  // Builds the concert-web binary + the pristine fixture (DB + media) once.
  globalSetup: require.resolve("./e2e/global-setup.js"),
  use: {
    browserName: "chromium",
    // Real media: let the player start tracks programmatically (auto-advance,
    // back/next) without a user gesture.
    // launchOptions are set per-test in e2e/fixtures.js (needed for sandbox).
    launchOptions: { args: ["--autoplay-policy=no-user-gesture-required", "--no-proxy-server", "--single-process"] },
  },
});
