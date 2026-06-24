import { Option, Schema as S } from "effect";
import { ts } from "foldkit/schema";

import type { MediaInfo as MediaInfoJson, PlaybackItemJson, PrepareStatus as PrepareStatusJson } from "../../api/client";

// MODEL
//
// Mirrors ../core.ts's PlaybackState/QueueEntry/ConcertPlaybackState (plain
// TS interfaces, DOM-free) plus the mutable module state player.ts used to
// keep alongside them (queue, nextGroupId, pendingPlay, sidebar-open,
// video-panel-open, pendingSeek, status/error). Wire-shape types from
// api/client (generated from OpenAPI) are hand-rolled as S.Struct mirrors
// here, the same pattern playlists/widget/model.ts uses for AddTarget —
// Effect Schema can't consume the generated `interface` types directly, and
// keeping the Model schema-encodable is what lets it flow through Story's
// harness and (eventually) survive a host Port boundary.

// ── Wire-shape mirrors ──────────────────────────────────────────────────

export const MediaInfo = S.Struct({
  artist: S.String,
  has_next: S.Boolean,
  has_prev: S.Boolean,
  is_video: S.Boolean,
  liked: S.Boolean,
  playable: S.Boolean,
  title: S.String,
  track_index: S.optionalKey(S.NullOr(S.Number)),
  url: S.String,
});
export type MediaInfo = typeof MediaInfo.Type;

export const PrepareStatus = S.Struct({
  download: S.String,
  split: S.String,
  split_queued: S.Boolean,
  tracks_present: S.mutable(S.Array(S.Boolean)),
});
export type PrepareStatus = typeof PrepareStatus.Type;

export const PlaybackItem = S.Struct({
  artist: S.String,
  interlude_index: S.optionalKey(S.NullOr(S.Number)),
  is_video: S.Boolean,
  kind: S.String,
  liked: S.Boolean,
  title: S.String,
  track_index: S.optionalKey(S.NullOr(S.Number)),
  url: S.String,
});
export type PlaybackItem = typeof PlaybackItem.Type;

// Compile-time assignability guards (both directions) against the generated
// openapi types, so a backend field change breaks the build here instead of
// silently desyncing. Mirrors playlists/widget/model.ts's AddTarget guard.
type AssertAssignable<A, _B extends A> = true;
export type _MediaInfoFromJson = AssertAssignable<MediaInfo, MediaInfoJson>;
export type _MediaInfoToJson = AssertAssignable<MediaInfoJson, MediaInfo>;
export type _PrepareStatusFromJson = AssertAssignable<PrepareStatus, PrepareStatusJson>;
export type _PrepareStatusToJson = AssertAssignable<PrepareStatusJson, PrepareStatus>;
export type _PlaybackItemFromJson = AssertAssignable<PlaybackItem, PlaybackItemJson>;
export type _PlaybackItemToJson = AssertAssignable<PlaybackItemJson, PlaybackItem>;

// ── Queue / concert-playback state (mirrors ../core.ts's plain interfaces) ──

export const QueueEntry = S.Struct({
  concertId: S.Number,
  trackIdx: S.Number,
  title: S.String,
  liked: S.Boolean,
  playlistName: S.NullOr(S.String),
  groupId: S.NullOr(S.Number),
});
export type QueueEntry = typeof QueueEntry.Type;

export const ConcertPlaybackState = S.Struct({
  id: S.Number,
  items: S.mutable(S.Array(PlaybackItem)),
  pos: S.Number,
});
export type ConcertPlaybackState = typeof ConcertPlaybackState.Type;

export const Playback = S.Struct({
  concertId: S.NullOr(S.Number),
  trackIdx: S.NullOr(S.Number),
  /** Mirrors updateInfo()'s #player-title/#player-artist text — Model-owned
   *  (rather than read back from the DOM) since the view (commit 2) renders
   *  the bar declaratively from this struct. */
  title: S.String,
  artist: S.String,
  isVideo: S.Boolean,
  watchUrl: S.NullOr(S.String),
  hasNext: S.Boolean,
  hasPrev: S.Boolean,
  liked: S.Boolean,
  /** True once the current track/album has played to its natural end with
   *  nothing to auto-advance to. Distinct from `!isPlaying` — a user-paused
   *  mid-track is NOT idle (see core "playerIdle" semantics in player.ts);
   *  only a true end-of-media counts. Reset to false by every new play. */
  ended: S.Boolean,
  concert: S.OptionFromNullOr(ConcertPlaybackState),
  /** Mirrors updatePlaylistLabel()'s #player-playlist text — set on every
   *  beginPlayback (null for non-playlist sources), not just playlist plays,
   *  matching play()'s unconditional `updatePlaylistLabel(playlistName)` call. */
  playlistLabel: S.NullOr(S.String),
});
export type Playback = typeof Playback.Type;

export const initialPlayback: Playback = {
  concertId: null,
  trackIdx: null,
  title: "",
  artist: "",
  isVideo: false,
  watchUrl: null,
  hasNext: false,
  hasPrev: false,
  liked: false,
  ended: false,
  concert: Option.none(),
  playlistLabel: null,
};

// ── Play targets / sources ──────────────────────────────────────────────
//
// `PlayTarget` is the prepare/poll discriminant (track or whole-album; a
// concert reconstruction item is never prepared — see player.ts's
// playConcertItem, which has no missing-file branch). `PlaySource` is the
// richer discriminant carried on `ReceivedMediaInfo` etc. so update.ts can
// derive the right listen/watch URLs and apply the result to the right
// place (play vs enqueue), covering concert items too.

const TrackTarget = ts("Track", { concertId: S.Number, trackIdx: S.Number });
const AlbumTarget = ts("Album", { concertId: S.Number });
export const PlayTarget = S.Union([TrackTarget, AlbumTarget]);
export type PlayTarget = typeof PlayTarget.Type;
export const PlayTargetValue = { Track: TrackTarget, Album: AlbumTarget };

export const sameTarget = (a: PlayTarget, b: PlayTarget): boolean => {
  switch (a._tag) {
    case "Track":
      return b._tag === "Track" && a.concertId === b.concertId && a.trackIdx === b.trackIdx;
    case "Album":
      return b._tag === "Album" && a.concertId === b.concertId;
  }
};

const TrackSource = ts("Track", { concertId: S.Number, trackIdx: S.Number });
const AlbumSource = ts("Album", { concertId: S.Number });
const ConcertItemSource = ts("ConcertItem", {
  concertId: S.Number,
  trackIdx: S.NullOr(S.Number),
  isInterlude: S.Boolean,
});
export const PlaySource = S.Union([TrackSource, AlbumSource, ConcertItemSource]);
export type PlaySource = typeof PlaySource.Type;
export const PlaySourceValue = { Track: TrackSource, Album: AlbumSource, ConcertItem: ConcertItemSource };

/** Options threaded alongside a fetched MediaInfo into the "begin playback"
 *  decision — everything `play()` needed beyond the wire fields and the
 *  listen/watch URLs (which are derived from `PlaySource`, see
 *  ./update.ts's `listenUrlFor`/`watchUrlFor`). `openVideoPanel` is not part
 *  of player.ts's `play()` signature — it captures watchTrackDirect()'s
 *  extra `showVideoPanel()` call after a successful start, since that
 *  decision has to travel with the fetch result rather than being inferred
 *  from `info.is_video` alone (every video play keeps the panel's *prior*
 *  state; only watchTrackDirect forces it open). */
export const PlayOpts = S.Struct({
  recordListen: S.Boolean,
  playlistName: S.NullOr(S.String),
  openVideoPanel: S.Boolean,
});
export type PlayOpts = typeof PlayOpts.Type;
export const defaultPlayOpts: PlayOpts = { recordListen: true, playlistName: null, openVideoPanel: false };

// ── Queue-drain plan ─────────────────────────────────────────────────────
//
// playFromQueue() in player.ts is called from four sites that each want a
// different fallback when the queue is empty (or every queued track turns
// out unplayable): playPlaylist tries the queue only; skipToNext falls
// through to the next set-list track; advanceAfterDelete additionally stops
// playback if that also fails; advanceOrCollapse (onEnded/onError)
// additionally collapses the video panel. One tag captures all four so the
// queue-drain Command/Message pair (see ./command.ts, ./update.ts) doesn't
// need a bespoke shape per call site.
export const AdvancePlan = S.Literals(["queue-only", "next-or-none", "next-or-stop", "next-or-collapse"]);
export type AdvancePlan = typeof AdvancePlan.Type;

// ── Status (collapses player.ts's separate #player-status / #player-error
//    text elements into one union, per the EL's design-review note) ───────

const StatusIdle = ts("Idle");
const StatusBusy = ts("Busy", { message: S.String });
const StatusError = ts("Error", { message: S.String });
export const Status = S.Union([StatusIdle, StatusBusy, StatusError]);
export type Status = typeof Status.Type;
export const StatusValue = { Idle: StatusIdle, Busy: StatusBusy, Error: StatusError };

// ── Model ────────────────────────────────────────────────────────────────

export const Model = S.Struct({
  playback: Playback,
  queue: S.Array(QueueEntry),
  /** Monotonic id minted per playPlaylist call so its queued tracks form one
   *  visually-grouped, separately-removable block (see ../core.ts's
   *  buildQueueRows/removeGroup). Lifetime-only, never persisted. */
  nextGroupId: S.Number,
  /** The track/album currently in the prepare→poll cycle, if any (mirrors
   *  player.ts's module-scoped `pendingPlay`). */
  pending: S.Option(PlayTarget),
  sidebar: S.Struct({ open: S.Boolean }),
  video: S.Struct({ open: S.Boolean }),
  /** Mirrors the real `<audio>` element's play/pause state, driven by the
   *  AudioPlaying/AudioPaused/AudioEnded messages a later commit's
   *  Subscription will dispatch from the element's real events — never set
   *  optimistically by a user-action message (see update.ts's TogglePause). */
  isPlaying: S.Boolean,
  /** A seek requested before the audio element had metadata loaded (e.g. a
   *  splitter preview click); mirrors player.ts's `seekWhenReady`. Applied
   *  by a later commit's AudioLoadedMetadata handling. */
  pendingSeek: S.Option(S.Number),
  status: Status,
});
export type Model = typeof Model.Type;

export const initialModel: Model = {
  playback: initialPlayback,
  queue: [],
  nextGroupId: 1,
  pending: Option.none(),
  sidebar: { open: false },
  video: { open: false },
  isPlaying: false,
  pendingSeek: Option.none(),
  status: StatusValue.Idle(),
};

/** The widget mounts with no flags, mirroring player.ts's module-load-time
 *  `init()` — there is nothing to seed from the host at construction time;
 *  all state arrives via the inbound command Port (see ./port.ts). */
export const Flags = S.Struct({});
export type Flags = typeof Flags.Type;
