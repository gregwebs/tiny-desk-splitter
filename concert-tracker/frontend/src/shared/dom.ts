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
export function byId<T extends Element = HTMLElement>(id: string): T {
  const el = document.getElementById(id);
  if (!el) throw new Error(`expected #${id} to exist`);
  return el as unknown as T;
}

/**
 * Look up an element that may legitimately be absent (e.g. only rendered on
 * some pages/states). Callers handle the null case explicitly.
 */
export function byIdOrNull<T extends Element = HTMLElement>(id: string): T | null {
  return document.getElementById(id) as unknown as T | null;
}
