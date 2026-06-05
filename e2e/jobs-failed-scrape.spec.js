const { test, expect } = require("./fixtures");

// Background metadata-scrape failures (e.g. an archived-NAS write failure) must
// surface on the Jobs page, not just in the server log. The fixture seeds one
// failed "scrape" job for concert 1 (see examples/make_test_fixture.rs). These
// guard the Step A wiring: the Scrape failed-job row renders, and the Scrape
// filter chip exists and filters correctly.

test.describe("Jobs page surfaces failed scrapes", () => {
  test("the seeded scrape failure renders with a Scrape badge + message", async ({
    page,
  }) => {
    await page.goto("/jobs");

    // The Scrape filter chip exists alongside Download/Split.
    await expect(
      page.locator('.filter-chips a[href="/jobs?failed_filter=scrape"]')
    ).toHaveText("Scrape");

    // The failed-jobs table shows the scrape row: badge label + NAS error text.
    const failedTable = page.locator(".jobs-table").last();
    await expect(failedTable.locator(".badge", { hasText: "Scrape" })).toBeVisible();
    await expect(
      page.getByText("Failed to write JSON file", { exact: false })
    ).toBeVisible();
  });

  test("the Scrape filter keeps scrape rows; other filters exclude them", async ({
    page,
  }) => {
    // Scrape filter: the row is present.
    await page.goto("/jobs?failed_filter=scrape");
    await expect(
      page.getByText("Failed to write JSON file", { exact: false })
    ).toBeVisible();

    // Download filter: the fixture has no download failures, so the scrape row
    // is excluded and the empty state shows.
    await page.goto("/jobs?failed_filter=download");
    await expect(page.getByText("No failed jobs.")).toBeVisible();
    await expect(
      page.getByText("Failed to write JSON file", { exact: false })
    ).toHaveCount(0);
  });

  test("clicking the Scrape chip filters to scrape failures", async ({ page }) => {
    await page.goto("/jobs");
    await page.locator('.filter-chips a[href="/jobs?failed_filter=scrape"]').click();

    await page.waitForFunction(
      () => new URLSearchParams(location.search).get("failed_filter") === "scrape"
    );
    await expect(
      page.getByText("Failed to write JSON file", { exact: false })
    ).toBeVisible();
    await expect(
      page.locator('.filter-chips a[href="/jobs?failed_filter=scrape"].active')
    ).toBeVisible();
  });
});
