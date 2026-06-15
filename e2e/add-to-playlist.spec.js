"use strict";

// Phase 2b — Add-to-playlist affordances: hover "+" on track rows, concert
// cards, and playlist rows; sidebar add panel; membership indicators; create-
// and-add flow; 422 error surface for cycle detection.
//
// Phase 2e — Add-to-playlist button in the player bar: the "+" (#player-add-pl)
// opens the same panel for the currently-playing track.

const { test, expect } = require("./fixtures");

// ── helpers ──────────────────────────────────────────────────────────────────

// All page.evaluate calls use relative URLs, so the page must be navigated to
// the server before calling these helpers.
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

async function addItemToPlaylist(page, playlistId, body) {
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

// Open the sidebar add panel for a track (concert 1, track 0).
async function openAddPanelForTrack(page) {
  await page.goto("/concerts/1");
  await page.waitForSelector(".track-list li");

  const trackLi = page.locator(".track-list li").first();
  await trackLi.hover();
  const addBtn = trackLi.locator(".btn-add-pl");
  await addBtn.waitFor({ state: "visible" });
  await addBtn.click();

  await expect(page.locator("#sidebar-add-section")).toBeVisible();
  await expect(page.locator(".add-pl-context")).toContainText("Adding");
}

// Wait for the add panel list to finish loading (past the "Loading…" state).
async function waitForAddList(page) {
  await page.waitForFunction(() => {
    const rows = document.querySelectorAll(".add-pl-row");
    return rows.length > 0 && ![...rows].every((r) => r.textContent.includes("Loading"));
  });
}

// Dispatch a click via JS (works around single-process Chromium pointer-event
// constraints that can block Playwright .click() on list items inside a sidebar).
async function jsClick(page, locator) {
  const el = await locator.elementHandle();
  await page.evaluate((node) => node.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true })), el);
}

// ── specs ─────────────────────────────────────────────────────────────────────

test.describe("Add-to-playlist (2b)", () => {
  test("hovering a track row reveals the '+' button", async ({ page }) => {
    await page.goto("/concerts/1");
    await page.waitForSelector(".track-list li");

    const firstLi = page.locator(".track-list li").first();
    await firstLi.hover();
    const addBtn = firstLi.locator(".btn-add-pl");
    await addBtn.waitFor({ state: "visible" });
    await expect(addBtn).toBeVisible();
  });

  test("clicking '+' on a track opens the add panel in the sidebar", async ({ page }) => {
    await openAddPanelForTrack(page);
    await expect(page.locator("#player-sidebar")).toHaveClass(/showing-add/);
    await expect(page.locator("#sidebar-queue-section")).not.toBeVisible();
    await expect(page.locator("#add-pl-list")).toBeVisible();
  });

  test("add panel lists existing playlists with membership checks", async ({ page }) => {
    // Navigate first so page.evaluate can use relative URLs.
    await page.goto("/concerts/1");
    const pid = await createPlaylist(page, "Already In");
    await addItemToPlaylist(page, pid, { type: "track", concert_id: 1, track_index: 0 });
    await createPlaylist(page, "Not In");

    await openAddPanelForTrack(page);
    await waitForAddList(page);

    // "Already In" should show a checkmark and the member class.
    const memberRow = page.locator(".add-pl-row-member", { hasText: "Already In" });
    await expect(memberRow).toBeVisible();
    await expect(memberRow.locator(".add-pl-check")).toHaveText("✓");

    // "Not In" should be a normal (clickable) row.
    const normalRow = page.locator(".add-pl-row:not(.add-pl-row-member)", { hasText: "Not In" });
    await expect(normalRow).toBeVisible();
  });

  test("clicking a playlist row adds the track and flips to checked", async ({ page }) => {
    await page.goto("/concerts/1");
    const pid = await createPlaylist(page, "Target Playlist");

    await openAddPanelForTrack(page);
    await waitForAddList(page);

    const row = page.locator(".add-pl-row:not(.add-pl-row-member)", { hasText: "Target Playlist" });
    await expect(row).toBeVisible();
    await jsClick(page, row);

    // Row should now be a member.
    await expect(page.locator(".add-pl-row-member", { hasText: "Target Playlist" })).toBeVisible();

    // Confirm via API.
    const items = await page.evaluate(async (id) => {
      const r = await fetch(`/api/playlists/${id}`);
      return (await r.json()).items;
    }, pid);
    expect(items.length).toBe(1);
    expect(items[0].item_type).toBe("track");
  });

  test("filter input narrows the playlist list", async ({ page }) => {
    await page.goto("/concerts/1");
    await createPlaylist(page, "Alpha List");
    await createPlaylist(page, "Beta List");

    await openAddPanelForTrack(page);
    await waitForAddList(page);

    await page.fill("#add-pl-filter", "Alpha");
    // Re-render is synchronous; just check the result.
    const texts = await page.locator(".add-pl-row").allTextContents();
    expect(texts.some((t) => t.includes("Beta"))).toBe(false);
    expect(texts.some((t) => t.includes("Alpha"))).toBe(true);
    // The "Create" row appears when there is filter text.
    expect(texts.some((t) => t.includes("Create"))).toBe(true);
  });

  test("create-and-add flow creates a new playlist with the track", async ({ page }) => {
    await openAddPanelForTrack(page);
    await waitForAddList(page);

    const uniqueName = "Brand New " + Date.now();
    await page.fill("#add-pl-filter", uniqueName);

    const createRow = page.locator(".add-pl-row-new", { hasText: "Create" });
    await expect(createRow).toBeVisible();
    await jsClick(page, createRow);

    // The new playlist row should appear as a member.
    await expect(page.locator(".add-pl-row-member", { hasText: uniqueName })).toBeVisible();

    // Confirm via API.
    const lists = await page.evaluate(async () => {
      return (await (await fetch("/api/playlists")).json());
    });
    const newPl = lists.find((e) => e.playlist.name === uniqueName);
    expect(newPl).toBeTruthy();

    const detail = await page.evaluate(async (id) => {
      return (await (await fetch(`/api/playlists/${id}`)).json());
    }, newPl.playlist.id);
    expect(detail.items.length).toBe(1);
    expect(detail.items[0].item_type).toBe("track");
  });

  test("closing the add panel restores queue/concert sections", async ({ page }) => {
    await openAddPanelForTrack(page);

    await page.locator(".add-pl-close").click();

    await expect(page.locator("#player-sidebar")).not.toHaveClass(/showing-add/);
    await expect(page.locator("#sidebar-queue-section")).toBeVisible();
  });

  test("concert card shows '+' button on hover and opens add panel", async ({ page }) => {
    await page.goto("/");
    await page.waitForSelector(".card");

    const card = page.locator(".card").first();
    await card.hover();

    const concertAddBtn = card.locator(".btn-add-pl-concert");
    await concertAddBtn.waitFor({ state: "visible" });
    await jsClick(page, concertAddBtn);

    await expect(page.locator("#sidebar-add-section")).toBeVisible();
    await expect(page.locator(".add-pl-context")).toContainText("Adding");
  });

  test("playlist row shows '+' button and opens add panel for nesting", async ({ page }) => {
    await page.goto("/playlists");
    await createPlaylist(page, "Inner Playlist");
    await createPlaylist(page, "Outer Playlist");

    await page.goto("/playlists");
    const row = page.locator(".playlist-row", { hasText: "Inner Playlist" });
    await row.hover();

    const nestBtn = row.locator(".btn-pl-nest");
    await nestBtn.waitFor({ state: "visible" });
    await jsClick(page, nestBtn);

    await expect(page.locator("#sidebar-add-section")).toBeVisible();
    await expect(page.locator(".add-pl-context")).toContainText("Nesting");
  });

  test("cycle detection 422 surfaces as inline error", async ({ page }) => {
    await page.goto("/playlists");
    const idA = await createPlaylist(page, "Playlist A");
    const idB = await createPlaylist(page, "Playlist B");
    // Nest B into A (valid direction).
    await addItemToPlaylist(page, idA, { type: "playlist", child_playlist_id: idB });

    // Try to nest A into B (would create a cycle) via the add panel.
    await page.goto("/playlists");
    const row = page.locator(".playlist-row", { hasText: "Playlist A" });
    await row.hover();
    const nestBtn = row.locator(".btn-pl-nest");
    await nestBtn.waitFor({ state: "visible" });
    await jsClick(page, nestBtn);

    await expect(page.locator("#sidebar-add-section")).toBeVisible();
    await waitForAddList(page);

    // Click "Playlist B" to attempt the cyclic nesting.
    const targetRow = page.locator(".add-pl-row:not(.add-pl-row-member)", { hasText: "Playlist B" });
    await expect(targetRow).toBeVisible();
    await jsClick(page, targetRow);

    // Error message should appear.
    await expect(page.locator("#add-pl-error")).toBeVisible({ timeout: 3000 });
    await expect(page.locator("#add-pl-error")).toContainText("Couldn't add");
  });

  test("typing the exact name of a non-member playlist highlights it and Enter adds it", async ({ page }) => {
    await page.goto("/concerts/1");
    const pid = await createPlaylist(page, "Keyboard Target");

    await openAddPanelForTrack(page);
    await waitForAddList(page);

    // Type the exact name — row should become active-highlighted.
    await page.fill("#add-pl-filter", "Keyboard Target");
    const row = page.locator(".add-pl-row-active", { hasText: "Keyboard Target" });
    await expect(row).toBeVisible();

    // Press Enter — should add the track and flip to member.
    await page.keyboard.press("Enter");
    await expect(page.locator(".add-pl-row-member", { hasText: "Keyboard Target" })).toBeVisible();

    // Confirm via API.
    const items = await page.evaluate(async (id) => {
      return (await (await fetch(`/api/playlists/${id}`)).json()).items;
    }, pid);
    expect(items.length).toBe(1);
    expect(items[0].item_type).toBe("track");
  });

  test("typing a unique new name leaves only the Create row highlighted and Enter creates-and-adds", async ({ page }) => {
    await openAddPanelForTrack(page);
    await waitForAddList(page);

    const uniqueName = "EnterCreate " + Date.now();
    await page.fill("#add-pl-filter", uniqueName);

    // Only the Create row should be visible and active.
    const createRow = page.locator(".add-pl-row-new.add-pl-row-active");
    await expect(createRow).toBeVisible();

    // Press Enter — should create the playlist and add the track.
    await page.keyboard.press("Enter");
    await expect(page.locator(".add-pl-row-member", { hasText: uniqueName })).toBeVisible();

    // Confirm via API.
    const lists = await page.evaluate(async () => (await (await fetch("/api/playlists")).json()));
    const newPl = lists.find((e) => e.playlist.name === uniqueName);
    expect(newPl).toBeTruthy();
    const detail = await page.evaluate(async (id) => (await (await fetch(`/api/playlists/${id}`)).json()), newPl.playlist.id);
    expect(detail.items.length).toBe(1);
    expect(detail.items[0].item_type).toBe("track");
  });

  test("Enter after add clears the filter text and shows all playlists", async ({ page }) => {
    await page.goto("/concerts/1");
    await createPlaylist(page, "Clear Test");

    await openAddPanelForTrack(page);
    await waitForAddList(page);

    // Type exact name so Enter is wired up.
    await page.fill("#add-pl-filter", "Clear Test");
    await expect(page.locator(".add-pl-row-active")).toBeVisible();

    await page.keyboard.press("Enter");

    // Filter should be cleared and all playlists visible (not just the match).
    await expect(page.locator("#add-pl-filter")).toHaveValue("");
    // The row should now show as a member (no longer active/highlighted).
    await expect(page.locator(".add-pl-row-member", { hasText: "Clear Test" })).toBeVisible();
  });

  test("Enter on empty filter closes the add panel", async ({ page }) => {
    await openAddPanelForTrack(page);
    await waitForAddList(page);

    // Ensure filter is empty, then press Enter.
    await expect(page.locator("#add-pl-filter")).toHaveValue("");
    await page.keyboard.press("Enter");

    await expect(page.locator("#player-sidebar")).not.toHaveClass(/showing-add/);
    await expect(page.locator("#sidebar-queue-section")).toBeVisible();
  });

  test("closing the add panel via Enter closes the sidebar when it was not open before", async ({ page }) => {
    await page.goto("/concerts/1");
    // Sidebar must not be open before clicking the + button.
    await expect(page.locator("body")).not.toHaveClass(/sidebar-open/);

    await openAddPanelForTrack(page);
    await waitForAddList(page);

    // Press Enter on empty filter → closeAdd should also close the sidebar.
    await page.keyboard.press("Enter");

    await expect(page.locator("body")).not.toHaveClass(/sidebar-open/);
  });

  test("closing the add panel via Enter leaves the sidebar open when it was already open", async ({ page }) => {
    await page.goto("/concerts/1");
    await page.waitForSelector(".track-list li");

    // Open the sidebar programmatically (the queue toggle requires an active player).
    await page.waitForFunction(() => !!window.Player);
    await page.evaluate(() => Player.openSidebar());
    await expect(page.locator("body")).toHaveClass(/sidebar-open/);

    // Now click the '+' button on the first track without re-navigating.
    const trackLi = page.locator(".track-list li").first();
    await trackLi.hover();
    const addBtn = trackLi.locator(".btn-add-pl");
    await addBtn.waitFor({ state: "visible" });
    await addBtn.click();
    await expect(page.locator("#sidebar-add-section")).toBeVisible();

    await waitForAddList(page);

    // Press Enter on empty filter to close the add panel.
    await page.keyboard.press("Enter");

    // Add panel should be gone but sidebar must remain open.
    await expect(page.locator("#player-sidebar")).not.toHaveClass(/showing-add/);
    await expect(page.locator("body")).toHaveClass(/sidebar-open/);
  });
});

// ── Phase 2e specs ────────────────────────────────────────────────────────────

async function waitForPlaying(page) {
  await page.waitForFunction(() => {
    const a = document.getElementById("player-audio");
    return a && !a.paused;
  });
}

test.describe("Player bar add-to-playlist (2e)", () => {
  test("#player-add-pl is hidden when nothing is playing", async ({ page }) => {
    await page.goto("/");
    await page.waitForFunction(() => !!window.Player);
    await expect(page.locator("#player-add-pl")).toBeHidden();
  });

  test("#player-add-pl appears when a track starts and disappears on stopPlayback", async ({ page }) => {
    await page.goto("/");
    await page.waitForFunction(() => !!window.Player);

    // Start a track.
    await page.evaluate(() => Player.startTrack(null, 1, 0));
    await waitForPlaying(page);

    await expect(page.locator("#player-add-pl")).toBeVisible();

    // Stop playback.
    await page.evaluate(() => Player.stopPlayback());

    await expect(page.locator("#player-add-pl")).toBeHidden();
  });

  test("#player-add-pl is hidden during whole-album playback (track-only)", async ({ page }) => {
    await page.goto("/");
    await page.waitForFunction(() => !!window.Player);

    // Start whole-album playback (trackIdx will be null).
    await page.evaluate(() => Player.startAlbum(null, 1, false));
    await waitForPlaying(page);

    await expect(page.locator("#player-add-pl")).toBeHidden();
  });

  test("clicking the player-bar '+' opens the add panel for the current track", async ({ page }) => {
    await page.goto("/");
    await page.waitForFunction(() => !!window.Player);

    await page.evaluate(() => Player.startTrack(null, 1, 0));
    await waitForPlaying(page);
    // Keep the track from ending during the test.
    await page.evaluate(() => { document.getElementById("player-audio").loop = true; });

    await expect(page.locator("#player-add-pl")).toBeVisible();
    await page.locator("#player-add-pl").evaluate(el => el.click());

    // The add panel must open in the sidebar.
    await expect(page.locator("#sidebar-add-section")).toBeVisible();
    // Context label should mention the track title.
    await expect(page.locator(".add-pl-context")).toContainText("Adding");
    await expect(page.locator(".add-pl-context")).toContainText("Celular");
  });

  test("player-bar '+' opens add panel even when queue sidebar was already open", async ({ page }) => {
    await page.goto("/");
    await page.waitForFunction(() => !!window.Player);

    await page.evaluate(() => Player.startTrack(null, 1, 0));
    await waitForPlaying(page);
    await page.evaluate(() => { document.getElementById("player-audio").loop = true; });

    // Open the queue sidebar first.
    await page.evaluate(() => Player.openSidebar());
    await expect(page.locator("body")).toHaveClass(/sidebar-open/);

    // Click the player-bar "+".
    await page.locator("#player-add-pl").evaluate(el => el.click());

    // Add panel should appear over the queue.
    await expect(page.locator("#sidebar-add-section")).toBeVisible();
    await expect(page.locator("#player-sidebar")).toHaveClass(/showing-add/);

    // Closing the add panel should restore the sidebar open state.
    await page.locator(".add-pl-close").click();
    await expect(page.locator("#player-sidebar")).not.toHaveClass(/showing-add/);
    await expect(page.locator("body")).toHaveClass(/sidebar-open/);
  });

  test("player-bar '+' can add the current track to a playlist", async ({ page }) => {
    await page.goto("/");
    await page.waitForFunction(() => !!window.Player);

    const pid = await createPlaylist(page, "Bar Target");

    await page.evaluate(() => Player.startTrack(null, 1, 0));
    await waitForPlaying(page);
    await page.evaluate(() => { document.getElementById("player-audio").loop = true; });

    await page.locator("#player-add-pl").evaluate(el => el.click());
    await expect(page.locator("#sidebar-add-section")).toBeVisible();
    await waitForAddList(page);

    // Click "Bar Target" row to add.
    const row = page.locator(".add-pl-row:not(.add-pl-row-member)", { hasText: "Bar Target" });
    await jsClick(page, row);

    // Row should flip to member.
    await expect(page.locator(".add-pl-row-member", { hasText: "Bar Target" })).toBeVisible();

    // Confirm via API.
    const items = await page.evaluate(async (id) => {
      return (await (await fetch(`/api/playlists/${id}`)).json()).items;
    }, pid);
    expect(items.length).toBe(1);
    expect(items[0].item_type).toBe("track");
    expect(items[0].concert_id).toBe(1);
    expect(items[0].track_index).toBe(0);
  });
});
