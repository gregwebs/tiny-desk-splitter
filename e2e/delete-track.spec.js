const { test, expect } = require("./fixtures");

// Drives the real /delete endpoint against the isolated fixture (concert 1,
// "Audio Concert": Celular, Limbo, Track Three, Dando Vueltas). Per-test
// isolation makes the destructive clicks safe. Real delete flips the track to
// "unavailable" in place (a span, no buttons) and re-renders the list via htmx;
// the swapped fragment's new delete buttons must stay wired.
const CONCERT_ID = 1;

const listenBtn = (idx) =>
  `#concert-${CONCERT_ID} ol.track-list button.btn-track-listen[data-track-idx="${idx}"]`;
const deleteBtn = (idx) =>
  `#concert-${CONCERT_ID} ol.track-list button.btn-delete[hx-post$="/tracks/${idx}/delete"]`;

async function expandTracks(page) {
  await page
    .locator(`#concert-${CONCERT_ID} button[onclick*="toggleTracks"]`)
    .click();
  await page.waitForSelector(
    `#concert-${CONCERT_ID} ol.track-list li button.btn-delete`
  );
}

test.describe("Delete track via ✕ button", () => {
  test("clicking ✕ on a dynamically expanded track removes it from the list", async ({
    page,
  }) => {
    await page.goto("/");
    await expandTracks(page);

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
  });

  test("subsequent ✕ clicks after a swap still fire delete requests", async ({
    page,
  }) => {
    // After the first delete does an htmx swap, the new delete buttons in the
    // returned fragment must also be wired up so a second one works.
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
  });

  test("✕ on the pre-rendered detail page list also removes the track", async ({
    page,
  }) => {
    // The detail page renders the track list server-side, so htmx processes
    // those buttons on load. Deleting must work there too.
    await page.goto(`/concerts/${CONCERT_ID}`);
    const detailList = page.locator("h3:has-text('Tracks') + ol.track-list");
    const first = detailList.locator("li").first();
    await expect(first.locator("button.btn-track-listen")).toHaveText("Celular");

    await first.locator("button.btn-delete").click();

    await expect(detailList.locator("button.btn-track-listen").first()).toHaveText(
      "Limbo"
    );
    await expect(
      detailList.locator(".track-title-unavailable")
    ).toHaveText("Celular");
  });
});
