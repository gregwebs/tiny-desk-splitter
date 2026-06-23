// Unit tests for the pure player logic in
// concert-tracker/frontend/src/player/core.ts.
// Run with: npm run test:unit (node --import tsx --test js-tests/*.test.ts)
// These cover only the DOM-free logic; the DOM/htmx/media interaction layer
// (../player.ts, and eventually player/widget/) is exercised by the
// Playwright e2e suite under ./e2e (player-queue.spec.js et al).
import test from "node:test";
import assert from "node:assert/strict";

import * as P from "../concert-tracker/frontend/src/player/core";
import type {
  PlaybackState,
  QueueEntry,
} from "../concert-tracker/frontend/src/player/core";
import type { PlaybackItemJson } from "../concert-tracker/frontend/src/api/client";

// ── helpers ──────────────────────────────────────────────────────────────

function entry(
  concertId: number,
  trackIdx: number,
  title: string,
  opts: { liked?: boolean; playlistName?: string | null; groupId?: number | null } = {},
): QueueEntry {
  return P.makeQueueEntry(
    concertId,
    trackIdx,
    title,
    opts.liked ?? false,
    opts.playlistName ?? null,
    opts.groupId ?? null,
  );
}

function basePlayback(overrides: Partial<PlaybackState> = {}): PlaybackState {
  return {
    concertId: null,
    trackIdx: null,
    isVideo: false,
    watchUrl: null,
    hasNext: false,
    hasPrev: false,
    liked: false,
    concert: null,
    ...overrides,
  };
}

function concertItem(overrides: Partial<PlaybackItemJson> = {}): PlaybackItemJson {
  return {
    artist: "Artist",
    is_video: false,
    kind: "track",
    liked: false,
    title: "Item",
    url: "/url",
    ...overrides,
  };
}

// ── makeQueueEntry ───────────────────────────────────────────────────────

test("makeQueueEntry normalizes liked/playlistName/groupId falsy-ness", () => {
  assert.deepEqual(P.makeQueueEntry(1, 2, "Song", 0 as unknown as boolean, "", 0), {
    concertId: 1,
    trackIdx: 2,
    title: "Song",
    liked: false,
    playlistName: null,
    groupId: null,
  });
  assert.deepEqual(P.makeQueueEntry(1, 2, "Song", true, "Mix", 5), {
    concertId: 1,
    trackIdx: 2,
    title: "Song",
    liked: true,
    playlistName: "Mix",
    groupId: 5,
  });
});

// ── enqueueDedupe ────────────────────────────────────────────────────────

test("enqueueDedupe appends a new entry and reports added: true", () => {
  const e1 = entry(1, 0, "A");
  const result = P.enqueueDedupe([], e1);
  assert.equal(result.added, true);
  assert.deepEqual(result.queue, [e1]);
});

test("enqueueDedupe skips a duplicate (concertId, trackIdx) and reports added: false", () => {
  const e1 = entry(1, 0, "A");
  const dup = entry(1, 0, "A again");
  const result = P.enqueueDedupe([e1], dup);
  assert.equal(result.added, false);
  assert.deepEqual(result.queue, [e1]);
});

test("enqueueDedupe does not mutate the input array", () => {
  const original = [entry(1, 0, "A")];
  const frozen = [...original];
  P.enqueueDedupe(original, entry(2, 0, "B"));
  assert.deepEqual(original, frozen);
});

// ── dequeueAt / removeGroup / takeFromQueue ─────────────────────────────

test("dequeueAt removes only the entry at the given position", () => {
  const queue = [entry(1, 0, "A"), entry(1, 1, "B"), entry(1, 2, "C")];
  const result = P.dequeueAt(queue, 1);
  assert.deepEqual(
    result.map((e) => e.title),
    ["A", "C"],
  );
  // input untouched
  assert.equal(queue.length, 3);
});

test("removeGroup removes every entry with the matching groupId, leaves others", () => {
  const queue = [
    entry(1, 0, "A", { groupId: 1 }),
    entry(1, 1, "B", { groupId: 2 }),
    entry(1, 2, "C", { groupId: 1 }),
    entry(1, 3, "D", { groupId: null }),
  ];
  const result = P.removeGroup(queue, 1);
  assert.deepEqual(
    result.map((e) => e.title),
    ["B", "D"],
  );
  assert.equal(queue.length, 4);
});

test("takeFromQueue pops the head (FIFO) and returns the remainder", () => {
  const queue = [entry(1, 0, "A"), entry(1, 1, "B")];
  const result = P.takeFromQueue(queue);
  assert.equal(result.entry?.title, "A");
  assert.deepEqual(
    result.queue.map((e) => e.title),
    ["B"],
  );
  assert.equal(queue.length, 2); // input untouched
});

test("takeFromQueue on an empty queue returns entry: null and an empty queue", () => {
  const result = P.takeFromQueue([]);
  assert.equal(result.entry, null);
  assert.deepEqual(result.queue, []);
});

// ── playlistEntries ──────────────────────────────────────────────────────

test("playlistEntries maps available tracks, skipping null track_index", () => {
  const tracks = [
    { concert_id: 1, track_index: 0, title: "A" },
    { concert_id: 1, track_index: null, title: "B (no index)" },
    { concert_id: 1, track_index: 2, title: "C" },
  ];
  const result = P.playlistEntries(tracks, "My Mix", 7);
  assert.deepEqual(
    result.map((e) => e.title),
    ["A", "C"],
  );
  assert.ok(result.every((e) => e.playlistName === "My Mix" && e.groupId === 7));
});

test("playlistEntries returns an empty array for no tracks", () => {
  assert.deepEqual(P.playlistEntries([], "Empty", 1), []);
});

// ── buildQueueRows ───────────────────────────────────────────────────────

test("buildQueueRows: empty queue yields no rows", () => {
  const { rows, nonContiguousGroups } = P.buildQueueRows([]);
  assert.deepEqual(rows, []);
  assert.deepEqual(nonContiguousGroups, []);
});

test("buildQueueRows: single ad-hoc entry yields one song row, reversed order n/a", () => {
  const queue = [entry(1, 0, "A")];
  const { rows } = P.buildQueueRows(queue);
  assert.deepEqual(rows, [{ kind: "song", pos: 0, entry: queue[0], nested: false }]);
});

test("buildQueueRows: two ad-hoc entries render reversed (highest index first), no headers", () => {
  const queue = [entry(1, 0, "A"), entry(1, 1, "B")];
  const { rows, nonContiguousGroups } = P.buildQueueRows(queue);
  assert.deepEqual(
    rows.map((r) => (r.kind === "song" ? r.entry.title : r)),
    ["B", "A"],
  );
  assert.ok(rows.every((r) => r.kind === "song" && !r.nested));
  assert.deepEqual(nonContiguousGroups, []);
});

test("buildQueueRows: one playlist group of N renders one header + N nested rows", () => {
  const queue = [
    entry(1, 0, "A", { groupId: 1, playlistName: "Mix" }),
    entry(1, 1, "B", { groupId: 1, playlistName: "Mix" }),
    entry(1, 2, "C", { groupId: 1, playlistName: "Mix" }),
  ];
  const { rows, nonContiguousGroups } = P.buildQueueRows(queue);
  // Reversed: header appears once, just before the highest-index song in the group.
  assert.deepEqual(
    rows.map((r) => (r.kind === "group-header" ? `header:${r.name}` : r.entry.title)),
    ["header:Mix", "C", "B", "A"],
  );
  assert.ok(
    rows.filter((r) => r.kind === "song").every((r) => r.kind === "song" && r.nested),
  );
  assert.deepEqual(nonContiguousGroups, []);
});

test("buildQueueRows: two adjacent groups each get their own header", () => {
  const queue = [
    entry(1, 0, "A", { groupId: 1, playlistName: "First" }),
    entry(1, 1, "B", { groupId: 2, playlistName: "Second" }),
    entry(1, 2, "C", { groupId: 2, playlistName: "Second" }),
  ];
  const { rows, nonContiguousGroups } = P.buildQueueRows(queue);
  assert.deepEqual(
    rows.map((r) => (r.kind === "group-header" ? `header:${r.name}` : r.entry.title)),
    ["header:Second", "C", "B", "header:First", "A"],
  );
  assert.deepEqual(nonContiguousGroups, []);
});

test("buildQueueRows: an ad-hoc entry between two groups breaks contiguity (header per run)", () => {
  const queue = [
    entry(1, 0, "A", { groupId: 1, playlistName: "Mix" }),
    entry(1, 1, "B"), // ad-hoc
    entry(1, 2, "C", { groupId: 1, playlistName: "Mix" }),
  ];
  const { rows, nonContiguousGroups } = P.buildQueueRows(queue);
  // Walking reversed: C (new header for group 1), B (ad-hoc, no header), A (group 1
  // reappears -> a second header, reported as non-contiguous).
  assert.deepEqual(
    rows.map((r) => (r.kind === "group-header" ? `header:${r.name}` : r.entry.title)),
    ["header:Mix", "C", "B", "header:Mix", "A"],
  );
  assert.deepEqual(nonContiguousGroups, [1]);
});

test("buildQueueRows: groupId reappearing non-contiguously is reported once per reappearance", () => {
  const queue = [
    entry(1, 0, "A", { groupId: 1, playlistName: "Mix" }),
    entry(1, 1, "B", { groupId: 2, playlistName: "Other" }),
    entry(1, 2, "C", { groupId: 1, playlistName: "Mix" }),
    entry(1, 3, "D", { groupId: 2, playlistName: "Other" }),
  ];
  const { nonContiguousGroups } = P.buildQueueRows(queue);
  assert.deepEqual(nonContiguousGroups, [2, 1]);
});

// ── next/prev enablement ─────────────────────────────────────────────────

test("nextEnabled: non-concert mode follows queue length / hasNext", () => {
  assert.equal(P.nextEnabled(basePlayback(), 0), false);
  assert.equal(P.nextEnabled(basePlayback(), 1), true);
  assert.equal(P.nextEnabled(basePlayback({ hasNext: true }), 0), true);
  assert.equal(P.nextEnabled(basePlayback({ hasNext: false }), 0), false);
});

test("nextEnabled: concert mode ignores queue/hasNext, follows item position", () => {
  const concert = { id: 1, items: [concertItem(), concertItem(), concertItem()], pos: 1 };
  assert.equal(P.nextEnabled(basePlayback({ concert }), 0), true);
  const atEnd = { ...concert, pos: 2 };
  assert.equal(P.nextEnabled(basePlayback({ concert: atEnd, hasNext: true }), 5), false);
});

test("prevEnabled: non-concert mode follows hasPrev", () => {
  assert.equal(P.prevEnabled(basePlayback({ hasPrev: true })), true);
  assert.equal(P.prevEnabled(basePlayback({ hasPrev: false })), false);
});

test("prevEnabled: concert mode follows item position, ignoring hasPrev", () => {
  const atStart = { id: 1, items: [concertItem(), concertItem()], pos: 0 };
  assert.equal(P.prevEnabled(basePlayback({ concert: atStart, hasPrev: true })), false);
  const midway = { ...atStart, pos: 1 };
  assert.equal(P.prevEnabled(basePlayback({ concert: midway, hasPrev: false })), true);
});

// ── concert navigation math ──────────────────────────────────────────────

test("concertItemNav reports hasPrev/hasNext/item for a middle position", () => {
  const items = [concertItem({ title: "A" }), concertItem({ title: "B" }), concertItem({ title: "C" })];
  const nav = P.concertItemNav(items, 1);
  assert.equal(nav.hasPrev, true);
  assert.equal(nav.hasNext, true);
  assert.equal(nav.item?.title, "B");
});

test("concertItemNav at the start/end has hasPrev/hasNext false respectively", () => {
  const items = [concertItem({ title: "A" }), concertItem({ title: "B" })];
  assert.equal(P.concertItemNav(items, 0).hasPrev, false);
  assert.equal(P.concertItemNav(items, 1).hasNext, false);
});

test("concertItemNav out of range returns item: null", () => {
  const items = [concertItem()];
  assert.equal(P.concertItemNav(items, 5).item, null);
});

test("concertAdvancePos returns the next position, or null at the end", () => {
  assert.equal(P.concertAdvancePos(0, 3), 1);
  assert.equal(P.concertAdvancePos(1, 3), 2);
  assert.equal(P.concertAdvancePos(2, 3), null);
});

test("refindPosByUrl re-finds the matching item by url", () => {
  const items = [concertItem({ url: "/a" }), concertItem({ url: "/b" }), concertItem({ url: "/c" })];
  assert.equal(P.refindPosByUrl(items, "/c", 0), 2);
});

test("refindPosByUrl falls back to fallbackPos when there's no current url or no match", () => {
  const items = [concertItem({ url: "/a" }), concertItem({ url: "/b" })];
  assert.equal(P.refindPosByUrl(items, null, 1), 1);
  assert.equal(P.refindPosByUrl(items, "/missing", 1), 1);
});

// ── formatTime ────────────────────────────────────────────────────────────

test("formatTime pads single-digit seconds and floors fractional input", () => {
  assert.equal(P.formatTime(0), "0:00");
  assert.equal(P.formatTime(5), "0:05");
  assert.equal(P.formatTime(65), "1:05");
  assert.equal(P.formatTime(125.9), "2:05");
  assert.equal(P.formatTime(3600), "60:00");
});

// ── clampSidebarWidth ─────────────────────────────────────────────────────

test("clampSidebarWidth clamps to [240, 600] and rounds", () => {
  assert.equal(P.clampSidebarWidth(100), 240);
  assert.equal(P.clampSidebarWidth(240), 240);
  assert.equal(P.clampSidebarWidth(400.4), 400);
  assert.equal(P.clampSidebarWidth(400.6), 401);
  assert.equal(P.clampSidebarWidth(600), 600);
  assert.equal(P.clampSidebarWidth(1000), 600);
});

// ── clickShouldDismiss ────────────────────────────────────────────────────
// A minimal DOM-free stand-in: an object satisfying the bits clickShouldDismiss
// touches (instanceof Node/Element checks need real DOM, so these tests use
// happy-dom-free plain objects only for the "no container/target" early-outs;
// the interactive-element/contains paths are covered by the Scene/e2e DOM
// tests that have a real document).

test("clickShouldDismiss returns false when target or container is missing", () => {
  assert.equal(P.clickShouldDismiss(null, null), false);
  assert.equal(P.clickShouldDismiss({} as EventTarget, null), false);
});

// ── key predicates ────────────────────────────────────────────────────────

function key(overrides: Partial<KeyboardEvent>): KeyboardEvent {
  return {
    code: "",
    key: "",
    ctrlKey: false,
    metaKey: false,
    altKey: false,
    shiftKey: false,
    ...overrides,
  } as KeyboardEvent;
}

test("isPlainSpaceKey matches a bare space across code/key spellings", () => {
  assert.equal(P.isPlainSpaceKey(key({ code: "Space" })), true);
  assert.equal(P.isPlainSpaceKey(key({ key: " " })), true);
  assert.equal(P.isPlainSpaceKey(key({ key: "Spacebar" })), true);
});

test("isPlainSpaceKey rejects space with a modifier held", () => {
  assert.equal(P.isPlainSpaceKey(key({ code: "Space", ctrlKey: true })), false);
  assert.equal(P.isPlainSpaceKey(key({ code: "Space", metaKey: true })), false);
  assert.equal(P.isPlainSpaceKey(key({ code: "Space", altKey: true })), false);
  assert.equal(P.isPlainSpaceKey(key({ code: "Space", shiftKey: true })), false);
});

test("isPlainSpaceKey rejects non-space keys", () => {
  assert.equal(P.isPlainSpaceKey(key({ code: "Enter", key: "Enter" })), false);
});

test("isPlainEscapeKey matches a bare escape across code/key spellings", () => {
  assert.equal(P.isPlainEscapeKey(key({ code: "Escape" })), true);
  assert.equal(P.isPlainEscapeKey(key({ key: "Escape" })), true);
  assert.equal(P.isPlainEscapeKey(key({ key: "Esc" })), true);
});

test("isPlainEscapeKey rejects escape with a modifier held", () => {
  assert.equal(P.isPlainEscapeKey(key({ code: "Escape", ctrlKey: true })), false);
});

test("isPlainEscapeKey rejects non-escape keys", () => {
  assert.equal(P.isPlainEscapeKey(key({ code: "Enter", key: "Enter" })), false);
});
