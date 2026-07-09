const { test, expect } = require("./fixtures");

// Covers the interlude-tracks feature (docs/change/2026-06-16-gap-interlude-tracks.md).
// When a user detaches a track boundary and creates a gap, submitting triggers a
// UserTimestamps split that emits interlude_NN.m4a files covering the uncovered
// spans. Once all songs + all interludes cover [0, media_duration], the
// "Delete redundant source" button appears on the detail page.
//
// Uses fixture concert 2 ("Second Concert" — split, 3 playable wav tracks, ~20s
// source). The stub-splitter handles --emit-interludes and creates interlude_NN.m4a
// stubs alongside the song copies so the gate can verify full coverage.

const ID = 2;

const toggle = ".splitter-toggle";
const timeline = "#splitter .splitter-timeline";
const seg = "#splitter .splitter-seg";
const gap = "#splitter .splitter-gap";
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

// Open the splitter panel on the concert detail page.
async function openSplitter(page) {
  await page.goto(`/concerts/${ID}`);
  await expect(page.locator(toggle)).toBeVisible();
  await page.click(toggle);
  await expect(page.locator(timeline)).toBeVisible();
  await expect(page.locator(seg)).toHaveCount(3);
}

// The gap block's width (0 while hidden) is derived from the *committed*
// model (editor.tracks), not from whatever an input's own DOM value happens
// to show — so polling it (rather than the just-typed input's toHaveValue)
// proves a `change` event actually landed before the next interaction, closing
// the race where a second fill/blur can fire its `change` while a re-render
// from the first edit is still in flight (see docs/change/
// 2026-07-08-fix-failing-e2e-tests.md).
async function gapWidthPx(page) {
  const box = await page.locator(gap).first().boundingBox();
  return box ? box.width : 0;
}

// Detach the first boundary, open a 3-second gap (5s–8s), and submit.
// Returns once the submit button is clicked (not once the split completes).
async function submitGapSplit(page) {
  await page.locator(detachBtn).first().click();
  await endInput(page, 0).fill("0:05.0");
  await endInput(page, 0).blur();
  // A gap block becomes visible once end[0] has actually committed away from
  // the still-linked start[1] — proves the first edit reached the model.
  await expect(page.locator(gap).first()).toBeVisible();
  const widthAfterFirstEdit = await gapWidthPx(page);

  await startInput(page, 1).fill("0:08.0");
  await startInput(page, 1).blur();
  // Wait for the gap to widen (start[1] moving from its stale auto value to
  // 8.0) before submitting — proves the second edit committed too.
  await expect
    .poll(() => gapWidthPx(page), { timeout: 5000 })
    .toBeGreaterThan(widthAfterFirstEdit * 1.5);

  await page.click(submit);
  await expect(page.locator(status)).toContainText("Splitting");
}

// Poll until user timestamps are stored in the DB (split job completed).
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

test.describe("interlude tracks and source-redundant gate", () => {
  test("source-redundant button appears after gap split and deleting it removes the source", async ({
    page,
  }) => {
    await openSplitter(page);
    await submitGapSplit(page);
    await waitForUserSplit(page);

    // Reload the detail page — source_redundant is computed at render time.
    await page.goto(`/concerts/${ID}`);

    // The button should now be visible (all songs present + interlude_01.m4a created).
    const deleteBtn = page.locator(`#concert-${ID} .btn-delete-redundant`);
    await expect(deleteBtn).toBeVisible({ timeout: 5000 });

    // "Play concert" is visible because the source file is still present.
    const playConcert = page.locator(`#concert-${ID} button.btn-play-concert`);
    await expect(playConcert).toBeVisible();

    // Click the gated delete button.
    await deleteBtn.click();

    // After deletion the card is swapped: source-redundant button disappears.
    // "Play concert" stays visible — reconstruction mode kicks in since tracks remain.
    await expect(deleteBtn).toHaveCount(0);
    await expect(playConcert).toBeVisible();

    // Song tracks are still present and playable.
    await expect(
      page.locator(`#concert-${ID} ol.track-list button.btn-track-listen`)
    ).toHaveCount(3);
  });

  test("source-redundant button is absent when a song track has been deleted", async ({
    page,
  }) => {
    await openSplitter(page);
    await submitGapSplit(page);
    await waitForUserSplit(page);

    // Delete the first song track so tracks_present has a false entry.
    const deleteTrackBtn = page.locator(
      `#concert-${ID} ol.track-list button.btn-delete[hx-post$="/tracks/0/delete"]`
    );
    // Navigate to the detail page (splitter is already there; also ensures track list visible).
    await page.goto(`/concerts/${ID}`);
    await page.waitForSelector(
      `#concert-${ID} ol.track-list button.btn-delete`
    );
    await page.locator(
      `#concert-${ID} ol.track-list button.btn-delete[hx-post$="/tracks/0/delete"]`
    ).click();
    // Wait for the card swap to settle.
    await expect(
      page.locator(`#concert-${ID} ol.track-list .track-title-unavailable`)
    ).toHaveCount(1);

    // Reload to get a fresh server-rendered card.
    await page.goto(`/concerts/${ID}`);

    // Source-redundant button must NOT be present — fails-closed on missing track.
    await expect(
      page.locator(`#concert-${ID} .btn-delete-redundant`)
    ).toHaveCount(0);
  });
});
