import { Effect } from "effect";
import { Runtime } from "foldkit";

import { LoadSidebarWidthCmd } from "./command";
import type { Message } from "./message";
import { Flags, initialModel, Model } from "./model";
import { ports } from "./port";
import { subscriptions } from "./subscription";
import { update } from "./update";
import { view } from "./view";

// WIDGET

export const init: Runtime.ElementInit<Model, Message, Flags> = () => [
  initialModel,
  [LoadSidebarWidthCmd({})],
];

/** Build a Foldkit Element for the player widget, ready to mount with
 *  `Runtime.embed`. The widget owns the full player bar + sidebar; its
 *  host (`player/index.ts`) exposes the `window.Player` shim. */
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
