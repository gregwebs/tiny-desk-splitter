import { Array, Match as M, Option } from "effect";
import { type Html, html } from "foldkit/html";

import {
  formatTimecode,
  handlesFor,
  handleTime,
  validate,
  type EditorState,
  type EditorTrack,
  type Handle,
} from "../core";
import {
  ChangedTimeInput,
  ClickedAudition,
  ClickedResetToAuto,
  ClickedRevertEdits,
  ClickedSubmitSplit,
  PressedHandle,
  ToggledBoundary,
  type Message,
} from "./message";
import type { DragState, Model, Status } from "./model";
import { TIMELINE_DATA_ATTRIBUTE, timeFromClientX, timelineElement } from "./timeline";

// VIEW
//
// NOTE: hand-rolling buttons/inputs here rather than Ui.Button / Ui.Input —
// @foldkit/ui is not a dependency of this project (see package.json).
// Adopting it is a separate decision from this a11y pass.

const percentOf = (duration: number, time: number): number =>
  duration > 0 ? (time / duration) * 100 : 0;

const statusClass = (status: Status): string =>
  M.value(status).pipe(
    M.withReturnType<string>(),
    M.tag("StatusOk", () => "splitter-status splitter-status-ok"),
    M.tag("StatusError", () => "splitter-status splitter-status-error"),
    M.tag("NoStatus", () => "splitter-status"),
    M.exhaustive,
  );

const statusText = (status: Status): string =>
  M.value(status).pipe(
    M.withReturnType<string>(),
    M.tag("StatusOk", ({ message }) => message),
    M.tag("StatusError", ({ message }) => message),
    M.tag("NoStatus", () => ""),
    M.exhaustive,
  );

const toolbarView = (editor: EditorState, busy: boolean, status: Status): Html => {
  const h = html<Message>();

  const hasErrors = Array.isArrayNonEmpty(validate(editor));

  return h.div(
    [h.Class("splitter-toolbar")],
    [
      h.span(
        [
          h.Class(statusClass(status)),
          ...(status._tag === "StatusError" ? [h.Role("alert")] : [h.AriaLive("polite")]),
        ],
        [statusText(status)],
      ),
      h.button(
        [
          h.Class("splitter-submit"),
          h.Type("button"),
          h.Disabled(busy || hasErrors),
          h.OnClick(ClickedSubmitSplit()),
        ],
        ["Split with these times"],
      ),
      h.button(
        [h.Class("splitter-revert"), h.Type("button"), h.Disabled(busy), h.OnClick(ClickedRevertEdits())],
        ["Discard my edits"],
      ),
      h.button(
        [h.Class("splitter-reset"), h.Type("button"), h.Disabled(busy), h.OnClick(ClickedResetToAuto())],
        ["Reset to auto"],
      ),
    ],
  );
};

const segmentView = (track: EditorTrack, index: number, duration: number): Html => {
  const h = html<Message>();

  return h.keyed("div")(
    `seg-${index}`,
    [
      h.Class("splitter-seg"),
      h.Title(track.title),
      h.Style({
        left: `${percentOf(duration, track.start)}%`,
        width: `${percentOf(duration, track.end - track.start)}%`,
      }),
    ],
    [h.span([h.Class("splitter-seg-label")], [`${index + 1}. ${track.title}`])],
  );
};

const GAP_VISIBILITY_THRESHOLD_SECONDS = 1e-6;

const gapView = (editor: EditorState, boundaryIndex: number): Html => {
  const h = html<Message>();

  const gapStart = Option.getOrElse(
    Option.map(Array.get(editor.tracks, boundaryIndex), (track) => track.end),
    () => 0,
  );
  const gapEnd = Option.getOrElse(
    Option.map(Array.get(editor.tracks, boundaryIndex + 1), (track) => track.start),
    () => 0,
  );
  const width = gapEnd - gapStart;
  const isVisible = width > GAP_VISIBILITY_THRESHOLD_SECONDS;

  return h.keyed("div")(
    `gap-${boundaryIndex}`,
    [
      h.Class("splitter-gap"),
      h.Style(
        isVisible
          ? {
              display: "block",
              left: `${percentOf(editor.duration, gapStart)}%`,
              width: `${percentOf(editor.duration, width)}%`,
            }
          : { display: "none" },
      ),
    ],
    [],
  );
};

const handleView = (editor: EditorState, handle: Handle, index: number, dragState: DragState): Html => {
  const h = html<Message>();

  const isDragging = dragState._tag === "Dragging" && dragState.handleIndex === index;

  return h.keyed("div")(
    `handle-${index}`,
    [
      h.Class(`splitter-handle splitter-handle-${handle.kind}${isDragging ? " dragging" : ""}`),
      h.Style({ left: `${percentOf(editor.duration, handleTime(editor, handle))}%` }),
      h.OnPointerDown(() => Option.some(PressedHandle({ handleIndex: index }))),
    ],
    [],
  );
};

const auditionAtClientX = (concertId: number, duration: number, clientX: number): Option.Option<Message> =>
  Option.map(timelineElement(concertId), (timeline) =>
    ClickedAudition({ time: timeFromClientX(clientX, timeline, duration) }),
  );

const timelineView = (
  concertId: number,
  editor: EditorState,
  dragState: DragState,
  playable: boolean,
  playheadFraction: Option.Option<number>,
): Html => {
  const h = html<Message>();

  const timelineInteractionAttributes = playable
    ? [
        h.OnPointerDown((_pointerType, _button, _screenX, _screenY, _timeStamp, clientX) =>
          dragState._tag === "Dragging" ? Option.none() : auditionAtClientX(concertId, editor.duration, clientX),
        ),
      ]
    : [];

  return h.div(
    [h.Class("splitter-timeline"), h.DataAttribute(TIMELINE_DATA_ATTRIBUTE, String(concertId)), ...timelineInteractionAttributes],
    [
      h.div(
        [
          h.Class("splitter-playhead"),
          h.Style(
            Option.match(playheadFraction, {
              onNone: () => ({ display: "none" }),
              onSome: (fraction) => ({ display: "block", left: `${fraction * 100}%` }),
            }),
          ),
        ],
        [],
      ),
      ...Array.map(editor.tracks, (track, index) => segmentView(track, index, editor.duration)),
      ...Array.makeBy(Math.max(0, editor.tracks.length - 1), (boundaryIndex) => gapView(editor, boundaryIndex)),
      ...Array.map(handlesFor(editor), (handle, index) => handleView(editor, handle, index, dragState)),
    ],
  );
};

const previewNoteView = (mediaUrl: Option.Option<string>): ReadonlyArray<Html> => {
  const h = html<Message>();

  const text = Option.isSome(mediaUrl)
    ? "Audio preview unavailable for this file format."
    : "Audio preview unavailable — source file not found.";

  return [h.keyed("p")("preview-note", [h.Class("splitter-note")], [text])];
};

const boundariesView = (editor: EditorState, busy: boolean): Html => {
  const h = html<Message>();

  const boundaryCount = Math.max(0, editor.tracks.length - 1);

  return h.div(
    [h.Class("splitter-boundaries")],
    Array.makeBy(boundaryCount, (boundaryIndex) => {
      const fromTitle = Option.match(Array.get(editor.tracks, boundaryIndex), {
        onNone: () => "",
        onSome: (track) => track.title,
      });
      const toTitle = Option.match(Array.get(editor.tracks, boundaryIndex + 1), {
        onNone: () => "",
        onSome: (track) => track.title,
      });
      const isLinked = Option.getOrElse(Array.get(editor.linked, boundaryIndex), () => true);
      return h.keyed("div")(
        `boundary-${boundaryIndex}`,
        [h.Class("splitter-boundary")],
        [
          h.span([h.Class("splitter-boundary-label")], [`${fromTitle} → ${toTitle}`]),
          h.button(
            [
              h.Class("splitter-detach"),
              h.Type("button"),
              h.Disabled(busy),
              h.OnClick(ToggledBoundary({ boundaryIndex })),
            ],
            [isLinked ? "Detach (add gap)" : "Link (remove gap)"],
          ),
        ],
      );
    }),
  );
};

const auditionButton = (time: number, playable: boolean): Html => {
  const h = html<Message>();

  return h.button(
    [
      h.Class("splitter-play"),
      h.Type("button"),
      h.Title("Play from here"),
      h.AriaLabel("Play from here"),
      h.Disabled(!playable),
      ...(playable ? [h.OnClick(ClickedAudition({ time }))] : []),
    ],
    ["▶"],
  );
};

const timeInput = (trackIndex: number, kind: "Start" | "End", value: number): Html => {
  const h = html<Message>();

  return h.input([
    h.Class("splitter-time"),
    h.Type("text"),
    h.Attribute("inputmode", "decimal"),
    h.Value(formatTimecode(value)),
    h.OnChange((rawValue) => ChangedTimeInput({ trackIndex, kind, rawValue })),
  ]);
};

const PREVIEW_END_LEAD_SECONDS = 3;

const tableRowView = (track: EditorTrack, index: number, playable: boolean): Html => {
  const h = html<Message>();

  return h.keyed("tr")(`row-${index}`, [], [
    h.td([h.Class("splitter-num")], [String(index + 1)]),
    h.td([h.Class("splitter-title")], [track.title]),
    h.td([], [timeInput(index, "Start", track.start), auditionButton(track.start, playable)]),
    h.td(
      [],
      [
        timeInput(index, "End", track.end),
        auditionButton(Math.max(0, track.end - PREVIEW_END_LEAD_SECONDS), playable),
      ],
    ),
  ]);
};

const tableView = (editor: EditorState, playable: boolean): Html => {
  const h = html<Message>();

  return h.table(
    [h.Class("splitter-table")],
    [
      h.thead(
        [],
        [
          h.tr(
            [],
            Array.map(["#", "Track", "Start", "End (▶ auditions last 3s)"], (label) =>
              h.th([], [label]),
            ),
          ),
        ],
      ),
      h.tbody([], Array.map(editor.tracks, (track, index) => tableRowView(track, index, playable))),
    ],
  );
};

const readyView = (model: Model, editor: EditorState, mediaUrl: Option.Option<string>, playable: boolean): Html => {
  const h = html<Message>();

  return h.keyed("div")("ready", [], [
    toolbarView(editor, model.busy, model.status),
    timelineView(model.concertId, editor, model.dragState, playable, model.playheadFraction),
    ...(playable ? [] : previewNoteView(mediaUrl)),
    boundariesView(editor, model.busy),
    tableView(editor, playable),
  ]);
};

export const view = (model: Model): Html => {
  const h = html<Message>();

  return M.value(model.phase).pipe(
    M.withReturnType<Html>(),
    M.tag("Loading", () => h.keyed("p")("loading", [h.Class("splitter-status")], ["Loading…"])),
    M.tag("Empty", () =>
      h.keyed("p")("empty", [h.Class("splitter-status")], [
        "No split points yet — run an automatic split first, then come back to fine-tune them.",
      ]),
    ),
    M.tag("LoadFailed", () =>
      h.keyed("p")("load-failed", [h.Class("splitter-status splitter-status-error")], [
        "Could not load split timestamps.",
      ]),
    ),
    M.tag("Ready", (ready) => readyView(model, ready.editor, ready.mediaUrl, ready.playable)),
    M.exhaustive,
  );
};
