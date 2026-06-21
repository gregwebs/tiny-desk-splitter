import { Effect, Option, Schema as S, Stream } from "effect";
import { Port, Subscription } from "foldkit";

import { ChangedPlayhead, MovedDragPointer, ReleasedDragPointer, type Message } from "./message";
import type { Model } from "./model";
import { ports, PLAYHEAD_HIDDEN } from "./port";
import { timeFromClientX, timelineElement } from "./timeline";

// SUBSCRIPTION

const DragActivity = S.Literals(["Idle", "Active"]);

const dragActivityFromModel = (model: Model): typeof DragActivity.Type =>
  model.dragState._tag === "Dragging" ? "Active" : "Idle";

const durationFromModel = (model: Model): number =>
  model.phase._tag === "Ready" ? model.phase.editor.duration : 0;

export const subscriptions = Subscription.make<Model, Message>()((entry) => ({
  dragPointer: entry(
    { dragActivity: DragActivity, concertId: S.Number, duration: S.Number },
    {
      modelToDependencies: (model) => ({
        dragActivity: dragActivityFromModel(model),
        concertId: model.concertId,
        duration: durationFromModel(model),
      }),
      dependenciesToStream: ({ dragActivity, concertId, duration }) => {
        const movedOrReleased = Stream.merge(
          Stream.fromEventListener<PointerEvent>(document, "pointermove").pipe(
            Stream.mapEffect((event) =>
              Effect.sync(() =>
                Option.map(timelineElement(concertId), (element) =>
                  MovedDragPointer({ time: timeFromClientX(event.clientX, element, duration) }),
                ),
              ),
            ),
            Stream.filter(Option.isSome),
            Stream.map((option) => option.value),
          ),
          Stream.merge(
            Stream.fromEventListener<PointerEvent>(document, "pointerup"),
            Stream.fromEventListener<PointerEvent>(document, "pointercancel"),
          ).pipe(Stream.map(() => ReleasedDragPointer())),
        );

        // Mirrors @foldkit/ui's Slider: prevents text selection and locks the
        // cursor to grabbing for the duration of the drag.
        const documentDragStyles = Stream.callback<never>(() =>
          Effect.acquireRelease(
            Effect.sync(() => {
              document.documentElement.style.setProperty("user-select", "none");
              const cursorStyle = document.createElement("style");
              cursorStyle.textContent = "* { cursor: ew-resize !important; }";
              document.head.appendChild(cursorStyle);
              return cursorStyle;
            }),
            (cursorStyle) =>
              Effect.sync(() => {
                document.documentElement.style.removeProperty("user-select");
                cursorStyle.remove();
              }),
          ).pipe(Effect.flatMap(() => Effect.never)),
        );

        return Stream.when(
          Stream.merge(movedOrReleased, documentDragStyles),
          Effect.sync(() => dragActivity === "Active"),
        );
      },
    },
  ),

  playhead: Port.subscription(ports.inbound.playhead, (raw) =>
    ChangedPlayhead({ fraction: raw === PLAYHEAD_HIDDEN ? Option.none() : Option.some(raw) }),
  ),
}));
