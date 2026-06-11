const { test, expect } = require("./fixtures");

// The listing uses the small, always-local thumbnail (served from /thumbnails);
// the detail-page card uses the full-size preview (served from /concert-files).
// The detail page must not render a second, duplicate preview image.

test.describe("Listing thumbnails vs detail full image", () => {
  test("every listing card image is a /thumbnails URL and they serve", async ({ page }) => {
    await page.goto("/");
    const srcs = await page.locator("img.card-thumb").evaluateAll((imgs) =>
      imgs.map((i) => i.getAttribute("src"))
    );
    expect(srcs.length).toBeGreaterThan(0);
    // Every card uses the thumbnail route (the wiring under test).
    for (const src of srcs) {
      expect(src).toMatch(/^\/thumbnails\/.+\.jpg$/);
    }
    // At least one thumbnail actually resolves (serving works; not all test
    // concerts have a preview on disk in this fixture).
    let served = 0;
    for (const src of srcs) {
      const resp = await page.request.get(src);
      if (resp.status() === 200 && Number(resp.headers()["content-length"]) > 0) served++;
    }
    expect(served).toBeGreaterThan(0);

    // …and at least one card image actually *decodes* in the browser
    // (naturalWidth > 0). A presence- or even 200-only check passes on a
    // broken/404 image; this proves a real thumbnail is rendered — the
    // user-visible symptom of the sync-thumbnail bug.
    const decoded = await page.locator("img.card-thumb").evaluateAll((imgs) =>
      imgs.filter((i) => i.complete && i.naturalWidth > 0).length
    );
    expect(decoded).toBeGreaterThan(0);
  });

  test("detail card uses the full preview and shows no duplicate image", async ({ page }) => {
    await page.goto("/");
    // Pick a card whose thumbnail actually resolves, then open that concert so
    // the full preview is present too.
    const cards = page.locator("div.card", { has: page.locator("img.card-thumb") });
    const count = await cards.count();
    let opened = false;
    for (let i = 0; i < count; i++) {
      const card = cards.nth(i);
      const src = await card.locator("img.card-thumb").getAttribute("src");
      const resp = await page.request.get(src);
      if (resp.status() === 200) {
        // Click the title link: hovering the card to click hides the
        // thumbnail (the hover swaps picture → track list), so the image
        // link itself is unclickable on the listing.
        await card.locator(".card-title a").click();
        opened = true;
        break;
      }
    }
    expect(opened).toBeTruthy();
    await page.waitForFunction(() => /\/concerts\/\d+/.test(location.pathname));

    const cardImg = page.locator("img.card-thumb");
    await expect(cardImg).toHaveAttribute("src", /^\/concert-files\/.+\/preview\.jpg$/);
    const fullSrc = await cardImg.getAttribute("src");
    expect((await page.request.get(fullSrc)).status()).toBe(200);

    // No separate full preview image below the card anymore.
    await expect(page.locator("img.preview-image")).toHaveCount(0);
  });
});
