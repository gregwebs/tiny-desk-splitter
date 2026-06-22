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

  test("Enter on an arrow-highlighted member row removes it (unlike a row click, which is a no-op)", async ({
    page,
  }) => {
    const betaId = await createPlaylist(page, "Beta"); // member
    await addTrackToPlaylist(page, betaId, AUDIO, 0);

    await openAddPanel(page, AUDIO, 0);

    // Empty filter → the member "Beta" is the first row. ArrowDown highlights it.
    const filter = page.locator("#add-pl-filter");
    await filter.focus();
    await filter.press("ArrowDown");
    const activeName = page.locator("#add-pl-list .add-pl-row-active .add-pl-name");
    await expect(activeName).toHaveText("Beta");
    await expect(page.locator("#add-pl-list .add-pl-row-active")).toHaveClass(/add-pl-row-member/);

    // Enter on an arrow-highlighted member toggles it off (commandsForRow member
    // → RemoveItem), unlike a mouse click on a member row, which is a no-op.
    await filter.press("Enter");

    // The row flips to a non-member, and the track is gone from the playlist.
    await expect(page.locator("#add-pl-list li.add-pl-row-member", { hasText: "Beta" })).toHaveCount(0);
    await expect(page.locator("#add-pl-list .add-pl-name", { hasText: "Beta" })).toBeVisible();
    const items = await page.evaluate(async (id) => {
      const r = await fetch(`/api/playlists/${id}`);
      return (await r.json()).items;
    }, betaId);
    expect(items.length).toBe(0);
  });

  test("a slow membership fetch for a superseded target does not clobber the current one", async ({
    page,
  }) => {
    // "Shared" contains track 0 (Celular) but not track 1 (Limbo).
    const pid = await createPlaylist(page, "Shared");
    await addTrackToPlaylist(page, pid, AUDIO, 0);

    await openTracks(page, AUDIO);

    // Hold track 0's membership response until we release it; track 1's is normal.
    let releaseTrack0;
    const track0Held = new Promise((resolve) => (releaseTrack0 = resolve));
    await page.route("**/api/concerts/1/tracks/0/playlists", async (route) => {
      await track0Held;
      await route.continue();
    });

    // Open the panel for track 0 (its load is now stuck), then immediately for
    // track 1 (supersedes track 0; loads normally).
    await page.evaluate(() => document.querySelector('.btn-add-pl[onclick*="trackIndex:0"]').click());
    await page.evaluate(() => document.querySelector('.btn-add-pl[onclick*="trackIndex:1"]').click());

    // Panel now shows track 1 ("Limbo"): "Shared" is a non-member.
    await expect(page.locator("#add-pl-context")).toContainText("Limbo");
    await expect(page.locator("#add-pl-list .add-pl-name", { hasText: "Shared" })).toBeVisible();
    await expect(
      page.locator("#add-pl-list li.add-pl-row-member", { hasText: "Shared" })
    ).toHaveCount(0);

    // Release the stale track-0 response — the forTarget staleness rule must
    // drop it (it would otherwise show "Shared" as a member of track 1).
    releaseTrack0();
    await page.waitForTimeout(500);

    await expect(page.locator("#add-pl-context")).toContainText("Limbo");
    await expect(
      page.locator("#add-pl-list li.add-pl-row-member", { hasText: "Shared" })
    ).toHaveCount(0);
  });
});
