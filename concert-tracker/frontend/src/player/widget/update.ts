import { Array, Match as M, Option } from "effect";
import type { Command } from "foldkit/command";
import { evo } from "foldkit/struct";

import {
  concertAdvancePos,
  concertItemNav,
  dequeueAt,
  enqueueDedupe,
  makeQueueEntry,
  nextEnabled,
  PREPARE_TIMEOUT_MS,
  prevEnabled,
  refindPosByUrl,
  removeGroup,
  type PlaybackState as CorePlaybackState,
} from "../core";
import {
  ClearAudioSrc,
  ClearPreparingExternal,
  DeleteTrackRequest,
  DisableCardTracksExternal,
  DrainQueue,
  FetchAlbumInfo,
  FetchConcertPlayback,
  FetchNextTrackInfo,
  FetchPlaylistForPlay,
  FetchPrevTrackInfo,
  FetchTrackDetails,
  FetchTrackInfo,
  FetchTrackInfoForEnqueue,
  HideVideoPanel,
  MarkPlayingExternal,
  MarkPlayingInterludeExternal,
  MarkPreparingExternal,
  MutateBodyClass,
  NavigateToConcert,
  OpenAddToPlaylist,
  OpenExternalRequest,
  OpenInNewTab,
  PauseAudio,
  PersistSidebarWidth,
  PlayAudio,
  PollPrepareStatus,
  PostDeleteInterlude,
  PostPrepare,
  RecordListenEvent,
  RefreshCardStatus,
  RefreshConcertItems,
  ResolveFirstAvailableTrack,
  ResumeAudio,
  ScrollQueueToBottom,
  SeekAudio,
  SetSidebarWidthVar,
  ShowVideoPanel,
  SyncLikeButtonsExternal,
  SyncNowPlayingMirror,
  ToggleLikeRequest,
} from "./command";
import type { Message } from "./message";
import {
  type AdvancePlan,
  defaultPlayOpts,
  initialPlayback,
  type MediaInfo,
  type Model,
  type PlayOpts,
  type PlaySource,
  PlaySourceValue,
  PlayTargetValue,
  sameTarget,
  StatusValue,
} from "./model";
import type { PlayerCommand } from "./port";

// UPDATE
//
// Ports nearly all decision logic from player.ts (everything message.ts's
// scope comment claims). The one structural rule worth stating up front:
// `withPlayback` is the ONLY place that appends SyncNowPlayingMirror, and
// it must be called exactly once at the outermost return of any branch that
// changes playback identity — never from inside a helper another handler also
// wraps (see beginPlayback/playConcertItem below), or the mirror sync would
// fire twice for one logical transition.

type UpdateReturn = readonly [Model, ReadonlyArray<Command<Message>>];
const withUpdateReturn = M.withReturnType<UpdateReturn>();

// ── Small pure predicates / adapters ────────────────────────────────────

const toCoreState = (p: Model["playback"]): CorePlaybackState => ({
  concertId: p.concertId,
  trackIdx: p.trackIdx,
  isVideo: p.isVideo,
  watchUrl: p.watchUrl,
  hasNext: p.hasNext,
  hasPrev: p.hasPrev,
  liked: p.liked,
  concert: Option.getOrNull(p.concert),
});

const hasActiveMedia = (model: Model): boolean => model.playback.concertId !== null;
const isMediaPlaying = (model: Model): boolean => hasActiveMedia(model) && model.isPlaying;
const playerIdle = (model: Model): boolean => !hasActiveMedia(model) || model.playback.ended;

// ── Status helpers ───────────────────────────────────────────────────────

const withError = (model: Model, message: string): Model =>
  evo(model, { status: () => StatusValue.Error({ message }) });
const withBusy = (model: Model, message: string): Model =>
  evo(model, { status: () => StatusValue.Busy({ message }) });

/** Appends the now-playing mirror sync. Call exactly once, at the outermost
 *  return of a branch that changes playback identity — see ../mirror.ts. */
const withPlayback = (model: Model, commands: ReadonlyArray<Command<Message>>): UpdateReturn => [
  model,
  [...commands, SyncNowPlayingMirror({ concertId: model.playback.concertId, trackIdx: model.playback.trackIdx })],
];

// ── PlaySource → identity/URL derivation ────────────────────────────────

const targetIdFor = (source: PlaySource): { concertId: number; trackIdx: number | null } => {
  switch (source._tag) {
    case "Track":
      return { concertId: source.concertId, trackIdx: source.trackIdx };
    case "Album":
      return { concertId: source.concertId, trackIdx: null };
    case "ConcertItem":
      return { concertId: source.concertId, trackIdx: source.trackIdx };
  }
};

const listenUrlFor = (source: PlaySource): string | null => {
  switch (source._tag) {
    case "Track":
      return `/concerts/${source.concertId}/tracks/${source.trackIdx}/listen`;
    case "Album":
      return `/concerts/${source.concertId}/listen`;
    case "ConcertItem":
      return source.isInterlude || source.trackIdx === null
        ? null
        : `/concerts/${source.concertId}/tracks/${source.trackIdx}/listen`;
  }
};

const watchUrlFor = (source: PlaySource, info: MediaInfo): string | null => {
  if (!info.is_video) return null;
  switch (source._tag) {
    case "Track":
      return `/concerts/${source.concertId}/tracks/${source.trackIdx}/watch`;
    case "Album":
      return `/concerts/${source.concertId}/watch`;
    case "ConcertItem":
      // playConcertItem() always passes watchUrl: null, even for video items
      // (no per-item watch endpoint, and interludes have no trackIdx to build
      // one from). The view must therefore gate the Watch button on isVideo
      // alone, not on watchUrl — see view.ts's player-watch button.
      return null;
  }
};

// ── Begin playback (mirrors player.ts's play()) ─────────────────────────
//
// Raw [Model, Command[]] — deliberately does NOT call withPlayback itself, so
// callers (playConcertItemPure, which further restores playback.concert
// after calling this) can wrap the *final* result exactly once.
const beginPlayback = (
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
  const commands: ReadonlyArray<Command<Message>> = [
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
const refetchSidebarIfConcertChanged = (
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
const dispatchPlayTrack = (model: Model, concertId: number, trackIdx: number): UpdateReturn => {
  if (model.playback.concertId === concertId && model.playback.trackIdx === trackIdx) {
    return model.isPlaying ? [model, [PauseAudio()]] : [model, [ResumeAudio()]];
  }
  if (isMediaPlaying(model)) {
    return [model, [FetchTrackInfoForEnqueue({ concertId, trackIdx })]];
  }
  return [model, [FetchTrackInfo({ concertId, trackIdx, opts: defaultPlayOpts })]];
};

// ── Stop / advance-failure terminal states ──────────────────────────────

const stopPlaybackPure = (model: Model): UpdateReturn =>
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
const applyAdvanceFailure = (model: Model, plan: AdvancePlan): UpdateReturn => {
  switch (plan) {
    case "queue-only":
    case "next-or-none":
      return [evo(model, { isPlaying: () => false }), []];
    case "next-or-stop":
      return stopPlaybackPure(model);
    case "next-or-collapse":
      return [
        evo(model, { isPlaying: () => false, video: () => ({ open: false }) }),
        model.video.open ? [HideVideoPanel()] : [],
      ];
  }
};

const advanceToNextTrack = (model: Model, plan: AdvancePlan): UpdateReturn => {
  const { concertId, trackIdx } = model.playback;
  if (concertId === null || trackIdx === null) return applyAdvanceFailure(model, plan);
  return [model, [FetchNextTrackInfo({ concertId, trackIdx, plan })]];
};

const advanceAfterDelete = (model: Model): UpdateReturn => [
  model,
  [PauseAudio(), DrainQueue({ queue: model.queue, plan: "next-or-stop" })],
];

const advanceOrCollapse = (model: Model): UpdateReturn =>
  Option.isSome(model.playback.concert)
    ? advanceConcertPure(model)
    : [model, [DrainQueue({ queue: model.queue, plan: "next-or-collapse" })]];

// ── Concert reconstruction ───────────────────────────────────────────────

/** Looks up `concert.items[pos]` and synthesizes a MediaInfo-shaped object
 *  directly — concert items are already loaded, no fetch needed. Saves +
 *  restores playback.concert around the shared beginPlayback, mirroring
 *  playConcertItem()'s save/restore-state.concert pattern. */
function playConcertItemPure(model: Model, pos: number): UpdateReturn {
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
function advanceConcertPure(model: Model): UpdateReturn {
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
function playConcertPosOrEnd(model: Model): UpdateReturn {
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

const concertErrorMessages = { start: "Couldn't start concert", load: "Couldn't load concert" } as const;

// ── update ───────────────────────────────────────────────────────────────

export const update = (model: Model, message: Message): UpdateReturn =>
  M.value(message).pipe(
    withUpdateReturn,
    M.tagsExhaustive({
      CommandReceived: ({ command }) => handleCommand(model, command),

      ReceivedMediaInfo: ({ source, info, opts }) =>
        withPlayback(...refetchSidebarIfConcertChanged(model, beginPlayback(model, source, info, opts))),

      NotPlayable: ({ url }) => [model, [OpenInNewTab({ url })]],

      TrackMissing: ({ source }) => {
        if (source._tag !== "Track") return [model, []]; // album/concert-item never reach prepare in practice
        return [model, [PostPrepare({ target: PlayTargetValue.Track({ concertId: source.concertId, trackIdx: source.trackIdx }) })]];
      },

      FailedFetchInfo: ({ errorMessage: msg }) => [withError(model, msg), []],

      ReceivedTrackInfoForEnqueue: ({ concertId, trackIdx, info }) =>
        Option.match(info, {
          onNone: () => [model, [PostPrepare({ target: PlayTargetValue.Track({ concertId, trackIdx }) })]],
          onSome: ({ title, liked }) => {
            const result = enqueueDedupe(model.queue, makeQueueEntry(concertId, trackIdx, title, liked));
            return [evo(model, { queue: () => result.queue }), result.added ? [ScrollQueueToBottom()] : []];
          },
        }),

      ResolvedFirstAvailableTrack: ({ concertId, trackIdx }) =>
        Option.match(trackIdx, {
          onNone: () => [model, [PostPrepare({ target: PlayTargetValue.Track({ concertId, trackIdx: 0 }) })]],
          onSome: (idx) => dispatchPlayTrack(model, concertId, idx),
        }),

      ReceivedQueueDrainResult: ({ played, skippedCount, plan }) => {
        const playedCount = Option.isSome(played) ? skippedCount + 1 : skippedCount;
        const model1 = evo(model, { queue: (queue) => queue.slice(playedCount) });
        return Option.match(played, {
          onSome: ({ entry, info }) =>
            withPlayback(
              ...refetchSidebarIfConcertChanged(
                model1,
                beginPlayback(
                  model1,
                  PlaySourceValue.Track({ concertId: entry.concertId, trackIdx: entry.trackIdx }),
                  info,
                  { ...defaultPlayOpts, playlistName: entry.playlistName },
                ),
              ),
            ),
          onNone: () => (plan === "queue-only" ? [model1, []] : advanceToNextTrack(model1, plan)),
        });
      },

      FailedNextTrackInfo: ({ plan }) => {
        // "next-or-stop" calls stopPlaybackPure which clears status, so skip the error there.
        const model1 = plan === "next-or-stop"
          ? evo(model, { isPlaying: () => false })
          : withError(evo(model, { isPlaying: () => false }), "Couldn't load next track");
        return applyAdvanceFailure(model1, plan);
      },
      FailedPrevTrackInfo: () => [evo(model, { isPlaying: () => false }), []],

      ReceivedPrepareStart: ({ target, seedStatus }) => {
        const model1 = evo(model, { pending: () => Option.some(target), status: () => StatusValue.Busy({ message: "Preparing…" }) });
        const commands: ReadonlyArray<Command<Message>> =
          target._tag === "Track"
            ? [
                MarkPreparingExternal({ concertId: target.concertId, trackIdx: target.trackIdx }),
                DisableCardTracksExternal({ concertId: target.concertId }),
                RefreshCardStatus({ concertId: target.concertId }),
              ]
            : [DisableCardTracksExternal({ concertId: target.concertId }), RefreshCardStatus({ concertId: target.concertId })];
        return [model1, [...commands, PollPrepareStatus({ target, elapsedMs: 0, seedStatus })]];
      },

      FailedPrepareStart: () => [withError(model, "Prepare failed"), []],

      ReceivedPrepareStatus: ({ target, status, elapsedMs }) =>
        Option.match(model.pending, {
          onNone: () => [model, []],
          onSome: (pendingTarget) => {
            if (!sameTarget(pendingTarget, target)) return [model, []]; // superseded by a newer prepare
            const ready = target._tag === "Track" && status.tracks_present[target.trackIdx] === true;
            if (ready && target._tag === "Track") {
              const model1 = evo(model, { pending: () => Option.none() });
              const [model2, commands] = dispatchPlayTrack(model1, target.concertId, target.trackIdx);
              return [model2, [ClearPreparingExternal(), ...commands]];
            }
            if (status.download === "download-error" || status.split === "split-error") {
              return [evo(withError(model, "Preparing tracks failed"), { pending: () => Option.none() }), [ClearPreparingExternal()]];
            }
            if (elapsedMs > PREPARE_TIMEOUT_MS) {
              return [evo(withError(model, "Preparing tracks timed out"), { pending: () => Option.none() }), [ClearPreparingExternal()]];
            }
            const progress = status.split === "splitting" ? "Preparing… (splitting)" : "Preparing… (downloading)";
            return [withBusy(model, progress), [PollPrepareStatus({ target, elapsedMs, seedStatus: Option.none() })]];
          },
        }),

      FailedPollPrepareStatus: ({ target, elapsedMs }) =>
        Option.match(model.pending, {
          onNone: () => [model, []],
          onSome: (pendingTarget) => {
            if (!sameTarget(pendingTarget, target)) return [model, []];
            if (elapsedMs > PREPARE_TIMEOUT_MS) {
              return [evo(withError(model, "Preparing tracks timed out"), { pending: () => Option.none() }), [ClearPreparingExternal()]];
            }
            return [model, [PollPrepareStatus({ target, elapsedMs, seedStatus: Option.none() })]];
          },
        }),

      // Confirm server value (should match the optimistic flip, but carry it through).
      CompletedLikeToggle: ({ concertId, trackIdx, liked }) => [
        applyLikedEverywhere(model, concertId, trackIdx, liked),
        [],
      ],

      FailedLikeToggle: ({ concertId, trackIdx, attempted }) => {
        const reverted = !attempted;
        const isCurrentTrack = model.playback.concertId === concertId && model.playback.trackIdx === trackIdx;
        const model1 = applyLikedEverywhere(model, concertId, trackIdx, reverted);
        return [
          isCurrentTrack ? withError(model1, "Like failed") : model1,
          [SyncLikeButtonsExternal({ concertId, trackIdx: Option.some(trackIdx), liked: reverted })],
        ];
      },

      ReceivedDeleteTrackResult: ({ concertId, trackIdx, ok, source }) => {
        if (!ok) return [withError(model, "Delete failed"), []];
        if (source === "bar") {
          if (model.playback.concertId !== concertId || model.playback.trackIdx !== trackIdx) return [model, []];
          return advanceAfterDelete(model);
        }
        const inConcertMode = Option.isSome(model.playback.concert) && model.playback.concert.value.id === concertId;
        if (!inConcertMode) {
          // Whole-album sidebar: grey the deleted row, and if it was the playing
          // track, advance like the bar-source delete does.
          const model1 = flipSidebarTrackAvailable(model, concertId, trackIdx, false);
          if (model.playback.concertId === concertId && model.playback.trackIdx === trackIdx) {
            return advanceAfterDelete(model1);
          }
          return [model1, []];
        }
        const wasPlayingThis = model.playback.concertId === concertId && model.playback.trackIdx === trackIdx;
        return [model, [RefreshConcertItems({ concertId, advanceAfter: wasPlayingThis })]];
      },

      ReceivedConcertItems: ({ concertId, items, advanceAfter }) =>
        Option.match(model.playback.concert, {
          onNone: () => [model, []],
          onSome: (concert) => {
            if (concert.id !== concertId) return [model, []];
            const currentItem = concert.items[concert.pos] ?? null;
            const pos = refindPosByUrl(items, currentItem ? currentItem.url : null, concert.pos);
            const model1 = evo(model, {
              playback: () => evo(model.playback, { concert: () => Option.some(evo(concert, { items: () => items, pos: () => pos })) }),
            });
            return advanceAfter ? playConcertPosOrEnd(model1) : [model1, []];
          },
        }),
      FailedConcertItems: () => [model, []], // bare catch in the original — not user-visible

      ReceivedConcertPlaybackItems: ({ concertId, items, atPos }) =>
        playConcertPosOrEnd(
          evo(model, {
            playback: () => evo(model.playback, { concert: () => Option.some({ id: concertId, items, pos: atPos }) }),
          }),
        ),
      FailedConcertPlayback: ({ errorMessage: msg }) => [withError(model, msg), []],

      CompletedDeleteInterlude: ({ concertId, wasPlayingThis }) => {
        const inConcertMode = Option.isSome(model.playback.concert) && model.playback.concert.value.id === concertId;
        return inConcertMode ? [model, [RefreshConcertItems({ concertId, advanceAfter: wasPlayingThis })]] : [model, []];
      },
      FailedDeleteInterlude: () => [withError(model, "Delete failed"), []],

      ReceivedPlaylistTracks: ({ tracks, name }) => {
        if (Array.isReadonlyArrayEmpty(tracks)) return [withError(model, "Nothing to play in this playlist"), []];
        const groupId = model.nextGroupId;
        const entries = tracks.map((track) =>
          makeQueueEntry(track.concertId, track.trackIdx, track.title, false, name, groupId),
        );
        const model1 = evo(model, { queue: (queue) => [...queue, ...entries], nextGroupId: () => groupId + 1 });
        return playerIdle(model1)
          ? [model1, [DrainQueue({ queue: model1.queue, plan: "queue-only" }), ScrollQueueToBottom()]]
          : [model1, [ScrollQueueToBottom()]];
      },
      FailedPlaylistLoad: () => [withError(model, "Couldn't load playlist"), []],

      ReceivedTrackDetails: ({ concertId, loadGen, tracksBusy, tracks }) => {
        if (model.sidebar.loadGen !== loadGen) return [model, []]; // stale — newer fetch started
        if (model.playback.concertId !== concertId) return [model, []]; // concert changed
        return [
          evo(model, { sidebar: () => evo(model.sidebar, { tracks: () => Option.some({ tracksBusy, tracks }) }) }),
          [],
        ];
      },
      FailedTrackDetails: () => [model, []], // sidebar stays at Option.none(); not user-visible

      FailedOpenExternal: () => [withError(model, "Couldn't open externally"), []],

      AudioPlaying: () => [evo(model, { isPlaying: () => true }), []],
      AudioPaused: () => [evo(model, { isPlaying: () => false }), []],
      AudioEnded: () =>
        advanceOrCollapse(evo(model, { playback: () => evo(model.playback, { ended: () => true }) })),
      AudioErrored: () =>
        advanceOrCollapse(
          withError(
            evo(model, { playback: () => evo(model.playback, { ended: () => true }) }),
            "Failed to load media",
          ),
        ),
      AudioPlayRejected: () => [withError(evo(model, { isPlaying: () => false }), "Playback blocked"), []],

      // ── Subscription-dispatched messages ──────────────────────────────
      // Re-stamp playing/preparing CSS markers after htmx:afterSettle /
      // historyRestore, mirroring player.ts's reassertPlayerUi().
      ReassertUi: () => {
        const { concertId, trackIdx } = model.playback;
        const markPlaying =
          concertId !== null ? [MarkPlayingExternal({ concertId, trackIdx: Option.fromNullishOr(trackIdx) })] : [];
        const markPreparing = Option.match(model.pending, {
          onNone: () => [],
          onSome: (target) =>
            target._tag === "Track"
              ? [MarkPreparingExternal({ concertId: target.concertId, trackIdx: target.trackIdx })]
              : [],
        });
        return [model, [...markPlaying, ...markPreparing]];
      },

      // htmx swapped in new like-button HTML; sync our model copies so bar
      // star + sidebar list reflect the server's authoritative liked value.
      SyncLikeFromSwap: ({ concertId, trackIdx, liked }) => [
        applyLikedEverywhere(model, concertId, trackIdx, liked),
        [],
      ],

      PressedSpace: () => (model.isPlaying ? [model, [PauseAudio()]] : [model, [ResumeAudio()]]),

      PressedEscape: () =>
        model.video.open
          ? [evo(model, { video: () => ({ open: false }) }), [HideVideoPanel()]]
          : [model, []],

      ClickedOutsideVideo: () =>
        model.video.open
          ? [evo(model, { video: () => ({ open: false }) }), [HideVideoPanel()]]
          : [model, []],

      MovedSidebarDrag: ({ clientX }) => [model, [SetSidebarWidthVar({ px: clientX })]],

      ReleasedSidebarDrag: ({ clientX, moved }) =>
        moved
          ? [model, [SetSidebarWidthVar({ px: clientX }), PersistSidebarWidth({ px: clientX })]]
          : [model, []],

      Acked: () => [model, []],
    }),
  );

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
function flipSidebarTrackAvailable(model: Model, concertId: number, trackIdx: number, available: boolean): Model {
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
 *  CompletedLikeToggle/FailedLikeToggle, and the htmx-swap SyncLikeFromSwap). */
function applyLikedEverywhere(model: Model, concertId: number, trackIdx: number, liked: boolean): Model {
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
function findCurrentLiked(model: Model, concertId: number, trackIdx: number): Option.Option<boolean> {
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

// ── PlayerCommand dispatch (host calls in via the single inbound Port) ──

function handleCommand(model: Model, command: PlayerCommand): UpdateReturn {
  return M.value(command).pipe(
    withUpdateReturn,
    M.tagsExhaustive({
      PlayAlbum: ({ concertId }) => [model, [FetchAlbumInfo({ concertId, opts: defaultPlayOpts })]],
      PlayTrack: ({ concertId, trackIdx }) => dispatchPlayTrack(model, concertId, trackIdx),
      PlayTracks: ({ concertId }) => [model, [ResolveFirstAvailableTrack({ concertId })]],
      StartAlbum: ({ concertId, recordListen }) => [
        model,
        [FetchAlbumInfo({ concertId, opts: { recordListen, playlistName: null, openVideoPanel: false } })],
      ],
      StartTrack: ({ concertId, trackIdx }) => [model, [FetchTrackInfo({ concertId, trackIdx, opts: defaultPlayOpts })]],

      TogglePause: () => (model.isPlaying ? [model, [PauseAudio()]] : [model, [ResumeAudio()]]),
      Seek: ({ seconds }) => [model, [SeekAudio({ seconds })]],

      SkipToNext: () => {
        if (Option.isSome(model.playback.concert)) {
          const [model2, commands] = advanceConcertPure(model);
          return [model2, [PauseAudio(), ...commands]];
        }
        if (!nextEnabled(toCoreState(model.playback), model.queue.length)) return [model, []];
        return [model, [PauseAudio(), DrainQueue({ queue: model.queue, plan: "next-or-none" })]];
      },
      SkipToPrev: () => {
        if (Option.isSome(model.playback.concert)) {
          const concert = model.playback.concert.value;
          if (concert.pos <= 0) return [model, []];
          const [model2, commands] = playConcertItemPure(model, concert.pos - 1);
          return [model2, [PauseAudio(), ...commands]];
        }
        if (!prevEnabled(toCoreState(model.playback))) return [model, []];
        if (model.playback.concertId === null || model.playback.trackIdx === null) return [model, []];
        return [
          model,
          [PauseAudio(), FetchPrevTrackInfo({ concertId: model.playback.concertId, trackIdx: model.playback.trackIdx })],
        ];
      },

      Watch: () => {
        const open = !model.video.open;
        return [evo(model, { video: () => ({ open }) }), [open ? ShowVideoPanel() : HideVideoPanel()]];
      },
      OpenExternal: () =>
        model.playback.watchUrl === null
          ? [model, []]
          : [model, [PauseAudio(), OpenExternalRequest({ url: model.playback.watchUrl })]],
      WatchTrackDirect: ({ concertId, trackIdx }) => [
        model,
        [FetchTrackInfo({ concertId, trackIdx, opts: { recordListen: true, playlistName: null, openVideoPanel: true } })],
      ],

      ToggleLike: () => {
        if (model.playback.trackIdx === null || model.playback.concertId === null) return [model, []];
        const { concertId, trackIdx } = model.playback;
        const next = !model.playback.liked;
        return [
          applyLikedEverywhere(model, concertId, trackIdx, next),
          [
            ToggleLikeRequest({ concertId, trackIdx, next }),
            SyncLikeButtonsExternal({ concertId, trackIdx: Option.some(trackIdx), liked: next }),
          ],
        ];
      },
      DeleteTrack: () =>
        model.playback.trackIdx === null || model.playback.concertId === null
          ? [model, []]
          : [model, [DeleteTrackRequest({ concertId: model.playback.concertId, trackIdx: model.playback.trackIdx, source: "bar" })]],

      OpenConcert: () =>
        model.playback.concertId === null ? [model, []] : [model, [NavigateToConcert({ concertId: model.playback.concertId })]],
      OpenSidebar: () => {
        const model1 = evo(model, { sidebar: () => evo(model.sidebar, { open: () => true }) });
        // Whole-album mode: fetch the track list. Reconstruction mode (concert
        // Some) renders from model.playback.concert.items — no fetch needed.
        const concertId = model.playback.concertId;
        if (concertId !== null && Option.isNone(model.playback.concert)) {
          const loadGen = model.sidebar.loadGen + 1;
          return [
            evo(model1, { sidebar: () => evo(model1.sidebar, { loadGen: () => loadGen }) }),
            [MutateBodyClass({ className: "sidebar-open", add: true }), FetchTrackDetails({ concertId, loadGen })],
          ];
        }
        return [model1, [MutateBodyClass({ className: "sidebar-open", add: true })]];
      },
      CloseSidebar: () => [
        evo(model, { sidebar: () => evo(model.sidebar, { open: () => false }) }),
        [MutateBodyClass({ className: "sidebar-open", add: false })],
      ],
      ToggleSidebar: () => {
        const opening = !model.sidebar.open;
        const model1 = evo(model, { sidebar: () => evo(model.sidebar, { open: () => opening }) });
        const concertId = model.playback.concertId;
        if (opening && concertId !== null && Option.isNone(model.playback.concert)) {
          const loadGen = model.sidebar.loadGen + 1;
          return [
            evo(model1, { sidebar: () => evo(model1.sidebar, { loadGen: () => loadGen }) }),
            [MutateBodyClass({ className: "sidebar-open", add: opening }), FetchTrackDetails({ concertId, loadGen })],
          ];
        }
        return [model1, [MutateBodyClass({ className: "sidebar-open", add: opening })]];
      },
      SidebarDeleteTrack: ({ concertId, trackIdx }) => [model, [DeleteTrackRequest({ concertId, trackIdx, source: "sidebar" })]],

      PlayQueueEntryNow: ({ pos }) => {
        const entry = model.queue[pos];
        if (!entry) return [model, []];
        const model1 = evo(model, { queue: (queue) => dequeueAt(queue, pos) });
        return [model1, [FetchTrackInfo({ concertId: entry.concertId, trackIdx: entry.trackIdx, opts: defaultPlayOpts })]];
      },
      Dequeue: ({ pos }) => [evo(model, { queue: (queue) => dequeueAt(queue, pos) }), []],
      RemoveGroup: ({ groupId }) => [evo(model, { queue: (queue) => removeGroup(queue, groupId) }), []],
      Enqueue: ({ concertId, trackIdx, title, liked }) => {
        const result = enqueueDedupe(model.queue, makeQueueEntry(concertId, trackIdx, title, liked));
        return [evo(model, { queue: () => result.queue }), result.added ? [ScrollQueueToBottom()] : []];
      },

      PlayAlbumAt: ({ concertId, seconds }) => {
        if (model.playback.concertId === concertId && model.playback.trackIdx === null) {
          return [model, [SeekAudio({ seconds }), ...(model.isPlaying ? [] : [ResumeAudio()])]];
        }
        return [
          evo(model, { pendingSeek: () => Option.some(seconds) }),
          [FetchAlbumInfo({ concertId, opts: { recordListen: false, playlistName: null, openVideoPanel: false } })],
        ];
      },
      PlayPlaylist: ({ playlistId }) => [model, [FetchPlaylistForPlay({ playlistId })]],

      AddToPlaylist: () =>
        model.playback.trackIdx === null || model.playback.concertId === null
          ? [model, []]
          : [
              model,
              [
                OpenAddToPlaylist({
                  concertId: model.playback.concertId,
                  trackIdx: model.playback.trackIdx,
                  label: model.playback.title,
                }),
              ],
            ],

      StopPlayback: () => stopPlaybackPure(model),

      PlayConcert: ({ concertId }) => [
        model,
        [FetchConcertPlayback({ concertId, atPos: Option.none(), errorMessage: concertErrorMessages.start })],
      ],
      PlayConcertFrom: ({ concertId, pos }) =>
        Option.match(model.playback.concert, {
          onSome: (concert) =>
            concert.id === concertId
              ? playConcertItemPure(
                  evo(model, {
                    playback: () => evo(model.playback, { concert: () => Option.some(evo(concert, { pos: () => pos })) }),
                  }),
                  pos,
                )
              : [model, [FetchConcertPlayback({ concertId, atPos: Option.some(pos), errorMessage: concertErrorMessages.load })]],
          onNone: () => [model, [FetchConcertPlayback({ concertId, atPos: Option.some(pos), errorMessage: concertErrorMessages.load })]],
        }),
      SidebarDeleteInterlude: ({ concertId, interludeIdx }) => {
        const wasPlayingThis = Option.match(model.playback.concert, {
          onNone: () => false,
          onSome: (concert) => {
            const cur = concert.items[concert.pos];
            return !!(cur && cur.kind === "interlude" && cur.interlude_index === interludeIdx);
          },
        });
        return [model, [PostDeleteInterlude({ concertId, interludeIdx, wasPlayingThis })]];
      },

      SidebarLikeTrack: ({ concertId, trackIdx }) => {
        const maybeCurrentLiked = findCurrentLiked(model, concertId, trackIdx);
        if (Option.isNone(maybeCurrentLiked)) return [model, []]; // track not in any loaded list

        const next = !maybeCurrentLiked.value;
        return [
          applyLikedEverywhere(model, concertId, trackIdx, next),
          [
            ToggleLikeRequest({ concertId, trackIdx, next }),
            SyncLikeButtonsExternal({ concertId, trackIdx: Option.some(trackIdx), liked: next }),
          ],
        ];
      },

      SidebarAddToPlaylist: ({ concertId, trackIdx, label }) => [
        model,
        [OpenAddToPlaylist({ concertId, trackIdx, label })],
      ],
    }),
  );
}
