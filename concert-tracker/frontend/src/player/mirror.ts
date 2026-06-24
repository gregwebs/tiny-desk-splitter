// A synchronous escape hatch for window.Player.nowPlaying(). splitter/index.ts
// reads it from inside a `timeupdate` handler to draw the splitter's
// playhead — it cannot await a Foldkit runtime round-trip just to know what's
// playing. The widget's update.ts (widget/update.ts) writes this mirror via a
// SyncNowPlayingMirror Command appended to every branch that changes
// playback identity (see withPlayback in widget/update.ts); nothing else
// should call `setNowPlaying`.
import type { PlayerNowPlaying } from "../shared/player-api";

let current: PlayerNowPlaying = { concertId: null, trackIdx: null };

export function setNowPlaying(next: PlayerNowPlaying): void {
  current = next;
}

export function nowPlaying(): PlayerNowPlaying {
  return current;
}
