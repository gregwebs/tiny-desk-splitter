import { Effect, Option } from "effect";
import { Runtime } from "foldkit";

import { FetchSplitterData } from "./command";
import type { Message } from "./message";
import { DragStateValue, Flags, Model, PhaseValue, StatusValue } from "./model";
import { ports } from "./port";
import { subscriptions } from "./subscription";
import { update } from "./update";
import { view } from "./view";

// INIT

export const init: Runtime.ElementInit<Model, Message, Flags> = (flags) => [
  {
    concertId: flags.concertId,
    phase: PhaseValue.Loading(),
    busy: false,
    status: StatusValue.NoStatus(),
    dragState: DragStateValue.NotDragging(),
    playheadFraction: Option.none(),
  },
  [FetchSplitterData({ concertId: flags.concertId })],
];

// PROGRAM

/** Builds a Foldkit Element for the splitter, ready to mount with
 *  `Runtime.embed`. `flags.concertId` selects which concert's split data to
 *  load. */
export const makeElement = (container: HTMLElement, flags: Flags) =>
  Runtime.makeElement({
    Model,
    Flags,
    flags: Effect.succeed(flags),
    init,
    update,
    view,
    subscriptions,
    ports,
    container,
  });
