"use strict";

const { test, expect, openTracks } = require("./fixtures");

// Fixture: concert 1 "Audio Concert" — Celular(0), Limbo(1), Track Three(2), Dando Vueltas(3)
const AUDIO = 1;

// Helpers to set up playlists via the JSON API.
async function createPlaylist(page, name) {
  const res = await page.evaluate(async (n) => {
    const r = await fetch("/api/playlists", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ name: n, description: "" }),
    });
    return { ok: r.ok, json: await r.json() };
  }, name);
  if (!res.ok) throw new Error("createPlaylist failed for: " + name);
  return res.json.id;
}

async function addTrackToPlaylist(page, playlistId, concertId, trackIndex) {
  await page.evaluate(
    async ({ playlistId, concertId, trackIndex }) => {
      await fetch(`/api/playlists/${playlistId}/items`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          type: "track",
          concert_id: concertId,
          track_index: trackIndex,
        }),
      });
    },
    { playlistId, concertId, trackIndex }
  );
}

// Open the add-to-playlist panel for a track via its "+" button.
async function openAddPanel(page, concertId, trackIdx) {
  await openTracks(page, concertId);
  // Trigger the "+" button via evaluate to avoid pointer-event issues.
  await page.evaluate(
    ({ cid, idx }) => {
      const btn = document.querySelector(
        `.btn-add-pl[onclick*="concertId:${cid}"][onclick*="trackIndex:${idx}"]`
      );
      if (!btn) throw new Error("no btn-add-pl found for concert " + cid + " track " + idx);
      btn.click();
    },
    { cid: concertId, idx: trackIdx }
  );
  // Wait for the panel to be visible.
  await page.waitForSelector("#sidebar-add-section:not(.hidden)");
  // Wait for real playlist rows (not just the "Loading..." placeholder).
  await page.waitForSelector("#add-pl-list li .add-pl-name");
}

test.describe("Add-to-playlist sidebar: member ordering", () => {
  test.beforeEach(async ({ page }) => {
    await page.goto("/");
  });

  test("member playlists appear at the top when filter is empty", async ({ page }) => {
    // Set up: create two non-member playlists + one member playlist.
    // Alphabetical order: Alpha (non-member), Beta (member), Gamma (non-member)
    const alphaId = await createPlaylist(page, "Alpha");
    const betaId = await createPlaylist(page, "Beta");
    const gammaId = await createPlaylist(page, "Gamma");
    await addTrackToPlaylist(page, betaId, AUDIO, 0);

    await openAddPanel(page, AUDIO, 0);

    // Filter is empty — member "Beta" should be first row, then non-members.
    const rows = await page.locator("#add-pl-list li.add-pl-row").all();
    expect(rows.length).toBeGreaterThanOrEqual(3);

    const firstText = await rows[0].locator(".add-pl-name").textContent();
    expect(firstText).toBe("Beta");
    expect(await rows[0].evaluate((el) => el.classList.contains("add-pl-row-member"))).toBe(true);

    // Non-members follow (Alpha before Gamma alphabetically).
    const secondText = await rows[1].locator(".add-pl-name").textContent();
    const thirdText = await rows[2].locator(".add-pl-name").textContent();
    expect(secondText).toBe("Alpha");
    expect(thirdText).toBe("Gamma");
    expect(await rows[1].evaluate((el) => el.classList.contains("add-pl-row-member"))).toBe(false);
    expect(await rows[2].evaluate((el) => el.classList.contains("add-pl-row-member"))).toBe(false);
  });

  test("member playlists move to the bottom when filter has text", async ({ page }) => {
    const alphaId = await createPlaylist(page, "Alpha Playlist");
    const betaId = await createPlaylist(page, "Beta Playlist");
    const gammaId = await createPlaylist(page, "Gamma Playlist");
    await addTrackToPlaylist(page, betaId, AUDIO, 0);

    await openAddPanel(page, AUDIO, 0);

    // Type a filter that matches all three.
    const filter = page.locator("#add-pl-filter");
    await filter.fill("playlist");

    // Wait for re-render.
    await page.waitForFunction(() => {
      const rows = document.querySelectorAll("#add-pl-list li.add-pl-row");
      return rows.length >= 3;
    });

    // With filter: non-members first, the single member ("Beta Playlist") sinks
    // to the bottom. The widget re-renders asynchronously (MVU), so use
    // auto-retrying assertions on the final reordered state rather than reading
    // a mid-reorder snapshot.
    await expect(
      page.locator("#add-pl-list li.add-pl-row-member", { hasText: "Beta Playlist" })
    ).toBeVisible();
    // Exactly one member row, and it is the last playlist row (so everything
    // above it is a non-member).
    await expect(page.locator("#add-pl-list li.add-pl-row-member")).toHaveCount(1);
    await expect(page.locator("#add-pl-list li.add-pl-row").last()).toHaveClass(
      /add-pl-row-member/
    );
  });

  test("clearing the filter moves members back to the top", async ({ page }) => {
    const alphaId = await createPlaylist(page, "Aardvark");
    const betaId = await createPlaylist(page, "Zebra");
    await addTrackToPlaylist(page, betaId, AUDIO, 0);

    await openAddPanel(page, AUDIO, 0);

    // Filter to match both.
    const filter = page.locator("#add-pl-filter");
    await filter.fill("a");
    await page.waitForFunction(() => {
      const rows = document.querySelectorAll("#add-pl-list li.add-pl-row");
      // After filtering, member (Zebra) should be last.
      if (rows.length < 2) return false;
      return rows[rows.length - 1].classList.contains("add-pl-row-member");
    });

    // Clear the filter.
    await filter.fill("");
    await page.waitForFunction(() => {
      const rows = document.querySelectorAll("#add-pl-list li.add-pl-row");
      // Member (Zebra) should now be first.
      return rows.length >= 2 && rows[0].classList.contains("add-pl-row-member");
    });

    const rows = await page.locator("#add-pl-list li.add-pl-row").all();
    const firstText = await rows[0].locator(".add-pl-name").textContent();
    expect(firstText).toBe("Zebra");
    expect(await rows[0].evaluate((el) => el.classList.contains("add-pl-row-member"))).toBe(true);
  });

  test("arrow-key navigation follows display order (empty filter: members-first)", async ({ page }) => {
    const alphaId = await createPlaylist(page, "Alpha");
    const betaId = await createPlaylist(page, "Beta"); // member
    const gammaId = await createPlaylist(page, "Gamma");
    await addTrackToPlaylist(page, betaId, AUDIO, 0);

    await openAddPanel(page, AUDIO, 0);

    const filter = page.locator("#add-pl-filter");
    await filter.focus();

    // First ArrowDown should highlight the first row — which should be the member "Beta".
    // The widget re-renders asynchronously (MVU), so use auto-retrying assertions.
    const activeName = page.locator("#add-pl-list .add-pl-row-active .add-pl-name");
    await filter.press("ArrowDown");
    await expect(activeName).toHaveText("Beta");
    await expect(page.locator("#add-pl-list .add-pl-row-active")).toHaveClass(/add-pl-row-member/);

    // Second ArrowDown -> "Alpha" (first non-member).
    await filter.press("ArrowDown");
    await expect(activeName).toHaveText("Alpha");

    // Third ArrowDown -> "Gamma".
    await filter.press("ArrowDown");
    await expect(activeName).toHaveText("Gamma");
  });

  test("arrow-key navigation follows display order (with filter: members-last)", async ({ page }) => {
    const alphaId = await createPlaylist(page, "Alpha");
    const betaId = await createPlaylist(page, "Beta"); // member
    await addTrackToPlaylist(page, betaId, AUDIO, 0);

    await openAddPanel(page, AUDIO, 0);

    const filter = page.locator("#add-pl-filter");
    await filter.fill("a"); // matches both Alpha and Beta

    // Wait for re-render to show 2 rows.
    await page.waitForFunction(() => document.querySelectorAll("#add-pl-list li.add-pl-row").length >= 2);

    // Focus the filter and press ArrowDown — first highlight should be non-member "Alpha".
    // The widget re-renders asynchronously (MVU), so use auto-retrying assertions.
    const activeName = page.locator("#add-pl-list .add-pl-row-active .add-pl-name");
    await filter.focus();
    await filter.press("ArrowDown");
    await expect(activeName).toHaveText("Alpha");
    await expect(page.locator("#add-pl-list .add-pl-row-active")).not.toHaveClass(/add-pl-row-member/);

    // Second ArrowDown -> Create "a" row (positioned between non-members and members).
    await filter.press("ArrowDown");
    await expect(activeName).toContainText("Create");

    // Third ArrowDown -> member "Beta" (now at the bottom, after the Create row).
    await filter.press("ArrowDown");
    await expect(activeName).toHaveText("Beta");
    await expect(page.locator("#add-pl-list .add-pl-row-active")).toHaveClass(/add-pl-row-member/);
  });
});
