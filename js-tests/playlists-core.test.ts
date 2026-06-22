// Unit tests for the pure add-to-playlist logic in
// concert-tracker/frontend/src/playlists/core.ts.
// Run with: npm run test:unit (node --import tsx --test js-tests/*.test.ts)
// These cover only the DOM-free logic; the widget/DOM layer is exercised by
// the Playwright e2e suite (e2e/add-to-playlist*.spec.js).
import test from "node:test";
import assert from "node:assert/strict";

import * as C from "../concert-tracker/frontend/src/playlists/core";
import type { Member, PlaylistRef, RowId } from "../concert-tracker/frontend/src/playlists/core";
import type { AddTarget } from "../concert-tracker/frontend/src/shared/playlists-api";

const pls = (...names: [number, string][]): PlaylistRef[] =>
  names.map(([id, name]) => ({ id, name }));

const members = (...ids: number[]): Member[] => ids.map((id) => ({ playlistId: id, itemId: id * 10 }));

const ids = (rows: { id: RowId }[]): RowId[] => rows.map((r) => r.id);
const kinds = (rows: { kind: string }[]): string[] => rows.map((r) => r.kind);

// ── targetLabel ─────────────────────────────────────────────────────────────

test("targetLabel covers all target types, with and without a label", () => {
  assert.equal(C.targetLabel({ type: "track", concertId: 1, trackIndex: 2 }), "Adding track to…");
  assert.equal(
    C.targetLabel({ type: "track", concertId: 1, trackIndex: 2, label: "Song" }),
    "Adding “Song” to…",
  );
  assert.equal(C.targetLabel({ type: "concert", concertId: 1 }), "Adding concert to…");
  assert.equal(
    C.targetLabel({ type: "concert", concertId: 1, label: "Gig" }),
    "Adding “Gig” to…",
  );
  assert.equal(C.targetLabel({ type: "playlist", childPlaylistId: 3 }), "Nesting playlist into…");
  assert.equal(
    C.targetLabel({ type: "playlist", childPlaylistId: 3, label: "Mix" }),
    "Nesting “Mix” into…",
  );
});

// ── addItemBody ─────────────────────────────────────────────────────────────

test("addItemBody maps each target type to its request body", () => {
  assert.deepEqual(C.addItemBody({ type: "track", concertId: 7, trackIndex: 4 }), {
    type: "track",
    concert_id: 7,
    track_index: 4,
  });
  assert.deepEqual(C.addItemBody({ type: "concert", concertId: 7 }), {
    type: "concert",
    concert_id: 7,
  });
  assert.deepEqual(C.addItemBody({ type: "playlist", childPlaylistId: 9 }), {
    type: "playlist",
    child_playlist_id: 9,
  });
});

// addItemBody must accept every AddTarget variant (compile-time exhaustiveness).
const _targets: AddTarget[] = [
  { type: "track", concertId: 1, trackIndex: 0 },
  { type: "concert", concertId: 1 },
  { type: "playlist", childPlaylistId: 1 },
];
void _targets;

// ── isMember / itemIdFor ────────────────────────────────────────────────────

test("isMember / itemIdFor read the membership list", () => {
  const m = members(2, 5);
  assert.equal(C.isMember(m, 2), true);
  assert.equal(C.isMember(m, 3), false);
  assert.equal(C.itemIdFor(m, 5), 50);
  assert.equal(C.itemIdFor(m, 99), null);
});

// ── buildRows ───────────────────────────────────────────────────────────────

test("buildRows with no filter puts members on top, then non-members", () => {
  const rows = C.buildRows({
    playlists: pls([1, "Alpha"], [2, "Beta"], [3, "Gamma"]),
    members: members(2),
    filter: "",
  });
  assert.deepEqual(ids(rows), [2, 1, 3]);
  assert.deepEqual(kinds(rows), ["member", "nonmember", "nonmember"]);
});

test("buildRows with no filter and no playlists shows the empty-state row", () => {
  const rows = C.buildRows({ playlists: [], members: [], filter: "" });
  assert.deepEqual(ids(rows), ["new"]);
  assert.deepEqual(kinds(rows), ["empty"]);
});

test("buildRows when filtered: non-members, then create row, then members", () => {
  const rows = C.buildRows({
    playlists: pls([1, "Rocky"], [2, "Rockabilly"], [3, "Jazz"]),
    members: members(2),
    filter: "rock",
  });
  // "Jazz" filtered out; no exact match for "rock" so a create row appears.
  // Non-member "Rocky" first, create row, member "Rockabilly".
  assert.deepEqual(ids(rows), [1, "new", 2]);
  assert.deepEqual(kinds(rows), ["nonmember", "create", "member"]);
  // The create row carries the case-preserved query as its name.
  assert.equal(rows[1]!.name, "rock");
});

test("buildRows: a filter exactly matching a member shows no create row, member stays a member row", () => {
  const rows = C.buildRows({
    playlists: pls([1, "Rock"], [2, "Jazz"]),
    members: members(1), // Rock is a member
    filter: "Rock",
  });
  // Exact match to a member → no create row; "Jazz" filtered out.
  assert.deepEqual(kinds(rows), ["member"]);
  assert.equal(rows[0]!.id, 1);
});

test("buildRows suppresses the create row on an exact name match", () => {
  const rows = C.buildRows({
    playlists: pls([1, "Rock"]),
    members: [],
    filter: "Rock",
  });
  assert.deepEqual(kinds(rows), ["nonmember"]); // no create row
});

test("buildRows preserves the case of the create-row name", () => {
  const rows = C.buildRows({ playlists: pls([1, "Jazz"]), members: [], filter: "  New Mix  " });
  const create = rows.find((r) => r.kind === "create");
  assert.equal(create!.name, "New Mix");
});

// ── autoHighlight ───────────────────────────────────────────────────────────

test("autoHighlight Rule 1: exact non-member match is highlighted", () => {
  const rows = C.buildRows({ playlists: pls([1, "Rock"], [2, "Jazz"]), members: [], filter: "rock" });
  assert.equal(C.autoHighlight({ rows, filter: "rock", currentActive: null }), 1);
});

test("autoHighlight Rule 2: no non-members but a create row -> highlight create", () => {
  const rows = C.buildRows({ playlists: pls([1, "Rock"]), members: members(1), filter: "ro" });
  // "Rock" is a member, so no non-member rows; "ro" has no exact match -> create row.
  assert.equal(C.autoHighlight({ rows, filter: "ro", currentActive: null }), "new");
});

test("autoHighlight does not fire when a row is already active", () => {
  const rows = C.buildRows({ playlists: pls([1, "Rock"]), members: [], filter: "rock" });
  assert.equal(C.autoHighlight({ rows, filter: "rock", currentActive: 1 }), null);
});

test("autoHighlight does not fire with an empty filter", () => {
  const rows = C.buildRows({ playlists: pls([1, "Rock"]), members: [], filter: "" });
  assert.equal(C.autoHighlight({ rows, filter: "", currentActive: null }), null);
});

test("autoHighlight Rule 1 picks the exact non-member match, not just the first match", () => {
  // Both "Rock" and "Rockabilly" match "rock"; Rule 1 highlights the exact
  // match ("Rock", id 1), not the first row.
  const rows = C.buildRows({ playlists: pls([1, "Rock"], [2, "Rockabilly"]), members: [], filter: "Rock" });
  assert.equal(C.autoHighlight({ rows, filter: "Rock", currentActive: null }), 1);
});

test("autoHighlight does not highlight an exact match that is already a member", () => {
  const rows = C.buildRows({
    playlists: pls([1, "Rock"], [2, "Rockabilly"]),
    members: members(1),
    filter: "rock",
  });
  // Exact match "Rock" is a member (no Rule 1); "Rockabilly" is a non-member so
  // Rule 2 doesn't fire either. No auto-highlight.
  assert.equal(C.autoHighlight({ rows, filter: "rock", currentActive: null }), null);
});

// ── nextRow / prevRow ───────────────────────────────────────────────────────

test("nextRow: from null selects the first row, then advances, then clamps", () => {
  const rows = C.buildRows({ playlists: pls([1, "A"], [2, "B"], [3, "C"]), members: [], filter: "" });
  assert.equal(C.nextRow(rows, null), 1);
  assert.equal(C.nextRow(rows, 1), 2);
  assert.equal(C.nextRow(rows, 3), 3); // clamp at bottom
});

test("nextRow with no rows leaves the highlight unchanged", () => {
  assert.equal(C.nextRow([], null), null);
});

test("nextRow with an active id no longer in the list leaves it unchanged", () => {
  const rows = C.buildRows({ playlists: pls([1, "A"], [2, "B"]), members: [], filter: "" });
  assert.equal(C.nextRow(rows, 999), 999);
});

test("prevRow with an active id no longer in the list returns null", () => {
  const rows = C.buildRows({ playlists: pls([1, "A"], [2, "B"]), members: [], filter: "" });
  assert.equal(C.prevRow(rows, 999), null);
});

test("prevRow: moving up past the first row returns null (back to filter)", () => {
  const rows = C.buildRows({ playlists: pls([1, "A"], [2, "B"], [3, "C"]), members: [], filter: "" });
  assert.equal(C.prevRow(rows, 2), 1);
  assert.equal(C.prevRow(rows, 1), null); // up past top
  assert.equal(C.prevRow(rows, null), null);
});
