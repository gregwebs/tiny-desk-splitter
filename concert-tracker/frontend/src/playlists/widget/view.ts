import { Array as Arr, Match as M, Option } from "effect";
import { type Html, html } from "foldkit/html";

import { buildRows, targetLabel, type Row, type RowId } from "../core";
import {
  ChangedFilter,
  ClickedClose,
  ClickedRemove,
  ClickedRow,
  PressedArrowDown,
  PressedArrowUp,
  PressedEnter,
  type Message,
} from "./message";
import type { AddTarget, Model } from "./model";

// VIEW
//
// Renders the entire #sidebar-add-section interior (the template now holds only
// the mount point), preserving every id/class/aria attribute the CSS and the
// e2e suite depend on. The DOM is a pure function of the Model — no separate
// "now sync the DOM to match a variable" pass (the old applyActiveHighlight).

const rowIsActive = (activeId: Option.Option<RowId>, id: RowId): boolean =>
  Option.match(activeId, { onNone: () => false, onSome: (a) => a === id });

const rowClass = (base: string, isActive: boolean): string =>
  isActive ? `${base} add-pl-row-active` : base;

/** Member/non-member rows carry a numeric playlist id (never "new"). */
const playlistIdOf = (row: Row): number => row.id as number;

// Row keys encode the kind so a kind change (member↔nonmember on a toggle,
// empty↔create as the filter changes) recreates the <li> rather than patching
// it in place — the latter can leave a stale click handler from the previous
// kind (e.g. the create row firing the empty row's prompt() bridge). The DOM
// id stays add-pl-opt-<id> regardless, for aria-activedescendant + scroll.
const memberRowView = (row: Row, isActive: boolean): Html => {
  const h = html<Message>();
  const playlistId = playlistIdOf(row);
  return h.keyed("li")(
    `member-${row.id}`,
    [
      h.Id(`add-pl-opt-${row.id}`),
      h.Class(rowClass("add-pl-row add-pl-row-member", isActive)),
      h.Attribute("role", "option"),
      h.Attribute("aria-selected", isActive ? "true" : "false"),
    ],
    [
      h.span([h.Class("add-pl-check"), h.Attribute("aria-hidden", "true")], ["✓"]),
      h.span([h.Class("add-pl-name")], [row.name]),
      h.button(
        [
          h.Class("add-pl-trash"),
          h.Type("button"),
          h.Attribute("aria-label", "Remove from playlist"),
          h.Title("Remove from playlist"),
          h.OnClick(ClickedRemove({ playlistId })),
        ],
        [h.span([h.Class("icon-trash")], [])],
      ),
    ],
  );
};

const nonMemberRowView = (row: Row, isActive: boolean): Html => {
  const h = html<Message>();
  return h.keyed("li")(
    `nonmember-${row.id}`,
    [
      h.Id(`add-pl-opt-${row.id}`),
      h.Class(rowClass("add-pl-row", isActive)),
      h.Attribute("role", "option"),
      h.Attribute("aria-selected", isActive ? "true" : "false"),
      h.Attribute("tabindex", "0"),
      h.OnClick(ClickedRow({ id: row.id })),
      h.OnKeyDownPreventDefault((key) =>
        key === "Enter" || key === " " ? Option.some(ClickedRow({ id: row.id })) : Option.none(),
      ),
    ],
    [
      h.span([h.Class("add-pl-check"), h.Attribute("aria-hidden", "true")], [""]),
      h.span([h.Class("add-pl-name")], [row.name]),
    ],
  );
};

const createRowView = (row: Row, isActive: boolean): Html => {
  const h = html<Message>();
  return h.keyed("li")(
    "create-new",
    [
      h.Id("add-pl-opt-new"),
      h.Class(rowClass("add-pl-row add-pl-row-new", isActive)),
      h.Attribute("role", "option"),
      h.Attribute("aria-selected", isActive ? "true" : "false"),
      h.Attribute("tabindex", "0"),
      h.OnClick(ClickedRow({ id: "new" })),
      h.OnKeyDownPreventDefault((key) =>
        key === "Enter" || key === " " ? Option.some(ClickedRow({ id: "new" })) : Option.none(),
      ),
    ],
    [
      h.span([h.Class("add-pl-check"), h.Attribute("aria-hidden", "true")], ["+"]),
      h.span([h.Class("add-pl-name")], [`Create “${row.name}”`]),
    ],
  );
};

const emptyRowView = (isActive: boolean): Html => {
  const h = html<Message>();
  return h.keyed("li")(
    "empty-new",
    [
      h.Id("add-pl-opt-new"),
      h.Class(rowClass("add-pl-row add-pl-row-new", isActive)),
      h.Attribute("role", "option"),
      h.Attribute("aria-selected", isActive ? "true" : "false"),
      h.Attribute("tabindex", "0"),
      h.OnClick(ClickedRow({ id: "new" })),
      h.OnKeyDownPreventDefault((key) =>
        key === "Enter" || key === " " ? Option.some(ClickedRow({ id: "new" })) : Option.none(),
      ),
    ],
    [
      h.span([h.Class("add-pl-check"), h.Attribute("aria-hidden", "true")], ["+"]),
      h.span([h.Class("add-pl-name")], ["Create a new playlist"]),
    ],
  );
};

const rowView = (row: Row, isActive: boolean): Html => {
  switch (row.kind) {
    case "member":
      return memberRowView(row, isActive);
    case "nonmember":
      return nonMemberRowView(row, isActive);
    case "create":
      return createRowView(row, isActive);
    case "empty":
      return emptyRowView(isActive);
  }
};

const loadingRowView = (): Html => {
  const h = html<Message>();
  return h.keyed("li")(
    "loading",
    [h.Class("add-pl-row add-pl-row-member"), h.Style({ justifyContent: "center" })],
    ["Loading…"],
  );
};

const sectionView = (opts: {
  context: string;
  filterValue: string;
  activeId: Option.Option<RowId>;
  listChildren: ReadonlyArray<Html>;
  error: Option.Option<string>;
}): Html => {
  const h = html<Message>();

  const activeDescendantAttrs = Option.match(opts.activeId, {
    onNone: () => [],
    onSome: (id) => [h.Attribute("aria-activedescendant", `add-pl-opt-${id}`)],
  });

  return h.div(
    [],
    [
      h.div(
        [h.Class("add-pl-header")],
        [
          h.h2([], ["Add to playlist"]),
          h.button(
            [
              h.Class("add-pl-close"),
              h.Type("button"),
              h.Title("Close"),
              h.Attribute("aria-label", "Close"),
              h.OnClick(ClickedClose()),
            ],
            ["×"],
          ),
        ],
      ),
      h.p([h.Class("add-pl-context"), h.Id("add-pl-context")], [opts.context]),
      h.input([
        h.Id("add-pl-filter"),
        h.Class("add-pl-filter"),
        h.Type("text"),
        h.Attribute("placeholder", "Filter playlists…"),
        h.Attribute("autocomplete", "off"),
        h.Attribute("role", "combobox"),
        h.Attribute("aria-expanded", "true"),
        h.Attribute("aria-haspopup", "listbox"),
        h.Attribute("aria-controls", "add-pl-list"),
        h.Value(opts.filterValue),
        h.OnInput((value) => ChangedFilter({ value })),
        h.OnKeyDownPreventDefault((key) =>
          key === "ArrowDown"
            ? Option.some(PressedArrowDown())
            : key === "ArrowUp"
              ? Option.some(PressedArrowUp())
              : key === "Enter"
                ? Option.some(PressedEnter())
                : Option.none(),
        ),
        ...activeDescendantAttrs,
      ]),
      h.ul(
        [h.Id("add-pl-list"), h.Class("add-pl-list"), h.Attribute("role", "listbox")],
        opts.listChildren,
      ),
      h.p(
        [
          h.Class("add-pl-error"),
          h.Id("add-pl-error"),
          h.Style(Option.isSome(opts.error) ? {} : { display: "none" }),
        ],
        [Option.getOrElse(opts.error, () => "")],
      ),
    ],
  );
};

export const view = (model: Model): Html =>
  M.value(model.phase).pipe(
    M.withReturnType<Html>(),
    M.tag("Closed", () =>
      sectionView({
        context: "",
        filterValue: "",
        activeId: Option.none(),
        listChildren: [],
        error: Option.none(),
      }),
    ),
    M.tag("Loading", (p) =>
      sectionView({
        context: targetLabel(p.target as AddTarget),
        filterValue: "",
        activeId: Option.none(),
        listChildren: [loadingRowView()],
        error: model.error,
      }),
    ),
    M.tag("LoadFailed", (p) =>
      sectionView({
        context: targetLabel(p.target as AddTarget),
        filterValue: "",
        activeId: Option.none(),
        listChildren: [],
        error: model.error,
      }),
    ),
    M.tag("Loaded", (l) => {
      const rows = buildRows({ playlists: l.playlists, members: l.members, filter: l.filter });
      return sectionView({
        context: targetLabel(l.target as AddTarget),
        filterValue: l.filter,
        activeId: l.activeId,
        listChildren: Arr.map(rows, (row) => rowView(row, rowIsActive(l.activeId, row.id))),
        error: model.error,
      });
    }),
    M.exhaustive,
  );
