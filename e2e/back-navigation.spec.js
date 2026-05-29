const { test, expect } = require("@playwright/test");

// A boosted body swap (notably the browser Back/Forward button restoring a
// cached page) detaches #player-audio, which the browser pauses. These tests
// guard that playback resumes and the position is retained.

function mockAudio(page) {
  return page.route("**/test-audio/silence.wav", async (route) => {
    const sr = 8000, n = sr * 30, ds = n * 2, buf = Buffer.alloc(44 + ds);
    buf.write("RIFF", 0); buf.writeUInt32LE(36 + ds, 4); buf.write("WAVE", 8);
    buf.write("fmt ", 12); buf.writeUInt32LE(16, 16); buf.writeUInt16LE(1, 20);
    buf.writeUInt16LE(1, 22); buf.writeUInt32LE(sr, 24); buf.writeUInt32LE(sr * 2, 28);
    buf.writeUInt16LE(2, 32); buf.writeUInt16LE(16, 34); buf.write("data", 36);
    buf.writeUInt32LE(ds, 40);
    await route.fulfill({ status: 200, contentType: "audio/wav", body: buf });
  });
}

function mockMediaInfo(page) {
  return page.route("**/tracks/*/media-info", async (route) => {
    const m = route.request().url().match(/concerts\/(\d+)\/tracks\/(\d+)\/media-info/);
    await route.fulfill({
      status: 200, contentType: "application/json",
      body: JSON.stringify({
        url: "/test-audio/silence.wav", title: `Track ${m[2]} of C${m[1]}`,
        artist: `Artist ${m[1]}`, is_video: false, playable: true,
        track_index: parseInt(m[2]),
      }),
    });
  });
}

function isPlaying(page) {
  return page.waitForFunction(() => {
    const a = document.getElementById("player-audio");
    return a && !a.paused && a.currentTime > 0;
  });
}

async function startPlaying(page) {
  await page.locator(`#concert-3 button[onclick*="toggleTracks"]`).click();
  await page.waitForSelector(`[data-concert-id="3"][data-track-idx="1"]`);
  await page.locator(`[data-concert-id="3"][data-track-idx="1"]`).click();
  await isPlaying(page);
}

test.describe("Back/forward navigation keeps the player going", () => {
  test.beforeEach(async ({ page }) => {
    await mockAudio(page);
    await mockMediaInfo(page);
    await page.route("**/listen", (r) => r.fulfill({ status: 200, body: "" }));
    await page.goto("/");
  });

  test("Back button resumes playback at the retained position", async ({ page }) => {
    await startPlaying(page);
    const before = await page.evaluate(() => document.getElementById("player-audio").currentTime);

    await page.locator('header a[href="/settings"]').click();
    await page.waitForFunction(() => location.pathname === "/settings");

    await page.goBack();
    await page.waitForFunction(() => location.pathname === "/");

    // Playback resumes...
    await isPlaying(page);
    await expect(page.locator("#player-bar")).toHaveClass(/active/);
    // ...near where it left off (not reset to 0).
    const after = await page.evaluate(() => document.getElementById("player-audio").currentTime);
    expect(after).toBeGreaterThanOrEqual(before - 0.5);
  });

  test("Forward button also resumes playback", async ({ page }) => {
    await startPlaying(page);

    await page.locator('header a[href="/settings"]').click();
    await page.waitForFunction(() => location.pathname === "/settings");
    await page.goBack();
    await page.waitForFunction(() => location.pathname === "/");
    await isPlaying(page);

    await page.goForward();
    await page.waitForFunction(() => location.pathname === "/settings");
    await isPlaying(page);
  });
});
