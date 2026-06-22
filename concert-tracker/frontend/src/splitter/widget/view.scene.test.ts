import { Option } from "effect";
import { Scene } from "foldkit";
import { describe, test } from "vitest";

import { DragStateValue, PhaseValue, StatusValue, type EditorState, type Model } from "./model";
import { update } from "./update";
import { view } from "./view";

// Foldkit Scene tests: render the view for a given Model and assert/interact
// through the accessible DOM (role/text), exercising view + update together in
// happy-dom. Complements the Story tests (model-level) and the Playwright e2e
// (real browser, e2e/splitter.spec.js).

const editor = (over?: Partial<EditorState>): EditorState => ({
  duration: 100,
  tracks: [
    { title: "One", start: 0, end: 40 },
    { title: "Two", start: 40, end: 100 },
  ],
  linked: [true],
  ...over,
});

const baseModel: Model = {
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
  playable?: boolean;
  mediaUrl?: Option.Option<string>;
}): Model => ({
  ...baseModel,
  phase: PhaseValue.Ready({
    editor: over?.editorState ?? editor(),
    mediaUrl: over?.mediaUrl ?? Option.none(),
    playable: over?.playable ?? true,
  }),
  busy: over?.busy ?? false,
  status: over?.status ?? StatusValue.NoStatus(),
});

describe("splitter view", () => {
  test("Ready renders the toolbar, one table row per track, and boundary controls", () => {
    Scene.scene(
      { update, view },
      Scene.with(ready()),
      Scene.expect(Scene.text("Split with these times")).toExist(),
      Scene.expect(Scene.text("Discard my edits")).toExist(),
      Scene.expect(Scene.text("Reset to auto")).toExist(),
      Scene.expect(Scene.text("One")).toExist(),
      Scene.expect(Scene.text("Two")).toExist(),
      Scene.expect(Scene.text("Detach (add gap)")).toExist(),
    );
  });

  test("Loading shows a loading status", () => {
    Scene.scene(
      { update, view },
      Scene.with(baseModel),
      Scene.expect(Scene.text("Loading…")).toExist(),
    );
  });

  test("Empty explains no split points exist yet", () => {
    Scene.scene(
      { update, view },
      Scene.with({ ...baseModel, phase: PhaseValue.Empty() }),
      Scene.expect(
        Scene.text("No split points yet — run an automatic split first, then come back to fine-tune them."),
      ).toExist(),
    );
  });

  test("LoadFailed surfaces a load error", () => {
    Scene.scene(
      { update, view },
      Scene.with({ ...baseModel, phase: PhaseValue.LoadFailed() }),
      Scene.expect(Scene.text("Could not load split timestamps.")).toExist(),
    );
  });

  test("a status error shows in the toolbar", () => {
    Scene.scene(
      { update, view },
      Scene.with(ready({ status: StatusValue.StatusError({ message: "Enter a time like 2:05.0" }) })),
      Scene.expect(Scene.text("Enter a time like 2:05.0")).toExist(),
    );
  });

  test("clicking Detach toggles the boundary to Link, and back on a second click", () => {
    Scene.scene(
      { update, view },
      Scene.with(ready()),
      Scene.expect(Scene.text("Detach (add gap)")).toExist(),
      Scene.click(Scene.text("Detach (add gap)")),
      Scene.expect(Scene.text("Link (remove gap)")).toExist(),
      Scene.expect(Scene.text("Detach (add gap)")).toBeAbsent(),
      Scene.Command.expectNone(),
      Scene.click(Scene.text("Link (remove gap)")),
      Scene.expect(Scene.text("Detach (add gap)")).toExist(),
      Scene.Command.expectNone(),
    );
  });

  test("an unsupported source format shows the format-specific preview note", () => {
    Scene.scene(
      { update, view },
      Scene.with(ready({ playable: false, mediaUrl: Option.some("file:///concert.flac") })),
      Scene.expect(Scene.text("Audio preview unavailable for this file format.")).toExist(),
    );
  });

  test("a missing source file shows the not-found preview note", () => {
    Scene.scene(
      { update, view },
      Scene.with(ready({ playable: false, mediaUrl: Option.none() })),
      Scene.expect(Scene.text("Audio preview unavailable — source file not found.")).toExist(),
    );
  });
});
