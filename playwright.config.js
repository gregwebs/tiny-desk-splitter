const { defineConfig } = require("@playwright/test");

module.exports = defineConfig({
  testDir: "./e2e",
  // Each test boots its own concert-web (see e2e/fixtures.js), so allow a little
  // headroom over a pure in-browser test.
  timeout: 45000,
  // Builds the concert-web binary + the pristine fixture (DB + media) once.
  globalSetup: require.resolve("./e2e/global-setup.js"),
  use: {
    browserName: "chromium",
    // Real media: let the player start tracks programmatically (auto-advance,
    // back/next) without a user gesture.
    launchOptions: { args: ["--autoplay-policy=no-user-gesture-required"] },
  },
});
