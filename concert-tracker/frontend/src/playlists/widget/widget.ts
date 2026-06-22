import { Effect, Option } from "effect";
import { Runtime } from "foldkit";

import type { Message } from "./message";
import { Flags, Model, PhaseValue } from "./model";
import { ports } from "./port";
import { subscriptions } from "./subscription";
import { update } from "./update";
import { view } from "./view";

// INIT

/** The widget mounts idle (Closed). The host drives it open with the first
 *  `opened` Port message, which carries the target and starts the load. */
export const init: Runtime.ElementInit<Model, Message, Flags> = () => [
  { phase: PhaseValue.Closed(), error: Option.none() },
  [],
];

// PROGRAM

/** Builds the add-to-playlist Foldkit Element, ready to mount with
 *  `Runtime.embed`. Mounted once (into a child of #sidebar-add-section) and
 *  kept for the page lifetime; open/close flow over its Ports. */
export const makeElement = (container: HTMLElement) =>
  Runtime.makeElement({
    Model,
    Flags,
    flags: Effect.succeed({}),
    init,
    update,
    view,
    subscriptions,
    ports,
    container,
  });
