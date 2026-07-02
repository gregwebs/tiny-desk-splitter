import { Match as M, Option } from "effect";
import type { Command } from "foldkit/command";
import { evo } from "foldkit/struct";

import { autoHighlight, buildRows, itemIdFor, nextRow, prevRow, type Member, type Row } from "../core";
import {
  AddItem,
  CreateAndAdd,
  FocusFilter,
  LoadAddPanel,
  RemoveItem,
  RequestClose,
  RequestNewName,
  ScrollActiveIntoView,
} from "./command";
import type { Message } from "./message";
import { type AddTarget, type Model, type Phase, PhaseValue } from "./model";

// UPDATE

type UpdateReturn = readonly [Model, ReadonlyArray<Command<Message>>];
const withUpdateReturn = M.withReturnType<UpdateReturn>();

type Loaded = Extract<Phase, { _tag: "Loaded" }>;

/** Target identity for the staleness rule (label is cosmetic, excluded). */
const sameTarget = (a: AddTarget, b: AddTarget): boolean => {
  switch (a.type) {
    case "track":
      return b.type === "track" && a.concertId === b.concertId && a.trackIndex === b.trackIndex;
    case "concert":
      return b.type === "concert" && a.concertId === b.concertId;
    case "playlist":
      return b.type === "playlist" && a.childPlaylistId === b.childPlaylistId;
  }
};

const currentTarget = (model: Model): Option.Option<AddTarget> =>
  M.value(model.phase).pipe(
    M.withReturnType<Option.Option<AddTarget>>(),
    M.tag("Loading", (phase) => Option.some(phase.target)),
    M.tag("LoadFailed", (phase) => Option.some(phase.target)),
    M.tag("Loaded", (phase) => Option.some(phase.target)),
    M.tag("Closed", () => Option.none()),
    M.exhaustive,
  );

const clearError = (model: Model): Model => evo(model, { error: () => Option.none() });

const closed = (model: Model): Model =>
  evo(model, { phase: () => PhaseValue.Closed(), error: () => Option.none() });

type LoadedOverrides = Partial<{
  playlists: Loaded["playlists"];
  members: Loaded["members"];
  filter: string;
  activeId: Loaded["activeId"];
  activeFromTyping: boolean;
}>;

const setLoaded = (model: Model, loaded: Loaded, overrides: LoadedOverrides): Model =>
  evo(model, {
    phase: () =>
      PhaseValue.Loaded({
        target: loaded.target,
        playlists: overrides.playlists ?? loaded.playlists,
        members: overrides.members ?? loaded.members,
        filter: overrides.filter ?? loaded.filter,
        activeId: overrides.activeId ?? loaded.activeId,
        activeFromTyping: overrides.activeFromTyping ?? loaded.activeFromTyping,
      }),
  });

const withLoaded = (model: Model, f: (loaded: Loaded) => UpdateReturn): UpdateReturn =>
  M.value(model.phase).pipe(
    withUpdateReturn,
    M.tag("Loaded", (loaded) => f(loaded)),
    M.orElse(() => [model, []]),
  );

/** The typing auto-highlight for a fresh render at `filter`, plus the scroll
 *  Command to bring the newly highlighted row into view. Only fires when
 *  nothing is already highlighted (currentActive null), per `core.autoHighlight`. */
const computeAutoHighlight = (
  playlists: Loaded["playlists"],
  members: Loaded["members"],
  filter: string,
): { activeId: Loaded["activeId"]; activeFromTyping: boolean; scroll: ReadonlyArray<Command<Message>> } => {
  const rows = buildRows({ playlists, members, filter });
  const auto = autoHighlight({ rows, filter, currentActive: null });
  return auto !== null
    ? { activeId: Option.some(auto), activeFromTyping: true, scroll: [ScrollActiveIntoView({ rowId: auto })] }
    : { activeId: Option.none(), activeFromTyping: false, scroll: [] };
};

/** The Command(s) the active row's primary action maps to (Enter / row click). */
const commandsForRow = (
  target: AddTarget,
  members: ReadonlyArray<Member>,
  row: Row,
): ReadonlyArray<Command<Message>> => {
  switch (row.kind) {
    case "nonmember":
      return typeof row.id === "number" ? [AddItem({ target, playlistId: row.id })] : [];
    case "member": {
      if (typeof row.id !== "number") return [];
      const itemId = itemIdFor(members, row.id);
      return itemId !== null ? [RemoveItem({ target, playlistId: row.id, itemId })] : [];
    }
    case "create":
      return [CreateAndAdd({ target, name: row.name })];
    case "empty":
      return [RequestNewName()];
  }
};

export const update = (model: Model, message: Message): UpdateReturn =>
  M.value(message).pipe(
    withUpdateReturn,
    M.tagsExhaustive({
      OpenRequested: ({ target }) => [
        evo(model, { phase: () => PhaseValue.Loading({ target }), error: () => Option.none() }),
        [LoadAddPanel({ target }), FocusFilter()],
      ],

      CloseRequested: () => [closed(model), []],

      CompletedLoad: ({ forTarget, playlists, members }) =>
        Option.match(currentTarget(model), {
          onNone: () => [model, []],
          onSome: (target) =>
            sameTarget(target, forTarget)
              ? [
                  evo(model, {
                    phase: () =>
                      PhaseValue.Loaded({
                        target: forTarget,
                        playlists,
                        members,
                        filter: "",
                        activeId: Option.none(),
                        activeFromTyping: false,
                      }),
                    error: () => Option.none(),
                  }),
                  [],
                ]
              : [model, []],
        }),

      FailedLoad: ({ forTarget }) =>
        Option.match(currentTarget(model), {
          onNone: () => [model, []],
          onSome: (target) =>
            sameTarget(target, forTarget)
              ? [
                  evo(model, {
                    phase: () => PhaseValue.LoadFailed({ target: forTarget }),
                    error: () => Option.some("Couldn't load playlists."),
                  }),
                  [],
                ]
              : [model, []],
        }),

      ChangedFilter: ({ value }) =>
        withLoaded(model, (loaded) => {
          const { activeId, activeFromTyping, scroll } = computeAutoHighlight(
            loaded.playlists,
            loaded.members,
            value,
          );
          return [setLoaded(clearError(model), loaded, { filter: value, activeId, activeFromTyping }), scroll];
        }),

      PressedArrowDown: () =>
        withLoaded(model, (loaded) => {
          const rows = buildRows({ playlists: loaded.playlists, members: loaded.members, filter: loaded.filter });
          const next = nextRow(rows, Option.getOrNull(loaded.activeId));
          return [
            setLoaded(model, loaded, { activeId: Option.fromNullishOr(next), activeFromTyping: false }),
            next !== null ? [ScrollActiveIntoView({ rowId: next })] : [],
          ];
        }),

      PressedArrowUp: () =>
        withLoaded(model, (loaded) => {
          const rows = buildRows({ playlists: loaded.playlists, members: loaded.members, filter: loaded.filter });
          const prev = prevRow(rows, Option.getOrNull(loaded.activeId));
          return [
            setLoaded(model, loaded, { activeId: Option.fromNullishOr(prev), activeFromTyping: false }),
            prev !== null ? [ScrollActiveIntoView({ rowId: prev })] : [],
          ];
        }),

      PressedEnter: () =>
        withLoaded(model, (loaded) => {
          const active = Option.getOrNull(loaded.activeId);
          if (active !== null) {
            const rows = buildRows({ playlists: loaded.playlists, members: loaded.members, filter: loaded.filter });
            const row = rows.find((row) => row.id === active);
            if (row) {
              const commands = commandsForRow(loaded.target, loaded.members, row);
              // Typing-originated: clear the filter + highlight in the same
              // update, then act. Arrow-originated: act, keep the filter so the
              // user can keep toggling the same row.
              return loaded.activeFromTyping
                ? [
                    setLoaded(clearError(model), loaded, {
                      filter: "",
                      activeId: Option.none(),
                      activeFromTyping: false,
                    }),
                    commands,
                  ]
                : [clearError(model), commands];
            }
          }
          // No highlight + empty filter: close. Otherwise a no-op (ambiguous).
          return loaded.filter.trim() === "" ? [closed(model), [RequestClose()]] : [model, []];
        }),

      ClickedRow: ({ id }) =>
        withLoaded(model, (loaded) => {
          // Interpret the click against the row's *current* kind, so a reused
          // row element (member↔nonmember, empty↔create) can never act on a
          // stale handler. A click on a member row is a deliberate no-op — the
          // trash button removes — unlike Enter on a highlighted member row,
          // which toggles it off (see commandsForRow).
          const rows = buildRows({ playlists: loaded.playlists, members: loaded.members, filter: loaded.filter });
          const row = rows.find((row) => row.id === id);
          if (!row) return [model, []];
          switch (row.kind) {
            case "member":
              return [model, []];
            case "nonmember":
              return typeof row.id === "number"
                ? [clearError(model), [AddItem({ target: loaded.target, playlistId: row.id })]]
                : [model, []];
            case "create":
              return [clearError(model), [CreateAndAdd({ target: loaded.target, name: loaded.filter.trim() })]];
            case "empty":
              return [clearError(model), [RequestNewName()]];
          }
        }),

      ClickedRemove: ({ playlistId }) =>
        withLoaded(model, (loaded) => {
          const itemId = itemIdFor(loaded.members, playlistId);
          return itemId !== null
            ? [clearError(model), [RemoveItem({ target: loaded.target, playlistId, itemId })]]
            : [model, []];
        }),

      EnteredNewName: ({ name }) =>
        withLoaded(model, (loaded) => [clearError(model), [CreateAndAdd({ target: loaded.target, name })]]),

      ClickedClose: () => [closed(model), [RequestClose()]],

      CompletedMutation: ({ forTarget, playlists, members }) =>
        withLoaded(model, (loaded) => {
          if (!sameTarget(loaded.target, forTarget)) return [model, []];
          // Re-run the typing auto-highlight only when nothing is currently
          // highlighted (mirrors the old renderAddList's activeId === null guard).
          if (Option.isNone(loaded.activeId)) {
            const { activeId, activeFromTyping, scroll } = computeAutoHighlight(playlists, members, loaded.filter);
            return [setLoaded(clearError(model), loaded, { playlists, members, activeId, activeFromTyping }), scroll];
          }
          return [setLoaded(clearError(model), loaded, { playlists, members }), []];
        }),

      FailedMutation: ({ forTarget, errorMessage: msg }) =>
        withLoaded(model, (loaded) =>
          sameTarget(loaded.target, forTarget) ? [evo(model, { error: () => Option.some(msg) }), []] : [model, []],
        ),

      CompletedScrollActiveIntoView: () => [model, []],
      CompletedRequestClose: () => [model, []],
      CompletedRequestNewName: () => [model, []],
      CompletedFocusFilter: () => [model, []],
    }),
  );
