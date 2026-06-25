import { Match as M, Option } from "effect";
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
  ShowVideoPanel,
  SyncLikeButtonsExternal,
  SyncNowPlayingMirrorCmd,
  ToggleLikeRequest,
} from "./command";
import type { Message } from "./message";
import {
  type AdvancePlan,
  defaultPlayOpts,
  type MediaInfo,
  type Model,
  type PlayOpts,
  type PlaySource,
  PlaySourceValue,
  type PlayTarget,
  PlayTargetValue,
  StatusValue,
} from "./model";
import type { PlayerCommand } from "./port";

// UPDATE
//
// Ports nearly all decision logic from player.ts (everything message.ts's
// scope comment claims). The one structural rule worth stating up front:
// `withPlayback` is the ONLY place that appends SyncNowPlayingMirrorCmd, and
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
const withPlayback = (model: Model, cmds: ReadonlyArray<Command<Message>>): UpdateReturn => [
  model,
  [...cmds, SyncNowPlayingMirrorCmd({ concertId: model.playback.concertId, trackIdx: model.playback.trackIdx })],
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
      // playConcertItem() always passes watchUrl: null, even for video items.
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
): readonly [Model, Command<Message>[]] => {
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

  const cmds: Command<Message>[] = [
    PlayAudio({ url: info.url }),
    MarkPlayingExternal({ concertId, trackIdx: Option.fromNullishOr(trackIdx) }),
    ClearPreparingExternal({}),
  ];
  if (newVideoOpen !== model.video.open) cmds.push(newVideoOpen ? ShowVideoPanel({}) : HideVideoPanel({}));
  if (listenUrl && opts.recordListen) cmds.push(RecordListenEvent({ url: listenUrl }));
  if (Option.isSome(model.pendingSeek)) cmds.push(SeekAudio({ seconds: model.pendingSeek.value }));

  return [evo(model1, { pendingSeek: () => Option.none() }), cmds];
};

/** playTrack()/PlayerApi.playTrack's shared dispatch: same-track toggles
 *  pause/resume, something-else-playing enqueues, otherwise fetches+plays.
 *  Used by CommandReceived.PlayTrack and the prepare-ready path (applyPrepareStatus
 *  also calls playTrack, not startTrack). */
const dispatchPlayTrack = (model: Model, concertId: number, trackIdx: number): UpdateReturn => {
  if (model.playback.concertId === concertId && model.playback.trackIdx === trackIdx) {
    return model.isPlaying ? [model, [PauseAudio({})]] : [model, [ResumeAudio({})]];
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
      playback: () => ({
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
      }),
      queue: () => [],
      sidebar: () => ({ open: false, tracks: Option.none(), loadGen: 0 }),
      video: () => ({ open: false }),
      isPlaying: () => false,
      pendingSeek: () => Option.none(),
      status: () => StatusValue.Idle(),
      // pending intentionally untouched — stopPlayback() cancels auto-advance,
      // never a prepare-in-flight (cancelPendingPlay is never called there).
    }),
    model.video.open ? [ClearAudioSrc({}), HideVideoPanel({})] : [ClearAudioSrc({})],
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
        model.video.open ? [HideVideoPanel({})] : [],
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
  [PauseAudio({}), DrainQueue({ queue: model.queue, plan: "next-or-stop" })],
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
      const [model2, cmds] = beginPlayback(model, source, info, defaultPlayOpts);
      const model3 = evo(model2, { playback: (p) => ({ ...p, concert: Option.some({ ...concert, pos }) }) });
      const extraCmds: Command<Message>[] =
        isInterlude && item.interlude_index != null
          ? [MarkPlayingInterludeExternal({ concertId: concert.id, interludeIdx: item.interlude_index })]
          : [];
      return withPlayback(model3, [...cmds, ...extraCmds]);
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
              playback: (p) => ({ ...p, concert: Option.none() }),
              video: () => ({ open: false }),
            }),
            model.video.open ? [HideVideoPanel({})] : [],
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
              playback: (p) => ({ ...p, concert: Option.none() }),
              video: () => ({ open: false }),
            }),
            model.video.open ? [HideVideoPanel({})] : [],
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

      ReceivedMediaInfo: ({ source, info, opts }) => withPlayback(...beginPlayback(model, source, info, opts)),

      NotPlayable: ({ url }) => [model, [OpenInNewTab({ url })]],

      TrackMissing: ({ source }) => {
        if (source._tag !== "Track") return [model, []]; // album/concert-item never reach prepare in practice
        return [model, [PostPrepare({ target: PlayTargetValue.Track({ concertId: source.concertId, trackIdx: source.trackIdx }) })]];
      },

      FailedFetchInfo: ({ message: msg }) => [withError(model, msg), []],

      ReceivedTrackInfoForEnqueue: ({ concertId, trackIdx, info }) =>
        Option.match(info, {
          onNone: () => [model, [PostPrepare({ target: PlayTargetValue.Track({ concertId, trackIdx }) })]],
          onSome: ({ title, liked }) => {
            const result = enqueueDedupe(model.queue, makeQueueEntry(concertId, trackIdx, title, liked));
            return [evo(model, { queue: () => result.queue }), result.added ? [ScrollQueueToBottom({})] : []];
          },
        }),

      ResolvedFirstAvailableTrack: ({ concertId, trackIdx }) =>
        Option.match(trackIdx, {
          onNone: () => [model, [PostPrepare({ target: PlayTargetValue.Track({ concertId, trackIdx: 0 }) })]],
          onSome: (idx) => dispatchPlayTrack(model, concertId, idx),
        }),

      ReceivedQueueDrainResult: ({ played, skippedCount, plan }) => {
        const playedCount = Option.isSome(played) ? skippedCount + 1 : skippedCount;
        const model1 = evo(model, { queue: (q) => q.slice(playedCount) });
        return Option.match(played, {
          onSome: ({ entry, info }) =>
            withPlayback(...beginPlayback(model1, PlaySourceValue.Track({ concertId: entry.concertId, trackIdx: entry.trackIdx }), info, defaultPlayOpts)),
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
        const cmds: Command<Message>[] =
          target._tag === "Track"
            ? [
                MarkPreparingExternal({ concertId: target.concertId, trackIdx: target.trackIdx }),
                DisableCardTracksExternal({ concertId: target.concertId }),
                RefreshCardStatus({ concertId: target.concertId }),
              ]
            : [DisableCardTracksExternal({ concertId: target.concertId }), RefreshCardStatus({ concertId: target.concertId })];
        return [model1, [...cmds, PollPrepareStatus({ target, elapsedMs: 0, seedStatus })]];
      },

      FailedPrepareStart: () => [withError(model, "Prepare failed"), []],

      ReceivedPrepareStatus: ({ target, status, elapsedMs }) =>
        Option.match(model.pending, {
          onNone: () => [model, []],
          onSome: (pendingTarget) => {
            if (!sameTargetLocal(pendingTarget, target)) return [model, []]; // superseded by a newer prepare
            const ready = target._tag === "Track" && status.tracks_present[target.trackIdx] === true;
            if (ready && target._tag === "Track") {
              const model1 = evo(model, { pending: () => Option.none() });
              const [model2, cmds] = dispatchPlayTrack(model1, target.concertId, target.trackIdx);
              return [model2, [ClearPreparingExternal({}), ...cmds]];
            }
            if (status.download === "download-error" || status.split === "split-error") {
              return [evo(withError(model, "Preparing tracks failed"), { pending: () => Option.none() }), [ClearPreparingExternal({})]];
            }
            if (elapsedMs > PREPARE_TIMEOUT_MS) {
              return [evo(withError(model, "Preparing tracks timed out"), { pending: () => Option.none() }), [ClearPreparingExternal({})]];
            }
            const progress = status.split === "splitting" ? "Preparing… (splitting)" : "Preparing… (downloading)";
            return [withBusy(model, progress), [PollPrepareStatus({ target, elapsedMs, seedStatus: Option.none() })]];
          },
        }),

      FailedPollPrepareStatus: ({ target, elapsedMs }) =>
        Option.match(model.pending, {
          onNone: () => [model, []],
          onSome: (pendingTarget) => {
            if (!sameTargetLocal(pendingTarget, target)) return [model, []];
            if (elapsedMs > PREPARE_TIMEOUT_MS) {
              return [evo(withError(model, "Preparing tracks timed out"), { pending: () => Option.none() }), [ClearPreparingExternal({})]];
            }
            return [model, [PollPrepareStatus({ target, elapsedMs, seedStatus: Option.none() })]];
          },
        }),

      CompletedLikeToggle: ({ concertId, trackIdx, liked }) => {
        // Confirm server value (should match the optimistic flip, but carry it through).
        let model1 = model;
        if (model.playback.concertId === concertId && model.playback.trackIdx === trackIdx) {
          model1 = evo(model1, { playback: (p) => ({ ...p, liked }) });
        }
        model1 = flipSidebarTrackLiked(model1, concertId, trackIdx, liked);
        model1 = flipConcertItemLiked(model1, concertId, trackIdx, liked);
        return [model1, []];
      },

      FailedLikeToggle: ({ concertId, trackIdx, attempted }) => {
        const reverted = !attempted;
        let model1 = model;
        const isCurrentTrack =
          model.playback.concertId === concertId && model.playback.trackIdx === trackIdx;
        if (isCurrentTrack) {
          model1 = withError(evo(model1, { playback: (p) => ({ ...p, liked: reverted }) }), "Like failed");
        }
        model1 = flipSidebarTrackLiked(model1, concertId, trackIdx, reverted);
        model1 = flipConcertItemLiked(model1, concertId, trackIdx, reverted);
        return [model1, [SyncLikeButtonsExternal({ concertId, trackIdx: Option.some(trackIdx), liked: reverted })]];
      },

      ReceivedDeleteTrackResult: ({ concertId, trackIdx, ok, source }) => {
        if (!ok) return [withError(model, "Delete failed"), []];
        if (source === "bar") {
          if (model.playback.concertId !== concertId || model.playback.trackIdx !== trackIdx) return [model, []];
          return advanceAfterDelete(model);
        }
        const inConcertMode = Option.isSome(model.playback.concert) && model.playback.concert.value.id === concertId;
        if (!inConcertMode) return [model, []];
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
            const model1 = evo(model, { playback: (p) => ({ ...p, concert: Option.some({ ...concert, items, pos }) }) });
            return advanceAfter ? playConcertPosOrEnd(model1) : [model1, []];
          },
        }),
      FailedConcertItems: () => [model, []], // bare catch in the original — not user-visible

      ReceivedConcertPlaybackItems: ({ concertId, items, atPos }) =>
        playConcertPosOrEnd(
          evo(model, { playback: (p) => ({ ...p, concert: Option.some({ id: concertId, items, pos: atPos }) }) }),
        ),
      FailedConcertPlayback: ({ message: msg }) => [withError(model, msg), []],

      CompletedDeleteInterlude: ({ concertId, wasPlayingThis }) => {
        const inConcertMode = Option.isSome(model.playback.concert) && model.playback.concert.value.id === concertId;
        return inConcertMode ? [model, [RefreshConcertItems({ concertId, advanceAfter: wasPlayingThis })]] : [model, []];
      },
      FailedDeleteInterlude: () => [withError(model, "Delete failed"), []],

      ReceivedPlaylistTracks: ({ tracks, name }) => {
        if (tracks.length === 0) return [withError(model, "Nothing to play in this playlist"), []];
        const groupId = model.nextGroupId;
        const entries = tracks.map((t) => makeQueueEntry(t.concertId, t.trackIdx, t.title, false, name, groupId));
        const model1 = evo(model, { queue: (q) => [...q, ...entries], nextGroupId: () => groupId + 1 });
        return playerIdle(model1)
          ? [model1, [DrainQueue({ queue: model1.queue, plan: "queue-only" }), ScrollQueueToBottom({})]]
          : [model1, [ScrollQueueToBottom({})]];
      },
      FailedPlaylistLoad: () => [withError(model, "Couldn't load playlist"), []],

      ReceivedTrackDetails: ({ concertId, loadGen, tracksBusy, tracks }) => {
        if (model.sidebar.loadGen !== loadGen) return [model, []]; // stale — newer fetch started
        if (model.playback.concertId !== concertId) return [model, []]; // concert changed
        return [evo(model, { sidebar: (s) => ({ ...s, tracks: Option.some({ tracksBusy, tracks }) }) }), []];
      },
      FailedTrackDetails: () => [model, []], // sidebar stays at Option.none(); not user-visible

      FailedOpenExternal: () => [withError(model, "Couldn't open externally"), []],

      AudioPlaying: () => [evo(model, { isPlaying: () => true }), []],
      AudioPaused: () => [evo(model, { isPlaying: () => false }), []],
      AudioEnded: () => advanceOrCollapse(evo(model, { playback: (p) => ({ ...p, ended: true }) })),
      AudioErrored: () => advanceOrCollapse(withError(evo(model, { playback: (p) => ({ ...p, ended: true }) }), "Failed to load media")),
      AudioPlayRejected: () => [withError(evo(model, { isPlaying: () => false }), "Playback blocked"), []],

      // ── Subscription-dispatched messages ──────────────────────────────
      ReassertUi: () => {
        // Re-stamp playing/preparing CSS markers after htmx:afterSettle /
        // historyRestore, mirroring player.ts's reassertPlayerUi().
        const cmds: Command<Message>[] = [];
        const { concertId, trackIdx } = model.playback;
        if (concertId !== null) {
          cmds.push(MarkPlayingExternal({ concertId, trackIdx: Option.fromNullishOr(trackIdx) }));
        }
        if (Option.isSome(model.pending) && model.pending.value._tag === "Track") {
          const t = model.pending.value;
          cmds.push(MarkPreparingExternal({ concertId: t.concertId, trackIdx: t.trackIdx }));
        }
        return [model, cmds];
      },

      SyncLikeFromSwap: ({ concertId, trackIdx, liked }) => {
        // htmx swapped in new like-button HTML; sync our model copies so bar
        // star + sidebar list reflect the server's authoritative liked value.
        let model1 = model;
        if (model.playback.concertId === concertId && model.playback.trackIdx === trackIdx) {
          model1 = evo(model1, { playback: (p) => ({ ...p, liked }) });
        }
        model1 = flipSidebarTrackLiked(model1, concertId, trackIdx, liked);
        model1 = flipConcertItemLiked(model1, concertId, trackIdx, liked);
        return [model1, []];
      },

      PressedSpace: () => (model.isPlaying ? [model, [PauseAudio({})]] : [model, [ResumeAudio({})]]),

      PressedEscape: () =>
        model.video.open
          ? [evo(model, { video: () => ({ open: false }) }), [HideVideoPanel({})]]
          : [model, []],

      ClickedOutsideVideo: () =>
        model.video.open
          ? [evo(model, { video: () => ({ open: false }) }), [HideVideoPanel({})]]
          : [model, []],

      Acked: () => [model, []],
    }),
  );

/** Local sameTarget over PlayTarget — model.ts already exports one, reused
 *  here under a distinct name to avoid colliding with PlaySource's own
 *  identity helpers above. */
function sameTargetLocal(a: PlayTarget, b: PlayTarget): boolean {
  if (a._tag !== b._tag) return false;
  if (a._tag === "Track" && b._tag === "Track") return a.concertId === b.concertId && a.trackIdx === b.trackIdx;
  if (a._tag === "Album" && b._tag === "Album") return a.concertId === b.concertId;
  return false;
}

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
      const updated = sidebarTracks.tracks.map((t) => (t.index === trackIdx ? { ...t, liked } : t));
      return evo(model, { sidebar: (s) => ({ ...s, tracks: Option.some({ ...sidebarTracks, tracks: updated }) }) });
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
      return evo(model, { playback: (p) => ({ ...p, concert: Option.some({ ...concert, items: updated }) }) });
    },
  });
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

      TogglePause: () => (model.isPlaying ? [model, [PauseAudio({})]] : [model, [ResumeAudio({})]]),
      Seek: ({ seconds }) => [model, [SeekAudio({ seconds })]],

      SkipToNext: () => {
        if (Option.isSome(model.playback.concert)) {
          const [model2, cmds] = advanceConcertPure(model);
          return [model2, [PauseAudio({}), ...cmds]];
        }
        if (!nextEnabled(toCoreState(model.playback), model.queue.length)) return [model, []];
        return [model, [PauseAudio({}), DrainQueue({ queue: model.queue, plan: "next-or-none" })]];
      },
      SkipToPrev: () => {
        if (Option.isSome(model.playback.concert)) {
          const concert = model.playback.concert.value;
          if (concert.pos <= 0) return [model, []];
          const [model2, cmds] = playConcertItemPure(model, concert.pos - 1);
          return [model2, [PauseAudio({}), ...cmds]];
        }
        if (!prevEnabled(toCoreState(model.playback))) return [model, []];
        if (model.playback.concertId === null || model.playback.trackIdx === null) return [model, []];
        return [
          model,
          [PauseAudio({}), FetchPrevTrackInfo({ concertId: model.playback.concertId, trackIdx: model.playback.trackIdx })],
        ];
      },

      Watch: () => {
        const open = !model.video.open;
        return [evo(model, { video: () => ({ open }) }), [open ? ShowVideoPanel({}) : HideVideoPanel({})]];
      },
      OpenExternal: () =>
        model.playback.watchUrl === null
          ? [model, []]
          : [model, [PauseAudio({}), OpenExternalRequest({ url: model.playback.watchUrl })]],
      WatchTrackDirect: ({ concertId, trackIdx }) => [
        model,
        [FetchTrackInfo({ concertId, trackIdx, opts: { recordListen: true, playlistName: null, openVideoPanel: true } })],
      ],

      ToggleLike: () => {
        if (model.playback.trackIdx === null || model.playback.concertId === null) return [model, []];
        const { concertId, trackIdx } = model.playback;
        const next = !model.playback.liked;
        let model1 = evo(model, { playback: (p) => ({ ...p, liked: next }) });
        model1 = flipSidebarTrackLiked(model1, concertId, trackIdx, next);
        model1 = flipConcertItemLiked(model1, concertId, trackIdx, next);
        return [
          model1,
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
        const model1 = evo(model, { sidebar: (s) => ({ ...s, open: true }) });
        // Whole-album mode: fetch the track list. Reconstruction mode (concert
        // Some) renders from model.playback.concert.items — no fetch needed.
        const concertId = model.playback.concertId;
        if (concertId !== null && Option.isNone(model.playback.concert)) {
          const loadGen = model.sidebar.loadGen + 1;
          return [
            evo(model1, { sidebar: (s) => ({ ...s, loadGen }) }),
            [MutateBodyClass({ className: "sidebar-open", add: true }), FetchTrackDetails({ concertId, loadGen })],
          ];
        }
        return [model1, [MutateBodyClass({ className: "sidebar-open", add: true })]];
      },
      CloseSidebar: () => [
        evo(model, { sidebar: (s) => ({ ...s, open: false }) }),
        [MutateBodyClass({ className: "sidebar-open", add: false })],
      ],
      ToggleSidebar: () => {
        const opening = !model.sidebar.open;
        const model1 = evo(model, { sidebar: (s) => ({ ...s, open: opening }) });
        const concertId = model.playback.concertId;
        if (opening && concertId !== null && Option.isNone(model.playback.concert)) {
          const loadGen = model.sidebar.loadGen + 1;
          return [
            evo(model1, { sidebar: (s) => ({ ...s, loadGen }) }),
            [MutateBodyClass({ className: "sidebar-open", add: opening }), FetchTrackDetails({ concertId, loadGen })],
          ];
        }
        return [model1, [MutateBodyClass({ className: "sidebar-open", add: opening })]];
      },
      SidebarDeleteTrack: ({ concertId, trackIdx }) => [model, [DeleteTrackRequest({ concertId, trackIdx, source: "sidebar" })]],

      PlayQueueEntryNow: ({ pos }) => {
        const entry = model.queue[pos];
        if (!entry) return [model, []];
        const model1 = evo(model, { queue: (q) => dequeueAt(q, pos) });
        return [model1, [FetchTrackInfo({ concertId: entry.concertId, trackIdx: entry.trackIdx, opts: defaultPlayOpts })]];
      },
      Dequeue: ({ pos }) => [evo(model, { queue: (q) => dequeueAt(q, pos) }), []],
      Enqueue: ({ concertId, trackIdx, title, liked }) => {
        const result = enqueueDedupe(model.queue, makeQueueEntry(concertId, trackIdx, title, liked));
        return [evo(model, { queue: () => result.queue }), result.added ? [ScrollQueueToBottom({})] : []];
      },

      PlayAlbumAt: ({ concertId, seconds }) => {
        if (model.playback.concertId === concertId && model.playback.trackIdx === null) {
          const cmds: Command<Message>[] = [SeekAudio({ seconds })];
          if (!model.isPlaying) cmds.push(ResumeAudio({}));
          return [model, cmds];
        }
        return [evo(model, { pendingSeek: () => Option.some(seconds) }), [FetchAlbumInfo({ concertId, opts: { recordListen: false, playlistName: null, openVideoPanel: false } })]];
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
              ? playConcertItemPure(evo(model, { playback: (p) => ({ ...p, concert: Option.some({ ...concert, pos }) }) }), pos)
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
        // Find current liked state from whichever list is active.
        let currentLiked: boolean | null = null;
        const sidebarTracks = Option.getOrNull(model.sidebar.tracks);
        if (sidebarTracks && model.playback.concertId === concertId) {
          const t = sidebarTracks.tracks.find((t) => t.index === trackIdx);
          if (t) currentLiked = t.liked;
        }
        if (currentLiked === null) {
          const concert = Option.getOrNull(model.playback.concert);
          if (concert && concert.id === concertId) {
            const item = concert.items.find(
              (item) => item.track_index === trackIdx && item.kind !== "interlude",
            );
            if (item) currentLiked = item.liked;
          }
        }
        if (currentLiked === null) return [model, []]; // track not in any loaded list

        const next = !currentLiked;
        let model1 = flipSidebarTrackLiked(model, concertId, trackIdx, next);
        model1 = flipConcertItemLiked(model1, concertId, trackIdx, next);
        // Also sync bar star if this is the currently-playing track.
        if (model.playback.concertId === concertId && model.playback.trackIdx === trackIdx) {
          model1 = evo(model1, { playback: (p) => ({ ...p, liked: next }) });
        }
        return [
          model1,
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
