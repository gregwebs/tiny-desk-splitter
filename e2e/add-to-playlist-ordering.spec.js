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
