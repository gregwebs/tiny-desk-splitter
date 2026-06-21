import { Schema as S } from "effect";
import { m } from "foldkit/message";
import { ts } from "foldkit/schema";

import { EditorState } from "./model";

// MESSAGE

export const SucceededFetchSplitterData = m("SucceededFetchSplitterData", {
  maybeEditor: S.Option(EditorState),
  maybeMediaUrl: S.Option(S.String),
  playable: S.Boolean,
});
export const FailedFetchSplitterData = m("FailedFetchSplitterData");

export const PressedHandle = m("PressedHandle", { handleIndex: S.Number });
export const MovedDragPointer = m("MovedDragPointer", { time: S.Number });
export const ReleasedDragPointer = m("ReleasedDragPointer");

export const ChangedTimeInput = m("ChangedTimeInput", {
  trackIndex: S.Number,
  kind: S.Literals(["Start", "End"]),
  rawValue: S.String,
});
export const ToggledBoundary = m("ToggledBoundary", { boundaryIndex: S.Number });

export const ClickedSubmitSplit = m("ClickedSubmitSplit");
export const ClickedRevertEdits = m("ClickedRevertEdits");
export const ClickedResetToAuto = m("ClickedResetToAuto");
export const ClickedAudition = m("ClickedAudition", { time: S.Number });
/** Ack for the `EmitAuditionAt` Command. Carries no information; `update`
 *  ignores it. Exists because every Command needs a declared result Message. */
export const CompletedEmitAuditionAt = m("CompletedEmitAuditionAt");

export const CompletedSubmitSplit = m("CompletedSubmitSplit", {
  status: S.Number,
  body: S.String,
});
export const CompletedResetSplit = m("CompletedResetSplit", {
  status: S.Number,
  body: S.String,
});

/** Outcome of re-fetching saved split-timestamps for "Discard my edits". A
 *  discriminated union instead of `Option<EditorState> | error` because the
 *  three outcomes drive three different status messages (see `update.ts`). */
const RestoredEditor = ts("RestoredEditor", { editor: EditorState });
const NoSavedEditor = ts("NoSavedEditor");
const RevertFetchFailed = ts("RevertFetchFailed");
export const RevertOutcome = S.Union([
  RestoredEditor,
  NoSavedEditor,
  RevertFetchFailed,
]);
export type RevertOutcome = typeof RevertOutcome.Type;
export const RevertOutcomeValue = {
  RestoredEditor,
  NoSavedEditor,
  RevertFetchFailed,
};

export const CompletedRevertEdits = m("CompletedRevertEdits", {
  outcome: RevertOutcome,
});

/** Result of the silent re-fetch after a 409 (a split job is already
 *  running). `None` means nothing changed or the fetch failed; both are
 *  treated identically (keep showing the current view), matching the old
 *  `resync()`'s empty catch block. */
export const CompletedResync = m("CompletedResync", {
  maybeEditor: S.Option(EditorState),
});

/** Host-pushed playhead position via the inbound `playhead` Port. `None`
 *  means hidden (paused, a different track playing, or no global player). */
export const ChangedPlayhead = m("ChangedPlayhead", {
  fraction: S.Option(S.Number),
});

export const Message = S.Union([
  SucceededFetchSplitterData,
  FailedFetchSplitterData,
  PressedHandle,
  MovedDragPointer,
  ReleasedDragPointer,
  ChangedTimeInput,
  ToggledBoundary,
  ClickedSubmitSplit,
  ClickedRevertEdits,
  ClickedResetToAuto,
  ClickedAudition,
  CompletedEmitAuditionAt,
  CompletedSubmitSplit,
  CompletedResetSplit,
  CompletedRevertEdits,
  CompletedResync,
  ChangedPlayhead,
]);
export type Message = typeof Message.Type;
