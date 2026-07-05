"use strict";

// Verification spec for:
//  1. Sidebar top-right "X" (normal mode) closes the sidebar.
//  2. Add-to-playlist mode: sidebar "X" disappears; add panel "×" appears and
//     closes add mode only.
//  3. Resize drag handle changes sidebar width; width is persisted in localStorage
//     and restored on next load.

const { test, expect } = require("./fixtures");

// helpers from the existing pattern
async function openSidebar(page) {
  await page.evaluate(() => Player.openSidebar());
  await page.waitForFunction(() => document.body.classList.contains("sidebar-open"));
}

async function openAddPanelForTrack(page) {
  // Navigate to a concert detail page so we have track rows.
  await page.goto("/concerts/1");
  await page.waitForSelector(".track-list li");
  const trackLi = page.locator(".track-list li").first();
  await trackLi.hover();
  const addBtn = trackLi.locator(".btn-add-pl");
  await addBtn.waitFor({ state: "visible" });
  await addBtn.click();
  await expect(page.locator("#sidebar-add-section")).toBeVisible();
}

test.describe("Sidebar close + resize", () => {
  // ── 1. Normal-mode close button ──────────────────────────────────────────────
  test("sidebar close button exists and closes the sidebar", async ({ page }) => {
    await page.goto("/");
    // Start a track so the player bar / sidebar are live.
    await page.evaluate(() => Player.startTrack(null, 1, 0));
    await page.waitForFunction(() => {
      const a = document.getElementById("player-audio");
      return a && !a.paused;
    });

    await openSidebar(page);

    // Button must be visible in normal (non-add) mode.
    const closeBtn = page.locator("#sidebar-close");
    await expect(closeBtn).toBeVisible();

    // Clicking it must close the sidebar.
    await closeBtn.click();
    await page.waitForFunction(() => !document.body.classList.contains("sidebar-open"));
    await expect(page.locator("body")).not.toHaveClass(/sidebar-open/);

    // The player bar toggle should report aria-expanded=false.
    await expect(page.locator("#player-queue-toggle")).toHaveAttribute("aria-expanded", "false");
  });

  // ── 2. Add-to-playlist mode: mutual exclusion of the two X buttons ───────────
  test("sidebar X hidden in add mode; add panel X closes add mode only", async ({ page }) => {
    await openAddPanelForTrack(page);

    // Sidebar is now in add mode (showing-add class).
    await page.waitForFunction(() =>
      document.body.classList.contains("showing-add")
    );

    // Sidebar-level X must be hidden.
    const sidebarClose = page.locator("#sidebar-close");
    await expect(sidebarClose).toBeHidden();

    // Add panel's own X must be visible.
    const addClose = page.locator(".add-pl-close");
    await expect(addClose).toBeVisible();

    // Track whether the sidebar was open before add mode.
    const wasOpen = await page.evaluate(() => {
      // openAdd() remembers sidebarWasOpen; we can't read it directly,
      // but we can read whether the sidebar-open class is currently set.
      return document.body.classList.contains("sidebar-open");
    });

    // Click the add panel X (calls Playlists.closeAdd).
    await addClose.click();

    // Add mode must be gone.
    await page.waitForFunction(() =>
      !document.body.classList.contains("showing-add")
    );
    await expect(page.locator("#sidebar-add-section")).toBeHidden();

    // Sidebar-level X should be visible again if sidebar stayed open,
    // or simply not be in add mode.
    if (wasOpen) {
      await expect(sidebarClose).toBeVisible();
    } else {
      // sidebar should have closed (or at least is no longer in add mode).
      const stillInAddMode = await page.evaluate(() =>
        document.body.classList.contains("showing-add")
      );
      expect(stillInAddMode).toBe(false);
    }
  });

  // ── 3. Resize drag handle + localStorage persistence ─────────────────────────
  test("resize handle changes sidebar width and localStorage persists it", async ({ page, context }) => {
    await page.setViewportSize({ width: 1280, height: 800 });
    await page.goto("/");
    await page.evaluate(() => Player.startTrack(null, 1, 0));
    await page.waitForFunction(() => {
      const a = document.getElementById("player-audio");
      return a && !a.paused;
    });
    await openSidebar(page);

    // Simulate a drag: fire pointer events on #sidebar-resize to move width from 320 → 450.
    // The sidebar is left:0, so clientX = new width.
    const newWidth = 450;
    await page.evaluate((w) => {
      const handle = document.getElementById("sidebar-resize");
      if (!handle) throw new Error("sidebar-resize not found");
      const rect = handle.getBoundingClientRect();
      const cx = rect.left + rect.width / 2;
      const cy = rect.top + rect.height / 2;

      function fire(type, clientX) {
        const e = new PointerEvent(type, { bubbles: true, cancelable: true, clientX, clientY: cy, pointerId: 1 });
        handle.dispatchEvent(e);
      }
      fire("pointerdown", cx);
      // Move: trigger pointermove with the target clientX.
      fire("pointermove", w);
      fire("pointerup", w);
    }, newWidth);

    // CSS var should reflect the new width.
    const cssWidth = await page.evaluate(() =>
      getComputedStyle(document.documentElement).getPropertyValue("--sidebar-width").trim()
    );
    expect(cssWidth).toBe(`${newWidth}px`);

    // localStorage must have been written.
    const stored = await page.evaluate(() => localStorage.getItem("sidebarWidth"));
    expect(stored).toBe(String(newWidth));

    // Open a new page in the same context (simulates reload / new tab with same storage).
    const page2 = await context.newPage();
    await page2.goto("/");

    // loadSidebarWidth runs on DOMContentLoaded in init() — verify the CSS var.
    const restoredWidth = await page2.evaluate(() =>
      getComputedStyle(document.documentElement).getPropertyValue("--sidebar-width").trim()
    );
    expect(restoredWidth).toBe(`${newWidth}px`);
    await page2.close();
  });

  // ── 4. Probe: click-without-drag must not persist width 0 ───────────────────
  test("click-without-drag on resize handle does not persist width 0", async ({ page }) => {
    await page.setViewportSize({ width: 1280, height: 800 });
    await page.goto("/");

    // Clear any stale storage.
    await page.evaluate(() => localStorage.removeItem("sidebarWidth"));

    await page.evaluate(() => Player.startTrack(null, 1, 0));
    await page.waitForFunction(() => {
      const a = document.getElementById("player-audio");
      return a && !a.paused;
    });
    await openSidebar(page);

    // Fire pointerdown then pointerup at the same position — no pointermove.
    await page.evaluate(() => {
      const handle = document.getElementById("sidebar-resize");
      if (!handle) throw new Error("sidebar-resize not found");
      const rect = handle.getBoundingClientRect();
      const cx = rect.left + rect.width / 2;
      const cy = rect.top + rect.height / 2;
      const opts = { bubbles: true, cancelable: true, clientX: cx, clientY: cy, pointerId: 1 };
      handle.dispatchEvent(new PointerEvent("pointerdown", opts));
      handle.dispatchEvent(new PointerEvent("pointerup", opts));
    });

    const stored = await page.evaluate(() => localStorage.getItem("sidebarWidth"));
    // Must NOT be "0" — either null (preferred) or the original valid width.
    expect(stored).not.toBe("0");
  });
});
