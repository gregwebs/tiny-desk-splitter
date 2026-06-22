// Pure, DOM-free logic for the add-to-playlist sidebar panel. The Foldkit
// widget (./widget) renders a list of playlist rows the user can add the
// current target (a track, a concert, or another playlist) to or remove it
// from, with a filter box, keyboard navigation, and a "create new playlist"
// affordance.
//
// This module owns the rules that were previously tangled into the imperative
// renderAddList / filterKeydown in the old playlists.ts: row ordering, the
// create/empty-state row, the typing auto-highlight, and arrow-key movement.
// No DOM access here — see ./widget for the MVU layer and ./index.ts for the
// host glue. Unit-tested directly by js-tests/playlists-core.test.ts.
import type { AddItemReq } from "../api/client";
import type { AddTarget } from "../shared/playlists-api";

/** A playlist as shown in the add panel. */
export interface PlaylistRef {
  id: number;
  name: string;
}

/** Membership of the current target: which playlists already contain it, and
 *  the playlist-item id needed to remove it again. */
export interface Member {
  playlistId: number;
  itemId: number;
}

/** Identifies a row for highlight/navigation. `"new"` is the create row (there
 *  is only ever one); every other row is keyed by its playlist id. */
export type RowId = number | "new";

export type RowKind = "member" | "nonmember" | "create" | "empty";

/** A single rendered row. For `member`/`nonmember`, `name` is the playlist
 *  name; for `create`, `name` is the (case-preserved) text to create; for
 *  `empty`, `name` is unused (the view shows a fixed "Create a new playlist"). */
export interface Row {
  id: RowId;
  kind: RowKind;
  name: string;
}

export function isMember(members: readonly Member[], playlistId: number): boolean {
  return members.some((m) => m.playlistId === playlistId);
}

export function itemIdFor(members: readonly Member[], playlistId: number): number | null {
  const m = members.find((mm) => mm.playlistId === playlistId);
  return m ? m.itemId : null;
}

export function targetLabel(target: AddTarget): string {
  const n = target.label || "";
  if (target.type === "track") return n ? `Adding “${n}” to…` : "Adding track to…";
  if (target.type === "concert") return n ? `Adding “${n}” to…` : "Adding concert to…";
  return n ? `Nesting “${n}” into…` : "Nesting playlist into…";
}

export function addItemBody(target: AddTarget): AddItemReq {
  if (target.type === "track") {
    return { type: "track", concert_id: target.concertId, track_index: target.trackIndex };
  }
  if (target.type === "concert") {
    return { type: "concert", concert_id: target.concertId };
  }
  return { type: "playlist", child_playlist_id: target.childPlaylistId };
}

/**
 * Build the ordered list of rows for a given filter. Row order *is* the
 * arrow-key navigation order, so the two can never disagree (the old code kept
 * a parallel `actionableRows` array in sync by hand).
 *
 * Ordering mirrors the original renderAddList:
 *   - No filter: members on top (so already-added playlists stay visible even
 *     with a long list), then non-members.
 *   - Filtered: non-members first (the likely add targets), then the create
 *     row, then members sink to the bottom.
 * A "Create '<query>'" row appears when the filter term has no exact-name
 * match; a single "Create a new playlist" empty-state row appears when there
 * are no playlists at all.
 */
export function buildRows(input: {
  playlists: readonly PlaylistRef[];
  members: readonly Member[];
  filter: string;
}): Row[] {
  const { playlists, members, filter } = input;
  const q = filter.trim().toLowerCase();
  const qRaw = filter.trim();

  const filtered = playlists.filter((p) => q === "" || p.name.toLowerCase().indexOf(q) !== -1);

  const memberRows: Row[] = [];
  const nonMemberRows: Row[] = [];
  for (const p of filtered) {
    if (isMember(members, p.id)) {
      memberRows.push({ id: p.id, kind: "member", name: p.name });
    } else {
      nonMemberRows.push({ id: p.id, kind: "nonmember", name: p.name });
    }
  }

  const exactNameExists = playlists.some((p) => p.name.toLowerCase() === q);
  let createRow: Row | null = null;
  if (q !== "" && !exactNameExists) {
    createRow = { id: "new", kind: "create", name: qRaw };
  } else if (filtered.length === 0) {
    createRow = { id: "new", kind: "empty", name: "" };
  }

  const createRows = createRow ? [createRow] : [];
  return q === ""
    ? [...memberRows, ...nonMemberRows, ...createRows]
    : [...nonMemberRows, ...createRows, ...memberRows];
}

/**
 * The row to auto-highlight while the user is typing, or `null` for none.
 * Only fires when there is a filter term and nothing is already highlighted
 * (so arrow-key navigation is never overridden):
 *   Rule 1 — an exact-name match to a *non-member* row: highlight it (an exact
 *            match to a member is a no-op, it's already added).
 *   Rule 2 — no non-member rows but a create row is present (a unique new name,
 *            or every match is already a member): highlight the create row.
 * The caller marks any non-null result as typing-originated (so Enter clears
 * the filter after acting).
 */
export function autoHighlight(input: {
  rows: readonly Row[];
  filter: string;
  currentActive: RowId | null;
}): RowId | null {
  const { rows, filter, currentActive } = input;
  const q = filter.trim().toLowerCase();
  if (q === "" || currentActive !== null) return null;

  const nonMembers = rows.filter((r) => r.kind === "nonmember");
  const exact = nonMembers.find((r) => r.name.toLowerCase() === q);
  if (exact) return exact.id;

  const hasCreate = rows.some((r) => r.kind === "create");
  if (nonMembers.length === 0 && hasCreate) return "new";
  return null;
}

/** Next row down (ArrowDown). From no highlight, selects the first row; clamps
 *  at the bottom. Returns the unchanged `active` when there are no rows. */
export function nextRow(rows: readonly Row[], active: RowId | null): RowId | null {
  const first = rows[0];
  if (first === undefined) return active;
  if (active === null) return first.id;
  const idx = rows.findIndex((r) => r.id === active);
  if (idx === -1) return active;
  const below = rows[idx + 1];
  return below ? below.id : active; // clamp at bottom
}

/** Previous row up (ArrowUp). Moving up past the first row returns `null`,
 *  handing focus back to the filter input only. */
export function prevRow(rows: readonly Row[], active: RowId | null): RowId | null {
  if (active === null) return null;
  const idx = rows.findIndex((r) => r.id === active);
  if (idx <= 0) return null;
  const above = rows[idx - 1];
  return above ? above.id : null;
}
