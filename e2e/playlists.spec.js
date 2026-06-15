"use strict";

// Phase 2a — playlists pages: nav link, list page, create flow, detail page, and
// drag-drop reorder. The reorder is exercised by dispatching the native HTML5
// drag events directly (Playwright can't drive native DnD, and real pointer
// events crash single-process Chromium in the sandbox), which still runs the
// real delegation handlers in playlists.js and the reorder API call.
//
// Phase 2c — playlist playback: Player.playPlaylist(id) appends the playlist's
// resolved tracks to the queue and starts playing if idle.
//
// Phase 2d — playlist queue groups: a played playlist appears in the queue sidebar
// as a group (header + nested song rows). The header carries a single ✕ that
// removes all remaining songs in the group at once.

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

// ── Phase 2c helpers ──────────────────────────────────────────────────────────

async function createPlaylist(page, name) {
  return page.evaluate(async (n) => {
    const r = await fetch("/api/playlists", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ name: n }),
    });
    return (await r.json()).id;
  }, name);
}

async function waitForPlaying(page) {
  await page.waitForFunction(() => {
    const a = document.getElementById("player-audio");
    return a && !a.paused;
  });
}

// ── Phase 2c specs ────────────────────────────────────────────────────────────

test.describe("Playlist playback (2c)", () => {
  // Fixture concert IDs referenced in these tests.
  // Concert 1 "Audio Concert"         — 4 available tracks (Celular, Limbo, Track Three, Dando Vueltas)
  // Concert 2 "Second Concert"        — 3 available tracks (Song One, Song Two, Song Three)
  // Concert 5 "Deleted-First Concert" — track 0 unavailable; tracks 1+2 available

  test("playPlaylist from idle player starts first track, queues rest, shows label", async ({ page }) => {
    await page.goto("/");
    const pid = await createPlaylist(page, "My Mix");
    await addItem(page, pid, { type: "track", concert_id: 1, track_index: 0 });
    await addItem(page, pid, { type: "track", concert_id: 1, track_index: 1 });
    await addItem(page, pid, { type: "track", concert_id: 1, track_index: 2 });

    await page.waitForFunction(() => !!window.Player);
    await page.evaluate((id) => Player.playPlaylist(id), pid);
    await waitForPlaying(page);

    // First track should be playing.
    await expect(page.locator("#player-title")).toHaveText("Celular");
    // The other two tracks are queued.
    await expect(page.locator("#player-queue-badge")).toHaveText("2");
    // Playlist label is visible and shows the playlist name.
    await expect(page.locator("#player-playlist")).toBeVisible();
    await expect(page.locator("#player-playlist")).toContainText("My Mix");
  });

  test("playPlaylist while playing appends to queue without interrupting current track", async ({ page }) => {
    await page.goto("/");
    await page.waitForFunction(() => !!window.Player);
    // Start a track directly (startTrack is exported on window.Player).
    await page.evaluate(() => Player.startTrack(null, 1, 0));
    await waitForPlaying(page);
    await expect(page.locator("#player-title")).toHaveText("Celular");

    const pid = await createPlaylist(page, "Appended");
    await addItem(page, pid, { type: "track", concert_id: 2, track_index: 0 });
    await addItem(page, pid, { type: "track", concert_id: 2, track_index: 1 });

    await page.evaluate((id) => Player.playPlaylist(id), pid);

    // Current track must be unchanged.
    await expect(page.locator("#player-title")).toHaveText("Celular");
    // The two playlist tracks were appended to the queue.
    await expect(page.locator("#player-queue-badge")).toHaveText("2");
  });

  test("playPlaylist skips unavailable tracks", async ({ page }) => {
    await page.goto("/");
    // Concert 5 track 0 has tracks_present[0]=false so available=false in resolved_tracks.
    const pid = await createPlaylist(page, "Partial Avail");
    await addItem(page, pid, { type: "track", concert_id: 5, track_index: 0 }); // unavailable
    await addItem(page, pid, { type: "track", concert_id: 1, track_index: 0 }); // available
    await addItem(page, pid, { type: "track", concert_id: 1, track_index: 1 }); // available

    await page.waitForFunction(() => !!window.Player);
    await page.evaluate((id) => Player.playPlaylist(id), pid);
    await waitForPlaying(page);

    // First *available* track plays (the unavailable one was filtered before enqueuing).
    await expect(page.locator("#player-title")).toHaveText("Celular");
    // One remaining in queue (concert 1 track 1); unavailable track never entered the queue.
    await expect(page.locator("#player-queue-badge")).toHaveText("1");
  });

  test("playlist label clears when a non-playlist track starts", async ({ page }) => {
    await page.goto("/");
    const pid = await createPlaylist(page, "Temp List");
    await addItem(page, pid, { type: "track", concert_id: 1, track_index: 0 });

    await page.waitForFunction(() => !!window.Player);
    await page.evaluate((id) => Player.playPlaylist(id), pid);
    await waitForPlaying(page);

    await expect(page.locator("#player-playlist")).toBeVisible();
    await expect(page.locator("#player-playlist")).toContainText("Temp List");

    // Start a regular (non-playlist) track — playlistName defaults to null in startTrack.
    await page.evaluate(() => Player.startTrack(null, 2, 0));
    await waitForPlaying(page);

    // Label should now be hidden.
    await expect(page.locator("#player-playlist")).toBeHidden();
  });
});

// ── Phase 2d specs ────────────────────────────────────────────────────────────

test.describe("Playlist queue groups (2d)", () => {
  // Open the queue sidebar via the toggle button.
  async function openQueueSidebar(page) {
    const sidebar = page.locator("#player-sidebar");
    const isOpen = await sidebar.evaluate(el => el.classList.contains("showing-queue"));
    if (!isOpen) {
      await page.locator("#player-queue-toggle").evaluate(el => el.click());
    }
    await expect(page.locator("#sidebar-queue-section")).toBeVisible();
  }

  test("playing a playlist while another track plays shows a group header in the queue", async ({ page }) => {
    await page.goto("/");
    await page.waitForFunction(() => !!window.Player);

    // Start a track so the player is busy; playlist will queue behind it.
    await page.evaluate(() => Player.startTrack(null, 1, 0));
    await waitForPlaying(page);
    await page.evaluate(() => { document.getElementById("player-audio").loop = true; });

    const pid = await createPlaylist(page, "Group Test");
    await addItem(page, pid, { type: "track", concert_id: 2, track_index: 0 });
    await addItem(page, pid, { type: "track", concert_id: 2, track_index: 1 });

    await page.evaluate((id) => Player.playPlaylist(id), pid);

    // Two tracks queued.
    await expect(page.locator("#player-queue-badge")).toHaveText("2");

    await openQueueSidebar(page);

    // One group header appears with the playlist name.
    await expect(page.locator(".queue-group")).toHaveCount(1);
    await expect(page.locator(".queue-group .queue-group-name")).toHaveText("Group Test");

    // Both song rows are nested under the group.
    await expect(page.locator(".queue-item.nested")).toHaveCount(2);
  });

  test("playing the same playlist twice creates two independent groups", async ({ page }) => {
    await page.goto("/");
    await page.waitForFunction(() => !!window.Player);

    await page.evaluate(() => Player.startTrack(null, 1, 0));
    await waitForPlaying(page);
    await page.evaluate(() => { document.getElementById("player-audio").loop = true; });

    const pid = await createPlaylist(page, "Double Play");
    await addItem(page, pid, { type: "track", concert_id: 2, track_index: 0 });
    await addItem(page, pid, { type: "track", concert_id: 2, track_index: 1 });

    // Play the same playlist twice.
    await page.evaluate((id) => Player.playPlaylist(id), pid);
    await page.evaluate((id) => Player.playPlaylist(id), pid);

    // 4 tracks total in queue (no dedup for playlist pushes).
    await expect(page.locator("#player-queue-badge")).toHaveText("4");

    await openQueueSidebar(page);

    // Two separate group headers.
    await expect(page.locator(".queue-group")).toHaveCount(2);
  });

  test("the group ✕ removes all songs in the group at once", async ({ page }) => {
    await page.goto("/");
    await page.waitForFunction(() => !!window.Player);

    await page.evaluate(() => Player.startTrack(null, 1, 0));
    await waitForPlaying(page);
    await page.evaluate(() => { document.getElementById("player-audio").loop = true; });

    const pid = await createPlaylist(page, "Removable");
    await addItem(page, pid, { type: "track", concert_id: 2, track_index: 0 });
    await addItem(page, pid, { type: "track", concert_id: 2, track_index: 1 });
    await addItem(page, pid, { type: "track", concert_id: 2, track_index: 2 });

    await page.evaluate((id) => Player.playPlaylist(id), pid);
    await expect(page.locator("#player-queue-badge")).toHaveText("3");

    await openQueueSidebar(page);
    await expect(page.locator(".queue-group")).toHaveCount(1);

    // Click the group ✕ button.
    await page.locator(".queue-group .btn-queue-remove").evaluate(el => el.click());

    // All 3 tracks removed — badge gone and header vanishes.
    await expect(page.locator("#player-queue-badge")).toHaveText("");
    await expect(page.locator(".queue-group")).toHaveCount(0);
    await expect(page.locator(".queue-item.nested")).toHaveCount(0);
  });

  test("removing a group ✕ leaves another group untouched", async ({ page }) => {
    await page.goto("/");
    await page.waitForFunction(() => !!window.Player);

    await page.evaluate(() => Player.startTrack(null, 1, 0));
    await waitForPlaying(page);
    await page.evaluate(() => { document.getElementById("player-audio").loop = true; });

    const pid1 = await createPlaylist(page, "First Group");
    await addItem(page, pid1, { type: "track", concert_id: 2, track_index: 0 });
    await addItem(page, pid1, { type: "track", concert_id: 2, track_index: 1 });

    const pid2 = await createPlaylist(page, "Second Group");
    await addItem(page, pid2, { type: "track", concert_id: 1, track_index: 1 });

    await page.evaluate((id) => Player.playPlaylist(id), pid1);
    await page.evaluate((id) => Player.playPlaylist(id), pid2);

    // 3 total queued across two groups.
    await expect(page.locator("#player-queue-badge")).toHaveText("3");

    await openQueueSidebar(page);
    await expect(page.locator(".queue-group")).toHaveCount(2);

    // Remove the FIRST group (it renders at the bottom = last-appended, so it is
    // the last .queue-group in the list since we iterate in reverse).
    const groups = page.locator(".queue-group");
    await groups.last().locator(".btn-queue-remove").evaluate(el => el.click());

    // 1 track remains (the second group's song).
    await expect(page.locator("#player-queue-badge")).toHaveText("1");
    await expect(page.locator(".queue-group")).toHaveCount(1);
    await expect(page.locator(".queue-group .queue-group-name")).toHaveText("Second Group");
  });

  test("ad-hoc queued tracks are not nested and have no group header", async ({ page }) => {
    await page.goto("/");
    await page.waitForFunction(() => !!window.Player);

    // Queue a track ad-hoc (while playing, so it enqueues rather than plays).
    await page.evaluate(() => Player.startTrack(null, 1, 0));
    await waitForPlaying(page);
    await page.evaluate(() => { document.getElementById("player-audio").loop = true; });

    // Enqueue a track directly (not via playPlaylist) using the public API.
    await page.evaluate(() => Player.enqueue(2, 0, "Song One", false));

    await expect(page.locator("#player-queue-badge")).toHaveText("1");

    await openQueueSidebar(page);

    // No group header; the item is not nested.
    await expect(page.locator(".queue-group")).toHaveCount(0);
    await expect(page.locator(".queue-item")).toHaveCount(1);
    await expect(page.locator(".queue-item.nested")).toHaveCount(0);
  });
});
