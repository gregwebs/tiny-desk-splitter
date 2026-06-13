const { test, expect } = require("./fixtures");

// The inline track splitter on the concert detail page (docs/change/
// 2026-06-13-splitter-timeline-ui.md). Drives the real split-timestamps
// endpoints; the split itself runs through stub-splitter.js. Uses fixture
// concert 2 ("Second Concert" — split, 3 playable wav tracks), which the
// fixture seeds with automated split timestamps over the ~20s source file.
const ID = 2;

const toggle = ".splitter-toggle";
const timeline = "#splitter .splitter-timeline";
const seg = "#splitter .splitter-seg";
const handle = "#splitter .splitter-handle";
const gap = "#splitter .splitter-gap";
const detachBtn = "#splitter .splitter-detach";
const submit = "#splitter .splitter-submit";
const reset = "#splitter .splitter-reset";
const status = "#splitter .splitter-status";
const rows = "#splitter .splitter-table tbody tr";

// The start/end inputs of a given track row (0-based).
function startInput(page, i) {
  return page.locator(rows).nth(i).locator(".splitter-time").nth(0);
}
function endInput(page, i) {
  return page.locator(rows).nth(i).locator(".splitter-time").nth(1);
}

async function openSplitter(page) {
  await page.goto(`/concerts/${ID}`);
  await expect(page.locator(toggle)).toBeVisible();
  await page.click(toggle);
  await expect(page.locator(timeline)).toBeVisible();
  await expect(page.locator(seg)).toHaveCount(3);
}

test.describe("track splitter", () => {
  test("loads auto timestamps into the timeline and table", async ({ page }) => {
    await openSplitter(page);
    // Three contiguous segments → no gaps, 4 handles (head + 2 linked + tail).
    await expect(page.locator(handle)).toHaveCount(4);
    await expect(page.locator(rows)).toHaveCount(3);
    // First track starts at 0; values come from the seeded auto split.
    await expect(startInput(page, 0)).toHaveValue("0:00.0");
    // Detach buttons read "Detach" while boundaries are linked.
    await expect(page.locator(detachBtn).first()).toContainText("Detach");
    // Submitting the unchanged auto split is allowed (within media duration).
    await expect(page.locator(submit)).toBeEnabled();
  });

  test("editing a linked boundary moves both adjacent times", async ({ page }) => {
    await openSplitter(page);
    await endInput(page, 0).fill("0:05.0");
    await endInput(page, 0).blur();
    // Linked: track 1's start follows track 0's end.
    await expect(startInput(page, 1)).toHaveValue("0:05.0");
  });

  test("detach opens a gap, submit re-cuts with the gap, reset returns to auto", async ({
    page,
  }) => {
    await openSplitter(page);

    // Detach the first boundary, then pull the two handles apart via the inputs.
    await page.locator(detachBtn).first().click();
    await expect(page.locator(detachBtn).first()).toContainText("Link");
    await endInput(page, 0).fill("0:05.0");
    await endInput(page, 0).blur();
    await startInput(page, 1).fill("0:08.0");
    await startInput(page, 1).blur();
    // A gap block becomes visible between the first two tracks.
    await expect(page.locator(gap).first()).toBeVisible();

    await page.click(submit);
    await expect(page.locator(status)).toContainText("Splitting");

    // The job (stub) completes and the user column is persisted with our gap.
    await expect
      .poll(
        async () => {
          const r = await page.request.get(`/concerts/${ID}/split-timestamps`);
          const j = await r.json();
          return j.user ? j.user.length : 0;
        },
        { timeout: 10000 }
      )
      .toBe(3);
    const user = await (await page.request.get(`/concerts/${ID}/split-timestamps`)).json();
    expect(user.user[0].end_time).toBeCloseTo(5.0, 1);
    expect(user.user[1].start_time).toBeCloseTo(8.0, 1);
    expect(user.media_duration).toBeGreaterThan(15);

    // Reset clears the user column back to auto.
    await page.reload();
    await page.click(toggle);
    await expect(page.locator(reset)).toBeEnabled();
    await page.click(reset);
    await expect(page.locator(status)).toContainText("Splitting");
    await expect
      .poll(
        async () => {
          const r = await page.request.get(`/concerts/${ID}/split-timestamps`);
          const j = await r.json();
          return j.user === null;
        },
        { timeout: 10000 }
      )
      .toBe(true);
  });

  test("editing below the 1s minimum is clamped, keeping submit enabled", async ({ page }) => {
    await openSplitter(page);
    await page.locator(detachBtn).first().click();
    // Try to shrink track 0 below the 1s minimum; the input snaps to 0:01.0.
    await endInput(page, 0).fill("0:00.4");
    await endInput(page, 0).blur();
    await expect(endInput(page, 0)).toHaveValue("0:01.0");
    await expect(page.locator(submit)).toBeEnabled();
  });

  test("dragging the tail handle to the far right clamps within media duration", async ({
    page,
  }) => {
    await openSplitter(page);
    const box = await page.locator(timeline).boundingBox();
    const tail = page.locator(handle).last();
    const hb = await tail.boundingBox();
    // Drive raw pointer events (the handle uses pointer capture, which
    // dragTo's HTML-drag emulation doesn't trigger).
    await page.mouse.move(hb.x + hb.width / 2, hb.y + hb.height / 2);
    await page.mouse.down();
    await page.mouse.move(box.x + box.width - 1, box.y + box.height / 2, { steps: 10 });
    await page.mouse.up();
    // The end was clamped to the media duration, not beyond it.
    await expect(endInput(page, 2)).toHaveValue(/0:(19\.[5-9]|20\.0)/);
    await expect(page.locator(submit)).toBeEnabled();
    await page.click(submit);
    await expect(page.locator(status)).toContainText("Splitting");
  });
});
