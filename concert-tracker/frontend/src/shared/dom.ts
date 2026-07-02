// Small typed DOM lookup helpers. Under strict mode, getElementById/querySelector
// return `T | null`; these centralize the two ways the codebase deals with that
// instead of repeating ad hoc null checks at every call site (~50 across the
// ported modules).

/**
 * Look up an element that `layout.html`/the relevant template guarantees
 * exists (e.g. fixed chrome like #player-audio). Throws loudly instead of
 * silently no-op'ing if the assumption is ever violated — that's a markup
 * bug, not a runtime condition to swallow.
 */
export function byId(id: string): HTMLElement {
  const el = document.getElementById(id);
  if (!el) throw new Error(`expected #${id} to exist`);
  return el;
}

/**
 * Look up an element that may legitimately be absent (e.g. only rendered on
 * some pages/states). Callers handle the null case explicitly.
 */
export function byIdOrNull(id: string): HTMLElement | null {
  return document.getElementById(id);
}

/**
 * Look up an element expected to exist (see `byId`) and be of type `T`,
 * verified with `instanceof` rather than trusted via a generic type
 * parameter. Throws loudly on either a missing id or the wrong element type.
 */
export function byIdOf<T extends Element>(id: string, ctor: abstract new (...args: never[]) => T): T {
  const el = byId(id);
  if (!(el instanceof ctor)) throw new Error(`expected #${id} to be a ${ctor.name}`);
  return el;
}

/**
 * Look up an element that may legitimately be absent (see `byIdOrNull`) and,
 * if present, is of type `T`, verified with `instanceof`. Throws if the
 * element exists but is the wrong type — that's a markup bug, not an
 * absence to swallow.
 */
export function byIdOfOrNull<T extends Element>(
  id: string,
  ctor: abstract new (...args: never[]) => T,
): T | null {
  const el = byIdOrNull(id);
  if (el === null) return null;
  if (!(el instanceof ctor)) throw new Error(`expected #${id} to be a ${ctor.name}`);
  return el;
}
