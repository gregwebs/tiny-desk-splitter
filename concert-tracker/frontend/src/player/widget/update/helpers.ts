import { Match as M, Option } from "effect";
import type { Command } from "foldkit/command";
import { evo } from "foldkit/struct";

import { concertAdvancePos, concertItemNav, type PlaybackState as CorePlaybackState } from "../../core";
import {
  ClearAudioSrc,
  ClearPreparingExternal,
  DrainQueue,
  FetchNextTrackInfo,
  FetchTrackDetails,
  FetchTrackInfo,
  FetchTrackInfoForEnqueue,
  HideVideoPanel,
  MarkPlayingExternal,
  MarkPlayingInterludeExternal,
  PauseAudio,
  PlayAudio,
  RecordListenEvent,
  ResumeAudio,
  SeekAudio,
  ShowVideoPanel,
  SyncNowPlayingMirror,
} from "../command";
import type { Message } from "../message";
import {
  type AdvancePlan,
  defaultPlayOpts,
  initialPlayback,
  type MediaInfo,
  type Model,
  type PlayOpts,
  type PlaySource,
  PlaySourceValue,
  StatusValue,
} from "../model";

// UPDATE HELPERS
//
// Pure decision logic shared by both the top-level `update` (update.ts) and
// the PlayerCommand dispatch (update/handleHostCommand.ts) — split out so
// neither of those two needs to import from the other (a straight
// update.ts <-> handleHostCommand.ts pair would be circular, since
// update.ts's CommandReceived case calls into handleHostCommand, and
// handleHostCommand needs the same beginPlayback/dispatchPlayTrack/
// applyLikedEverywhere machinery update.ts's own handlers use).
//
// `withPlayback` is the ONLY place that appends SyncNowPlayingMirror, and it
// must be called exactly once at the outermost return of any branch that
// changes playback identity — never from inside a helper another handler
// also wraps (see beginPlayback/playConcertItem below), or the mirror sync
// would fire twice for one logical transition.

export type UpdateReturn = readonly [Model, ReadonlyArray<Command<Message>>];
export const withUpdateReturn = M.withReturnType<UpdateReturn>();

// ── Small pure predicates / adapters ────────────────────────────────────

export const toCoreState = (playback: Model["playback"]): CorePlaybackState => ({
  concertId: playback.concertId,
  trackIdx: playback.trackIdx,
  isVideo: playback.isVideo,
  watchUrl: playback.watchUrl,
  hasNext: playback.hasNext,
  hasPrev: playback.hasPrev,
  liked: playback.liked,
  concert: Option.getOrNull(playback.concert),
});

const hasActiveMedia = (model: Model): boolean => model.playback.concertId !== null;
export const isMediaPlaying = (model: Model): boolean => hasActiveMedia(model) && model.isPlaying;
export const playerIdle = (model: Model): boolean => !hasActiveMedia(model) || model.playback.ended;

// ── Status helpers ───────────────────────────────────────────────────────

export const withError = (model: Model, message: string): Model =>
  evo(model, { status: () => StatusValue.Error({ message }) });
export const withBusy = (model: Model, message: string): Model =>
  evo(model, { status: () => StatusValue.Busy({ message }) });

/** Appends the now-playing mirror sync. Call exactly once, at the outermost
 *  return of a branch that changes playback identity — see ../mirror.ts. */
export const withPlayback = (model: Model, commands: ReadonlyArray<Command<Message>>): UpdateReturn => [
  model,
  [...commands, SyncNowPlayingMirror({ concertId: model.playback.concertId, trackIdx: model.playback.trackIdx })],
];

// ── PlaySource → identity/URL derivation ────────────────────────────────

const targetIdFor = (source: PlaySource): { concertId: number; trackIdx: number | null } =>
  M.value(source).pipe(
    M.withReturnType<{ concertId: number; trackIdx: number | null }>(),
    M.tagsExhaustive({
      Track: ({ concertId, trackIdx }) => ({ concertId, trackIdx }),
      Album: ({ concertId }) => ({ concertId, trackIdx: null }),
      ConcertItem: ({ concertId, trackIdx }) => ({ concertId, trackIdx }),
    }),
  );

const listenUrlFor = (source: PlaySource): string | null =>
  M.value(source).pipe(
    M.withReturnType<string | null>(),
    M.tagsExhaustive({
      Track: ({ concertId, trackIdx }) => `/concerts/${concertId}/tracks/${trackIdx}/listen`,
      Album: ({ concertId }) => `/concerts/${concertId}/listen`,
      ConcertItem: ({ concertId, trackIdx, isInterlude }) =>
        isInterlude || trackIdx === null ? null : `/concerts/${concertId}/tracks/${trackIdx}/listen`,
    }),
  );

const watchUrlFor = (source: PlaySource, info: MediaInfo): string | null => {
  if (!info.is_video) return null;
  return M.value(source).pipe(
    M.withReturnType<string | null>(),
    M.tagsExhaustive({
      Track: ({ concertId, trackIdx }) => `/concerts/${concertId}/tracks/${trackIdx}/watch`,
      Album: ({ concertId }) => `/concerts/${concertId}/watch`,
      // playConcertItem() always passes watchUrl: null, even for video items
      // (no per-item watch endpoint, and interludes have no trackIdx to build
      // one from). The view must therefore gate the Watch button on isVideo
      // alone, not on watchUrl — see view.ts's player-watch button.
      ConcertItem: () => null,
    }),
  );
};

// ── Begin playback (mirrors player.ts's play()) ─────────────────────────
//
// Raw [Model, Command[]] — deliberately does NOT call withPlayback itself, so
// callers (playConcertItemPure, which further restores playback.concert
// after calling this) can wrap the *final* result exactly once.
export const beginPlayback = (
  model: Model,
  source: PlaySource,
  info: MediaInfo,
  opts: PlayOpts,
): readonly [Model, ReadonlyArray<Command<Message>>] => {
  const { concertId, trackIdx } = targetIdFor(source);
  const listenUrl = listenUrlFor(source);
  const watchUrl = watchUrlFor(source, info);
  // Mirrors `if (!isVideo) hideVideoPanel()` + watchTrackDirect's forceOpen.
  const newVideoOpen = !info.is_video ? false : opts.openVideoPanel ? true : model.video.open;

  const model1 = evo(model, {
    playback: () => ({
      concertId,
      trackIdx,
      title: info.title,
      artist: info.artist,
      isVideo: info.is_video,
      watchUrl,
      hasNext: info.has_next,
      hasPrev: info.has_prev,
      liked: info.liked,
      ended: false,
      // Cleared here; playConcertItemPure restores it after this returns.
      concert: Option.none(),
      playlistLabel: opts.playlistName,
    }),
    video: () => ({ open: newVideoOpen }),
    pending: () => Option.none(),
    status: () => StatusValue.Idle(),
  });

  const videoPanelToggle = newVideoOpen !== model.video.open ? [newVideoOpen ? ShowVideoPanel() : HideVideoPanel()] : [];
  const recordListen = listenUrl && opts.recordListen ? [RecordListenEvent({ url: listenUrl })] : [];
  const resumeSeek = Option.match(model.pendingSeek, {
    onNone: () => [],
    onSome: (seconds) => [SeekAudio({ seconds })],
  });
  const commands: Command<Message>[] = [
    PlayAudio({ url: info.url }),
    MarkPlayingExternal({ concertId, trackIdx: Option.fromNullishOr(trackIdx) }),
    ClearPreparingExternal(),
    ...videoPanelToggle,
    ...recordListen,
    ...resumeSeek,
  ];

  return [evo(model1, { pendingSeek: () => Option.none() }), commands];
};

/** When a whole-album play changes the playing concert while the sidebar is
 *  open, the sidebar track list (model.sidebar.tracks) is for the old concert.
 *  Refetch it, mirroring the OpenSidebar fetch. The concertId-changed guard is
 *  load-bearing, not an optimization: this also runs on every intra-album
 *  next/prev advance (same concertId -> no refetch). Reconstruction plays do
 *  not call this — they render the sidebar from playback.concert.items. */
export const refetchSidebarIfConcertChanged = (
  prevModel: Model,
  [model, commands]: readonly [Model, ReadonlyArray<Command<Message>>],
): readonly [Model, ReadonlyArray<Command<Message>>] => {
  const concertId = model.playback.concertId;
  if (
    !prevModel.sidebar.open ||
    concertId === null ||
    concertId === prevModel.playback.concertId ||
    Option.isSome(model.playback.concert)
  ) {
    return [model, commands];
  }
  const loadGen = prevModel.sidebar.loadGen + 1;
  return [
    evo(model, { sidebar: () => evo(model.sidebar, { loadGen: () => loadGen }) }),
    [...commands, FetchTrackDetails({ concertId, loadGen })],
  ];
};

/** playTrack()/PlayerApi.playTrack's shared dispatch: same-track toggles
 *  pause/resume, something-else-playing enqueues, otherwise fetches+plays.
 *  Used by CommandReceived.PlayTrack and the prepare-ready path (applyPrepareStatus
 *  also calls playTrack, not startTrack). */
export const dispatchPlayTrack = (model: Model, concertId: number, trackIdx: number): UpdateReturn => {
  if (model.playback.concertId === concertId && model.playback.trackIdx === trackIdx) {
    return model.isPlaying ? [model, [PauseAudio()]] : [model, [ResumeAudio()]];
  }
  if (isMediaPlaying(model)) {
    return [model, [FetchTrackInfoForEnqueue({ concertId, trackIdx })]];
  }
  return [model, [FetchTrackInfo({ concertId, trackIdx, opts: defaultPlayOpts })]];
};

// ── Stop / advance-failure terminal states ──────────────────────────────

export const stopPlaybackPure = (model: Model): UpdateReturn =>
  withPlayback(
    evo(model, {
      playback: () => initialPlayback,
      queue: () => [],
      sidebar: () => ({ open: false, tracks: Option.none(), loadGen: 0 }),
      video: () => ({ open: false }),
      isPlaying: () => false,
      pendingSeek: () => Option.none(),
      status: () => StatusValue.Idle(),
      // pending intentionally untouched — stopPlayback() cancels auto-advance,
      // never a prepare-in-flight (cancelPendingPlay is never called there).
    }),
    model.video.open ? [ClearAudioSrc(), HideVideoPanel()] : [ClearAudioSrc()],
  );

/** playNextTrack()'s definitively-nothing-to-advance-to terminal state,
 *  shared by the immediate no-track guard and FailedNextTrackInfo. */
export const applyAdvanceFailure = (model: Model, plan: AdvancePlan): UpdateReturn =>
  M.value(plan).pipe(
    withUpdateReturn,
    M.whenOr("queue-only", "next-or-none", () => [evo(model, { isPlaying: () => false }), []]),
    M.when("next-or-stop", () => stopPlaybackPure(model)),
    M.when("next-or-collapse", () => [
      evo(model, { isPlaying: () => false, video: () => ({ open: false }) }),
      model.video.open ? [HideVideoPanel()] : [],
    ]),
    M.exhaustive,
  );

export const advanceToNextTrack = (model: Model, plan: AdvancePlan): UpdateReturn => {
  const { concertId, trackIdx } = model.playback;
  if (concertId === null || trackIdx === null) return applyAdvanceFailure(model, plan);
  return [model, [FetchNextTrackInfo({ concertId, trackIdx, plan })]];
};

export const advanceAfterDelete = (model: Model): UpdateReturn => [
  model,
  [PauseAudio(), DrainQueue({ queue: model.queue, plan: "next-or-stop" })],
];

export const advanceOrCollapse = (model: Model): UpdateReturn =>
  Option.isSome(model.playback.concert)
    ? advanceConcertPure(model)
    : [model, [DrainQueue({ queue: model.queue, plan: "next-or-collapse" })]];

// ── Concert reconstruction ───────────────────────────────────────────────

/** Looks up `concert.items[pos]` and synthesizes a MediaInfo-shaped object
 *  directly — concert items are already loaded, no fetch needed. Saves +
 *  restores playback.concert around the shared beginPlayback, mirroring
 *  playConcertItem()'s save/restore-state.concert pattern. */
export function playConcertItemPure(model: Model, pos: number): UpdateReturn {
  return Option.match(model.playback.concert, {
    onNone: () => [model, []],
    onSome: (concert) => {
      const item = concert.items[pos];
      if (!item) return [model, []];
      const isInterlude = item.kind === "interlude";
      const trackIdx = isInterlude ? null : (item.track_index ?? null);
      const { hasPrev, hasNext } = concertItemNav(concert.items, pos);
      const info: MediaInfo = {
        artist: item.artist,
        has_next: hasNext,
        has_prev: hasPrev,
        is_video: item.is_video,
        liked: item.liked,
        playable: true,
        title: item.title,
        track_index: trackIdx,
        url: item.url,
      };
      const source = PlaySourceValue.ConcertItem({ concertId: concert.id, trackIdx, isInterlude });
      const [model2, commands] = beginPlayback(model, source, info, defaultPlayOpts);
      const model3 = evo(model2, {
        playback: () => evo(model2.playback, { concert: () => Option.some(evo(concert, { pos: () => pos })) }),
      });
      const extraCommands: ReadonlyArray<Command<Message>> =
        isInterlude && item.interlude_index != null
          ? [MarkPlayingInterludeExternal({ concertId: concert.id, interludeIdx: item.interlude_index })]
          : [];
      return withPlayback(model3, [...commands, ...extraCommands]);
    },
  });
}

/** advanceConcert(): move to the next item in the reconstruction, or end
 *  concert mode (no withPlayback — clearing `concert` alone doesn't change
 *  nowPlaying()'s concertId/trackIdx). */
export function advanceConcertPure(model: Model): UpdateReturn {
  return Option.match(model.playback.concert, {
    onNone: () => [model, []],
    onSome: (concert) => {
      const next = concertAdvancePos(concert.pos, concert.items.length);
      return next === null
        ? [
            evo(model, {
              playback: () => evo(model.playback, { concert: () => Option.none() }),
              video: () => ({ open: false }),
            }),
            model.video.open ? [HideVideoPanel()] : [],
          ]
        : playConcertItemPure(model, next);
    },
  });
}

/** playConcertPosOrEnd(): same shape as advanceConcertPure but driven by an
 *  already-resolved `pos` (post-refresh), rather than advancing by one. */
export function playConcertPosOrEnd(model: Model): UpdateReturn {
  return Option.match(model.playback.concert, {
    onNone: () => [model, []],
    onSome: (concert) =>
      concert.pos < concert.items.length
        ? playConcertItemPure(model, concert.pos)
        : [
            evo(model, {
              playback: () => evo(model.playback, { concert: () => Option.none() }),
              video: () => ({ open: false }),
            }),
            model.video.open ? [HideVideoPanel()] : [],
          ],
  });
}

export const concertErrorMessages = { start: "Couldn't start concert", load: "Couldn't load concert" } as const;

// ── Like-sync helpers ────────────────────────────────────────────────────
//
// Called from every like-toggle path (bar ToggleLike, sidebar SidebarLikeTrack,
// and their respective FailedLikeToggle revert) so the liked field stays
// consistent across all three copies: model.playback.liked (bar star),
// model.sidebar.tracks[i].liked (whole-album list), and
// model.playback.concert.items[pos].liked (reconstruction list).

function flipSidebarTrackLiked(model: Model, concertId: number, trackIdx: number, liked: boolean): Model {
  return Option.match(model.sidebar.tracks, {
    onNone: () => model,
    onSome: (sidebarTracks) => {
      if (model.playback.concertId !== concertId) return model;
      const updated = sidebarTracks.tracks.map((track) => (track.index === trackIdx ? { ...track, liked } : track));
      return evo(model, {
        sidebar: () => evo(model.sidebar, { tracks: () => Option.some(evo(sidebarTracks, { tracks: () => updated })) }),
      });
    },
  });
}

// Optimistically mark a whole-album sidebar track available/unavailable (used
// when a sidebar delete removes the files) so the row greys out without a
// refetch. Mirrors flipSidebarTrackLiked; no-op if the sidebar isn't showing
// this concert's track list.
export function flipSidebarTrackAvailable(model: Model, concertId: number, trackIdx: number, available: boolean): Model {
  return Option.match(model.sidebar.tracks, {
    onNone: () => model,
    onSome: (sidebarTracks) => {
      if (model.playback.concertId !== concertId) return model;
      const updated = sidebarTracks.tracks.map((track) =>
        track.index === trackIdx ? { ...track, available } : track,
      );
      return evo(model, {
        sidebar: () => evo(model.sidebar, { tracks: () => Option.some(evo(sidebarTracks, { tracks: () => updated })) }),
      });
    },
  });
}

function flipConcertItemLiked(model: Model, concertId: number, trackIdx: number, liked: boolean): Model {
  return Option.match(model.playback.concert, {
    onNone: () => model,
    onSome: (concert) => {
      if (concert.id !== concertId) return model;
      const updated = concert.items.map((item) =>
        item.track_index === trackIdx && item.kind !== "interlude" ? { ...item, liked } : item,
      );
      return evo(model, {
        playback: () => evo(model.playback, { concert: () => Option.some(evo(concert, { items: () => updated })) }),
      });
    },
  });
}

/** Applies a `liked` flip to every copy the Model keeps in sync: the bar star
 *  (only when `concertId`/`trackIdx` is the currently-playing track),
 *  the whole-album sidebar list, and the concert-reconstruction list.
 *  Shared by every like-toggle path (bar ToggleLike, sidebar SidebarLikeTrack,
 *  CompletedLikeToggle/FailedLikeToggle, and the htmx-swap SwappedLikeButton). */
export function applyLikedEverywhere(model: Model, concertId: number, trackIdx: number, liked: boolean): Model {
  const isCurrentTrack = model.playback.concertId === concertId && model.playback.trackIdx === trackIdx;
  const model1 = isCurrentTrack
    ? evo(model, { playback: () => evo(model.playback, { liked: () => liked }) })
    : model;
  const model2 = flipSidebarTrackLiked(model1, concertId, trackIdx, liked);
  return flipConcertItemLiked(model2, concertId, trackIdx, liked);
}

/** The current `liked` state for a track, looked up from whichever list is
 *  active (the whole-album sidebar list, then the concert-reconstruction
 *  list) — `None` when the track isn't in either loaded list. */
export function findCurrentLiked(model: Model, concertId: number, trackIdx: number): Option.Option<boolean> {
  const inSidebar =
    model.playback.concertId === concertId
      ? Option.flatMap(model.sidebar.tracks, (sidebarTracks) =>
          Option.fromNullishOr(sidebarTracks.tracks.find((track) => track.index === trackIdx)?.liked),
        )
      : Option.none();
  return Option.orElse(inSidebar, () =>
    Option.flatMap(model.playback.concert, (concert) =>
      concert.id === concertId
        ? Option.fromNullishOr(
            concert.items.find((item) => item.track_index === trackIdx && item.kind !== "interlude")?.liked,
          )
        : Option.none(),
    ),
  );
}
