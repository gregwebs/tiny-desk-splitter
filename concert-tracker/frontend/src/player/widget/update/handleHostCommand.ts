import { Match as M, Option } from "effect";
import { evo } from "foldkit/struct";

import { dequeueAt, enqueueDedupe, makeQueueEntry, nextEnabled, prevEnabled, removeGroup } from "../../core";
import {
  DeleteTrackRequest,
  DrainQueue,
  FetchAlbumInfo,
  FetchConcertPlayback,
  FetchPlaylistForPlay,
  FetchPrevTrackInfo,
  FetchTrackDetails,
  FetchTrackInfo,
  HideVideoPanel,
  MutateBodyClass,
  OpenAddToPlaylist,
  OpenExternalRequest,
  PauseAudio,
  PostDeleteInterlude,
  ResolveFirstAvailableTrack,
  ResumeAudio,
  ScrollQueueToBottom,
  SeekAudio,
  ShowVideoPanel,
  SyncLikeButtonsExternal,
  ToggleLikeRequest,
} from "../command";
import { defaultPlayOpts, type Model } from "../model";
import type { PlayerCommand } from "../port";
import {
  advanceConcertPure,
  applyLikedEverywhere,
  concertErrorMessages,
  dispatchPlayTrack,
  findCurrentLiked,
  playConcertItemPure,
  stopPlaybackPure,
  toCoreState,
  type UpdateReturn,
  withUpdateReturn,
} from "./helpers";

// ── PlayerCommand dispatch (host calls in via the single inbound Port) ──
//
// Curried so it composes as a plain Match case in update.ts's own
// M.tagsExhaustive: `CommandReceived: ({ command }) => handleHostCommand(model)(command)`.
export const handleHostCommand =
  (model: Model) =>
  (command: PlayerCommand): UpdateReturn =>
    M.value(command).pipe(
      withUpdateReturn,
      M.tagsExhaustive({
        PlayAlbum: ({ concertId }) => [model, [FetchAlbumInfo({ concertId, opts: defaultPlayOpts })]],
        PlayTrack: ({ concertId, trackIdx }) => dispatchPlayTrack(model, concertId, trackIdx),
        PlayTracks: ({ concertId }) => [model, [ResolveFirstAvailableTrack({ concertId })]],
        StartAlbum: ({ concertId, recordListen }) => [
          model,
          [FetchAlbumInfo({ concertId, opts: { recordListen, playlistName: null, openVideoPanel: false } })],
        ],
        StartTrack: ({ concertId, trackIdx }) => [
          model,
          [FetchTrackInfo({ concertId, trackIdx, opts: defaultPlayOpts })],
        ],

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
            : [
                model,
                [DeleteTrackRequest({ concertId: model.playback.concertId, trackIdx: model.playback.trackIdx, source: "bar" })],
              ],

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
        SidebarDeleteTrack: ({ concertId, trackIdx }) => [
          model,
          [DeleteTrackRequest({ concertId, trackIdx, source: "sidebar" })],
        ],

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
            onNone: () => [
              model,
              [FetchConcertPlayback({ concertId, atPos: Option.some(pos), errorMessage: concertErrorMessages.load })],
            ],
          }),
        SidebarDeleteInterlude: ({ concertId, interludeIdx }) => {
          const wasPlayingThis = Option.match(model.playback.concert, {
            onNone: () => false,
            onSome: (concert) => {
              const currentItem = concert.items[concert.pos];
              return !!(currentItem && currentItem.kind === "interlude" && currentItem.interlude_index === interludeIdx);
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
