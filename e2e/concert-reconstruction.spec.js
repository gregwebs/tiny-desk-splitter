"use strict";

const { test, expect } = require("./fixtures");

// Covers concert reconstruction playback (docs/change/2026-06-17-concert-reconstruction-playback.md).
//
// Uses fixture concert 2 ("Second Concert" — split, 3 songs, ~20s source wav).
// The full flow:
//   1. "Play concert" button is visible with source present.
//   2. After a gap split + source delete, "Play concert" opens reconstruction mode.
//   3. Reconstruction sidebar shows songs AND interludes.
//   4. Deleting an interlude from the sidebar removes it.
//   5. Normal per-track play does NOT show interludes in the sidebar.

const ID = 2;

// ── Splitter helpers (mirrors interlude-tracks.spec.js) ──────────────────────
const toggle = ".splitter-toggle";
const timeline = "#splitter .splitter-timeline";
const seg = "#splitter .splitter-seg";
const detachBtn = "#splitter .splitter-detach";
const submit = "#splitter .splitter-submit";
const status = "#splitter .splitter-status";
const rows = "#splitter .splitter-table tbody tr";

function startInput(page, i) {
  return page.locator(rows).nth(i).locator(".splitter-time").nth(0);
}
function endInput(page, i) {
  return page.locator(rows).nth(i).locator(".splitter-time").nth(1);
}

async function openSplitter(page) {
  await page.goto(`/concerts/${ID}`);
  await expect(page.locator(toggle)).toBeVisible();
  await page.click(toggle);
  await expect(page.locator(timeline)).toBeVisible();
  await expect(page.locator(seg)).toHaveCount(3);
}

async function submitGapSplit(page) {
  await page.locator(detachBtn).first().click();
  await endInput(page, 0).fill("0:05.0");
  await endInput(page, 0).blur();
  await startInput(page, 1).fill("0:08.0");
  await startInput(page, 1).blur();
  await page.click(submit);
  await expect(page.locator(status)).toContainText("Splitting");
}

async function waitForUserSplit(page) {
  await expect
    .poll(
      async () => {
        const r = await page.request.get(`/concerts/${ID}/split-timestamps`);
        const j = await r.json();
        return j.user !== null;
      },
      { timeout: 15000 }
    )
    .toBe(true);
}

// ── Player helpers ───────────────────────────────────────────────────────────

async function openSidebar(page) {
  await page.evaluate(() => Player.toggleSidebar());
  await page.waitForFunction(() => document.body.classList.contains("sidebar-open"));
}

async function waitForSidebarTracks(page) {
  await page.waitForFunction((cid) => {
    const section = document.getElementById("sidebar-concert-tracks");
    return section != null && section.querySelector(`[data-concert-id="${cid}"]`) != null;
  }, ID);
}

// ── Tests ────────────────────────────────────────────────────────────────────

test.describe("Concert reconstruction playback", () => {
  test("Play concert button is visible when source file is present", async ({ page }) => {
    await page.goto(`/concerts/${ID}`);
    const btn = page.locator(`#concert-${ID} button.btn-play-concert`);
    await expect(btn).toBeVisible();
    await expect(btn).toHaveText("Play concert");
  });

  test("reconstruction mode: source deleted → sidebar shows interludes → delete interlude works", async ({
    page,
  }) => {
    // Step 1: split with a gap to produce interlude_01.m4a.
    await openSplitter(page);
    await submitGapSplit(page);
    await waitForUserSplit(page);

    // Step 2: reload detail page; confirm source-redundant button appeared.
    await page.goto(`/concerts/${ID}`);
    const deleteRedundant = page.locator(`#concert-${ID} .btn-delete-redundant`);
    await expect(deleteRedundant).toBeVisible({ timeout: 8000 });

    // Step 3: delete the redundant source file.
    await deleteRedundant.click();
    await expect(deleteRedundant).toHaveCount(0);

    // Step 4: "Play concert" should still be visible (reconstruction is non-empty).
    const playConcert = page.locator(`#concert-${ID} button.btn-play-concert`);
    await expect(playConcert).toBeVisible();

    // Step 5: click "Play concert" — kicks off reconstruction playback.
    await playConcert.click();

    // Audio should start (reconstruction plays first song).
    await page.waitForFunction(() => {
      const a = document.getElementById("player-audio");
      return a && !a.paused;
    }, { timeout: 8000 });

    // Step 6: open sidebar and verify interludes are listed there.
    await openSidebar(page);
    await waitForSidebarTracks(page);

    // Sidebar should contain interlude buttons (data-interlude-idx).
    const interludeBtn = page.locator(
      `#sidebar-concert-tracks .btn-interlude[data-interlude-idx]`
    );
    await expect(interludeBtn).toBeVisible({ timeout: 5000 });

    // Song buttons are also present (data-track-idx).
    const songBtns = page.locator(
      `#sidebar-concert-tracks [data-concert-id="${ID}"][data-track-idx]`
    );
    await expect(songBtns).toHaveCount(3);

    // Step 7: delete the interlude from the sidebar.
    const interludeDeleteBtn = page.locator(
      `#sidebar-concert-tracks li:has(.btn-interlude) .btn-delete`
    ).first();
    await expect(interludeDeleteBtn).toBeVisible();
    await interludeDeleteBtn.click();

    // After deletion the interlude button is gone from the sidebar.
    await expect(interludeBtn).toHaveCount(0, { timeout: 5000 });

    // Songs are still listed.
    await expect(songBtns).toHaveCount(3);
  });

  test("normal per-track play does NOT show interludes in the sidebar", async ({ page }) => {
    // Play a specific track (not via Play concert).
    await page.goto(`/concerts/${ID}`);
    // Expand the inline track list so we can click a track button.
    await page.hover(`#concert-${ID}`, { position: { x: 20, y: 20 } });
    await page.waitForSelector(`#concert-${ID} .card-tracks-box ol.track-list`);

    const trackBtn = page.locator(
      `#concert-${ID} ol.track-list button.btn-track-listen[data-track-idx="0"]`
    );
    await expect(trackBtn).toBeVisible();
    await trackBtn.click();

    await page.waitForFunction(() => {
      const a = document.getElementById("player-audio");
      return a && !a.paused;
    }, { timeout: 8000 });

    // Open sidebar.
    await openSidebar(page);
    await waitForSidebarTracks(page);

    // No interlude buttons in normal per-track mode.
    const interludeBtns = page.locator(
      `#sidebar-concert-tracks .btn-interlude`
    );
    await expect(interludeBtns).toHaveCount(0);
  });
});
