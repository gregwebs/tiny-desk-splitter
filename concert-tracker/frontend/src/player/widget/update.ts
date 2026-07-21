import { Array, Match as M, Option } from "effect";
import type { Command } from "foldkit/command";
import { evo } from "foldkit/struct";

import { enqueueDedupe, makeQueueEntry, PREPARE_TIMEOUT_MS, refindPosByUrl } from "../core";
import {
  ClearPreparingExternal,
  DisableCardTracksExternal,
  DrainQueue,
  HideVideoPanel,
  MarkPlayingExternal,
  MarkPreparingExternal,
  OpenInNewTab,
  PauseAudio,
  PersistSidebarWidth,
  PollPrepareStatus,
  PostPrepare,
  RefreshCardStatus,
  RefreshConcertItems,
  ResumeAudio,
  ScrollQueueToBottom,
  SetSidebarWidthVar,
  SyncLikeButtonsExternal,
} from "./command";
import type { Message } from "./message";
import { defaultPlayOpts, PlaySourceValue, PlayTargetValue, sameTarget, StatusValue, type Model } from "./model";
import {
  advanceAfterDelete,
  advanceOrCollapse,
  advanceToNextTrack,
  applyAdvanceFailure,
  applyLikedEverywhere,
  beginPlayback,
  dispatchPlayTrack,
  flipSidebarTrackAvailable,
  playConcertPosOrEnd,
  playerIdle,
  refetchSidebarIfConcertChanged,
  withBusy,
  withError,
  withPlayback,
  type UpdateReturn,
  withUpdateReturn,
} from "./update/helpers";
import { handleHostCommand } from "./update/handleHostCommand";

// UPDATE
//
// Ports nearly all decision logic from player.ts (everything message.ts's
// scope comment claims). The pure decision-logic helpers this function and
// handleHostCommand (update/handleHostCommand.ts) share — beginPlayback,
// dispatchPlayTrack, applyLikedEverywhere, the concert-reconstruction
// helpers, etc. — live in update/helpers.ts, so neither of those two needs
// to import from the other.

export const update = (model: Model, message: Message): UpdateReturn =>
  M.value(message).pipe(
    withUpdateReturn,
    M.tagsExhaustive({
      CommandReceived: ({ command }) => handleHostCommand(model)(command),

      SucceededMediaInfo: ({ source, info, opts }) =>
        withPlayback(...refetchSidebarIfConcertChanged(model, beginPlayback(model, source, info, opts))),

      NotPlayable: ({ url }) => [model, [OpenInNewTab({ url })]],

      TrackMissing: ({ source }) => {
        if (source._tag !== "Track") return [model, []]; // album/concert-item never reach prepare in practice
        return [model, [PostPrepare({ target: PlayTargetValue.Track({ concertId: source.concertId, trackIdx: source.trackIdx }) })]];
      },

      FailedFetchInfo: ({ errorMessage: msg }) => [withError(model, msg), []],

      SucceededTrackInfoForEnqueue: ({ concertId, trackIdx, info }) =>
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

      DrainedQueue: ({ played, skippedCount, plan }) => {
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

      // The benign "no later playable track" outcome (404) — reaching the end
      // of a set list/queue is normal, so no error status is shown.
      // applyAdvanceFailure already sets isPlaying false on every branch.
      NoNextTrack: ({ plan }) => applyAdvanceFailure(model, plan),

      FailedNextTrackInfo: ({ plan }) => {
        // "next-or-stop" calls stopPlaybackPure which clears status, so skip the error there.
        const model1 = plan === "next-or-stop"
          ? evo(model, { isPlaying: () => false })
          : withError(evo(model, { isPlaying: () => false }), "Couldn't load next track");
        return applyAdvanceFailure(model1, plan);
      },
      FailedPrevTrackInfo: () => [evo(model, { isPlaying: () => false }), []],

      SucceededPrepareStart: ({ target, seedStatus }) => {
        const model1 = evo(model, { pending: () => Option.some(target), status: () => StatusValue.Busy({ message: "Preparing…" }) });
        const commands: Command<Message>[] =
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

      SucceededPrepareStatus: ({ target, status, elapsedMs }) =>
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

      CompletedDeleteTrack: ({ concertId, trackIdx, ok, source }) => {
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

      SucceededConcertItems: ({ concertId, items, advanceAfter }) =>
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

      SucceededConcertPlaybackItems: ({ concertId, items, atPos }) =>
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

      SucceededPlaylistTracks: ({ tracks, name }) => {
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

      SucceededTrackDetails: ({ concertId, loadGen, tracksBusy, tracks }) => {
        if (model.sidebar.loadGen !== loadGen) return [model, []]; // stale — newer fetch started
        if (model.playback.concertId !== concertId) return [model, []]; // concert changed
        return [
          evo(model, { sidebar: () => evo(model.sidebar, { tracks: () => Option.some({ tracksBusy, tracks }) }) }),
          [],
        ];
      },
      FailedTrackDetails: () => [model, []], // sidebar stays at Option.none(); not user-visible

      FailedOpenExternal: () => [withError(model, "Couldn't open externally"), []],

      StartedAudio: () => [evo(model, { isPlaying: () => true }), []],
      PausedAudio: () => [evo(model, { isPlaying: () => false }), []],
      // loadGen is DOM-stamped (see model.ts's doc comment) — a mismatch
      // means this event is from a resource the element is no longer
      // actually playing. No-op rather than let it overwrite audioTime.
      UpdatedAudioTime: ({ currentTime, duration, loadGen }) =>
        loadGen === model.audioLoadGen
          ? [evo(model, { audioTime: () => ({ currentTime, duration }) }), []]
          : [model, []],
      EndedAudio: () =>
        advanceOrCollapse(evo(model, { playback: () => evo(model.playback, { ended: () => true }) })),
      ErroredAudio: () =>
        advanceOrCollapse(
          withError(
            evo(model, { playback: () => evo(model.playback, { ended: () => true }) }),
            "Failed to load media",
          ),
        ),
      RejectedAudioPlay: () => [withError(evo(model, { isPlaying: () => false }), "Playback blocked"), []],

      // ── Subscription-dispatched messages ──────────────────────────────
      // Re-stamp playing/preparing CSS markers after htmx:afterSettle /
      // historyRestore, mirroring player.ts's reassertPlayerUi().
      SettledHtmxContent: () => {
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
      SwappedLikeButton: ({ concertId, trackIdx, liked }) => [
        applyLikedEverywhere(model, concertId, trackIdx, liked),
        [],
      ],

      PressedSpace: ({ audioPaused }) => (audioPaused ? [model, [ResumeAudio()]] : [model, [PauseAudio()]]),

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
