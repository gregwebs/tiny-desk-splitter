"use strict";

// OpenAPI/Swagger surface: the generated spec at /api-docs/openapi.json and the
// interactive Swagger UI at /swagger-ui. Covers the JSON API documented in
// concert-tracker/src/web/openapi.rs (playlists, playback, splitting).

const { test, expect } = require("./fixtures");

test.describe("OpenAPI docs", () => {
  test("openapi.json is a well-formed 3.1 spec with the expected paths", async ({ page }) => {
    const spec = await page.evaluate(async () => {
      const r = await fetch("/api-docs/openapi.json");
      return { status: r.status, body: await r.json() };
    });
    expect(spec.status).toBe(200);
    expect(spec.body.openapi.startsWith("3.1")).toBe(true);
    expect(Object.keys(spec.body.paths)).toEqual(
      expect.arrayContaining(["/api/playlists", "/concerts/{id}/concert-playback"])
    );
  });

  test("swagger UI renders the grouped endpoints", async ({ page }) => {
    await page.goto("/swagger-ui/");
    await expect(page.locator(".swagger-ui")).toBeVisible();
    // Tag groups from openapi.rs.
    await expect(page.getByText("playlists", { exact: true })).toBeVisible();
    await expect(page.getByText("playback", { exact: true })).toBeVisible();
    await expect(page.getByText("splitting", { exact: true })).toBeVisible();
    // A representative operation is listed under its tag.
    await expect(page.locator(".opblock-summary-path", { hasText: "/api/playlists" }).first()).toBeVisible();
  });

  test("Try it out on GET /api/playlists returns 200", async ({ page }) => {
    await page.goto("/swagger-ui/");
    // Expand the GET /api/playlists operation block, then drive its
    // "Try it out" → "Execute" flow exactly as a user would.
    const opBlock = page
      .locator(".opblock-get")
      .filter({ has: page.locator(".opblock-summary-path", { hasText: "/api/playlists" }) })
      .first();
    await opBlock.locator(".opblock-summary").click();
    await opBlock.getByRole("button", { name: "Try it out" }).click();
    await opBlock.getByRole("button", { name: "Execute" }).click();
    await expect(opBlock.locator(".response-col_status").first()).toHaveText(/200/, {
      timeout: 10000,
    });
  });
});
