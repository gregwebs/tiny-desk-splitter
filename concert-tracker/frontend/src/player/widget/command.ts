import { Effect, Option, Schema as S } from "effect";
import { Command } from "foldkit";

import {
  getConcertPlayback,
  getMediaInfo,
  getNextTrackMediaInfo,
  getNextTrackMediaInfoOrNull,
  getPlaylist,
  getPrepareStatus,
  getPrevTrackMediaInfo,
  getTrackDetails,
  getTrackMediaInfoOrNull,
  isSourcePlayback,
  postDeleteInterlude,
  postDeleteTrack,
  postEvent,
  postLikeTrack,
  postPrepare,
} from "../../api/client";
import { clampSidebarWidth, PREPARE_POLL_MS, SIDEBAR_WIDTH_KEY } from "../core";
import { byIdOfOrNull, byIdOrNull } from "../../shared/dom";
import { setNowPlaying } from "../mirror";
import {
  Acked,
  RejectedAudioPlay,
  CompletedDeleteInterlude,
  CompletedLikeToggle,
  FailedConcertItems,
  FailedConcertPlayback,
  FailedDeleteInterlude,
  FailedFetchInfo,
  FailedLikeToggle,
  FailedNextTrackInfo,
  FailedOpenExternal,
  FailedPlaylistLoad,
  FailedPollPrepareStatus,
  FailedPrepareStart,
  FailedPrevTrackInfo,
  FailedTrackDetails,
  NotPlayable,
  SucceededConcertItems,
  SucceededConcertPlaybackItems,
  CompletedDeleteTrack,
  SucceededMediaInfo,
  SucceededPlaylistTracks,
  SucceededPrepareStart,
  SucceededPrepareStatus,
  DrainedQueue,
  NoNextTrack,
  SucceededTrackDetails,
  SucceededTrackInfoForEnqueue,
  ResolvedFirstAvailableTrack,
  TrackMissing,
} from "./message";
import {
  AdvancePlan,
  MediaInfo,
  PlayOpts,
  PlaySourceValue,
  PlayTarget,
  PrepareStatus,
  QueueEntry,
} from "./model";

// COMMAND
//
// Two families: typed fetch wrappers over api/client (Effect.tryPromise),
// and real DOM Commands for the handful of nodes/behaviors that stay outside
// the widget's own vdom — the <audio> element itself (imperative play/pause/
// seek regardless of who renders the tag) and external (non-widget) DOM the
// card list owns (playing/preparing marks, like-button copies). Everything
// that was a player.ts `update*`/`render*`/`set*` function touching the
// widget's own bar/sidebar/queue markup is NOT here — it becomes declarative
// view output from Model in commits 2/5/6. See message.ts's scope-decision
// comment for the full list of what's deliberately deferred.

// ── Fetch: media info / play ────────────────────────────────────────────

export const FetchAlbumInfo = Command.define(
  "FetchAlbumInfo",
  { concertId: S.Number, opts: PlayOpts },
  SucceededMediaInfo,
  NotPlayable,
  FailedFetchInfo,
)(({ concertId, opts }) =>
  Effect.tryPromise(() => getMediaInfo(concertId)).pipe(
    Effect.map((info) =>
      info.playable
        ? SucceededMediaInfo({ source: PlaySourceValue.Album({ concertId }), info, opts })
        : NotPlayable({ source: PlaySourceValue.Album({ concertId }), url: info.url }),
    ),
    Effect.catch(() =>
      Effect.succeed(
        FailedFetchInfo({ source: PlaySourceValue.Album({ concertId }), errorMessage: "Couldn't load album" }),
      ),
    ),
  ),
);

/** Mirrors startTrack(): a null result (no 404 thrown — getTrackMediaInfoOrNull
 *  swallows it) means the file doesn't exist yet, routed to TrackMissing
 *  (the prepare flow), distinct from a real fetch failure. */
export const FetchTrackInfo = Command.define(
  "FetchTrackInfo",
  { concertId: S.Number, trackIdx: S.Number, opts: PlayOpts },
  SucceededMediaInfo,
  NotPlayable,
  TrackMissing,
  FailedFetchInfo,
)(({ concertId, trackIdx, opts }) => {
  const source = PlaySourceValue.Track({ concertId, trackIdx });
  return Effect.tryPromise(() => getTrackMediaInfoOrNull(concertId, trackIdx)).pipe(
    Effect.map((info) =>
      info === null
        ? TrackMissing({ source })
        : info.playable
          ? SucceededMediaInfo({ source, info, opts })
          : NotPlayable({ source, url: info.url }),
    ),
    Effect.catch(() => Effect.succeed(FailedFetchInfo({ source, errorMessage: "Couldn't load track" }))),
  );
});

export const FetchTrackInfoForEnqueue = Command.define(
  "FetchTrackInfoForEnqueue",
  { concertId: S.Number, trackIdx: S.Number },
  SucceededTrackInfoForEnqueue,
)(({ concertId, trackIdx }) =>
  Effect.tryPromise(() => getTrackMediaInfoOrNull(concertId, trackIdx)).pipe(
    Effect.map((info) =>
      SucceededTrackInfoForEnqueue({
        concertId,
        trackIdx,
        info: info === null ? Option.none() : Option.some({ title: info.title, liked: !!info.liked }),
      }),
    ),
    Effect.catch(() =>
      Effect.succeed(SucceededTrackInfoForEnqueue({ concertId, trackIdx, info: Option.none() })),
    ),
  ),
);

export const ResolveFirstAvailableTrack = Command.define(
  "ResolveFirstAvailableTrack",
  { concertId: S.Number },
  ResolvedFirstAvailableTrack,
)(({ concertId }) =>
  Effect.gen(function* () {
    const head = yield* Effect.tryPromise(() => getTrackMediaInfoOrNull(concertId, 0)).pipe(
      Effect.catch(() => Effect.succeed(null)),
    );
    if (head) return ResolvedFirstAvailableTrack({ concertId, trackIdx: Option.some(0) });
    const next = yield* Effect.tryPromise(() => getNextTrackMediaInfo(concertId, 0)).pipe(
      Effect.catch(() => Effect.succeed(null)),
    );
    const idx = next?.track_index ?? null;
    return ResolvedFirstAvailableTrack({
      concertId,
      trackIdx: idx === null ? Option.none() : Option.some(idx),
    });
  }),
);

/** Mirrors playFromQueue(): try entries front-to-back until one has a
 *  playable file, skipping (and permanently dropping) any that don't.
 *  `skippedCount` lets update.ts trim that many off the front of the
 *  *current* model.queue, tolerant of a concurrent Enqueue while this was
 *  in flight (see DrainedQueue's doc comment in message.ts). */
export const DrainQueue = Command.define(
  "DrainQueue",
  { queue: S.Array(QueueEntry), plan: AdvancePlan },
  DrainedQueue,
)(({ queue, plan }) =>
  Effect.gen(function* () {
    for (let i = 0; i < queue.length; i++) {
      const entry = queue[i]!;
      const info: MediaInfo | null = yield* Effect.tryPromise(() =>
        getTrackMediaInfoOrNull(entry.concertId, entry.trackIdx),
      ).pipe(Effect.catch(() => Effect.succeed(null)));
      if (info && info.playable) {
        return DrainedQueue({ played: Option.some({ entry, info }), skippedCount: i, plan });
      }
    }
    return DrainedQueue({ played: Option.none(), skippedCount: queue.length, plan });
  }),
);

export const FetchNextTrackInfo = Command.define(
  "FetchNextTrackInfo",
  { concertId: S.Number, trackIdx: S.Number, plan: AdvancePlan },
  SucceededMediaInfo,
  NoNextTrack,
  FailedNextTrackInfo,
)(({ concertId, trackIdx, plan }) =>
  Effect.tryPromise(() => getNextTrackMediaInfoOrNull(concertId, trackIdx)).pipe(
    Effect.map((info) =>
      info === null
        ? NoNextTrack({ plan })
        : SucceededMediaInfo({
            source: PlaySourceValue.Track({ concertId, trackIdx: info.track_index ?? trackIdx }),
            info,
            opts: { recordListen: true, playlistName: null, openVideoPanel: false },
          }),
    ),
    Effect.catch(() => Effect.succeed(FailedNextTrackInfo({ plan }))),
  ),
);

export const FetchPrevTrackInfo = Command.define(
  "FetchPrevTrackInfo",
  { concertId: S.Number, trackIdx: S.Number },
  SucceededMediaInfo,
  FailedPrevTrackInfo,
)(({ concertId, trackIdx }) =>
  Effect.tryPromise(() => getPrevTrackMediaInfo(concertId, trackIdx)).pipe(
    Effect.map((info) =>
      SucceededMediaInfo({
        source: PlaySourceValue.Track({ concertId, trackIdx: info.track_index ?? trackIdx }),
        info,
        opts: { recordListen: true, playlistName: null, openVideoPanel: false },
      }),
    ),
    Effect.catch(() => Effect.succeed(FailedPrevTrackInfo())),
  ),
);

// ── Prepare / poll ───────────────────────────────────────────────────────

export const PostPrepare = Command.define(
  "PostPrepare",
  { target: PlayTarget },
  SucceededPrepareStart,
  FailedPrepareStart,
)(({ target }) =>
  Effect.gen(function* () {
    const resp = yield* Effect.tryPromise(() => postPrepare(target.concertId));
    if (!resp.ok) return FailedPrepareStart({ target });
    const seedStatus = yield* Effect.tryPromise(() => resp.json()).pipe(
      Effect.map((json) => {
        const exit = S.decodeUnknownExit(PrepareStatus)(json);
        return exit._tag === "Success" ? Option.some(exit.value) : Option.none<PrepareStatus>();
      }),
      Effect.catch(() => Effect.succeed(Option.none<PrepareStatus>())),
    );
    return SucceededPrepareStart({ target, seedStatus });
  }).pipe(Effect.catch(() => Effect.succeed(FailedPrepareStart({ target })))),
);

export const PollPrepareStatus = Command.define(
  "PollPrepareStatus",
  { target: PlayTarget, elapsedMs: S.Number, seedStatus: S.Option(PrepareStatus) },
  SucceededPrepareStatus,
  FailedPollPrepareStatus,
)(({ target, elapsedMs, seedStatus }) =>
  Option.match(seedStatus, {
    onSome: (status) => Effect.succeed(SucceededPrepareStatus({ target, status, elapsedMs })),
    onNone: () => {
      const concertId = target.concertId;
      return Effect.sleep(PREPARE_POLL_MS).pipe(
        Effect.flatMap(() => Effect.tryPromise(() => getPrepareStatus(concertId))),
        Effect.map((status) => SucceededPrepareStatus({ target, status, elapsedMs: elapsedMs + PREPARE_POLL_MS })),
        Effect.catch(() => Effect.succeed(FailedPollPrepareStatus({ target, elapsedMs: elapsedMs + PREPARE_POLL_MS }))),
      );
    },
  }),
);

// ── Like / delete ────────────────────────────────────────────────────────

export const ToggleLikeRequest = Command.define(
  "ToggleLikeRequest",
  { concertId: S.Number, trackIdx: S.Number, next: S.Boolean },
  CompletedLikeToggle,
  FailedLikeToggle,
)(({ concertId, trackIdx, next }) =>
  Effect.tryPromise(() => postLikeTrack(concertId, trackIdx)).pipe(
    Effect.map((resp) =>
      resp.ok
        ? CompletedLikeToggle({ concertId, trackIdx, liked: next })
        : FailedLikeToggle({ concertId, trackIdx, attempted: next }),
    ),
    Effect.catch(() => Effect.succeed(FailedLikeToggle({ concertId, trackIdx, attempted: next }))),
  ),
);

/** Also performs the original's external-DOM follow-up (swapping the
 *  refreshed concert-card HTML in if it's on the page) — that's real
 *  imperative DOM work outside the widget's own root, same family as
 *  MarkPlayingExternal below, so it belongs in the Command rather than
 *  a separate Acked-ignoring round trip. */
export const DeleteTrackRequest = Command.define(
  "DeleteTrackRequest",
  { concertId: S.Number, trackIdx: S.Number, source: S.Literals(["bar", "sidebar"]) },
  CompletedDeleteTrack,
)(({ concertId, trackIdx, source }) =>
  Effect.tryPromise(() => postDeleteTrack(concertId, trackIdx)).pipe(
    Effect.flatMap((resp) =>
      resp.ok
        ? Effect.tryPromise(() => resp.text()).pipe(
            Effect.tap((html) =>
              Effect.sync(() => {
                const card = byIdOrNull(`concert-${concertId}`);
                if (card) {
                  card.outerHTML = html;
                  const fresh = byIdOrNull(`concert-${concertId}`);
                  if (fresh && window.htmx) window.htmx.process(fresh);
                }
              }),
            ),
            Effect.as(CompletedDeleteTrack({ concertId, trackIdx, ok: true, source })),
          )
        : Effect.succeed(CompletedDeleteTrack({ concertId, trackIdx, ok: false, source })),
    ),
    Effect.catch(() => Effect.succeed(CompletedDeleteTrack({ concertId, trackIdx, ok: false, source }))),
  ),
);

// ── Concert reconstruction ──────────────────────────────────────────────

export const RefreshConcertItems = Command.define(
  "RefreshConcertItems",
  { concertId: S.Number, advanceAfter: S.Boolean },
  SucceededConcertItems,
  FailedConcertItems,
)(({ concertId, advanceAfter }) =>
  Effect.tryPromise(() => getConcertPlayback(concertId)).pipe(
    Effect.map((data) =>
      isSourcePlayback(data)
        ? FailedConcertItems({ concertId }) // defensive: shouldn't happen while in reconstruction mode
        : SucceededConcertItems({ concertId, items: data.items, advanceAfter }),
    ),
    Effect.catch(() => Effect.succeed(FailedConcertItems({ concertId }))),
  ),
);

export const FetchConcertPlayback = Command.define(
  "FetchConcertPlayback",
  { concertId: S.Number, atPos: S.Option(S.Number), errorMessage: S.String },
  SucceededMediaInfo,
  NotPlayable,
  SucceededConcertPlaybackItems,
  FailedConcertPlayback,
)(({ concertId, atPos, errorMessage }) =>
  Effect.tryPromise(() => getConcertPlayback(concertId)).pipe(
    Effect.map((data) => {
      if (isSourcePlayback(data)) {
        const info = data.source;
        return info.playable
          ? SucceededMediaInfo({
              source: PlaySourceValue.Album({ concertId }),
              info,
              opts: { recordListen: true, playlistName: null, openVideoPanel: false },
            })
          : NotPlayable({ source: PlaySourceValue.Album({ concertId }), url: info.url });
      }
      return data.items.length > 0
        ? SucceededConcertPlaybackItems({ concertId, items: data.items, atPos: Option.getOrElse(atPos, () => 0) })
        : FailedConcertPlayback({ concertId, errorMessage: "Nothing to play" });
    }),
    Effect.catch(() => Effect.succeed(FailedConcertPlayback({ concertId, errorMessage }))),
  ),
);

export const PostDeleteInterlude = Command.define(
  "PostDeleteInterlude",
  { concertId: S.Number, interludeIdx: S.Number, wasPlayingThis: S.Boolean },
  CompletedDeleteInterlude,
  FailedDeleteInterlude,
)(({ concertId, interludeIdx, wasPlayingThis }) =>
  Effect.tryPromise(() => postDeleteInterlude(concertId, interludeIdx)).pipe(
    Effect.map((resp) =>
      resp.ok
        ? CompletedDeleteInterlude({ concertId, interludeIdx, wasPlayingThis })
        : FailedDeleteInterlude({ concertId, interludeIdx }),
    ),
    Effect.catch(() => Effect.succeed(FailedDeleteInterlude({ concertId, interludeIdx }))),
  ),
);

// ── Playlists ────────────────────────────────────────────────────────────

/** Mirrors playPlaylist()'s fetch + `resolved_tracks.filter(available)`
 *  step; the groupId mint and queue append are pure model.ts/update.ts work,
 *  not part of this Command (only the network call is). */
export const FetchPlaylistForPlay = Command.define(
  "FetchPlaylistForPlay",
  { playlistId: S.Number },
  SucceededPlaylistTracks,
  FailedPlaylistLoad,
)(({ playlistId }) =>
  Effect.tryPromise(() => getPlaylist(playlistId)).pipe(
    Effect.map((data) => {
      const tracks = (data.resolved_tracks || [])
        .filter((track) => track.available)
        .map((track) => ({ concertId: track.concert_id, trackIdx: track.track_index, title: track.title }));
      return SucceededPlaylistTracks({ playlistId, name: data.playlist.name, tracks });
    }),
    Effect.catch(() => Effect.succeed(FailedPlaylistLoad({ playlistId }))),
  ),
);

// ── Fire-and-forget ──────────────────────────────────────────────────────

export const RecordListenEvent = Command.define(
  "RecordListenEvent",
  { url: S.String },
  Acked,
)(({ url }) => Effect.tryPromise(() => postEvent(url)).pipe(Effect.catch(() => Effect.succeed(undefined)), Effect.as(Acked())));

/** openExternal()'s postEvent — unlike RecordListenEvent above, a failure
 *  here is user-visible ("Couldn't open externally"), so it doesn't swallow
 *  errors the same way. */
export const OpenExternalRequest = Command.define(
  "OpenExternalRequest",
  { url: S.String },
  Acked,
  FailedOpenExternal,
)(({ url }) =>
  Effect.tryPromise(() => postEvent(url)).pipe(
    Effect.as(Acked()),
    Effect.catch(() => Effect.succeed(FailedOpenExternal())),
  ),
);

// ── Audio element ────────────────────────────────────────────────────────

// `loadGen` is stamped onto the element's dataset in the same synchronous
// statement as `audio.src = url` — the two mutations happen atomically, so
// the audioTime Subscription's `audioTimeMessage` (subscription.ts) can read
// `audio.dataset.audioLoadGen` back as ground truth for "which resource is
// actually loaded right now," independent of when this Command's Effect
// happens to run relative to the model update that triggered it (see
// model.ts's audioLoadGen doc comment for the race this closes).
export const PlayAudio = Command.define(
  "PlayAudio",
  { url: S.String, loadGen: S.Number },
  Acked,
  RejectedAudioPlay,
)(({ url, loadGen }) =>
  Effect.sync(() => byIdOfOrNull("player-audio", HTMLMediaElement)).pipe(
    Effect.flatMap((audio) => {
      if (!audio) return Effect.succeed(RejectedAudioPlay());
      audio.src = url;
      audio.dataset.audioLoadGen = String(loadGen);
      return Effect.tryPromise(() => audio.play()).pipe(
        Effect.as(Acked()),
        Effect.catch(() => Effect.succeed(RejectedAudioPlay())),
      );
    }),
  ),
);

export const PauseAudio = Command.define(
  "PauseAudio",
  Acked,
)(Effect.sync(() => byIdOfOrNull("player-audio", HTMLMediaElement)?.pause()).pipe(Effect.as(Acked())));

export const ResumeAudio = Command.define(
  "ResumeAudio",
  Acked,
  RejectedAudioPlay,
)(
  Effect.sync(() => byIdOfOrNull("player-audio", HTMLMediaElement)).pipe(
    Effect.flatMap((audio) => (audio ? Effect.tryPromise(() => audio.play()) : Effect.void)),
    Effect.as(Acked()),
    Effect.catch(() => Effect.succeed(RejectedAudioPlay())),
  ),
);

// Toggle decisions must use the media element's live state. Model.isPlaying
// is event-derived and can still be stale when rapid host commands arrive.
export const ToggleAudio = Command.define(
  "ToggleAudio",
  Acked,
  RejectedAudioPlay,
)(
  Effect.sync(() => byIdOfOrNull("player-audio", HTMLMediaElement)).pipe(
    Effect.flatMap((audio) => {
      if (!audio) return Effect.succeed(Acked());
      if (!audio.paused) {
        audio.pause();
        return Effect.succeed(Acked());
      }
      return Effect.tryPromise(() => audio.play()).pipe(
        Effect.as(Acked()),
        Effect.catch(() => Effect.succeed(RejectedAudioPlay())),
      );
    }),
  ),
);

export const SeekAudio = Command.define(
  "SeekAudio",
  { seconds: S.Number },
  Acked,
)(({ seconds }) =>
  Effect.sync(() => {
    const audio = byIdOfOrNull("player-audio", HTMLMediaElement);
    if (audio && Number.isFinite(audio.duration) && audio.duration > 0) audio.currentTime = seconds;
  }).pipe(Effect.as(Acked())),
);

/** The removeAttribute+load() sequence is load-bearing: `audio.src = ""`
 *  resolves to the page URL and fires a spurious `error` event that would
 *  trigger auto-advance (see player.ts's stopPlayback comment). */
export const ClearAudioSrc = Command.define(
  "ClearAudioSrc",
  Acked,
)(
  Effect.sync(() => {
    const audio = byIdOfOrNull("player-audio", HTMLMediaElement);
    if (!audio) return;
    audio.pause();
    audio.removeAttribute("src");
    audio.load();
  }).pipe(Effect.as(Acked())),
);

// ── External (non-widget) DOM ───────────────────────────────────────────

function findTrackButtons(concertId: number, trackIdx: number | null): NodeListOf<HTMLElement> {
  if (trackIdx != null) {
    return document.querySelectorAll<HTMLElement>(`[data-concert-id="${concertId}"][data-track-idx="${trackIdx}"]`);
  }
  return document.querySelectorAll<HTMLElement>(`[data-concert-id="${concertId}"][data-role="listen-album"]`);
}

export const MarkPlayingExternal = Command.define(
  "MarkPlayingExternal",
  { concertId: S.Number, trackIdx: S.Option(S.Number) },
  Acked,
)(({ concertId, trackIdx }) =>
  Effect.sync(() => {
    document
      .querySelectorAll(".btn-track-listen.playing, .btn-listen.playing")
      .forEach((button) => button.classList.remove("playing"));
    findTrackButtons(concertId, Option.getOrNull(trackIdx)).forEach((button) => button.classList.add("playing"));
  }).pipe(Effect.as(Acked())),
);

/** Mirrors playConcertItem()'s interlude branch: clearPlaying() then mark by
 *  data-interlude-idx (not data-track-idx — interludes have no track index). */
export const MarkPlayingInterludeExternal = Command.define(
  "MarkPlayingInterludeExternal",
  { concertId: S.Number, interludeIdx: S.Number },
  Acked,
)(({ concertId, interludeIdx }) =>
  Effect.sync(() => {
    document
      .querySelectorAll(".btn-track-listen.playing, .btn-listen.playing")
      .forEach((button) => button.classList.remove("playing"));
    document
      .querySelectorAll(`[data-concert-id="${concertId}"][data-interlude-idx="${interludeIdx}"]`)
      .forEach((button) => button.classList.add("playing"));
  }).pipe(Effect.as(Acked())),
);

export const MarkPreparingExternal = Command.define(
  "MarkPreparingExternal",
  { concertId: S.Number, trackIdx: S.Number },
  Acked,
)(({ concertId, trackIdx }) =>
  Effect.sync(() => {
    findTrackButtons(concertId, trackIdx).forEach((button) => button.classList.add("preparing"));
  }).pipe(Effect.as(Acked())),
);

export const ClearPreparingExternal = Command.define(
  "ClearPreparingExternal",
  Acked,
)(
  Effect.sync(() => {
    document.querySelectorAll(".btn-track-listen.preparing").forEach((button) => button.classList.remove("preparing"));
  }).pipe(Effect.as(Acked())),
);

export const DisableCardTracksExternal = Command.define(
  "DisableCardTracksExternal",
  { concertId: S.Number },
  Acked,
)(({ concertId }) =>
  Effect.sync(() => {
    const card = byIdOrNull(`concert-${concertId}`);
    card?.querySelectorAll<HTMLButtonElement>(".btn-tracks, .btn-track-listen").forEach((button) => {
      button.disabled = true;
    });
  }).pipe(Effect.as(Acked())),
);

export const SyncLikeButtonsExternal = Command.define(
  "SyncLikeButtonsExternal",
  { concertId: S.Number, trackIdx: S.Option(S.Number), liked: S.Boolean },
  Acked,
)(({ concertId, trackIdx, liked }) =>
  Effect.sync(() => {
    document
      .querySelectorAll<HTMLElement>(
        `.btn-like[hx-post="/concerts/${concertId}/tracks/${Option.getOrNull(trackIdx)}/like"]`,
      )
      .forEach((likeButton) => {
        likeButton.classList.toggle("liked", liked);
        likeButton.textContent = liked ? "★" : "☆";
      });
  }).pipe(Effect.as(Acked())),
);

export const OpenInNewTab = Command.define(
  "OpenInNewTab",
  { url: S.String },
  Acked,
)(({ url }) => Effect.sync(() => window.open(url, "_blank", "noopener")).pipe(Effect.as(Acked())));

export const RefreshCardStatus = Command.define(
  "RefreshCardStatus",
  { concertId: S.Number },
  Acked,
)(({ concertId }) =>
  Effect.sync(() => {
    const card = byIdOrNull(`concert-${concertId}`);
    if (card && window.htmx) {
      window.htmx.ajax("GET", `/concerts/${concertId}/status`, {
        target: `#concert-${concertId}`,
        swap: "outerHTML",
      });
    }
  }).pipe(Effect.as(Acked())),
);

export const SyncNowPlayingMirror = Command.define(
  "SyncNowPlayingMirror",
  { concertId: S.NullOr(S.Number), trackIdx: S.NullOr(S.Number) },
  Acked,
)(({ concertId, trackIdx }) =>
  Effect.sync(() => {
    setNowPlaying({ concertId, trackIdx });
    // #player-bar is display:none until active; body.player-active reserves the
    // bottom padding. Both track playback identity, so toggle alongside the mirror.
    document.body.classList.toggle("player-active", concertId !== null);
  }).pipe(Effect.as(Acked())),
);

export const OpenAddToPlaylist = Command.define(
  "OpenAddToPlaylist",
  { concertId: S.Number, trackIdx: S.Number, label: S.String },
  Acked,
)(({ concertId, trackIdx, label }) =>
  Effect.sync(() => {
    window.Playlists?.openAdd({ type: "track", concertId, trackIndex: trackIdx, label });
  }).pipe(Effect.as(Acked())),
);

/** Scrolls the queue section to the bottom, pinning the view to the
 *  next-to-play entry (pos=0) after a new item is added to the end. */
export const ScrollQueueToBottom = Command.define(
  "ScrollQueueToBottom",
  Acked,
)(
  Effect.sync(() => {
    const section = byIdOrNull("sidebar-queue-section");
    if (section) section.scrollTop = section.scrollHeight;
  }).pipe(Effect.as(Acked())),
);

// ── Body class / video panel (external DOM, not widget-owned) ─────────────

/** Toggle a class on `document.body` (used by `sidebar-open` and the
 *  video-panel `open` class on `#player-video-panel`'s siblings). Idempotent. */
export const MutateBodyClass = Command.define(
  "MutateBodyClass",
  { className: S.String, add: S.Boolean },
  Acked,
)(({ className, add }) =>
  Effect.sync(() => {
    document.body.classList[add ? "add" : "remove"](className);
  }).pipe(Effect.as(Acked())),
);

/** Add `open` class to `#player-video-panel` (shows the video element). */
export const ShowVideoPanel = Command.define(
  "ShowVideoPanel",
  Acked,
)(
  Effect.sync(() => {
    byIdOrNull("player-video-panel")?.classList.add("open");
  }).pipe(Effect.as(Acked())),
);

/** Remove `open` (hides the video element) and `controls-visible` (the
 *  minimize-button reveal, mirroring pre-Foldkit hideVideoPanel()'s cleanup —
 *  otherwise the button could linger revealed across a close/reopen). */
export const HideVideoPanel = Command.define(
  "HideVideoPanel",
  Acked,
)(
  Effect.sync(() => {
    byIdOrNull("player-video-panel")?.classList.remove("open", "controls-visible");
  }).pipe(Effect.as(Acked())),
);

/** Read saved sidebar width from localStorage and apply it as a CSS variable.
 *  Runs once at widget init to restore the user's last drag position. */
export const LoadSidebarWidth = Command.define(
  "LoadSidebarWidth",
  Acked,
)(
  Effect.sync(() => {
    const saved = parseInt(localStorage.getItem(SIDEBAR_WIDTH_KEY) || "", 10);
    if (!isNaN(saved)) {
      document.documentElement.style.setProperty("--sidebar-width", `${clampSidebarWidth(saved)}px`);
    }
  }).pipe(Effect.as(Acked())),
);

export const SetSidebarWidthVar = Command.define(
  "SetSidebarWidthVar",
  { px: S.Number },
  Acked,
)(({ px }) =>
  Effect.sync(() => {
    document.documentElement.style.setProperty("--sidebar-width", `${clampSidebarWidth(px)}px`);
  }).pipe(Effect.as(Acked())),
);

export const PersistSidebarWidth = Command.define(
  "PersistSidebarWidth",
  { px: S.Number },
  Acked,
)(({ px }) =>
  Effect.sync(() => {
    try {
      localStorage.setItem(SIDEBAR_WIDTH_KEY, String(clampSidebarWidth(px)));
    } catch {
      /* storage unavailable */
    }
  }).pipe(Effect.as(Acked())),
);

// ── Sidebar track details ─────────────────────────────────────────────────

/** Fetches `GET /concerts/:id/track-details` for the player widget's sidebar
 *  concert section (whole-album / normal mode).  `loadGen` is echoed back so
 *  update.ts can discard stale responses when a newer fetch has already started. */
export const FetchTrackDetails = Command.define(
  "FetchTrackDetails",
  { concertId: S.Number, loadGen: S.Number },
  SucceededTrackDetails,
  FailedTrackDetails,
)(({ concertId, loadGen }) =>
  Effect.tryPromise(() => getTrackDetails(concertId)).pipe(
    Effect.map((data) =>
      SucceededTrackDetails({
        concertId,
        loadGen,
        tracksBusy: data.tracks_busy,
        tracks: data.tracks,
      }),
    ),
    Effect.catch(() => Effect.succeed(FailedTrackDetails({ concertId, loadGen }))),
  ),
);
