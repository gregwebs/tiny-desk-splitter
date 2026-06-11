const { test, expect, openTracks } = require("./fixtures");

// Exercises the global htmx request-failure feedback added for the track-list
// trash button (and every other hx-* control): a failed request must surface an
// inline ".req-error" next to the triggering element, and must NOT silently do
// nothing. Two failure modes are covered:
//   - network failure (server stopped)  -> htmx:sendError      -> "Server unreachable"
//   - HTTP error status (mocked 500)    -> htmx:responseError  -> "Request failed (500)"
// Like delete-track.spec.js this needs htmx (loaded from the CDN) to be present.
const CONCERT_ID = 1;

const listenBtn = (idx) =>
  `#concert-${CONCERT_ID} ol.track-list button.btn-track-listen[data-track-idx="${idx}"]`;
const deleteBtn = (idx) =>
  `#concert-${CONCERT_ID} ol.track-list button.btn-delete[hx-post$="/tracks/${idx}/delete"]`;
const reqError = `#concert-${CONCERT_ID} ol.track-list .req-error`;

async function expandTracks(page) {
  await openTracks(page, CONCERT_ID);
  await page.waitForSelector(
    `#concert-${CONCERT_ID} ol.track-list li button.btn-delete`
  );
}

test.describe("Failed request feedback", () => {
  test("deleting a track with the server stopped shows an inline error and keeps the track", async ({
    page,
    killServer,
  }) => {
    await page.goto("/");
    await expandTracks(page);
    await expect(page.locator(listenBtn(0))).toHaveText("Celular");

    // Stop the server so the delete POST fails at the network level (htmx fires
    // htmx:sendError, which performs no swap).
    await killServer();
    await page.locator(deleteBtn(0)).click();

    // The user sees an inline error rather than nothing happening...
    await expect(page.locator(reqError)).toHaveText("Server unreachable");
    // ...and the track is untouched (no swap happened).
    await expect(page.locator(listenBtn(0))).toHaveText("Celular");
    // The in-flight class is cleared once the request settles, so the button is
    // interactive again (not stuck disabled).
    await expect(page.locator(deleteBtn(0))).not.toHaveClass(/htmx-request/);
  });

  test("an HTTP error response shows the status inline and keeps the track", async ({
    page,
  }) => {
    await page.goto("/");
    await expandTracks(page);

    // Make just the delete endpoint return 500 (htmx fires htmx:responseError).
    await page.route("**/tracks/0/delete", (route) =>
      route.fulfill({ status: 500, body: "boom" })
    );

    await page.locator(deleteBtn(0)).click();

    await expect(page.locator(reqError)).toHaveText("Request failed (500)");
    await expect(page.locator(listenBtn(0))).toHaveText("Celular");
  });

  test("retrying after a failure clears the stale error and succeeds", async ({
    page,
  }) => {
    await page.goto("/");
    await expandTracks(page);

    // First attempt fails with 500 and shows the inline error.
    await page.route("**/tracks/0/delete", (route) =>
      route.fulfill({ status: 500, body: "boom" })
    );
    await page.locator(deleteBtn(0)).click();
    await expect(page.locator(reqError)).toHaveText("Request failed (500)");

    // Drop the mock so the retry hits the real endpoint and succeeds.
    await page.unroute("**/tracks/0/delete");
    await page.locator(deleteBtn(0)).click();

    // The stale error is gone and the track was actually deleted (it renders
    // as an unavailable — but still clickable — button now).
    await expect(page.locator(reqError)).toHaveCount(0);
    await expect(
      page.locator(`#concert-${CONCERT_ID} ol.track-list .track-title-unavailable`)
    ).toHaveText("Celular");
  });
});
