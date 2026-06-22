import { Option } from "effect";
import { Story } from "foldkit";
import { describe, expect, test } from "vitest";

import {
  EmitAuditionAt,
  ResetSplit,
  ResyncAfterConflict,
  RevertEdits,
  SubmitSplit,
} from "./command";
import {
  ChangedPlayhead,
  ChangedTimeInput,
  ClickedAudition,
  ClickedResetToAuto,
  ClickedRevertEdits,
  ClickedSubmitSplit,
  CompletedEmitAuditionAt,
  CompletedResetSplit,
  CompletedResync,
  CompletedRevertEdits,
  CompletedSubmitSplit,
  FailedFetchSplitterData,
  PressedHandle,
  MovedDragPointer,
  ReleasedDragPointer,
  RevertOutcomeValue,
  SucceededFetchSplitterData,
  ToggledBoundary,
} from "./message";
import {
  DragStateValue,
  PhaseValue,
  StatusValue,
  type EditorState,
  type Model,
} from "./model";
import { update } from "./update";

// Foldkit Story tests for the splitter `update` (foldkit's own MVU harness):
// feed a model + a sequence of Messages, assert on the resulting Model and the
// Commands it emits. `Story.story` throws if any emitted Command is left
// unresolved, so each command is either resolved or asserted absent.
// Complements js-tests/splitter.test.ts (pure core.ts logic) and the
// Playwright e2e suite (real browser, e2e/splitter.spec.js).

const editor = (over?: Partial<EditorState>): EditorState => ({
  duration: 100,
  tracks: [
    { title: "One", start: 0, end: 40 },
    { title: "Two", start: 40, end: 100 },
  ],
  linked: [true],
  ...over,
});

const loadingModel: Model = {
  concertId: 1,
  phase: PhaseValue.Loading(),
  busy: false,
  status: StatusValue.NoStatus(),
  dragState: DragStateValue.NotDragging(),
  playheadFraction: Option.none(),
};

const ready = (over?: {
  editorState?: EditorState;
  busy?: boolean;
  status?: Model["status"];
  dragState?: Model["dragState"];
  playhead?: Option.Option<number>;
  playable?: boolean;
  mediaUrl?: Option.Option<string>;
}): Model => ({
  concertId: 1,
  phase: PhaseValue.Ready({
    editor: over?.editorState ?? editor(),
    mediaUrl: over?.mediaUrl ?? Option.none(),
    playable: over?.playable ?? true,
  }),
  busy: over?.busy ?? false,
  status: over?.status ?? StatusValue.NoStatus(),
  dragState: over?.dragState ?? DragStateValue.NotDragging(),
  playheadFraction: over?.playhead ?? Option.none(),
});

/** Narrow `model.phase` to the `Ready` variant for assertions. */
const asReady = (model: Model) => {
  if (model.phase._tag !== "Ready") throw new Error(`expected Ready, got ${model.phase._tag}`);
  return model.phase;
};

describe("splitter update", () => {
  test("SucceededFetchSplitterData with an editor enters Ready", () => {
    Story.story(
      update,
      Story.with(loadingModel),
      Story.message(
        SucceededFetchSplitterData({
          maybeEditor: Option.some(editor()),
          maybeMediaUrl: Option.some("https://example.com/a.mp3"),
          playable: true,
        }),
      ),
      Story.model((m) => {
        const r = asReady(m);
        expect(r.editor.tracks.length).toBe(2);
        expect(r.playable).toBe(true);
      }),
      Story.Command.expectNone(),
    );
  });

  test("SucceededFetchSplitterData with no editor enters Empty", () => {
    Story.story(
      update,
      Story.with(loadingModel),
      Story.message(
        SucceededFetchSplitterData({
          maybeEditor: Option.none(),
          maybeMediaUrl: Option.none(),
          playable: false,
        }),
      ),
      Story.model((m) => expect(m.phase._tag).toBe("Empty")),
      Story.Command.expectNone(),
    );
  });

  test("FailedFetchSplitterData enters LoadFailed", () => {
    Story.story(
      update,
      Story.with(loadingModel),
      Story.message(FailedFetchSplitterData()),
      Story.model((m) => expect(m.phase._tag).toBe("LoadFailed")),
      Story.Command.expectNone(),
    );
  });

  test("PressedHandle enters Dragging for the pressed handle", () => {
    Story.story(
      update,
      Story.with(ready()),
      Story.message(PressedHandle({ handleIndex: 1 })),
      Story.model((m) => expect(m.dragState).toEqual(DragStateValue.Dragging({ handleIndex: 1 }))),
      Story.Command.expectNone(),
    );
  });

  test("MovedDragPointer while Dragging moves the linked boundary it targets", () => {
    Story.story(
      update,
      Story.with(ready({ dragState: DragStateValue.Dragging({ handleIndex: 1 }) })),
      Story.message(MovedDragPointer({ time: 50 })),
      Story.model((m) => {
        const r = asReady(m);
        // handleIndex 1 is the linked boundary between track 0 and track 1.
        expect(r.editor.tracks[0]?.end).toBe(50);
        expect(r.editor.tracks[1]?.start).toBe(50);
      }),
      Story.Command.expectNone(),
    );
  });

  test("MovedDragPointer while NotDragging is a no-op", () => {
    Story.story(
      update,
      Story.with(ready()),
      Story.message(MovedDragPointer({ time: 50 })),
      Story.model((m) => expect(asReady(m).editor.tracks[0]?.end).toBe(40)),
      Story.Command.expectNone(),
    );
  });

  test("ReleasedDragPointer resets dragState to NotDragging", () => {
    Story.story(
      update,
      Story.with(ready({ dragState: DragStateValue.Dragging({ handleIndex: 0 }) })),
      Story.message(ReleasedDragPointer()),
      Story.model((m) => expect(m.dragState).toEqual(DragStateValue.NotDragging())),
      Story.Command.expectNone(),
    );
  });

  test("ChangedTimeInput with a valid timecode updates the editor (clone-then-mutate)", () => {
    Story.story(
      update,
      Story.with(ready()),
      Story.message(ChangedTimeInput({ trackIndex: 0, kind: "Start", rawValue: "0:10" })),
      Story.model((m) => expect(asReady(m).editor.tracks[0]?.start).toBe(10)),
      Story.Command.expectNone(),
    );
  });

  test("ChangedTimeInput with an unparseable value surfaces a status error and leaves the editor untouched", () => {
    Story.story(
      update,
      Story.with(ready()),
      Story.message(ChangedTimeInput({ trackIndex: 0, kind: "Start", rawValue: "abc" })),
      Story.model((m) => {
        expect(m.status).toEqual(StatusValue.StatusError({ message: "Enter a time like 2:05.0" }));
        expect(asReady(m).editor.tracks[0]?.start).toBe(0);
      }),
      Story.Command.expectNone(),
    );
  });

  test("ToggledBoundary detaches a linked boundary", () => {
    Story.story(
      update,
      Story.with(ready()),
      Story.message(ToggledBoundary({ boundaryIndex: 0 })),
      Story.model((m) => expect(asReady(m).editor.linked[0]).toBe(false)),
      Story.Command.expectNone(),
    );
  });

  test("ToggledBoundary re-links a detached boundary, collapsing the gap", () => {
    const detached = editor({
      tracks: [
        { title: "One", start: 0, end: 40 },
        { title: "Two", start: 45, end: 100 },
      ],
      linked: [false],
    });
    Story.story(
      update,
      Story.with(ready({ editorState: detached })),
      Story.message(ToggledBoundary({ boundaryIndex: 0 })),
      Story.model((m) => {
        const r = asReady(m);
        expect(r.editor.linked[0]).toBe(true);
        expect(r.editor.tracks[1]?.start).toBe(40);
      }),
      Story.Command.expectNone(),
    );
  });

  test("ClickedAudition emits EmitAuditionAt", () => {
    Story.story(
      update,
      Story.with(ready()),
      Story.message(ClickedAudition({ time: 12 })),
      Story.Command.expectHas(EmitAuditionAt),
      Story.Command.resolve(EmitAuditionAt, CompletedEmitAuditionAt()),
    );
  });

  test("ClickedAudition while dragging is a no-op", () => {
    Story.story(
      update,
      Story.with(ready({ dragState: DragStateValue.Dragging({ handleIndex: 0 }) })),
      Story.message(ClickedAudition({ time: 12 })),
      Story.Command.expectNone(),
    );
  });

  test("CompletedEmitAuditionAt is a no-op ack", () => {
    Story.story(
      update,
      Story.with(ready()),
      Story.message(CompletedEmitAuditionAt()),
      Story.model((m) => expect(asReady(m).editor.tracks[0]?.start).toBe(0)),
      Story.Command.expectNone(),
    );
  });

  test("ClickedSubmitSplit with a valid editor goes busy and submits", () => {
    Story.story(
      update,
      Story.with(ready()),
      Story.message(ClickedSubmitSplit()),
      Story.model((m) => {
        expect(m.busy).toBe(true);
        expect(m.status).toEqual(StatusValue.NoStatus());
      }),
      Story.Command.expectHas(SubmitSplit),
      Story.Command.resolve(SubmitSplit, CompletedSubmitSplit({ status: 202, body: "" })),
      Story.model((m) => {
        expect(m.busy).toBe(false);
        expect(m.status).toEqual(
          StatusValue.StatusOk({ message: "Splitting… the track list will update when it finishes." }),
        );
      }),
    );
  });

  test("ClickedSubmitSplit with an invalid editor surfaces a validation error and submits nothing", () => {
    const overlapping = editor({
      tracks: [
        { title: "One", start: 0, end: 50 },
        { title: "Two", start: 40, end: 100 },
      ],
      linked: [false],
    });
    Story.story(
      update,
      Story.with(ready({ editorState: overlapping })),
      Story.message(ClickedSubmitSplit()),
      Story.model((m) => expect(m.status._tag).toBe("StatusError")),
      Story.Command.expectNone(),
    );
  });

  test("ClickedResetToAuto goes busy and resets", () => {
    Story.story(
      update,
      Story.with(ready()),
      Story.message(ClickedResetToAuto()),
      Story.model((m) => expect(m.busy).toBe(true)),
      Story.Command.expectHas(ResetSplit),
      Story.Command.resolve(ResetSplit, CompletedResetSplit({ status: 200, body: "" })),
      Story.model((m) =>
        expect(m.status).toEqual(StatusValue.StatusOk({ message: "Already using the automatic split." })),
      ),
    );
  });

  test("a 409 response surfaces an error and resyncs", () => {
    const resynced = editor({ tracks: [{ title: "Resynced", start: 0, end: 99 }], linked: [] });
    Story.story(
      update,
      Story.with(ready({ busy: true })),
      Story.message(CompletedSubmitSplit({ status: 409, body: "job already running" })),
      Story.model((m) => {
        expect(m.busy).toBe(false);
        expect(m.status).toEqual(StatusValue.StatusError({ message: "job already running" }));
      }),
      Story.Command.expectHas(ResyncAfterConflict),
      Story.Command.resolve(ResyncAfterConflict, CompletedResync({ maybeEditor: Option.some(resynced) })),
      Story.model((m) => expect(asReady(m).editor.tracks[0]?.title).toBe("Resynced")),
    );
  });

  test("an unexpected status surfaces the body as an error with no resync", () => {
    Story.story(
      update,
      Story.with(ready({ busy: true })),
      Story.message(CompletedResetSplit({ status: 422, body: "Track too short" })),
      Story.model((m) => {
        expect(m.busy).toBe(false);
        expect(m.status).toEqual(StatusValue.StatusError({ message: "Track too short" }));
      }),
      Story.Command.expectNone(),
    );
  });

  test("ClickedRevertEdits goes busy and requests a revert", () => {
    const saved = editor({ tracks: [{ title: "Saved", start: 0, end: 99 }], linked: [] });
    Story.story(
      update,
      Story.with(ready()),
      Story.message(ClickedRevertEdits()),
      Story.model((m) => {
        expect(m.busy).toBe(true);
        expect(m.status).toEqual(StatusValue.StatusOk({ message: "Discarding edits…" }));
      }),
      Story.Command.expectHas(RevertEdits),
      Story.Command.resolve(
        RevertEdits,
        CompletedRevertEdits({ outcome: RevertOutcomeValue.RestoredEditor({ editor: saved }) }),
      ),
      Story.model((m) => {
        expect(m.busy).toBe(false);
        expect(asReady(m).editor.tracks[0]?.title).toBe("Saved");
        expect(m.status).toEqual(StatusValue.StatusOk({ message: "Restored the last saved times." }));
      }),
    );
  });

  test("CompletedRevertEdits with no saved editor surfaces an error", () => {
    Story.story(
      update,
      Story.with(ready({ busy: true })),
      Story.message(CompletedRevertEdits({ outcome: RevertOutcomeValue.NoSavedEditor() })),
      Story.model((m) => {
        expect(m.busy).toBe(false);
        expect(m.status).toEqual(StatusValue.StatusError({ message: "No saved times to restore." }));
      }),
      Story.Command.expectNone(),
    );
  });

  test("CompletedRevertEdits with a failed fetch surfaces an error", () => {
    Story.story(
      update,
      Story.with(ready({ busy: true })),
      Story.message(CompletedRevertEdits({ outcome: RevertOutcomeValue.RevertFetchFailed() })),
      Story.model((m) => {
        expect(m.busy).toBe(false);
        expect(m.status).toEqual(
          StatusValue.StatusError({ message: "Could not load saved times — please retry." }),
        );
      }),
      Story.Command.expectNone(),
    );
  });

  test("CompletedResync with no data leaves the editor untouched", () => {
    Story.story(
      update,
      Story.with(ready()),
      Story.message(CompletedResync({ maybeEditor: Option.none() })),
      Story.model((m) => expect(asReady(m).editor.tracks[0]?.start).toBe(0)),
      Story.Command.expectNone(),
    );
  });

  test("ChangedPlayhead updates the playhead fraction", () => {
    Story.story(
      update,
      Story.with(ready()),
      Story.message(ChangedPlayhead({ fraction: Option.some(0.5) })),
      Story.model((m) => expect(Option.getOrNull(m.playheadFraction)).toBe(0.5)),
      Story.Command.expectNone(),
    );
  });
});
