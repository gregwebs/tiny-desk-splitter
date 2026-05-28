const { test, expect } = require("@playwright/test");

// Concert 2 (RaiNao) in test-player.db is split with 4 tracks: Celular, Limbo,
// track4, dandovueltas... We exercise the click flow on the list page where
// the track list is loaded lazily by `toggleTracks` (raw fetch + innerHTML).
// htmx must wire up the resulting X buttons or the click is a no-op.
const CONCERT_ID = 2;

function deleteEndpoint(concertId, trackIdx) {
  return `**/concerts/${concertId}/tracks/${trackIdx}/delete`;
}

function trackListMock(remainingTracks) {
  const items = remainingTracks
    .map(
      (t) => `
      <li>
        <button class="btn-track-listen" data-concert-id="${CONCERT_ID}" data-track-idx="${t.index}" onclick="void 0">${t.title}</button>
        <button class="btn-delete" hx-post="/concerts/${CONCERT_ID}/tracks/${t.index}/delete" hx-target="closest .track-list" hx-swap="outerHTML">✕</button>
      </li>`
    )
    .join("");
  return `<ol class="track-list">${items}</ol>`;
}

async function expandTracks(page, concertId) {
  const toggle = page.locator(
    `#concert-${concertId} button[onclick*="toggleTracks"]`
  );
  await toggle.click();
  await page.waitForSelector(
    `#concert-${concertId} ol.track-list li button.btn-delete`
  );
}

test.describe("Delete track via ✕ button", () => {
  test("clicking ✕ on a dynamically expanded track removes it from the list", async ({
    page,
  }) => {
    let deleteHits = 0;
    await page.route(deleteEndpoint(CONCERT_ID, 0), async (route) => {
      deleteHits += 1;
      await route.fulfill({
        status: 200,
        contentType: "text/html; charset=utf-8",
        body: trackListMock([
          { index: 1, title: "Limbo" },
          { index: 2, title: "track4" },
          { index: 3, title: "dandovueltas" },
        ]),
      });
    });

    await page.goto("/");
    await expandTracks(page, CONCERT_ID);

    const firstTitle = page
      .locator(`#concert-${CONCERT_ID} ol.track-list li`)
      .first()
      .locator("button.btn-track-listen");
    await expect(firstTitle).toHaveText("Celular");

    const firstDelete = page
      .locator(`#concert-${CONCERT_ID} ol.track-list li`)
      .first()
      .locator("button.btn-delete");
    await firstDelete.click();

    // After the swap the first row's track button must be the next track.
    await expect(firstTitle).toHaveText("Limbo");
    expect(deleteHits).toBe(1);
  });

  test("subsequent ✕ clicks after a swap still fire delete requests", async ({
    page,
  }) => {
    // After the first X click does an htmx swap, the new X buttons in the
    // returned fragment must also be wired up so a second click works.
    let firstDeleteHits = 0;
    let secondDeleteHits = 0;
    await page.route(deleteEndpoint(CONCERT_ID, 0), async (route) => {
      firstDeleteHits += 1;
      await route.fulfill({
        status: 200,
        contentType: "text/html; charset=utf-8",
        body: trackListMock([
          { index: 1, title: "Limbo" },
          { index: 2, title: "track4" },
        ]),
      });
    });
    await page.route(deleteEndpoint(CONCERT_ID, 1), async (route) => {
      secondDeleteHits += 1;
      await route.fulfill({
        status: 200,
        contentType: "text/html; charset=utf-8",
        body: trackListMock([{ index: 2, title: "track4" }]),
      });
    });

    await page.goto("/");
    await expandTracks(page, CONCERT_ID);

    const firstDelete = () =>
      page
        .locator(`#concert-${CONCERT_ID} ol.track-list li`)
        .first()
        .locator("button.btn-delete");
    const firstTitle = () =>
      page
        .locator(`#concert-${CONCERT_ID} ol.track-list li`)
        .first()
        .locator("button.btn-track-listen");

    await firstDelete().click();
    await expect(firstTitle()).toHaveText("Limbo");

    await firstDelete().click();
    await expect(firstTitle()).toHaveText("track4");
    expect(firstDeleteHits).toBe(1);
    expect(secondDeleteHits).toBe(1);
  });

  test("✕ on the pre-rendered detail page list also removes the track", async ({
    page,
  }) => {
    // Regression guard: the detail page renders the track list server-side
    // so htmx already processes those buttons on page load. This must keep
    // working after the dynamic-load fix.
    let deleteHits = 0;
    await page.route(deleteEndpoint(CONCERT_ID, 0), async (route) => {
      deleteHits += 1;
      await route.fulfill({
        status: 200,
        contentType: "text/html; charset=utf-8",
        body: trackListMock([
          { index: 1, title: "Limbo" },
          { index: 2, title: "track4" },
          { index: 3, title: "dandovueltas" },
        ]),
      });
    });

    await page.goto(`/concerts/${CONCERT_ID}`);
    // The detail page has a Tracks <h3> followed by the server-rendered ol.
    const detailList = page.locator("h3:has-text('Tracks') + ol.track-list");
    const firstTitle = detailList
      .locator("li")
      .first()
      .locator("button.btn-track-listen");
    await expect(firstTitle).toHaveText("Celular");

    await detailList
      .locator("li")
      .first()
      .locator("button.btn-delete")
      .click();

    await expect(firstTitle).toHaveText("Limbo");
    expect(deleteHits).toBe(1);
  });
});
