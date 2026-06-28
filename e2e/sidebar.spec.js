"use strict";

const { test, expect, openTracks } = require("./fixtures");

// Fixture concerts (see player-queue.spec.js for full legend):
//   1 "Audio Concert"  — Celular(0), Limbo(1), Track Three(2), Dando Vueltas(3)
//   2 "Second Concert" — Song One(0), Song Two(1), Song Three(2)
//   4 "Liked Concert"  — Liked Song(0)  (liked in DB)
const AUDIO = 1;
const SECOND = 2;
const LIKED = 4;

function trackButton(page, concertId, trackIdx) {
  return page.locator(
    `[data-concert-id="${concertId}"][data-track-idx="${trackIdx}"]`
  );
}

async function expandTracks(page, concertId) {
  await openTracks(page, concertId);
  await page.waitForSelector(`[data-concert-id="${concertId}"][data-track-idx="0"]`);
}

async function waitForPlaying(page) {
  await page.waitForFunction(() => {
    const a = document.getElementById("player-audio");
    return a && !a.paused;
  });
}

async function playTrack(page, concertId, trackIdx) {
  await expandTracks(page, concertId);
  await trackButton(page, concertId, trackIdx).click();
  await waitForPlaying(page);
}

// Use Player API directly to avoid real pointer events on the player bar.
async function openSidebar(page) {
  await page.evaluate(() => Player.toggleSidebar());
  await page.waitForFunction(() => document.body.classList.contains("sidebar-open"));
}

async function closeSidebar(page) {
  await page.evaluate(() => {
    if (document.body.classList.contains("sidebar-open")) Player.toggleSidebar();
  });
  await page.waitForFunction(() => !document.body.classList.contains("sidebar-open"));
}

// Wait for at least one track button to appear in the sidebar concert section.
async function waitForSidebarTracks(page, concertId) {
  await page.waitForFunction((cid) => {
    const section = document.getElementById("sidebar-concert-section");
    return section != null && section.querySelector(`[data-concert-id="${cid}"]`) != null;
  }, concertId);
}

test.describe("Player Sidebar", () => {
  test.beforeEach(async ({ page }) => {
    await page.goto("/");
  });

  // ── Player bar reorder ────────────────────────────────────────────────────

  test("#player-queue-toggle precedes #player-info in the player bar", async ({ page }) => {
    await playTrack(page, AUDIO, 0);
    const orderOk = await page.evaluate(() => {
      const bar = document.getElementById("player-bar");
      const children = Array.from(bar.children);
      const toggleIdx = children.findIndex(c => c.id === "player-queue-toggle");
      const infoIdx   = children.findIndex(c => c.id === "player-info");
      return toggleIdx >= 0 && infoIdx >= 0 && toggleIdx < infoIdx;
    });
    expect(orderOk).toBe(true);
  });

  // ── Sidebar open / close ──────────────────────────────────────────────────

  test("toggle button opens and closes the sidebar", async ({ page }) => {
    await playTrack(page, AUDIO, 0);

    await openSidebar(page);
    await expect(page.locator("body")).toHaveClass(/sidebar-open/);
    await expect(page.locator("#player-queue-toggle")).toHaveAttribute("aria-expanded", "true");

    await closeSidebar(page);
    await expect(page.locator("body")).not.toHaveClass(/sidebar-open/);
    await expect(page.locator("#player-queue-toggle")).toHaveAttribute("aria-expanded", "false");
  });

  test("clicking #player-title toggles the sidebar", async ({ page }) => {
    await playTrack(page, AUDIO, 0);
    await page.evaluate(() => document.getElementById("player-title").click());
    await page.waitForFunction(() => document.body.classList.contains("sidebar-open"));

    await page.evaluate(() => document.getElementById("player-title").click());
    await page.waitForFunction(() => !document.body.classList.contains("sidebar-open"));
  });

  test("clicking #player-track toggles the sidebar", async ({ page }) => {
    await playTrack(page, AUDIO, 0);
    await page.evaluate(() => document.getElementById("player-track").click());
    await page.waitForFunction(() => document.body.classList.contains("sidebar-open"));
  });

  // ── Layout: page shrink on desktop ───────────────────────────────────────

  test("body gets margin-left of 320px when sidebar is open on desktop", async ({ page }) => {
    await page.setViewportSize({ width: 1280, height: 800 });
    await playTrack(page, AUDIO, 0);
    await openSidebar(page);

    // Wait for CSS transition to settle.
    await page.waitForFunction(() => {
      const ml = window.getComputedStyle(document.body).marginLeft;
      return ml === "320px";
    });
    const marginLeft = await page.evaluate(() => window.getComputedStyle(document.body).marginLeft);
    expect(marginLeft).toBe("320px");
  });

  test("video panel left equals sidebar width when sidebar is open on desktop", async ({ page }) => {
    await page.setViewportSize({ width: 1280, height: 800 });
    await playTrack(page, AUDIO, 0);
    await openSidebar(page);

    await page.waitForFunction(() => {
      const left = window.getComputedStyle(document.getElementById("player-video-panel")).left;
      return left === "320px";
    });
    const left = await page.evaluate(
      () => window.getComputedStyle(document.getElementById("player-video-panel")).left
    );
    expect(left).toBe("320px");
  });

  // ── Sidebar survives htmx navigation ──────────────────────────────────────

  test("sidebar stays open through htmx #content navigation", async ({ page }) => {
    await playTrack(page, AUDIO, 0);
    await openSidebar(page);

    // Navigate to the Jobs page (distinct from the listing) via htmx ajax — avoids
    // real pointer movement that crashes single-process Chromium.
    await page.evaluate(() => {
      window.htmx.ajax("GET", "/jobs", {
        target: "#content",
        select: "#content",
        swap: "outerHTML",
      });
    });
    // Jobs page has a unique element.
    await page.waitForSelector("#content table, #content .jobs-empty, #content h2");

    // Sidebar lives outside #content so it must survive the swap.
    await expect(page.locator("body")).toHaveClass(/sidebar-open/);
  });

  // ── Concert-tracks section ────────────────────────────────────────────────

  test("sidebar loads concert track list after opening", async ({ page }) => {
    await playTrack(page, AUDIO, 0);
    await openSidebar(page);
    await waitForSidebarTracks(page, AUDIO);

    // All 4 tracks should appear in the sidebar list.
    await expect(
      page.locator(`#sidebar-concert-section [data-concert-id="${AUDIO}"]`)
    ).toHaveCount(4);
  });

  test("playing track is highlighted in sidebar", async ({ page }) => {
    await playTrack(page, AUDIO, 0);
    await openSidebar(page);
    await waitForSidebarTracks(page, AUDIO);

    // Track 0 should carry .playing in the sidebar.
    await expect(
      page.locator(`#sidebar-concert-section [data-concert-id="${AUDIO}"][data-track-idx="0"]`)
    ).toHaveClass(/playing/);
    // Track 1 should not.
    await expect(
      page.locator(`#sidebar-concert-section [data-concert-id="${AUDIO}"][data-track-idx="1"]`)
    ).not.toHaveClass(/playing/);
  });

  test("liked concert loads with pre-liked track", async ({ page }) => {
    await playTrack(page, LIKED, 0);
    await openSidebar(page);
    await waitForSidebarTracks(page, LIKED);

    // The track's sidebar star should already be liked.
    await expect(
      page.locator(`#sidebar-concert-section .btn-like`).first()
    ).toHaveClass(/liked/);
  });

  test("player bar star change syncs to sidebar star", async ({ page }) => {
    await playTrack(page, AUDIO, 0);
    await openSidebar(page);
    await waitForSidebarTracks(page, AUDIO);

    // Track 0 starts unliked. Toggle via the player bar star (safe — stays in player-bar).
    await page.locator("#player-like").click();
    // Player bar star becomes liked.
    await expect(page.locator("#player-like")).toHaveClass(/liked/);

    // Sidebar star for track 0 should sync to liked.
    await page.waitForFunction(() => {
      const s = document.querySelector("#sidebar-concert-section .btn-like");
      return s && s.classList.contains("liked");
    });
  });

  test("sidebar loads new concert tracks when the playing concert changes", async ({ page }) => {
    await playTrack(page, AUDIO, 0);
    await openSidebar(page);
    await waitForSidebarTracks(page, AUDIO);

    // Start a track from a different concert via the Player API — avoids real pointer
    // events that can crash single-process Chromium when the player bar is active.
    await page.evaluate(() => Player.startTrack(null, 2, 0));
    await waitForPlaying(page);

    // Sidebar should now show the second concert's tracks.
    await waitForSidebarTracks(page, SECOND);
    await expect(
      page.locator(`#sidebar-concert-section [data-concert-id="${SECOND}"]`)
    ).toHaveCount(3);
  });

  test("sidebar delete on non-playing track keeps playback running", async ({ page }) => {
    await playTrack(page, AUDIO, 0);
    await page.evaluate(() => { document.getElementById("player-audio").loop = true; });
    await openSidebar(page);
    await waitForSidebarTracks(page, AUDIO);

    // Delete track 3 (not the playing one).
    await page.evaluate(() => Player.sidebarDeleteTrack(1, 3));

    // Playback continues on track 0.
    await page.waitForFunction(() => {
      const a = document.getElementById("player-audio");
      return a && !a.paused;
    });
    await expect(page.locator("#player-title")).toHaveText("Celular");

    // The track remains in the sidebar but is now greyed (unavailable).
    // Deleted tracks retain their row so the user can see the set list.
    await page.waitForFunction(() => {
      const section = document.getElementById("sidebar-concert-section");
      const btn = section && section.querySelector('[data-concert-id="1"][data-track-idx="3"]');
      return btn && btn.classList.contains("track-title-unavailable");
    });
  });

  test("sidebar delete on playing track advances to next", async ({ page }) => {
    await playTrack(page, AUDIO, 0);
    await openSidebar(page);
    await waitForSidebarTracks(page, AUDIO);

    // Delete the currently playing track.
    await page.evaluate(() => Player.sidebarDeleteTrack(1, 0));

    // Should advance to track 1 ("Limbo").
    await waitForPlaying(page);
    await expect(page.locator("#player-title")).toHaveText("Limbo");
  });

  // ── Queue section ─────────────────────────────────────────────────────────

  test("empty queue shows 'Nothing queued' message", async ({ page }) => {
    await playTrack(page, AUDIO, 0);
    await openSidebar(page);

    await expect(page.locator("#sidebar-queue-empty")).toBeVisible();
    await expect(page.locator("#sidebar-queue-empty")).toHaveText("Nothing queued");
    await expect(page.locator("#sidebar-queue-list .queue-song")).toHaveCount(0);
  });

  test("queued track appears in the sidebar list", async ({ page }) => {
    await playTrack(page, AUDIO, 0);
    await page.evaluate(() => { document.getElementById("player-audio").loop = true; });

    // Enqueue track 1 while track 0 is playing. Use evaluate to avoid real pointer
    // events being intercepted by the fixed player bar on top of the card list.
    await expandTracks(page, AUDIO);
    await trackButton(page, AUDIO, 1).evaluate(el => el.click());

    await openSidebar(page);

    await expect(page.locator("#sidebar-queue-empty")).not.toBeVisible();
    await expect(page.locator("#sidebar-queue-list .queue-song")).toHaveCount(1);
    await expect(page.locator("#sidebar-queue-list .btn-play-queue").first()).toHaveText("Limbo");
    // Remove button should NOT use the trash icon.
    await expect(page.locator("#sidebar-queue-list .btn-remove-queue").first()).toHaveText("×");
    await expect(page.locator("#sidebar-queue-list .icon-trash")).toHaveCount(0);
  });

  test("remove button deletes queue entry without affecting playback", async ({ page }) => {
    await playTrack(page, AUDIO, 0);
    await page.evaluate(() => { document.getElementById("player-audio").loop = true; });

    await expandTracks(page, AUDIO);
    await trackButton(page, AUDIO, 1).evaluate(el => el.click());

    await openSidebar(page);
    await expect(page.locator("#sidebar-queue-list .queue-song")).toHaveCount(1);

    await page.locator("#sidebar-queue-list .btn-remove-queue").first().click();

    await expect(page.locator("#sidebar-queue-list .queue-song")).toHaveCount(0);
    await expect(page.locator("#sidebar-queue-empty")).toBeVisible();
    // Playback unaffected.
    await expect(page.locator("#player-title")).toHaveText("Celular");
  });

  test("play-now button removes entry and immediately plays that track", async ({ page }) => {
    await playTrack(page, AUDIO, 0);
    await page.evaluate(() => { document.getElementById("player-audio").loop = true; });

    await expandTracks(page, AUDIO);
    await trackButton(page, AUDIO, 2).evaluate(el => el.click()); // queue Track Three

    await openSidebar(page);
    await expect(page.locator("#sidebar-queue-list .queue-song")).toHaveCount(1);

    await page.locator("#sidebar-queue-list .btn-play-queue").first().click();

    await waitForPlaying(page);
    await expect(page.locator("#player-title")).toHaveText("Track Three");
    await expect(page.locator("#sidebar-queue-list .queue-song")).toHaveCount(0);
  });
});
