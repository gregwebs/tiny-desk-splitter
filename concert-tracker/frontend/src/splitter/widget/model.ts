import { Schema as S } from "effect";
import { ts } from "foldkit/schema";

// MODEL

/** Mirrors `../core.ts`'s `EditorTrack`/`EditorState` interfaces exactly, so a
 *  `structuredClone` of this Schema's `.Type` is a valid `core.EditorState`
 *  for the mutating editor functions (`setStart`, `setEnd`, `applyHandle`,
 *  `detach`, `link`). See `update.ts`'s `withClonedEditor` for the
 *  clone-then-mutate boundary that keeps those calls out of the Model. */
export const EditorTrack = S.Struct({
  title: S.String,
  start: S.Number,
  end: S.Number,
});
export type EditorTrack = typeof EditorTrack.Type;

export const EditorState = S.Struct({
  duration: S.Number,
  // S.mutable strips the ReadonlyArray Effect Schema infers by default —
  // core.ts's editor functions mutate these arrays in place (see
  // `update.ts`'s clone-then-mutate boundary), so the Model's inferred
  // `.Type` needs to match `core.EditorState`'s plain mutable arrays
  // exactly, not just structurally up to readonly-ness.
  tracks: S.mutable(S.Array(EditorTrack)),
  linked: S.mutable(S.Array(S.Boolean)),
});
export type EditorState = typeof EditorState.Type;

const NoStatus = ts("NoStatus");
const StatusOk = ts("StatusOk", { message: S.String });
const StatusError = ts("StatusError", { message: S.String });

export const Status = S.Union([NoStatus, StatusOk, StatusError]);
export type Status = typeof Status.Type;

export const StatusValue = { NoStatus, StatusOk, StatusError };

const NotDragging = ts("NotDragging");
/** `handleIndex` indexes into `core.handlesFor(editor)`'s result for the
 *  current `editor`. Topology (and so the handle list) cannot change while
 *  dragging — that requires a boundary-toggle click, a separate gesture. */
const Dragging = ts("Dragging", { handleIndex: S.Number });

export const DragState = S.Union([NotDragging, Dragging]);
export type DragState = typeof DragState.Type;

export const DragStateValue = { NotDragging, Dragging };

/** The editor is open and showing data loaded from the server: the steady
 *  state for almost all interaction. `mediaUrl` is absent when the source
 *  file isn't found. */
const Ready = ts("Ready", {
  editor: EditorState,
  mediaUrl: S.Option(S.String),
  playable: S.Boolean,
});

const Loading = ts("Loading");
/** No split points exist yet (auto-split hasn't run). */
const Empty = ts("Empty");
const LoadFailed = ts("LoadFailed");

/** What's known about the server-side split-timestamps data. `busy`,
 *  `status`, `dragState`, and `playheadFraction` live as sibling Model
 *  fields rather than inside `Ready`: they're meaningful only once `Ready`,
 *  but keeping them top-level lets every routine update patch them with
 *  `evo` directly instead of reconstructing a multi-field tagged variant. */
export const Phase = S.Union([Loading, Empty, LoadFailed, Ready]);
export type Phase = typeof Phase.Type;

export const PhaseValue = { Loading, Empty, LoadFailed, Ready };

export const Model = S.Struct({
  concertId: S.Number,
  phase: Phase,
  busy: S.Boolean,
  status: Status,
  dragState: DragState,
  playheadFraction: S.Option(S.Number),
});
export type Model = typeof Model.Type;

export const Flags = S.Struct({ concertId: S.Number });
export type Flags = typeof Flags.Type;
