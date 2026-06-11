const { test, expect, openTracks } = require("./fixtures");

// In-app navigation (list <-> detail, header/filter links) and the browser
// Back/Forward button swap only the #content region (per-link hx-target/
// hx-select="#content", and hx-history-elt="#content" for history restores).
// The persistent player lives in #player-container, a *sibling* of #content, so
// its <video id="player-audio"> node is never detached. These guard that the
// player keeps playing uninterrupted across navigation: the same media node is
// retained (not re-created) and currentTime keeps advancing — the regression
// this catches is a return to full-body swaps, which detach/re-create the audio
// node and force a reload + re-seek (the audible gap we removed).

const CONCERT = 1; // Audio Concert

async function startPlaying(page) {
  await openTracks(page, CONCERT);
  await page.locator(`[data-concert-id="${CONCERT}"][data-track-idx="1"]`).click();
  // Limbo (track 1) is now the active player track.
  await expect(page.locator("#player-title")).toHaveText("Limbo");
  await page.waitForFunction(() => {
    const a = document.getElementById("player-audio");
    return a && !a.paused;
  });
  // Tag the live media node so we can prove the *same* node survives navigation.
  await page.evaluate(() => {
    document.getElementById("player-audio").dataset.navMarker = "kept";
  });
}

// Assert the audio node was never re-created and playback advanced past `from`.
async function expectUninterrupted(page, from) {
  await expect(page.locator("#player-audio")).toHaveAttribute("data-nav-marker", "kept");
  await page.waitForFunction(
    (t) => {
      const a = document.getElementById("player-audio");
      return a && !a.paused && a.currentTime > t;
    },
    from,
    { timeout: 5000 }
  );
}

function currentTime(page) {
  return page.evaluate(() => document.getElementById("player-audio").currentTime);
}

test.describe("Navigation keeps the player playing uninterrupted", () => {
  test.beforeEach(async ({ page }) => {
    await page.goto("/");
  });

  test("list -> detail keeps the same audio node playing", async ({ page }) => {
    await startPlaying(page);
    const t0 = await currentTime(page);

    await page.locator(`#concert-${CONCERT} .card-title a`).click();
    await page.waitForFunction(
      (id) => location.pathname === `/concerts/${id}`,
      CONCERT
    );

    await expectUninterrupted(page, t0);
    await expect(page.locator("#player-title")).toHaveText("Limbo");
    // Exactly one #content region — guards the outerHTML/hx-select nesting bug.
    await expect(page.locator("#content")).toHaveCount(1);
  });

  test("Back button keeps the same audio node playing", async ({ page }) => {
    await startPlaying(page);

    await page.locator(`#concert-${CONCERT} .card-title a`).click();
    await page.waitForFunction(
      (id) => location.pathname === `/concerts/${id}`,
      CONCERT
    );
    const t0 = await currentTime(page);

    await page.goBack();
    await page.waitForFunction(() => location.pathname === "/");

    await expectUninterrupted(page, t0);
    await expect(page.locator("#player-bar")).toHaveClass(/active/);
    await expect(page.locator("#player-title")).toHaveText("Limbo");
    await expect(page.locator("#content")).toHaveCount(1);
  });

  test("player-bar artist link -> detail keeps the same audio node playing", async ({ page }) => {
    await startPlaying(page);
    const t0 = await currentTime(page);

    // The artist is a link to the playing concert's detail page. A plain click
    // must do an htmx partial swap of #content (not a full-page nav, which would
    // reload the page and stop playback).
    await expect(page.locator("#player-artist")).toHaveText("Audio Artist");
    await page.locator("#player-artist").click();
    await page.waitForFunction(
      (id) => location.pathname === `/concerts/${id}`,
      CONCERT
    );

    await expectUninterrupted(page, t0);
    await expect(page.locator("#player-title")).toHaveText("Limbo");
    await expect(page.locator("#content")).toHaveCount(1);

    // History was pushed (hx-push-url), so Back returns to the list.
    await page.goBack();
    await page.waitForFunction(() => location.pathname === "/");
    await expect(page.locator("#content")).toHaveCount(1);
  });

  test("Back then Forward keeps the same audio node playing", async ({ page }) => {
    await startPlaying(page);

    await page.locator('header a[href="/settings"]').click();
    await page.waitForFunction(() => location.pathname === "/settings");
    await page.goBack();
    await page.waitForFunction(() => location.pathname === "/");
    const t0 = await currentTime(page);

    await page.goForward();
    await page.waitForFunction(() => location.pathname === "/settings");

    // The player persists on the forward page too (it lives in every layout).
    await expectUninterrupted(page, t0);
    await expect(page.locator("#player-title")).toHaveText("Limbo");
  });
});
