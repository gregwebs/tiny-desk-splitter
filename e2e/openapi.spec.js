"use strict";

// OpenAPI/Swagger surface: the generated spec at /api-docs/openapi.json and the
// interactive Swagger UI at /swagger-ui. Covers the JSON API documented in
// concert-tracker/src/web/openapi.rs (playlists, playback, splitting).

const { test, expect } = require("./fixtures");

test.describe("OpenAPI docs", () => {
  test("openapi.json is a well-formed 3.1 spec with the expected paths", async ({ request }) => {
    const response = await request.get("/api-docs/openapi.json");
    expect(response.status()).toBe(200);
    const spec = await response.json();
    expect(spec.openapi.startsWith("3.1")).toBe(true);
    expect(Object.keys(spec.paths)).toEqual(
      expect.arrayContaining(["/api/playlists", "/concerts/{id}/concert-playback"])
    );
  });

  test("swagger UI renders the grouped endpoints", async ({ page }) => {
    await page.goto("/swagger-ui/");
    await expect(page.locator("section.swagger-ui.swagger-container")).toBeVisible();
    // Tag groups from openapi.rs.
    const tagNames = page.locator(".opblock-tag > a.nostyle > span");
    await expect(tagNames.filter({ hasText: /^playlists$/ })).toBeVisible();
    await expect(tagNames.filter({ hasText: /^playback$/ })).toBeVisible();
    await expect(tagNames.filter({ hasText: /^splitting$/ })).toBeVisible();
    // A representative operation is listed under its tag.
    await expect(
      page
        .locator(".opblock-summary-path")
        .filter({ hasText: /^\/api\/playlists$/ })
        .first()
    ).toBeVisible();
  });

  test("Try it out on GET /api/playlists returns 200", async ({ page }) => {
    await page.goto("/swagger-ui/");
    // Expand the GET /api/playlists operation block, then drive its
    // "Try it out" → "Execute" flow exactly as a user would.
    const opBlock = page
      .locator(".opblock-get")
      .filter({
        has: page
          .locator(".opblock-summary-path")
          .filter({ hasText: /^\/api\/playlists$/ }),
      })
      .first();
    await opBlock.locator(".opblock-summary").click();
    await opBlock.getByRole("button", { name: "Try it out" }).click();
    await opBlock.getByRole("button", { name: "Execute" }).click();
    await expect(
      opBlock.locator(".responses-table tbody .response-col_status").first()
    ).toHaveText("200", { timeout: 10000 });
  });
});
