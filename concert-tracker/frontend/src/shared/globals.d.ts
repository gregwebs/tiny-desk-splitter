// Window augmentation shared by all three entry points. Each entry module
// (player.ts, playlists.ts, splitter/index.ts) augments this further with its
// own global (Window.Player / Window.Playlists / Window.Splitter) right next
// to the code that assigns it, since that's the single source of truth for
// each module's public API surface.
import type { HtmxApi } from "./htmx";

declare global {
  interface Window {
    /** Vendored htmx 2.x, loaded before player/playlists/splitter (see layout.html). */
    htmx?: HtmxApi;
  }
}

// Required for `declare global` to attach to this file's scope instead of
// being parsed as a script (ambient) file — see TS handbook on global
// augmentation in modules.
export {};
