import { Port, Subscription } from "foldkit";

import { CloseRequested, EnteredNewName, OpenRequested, type Message } from "./message";
import type { Model } from "./model";
import { ports } from "./port";

// SUBSCRIPTION
//
// The panel has no streams of its own (unlike the splitter's pointer drag) —
// it is driven entirely by the host through the inbound Ports.

export const subscriptions = Subscription.make<Model, Message>()(() => ({
  opened: Port.subscription(ports.inbound.opened, (target) => OpenRequested({ target })),
  closed: Port.subscription(ports.inbound.closed, () => CloseRequested()),
  newName: Port.subscription(ports.inbound.newName, (name) => EnteredNewName({ name })),
}));
