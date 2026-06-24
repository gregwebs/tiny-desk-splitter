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
  FetchTrackInfo,
  MarkPlayingExternal,
  MarkPlayingInterludeExternal,
  MarkPreparingExternal,
  PauseAudio,
  PlayAudio,
  PollPrepareStatus,
  RecordListenEvent,
  RefreshCardStatus,
  SeekAudio,
  SyncNowPlayingMirrorCmd,
} from "./command";
import {
  Acked,
  AudioEnded,
  AudioPaused,
  AudioPlaying,
  CommandReceived,
  FailedFetchInfo,
  FailedNextTrackInfo,
  FailedPollPrepareStatus,
  ReceivedConcertItems,
  ReceivedConcertPlaybackItems,
  ReceivedDeleteTrackResult,
  ReceivedMediaInfo,
  ReceivedPrepareStart,
  ReceivedPrepareStatus,
  ReceivedQueueDrainResult,
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

const interludeItem = (url: string, interludeIdx: number, title = `Interlude ${interludeIdx}`): PlaybackItem => ({
  artist: "",
  interlude_index: interludeIdx,
  is_video: false,
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
      Story.Command.expectNone(),
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
