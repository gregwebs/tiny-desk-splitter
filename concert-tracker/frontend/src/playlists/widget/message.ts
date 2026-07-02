import { Schema as S } from "effect";
import { m } from "foldkit/message";

import { AddTarget, Member, PlaylistRef, RowId } from "./model";

// MESSAGE

/** Host opened the panel for a new target (inbound `opened` Port). */
export const OpenRequested = m("OpenRequested", { target: AddTarget });
/** Host closed the sidebar out from under us (inbound `closed` Port). */
export const CloseRequested = m("CloseRequested");
/** Host resolved the empty-state `prompt()` with a name (inbound `newName`). */
export const EnteredNewName = m("EnteredNewName", { name: S.String });

export const CompletedLoad = m("CompletedLoad", {
  forTarget: AddTarget,
  playlists: S.Array(PlaylistRef),
  members: S.Array(Member),
});
export const FailedLoad = m("FailedLoad", { forTarget: AddTarget });

export const ChangedFilter = m("ChangedFilter", { value: S.String });
export const PressedArrowDown = m("PressedArrowDown");
export const PressedArrowUp = m("PressedArrowUp");
export const PressedEnter = m("PressedEnter");

/** A list row was clicked (or activated via Enter/Space on the row itself).
 *  Carries only which row; `update` decides add / remove / create / prompt from
 *  the row's *current* kind. Encoding the action in the Message (rather than in
 *  per-kind handlers) keeps it correct even if the vdom reuses a row element
 *  across a kind change (member↔nonmember, empty↔create), which would otherwise
 *  leave a stale handler. Member rows are a no-op on row click — the trash
 *  button (ClickedRemove) removes. */
export const ClickedRow = m("ClickedRow", { id: RowId });
export const ClickedRemove = m("ClickedRemove", { playlistId: S.Number });
/** The close "×" the widget renders. Closes locally and asks the host to tear
 *  down the sidebar chrome (outbound `requestClose`). */
export const ClickedClose = m("ClickedClose");

/** A mutation (add/remove/create-and-add) finished. Carries the refreshed
 *  playlist list *and* membership so a just-created playlist renders without a
 *  separate reload. Ignored unless `forTarget` is still the current target. */
export const CompletedMutation = m("CompletedMutation", {
  forTarget: AddTarget,
  playlists: S.Array(PlaylistRef),
  members: S.Array(Member),
});
export const FailedMutation = m("FailedMutation", {
  forTarget: AddTarget,
  errorMessage: S.String,
});

/** Command acks (no information; `update` ignores them). */
export const CompletedScrollActiveIntoView = m("CompletedScrollActiveIntoView");
export const CompletedRequestClose = m("CompletedRequestClose");
export const CompletedRequestNewName = m("CompletedRequestNewName");
export const CompletedFocusFilter = m("CompletedFocusFilter");

export const Message = S.Union([
  OpenRequested,
  CloseRequested,
  EnteredNewName,
  CompletedLoad,
  FailedLoad,
  ChangedFilter,
  PressedArrowDown,
  PressedArrowUp,
  PressedEnter,
  ClickedRow,
  ClickedRemove,
  ClickedClose,
  CompletedMutation,
  FailedMutation,
  CompletedScrollActiveIntoView,
  CompletedRequestClose,
  CompletedRequestNewName,
  CompletedFocusFilter,
]);
export type Message = typeof Message.Type;
