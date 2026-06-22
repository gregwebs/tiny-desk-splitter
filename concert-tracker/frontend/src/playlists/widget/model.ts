import { Schema as S } from "effect";
import { ts } from "foldkit/schema";

import type { AddTarget as SharedAddTarget } from "../../shared/playlists-api";

// MODEL

/** The add-panel's target as carried across the host Port. Hand-rolled
 *  `S.Struct` + `S.Literal` (discriminant `type`, NOT `_tag`) so it decodes the
 *  plain objects player.ts and the templates already construct. Kept
 *  structurally identical to the Effect-free `AddTarget` in
 *  ../../shared/playlists-api.ts (see the assignability guard at the bottom of
 *  this file) — player.ts imports that one and must stay out of the Effect
 *  bundle. */
const TrackTarget = S.Struct({
  type: S.Literal("track"),
  concertId: S.Number,
  trackIndex: S.Number,
  label: S.optionalKey(S.NullOr(S.String)),
});
const ConcertTarget = S.Struct({
  type: S.Literal("concert"),
  concertId: S.Number,
  label: S.optionalKey(S.NullOr(S.String)),
});
const PlaylistTarget = S.Struct({
  type: S.Literal("playlist"),
  childPlaylistId: S.Number,
  label: S.optionalKey(S.NullOr(S.String)),
});
export const AddTarget = S.Union([TrackTarget, ConcertTarget, PlaylistTarget]);
export type AddTarget = typeof AddTarget.Type;

/** Mirrors `../core.ts`'s `PlaylistRef`/`Member` interfaces (readonly here; the
 *  pure core only reads them). */
export const PlaylistRef = S.Struct({ id: S.Number, name: S.String });
export const Member = S.Struct({ playlistId: S.Number, itemId: S.Number });

/** Identifies a highlighted/navigated row. `"new"` is the single create row. */
export const RowId = S.Union([S.Number, S.Literal("new")]);
export type RowId = typeof RowId.Type;

/** The panel is idle (sidebar showing the queue, or closed). */
const Closed = ts("Closed");
/** Fetching playlists + membership for `target` (the host just opened us). */
const Loading = ts("Loading", { target: AddTarget });
const LoadFailed = ts("LoadFailed", { target: AddTarget });
/** The steady interactive state. `filter`/`activeId`/`activeFromTyping` live
 *  inside `Loaded` because they're meaningless before data has loaded; the row
 *  list and highlight are derived from them by `../core.ts` in the view. */
const Loaded = ts("Loaded", {
  target: AddTarget,
  playlists: S.Array(PlaylistRef),
  members: S.Array(Member),
  filter: S.String,
  activeId: S.Option(RowId),
  activeFromTyping: S.Boolean,
});

export const Phase = S.Union([Closed, Loading, LoadFailed, Loaded]);
export type Phase = typeof Phase.Type;

export const PhaseValue = { Closed, Loading, LoadFailed, Loaded };

export const Model = S.Struct({
  phase: Phase,
  /** Last error to surface in `#add-pl-error`; cleared at the start of every
   *  user action and on a successful load/mutation. */
  error: S.Option(S.String),
});
export type Model = typeof Model.Type;

/** The widget mounts with no flags — the first `OpenRequested` Port message
 *  carries the target and triggers the load. */
export const Flags = S.Struct({});
export type Flags = typeof Flags.Type;

// Compile-time assignability guard (both directions): the Port's `AddTarget`
// schema and the hand-written shared `AddTarget` must not drift. Strict type
// equality is infeasible — schema struct fields are readonly while the shared
// type's are mutable — so we assert mutual assignability; a field added on
// either side breaks one direction. Types only, no runtime cost.
type AddTargetType = typeof AddTarget.Type;
type AssertAssignable<A, _B extends A> = true;
export type _SharedToSchema = AssertAssignable<AddTargetType, SharedAddTarget>;
export type _SchemaToShared = AssertAssignable<SharedAddTarget, AddTargetType>;
