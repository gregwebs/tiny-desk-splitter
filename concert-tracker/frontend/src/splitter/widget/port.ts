import { Schema as S } from "effect";
import { Port } from "foldkit";

// PORT

/** Sentinel for "no playhead" on the inbound `playhead` Port. A plain number
 *  keeps the host's send-site (plain imperative TypeScript, not an Effect
 *  Schema-encoded value) simple; `subscription.ts` converts it to
 *  `Option<number>` immediately, so nothing past that boundary sees the
 *  sentinel. */
export const PLAYHEAD_HIDDEN = -1;

export const ports = {
  inbound: {
    /** Playhead position as a 0–1 fraction of `editor.duration`, pushed by
     *  the host from `#player-audio`'s `timeupdate` + `window.Player`. */
    playhead: Port.inbound(S.Number),
  },
  outbound: {
    /** Ask the host to preview playback at this many seconds into the
     *  concert (`window.Player.playAlbumAt`). */
    auditionAt: Port.outbound(S.Number),
    /** A split/reset job was queued; ask the host to refresh the concert
     *  card so its in-progress badge and status polling pick it up. */
    cardDirty: Port.outbound(S.Void),
  },
};
