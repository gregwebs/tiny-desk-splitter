import { Array, Match as M, Option } from "effect";
import type { Command } from "foldkit/command";
import { evo } from "foldkit/struct";

import {
  applyHandle,
  buildPayload,
  detach,
  handlesFor,
  link,
  parseTimecode,
  setEnd,
  setStart,
  validate,
} from "../core";
import { EmitAuditionAt, ResetSplit, ResyncAfterConflict, RevertEdits, SubmitSplit } from "./command";
import type { Message } from "./message";
import { DragStateValue, type EditorState, type Model, PhaseValue, StatusValue } from "./model";

type UpdateReturn = readonly [Model, ReadonlyArray<Command<Message>>];
const withUpdateReturn = M.withReturnType<UpdateReturn>();

/** Runs `f` against the current editor when `phase` is `Ready`; a no-op
 *  otherwise. The interactive elements that dispatch editor-mutating
 *  Messages only render in `Ready`, so the `orElse` arm should never
 *  actually run — it exists so a stale or out-of-order dispatch can't crash
 *  the app. */
const withEditor = (model: Model, f: (editor: EditorState) => UpdateReturn): UpdateReturn =>
  M.value(model.phase).pipe(
    withUpdateReturn,
    M.tag("Ready", (ready) => f(ready.editor)),
    M.orElse(() => [model, []]),
  );

/** Replaces the current `Ready` editor with `nextEditor`, leaving
 *  `mediaUrl`/`playable` untouched. No-op outside `Ready`. */
const withNextEditor = (model: Model, nextEditor: EditorState): Model =>
  M.value(model.phase).pipe(
    M.withReturnType<Model>(),
    M.tag("Ready", (ready) =>
      evo(model, {
        phase: () =>
          PhaseValue.Ready({
            editor: nextEditor,
            mediaUrl: ready.mediaUrl,
            playable: ready.playable,
          }),
      }),
    ),
    M.orElse(() => model),
  );

/** Mutates a clone of the current editor with one of `core.ts`'s in-place
 *  editor functions (`setStart`, `setEnd`, `applyHandle`, `detach`, `link`)
 *  and stores the clone. Never call those functions on `model`'s own editor
 *  directly — they mutate and return the same object. */
const withClonedEditor = (model: Model, f: (editor: EditorState) => void): UpdateReturn =>
  withEditor(model, (editor) => {
    const next = structuredClone(editor);
    f(next);
    return [withNextEditor(model, next), []];
  });

const isDragging = (model: Model): boolean => model.dragState._tag === "Dragging";

/** Shared by `CompletedSubmitSplit` and `CompletedResetSplit`: both POST to
 *  a split-job endpoint and see the same response shape. Mirrors the old
 *  `postJob` helper's status-code branching: 202 queued, 200 reset no-op,
 *  409 a job is already running (re-fetch to resync), anything else is an
 *  error to surface verbatim. */
const handleSplitJobResult = (model: Model, status: number, body: string): UpdateReturn => {
  if (status === 202) {
    return [
      evo(model, {
        busy: () => false,
        status: () =>
          StatusValue.StatusOk({
            message: "Splitting… the track list will update when it finishes.",
          }),
      }),
      [],
    ];
  }
  if (status === 200) {
    return [
      evo(model, {
        busy: () => false,
        status: () => StatusValue.StatusOk({ message: "Already using the automatic split." }),
      }),
      [],
    ];
  }
  const failed = evo(model, {
    busy: () => false,
    status: () => StatusValue.StatusError({ message: body || `Request failed (${status})` }),
  });
  if (status === 409) {
    return [failed, [ResyncAfterConflict({ concertId: model.concertId })]];
  }
  return [failed, []];
};

export const update = (model: Model, message: Message): UpdateReturn =>
  M.value(message).pipe(
    withUpdateReturn,
    M.tagsExhaustive({
      SucceededFetchSplitterData: ({ maybeEditor, maybeMediaUrl, playable }) => [
        evo(model, {
          phase: () =>
            Option.match(maybeEditor, {
              onNone: () => PhaseValue.Empty(),
              onSome: (editor) =>
                PhaseValue.Ready({ editor, mediaUrl: maybeMediaUrl, playable }),
            }),
        }),
        [],
      ],

      FailedFetchSplitterData: () => [evo(model, { phase: () => PhaseValue.LoadFailed() }), []],

      PressedHandle: ({ handleIndex }) => [
        evo(model, { dragState: () => DragStateValue.Dragging({ handleIndex }) }),
        [],
      ],

      MovedDragPointer: ({ time }) =>
        M.value(model.dragState).pipe(
          withUpdateReturn,
          M.tag("Dragging", ({ handleIndex }) =>
            withClonedEditor(model, (editor) => {
              Option.match(Array.get(handlesFor(editor), handleIndex), {
                onNone: () => undefined,
                onSome: (handle) => applyHandle(editor, handle, time),
              });
            }),
          ),
          M.orElse(() => [model, []]),
        ),

      ReleasedDragPointer: () => [
        evo(model, { dragState: () => DragStateValue.NotDragging() }),
        [],
      ],

      ChangedTimeInput: ({ trackIndex, kind, rawValue }) => {
        const value = parseTimecode(rawValue);
        if (!Number.isFinite(value)) {
          return [
            evo(model, {
              status: () => StatusValue.StatusError({ message: "Enter a time like 2:05.0" }),
            }),
            [],
          ];
        }
        const clearedModel =
          model.status._tag === "StatusError" ? evo(model, { status: () => StatusValue.NoStatus() }) : model;
        return withClonedEditor(clearedModel, (editor) => {
          if (kind === "Start") {
            setStart(editor, trackIndex, value);
          } else {
            setEnd(editor, trackIndex, value);
          }
        });
      },

      ToggledBoundary: ({ boundaryIndex }) =>
        withClonedEditor(model, (editor) => {
          const isLinked = Option.getOrElse(Array.get(editor.linked, boundaryIndex), () => false);
          if (isLinked) {
            detach(editor, boundaryIndex);
          } else {
            link(editor, boundaryIndex);
          }
        }),

      ClickedAudition: ({ time }) =>
        isDragging(model) ? [model, []] : [model, [EmitAuditionAt({ time })]],

      ClickedSubmitSplit: () =>
        withEditor(model, (editor) => {
          const firstError = Array.get(validate(editor), 0);
          return Option.match(firstError, {
            onSome: (message) => [
              evo(model, { status: () => StatusValue.StatusError({ message }) }),
              [],
            ],
            onNone: () => [
              evo(model, { busy: () => true, status: () => StatusValue.NoStatus() }),
              [SubmitSplit({ concertId: model.concertId, payload: buildPayload(editor) })],
            ],
          });
        }),

      ClickedResetToAuto: () => [
        evo(model, { busy: () => true, status: () => StatusValue.NoStatus() }),
        [ResetSplit({ concertId: model.concertId })],
      ],

      ClickedRevertEdits: () => [
        evo(model, {
          busy: () => true,
          status: () => StatusValue.StatusOk({ message: "Discarding edits…" }),
        }),
        [RevertEdits({ concertId: model.concertId })],
      ],

      CompletedSubmitSplit: ({ status, body }) => handleSplitJobResult(model, status, body),
      CompletedResetSplit: ({ status, body }) => handleSplitJobResult(model, status, body),

      CompletedRevertEdits: ({ outcome }) =>
        M.value(outcome).pipe(
          withUpdateReturn,
          M.tag("RestoredEditor", ({ editor }) => [
            evo(withNextEditor(model, editor), {
              busy: () => false,
              status: () => StatusValue.StatusOk({ message: "Restored the last saved times." }),
            }),
            [],
          ]),
          M.tag("NoSavedEditor", () => [
            evo(model, {
              busy: () => false,
              status: () => StatusValue.StatusError({ message: "No saved times to restore." }),
            }),
            [],
          ]),
          M.tag("RevertFetchFailed", () => [
            evo(model, {
              busy: () => false,
              status: () =>
                StatusValue.StatusError({ message: "Could not load saved times — please retry." }),
            }),
            [],
          ]),
          M.exhaustive,
        ),

      CompletedResync: ({ maybeEditor }) =>
        Option.match(maybeEditor, {
          onNone: () => [model, []],
          onSome: (editor) => [withNextEditor(model, editor), []],
        }),

      ChangedPlayhead: ({ fraction }) => [evo(model, { playheadFraction: () => fraction }), []],

      CompletedEmitAuditionAt: () => [model, []],
    }),
  );
