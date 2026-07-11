import { Option } from "effect";
import { Story } from "foldkit";
import { describe, expect, test } from "vitest";

import {
  AddItem,
  CreateAndAdd,
  FocusFilter,
  LoadAddPanel,
  RemoveItem,
  ScrollActiveIntoView,
} from "./command";
import {
  ChangedFilter,
  ClickedRemove,
  ClickedRow,
  CompletedFocusFilter,
  CompletedLoad,
  CompletedMutation,
  CompletedScrollActiveIntoView,
  FailedLoad,
  FailedMutation,
  OpenRequested,
  PressedArrowDown,
  PressedEnter,
} from "./message";
import { type AddTarget, type Model, PhaseValue, type RowId } from "./model";
import { update } from "./update";

// Foldkit Story tests for the add-panel `update` (foldkit's own MVU harness):
// feed a model + a sequence of Messages, assert on the resulting Model and the
// Commands it emits. `Story.story` throws if any emitted Command is left
// unresolved, so each command is either resolved or asserted absent.

const trackA: AddTarget = { type: "track", concertId: 1, trackIndex: 0 };
const trackB: AddTarget = { type: "track", concertId: 2, trackIndex: 0 };

type PlaylistRef = { id: number; name: string };
type Member = { playlistId: number; itemId: number };

const closed: Model = { phase: PhaseValue.Closed(), error: Option.none() };

const loading = (target: AddTarget): Model => ({
  phase: PhaseValue.Loading({ target }),
  error: Option.none(),
});

const loaded = (fields: {
  target?: AddTarget;
  playlists?: PlaylistRef[];
  members?: Member[];
  filter?: string;
  activeId?: Option.Option<RowId>;
  activeFromTyping?: boolean;
}): Model => ({
  phase: PhaseValue.Loaded({
    target: fields.target ?? trackA,
    playlists: fields.playlists ?? [],
    members: fields.members ?? [],
    filter: fields.filter ?? "",
    activeId: fields.activeId ?? Option.none(),
    activeFromTyping: fields.activeFromTyping ?? false,
  }),
  error: Option.none(),
});

/** Narrow `model.phase` to the `Loaded` variant for assertions. */
const asLoaded = (model: Model) => {
  if (model.phase._tag !== "Loaded") throw new Error(`expected Loaded, got ${model.phase._tag}`);
  return model.phase;
};

describe("add-panel update", () => {
  test("OpenRequested enters Loading and fires the load + focus Commands", () => {
    Story.story(
      update,
      Story.with(closed),
      Story.message(OpenRequested({ target: trackA })),
      Story.model((m) => expect(m.phase._tag).toBe("Loading")),
      Story.Command.expectHas(LoadAddPanel, FocusFilter),
      // Resolve both so the story has no dangling Commands; the load result
      // carries the panel into Loaded.
      Story.Command.resolve(FocusFilter, CompletedFocusFilter()),
      Story.Command.resolve(
        LoadAddPanel,
        CompletedLoad({ forTarget: trackA, playlists: [{ id: 1, name: "Rock" }], members: [] }),
      ),
      Story.model((m) => {
        const l = asLoaded(m);
        expect(l.playlists.length).toBe(1);
        expect(l.filter).toBe("");
      }),
    );
  });

  test("a CompletedLoad for a superseded target is ignored (staleness rule)", () => {
    Story.story(
      update,
      Story.with(loading(trackA)),
      // A load that resolved for a target the panel has since moved off.
      Story.message(
        CompletedLoad({ forTarget: trackB, playlists: [{ id: 9, name: "Stale" }], members: [] }),
      ),
      Story.model((m) => {
        // Still Loading the original target; the stale result did not apply.
        expect(m.phase._tag).toBe("Loading");
      }),
      Story.Command.expectNone(),
    );
  });

  test("a matching CompletedLoad transitions to Loaded", () => {
    Story.story(
      update,
      Story.with(loading(trackA)),
      Story.message(
        CompletedLoad({ forTarget: trackA, playlists: [{ id: 1, name: "Rock" }], members: [] }),
      ),
      Story.model((m) => expect(asLoaded(m).playlists.length).toBe(1)),
      Story.Command.expectNone(),
    );
  });

  test("a FailedLoad for a superseded target is ignored (staleness rule)", () => {
    Story.story(
      update,
      Story.with(loading(trackA)),
      Story.message(FailedLoad({ forTarget: trackB })),
      Story.model((m) => expect(m.phase._tag).toBe("Loading")),
      Story.Command.expectNone(),
    );
  });

  test("a matching FailedLoad enters LoadFailed with an error", () => {
    Story.story(
      update,
      Story.with(loading(trackA)),
      Story.message(FailedLoad({ forTarget: trackA })),
      Story.model((m) => {
        expect(m.phase._tag).toBe("LoadFailed");
        expect(Option.getOrNull(m.error)).toBe("Couldn't load playlists.");
      }),
      Story.Command.expectNone(),
    );
  });

  test("clicking a non-member row adds it", () => {
    Story.story(
      update,
      Story.with(loaded({ playlists: [{ id: 1, name: "Rock" }], members: [] })),
      Story.message(ClickedRow({ id: 1 })),
      Story.Command.expectHas(AddItem),
      Story.Command.resolve(
        AddItem,
        CompletedMutation({
          forTarget: trackA,
          playlists: [{ id: 1, name: "Rock" }],
          members: [{ playlistId: 1, itemId: 5 }],
        }),
      ),
      Story.model((m) => expect(asLoaded(m).members).toContainEqual({ playlistId: 1, itemId: 5 })),
    );
  });

  test("a failed AddItem surfaces the error and leaves membership untouched", () => {
    Story.story(
      update,
      Story.with(loaded({ playlists: [{ id: 1, name: "Rock" }], members: [] })),
      Story.message(ClickedRow({ id: 1 })),
      Story.Command.expectHas(AddItem),
      Story.Command.resolve(
        AddItem,
        FailedMutation({ forTarget: trackA, errorMessage: "Couldn't add to playlist." }),
      ),
      Story.model((m) => {
        expect(Option.getOrNull(m.error)).toBe("Couldn't add to playlist.");
        expect(asLoaded(m).members.length).toBe(0);
      }),
    );
  });

  test("clicking a member row is a no-op (the trash button removes)", () => {
    Story.story(
      update,
      Story.with(loaded({ playlists: [{ id: 1, name: "Rock" }], members: [{ playlistId: 1, itemId: 5 }] })),
      Story.message(ClickedRow({ id: 1 })),
      Story.Command.expectNone(),
    );
  });

  test("the trash button on a member row removes it", () => {
    Story.story(
      update,
      Story.with(loaded({ playlists: [{ id: 1, name: "Rock" }], members: [{ playlistId: 1, itemId: 5 }] })),
      Story.message(ClickedRemove({ playlistId: 1 })),
      Story.Command.expectHas(RemoveItem),
      Story.Command.resolve(
        RemoveItem,
        CompletedMutation({ forTarget: trackA, playlists: [{ id: 1, name: "Rock" }], members: [] }),
      ),
      Story.model((m) => expect(asLoaded(m).members.length).toBe(0)),
    );
  });

  test("a failed RemoveItem surfaces the error and leaves membership untouched", () => {
    Story.story(
      update,
      Story.with(loaded({ playlists: [{ id: 1, name: "Rock" }], members: [{ playlistId: 1, itemId: 5 }] })),
      Story.message(ClickedRemove({ playlistId: 1 })),
      Story.Command.expectHas(RemoveItem),
      Story.Command.resolve(
        RemoveItem,
        FailedMutation({ forTarget: trackA, errorMessage: "Couldn't remove from playlist." }),
      ),
      Story.model((m) => {
        expect(Option.getOrNull(m.error)).toBe("Couldn't remove from playlist.");
        expect(asLoaded(m).members).toContainEqual({ playlistId: 1, itemId: 5 });
      }),
    );
  });

  test("a failed CreateAndAdd surfaces the error without clearing the loaded view", () => {
    Story.story(
      update,
      Story.with(
        loaded({ playlists: [], filter: "New Mix", activeId: Option.some("new"), activeFromTyping: true }),
      ),
      Story.message(PressedEnter()),
      Story.Command.expectHas(CreateAndAdd),
      Story.Command.resolve(
        CreateAndAdd,
        FailedMutation({ forTarget: trackA, errorMessage: "Couldn't create playlist." }),
      ),
      Story.model((m) => {
        expect(Option.getOrNull(m.error)).toBe("Couldn't create playlist.");
        expect(asLoaded(m).playlists.length).toBe(0);
      }),
    );
  });

  test("clicking the create row creates and adds the current target", () => {
    Story.story(
      update,
      Story.with(loaded({ playlists: [], filter: "New Mix", activeId: Option.some("new") })),
      Story.message(ClickedRow({ id: "new" })),
      Story.Command.expectHas(CreateAndAdd),
      Story.Command.resolve(
        CreateAndAdd,
        CompletedMutation({
          forTarget: trackA,
          playlists: [{ id: 7, name: "New Mix" }],
          members: [{ playlistId: 7, itemId: 9 }],
        }),
      ),
      Story.model((m) => {
        const phase = asLoaded(m);
        expect(phase.playlists).toContainEqual({ id: 7, name: "New Mix" });
        expect(phase.members).toContainEqual({ playlistId: 7, itemId: 9 });
      }),
    );
  });

  test("a stale CompletedMutation for a superseded target is ignored", () => {
    Story.story(
      update,
      Story.with(loaded({ target: trackA, playlists: [{ id: 1, name: "Rock" }], members: [] })),
      Story.message(CompletedMutation({ forTarget: trackB, playlists: [], members: [] })),
      Story.model((m) => {
        // Unchanged: still trackA's loaded view with its playlist.
        expect(asLoaded(m).playlists.length).toBe(1);
        expect(asLoaded(m).members.length).toBe(0);
      }),
      Story.Command.expectNone(),
    );
  });

  test("a stale FailedMutation for a superseded target is ignored", () => {
    Story.story(
      update,
      Story.with(loaded({ target: trackA, playlists: [{ id: 1, name: "Rock" }], members: [] })),
      Story.message(FailedMutation({ forTarget: trackB, errorMessage: "Couldn't add to playlist." })),
      Story.model((m) => {
        // Unchanged: no error surfaced, still trackA's loaded view.
        expect(Option.isNone(m.error)).toBe(true);
        expect(asLoaded(m).playlists.length).toBe(1);
      }),
      Story.Command.expectNone(),
    );
  });

  test("a matching FailedMutation surfaces the error without discarding the loaded view", () => {
    Story.story(
      update,
      Story.with(loaded({ target: trackA, playlists: [{ id: 1, name: "Rock" }], members: [] })),
      Story.message(FailedMutation({ forTarget: trackA, errorMessage: "Couldn't add to playlist." })),
      Story.model((m) => {
        expect(Option.getOrNull(m.error)).toBe("Couldn't add to playlist.");
        // Still Loaded, with its prior playlists untouched.
        expect(asLoaded(m).playlists.length).toBe(1);
      }),
      Story.Command.expectNone(),
    );
  });

  test("ChangedFilter to an exact non-member name auto-highlights it (typing-origin) and scrolls", () => {
    Story.story(
      update,
      Story.with(loaded({ playlists: [{ id: 1, name: "Rock" }], members: [] })),
      Story.message(ChangedFilter({ value: "Rock" })),
      Story.model((m) => {
        const l = asLoaded(m);
        expect(l.filter).toBe("Rock");
        expect(l.activeFromTyping).toBe(true);
        expect(Option.getOrNull(l.activeId)).toBe(1);
      }),
      // The auto-highlight schedules a scroll Command.
      Story.Command.expectHas(ScrollActiveIntoView),
      Story.Command.resolve(ScrollActiveIntoView, CompletedScrollActiveIntoView()),
    );
  });

  test("Enter on a typing-origin highlight clears the filter and acts", () => {
    Story.story(
      update,
      // Filter typed to a unique new name -> the create row is auto-highlighted.
      Story.with(
        loaded({ playlists: [], filter: "New Mix", activeId: Option.some("new"), activeFromTyping: true }),
      ),
      Story.message(PressedEnter()),
      Story.model((m) => {
        const l = asLoaded(m);
        // Typing-origin: the filter is cleared in the same update.
        expect(l.filter).toBe("");
        expect(Option.isNone(l.activeId)).toBe(true);
      }),
      Story.Command.expectHas(CreateAndAdd),
      Story.Command.resolve(
        CreateAndAdd,
        CompletedMutation({
          forTarget: trackA,
          playlists: [{ id: 7, name: "New Mix" }],
          members: [{ playlistId: 7, itemId: 1 }],
        }),
      ),
    );
  });

  test("ArrowDown from no highlight selects the first row and scrolls", () => {
    Story.story(
      update,
      Story.with(loaded({ playlists: [{ id: 1, name: "Rock" }, { id: 2, name: "Jazz" }], members: [] })),
      Story.message(PressedArrowDown()),
      Story.model((m) => expect(Option.getOrNull(asLoaded(m).activeId)).toBe(1)),
      Story.Command.expectHas(ScrollActiveIntoView),
      Story.Command.resolve(ScrollActiveIntoView, CompletedScrollActiveIntoView()),
    );
  });
});
