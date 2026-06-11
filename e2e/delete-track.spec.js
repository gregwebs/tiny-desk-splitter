const { test, expect, openTracks } = require("./fixtures");

// Drives the real /delete endpoint against the isolated fixture (concert 1,
// "Audio Concert": Celular, Limbo, Track Three, Dando Vueltas). Per-test
// isolation makes the destructive clicks safe. Deleting swaps the whole
// concert card (hx-target="closest .card") with the track list re-rendered
// embedded, so the tracks-button count refreshes and the swapped fragment's
// new delete buttons must stay wired.
const CONCERT_ID = 1;

const listenBtn = (idx) =>
  `#concert-${CONCERT_ID} ol.track-list button.btn-track-listen[data-track-idx="${idx}"]`;
const availableBtn = (idx) =>
  `#concert-${CONCERT_ID} ol.track-list button.btn-track-listen[data-track-idx="${idx}"]:not(.track-title-unavailable)`;
const deleteBtn = (idx) =>
  `#concert-${CONCERT_ID} ol.track-list button.btn-delete[hx-post$="/tracks/${idx}/delete"]`;
const tracksBtn = `#concert-${CONCERT_ID} button.btn-tracks`;

async function expandTracks(page) {
  await openTracks(page, CONCERT_ID);
  await page.waitForSelector(
    `#concert-${CONCERT_ID} ol.track-list li button.btn-delete`
  );
}

test.describe("Delete track via ✕ button", () => {
  test("clicking ✕ on a track removes it and updates the tracks-button count", async ({
    page,
  }) => {
    await page.goto("/");
    await expandTracks(page);

    await expect(page.locator(tracksBtn)).toHaveText("tracks (4)");
    await expect(page.locator(availableBtn(0))).toHaveText("Celular");
    await page.locator(deleteBtn(0)).click();

    // After the real delete the track has no file: its delete button is gone
    // and it renders as an unavailable (but still clickable — it would trigger
    // a re-split) title in place.
    await expect(page.locator(availableBtn(0))).toHaveCount(0);
    await expect(page.locator(deleteBtn(0))).toHaveCount(0);
    await expect(
      page.locator(`#concert-${CONCERT_ID} ol.track-list .track-title-unavailable`)
    ).toHaveText("Celular");
    // Other tracks are untouched.
    await expect(page.locator(availableBtn(1))).toHaveText("Limbo");
    // The card swap refreshed the count and kept the list populated.
    await expect(page.locator(tracksBtn)).toHaveText("tracks (3/4)");
    await expect(
      page.locator(`#concert-${CONCERT_ID} ol.track-list li`)
    ).toHaveCount(4);
  });

  test("subsequent ✕ clicks after a card swap still fire delete requests", async ({
    page,
  }) => {
    // After the first delete swaps the whole card, the delete buttons in the
    // re-rendered embedded list must also be wired up so a second one works.
    await page.goto("/");
    await expandTracks(page);

    await page.locator(deleteBtn(0)).click();
    await expect(page.locator(deleteBtn(0))).toHaveCount(0);

    await page.locator(deleteBtn(1)).click();
    await expect(page.locator(deleteBtn(1))).toHaveCount(0);

    // Both deleted tracks now render as unavailable; a later one still plays.
    await expect(
      page.locator(`#concert-${CONCERT_ID} ol.track-list .track-title-unavailable`)
    ).toHaveCount(2);
    await expect(page.locator(availableBtn(2))).toHaveText("Track Three");
    await expect(page.locator(tracksBtn)).toHaveText("tracks (2/4)");
  });

  test("the detail page shows picture and tracks together, with delete buttons", async ({
    page,
  }) => {
    // The detail card always shows both its image and the embedded track
    // list (the hover swap applies only to the listing), and per-track
    // deletion works there too.
    await page.goto(`/concerts/${CONCERT_ID}`);
    await expect(
      page.locator(`#concert-${CONCERT_ID} .card-thumb`)
    ).toBeVisible();
    await expect(
      page.locator(`#concert-${CONCERT_ID} ol.track-list`)
    ).toBeVisible();
    await expect(page.locator(`${listenBtn(0)}`)).toHaveText("Celular");
    await expect(
      page.locator(`#concert-${CONCERT_ID} ol.track-list button.btn-delete`)
    ).toHaveCount(4);
  });

  test("✕ in the detail page card's list updates that card's count", async ({
    page,
  }) => {
    await page.goto(`/concerts/${CONCERT_ID}`);

    await expect(page.locator(tracksBtn)).toHaveText("tracks (4)");
    await page.locator(deleteBtn(0)).click();

    await expect(page.locator(deleteBtn(0))).toHaveCount(0);
    await expect(page.locator(tracksBtn)).toHaveText("tracks (3/4)");
    // The card's list stays visible across the swap; the image stays too.
    await expect(
      page.locator(`#concert-${CONCERT_ID} ol.track-list li`)
    ).toHaveCount(4);
    await expect(
      page.locator(`#concert-${CONCERT_ID} .card-thumb`)
    ).toBeVisible();
  });

  test("deleting every track clears the split state on the card", async ({
    page,
  }) => {
    await page.goto("/");
    await expandTracks(page);

    for (const idx of [0, 1, 2, 3]) {
      await page.locator(deleteBtn(idx)).click();
      await expect(page.locator(deleteBtn(idx))).toHaveCount(0);
    }

    // The last delete clears the split record: the tracks button flips to
    // not-split with a zero count, and every track renders unavailable but
    // still clickable (clicking would trigger the automated re-split). There
    // is no manual Split button anymore.
    await expect(page.locator(tracksBtn)).toHaveText("not-split (0/4)");
    await expect(
      page.locator(`#concert-${CONCERT_ID} ol.track-list .track-title-unavailable`)
    ).toHaveCount(4);
    await expect(
      page.locator(`#concert-${CONCERT_ID} button[hx-post$="/split"]`)
    ).toHaveCount(0);
  });
});
