const { test, expect, openTracks } = require("./fixtures");

// The automated splitting flow (docs/change/2026-06-11-automate-splitting.md):
// listing cards reveal their track list on hover (picture hides, and comes
// back on mouseleave); the manual Split / tracks-row Play / delete-split
// buttons are gone; clicking any track that has no file on disk POSTs
// /prepare, which splits (and downloads if needed) and then auto-plays the
// track. The split itself runs through stub-splitter.js — a real executable
// the server spawns — so no part of the chain is mocked.
//
// Fixture concerts used here:
//   1 "Audio Concert"          — split, 4 wav tracks (hover/button checks)
//   5 "Deleted-First Concert"  — split, track 0 deleted (re-split on play)
//   6 "Unsplit Concert"        — downloaded, never split (the prepare flow)
const AUDIO = 1;
const DELETED_FIRST = 5;
const UNSPLIT = 6;

const trackBtn = (concertId, idx) =>
  `[data-concert-id="${concertId}"][data-track-idx="${idx}"]`;
const tracksBtn = (concertId) => `#concert-${concertId} button.btn-tracks`;
const thumb = (concertId) => `#concert-${concertId} .card-thumb`;
const tracksBox = (concertId) => `#concert-${concertId} .card-tracks-box`;

async function waitForPlaying(page) {
  await page.waitForFunction(() => {
    const a = document.getElementById("player-audio");
    return a && !a.paused;
  });
}

test.describe("Hover reveals tracks on listing cards", () => {
  test("hover shrinks the picture and shows the tracks; mouseleave reverts; no refetch on second hover", async ({
    page,
  }) => {
    let tracksFetches = 0;
    page.on("request", (r) => {
      if (new URL(r.url()).pathname === `/concerts/${AUDIO}/tracks`) {
        tracksFetches++;
      }
    });

    const cardHeight = () =>
      page.locator(`#concert-${AUDIO}`).evaluate((el) => el.offsetHeight);
    const thumbHeight = () =>
      page.locator(thumb(AUDIO)).evaluate((el) => el.offsetHeight);

    await page.goto("/");
    await expect(page.locator(thumb(AUDIO))).toBeVisible();
    await expect(page.locator(tracksBox(AUDIO))).toBeHidden();
    const cardBefore = await cardHeight();
    const thumbBefore = await thumbHeight();

    await openTracks(page, AUDIO);
    // The picture stays visible but shrinks to a banner strip; the track list
    // fills the freed space and the card height does not change.
    await expect(page.locator(thumb(AUDIO))).toBeVisible();
    expect(await thumbHeight()).toBeLessThan(thumbBefore);
    await expect(page.locator(tracksBox(AUDIO))).toBeVisible();
    await expect(
      page.locator(`${tracksBox(AUDIO)} ol.track-list li`)
    ).toHaveCount(4);
    expect(Math.abs((await cardHeight()) - cardBefore)).toBeLessThanOrEqual(1);

    // Mouse leaves the card: picture returns to full size, list hides but
    // stays in the DOM as a cache.
    await page.hover("header");
    await expect(page.locator(thumb(AUDIO))).toBeVisible();
    expect(await thumbHeight()).toBe(thumbBefore);
    await expect(page.locator(tracksBox(AUDIO))).toBeHidden();

    // Second hover shows the cached list without another fetch.
    await openTracks(page, AUDIO);
    await expect(page.locator(tracksBox(AUDIO))).toBeVisible();
    expect(tracksFetches).toBe(1);
  });

  test("an unsplit concert still shows its tracks on hover", async ({
    page,
  }) => {
    await page.goto("/");
    await expect(page.locator(tracksBtn(UNSPLIT))).toHaveText("not-split (0/2)");

    await openTracks(page, UNSPLIT);
    const items = page.locator(`${tracksBox(UNSPLIT)} ol.track-list li`);
    await expect(items).toHaveCount(2);
    // Unsplit tracks render as clickable (unavailable-styled) buttons.
    await expect(
      page.locator(
        `${tracksBox(UNSPLIT)} button.btn-track-listen.track-title-unavailable`
      )
    ).toHaveCount(2);
  });
});

test.describe("Removed manual controls", () => {
  test("no Split, tracks-row Play, or delete-split buttons anywhere", async ({
    page,
  }) => {
    await page.goto("/");
    await expect(page.locator('button[hx-post$="/split"]')).toHaveCount(0);
    await expect(page.locator('button[hx-post$="/delete-split"]')).toHaveCount(0);
    // The only Play button left is the album one in the download slot; the
    // tracks row holds just the tracks button.
    await expect(
      page.locator(`#concert-${AUDIO} .card-tracks-row button`)
    ).toHaveCount(1);
  });
});

test.describe("Automated split on play", () => {
  test("clicking an unsplit track prepares (splits) and auto-plays it", async ({
    page,
  }) => {
    await page.goto("/");
    await openTracks(page, UNSPLIT);

    await page.locator(trackBtn(UNSPLIT, 0)).click();

    // The clicked track is marked pending and the player bar reports progress
    // while the (deliberately slowed) stub splitter runs.
    await expect(page.locator(`${trackBtn(UNSPLIT, 0)}.preparing`)).toBeVisible();
    await expect(page.locator("#player-status")).toContainText("Preparing");

    // When the split lands, the track auto-plays.
    await expect(page.locator("#player-title")).toHaveText("First Song", {
      timeout: 15000,
    });
    await waitForPlaying(page);

    // The card caught up via its status polling: split status + full count.
    await expect(page.locator(tracksBtn(UNSPLIT))).toHaveText("tracks (2)", {
      timeout: 15000,
    });
  });

  test("the tracks button on an unsplit concert prepares and auto-plays", async ({
    page,
  }) => {
    await page.goto("/");
    // Hover first so the card's hover layout swap settles before the click.
    await openTracks(page, UNSPLIT);
    await page.locator(tracksBtn(UNSPLIT)).click();

    await expect(page.locator("#player-title")).toHaveText("First Song", {
      timeout: 15000,
    });
    await waitForPlaying(page);
  });

  test("clicking a deleted track re-splits and restores it", async ({
    page,
  }) => {
    await page.goto("/");
    await expect(page.locator(tracksBtn(DELETED_FIRST))).toHaveText(
      "tracks (2/3)"
    );
    await openTracks(page, DELETED_FIRST);

    // "Gone Opener" (track 0) has no file; clicking it triggers the re-split.
    await page.locator(trackBtn(DELETED_FIRST, 0)).click();

    await expect(page.locator("#player-title")).toHaveText("Gone Opener", {
      timeout: 15000,
    });
    await waitForPlaying(page);

    // The re-split restored every deleted track.
    await expect(page.locator(tracksBtn(DELETED_FIRST))).toHaveText(
      "tracks (3)",
      { timeout: 15000 }
    );
  });

  test("the detail page prepares and auto-plays too", async ({ page }) => {
    await page.goto(`/concerts/${UNSPLIT}`);
    // Picture and tracks are both visible on the detail card.
    await expect(page.locator(thumb(UNSPLIT))).toBeVisible();
    await expect(
      page.locator(`${tracksBox(UNSPLIT)} ol.track-list li`)
    ).toHaveCount(2);

    await page.locator(trackBtn(UNSPLIT, 1)).click();

    await expect(page.locator("#player-title")).toHaveText("Second Song", {
      timeout: 15000,
    });
    await waitForPlaying(page);
  });
});
