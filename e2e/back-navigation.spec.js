const { test, expect } = require("./fixtures");

// hx-boost swaps <body> on navigation and the browser Back/Forward button
// restores a cached page; both detach/re-create #player-audio. These guard that
// the persistent player (inside the hx-preserve'd #player-container) survives
// that round trip — the bar stays active and the playing track is retained,
// rather than the player silently disappearing.
//
// Note: with real media, audible playback pauses and the position resets across
// a boosted Back/Forward; the browser's autoplay policy requires a user gesture
// to resume audible sound (verified manually). So these assert the durable
// player *state*, not that audio keeps sounding — the regression worth catching
// is the player getting wiped, which this covers.

const CONCERT = 1; // Audio Concert

async function startPlaying(page) {
  await page.locator(`#concert-${CONCERT} button[onclick*="toggleTracks"]`).click();
  await page.waitForSelector(`[data-concert-id="${CONCERT}"][data-track-idx="1"]`);
  await page.locator(`[data-concert-id="${CONCERT}"][data-track-idx="1"]`).click();
  // Limbo (track 1) is now the active player track.
  await expect(page.locator("#player-title")).toHaveText("Limbo");
  await page.waitForFunction(() => {
    const a = document.getElementById("player-audio");
    return a && !a.paused;
  });
}

test.describe("Back/forward navigation keeps the player present", () => {
  test.beforeEach(async ({ page }) => {
    await page.goto("/");
  });

  test("the player survives the Back button", async ({ page }) => {
    await startPlaying(page);

    await page.locator('header a[href="/settings"]').click();
    await page.waitForFunction(() => location.pathname === "/settings");

    await page.goBack();
    await page.waitForFunction(() => location.pathname === "/");

    // The player bar and its track context are retained across the boosted swap.
    await expect(page.locator("#player-bar")).toHaveClass(/active/);
    await expect(page.locator("#player-title")).toHaveText("Limbo");
  });

  test("the player survives Back then Forward", async ({ page }) => {
    await startPlaying(page);

    await page.locator('header a[href="/settings"]').click();
    await page.waitForFunction(() => location.pathname === "/settings");
    await page.goBack();
    await page.waitForFunction(() => location.pathname === "/");
    await expect(page.locator("#player-bar")).toHaveClass(/active/);

    await page.goForward();
    await page.waitForFunction(() => location.pathname === "/settings");

    // The player persists on the forward page too (it lives in every layout).
    await expect(page.locator("#player-bar")).toHaveClass(/active/);
    await expect(page.locator("#player-title")).toHaveText("Limbo");
  });
});
