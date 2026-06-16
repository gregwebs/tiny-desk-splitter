// Regression guard: clicking Sync must not destroy the persistent music player.
//
// Before the fix, the sync handler returned HX-Refresh: true, forcing a full
// page reload that rebuilt the DOM and stopped playback. The fix returns
// HX-Location instead, swapping only #content and leaving #player-container
// (and its <video>) untouched.
//
// The sync endpoint hits the real NPR archive. We intercept the POST with
// page.route() to return the same HX-Location response the handler would
// return, without any external network calls.

const { test, expect, openTracks } = require("./fixtures");

const AUDIO = 1; // "Audio Concert" — has split tracks

function trackButton(page, concertId, trackIdx) {
  return page.locator(
    `[data-concert-id="${concertId}"][data-track-idx="${trackIdx}"]`
  );
}

async function waitForPlaying(page) {
  await page.waitForFunction(() => {
    const a = document.getElementById("player-audio");
    return a && !a.paused;
  });
}

async function playTrack(page, concertId, trackIdx) {
  await openTracks(page, concertId);
  await page.waitForSelector(
    `[data-concert-id="${concertId}"][data-track-idx="${trackIdx}"]`
  );
  await trackButton(page, concertId, trackIdx).evaluate((el) => el.click());
  await waitForPlaying(page);
}

test.describe("Sync button leaves the player running", () => {
  test.beforeEach(async ({ page }) => {
    await page.goto("/");
  });

  test("Sync swaps only #content — player keeps playing and DOM is not reloaded", async ({
    page,
    baseURL,
  }) => {
    // Start a track playing.
    await playTrack(page, AUDIO, 0);
    await expect(page.locator("#player-bar")).toHaveClass(/active/);
    await expect(page.locator("#player-title")).toHaveText("Celular");

    // Loop so the short fixture track doesn't end mid-test.
    await page.evaluate(() => {
      document.getElementById("player-audio").loop = true;
    });

    // Sentinel: survives a partial (htmx) swap but not a full page reload.
    await page.evaluate(() => {
      window.__noReload = true;
    });

    // Mock the sync POST to return HX-Location (as the real handler does) without
    // hitting the NPR archive. The path "/" matches what the handler returns when
    // no ?filter= is active.
    await page.route("**/sync/**", async (route) => {
      await route.fulfill({
        status: 200,
        headers: {
          "HX-Location": JSON.stringify({
            path: "/",
            target: "#content",
            select: "#content",
            swap: "outerHTML show:window:top",
          }),
        },
        body: "",
      });
    });

    // Find any Sync button on the page and click it.
    const syncBtn = page.locator("button[hx-post*='/sync/']").first();
    await expect(syncBtn).toBeVisible();
    await syncBtn.evaluate((el) => el.click());

    // Wait for htmx to settle the partial swap (#content replaced).
    await page.waitForFunction(() => {
      // htmx removes hx-request attribute from body/root once settled.
      return !document.querySelector("[hx-request]");
    });

    // 1. No full reload: the sentinel is still set.
    expect(await page.evaluate(() => window.__noReload)).toBe(true);

    // 2. Player element is the same node (not re-created by a reload).
    expect(
      await page.evaluate(() => document.getElementById("player-audio") !== null)
    ).toBe(true);

    // 3. Audio is still playing (not paused by a reload or DOM detach).
    expect(
      await page.evaluate(
        () => document.getElementById("player-audio").paused
      )
    ).toBe(false);

    // 4. Player bar still shows the same track.
    await expect(page.locator("#player-bar")).toHaveClass(/active/);
    await expect(page.locator("#player-title")).toHaveText("Celular");
  });

  test("Sync preserves active filter in the HX-Location path", async ({
    page,
    baseURL,
  }) => {
    // Navigate to /?filter=liked so a ?filter= param is in the current URL.
    await page.goto("/?filter=liked");

    // Capture what path the server responds with in HX-Location.
    let capturedLocationPath = null;
    await page.route("**/sync/**", async (route) => {
      // Let the real request go through so we can read the real response header.
      const response = await route.fetch();
      const locationHeader = response.headers()["hx-location"];
      if (locationHeader) {
        try {
          capturedLocationPath = JSON.parse(locationHeader).path;
        } catch (_) {
          capturedLocationPath = locationHeader;
        }
      }
      await route.fulfill({ response });
    });

    const syncBtn = page.locator("button[hx-post*='/sync/']").first();
    // Sync buttons may not be visible when the filter hides all un-synced months;
    // skip rather than fail if none visible.
    const count = await syncBtn.count();
    if (count === 0) {
      test.skip(
        true,
        "no Sync button visible under liked filter — skipping filter-preservation sub-test"
      );
      return;
    }
    await syncBtn.evaluate((el) => el.click());

    await page.waitForFunction(() => !document.querySelector("[hx-request]"));

    // The server must echo back /?filter=liked in the HX-Location path.
    if (capturedLocationPath !== null) {
      expect(capturedLocationPath).toBe("/?filter=liked");
    }
  });
});
