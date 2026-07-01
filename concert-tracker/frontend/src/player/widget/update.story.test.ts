import { Option } from "effect";
import { Story } from "foldkit";
import { describe, expect, test } from "vitest";

import { makeQueueEntry, PREPARE_TIMEOUT_MS } from "../core";
import {
  ClearAudioSrc,
  ClearPreparingExternal,
  DisableCardTracksExternal,
  DrainQueue,
  FetchAlbumInfo,
  FetchNextTrackInfo,
  FetchTrackDetails,
  FetchTrackInfo,
  HideVideoPanel,
  MarkPlayingExternal,
  MarkPlayingInterludeExternal,
  MarkPreparingExternal,
  MutateBodyClass,
  OpenAddToPlaylist,
  PauseAudio,
  PlayAudio,
  PollPrepareStatus,
  RecordListenEvent,
  RefreshCardStatus,
  RefreshConcertItems,
  ResumeAudio,
  ScrollQueueToBottom,
  SeekAudio,
  ShowVideoPanel,
  SyncLikeButtonsExternal,
  SyncNowPlayingMirrorCmd,
  ToggleLikeRequest,
} from "./command";
import {
  Acked,
  AudioEnded,
  AudioPaused,
  AudioPlaying,
  ClickedOutsideVideo,
  CommandReceived,
  CompletedLikeToggle,
  FailedFetchInfo,
  FailedLikeToggle,
  FailedNextTrackInfo,
  FailedPollPrepareStatus,
  FailedTrackDetails,
  PressedEscape,
  PressedSpace,
  ReassertUi,
  ReceivedConcertItems,
  ReceivedConcertPlaybackItems,
  ReceivedDeleteTrackResult,
  ReceivedMediaInfo,
  ReceivedPlaylistTracks,
  ReceivedPrepareStart,
  ReceivedPrepareStatus,
  ReceivedQueueDrainResult,
  ReceivedTrackDetails,
  ReceivedTrackInfoForEnqueue,
  SyncLikeFromSwap,
} from "./message";
import {
  defaultPlayOpts,
  initialModel,
  initialPlayback,
  type MediaInfo,
  type Model,
  type PlaybackItem,
  PlaySourceValue,
  StatusValue,
} from "./model";
import { PlayerCommandValue } from "./port";
import { update } from "./update";

// Foldkit Story tests for the player `update` (foldkit's own MVU harness):
// feed a model + a sequence of Messages, assert on the resulting Model and the
// Commands it emits. `Story.story` throws if any emitted Command is left
// unresolved, so each command is either resolved or asserted absent.
// Complements js-tests/player-core.test.ts (pure core.ts logic) and the
// Playwright e2e suite.

const mediaInfo: MediaInfo = {
  artist: "Artist",
  has_next: true,
  has_prev: false,
  is_video: false,
  liked: false,
  playable: true,
  title: "Track One",
  track_index: 0,
  url: "/audio/t1.mp3",
};

const interludeItem = (
  url: string,
  interludeIdx: number,
  title = `Interlude ${interludeIdx}`,
  isVideo = false,
): PlaybackItem => ({
  artist: "",
  interlude_index: interludeIdx,
  is_video: isVideo,
  kind: "interlude",
  liked: false,
  title,
  url,
});

const prepareStatus = (overrides: Partial<{ download: string; split: string; tracks_present: boolean[] }> = {}) => ({
  download: "done",
  split: "done",
  split_queued: false,
  tracks_present: [false],
  ...overrides,
});

const trackTarget = { _tag: "Track" as const, concertId: 1, trackIdx: 0 };

/** A model with active non-concert playback (concertId=1, trackIdx=0, isPlaying=true). */
const playingModel: Model = {
  ...initialModel,
  playback: {
    ...initialPlayback,
    concertId: 1,
    trackIdx: 0,
    title: "Track One",
    artist: "Artist",
  },
  isPlaying: true,
};

/** A model in concert mode with two interlude items.
 *  Interludes emit no RecordListenEvent, keeping command queues predictably short. */
const concertModel = (pos: number): Model => ({
  ...initialModel,
  playback: {
    ...initialPlayback,
    concertId: 42,
    trackIdx: null,
    title: `Interlude ${pos}`,
    artist: "",
    concert: Option.some({
      id: 42,
      items: [interludeItem("/c/0.mp3", 0), interludeItem("/c/1.mp3", 1)],
      pos,
    }),
  },
  isPlaying: true,
});

describe("player update — mirror invariant", () => {
  test("ReceivedMediaInfo always emits SyncNowPlayingMirrorCmd (mirror invariant)", () => {
    Story.story(
      update,
      Story.with(initialModel),
      Story.message(
        ReceivedMediaInfo({
          source: PlaySourceValue.Track({ concertId: 1, trackIdx: 0 }),
          info: mediaInfo,
          opts: defaultPlayOpts,
        }),
      ),
      Story.model((m) => {
        expect(m.playback.concertId).toBe(1);
        expect(m.playback.trackIdx).toBe(0);
        expect(m.playback.title).toBe("Track One");
      }),
      Story.Command.expectHas(SyncNowPlayingMirrorCmd),
      // beginPlayback + withPlayback emit: PlayAudio, MarkPlayingExternal,
      // ClearPreparingExternal, RecordListenEvent (Track+recordListen=true),
      // SyncNowPlayingMirrorCmd.
      Story.Command.resolve(PlayAudio, Acked()),
      Story.Command.resolve(MarkPlayingExternal, Acked()),
      Story.Command.resolve(ClearPreparingExternal, Acked()),
      Story.Command.resolve(RecordListenEvent, Acked()),
      Story.Command.resolve(SyncNowPlayingMirrorCmd, Acked()),
    );
  });

  test("StopPlayback emits SyncNowPlayingMirrorCmd with null ids (mirror cleared)", () => {
    Story.story(
      update,
      Story.with(playingModel),
      Story.message(CommandReceived({ command: PlayerCommandValue.StopPlayback() })),
      Story.model((m) => {
        expect(m.playback.concertId).toBeNull();
        expect(m.playback.trackIdx).toBeNull();
        expect(m.queue).toEqual([]);
        expect(m.isPlaying).toBe(false);
      }),
      Story.Command.expectHas(ClearAudioSrc, SyncNowPlayingMirrorCmd),
      Story.Command.resolve(ClearAudioSrc, Acked()),
      Story.Command.resolve(SyncNowPlayingMirrorCmd, Acked()),
    );
  });
});

describe("player update — queue operations", () => {
  test("Enqueue adds a track to the queue", () => {
    Story.story(
      update,
      Story.with(initialModel),
      Story.message(
        CommandReceived({ command: PlayerCommandValue.Enqueue({ concertId: 1, trackIdx: 2, title: "Q", liked: false }) }),
      ),
      Story.model((m) => {
        expect(m.queue.length).toBe(1);
        expect(m.queue[0]?.concertId).toBe(1);
        expect(m.queue[0]?.trackIdx).toBe(2);
      }),
      Story.Command.resolve(ScrollQueueToBottom, Acked()),
    );
  });

  test("Enqueue deduplicates a track already in the queue", () => {
    const withEntry: Model = {
      ...initialModel,
      queue: [makeQueueEntry(1, 2, "Q", false)],
    };
    Story.story(
      update,
      Story.with(withEntry),
      Story.message(
        CommandReceived({ command: PlayerCommandValue.Enqueue({ concertId: 1, trackIdx: 2, title: "Q", liked: false }) }),
      ),
      Story.model((m) => expect(m.queue.length).toBe(1)),
      Story.Command.expectNone(),
    );
  });

  test("Dequeue removes the entry at the given position", () => {
    const withTwo: Model = {
      ...initialModel,
      queue: [makeQueueEntry(1, 0, "A", false), makeQueueEntry(1, 1, "B", false)],
    };
    Story.story(
      update,
      Story.with(withTwo),
      Story.message(CommandReceived({ command: PlayerCommandValue.Dequeue({ pos: 0 }) })),
      Story.model((m) => {
        expect(m.queue.length).toBe(1);
        expect(m.queue[0]?.trackIdx).toBe(1);
      }),
      Story.Command.expectNone(),
    );
  });

  test("RemoveGroup removes every entry sharing the groupId, leaving others", () => {
    const withGroup: Model = {
      ...initialModel,
      queue: [
        makeQueueEntry(1, 0, "A", false, "PL", 7),
        makeQueueEntry(1, 1, "B", false, "PL", 7),
        makeQueueEntry(2, 0, "C", false),
      ],
    };
    Story.story(
      update,
      Story.with(withGroup),
      Story.message(CommandReceived({ command: PlayerCommandValue.RemoveGroup({ groupId: 7 }) })),
      Story.model((m) => {
        expect(m.queue.length).toBe(1);
        expect(m.queue[0]?.title).toBe("C");
      }),
      Story.Command.expectNone(),
    );
  });

  test("PlayQueueEntryNow dequeues the entry and fetches it for immediate play", () => {
    const withTwo: Model = {
      ...initialModel,
      queue: [makeQueueEntry(3, 5, "Now", false), makeQueueEntry(1, 1, "Later", false)],
    };
    Story.story(
      update,
      Story.with(withTwo),
      Story.message(CommandReceived({ command: PlayerCommandValue.PlayQueueEntryNow({ pos: 0 }) })),
      Story.model((m) => {
        expect(m.queue.length).toBe(1);
        expect(m.queue[0]?.title).toBe("Later");
      }),
      Story.Command.expectHas(FetchTrackInfo),
      Story.Command.resolve(
        FetchTrackInfo,
        FailedFetchInfo({ source: PlaySourceValue.Track({ concertId: 3, trackIdx: 5 }), message: "test-terminal" }),
      ),
    );
  });

  test("ReceivedQueueDrainResult with nothing played and queue-only plan trims the queue without advancing", () => {
    const withTwo: Model = {
      ...initialModel,
      queue: [makeQueueEntry(1, 0, "A", false), makeQueueEntry(1, 1, "B", false)],
    };
    Story.story(
      update,
      Story.with(withTwo),
      Story.message(ReceivedQueueDrainResult({ played: Option.none(), skippedCount: 2, plan: "queue-only" })),
      Story.model((m) => expect(m.queue).toEqual([])),
      Story.Command.expectNone(),
    );
  });
});

describe("player update — skip guards", () => {
  // Non-concert playback with no neighbours and an empty queue: SkipToNext/Prev
  // must be no-ops (no PauseAudio), matching the disabled Next/Back buttons. This
  // is the in-process counterpart to the e2e guard that the public skip API does
  // not pause the audio element when there is nothing to advance to.
  const noNeighbors: Model = {
    ...playingModel,
    playback: { ...playingModel.playback, hasNext: false, hasPrev: false },
  };

  test("SkipToNext is a no-op when nothing is next and the queue is empty", () => {
    Story.story(
      update,
      Story.with(noNeighbors),
      Story.message(CommandReceived({ command: PlayerCommandValue.SkipToNext() })),
      Story.model((m) => expect(m).toEqual(noNeighbors)),
      Story.Command.expectNone(),
    );
  });

  test("SkipToPrev is a no-op on the first track (nothing previous)", () => {
    Story.story(
      update,
      Story.with(noNeighbors),
      Story.message(CommandReceived({ command: PlayerCommandValue.SkipToPrev() })),
      Story.model((m) => expect(m).toEqual(noNeighbors)),
      Story.Command.expectNone(),
    );
  });
});

describe("player update — delete and advance", () => {
  test("ReceivedDeleteTrackResult bar-source for playing track pauses + drains queue", () => {
    // advanceAfterDelete: emits [PauseAudio, DrainQueue({plan:"next-or-stop"})]
    Story.story(
      update,
      Story.with(playingModel),
      Story.message(ReceivedDeleteTrackResult({ concertId: 1, trackIdx: 0, ok: true, source: "bar" })),
      Story.Command.expectHas(PauseAudio, DrainQueue),
      Story.Command.resolve(PauseAudio, Acked()),
      // Empty queue + "next-or-stop" → advanceToNextTrack → FetchNextTrackInfo for the deleted track.
      // The server skips the deleted index and returns the next available track.
      Story.Command.resolve(
        DrainQueue,
        ReceivedQueueDrainResult({ played: Option.none(), skippedCount: 0, plan: "next-or-stop" }),
      ),
      Story.Command.expectHas(FetchNextTrackInfo),
      // Resolve with failure to terminate the chain cleanly
      Story.Command.resolve(FetchNextTrackInfo, FailedNextTrackInfo({ plan: "next-or-stop" })),
      // next-or-stop after advance failure → stopPlaybackPure
      Story.Command.resolve(ClearAudioSrc, Acked()),
      Story.Command.resolve(SyncNowPlayingMirrorCmd, Acked()),
    );
  });

  test("PlayAlbumAt seeks in-place when the same album is playing (trackIdx===null guard)", () => {
    // B1 regression: the guard must check trackIdx===null, not concert._tag==="None".
    // Album play has trackIdx=null; same concert + null trackIdx → seek, no fetch.
    const albumPlaying: Model = {
      ...initialModel,
      playback: { ...initialPlayback, concertId: 5, trackIdx: null, title: "Album" },
      isPlaying: true,
    };
    Story.story(
      update,
      Story.with(albumPlaying),
      Story.message(CommandReceived({ command: PlayerCommandValue.PlayAlbumAt({ concertId: 5, seconds: 30 }) })),
      // isPlaying=true → only SeekAudio emitted (no ResumeAudio)
      Story.Command.expectHas(SeekAudio),
      Story.Command.resolve(SeekAudio, Acked()),
    );
  });

  test("PlayAlbumAt fetches album info when a track (not album) of the same concert is playing", () => {
    // B1 regression: track plays have trackIdx!==null → must fetch, not seek in-place.
    Story.story(
      update,
      Story.with(playingModel), // concertId=1, trackIdx=0
      Story.message(CommandReceived({ command: PlayerCommandValue.PlayAlbumAt({ concertId: 1, seconds: 30 }) })),
      Story.model((m) => expect(Option.isSome(m.pendingSeek)).toBe(true)),
      Story.Command.expectHas(FetchAlbumInfo),
      // Terminate FetchAlbumInfo cleanly with a failure
      Story.Command.resolve(
        FetchAlbumInfo,
        FailedFetchInfo({ source: PlaySourceValue.Album({ concertId: 1 }), message: "test-terminal" }),
      ),
      Story.model((m) => expect(m.status._tag).toBe("Error")),
    );
  });
});

describe("player update — prepare / poll", () => {
  test("ReceivedPrepareStatus for a superseded target is a no-op (staleness guard)", () => {
    const model: Model = {
      ...initialModel,
      pending: Option.some(trackTarget),
    };
    Story.story(
      update,
      Story.with(model),
      Story.message(
        ReceivedPrepareStatus({
          target: { _tag: "Track", concertId: 99, trackIdx: 0 }, // different concert → stale
          status: prepareStatus({ tracks_present: [true] }),
          elapsedMs: 100,
        }),
      ),
      Story.model((m) => expect(Option.isSome(m.pending)).toBe(true)),
      Story.Command.expectNone(),
    );
  });

  test("ReceivedPrepareStart enters Busy, starts polling, then clears pending on ready track", () => {
    Story.story(
      update,
      Story.with(initialModel),
      Story.message(ReceivedPrepareStart({ target: trackTarget, seedStatus: Option.none() })),
      Story.model((m) => {
        expect(Option.isSome(m.pending)).toBe(true);
        expect(m.status._tag).toBe("Busy");
      }),
      Story.Command.expectHas(PollPrepareStatus),
      Story.Command.resolve(MarkPreparingExternal, Acked()),
      Story.Command.resolve(DisableCardTracksExternal, Acked()),
      Story.Command.resolve(RefreshCardStatus, Acked()),
      // Track ready on first poll → pending cleared, play dispatched
      Story.Command.resolve(
        PollPrepareStatus,
        ReceivedPrepareStatus({ target: trackTarget, status: prepareStatus({ tracks_present: [true] }), elapsedMs: 100 }),
      ),
      Story.model((m) => {
        expect(Option.isNone(m.pending)).toBe(true);
        // status stays Busy until FetchTrackInfo completes (beginPlayback clears it)
        expect(m.status._tag).toBe("Busy");
      }),
      Story.Command.expectHas(FetchTrackInfo),
      Story.Command.resolve(ClearPreparingExternal, Acked()),
      // Terminate the FetchTrackInfo chain cleanly with a failure
      Story.Command.resolve(
        FetchTrackInfo,
        FailedFetchInfo({ source: PlaySourceValue.Track(trackTarget), message: "test-terminal" }),
      ),
      Story.model((m) => expect(m.status._tag).toBe("Error")),
    );
  });

  test("ReceivedPrepareStatus with download-error surfaces an error and clears pending", () => {
    const model: Model = {
      ...initialModel,
      pending: Option.some(trackTarget),
      status: StatusValue.Busy({ message: "Preparing…" }),
    };
    Story.story(
      update,
      Story.with(model),
      Story.message(
        ReceivedPrepareStatus({
          target: trackTarget,
          status: prepareStatus({ download: "download-error" }),
          elapsedMs: 100,
        }),
      ),
      Story.model((m) => {
        expect(Option.isNone(m.pending)).toBe(true);
        expect(m.status._tag).toBe("Error");
      }),
      Story.Command.expectHas(ClearPreparingExternal),
      Story.Command.resolve(ClearPreparingExternal, Acked()),
    );
  });

  test("ReceivedPrepareStatus past the timeout surfaces a timeout error", () => {
    const model: Model = {
      ...initialModel,
      pending: Option.some(trackTarget),
      status: StatusValue.Busy({ message: "Preparing…" }),
    };
    Story.story(
      update,
      Story.with(model),
      Story.message(
        ReceivedPrepareStatus({ target: trackTarget, status: prepareStatus(), elapsedMs: PREPARE_TIMEOUT_MS + 1 }),
      ),
      Story.model((m) => {
        expect(Option.isNone(m.pending)).toBe(true);
        if (m.status._tag !== "Error") throw new Error(`expected Error, got ${m.status._tag}`);
        expect(m.status.message).toContain("timed out");
      }),
      Story.Command.expectHas(ClearPreparingExternal),
      Story.Command.resolve(ClearPreparingExternal, Acked()),
    );
  });

  test("FailedPollPrepareStatus before timeout retries the poll", () => {
    const model: Model = {
      ...initialModel,
      pending: Option.some(trackTarget),
      status: StatusValue.Busy({ message: "Preparing…" }),
    };
    Story.story(
      update,
      Story.with(model),
      Story.message(FailedPollPrepareStatus({ target: trackTarget, elapsedMs: 100 })),
      Story.model((m) => {
        expect(Option.isSome(m.pending)).toBe(true);
        expect(m.status._tag).toBe("Busy");
      }),
      Story.Command.expectHas(PollPrepareStatus),
      // Terminate the retry with a download-error to avoid infinite recursion
      Story.Command.resolve(
        PollPrepareStatus,
        ReceivedPrepareStatus({ target: trackTarget, status: prepareStatus({ download: "download-error" }), elapsedMs: 200 }),
      ),
      Story.model((m) => expect(m.status._tag).toBe("Error")),
      Story.Command.resolve(ClearPreparingExternal, Acked()),
    );
  });
});

describe("player update — port-behavior fixes (#23)", () => {
  const sidebarTrack = (index: number, title: string, available = true) => ({
    index,
    title,
    available,
    is_video: false,
    liked: false,
  });
  // playingModel (concert 1, track 0) with the sidebar open and its track list loaded.
  const playingWithSidebar: Model = {
    ...playingModel,
    sidebar: {
      open: true,
      tracks: Option.some({
        tracksBusy: false,
        tracks: [sidebarTrack(0, "Celular"), sidebarTrack(1, "Limbo"), sidebarTrack(3, "Dando Vueltas")],
      }),
      loadGen: 3,
    },
  };
  const findSidebarTrack = (m: Model, index: number) =>
    Option.isSome(m.sidebar.tracks) ? m.sidebar.tracks.value.tracks.find((t) => t.index === index) : undefined;

  const resolvePlay = [
    Story.Command.resolve(PlayAudio, Acked()),
    Story.Command.resolve(MarkPlayingExternal, Acked()),
    Story.Command.resolve(ClearPreparingExternal, Acked()),
    Story.Command.resolve(RecordListenEvent, Acked()),
    Story.Command.resolve(SyncNowPlayingMirrorCmd, Acked()),
  ];

  test("ReceivedQueueDrainResult sets the playlist label from the played entry", () => {
    const entry = makeQueueEntry(1, 0, "Track One", false, "My Mix", 7);
    Story.story(
      update,
      Story.with({ ...initialModel, queue: [entry] }),
      Story.message(ReceivedQueueDrainResult({ played: Option.some({ entry, info: mediaInfo }), skippedCount: 0, plan: "queue-only" })),
      Story.model((m) => expect(m.playback.playlistLabel).toBe("My Mix")),
      ...resolvePlay,
    );
  });

  test("ReceivedQueueDrainResult of an ad-hoc entry leaves the playlist label null", () => {
    const entry = makeQueueEntry(2, 1, "Ad-hoc", false);
    Story.story(
      update,
      Story.with({ ...initialModel, queue: [entry] }),
      Story.message(ReceivedQueueDrainResult({ played: Option.some({ entry, info: mediaInfo }), skippedCount: 0, plan: "queue-only" })),
      Story.model((m) => expect(m.playback.playlistLabel).toBeNull()),
      ...resolvePlay,
    );
  });

  test("ReceivedMediaInfo refetches sidebar tracks when the concert changes while open", () => {
    const model: Model = { ...playingModel, sidebar: { open: true, tracks: Option.none(), loadGen: 5 } };
    Story.story(
      update,
      Story.with(model),
      Story.message(ReceivedMediaInfo({ source: PlaySourceValue.Track({ concertId: 2, trackIdx: 0 }), info: mediaInfo, opts: defaultPlayOpts })),
      Story.model((m) => expect(m.sidebar.loadGen).toBe(6)),
      Story.Command.expectHas(FetchTrackDetails),
      Story.Command.resolve(FetchTrackDetails, FailedTrackDetails({ concertId: 2, loadGen: 6 })),
      ...resolvePlay,
    );
  });

  test("ReceivedMediaInfo does not refetch the sidebar on an intra-album advance (same concert)", () => {
    // The concertId-unchanged guard is load-bearing: next/prev advance flows through
    // ReceivedMediaInfo too. loadGen stays put and no FetchTrackDetails fires (Story
    // would throw on an unresolved FetchTrackDetails if it did).
    const model: Model = { ...playingModel, sidebar: { open: true, tracks: Option.none(), loadGen: 5 } };
    Story.story(
      update,
      Story.with(model),
      Story.message(ReceivedMediaInfo({ source: PlaySourceValue.Track({ concertId: 1, trackIdx: 1 }), info: mediaInfo, opts: defaultPlayOpts })),
      Story.model((m) => expect(m.sidebar.loadGen).toBe(5)),
      ...resolvePlay,
    );
  });

  test("sidebar delete on the playing track advances and greys the row", () => {
    Story.story(
      update,
      Story.with(playingWithSidebar),
      Story.message(ReceivedDeleteTrackResult({ concertId: 1, trackIdx: 0, ok: true, source: "sidebar" })),
      Story.model((m) => expect(findSidebarTrack(m, 0)?.available).toBe(false)),
      Story.Command.expectHas(PauseAudio, DrainQueue),
      Story.Command.resolve(PauseAudio, Acked()),
      Story.Command.resolve(DrainQueue, ReceivedQueueDrainResult({ played: Option.none(), skippedCount: 0, plan: "queue-only" })),
    );
  });

  test("sidebar delete on a non-playing track greys the row without advancing", () => {
    Story.story(
      update,
      Story.with(playingWithSidebar),
      Story.message(ReceivedDeleteTrackResult({ concertId: 1, trackIdx: 3, ok: true, source: "sidebar" })),
      Story.model((m) => {
        expect(findSidebarTrack(m, 3)?.available).toBe(false);
        expect(m.playback.trackIdx).toBe(0);
      }),
      Story.Command.expectNone(),
    );
  });

  test("sidebar delete in concert mode still refreshes concert items (branch unchanged)", () => {
    Story.story(
      update,
      Story.with(concertModel(0)),
      Story.message(ReceivedDeleteTrackResult({ concertId: 42, trackIdx: 0, ok: true, source: "sidebar" })),
      Story.Command.expectHas(RefreshConcertItems),
      Story.Command.resolve(RefreshConcertItems, ReceivedConcertItems({ concertId: 42, items: [interludeItem("/c/0.mp3", 0)], advanceAfter: false })),
    );
  });
});

describe("player update — concert-reconstruction advance", () => {
  test("ReceivedConcertPlaybackItems enters concert mode and plays pos 0", () => {
    Story.story(
      update,
      Story.with(initialModel),
      Story.message(
        ReceivedConcertPlaybackItems({
          concertId: 42,
          items: [interludeItem("/c/0.mp3", 0), interludeItem("/c/1.mp3", 1)],
          atPos: 0,
        }),
      ),
      Story.model((m) => {
        expect(Option.isSome(m.playback.concert)).toBe(true);
        if (Option.isSome(m.playback.concert)) {
          expect(m.playback.concert.value.id).toBe(42);
          expect(m.playback.concert.value.pos).toBe(0);
        }
      }),
      Story.Command.expectHas(SyncNowPlayingMirrorCmd),
      // playConcertItemPure for an interlude: PlayAudio, MarkPlayingExternal,
      // ClearPreparingExternal, MarkPlayingInterludeExternal, SyncNowPlayingMirrorCmd.
      Story.Command.resolve(PlayAudio, Acked()),
      Story.Command.resolve(MarkPlayingExternal, Acked()),
      Story.Command.resolve(ClearPreparingExternal, Acked()),
      Story.Command.resolve(MarkPlayingInterludeExternal, Acked()),
      Story.Command.resolve(SyncNowPlayingMirrorCmd, Acked()),
    );
  });

  // Regression pin: a video item played via concert-reconstruction must land
  // in the state the Watch button's view gate expects (isVideo: true,
  // watchUrl: null) — see watchUrlFor's ConcertItem case in update.ts and the
  // matching Scene regression test in view.scene.test.ts.
  test("ReceivedConcertPlaybackItems with a video item sets isVideo true and watchUrl null", () => {
    Story.story(
      update,
      Story.with(initialModel),
      Story.message(
        ReceivedConcertPlaybackItems({
          concertId: 42,
          items: [interludeItem("/c/0.mp4", 0, "Interlude 0", true)],
          atPos: 0,
        }),
      ),
      Story.model((m) => {
        expect(m.playback.isVideo).toBe(true);
        expect(m.playback.watchUrl).toBeNull();
      }),
      Story.Command.expectHas(SyncNowPlayingMirrorCmd),
      Story.Command.resolve(PlayAudio, Acked()),
      Story.Command.resolve(MarkPlayingExternal, Acked()),
      Story.Command.resolve(ClearPreparingExternal, Acked()),
      Story.Command.resolve(MarkPlayingInterludeExternal, Acked()),
      Story.Command.resolve(SyncNowPlayingMirrorCmd, Acked()),
    );
  });

  test("AudioEnded in concert mode advances to the next item", () => {
    Story.story(
      update,
      Story.with(concertModel(0)),
      Story.message(AudioEnded()),
      Story.model((m) => {
        expect(Option.isSome(m.playback.concert)).toBe(true);
        if (Option.isSome(m.playback.concert)) {
          expect(m.playback.concert.value.pos).toBe(1);
        }
      }),
      Story.Command.expectHas(SyncNowPlayingMirrorCmd),
      Story.Command.resolve(PlayAudio, Acked()),
      Story.Command.resolve(MarkPlayingExternal, Acked()),
      Story.Command.resolve(ClearPreparingExternal, Acked()),
      Story.Command.resolve(MarkPlayingInterludeExternal, Acked()),
      Story.Command.resolve(SyncNowPlayingMirrorCmd, Acked()),
    );
  });

  test("AudioEnded at the last concert item clears concert mode without emitting commands", () => {
    // pos=1 is the last of 2 items; concertAdvancePos(1, 2) === null.
    // advanceConcertPure end-of-concert branch does NOT call withPlayback because
    // clearing `concert` alone doesn't change nowPlaying()'s concertId/trackIdx.
    Story.story(
      update,
      Story.with(concertModel(1)),
      Story.message(AudioEnded()),
      Story.model((m) => {
        expect(Option.isNone(m.playback.concert)).toBe(true);
        expect(m.video.open).toBe(false);
      }),
      Story.Command.expectNone(),
    );
  });

  test("ReceivedConcertItems with advanceAfter=false updates items without triggering play", () => {
    const updatedItem0 = interludeItem("/c/0.mp3", 0, "Updated Interlude 0");
    Story.story(
      update,
      Story.with(concertModel(0)),
      Story.message(
        ReceivedConcertItems({
          concertId: 42,
          items: [updatedItem0, interludeItem("/c/1.mp3", 1)],
          advanceAfter: false,
        }),
      ),
      Story.model((m) => {
        expect(Option.isSome(m.playback.concert)).toBe(true);
        if (Option.isSome(m.playback.concert)) {
          expect(m.playback.concert.value.items[0]?.title).toBe("Updated Interlude 0");
          expect(m.playback.concert.value.pos).toBe(0); // refound by URL
        }
      }),
      Story.Command.expectNone(),
    );
  });

  test("ReceivedConcertItems with advanceAfter=true plays the refreshed position", () => {
    Story.story(
      update,
      Story.with(concertModel(0)),
      Story.message(
        ReceivedConcertItems({
          concertId: 42,
          items: [interludeItem("/c/0.mp3", 0), interludeItem("/c/1.mp3", 1)],
          advanceAfter: true,
        }),
      ),
      Story.Command.expectHas(SyncNowPlayingMirrorCmd),
      Story.Command.resolve(PlayAudio, Acked()),
      Story.Command.resolve(MarkPlayingExternal, Acked()),
      Story.Command.resolve(ClearPreparingExternal, Acked()),
      Story.Command.resolve(MarkPlayingInterludeExternal, Acked()),
      Story.Command.resolve(SyncNowPlayingMirrorCmd, Acked()),
    );
  });
});

describe("player update — sidebar track details", () => {
  const concertModel: Model = {
    ...initialModel,
    playback: { ...initialPlayback, concertId: 1 },
    sidebar: { open: true, tracks: Option.none(), loadGen: 1 },
  };
  const sampleTracks = [
    { index: 0, title: "Track A", available: true, is_video: false, liked: false },
    { index: 1, title: "Track B", available: false, is_video: false, liked: true },
  ];

  test("ReceivedTrackDetails stores tracks when loadGen matches", () => {
    Story.story(
      update,
      Story.with(concertModel),
      Story.message(ReceivedTrackDetails({ concertId: 1, loadGen: 1, tracksBusy: false, tracks: sampleTracks })),
      Story.model((m) => {
        expect(Option.isSome(m.sidebar.tracks)).toBe(true);
        if (Option.isSome(m.sidebar.tracks)) {
          expect(m.sidebar.tracks.value.tracks).toHaveLength(2);
          expect(m.sidebar.tracks.value.tracksBusy).toBe(false);
        }
      }),
      Story.Command.expectNone(),
    );
  });

  test("ReceivedTrackDetails is discarded when loadGen is stale", () => {
    Story.story(
      update,
      Story.with(concertModel),
      Story.message(ReceivedTrackDetails({ concertId: 1, loadGen: 0, tracksBusy: false, tracks: sampleTracks })),
      Story.model((m) => expect(Option.isNone(m.sidebar.tracks)).toBe(true)),
      Story.Command.expectNone(),
    );
  });

  test("ReceivedTrackDetails is discarded when concert has changed", () => {
    Story.story(
      update,
      Story.with(concertModel),
      Story.message(ReceivedTrackDetails({ concertId: 99, loadGen: 1, tracksBusy: false, tracks: sampleTracks })),
      Story.model((m) => expect(Option.isNone(m.sidebar.tracks)).toBe(true)),
      Story.Command.expectNone(),
    );
  });

  test("FailedTrackDetails is a no-op", () => {
    Story.story(
      update,
      Story.with(concertModel),
      Story.message(FailedTrackDetails({ concertId: 1, loadGen: 1 })),
      Story.model((m) => expect(Option.isNone(m.sidebar.tracks)).toBe(true)),
      Story.Command.expectNone(),
    );
  });
});

describe("player update — OpenSidebar/ToggleSidebar FetchTrackDetails dispatch", () => {
  test("OpenSidebar in whole-album mode dispatches FetchTrackDetails and bumps loadGen", () => {
    const m: Model = {
      ...initialModel,
      playback: { ...initialPlayback, concertId: 5 },
      sidebar: { open: false, tracks: Option.none(), loadGen: 0 },
    };
    Story.story(
      update,
      Story.with(m),
      Story.message(CommandReceived({ command: PlayerCommandValue.OpenSidebar() })),
      Story.model((m2) => {
        expect(m2.sidebar.open).toBe(true);
        expect(m2.sidebar.loadGen).toBe(1);
      }),
      Story.Command.expectHas(FetchTrackDetails),
      Story.Command.resolve(MutateBodyClass, Acked()),
      Story.Command.resolve(
        FetchTrackDetails,
        ReceivedTrackDetails({ concertId: 5, loadGen: 1, tracksBusy: false, tracks: [] }),
      ),
    );
  });

  test("ToggleSidebar opening in whole-album mode dispatches FetchTrackDetails", () => {
    const m: Model = {
      ...initialModel,
      playback: { ...initialPlayback, concertId: 5 },
      sidebar: { open: false, tracks: Option.none(), loadGen: 0 },
    };
    Story.story(
      update,
      Story.with(m),
      Story.message(CommandReceived({ command: PlayerCommandValue.ToggleSidebar() })),
      Story.model((m2) => {
        expect(m2.sidebar.open).toBe(true);
        expect(m2.sidebar.loadGen).toBe(1);
      }),
      Story.Command.expectHas(FetchTrackDetails),
      Story.Command.resolve(MutateBodyClass, Acked()),
      Story.Command.resolve(
        FetchTrackDetails,
        ReceivedTrackDetails({ concertId: 5, loadGen: 1, tracksBusy: false, tracks: [] }),
      ),
    );
  });

  test("ToggleSidebar closing dispatches MutateBodyClass(sidebar-open, false)", () => {
    const m: Model = {
      ...initialModel,
      playback: { ...initialPlayback, concertId: 5 },
      sidebar: { open: true, tracks: Option.none(), loadGen: 1 },
    };
    Story.story(
      update,
      Story.with(m),
      Story.message(CommandReceived({ command: PlayerCommandValue.ToggleSidebar() })),
      Story.model((m2) => expect(m2.sidebar.open).toBe(false)),
      Story.Command.resolve(MutateBodyClass, Acked()),
    );
  });

  test("OpenSidebar in reconstruction mode skips FetchTrackDetails but dispatches body class", () => {
    const concertState = { id: 5, items: [interludeItem("/c/0.mp3", 0)], pos: 0 };
    const m: Model = {
      ...initialModel,
      playback: { ...initialPlayback, concertId: 5, concert: Option.some(concertState) },
    };
    Story.story(
      update,
      Story.with(m),
      Story.message(CommandReceived({ command: PlayerCommandValue.OpenSidebar() })),
      Story.model((m2) => {
        expect(m2.sidebar.open).toBe(true);
        expect(m2.sidebar.loadGen).toBe(0); // unchanged — reconstruction uses items, not a fetch
      }),
      Story.Command.resolve(MutateBodyClass, Acked()),
    );
  });

  test("OpenSidebar with no active concert dispatches body class", () => {
    Story.story(
      update,
      Story.with(initialModel),
      Story.message(CommandReceived({ command: PlayerCommandValue.OpenSidebar() })),
      Story.model((m) => expect(m.sidebar.open).toBe(true)),
      Story.Command.resolve(MutateBodyClass, Acked()),
    );
  });
});

describe("player update — like-sync", () => {
  const sampleTracks = [
    { index: 0, title: "Track A", available: true, is_video: false, liked: false },
    { index: 1, title: "Track B", available: false, is_video: false, liked: true },
  ];

  /** Model with concertId=1 trackIdx=0 playing, sidebar loaded. */
  const withSidebarTracks: Model = {
    ...initialModel,
    playback: { ...initialPlayback, concertId: 1, trackIdx: 0, liked: false },
    isPlaying: true,
    sidebar: {
      open: true,
      tracks: Option.some({ tracksBusy: false, tracks: sampleTracks }),
      loadGen: 1,
    },
  };

  test("SidebarLikeTrack flips sidebar track liked and dispatches ToggleLikeRequest", () => {
    Story.story(
      update,
      Story.with(withSidebarTracks),
      Story.message(
        CommandReceived({ command: PlayerCommandValue.SidebarLikeTrack({ concertId: 1, trackIdx: 0 }) }),
      ),
      Story.model((m) => {
        expect(Option.isSome(m.sidebar.tracks)).toBe(true);
        if (Option.isSome(m.sidebar.tracks)) {
          expect(m.sidebar.tracks.value.tracks[0]?.liked).toBe(true); // flipped false→true
        }
      }),
      Story.Command.expectHas(ToggleLikeRequest),
      Story.Command.resolve(
        ToggleLikeRequest,
        CompletedLikeToggle({ concertId: 1, trackIdx: 0, liked: true }),
      ),
      Story.Command.resolve(SyncLikeButtonsExternal, Acked()),
    );
  });

  test("SidebarLikeTrack for currently-playing track also flips bar star", () => {
    Story.story(
      update,
      Story.with(withSidebarTracks), // concertId=1, trackIdx=0 is playing
      Story.message(
        CommandReceived({ command: PlayerCommandValue.SidebarLikeTrack({ concertId: 1, trackIdx: 0 }) }),
      ),
      Story.model((m) => {
        expect(m.playback.liked).toBe(true); // bar star flipped
      }),
      Story.Command.resolve(
        ToggleLikeRequest,
        CompletedLikeToggle({ concertId: 1, trackIdx: 0, liked: true }),
      ),
      Story.Command.resolve(SyncLikeButtonsExternal, Acked()),
    );
  });

  test("SidebarLikeTrack for non-playing track leaves bar star unchanged", () => {
    Story.story(
      update,
      Story.with(withSidebarTracks), // playing trackIdx=0
      Story.message(
        CommandReceived({ command: PlayerCommandValue.SidebarLikeTrack({ concertId: 1, trackIdx: 1 }) }),
      ),
      Story.model((m) => {
        expect(m.playback.liked).toBe(false); // bar star unchanged (different track)
        expect(Option.isSome(m.sidebar.tracks)).toBe(true);
        if (Option.isSome(m.sidebar.tracks)) {
          expect(m.sidebar.tracks.value.tracks[1]?.liked).toBe(false); // flipped true→false
        }
      }),
      Story.Command.resolve(
        ToggleLikeRequest,
        CompletedLikeToggle({ concertId: 1, trackIdx: 1, liked: false }),
      ),
      Story.Command.resolve(SyncLikeButtonsExternal, Acked()),
    );
  });

  test("SidebarLikeTrack on track not in any list is a no-op", () => {
    Story.story(
      update,
      Story.with(withSidebarTracks),
      Story.message(
        CommandReceived({
          command: PlayerCommandValue.SidebarLikeTrack({ concertId: 1, trackIdx: 99 }),
        }),
      ),
      Story.model((m) => expect(m.playback.liked).toBe(false)),
      Story.Command.expectNone(),
    );
  });

  test("ToggleLike (bar) also syncs sidebar.tracks liked when loaded", () => {
    Story.story(
      update,
      Story.with(withSidebarTracks),
      Story.message(CommandReceived({ command: PlayerCommandValue.ToggleLike() })),
      Story.model((m) => {
        expect(m.playback.liked).toBe(true); // bar star flipped
        expect(Option.isSome(m.sidebar.tracks)).toBe(true);
        if (Option.isSome(m.sidebar.tracks)) {
          expect(m.sidebar.tracks.value.tracks[0]?.liked).toBe(true); // sidebar synced
        }
      }),
      Story.Command.resolve(
        ToggleLikeRequest,
        CompletedLikeToggle({ concertId: 1, trackIdx: 0, liked: true }),
      ),
      Story.Command.resolve(SyncLikeButtonsExternal, Acked()),
    );
  });

  test("FailedLikeToggle reverts sidebar.tracks liked and shows error for current track", () => {
    // Optimistic state: both bar liked and sidebar track[0].liked flipped to true.
    const optimistic: Model = {
      ...withSidebarTracks,
      playback: { ...withSidebarTracks.playback, liked: true },
      sidebar: {
        ...withSidebarTracks.sidebar,
        tracks: Option.some({
          tracksBusy: false,
          tracks: [{ ...sampleTracks[0]!, liked: true }, sampleTracks[1]!],
        }),
      },
    };
    Story.story(
      update,
      Story.with(optimistic),
      Story.message(FailedLikeToggle({ concertId: 1, trackIdx: 0, attempted: true })),
      Story.model((m) => {
        expect(m.playback.liked).toBe(false); // reverted
        expect(m.status._tag).toBe("Error");
        expect(Option.isSome(m.sidebar.tracks)).toBe(true);
        if (Option.isSome(m.sidebar.tracks)) {
          expect(m.sidebar.tracks.value.tracks[0]?.liked).toBe(false); // reverted
        }
      }),
      Story.Command.resolve(SyncLikeButtonsExternal, Acked()),
    );
  });

  test("FailedLikeToggle for a sidebar-only track reverts without showing error", () => {
    // Optimistic state: sidebar track[1].liked flipped to false (was true),
    // but playback is on trackIdx=0 — so no bar star error.
    const optimistic: Model = {
      ...withSidebarTracks,
      sidebar: {
        ...withSidebarTracks.sidebar,
        tracks: Option.some({
          tracksBusy: false,
          tracks: [sampleTracks[0]!, { ...sampleTracks[1]!, liked: false }],
        }),
      },
    };
    Story.story(
      update,
      Story.with(optimistic),
      Story.message(FailedLikeToggle({ concertId: 1, trackIdx: 1, attempted: false })),
      Story.model((m) => {
        expect(m.status._tag).toBe("Idle"); // no error for non-current track
        expect(Option.isSome(m.sidebar.tracks)).toBe(true);
        if (Option.isSome(m.sidebar.tracks)) {
          expect(m.sidebar.tracks.value.tracks[1]?.liked).toBe(true); // reverted back to true
        }
      }),
      Story.Command.resolve(SyncLikeButtonsExternal, Acked()),
    );
  });

  test("SidebarAddToPlaylist dispatches OpenAddToPlaylist with correct fields", () => {
    Story.story(
      update,
      Story.with(initialModel),
      Story.message(
        CommandReceived({
          command: PlayerCommandValue.SidebarAddToPlaylist({
            concertId: 3,
            trackIdx: 7,
            label: "Some Song",
          }),
        }),
      ),
      Story.Command.expectHas(OpenAddToPlaylist),
      Story.Command.resolve(OpenAddToPlaylist, Acked()),
    );
  });
});

describe("player update — audio events", () => {
  test("AudioPlaying sets isPlaying true", () => {
    Story.story(
      update,
      Story.with(initialModel),
      Story.message(AudioPlaying()),
      Story.model((m) => expect(m.isPlaying).toBe(true)),
      Story.Command.expectNone(),
    );
  });

  test("AudioPaused sets isPlaying false", () => {
    Story.story(
      update,
      Story.with(playingModel),
      Story.message(AudioPaused()),
      Story.model((m) => expect(m.isPlaying).toBe(false)),
      Story.Command.expectNone(),
    );
  });

  test("TogglePause when playing emits PauseAudio", () => {
    Story.story(
      update,
      Story.with(playingModel),
      Story.message(CommandReceived({ command: PlayerCommandValue.TogglePause() })),
      Story.Command.expectHas(PauseAudio),
      Story.Command.resolve(PauseAudio, Acked()),
    );
  });
});

describe("player update — queue section scroll", () => {
  test("ReceivedTrackInfoForEnqueue adds new entry dispatches ScrollQueueToBottom", () => {
    Story.story(
      update,
      Story.with(initialModel),
      Story.message(
        ReceivedTrackInfoForEnqueue({
          concertId: 1,
          trackIdx: 0,
          info: Option.some({ title: "Track 1", liked: false }),
        }),
      ),
      Story.model((m) => expect(m.queue).toHaveLength(1)),
      Story.Command.expectHas(ScrollQueueToBottom),
      Story.Command.resolve(ScrollQueueToBottom, Acked()),
    );
  });

  test("ReceivedTrackInfoForEnqueue duplicate skips ScrollQueueToBottom", () => {
    const model: Model = {
      ...initialModel,
      queue: [makeQueueEntry(1, 0, "Track 1", false)],
    };
    Story.story(
      update,
      Story.with(model),
      Story.message(
        ReceivedTrackInfoForEnqueue({
          concertId: 1,
          trackIdx: 0,
          info: Option.some({ title: "Track 1", liked: false }),
        }),
      ),
      Story.model((m) => expect(m.queue).toHaveLength(1)),
      Story.Command.expectNone(),
    );
  });

  test("ReceivedPlaylistTracks dispatches ScrollQueueToBottom", () => {
    Story.story(
      update,
      Story.with(initialModel),
      Story.message(
        ReceivedPlaylistTracks({
          playlistId: 1,
          name: "Jazz Classics",
          tracks: [{ concertId: 1, trackIdx: 0, title: "So What" }],
        }),
      ),
      Story.model((m) => expect(m.queue).toHaveLength(1)),
      Story.Command.expectHas(DrainQueue),
      Story.Command.resolve(
        DrainQueue,
        ReceivedQueueDrainResult({ played: Option.none(), skippedCount: 1, plan: "queue-only" }),
      ),
      Story.Command.expectHas(ScrollQueueToBottom),
      Story.Command.resolve(ScrollQueueToBottom, Acked()),
    );
  });

  test("Enqueue adds new entry dispatches ScrollQueueToBottom", () => {
    Story.story(
      update,
      Story.with(initialModel),
      Story.message(
        CommandReceived({
          command: PlayerCommandValue.Enqueue({ concertId: 2, trackIdx: 3, title: "Song", liked: false }),
        }),
      ),
      Story.model((m) => expect(m.queue).toHaveLength(1)),
      Story.Command.expectHas(ScrollQueueToBottom),
      Story.Command.resolve(ScrollQueueToBottom, Acked()),
    );
  });

  test("Enqueue duplicate skips ScrollQueueToBottom", () => {
    const model: Model = {
      ...initialModel,
      queue: [makeQueueEntry(2, 3, "Song", false)],
    };
    Story.story(
      update,
      Story.with(model),
      Story.message(
        CommandReceived({
          command: PlayerCommandValue.Enqueue({ concertId: 2, trackIdx: 3, title: "Song", liked: false }),
        }),
      ),
      Story.model((m) => expect(m.queue).toHaveLength(1)),
      Story.Command.expectNone(),
    );
  });
});

describe("player update — body class and video panel commands", () => {
  test("Watch opening dispatches ShowVideoPanel", () => {
    Story.story(
      update,
      Story.with({ ...initialModel, video: { open: false } }),
      Story.message(CommandReceived({ command: PlayerCommandValue.Watch() })),
      Story.model((m) => expect(m.video.open).toBe(true)),
      Story.Command.resolve(ShowVideoPanel, Acked()),
    );
  });

  test("Watch closing dispatches HideVideoPanel", () => {
    Story.story(
      update,
      Story.with({ ...initialModel, video: { open: true } }),
      Story.message(CommandReceived({ command: PlayerCommandValue.Watch() })),
      Story.model((m) => expect(m.video.open).toBe(false)),
      Story.Command.resolve(HideVideoPanel, Acked()),
    );
  });

  test("CloseSidebar dispatches MutateBodyClass(sidebar-open, false)", () => {
    Story.story(
      update,
      Story.with({ ...initialModel, sidebar: { open: true, tracks: Option.none(), loadGen: 0 } }),
      Story.message(CommandReceived({ command: PlayerCommandValue.CloseSidebar() })),
      Story.model((m) => expect(m.sidebar.open).toBe(false)),
      Story.Command.resolve(MutateBodyClass, Acked()),
    );
  });
});

describe("player update — subscription-dispatched messages", () => {
  test("ReassertUi with active playback dispatches MarkPlayingExternal", () => {
    Story.story(
      update,
      Story.with(playingModel),
      Story.message(ReassertUi()),
      Story.model((m) => expect(m).toEqual(playingModel)),
      Story.Command.resolve(MarkPlayingExternal, Acked()),
    );
  });

  test("ReassertUi with active playback + pending prepare dispatches both markers", () => {
    const m: Model = {
      ...playingModel,
      pending: Option.some({ _tag: "Track" as const, concertId: 1, trackIdx: 0 }),
    };
    Story.story(
      update,
      Story.with(m),
      Story.message(ReassertUi()),
      Story.Command.expectHas(MarkPlayingExternal, MarkPreparingExternal),
      Story.Command.resolve(MarkPlayingExternal, Acked()),
      Story.Command.resolve(MarkPreparingExternal, Acked()),
    );
  });

  test("ReassertUi with no active playback dispatches nothing", () => {
    Story.story(
      update,
      Story.with(initialModel),
      Story.message(ReassertUi()),
      Story.Command.expectNone(),
    );
  });

  test("PressedSpace while playing pauses audio", () => {
    Story.story(
      update,
      Story.with(playingModel),
      Story.message(PressedSpace()),
      Story.Command.resolve(PauseAudio, Acked()),
    );
  });

  test("PressedSpace while paused resumes audio", () => {
    Story.story(
      update,
      Story.with({ ...playingModel, isPlaying: false }),
      Story.message(PressedSpace()),
      Story.Command.resolve(ResumeAudio, Acked()),
    );
  });

  test("PressedEscape with video open closes video panel", () => {
    Story.story(
      update,
      Story.with({ ...initialModel, video: { open: true } }),
      Story.message(PressedEscape()),
      Story.model((m) => expect(m.video.open).toBe(false)),
      Story.Command.resolve(HideVideoPanel, Acked()),
    );
  });

  test("PressedEscape with video closed does nothing", () => {
    Story.story(
      update,
      Story.with(initialModel),
      Story.message(PressedEscape()),
      Story.model((m) => expect(m.video.open).toBe(false)),
      Story.Command.expectNone(),
    );
  });

  test("ClickedOutsideVideo with video open closes video panel", () => {
    Story.story(
      update,
      Story.with({ ...initialModel, video: { open: true } }),
      Story.message(ClickedOutsideVideo()),
      Story.model((m) => expect(m.video.open).toBe(false)),
      Story.Command.resolve(HideVideoPanel, Acked()),
    );
  });

  test("SyncLikeFromSwap updates playback liked state when track matches", () => {
    Story.story(
      update,
      Story.with(playingModel),
      Story.message(SyncLikeFromSwap({ concertId: 1, trackIdx: 0, liked: true })),
      Story.model((m) => expect(m.playback.liked).toBe(true)),
      Story.Command.expectNone(),
    );
  });

  test("SyncLikeFromSwap ignores swap for a different track", () => {
    Story.story(
      update,
      Story.with(playingModel),
      Story.message(SyncLikeFromSwap({ concertId: 1, trackIdx: 99, liked: true })),
      Story.model((m) => expect(m.playback.liked).toBe(false)), // unchanged
      Story.Command.expectNone(),
    );
  });
});
