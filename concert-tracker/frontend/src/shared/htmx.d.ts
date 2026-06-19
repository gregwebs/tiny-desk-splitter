// Minimal ambient typing for the subset of the vendored htmx 2.x global API
// (concert-tracker/static/htmx.min.js, untouched by this conversion) that the
// ported modules call. Deliberately not @types/htmx.org: that package targets
// a specific htmx version and pulls in a much larger surface than we use.
export interface HtmxAjaxOptions {
  target?: string | Element;
  source?: string | Element;
  select?: string;
  swap?: string;
  values?: Record<string, unknown>;
  headers?: Record<string, string>;
}

export interface HtmxApi {
  /** Re-scans an element for hx-* attributes (used after manual innerHTML swaps). */
  process(elt: Element): void;
  /** Issues an htmx-managed request and swaps the result into the DOM. */
  ajax(verb: string, path: string, options?: HtmxAjaxOptions): Promise<void>;
}
