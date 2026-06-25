import { Schema as S } from "effect";
import { m } from "foldkit/message";

import {
  AdvancePlan,
  MediaInfo,
  PlaybackItem,
  PlaySource,
  PlayTarget,
  PlayOpts,
  PrepareStatus,
  QueueEntry,
  SidebarTrack,
} from "./model";
import { PlayerCommand } from "./port";

// MESSAGE
//
// Scope decision (commit 1 of the player port — see the engineering-lead's
// project_player_foldkit_port memory and the 8-commit plan): this Message set
// covers the FULL decision-logic state machine for all 28 PlayerApi actions
// (everything in ../shared/player-api.ts except nowPlaying, which stays a
// synchronous ../mirror.ts read) — play/start/playTrack(s) routing,
// enqueue/dequeue/drain-the-queue, prepare→poll, like/delete (bar + sidebar),
// concert reconstruction navigation (playConcertItem/advanceConcert/
// playConcert/playConcertFrom/sidebarDeleteInterlude — the two advance paths
// the EL flagged as highest bug density), and the audio element's
// play/pause/ended/error events (AudioPlaying..AudioPlayRejected below).
//
// Deliberately OUT of scope for this commit (no decision content to port —
// pure DOM mechanics that later commits' Subscriptions will dispatch
// directly, or pure view output with no branching of its own):
//   - sidebar resize-drag + width persistence (mouse/localStorage only)
//   - video-panel idle-timer / outside-click dismissal (timers/click only)
//   - keyboard shortcuts (key→togglePause/Escape dispatch only)
//   - the old HTML-fragment sidebar track list (fetchSidebarTracks) — superseded
//     by the JSON sidebar-tracks endpoint + Foldkit-owned list, commits 4-5
//   - every player.ts `update*`/`render*`/`set*` DOM-mutation function
//     (updateInfo, updateLikeStar, setPlayPauseIcon, renderQueue, ...) — these
//     become declarative view output from Model in commits 2/5/6, not Commands
// The audio element itself and a handful of external (non-widget-owned) DOM
// nodes — the card list's playing/preparing marks, like-button copies outside
// the sidebar — remain real imperative Commands (see ./command.ts) since
// nothing in the widget's own vdom can reach them.

// Host calls a window.Player method → the shim builds a PlayerCommand and
// emits it through the single inbound `command` Port (see ./port.ts);
// update.ts dispatches on `command._tag` via Match.tagsExhaustive.
export const CommandReceived = m("CommandReceived", { command: PlayerCommand });

// ── Media info / play results ───────────────────────────────────────────

/** Fetched MediaInfo (or a concert item, which never fetches but shares this
 *  shape) was playable: the central "begin playback" transition, shared by
 *  every play path (startAlbum/startTrack/playAlbumAt/playFromQueue/
 *  playNextTrack/playPrevTrack/playConcertItem/playConcert's source branch).
 *  Mirrors player.ts's `play()`. */
export const ReceivedMediaInfo = m("ReceivedMediaInfo", { source: PlaySource, info: MediaInfo, opts: PlayOpts });
/** Fetched info exists but isn't browser-playable: falls back to opening the
 *  file URL directly (mirrors `window.open(info.url, "_blank")`). */
export const NotPlayable = m("NotPlayable", { source: PlaySource, url: S.String });
/** getTrackMediaInfoOrNull returned null: the track file doesn't exist yet —
 *  enter the prepare/poll flow. Track sources only (startAlbum has no
 *  prepare fallback; a concert item is never missing by definition). */
export const TrackMissing = m("TrackMissing", { source: PlaySource });
/** Generic fetch failure (album info, next/prev track info, playlist load,
 *  concert playback fetch). `message` is the user-facing Status.Error text;
 *  callers supply it via the Command since it varies ("Couldn't load next
 *  track" vs "Couldn't start concert" etc), matching player.ts's per-call-site
 *  showError() text. */
export const FailedFetchInfo = m("FailedFetchInfo", { source: PlaySource, message: S.String });

/** trackMediaInfo() result for the playTrack "something else is already
 *  playing" branch — enqueue, don't play. `info` is None when the track file
 *  doesn't exist (still enters the prepare flow, same as the play path). */
export const ReceivedTrackInfoForEnqueue = m("ReceivedTrackInfoForEnqueue", {
  concertId: S.Number,
  trackIdx: S.Number,
  info: S.Option(S.Struct({ title: S.String, liked: S.Boolean })),
});

/** firstAvailableTrackIndex() result for playTracks. None when no track in
 *  the concert is playable at all (enters the prepare flow via track 0). */
export const ResolvedFirstAvailableTrack = m("ResolvedFirstAvailableTrack", {
  concertId: S.Number,
  trackIdx: S.Option(S.Number),
});

// ── Queue drain (playFromQueue) ─────────────────────────────────────────

/** DrainQueue tried queue entries front-to-back until one was playable (or
 *  exhausted the queue). `skippedCount` lets update.ts trim exactly that many
 *  unplayable entries off the front of the *current* queue (which may have
 *  grown via a concurrent Enqueue while the drain Command was in flight —
 *  trimming a count rather than diffing by identity keeps that race benign).
 *  `plan` carries what to do next when nothing played (see model.ts's
 *  AdvancePlan doc comment). */
export const ReceivedQueueDrainResult = m("ReceivedQueueDrainResult", {
  played: S.Option(S.Struct({ entry: QueueEntry, info: MediaInfo })),
  skippedCount: S.Number,
  plan: AdvancePlan,
});

/** playNextTrack()/playPrevTrack() found nothing to advance to (no next/prev
 *  media-info, a fetch error, or an aborted auto-advance fetch). Always sets
 *  isPlaying false (mirrors `setPlayPauseIcon(false)` in both functions'
 *  catch blocks) before plan-specific fallback (next-or-stop /
 *  next-or-collapse / next-or-none). */
export const FailedNextTrackInfo = m("FailedNextTrackInfo", { plan: AdvancePlan });
export const FailedPrevTrackInfo = m("FailedPrevTrackInfo");

// ── Prepare / poll ───────────────────────────────────────────────────────

export const ReceivedPrepareStart = m("ReceivedPrepareStart", {
  target: PlayTarget,
  seedStatus: S.Option(PrepareStatus),
});
export const FailedPrepareStart = m("FailedPrepareStart", { target: PlayTarget });
/** One prepare-status payload (the POST /prepare response seeds the first one
 *  without waiting a full poll interval; each subsequent GET
 *  /prepare-status produces another). `elapsedMs` is the deterministic
 *  poll-clock surrogate — update.ts compares it to PREPARE_TIMEOUT_MS rather
 *  than reading Date.now(), so the timeout branch is exercised by a plain
 *  Story.message rather than a real timer. */
export const ReceivedPrepareStatus = m("ReceivedPrepareStatus", {
  target: PlayTarget,
  status: PrepareStatus,
  elapsedMs: S.Number,
});
export const FailedPollPrepareStatus = m("FailedPollPrepareStatus", { target: PlayTarget, elapsedMs: S.Number });

// ── Like / delete ────────────────────────────────────────────────────────

export const CompletedLikeToggle = m("CompletedLikeToggle", {
  concertId: S.Number,
  trackIdx: S.Number,
  liked: S.Boolean,
});
/** `attempted` is the optimistic value applied before the POST; on failure
 *  update.ts reverts to `!attempted` only if playback hasn't moved off this
 *  track meanwhile (mirrors player.ts's toggleLike catch guard). */
export const FailedLikeToggle = m("FailedLikeToggle", {
  concertId: S.Number,
  trackIdx: S.Number,
  attempted: S.Boolean,
});

/** Shared by the player-bar Delete button and the sidebar trash button
 *  (postDeleteTrackRequest in player.ts); `source` tells update.ts which
 *  advance behavior to run on success — advanceAfterDelete (non-concert-aware
 *  fallback) for the bar, or the concert-aware playConcertPosOrEnd /
 *  refreshConcertItems dance for the sidebar. See model.ts's AdvancePlan doc
 *  and the engineering-lead's project_concert_reconstruction memory. */
export const ReceivedDeleteTrackResult = m("ReceivedDeleteTrackResult", {
  concertId: S.Number,
  trackIdx: S.Number,
  ok: S.Boolean,
  source: S.Literals(["bar", "sidebar"]),
});

// ── Concert reconstruction ──────────────────────────────────────────────

/** refreshConcertItems() — re-fetches the item list after a sidebar delete so
 *  advanceConcert/playConcertPosOrEnd navigate against current positions
 *  (refindPosByUrl in ../core.ts re-finds `pos` by the currently-playing
 *  item's URL). A no-op (FailedConcertItems is just logged, not
 *  user-visible) when the fetch fails — mirrors the original's bare catch.
 *  `advanceAfter` carries sidebarDeleteTrack's/sidebarDeleteInterlude's
 *  `wasPlayingThis` through the refresh round trip: true only when the
 *  deleted item was the one currently playing, in which case update.ts
 *  plays the refreshed `pos` (mirrors playConcertPosOrEnd()); false just
 *  updates nav state silently (some other item in the list was deleted). */
export const ReceivedConcertItems = m("ReceivedConcertItems", {
  concertId: S.Number,
  items: S.mutable(S.Array(PlaybackItem)),
  advanceAfter: S.Boolean,
});
export const FailedConcertItems = m("FailedConcertItems", { concertId: S.Number });

/** playConcert()/playConcertFrom()'s reconstruction-mode branch (the
 *  source-present branch reuses ReceivedMediaInfo/NotPlayable with a PlaySource
 *  Album tag — see ./command.ts's FetchConcertPlayback, which mirrors the
 *  exact branching `isSourcePlayback(data)` does in player.ts). `atPos` is 0
 *  for playConcert, or the requested position for playConcertFrom starting
 *  fresh (not already in concert mode for this id). */
export const ReceivedConcertPlaybackItems = m("ReceivedConcertPlaybackItems", {
  concertId: S.Number,
  items: S.mutable(S.Array(PlaybackItem)),
  atPos: S.Number,
});
/** Covers both the empty-items case ("Nothing to play") and a fetch error
 *  ("Couldn't start/load concert"); `message` carries the right text since it
 *  differs by call site (playConcert vs playConcertFrom). */
export const FailedConcertPlayback = m("FailedConcertPlayback", { concertId: S.Number, message: S.String });

export const CompletedDeleteInterlude = m("CompletedDeleteInterlude", {
  concertId: S.Number,
  interludeIdx: S.Number,
  /** Was this interlude the one currently playing? Carried through so the
   *  success handler can play the next item at the now-refreshed position
   *  (mirrors player.ts's `wasPlayingThis` local, captured before the
   *  DELETE). */
  wasPlayingThis: S.Boolean,
});
export const FailedDeleteInterlude = m("FailedDeleteInterlude", { concertId: S.Number, interludeIdx: S.Number });

// ── Playlists ────────────────────────────────────────────────────────────

const PlaylistTrack = S.Struct({ concertId: S.Number, trackIdx: S.Number, title: S.String });
export const ReceivedPlaylistTracks = m("ReceivedPlaylistTracks", {
  playlistId: S.Number,
  name: S.String,
  tracks: S.Array(PlaylistTrack),
});
export const FailedPlaylistLoad = m("FailedPlaylistLoad", { playlistId: S.Number });

// ── Sidebar track details (GET /concerts/:id/track-details) ──────────────

/** FetchTrackDetails resolved successfully; `loadGen` is compared against
 *  `model.sidebar.loadGen` in update.ts to discard stale responses. */
export const ReceivedTrackDetails = m("ReceivedTrackDetails", {
  concertId: S.Number,
  loadGen: S.Number,
  tracksBusy: S.Boolean,
  tracks: S.mutable(S.Array(SidebarTrack)),
});
/** FetchTrackDetails fetch error; sidebar stays at Option.none() — not
 *  user-visible (the sidebar just shows an empty list). */
export const FailedTrackDetails = m("FailedTrackDetails", {
  concertId: S.Number,
  loadGen: S.Number,
});

// ── Audio element events ────────────────────────────────────────────────
//
// No Subscription dispatches these yet (that's a later commit, once the
// widget actually owns/wires the <audio> element) — they're included now so
// the decision logic they drive (advanceOrCollapse, the play/pause icon
// mirror, the "Playback blocked" error) can be ported and Story-tested
// alongside everything else, per the EL's request to review update.ts in
// isolation before any DOM wiring exists.

/** openExternal()'s postEvent(state.watchUrl) failure — the one fire-and-forget
 *  POST in player.ts that DOES show an error on failure ("Couldn't open
 *  externally"), unlike the listen-event POSTs in play()/playFromQueue/etc
 *  (those use a bare `.catch(() => {})` and are covered by Acked below). */
export const FailedOpenExternal = m("FailedOpenExternal");

export const AudioPlaying = m("AudioPlaying");
export const AudioPaused = m("AudioPaused");
/** Natural end of the current track/album: advanceOrCollapse() — try the
 *  queue, then the next set-list track, else collapse the video panel.
 *  Concert mode (state.concert set) advances within the concert item list
 *  first instead; see update.ts. */
export const AudioEnded = m("AudioEnded");
/** A load/playback error fired on the element itself (not a rejected
 *  `.play()` call — see AudioPlayRejected for that). Shows "Failed to load
 *  media" then runs the same advanceOrCollapse as AudioEnded. */
export const AudioErrored = m("AudioErrored");
/** `audio.play()` rejected (autoplay policy, etc.) — from play(),
 *  togglePause(), or playAlbumAt(). Model fields for the track were already
 *  applied before the play() call in every case, so this never reverts
 *  them — it only sets isPlaying false and shows "Playback blocked". */
export const AudioPlayRejected = m("AudioPlayRejected");

// ── Subscription-dispatched messages (commit 7) ─────────────────────────
//
// Dispatched by subscription.ts (not via the command Port); they drive purely
// declarative decisions in update.ts with no special host-coupling needed.

/** htmx:afterSettle / htmx:historyRestore fired: re-stamp the playing/
 *  preparing CSS markers on the card DOM (mirrors player.ts's
 *  reassertPlayerUi). Always a no-op on model, never changes playback. */
export const ReassertUi = m("ReassertUi");

/** htmx:afterSwap on a like button outside the widget (the card-side hx-post
 *  star) — sync the model-owned liked state so bar star + sidebar list stay
 *  in sync without a separate re-fetch. */
export const SyncLikeFromSwap = m("SyncLikeFromSwap", {
  concertId: S.Number,
  trackIdx: S.Number,
  liked: S.Boolean,
});

/** document keydown with no modifier + Space, on a non-editable, non-bar
 *  target — toggle pause. */
export const PressedSpace = m("PressedSpace");

/** document keydown with no modifier + Escape, on a non-editable target —
 *  close the video panel if open. */
export const PressedEscape = m("PressedEscape");

/** Click on document that falls outside #player-video-panel (only active
 *  while video.open is true — gated in subscription.ts). */
export const ClickedOutsideVideo = m("ClickedOutsideVideo");

// ── Command acks ─────────────────────────────────────────────────────────

/** Shared by every Command whose result update.ts ignores: the external-DOM
 *  marker/sync Commands (Mark/ClearPlayingExternal, Mark/ClearPreparingExternal,
 *  DisableCardTracksExternal, SyncLikeButtonsExternal), the audio
 *  pause/seek/clear-src Commands, navigation/window.open, the
 *  add-to-playlist host call, the listen-event POST, and the now-playing
 *  mirror sync. Distinct per-Command acks (the playlists/widget pattern)
 *  would add ~15 identical no-op cases to update.ts for no benefit here,
 *  since none of these Commands can fail in a way the user needs to see
 *  (PlayAudio/ResumeAudio are the one exception that DOES need a distinct
 *  outcome — see AudioPlayRejected above). */
export const Acked = m("Acked");

export const Message = S.Union([
  CommandReceived,
  ReceivedMediaInfo,
  NotPlayable,
  TrackMissing,
  FailedFetchInfo,
  ReceivedTrackInfoForEnqueue,
  ResolvedFirstAvailableTrack,
  ReceivedQueueDrainResult,
  FailedNextTrackInfo,
  FailedPrevTrackInfo,
  ReceivedPrepareStart,
  FailedPrepareStart,
  ReceivedPrepareStatus,
  FailedPollPrepareStatus,
  CompletedLikeToggle,
  FailedLikeToggle,
  ReceivedDeleteTrackResult,
  ReceivedConcertItems,
  FailedConcertItems,
  ReceivedConcertPlaybackItems,
  FailedConcertPlayback,
  CompletedDeleteInterlude,
  FailedDeleteInterlude,
  ReceivedPlaylistTracks,
  FailedPlaylistLoad,
  ReceivedTrackDetails,
  FailedTrackDetails,
  FailedOpenExternal,
  AudioPlaying,
  AudioPaused,
  AudioEnded,
  AudioErrored,
  AudioPlayRejected,
  ReassertUi,
  SyncLikeFromSwap,
  PressedSpace,
  PressedEscape,
  ClickedOutsideVideo,
  Acked,
]);
export type Message = typeof Message.Type;
