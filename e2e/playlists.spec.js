"use strict";

// Phase 2a — playlists pages: nav link, list page, create flow, detail page, and
// drag-drop reorder. The reorder is exercised by dispatching the native HTML5
// drag events directly (Playwright can't drive native DnD, and real pointer
// events crash single-process Chromium in the sandbox), which still runs the
// real delegation handlers in playlists.js and the reorder API call.

const { test, expect } = require("./fixtures");

// Add an item to a playlist via the JSON API from inside the page (same origin).
async function addItem(page, playlistId, body) {
  return page.evaluate(
    async ([id, b]) => {
      const r = await fetch(`/api/playlists/${id}/items`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(b),
      });
      return r.status;
    },
    [playlistId, body]
  );
}

async function itemOrder(page, playlistId) {
  return page.evaluate(async (id) => {
    const r = await fetch(`/api/playlists/${id}`);
    const d = await r.json();
    return d.items.map((i) => i.id);
  }, playlistId);
}

test.describe("Playlists — pages (2a)", () => {
  test("nav links to the playlists page and it renders", async ({ page }) => {
    await page.goto("/");
    await expect(page.locator('header a[href="/playlists"]')).toHaveText("Playlists");

    await page.evaluate(() =>
      window.htmx.ajax("GET", "/playlists", { target: "#content", select: "#content", swap: "outerHTML" })
    );
    await expect(page.locator("#content h2")).toHaveText("Playlists");
    await expect(page.locator("#new-playlist-form")).toBeVisible();
  });

  test("creating a playlist from the form lands on its detail page", async ({ page }) => {
    await page.goto("/playlists");
    await page.fill("#new-playlist-name", "Summer Set");
    await page.click("#new-playlist-form button[type=submit]");

    // createFromForm navigates to /playlists/:id on success.
    await page.waitForURL(/\/playlists\/\d+$/);
    await expect(page.locator("#playlist-name")).toHaveText("Summer Set");
    await expect(page.locator(".playlists-empty")).toBeVisible(); // empty playlist
  });

  test("a created playlist appears on the list page with a play button", async ({ page }) => {
    await page.goto("/playlists");
    await page.fill("#new-playlist-name", "On The List");
    await page.click("#new-playlist-form button[type=submit]");
    await page.waitForURL(/\/playlists\/\d+$/);

    await page.goto("/playlists");
    const row = page.locator(".playlist-row", { hasText: "On The List" });
    await expect(row).toHaveCount(1);
    await expect(row.locator(".btn-pl-play")).toBeVisible();
    await expect(row.locator('a.playlist-link[href^="/playlists/"]')).toBeVisible();
  });

  test("drag-drop reorder persists the new item order", async ({ page }) => {
    // Create a playlist and give it two concert items (concerts 1 and 2 exist in
    // the fixture).
    await page.goto("/playlists");
    const pid = await page.evaluate(async () => {
      const r = await fetch("/api/playlists", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ name: "Reorder Me" }),
      });
      return (await r.json()).id;
    });
    expect(await addItem(page, pid, { type: "concert", concert_id: 1 })).toBe(200);
    expect(await addItem(page, pid, { type: "concert", concert_id: 2 })).toBe(200);

    const before = await itemOrder(page, pid);
    expect(before.length).toBe(2);

    await page.goto(`/playlists/${pid}`);
    await expect(page.locator(".playlist-item")).toHaveCount(2);

    // Dispatch native DnD events to move the 2nd item above the 1st. The
    // delegated handlers in playlists.js reorder the DOM and POST the new order.
    await page.evaluate(() => {
      const list = document.getElementById("playlist-items");
      const items = list.querySelectorAll(".playlist-item");
      const src = items[1];
      const target = items[0];
      const dt = new DataTransfer();
      const fire = (el, type, clientY) =>
        el.dispatchEvent(
          new DragEvent(type, { bubbles: true, cancelable: true, dataTransfer: dt, clientY: clientY || 0 })
        );
      const box = target.getBoundingClientRect();
      fire(src, "dragstart", 0);
      fire(target, "dragover", box.top + 1);
      fire(target, "drop", box.top + 1);
      fire(src, "dragend", 0);
    });

    // The order persisted to the server should be the reverse of `before`.
    await expect
      .poll(async () => (await itemOrder(page, pid)).join(","))
      .toBe([before[1], before[0]].join(","));
  });
});
