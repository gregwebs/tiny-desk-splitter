import { Option, Schema as S } from "effect";
import { ts } from "foldkit/schema";

import type { MediaInfo as MediaInfoJson, PlaybackItemJson, PrepareStatus as PrepareStatusJson, TrackDetailItem as TrackDetailItemJson } from "../../api/client";

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

// ── Sidebar track list (GET /concerts/:id/track-details response) ───────

export const SidebarTrack = S.Struct({
  index: S.Number,
  title: S.String,
  available: S.Boolean,
  is_video: S.Boolean,
  liked: S.Boolean,
});
export type SidebarTrack = typeof SidebarTrack.Type;

export const SidebarTrackList = S.Struct({
  tracksBusy: S.Boolean,
  tracks: S.mutable(S.Array(SidebarTrack)),
});
export type SidebarTrackList = typeof SidebarTrackList.Type;

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
export type _SidebarTrackFromJson = AssertAssignable<SidebarTrack, TrackDetailItemJson>;
export type _SidebarTrackToJson = AssertAssignable<TrackDetailItemJson, SidebarTrack>;
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
// `PlayTarget` is the prepare/poll discriminant. Only tracks ever enter the
// prepare/poll flow: startAlbum has no prepare fallback (whole-album fetches
// are always either playable or a hard failure), and a concert reconstruction
// item is never prepared — see player.ts's playConcertItem, which has no
// missing-file branch. `PlaySource` is the richer discriminant carried on
// `SucceededMediaInfo` etc. so update.ts can derive the right listen/watch
// URLs and apply the result to the right place (play vs enqueue), covering
// concert items and whole-album plays too.

const TrackTarget = ts("Track", { concertId: S.Number, trackIdx: S.Number });
export const PlayTarget = TrackTarget;
export type PlayTarget = typeof PlayTarget.Type;
export const PlayTargetValue = { Track: TrackTarget };

export const sameTarget = (a: PlayTarget, b: PlayTarget): boolean =>
  a.concertId === b.concertId && a.trackIdx === b.trackIdx;

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
  sidebar: S.Struct({
    open: S.Boolean,
    /** Loaded once when the sidebar opens for whole-album / normal mode;
     *  None in reconstruction mode (items come from playback.concert instead).
     *  Cleared on stopPlayback; refreshed by FetchTrackDetails (commit 5 wires
     *  the dispatch). */
    tracks: S.Option(SidebarTrackList),
    /** Monotonic staleness guard: incremented each time FetchTrackDetails is
     *  dispatched so a stale response arriving after a newer fetch is discarded. */
    loadGen: S.Number,
  }),
  video: S.Struct({ open: S.Boolean }),
  /** Projects the real `<audio>` element's play/pause state. Native media
   *  events drive successful immediate transitions; failure and terminal
   *  paths may defensively reset it false. User toggle messages never set it
   *  optimistically: ToggleAudio reads live media state; see docs/player.md. */
  isPlaying: S.Boolean,
  /** A seek requested before the audio element had metadata loaded (e.g. a
   *  splitter preview click); mirrors player.ts's `seekWhenReady`. Applied
   *  by a later commit's AudioLoadedMetadata handling. */
  pendingSeek: S.Option(S.Number),
  status: Status,
  /** Mirrors player.ts's onTimeUpdate, driven by the audioEvents
   *  Subscription's timeupdate/loadedmetadata listeners (see
   *  UpdatedAudioTime). Zeroed by beginPlayback/stopPlaybackPure so a new or
   *  stopped track never shows the previous track's stale duration before
   *  its own loadedmetadata arrives. */
  audioTime: S.Struct({ currentTime: S.Number, duration: S.Number }),
  /** Monotonic generation counter, incremented by every beginPlayback/
   *  stopPlaybackPure alongside the audioTime reset. update.ts's
   *  UpdatedAudioTime handler compares the message's `loadGen` against this
   *  field and discards a mismatch — the guard against a stray
   *  timeupdate/loadedmetadata from a track that's no longer current.
   *
   *  A model-only counter (bumped when the *message* was processed, checked
   *  against when a later message was *received*) isn't sufficient by
   *  itself: `PlayAudio`'s `audio.src = url` reassignment happens later, in
   *  its own forked Command Effect, so a stray event from the *previous*
   *  (still-loaded) resource could fire and get processed in the window
   *  before that reassignment lands — indistinguishable from a legitimate
   *  event of the new track if the comparison were against a value the
   *  model alone tracks on both ends. So the generation isn't just kept
   *  model-side: PlayAudio (command.ts) stamps this exact value onto the
   *  audio element's own `dataset.audioLoadGen`, in the same synchronous
   *  statement as `audio.src = url`. audioTimeMessage (subscription.ts)
   *  reads that DOM-stamped value back, live, for every event — so the
   *  message's `loadGen` always reflects which resource the element is
   *  *actually* playing at read time, not merely which beginPlayback call
   *  most recently ran. That's what makes the reducer's comparison correct
   *  even for same-URL replays or Subscription-timing edge cases, where a
   *  purely model/Subscription-side generation could still be fooled. */
  audioLoadGen: S.Number,
});
export type Model = typeof Model.Type;

export const initialAudioTime = { currentTime: 0, duration: 0 };

export const initialModel: Model = {
  playback: initialPlayback,
  queue: [],
  nextGroupId: 1,
  pending: Option.none(),
  sidebar: { open: false, tracks: Option.none(), loadGen: 0 },
  video: { open: false },
  isPlaying: false,
  pendingSeek: Option.none(),
  status: StatusValue.Idle(),
  audioTime: initialAudioTime,
  audioLoadGen: 0,
};

/** The widget mounts with no flags, mirroring player.ts's module-load-time
 *  `init()` — there is nothing to seed from the host at construction time;
 *  all state arrives via the inbound command Port (see ./port.ts). */
export const Flags = S.Struct({});
export type Flags = typeof Flags.Type;
