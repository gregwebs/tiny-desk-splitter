const { test, expect } = require("./fixtures");

// Drives the real /delete endpoint against the isolated fixture (concert 1,
// "Audio Concert": Celular, Limbo, Track Three, Dando Vueltas). Per-test
// isolation makes the destructive clicks safe. Deleting swaps the whole
// concert card (hx-target="closest .card") with the track list re-rendered
// expanded, so the tracks-button count refreshes and the swapped fragment's
// new delete buttons must stay wired.
const CONCERT_ID = 1;

const listenBtn = (idx) =>
  `#concert-${CONCERT_ID} ol.track-list button.btn-track-listen[data-track-idx="${idx}"]`;
const deleteBtn = (idx) =>
  `#concert-${CONCERT_ID} ol.track-list button.btn-delete[hx-post$="/tracks/${idx}/delete"]`;
const tracksBtn = `#concert-${CONCERT_ID} button.btn-tracks`;

async function expandTracks(page) {
  await page
    .locator(`#concert-${CONCERT_ID} button[onclick*="toggleTracks"]`)
    .click();
  await page.waitForSelector(
    `#concert-${CONCERT_ID} ol.track-list li button.btn-delete`
  );
}

test.describe("Delete track via ✕ button", () => {
  test("clicking ✕ on an expanded track removes it and updates the tracks-button count", async ({
    page,
  }) => {
    await page.goto("/");
    await expandTracks(page);

    await expect(page.locator(tracksBtn)).toHaveText("tracks (4)");
    await expect(page.locator(listenBtn(0))).toHaveText("Celular");
    await page.locator(deleteBtn(0)).click();

    // After the real delete the track is no longer playable: its listen+delete
    // buttons are gone and it shows as an unavailable title in place.
    await expect(page.locator(listenBtn(0))).toHaveCount(0);
    await expect(page.locator(deleteBtn(0))).toHaveCount(0);
    await expect(
      page.locator(`#concert-${CONCERT_ID} ol.track-list .track-title-unavailable`)
    ).toHaveText("Celular");
    // Other tracks are untouched.
    await expect(page.locator(listenBtn(1))).toHaveText("Limbo");
    // The card swap refreshed the count and kept the list expanded.
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
    await expect(page.locator(listenBtn(0))).toHaveCount(0);

    await page.locator(deleteBtn(1)).click();
    await expect(page.locator(listenBtn(1))).toHaveCount(0);

    // Both deleted tracks now render as unavailable; a later one still plays.
    await expect(
      page.locator(`#concert-${CONCERT_ID} ol.track-list .track-title-unavailable`)
    ).toHaveCount(2);
    await expect(page.locator(listenBtn(2))).toHaveText("Track Three");
    await expect(page.locator(tracksBtn)).toHaveText("tracks (2/4)");
  });

  test("the detail page's bottom track list has no delete buttons", async ({
    page,
  }) => {
    // Deletion on the detail page happens via the card's expandable list; the
    // server-rendered bottom list is read-only apart from like/listen/watch.
    await page.goto(`/concerts/${CONCERT_ID}`);
    const detailList = page.locator("h3:has-text('Tracks') + ol.track-list");
    await expect(detailList.locator("button.btn-track-listen").first()).toHaveText(
      "Celular"
    );
    await expect(detailList.locator("button.btn-delete")).toHaveCount(0);
    // Listen and like controls are still present.
    await expect(detailList.locator("button.btn-track-listen")).toHaveCount(4);
    await expect(detailList.locator("button.btn-like")).toHaveCount(4);
  });

  test("✕ in the detail page card's expanded list updates that card's count", async ({
    page,
  }) => {
    await page.goto(`/concerts/${CONCERT_ID}`);
    await expandTracks(page);

    await expect(page.locator(tracksBtn)).toHaveText("tracks (4)");
    await page.locator(deleteBtn(0)).click();

    await expect(page.locator(listenBtn(0))).toHaveCount(0);
    await expect(page.locator(tracksBtn)).toHaveText("tracks (3/4)");
    // The card's list stays expanded across the swap.
    await expect(
      page.locator(`#concert-${CONCERT_ID} ol.track-list li`)
    ).toHaveCount(4);
  });

  test("deleting every track clears the split state on the card", async ({
    page,
  }) => {
    await page.goto("/");
    await expandTracks(page);

    for (const idx of [0, 1, 2, 3]) {
      await page.locator(deleteBtn(idx)).click();
      await expect(page.locator(listenBtn(idx))).toHaveCount(0);
    }

    // The last delete clears the split record: the tracks row and list are
    // gone and the Split action is offered again.
    await expect(page.locator(tracksBtn)).toHaveCount(0);
    await expect(
      page.locator(`#concert-${CONCERT_ID} ol.track-list`)
    ).toHaveCount(0);
    await expect(
      page.locator(`#concert-${CONCERT_ID} button[hx-post$="/split"]`)
    ).toBeVisible();
  });
});
