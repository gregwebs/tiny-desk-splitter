import { Option } from "effect";
import { Scene } from "foldkit";
import { describe, test } from "vitest";

import { ScrollActiveIntoView } from "./command";
import { CompletedScrollActiveIntoView } from "./message";
import { type AddTarget, type Model, PhaseValue, type RowId } from "./model";
import { update } from "./update";
import { view } from "./view";

// Foldkit Scene tests: render the view for a given Model and assert/interact
// through the accessible DOM (role/text), exercising view + update together in
// happy-dom. Complements the Story tests (model-level) and the Playwright e2e
// (real browser).

const trackA: AddTarget = { type: "track", concertId: 1, trackIndex: 0, label: "Celular" };

const loaded = (fields: {
  playlists?: { id: number; name: string }[];
  members?: { playlistId: number; itemId: number }[];
  filter?: string;
  activeId?: Option.Option<RowId>;
  activeFromTyping?: boolean;
}): Model => ({
  phase: PhaseValue.Loaded({
    target: trackA,
    playlists: fields.playlists ?? [],
    members: fields.members ?? [],
    filter: fields.filter ?? "",
    activeId: fields.activeId ?? Option.none(),
    activeFromTyping: fields.activeFromTyping ?? false,
  }),
  error: Option.none(),
});

describe("add-panel view", () => {
  test("renders the context label, a filter combobox, and a row per playlist", () => {
    Scene.scene(
      { update, view },
      Scene.with(
        loaded({
          playlists: [
            { id: 1, name: "Rock" },
            { id: 2, name: "Jazz" },
          ],
          members: [{ playlistId: 1, itemId: 5 }], // Rock is a member
        }),
      ),
      Scene.expect(Scene.text("Adding “Celular” to…")).toExist(),
      Scene.expect(Scene.role("combobox")).toExist(),
      Scene.expect(Scene.text("Rock")).toExist(),
      Scene.expect(Scene.text("Jazz")).toExist(),
      // The member row shows its checkmark.
      Scene.expect(Scene.text("✓")).toExist(),
      // Two playlist rows (no create/empty row when the filter is empty).
      Scene.expectAll(Scene.all.role("option")).toHaveCount(2),
    );
  });

  test("renders the loading row while loading", () => {
    Scene.scene(
      { update, view },
      Scene.with({ phase: PhaseValue.Loading({ target: trackA }), error: Option.none() }),
      Scene.expect(Scene.text("Loading…")).toExist(),
    );
  });

  test("surfaces an error message", () => {
    Scene.scene(
      { update, view },
      Scene.with({
        phase: PhaseValue.LoadFailed({ target: trackA }),
        error: Option.some("Couldn't load playlists."),
      }),
      Scene.expect(Scene.text("Couldn't load playlists.")).toExist(),
    );
  });

  test("typing in the filter narrows the list (no auto-highlight Command for a partial match)", () => {
    Scene.scene(
      { update, view },
      Scene.with(
        loaded({
          playlists: [
            { id: 1, name: "Rock" },
            { id: 2, name: "Jazz" },
          ],
          members: [],
        }),
      ),
      Scene.type(Scene.role("combobox"), "Ro"),
      // "Jazz" is filtered out; "Rock" stays.
      Scene.expect(Scene.text("Rock")).toExist(),
      Scene.expect(Scene.text("Jazz")).toBeAbsent(),
      // A partial match to an existing non-member auto-highlights nothing, so no
      // scroll Command is emitted.
      Scene.Command.expectNone(),
    );
  });

  test("ArrowDown on the filter highlights the first row in display order (members first)", () => {
    Scene.scene(
      { update, view },
      Scene.with(
        loaded({
          playlists: [
            { id: 1, name: "Rock" },
            { id: 2, name: "Jazz" },
          ],
          members: [{ playlistId: 1, itemId: 5 }], // Rock is a member, sorts first.
        }),
      ),
      Scene.keydown(Scene.role("combobox"), "ArrowDown"),
      Scene.expect(Scene.role("option", { selected: true })).toContainText("Rock"),
      Scene.Command.expectHas(ScrollActiveIntoView),
      Scene.Command.resolve(ScrollActiveIntoView, CompletedScrollActiveIntoView()),
    );
  });
});
