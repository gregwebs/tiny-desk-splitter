import { Option } from "effect";
import { Scene } from "foldkit";
import { describe, test } from "vitest";

import { makeQueueEntry } from "../core";
import {
  DrainQueue,
  FetchNextTrackInfo,
  FetchPrevTrackInfo,
  FetchTrackDetails,
  MutateBodyClass,
  OpenAddToPlaylist,
  PauseAudio,
  SyncLikeButtonsExternal,
  ToggleLikeRequest,
} from "./command";
import {
  Acked,
  CompletedLikeToggle,
  FailedNextTrackInfo,
  FailedPrevTrackInfo,
  ReceivedQueueDrainResult,
  ReceivedTrackDetails,
} from "./message";
import type { Model } from "./model";
import { initialModel, initialPlayback, StatusValue } from "./model";
import { update } from "./update";
import { view } from "./view";

// Foldkit Scene tests: render the view for a given Model and assert/interact
// through the accessible DOM.  Complements the Story tests (model-level) and
// the Playwright e2e (real browser).

const noPlayback: Model = initialModel;

// ── Concert section helpers ───────────────────────────────────────────────

const trackItem = (idx: number, title: string, url: string, liked = false, isVideo = false) => ({
  artist: "Artist",
  is_video: isVideo,
  kind: "track",
  liked,
  title,
  track_index: idx,
  url,
});

const interludeItem = (idx: number, title: string, url: string) => ({
  artist: "",
  interlude_index: idx,
  is_video: false,
  kind: "interlude",
  liked: false,
  title,
  url,
});

const trackModel = (
  overrides: Partial<Model["playback"]> = {},
  extra: Partial<Omit<Model, "playback">> = {},
): Model => ({
  ...initialModel,
  playback: {
    ...initialPlayback,
    concertId: 1,
    trackIdx: 0,
    title: "Blue Train",
    artist: "John Coltrane",
    ...overrides,
  },
  isPlaying: true,
  ...extra,
});

const albumModel = (overrides: Partial<Model["playback"]> = {}): Model => ({
  ...initialModel,
  playback: {
    ...initialPlayback,
    concertId: 1,
    trackIdx: null, // album-mode: no individual track selected
    title: "Blue Train (Album)",
    artist: "John Coltrane",
    ...overrides,
  },
  isPlaying: true,
});

describe("player-bar view", () => {
  // #player-bar is display:none until it has the `active` class (CSS), so this
  // guards the regression where the bar never activated and was invisible.
  test("player bar gets the active class only when media is loaded", () => {
    Scene.scene(
      { update, view },
      Scene.with(trackModel()),
      Scene.expect(Scene.selector("#player-bar")).toHaveAttr("class", "active"),
    );
  });

  test("player bar has no active class with nothing playing", () => {
    Scene.scene(
      { update, view },
      Scene.with(noPlayback),
      Scene.expect(Scene.selector("#player-bar")).not.toHaveAttr("class", "active"),
    );
  });

  test("no playback — action buttons are hidden, play shows ▶", () => {
    Scene.scene(
      { update, view },
      Scene.with(noPlayback),
      // Transport
      Scene.expect(Scene.text("▶")).toExist(),
      // Like/add/delete hidden when nothing is playing
      Scene.expect(Scene.selector("#player-like")).not.toBeVisible(),
      Scene.expect(Scene.selector("#player-add-pl")).not.toBeVisible(),
      Scene.expect(Scene.selector("#player-delete")).not.toBeVisible(),
      // Watch/open hidden (no media)
      Scene.expect(Scene.selector("#player-watch")).not.toBeVisible(),
      Scene.expect(Scene.selector("#player-open")).not.toBeVisible(),
      // No error or status text
      Scene.expect(Scene.selector("#player-error")).toContainText(""),
      Scene.expect(Scene.selector("#player-status")).toContainText(""),
    );
  });

  test("playing a track — like/add/delete visible, title and artist shown", () => {
    Scene.scene(
      { update, view },
      Scene.with(trackModel()),
      // Action buttons visible for a track
      Scene.expect(Scene.selector("#player-like")).toBeVisible(),
      Scene.expect(Scene.selector("#player-add-pl")).toBeVisible(),
      Scene.expect(Scene.selector("#player-delete")).toBeVisible(),
      // Content
      Scene.expect(Scene.selector("#player-title")).toContainText("Blue Train"),
      Scene.expect(Scene.selector("#player-artist")).toContainText("John Coltrane"),
      // Track number (1-based)
      Scene.expect(Scene.selector("#player-track")).toContainText("1."),
      // Playing → ⏸
      Scene.expect(Scene.text("⏸")).toExist(),
    );
  });

  test("playing in album mode — like/add/delete hidden (no trackIdx)", () => {
    Scene.scene(
      { update, view },
      Scene.with(albumModel()),
      Scene.expect(Scene.selector("#player-like")).not.toBeVisible(),
      Scene.expect(Scene.selector("#player-add-pl")).not.toBeVisible(),
      Scene.expect(Scene.selector("#player-delete")).not.toBeVisible(),
      // Title and artist are still shown
      Scene.expect(Scene.selector("#player-title")).toContainText("Blue Train (Album)"),
    );
  });

  test("video track — watch and open visible; audio-only — both hidden", () => {
    Scene.scene(
      { update, view },
      Scene.with(trackModel({ isVideo: true, watchUrl: "https://example.com/watch/1" })),
      Scene.expect(Scene.selector("#player-watch")).toBeVisible(),
      Scene.expect(Scene.selector("#player-open")).toBeVisible(),
    );
    Scene.scene(
      { update, view },
      Scene.with(trackModel({ isVideo: false, watchUrl: null })),
      Scene.expect(Scene.selector("#player-watch")).not.toBeVisible(),
      Scene.expect(Scene.selector("#player-open")).not.toBeVisible(),
    );
  });

  test("watchUrl present but isVideo false — open visible, watch hidden", () => {
    Scene.scene(
      { update, view },
      Scene.with(trackModel({ isVideo: false, watchUrl: "https://example.com/watch/1" })),
      Scene.expect(Scene.selector("#player-watch")).not.toBeVisible(),
      Scene.expect(Scene.selector("#player-open")).toBeVisible(),
    );
  });

  test("liked track shows filled star ★; unliked shows ☆", () => {
    Scene.scene(
      { update, view },
      Scene.with(trackModel({ liked: true })),
      Scene.expect(Scene.selector("#player-like")).toContainText("★"),
    );
    Scene.scene(
      { update, view },
      Scene.with(trackModel({ liked: false })),
      Scene.expect(Scene.selector("#player-like")).toContainText("☆"),
    );
  });

  test("queue badge shows count when queue is non-empty", () => {
    Scene.scene(
      { update, view },
      Scene.with({
        ...trackModel(),
        queue: [
          { concertId: 2, trackIdx: 3, title: "Giant Steps", liked: false, playlistName: null, groupId: null },
          { concertId: 2, trackIdx: 4, title: "Naima", liked: false, playlistName: null, groupId: null },
        ],
      }),
      Scene.expect(Scene.selector("#player-queue-badge")).toContainText("2"),
    );
  });

  test("queue badge is empty when queue is empty", () => {
    Scene.scene(
      { update, view },
      Scene.with(trackModel()),
      Scene.expect(Scene.selector("#player-queue-badge")).toContainText(""),
    );
  });

  test("queue badge shows 1 for a single-entry queue (post-dedup render)", () => {
    Scene.scene(
      { update, view },
      Scene.with({ ...trackModel(), queue: [makeQueueEntry(2, 3, "Giant Steps", false)] }),
      Scene.expect(Scene.selector("#player-queue-badge")).toContainText("1"),
    );
  });

  test("next/prev disabled when hasNext and hasPrev are false and queue empty", () => {
    Scene.scene(
      { update, view },
      Scene.with(trackModel({ hasNext: false, hasPrev: false })),
      Scene.expect(Scene.selector("#player-next")).toBeDisabled(),
      Scene.expect(Scene.selector("#player-prev")).toBeDisabled(),
    );
  });

  test("next enabled when hasNext true; prev enabled when hasPrev true", () => {
    Scene.scene(
      { update, view },
      Scene.with(trackModel({ hasNext: true, hasPrev: true })),
      Scene.expect(Scene.selector("#player-next")).not.toBeDisabled(),
      Scene.expect(Scene.selector("#player-prev")).not.toBeDisabled(),
    );
  });

  test("error status shows in #player-error", () => {
    Scene.scene(
      { update, view },
      Scene.with({ ...noPlayback, status: StatusValue.Error({ message: "Playback blocked" }) }),
      Scene.expect(Scene.selector("#player-error")).toContainText("Playback blocked"),
      Scene.expect(Scene.selector("#player-status")).toContainText(""),
    );
  });

  test("busy status shows in #player-status", () => {
    Scene.scene(
      { update, view },
      Scene.with({ ...noPlayback, status: StatusValue.Busy({ message: "Preparing…" }) }),
      Scene.expect(Scene.selector("#player-status")).toContainText("Preparing…"),
      Scene.expect(Scene.selector("#player-error")).toContainText(""),
    );
  });

  test("playlist label shown when present, hidden when null", () => {
    Scene.scene(
      { update, view },
      Scene.with(trackModel({ playlistLabel: "Jazz Favorites" })),
      Scene.expect(Scene.selector("#player-playlist")).toBeVisible(),
      Scene.expect(Scene.selector("#player-playlist")).toContainText("Jazz Favorites"),
    );
    Scene.scene(
      { update, view },
      Scene.with(trackModel({ playlistLabel: null })),
      Scene.expect(Scene.selector("#player-playlist")).not.toBeVisible(),
    );
  });

  test("artist href points to /concerts/:id when media is playing", () => {
    Scene.scene(
      { update, view },
      Scene.with(trackModel()),
      Scene.expect(Scene.selector("#player-artist")).toHaveAttr("href", "/concerts/1"),
    );
  });

  test("clicking like optimistically flips the star ☆→★", () => {
    Scene.scene(
      { update, view },
      Scene.with(trackModel({ liked: false })),
      Scene.expect(Scene.selector("#player-like")).toContainText("☆"),
      Scene.click(Scene.selector("#player-like")),
      // Optimistic update in update.ts flips liked before the network round-trip
      Scene.expect(Scene.selector("#player-like")).toContainText("★"),
      Scene.Command.resolve(ToggleLikeRequest, CompletedLikeToggle({ concertId: 1, trackIdx: 0, liked: true })),
      Scene.Command.resolve(SyncLikeButtonsExternal, Acked()),
    );
  });

  test("clicking next dispatches SkipToNext (not SkipToPrev — copy-paste guard)", () => {
    Scene.scene(
      { update, view },
      Scene.with(trackModel({ hasNext: true, hasPrev: false })),
      Scene.click(Scene.selector("#player-next")),
      Scene.Command.resolve(PauseAudio, Acked()),
      Scene.Command.resolve(DrainQueue, ReceivedQueueDrainResult({ played: Option.none(), skippedCount: 0, plan: "next-or-none" })),
      Scene.Command.resolve(FetchNextTrackInfo, FailedNextTrackInfo({ plan: "next-or-none" })),
    );
  });

  test("clicking prev dispatches SkipToPrev (not SkipToNext — copy-paste guard)", () => {
    Scene.scene(
      { update, view },
      Scene.with(trackModel({ hasPrev: true, hasNext: false })),
      Scene.click(Scene.selector("#player-prev")),
      Scene.Command.resolve(PauseAudio, Acked()),
      Scene.Command.resolve(FetchPrevTrackInfo, FailedPrevTrackInfo()),
    );
  });

  test("clicking add-to-playlist dispatches AddToPlaylist", () => {
    Scene.scene(
      { update, view },
      Scene.with(trackModel()),
      Scene.click(Scene.selector("#player-add-pl")),
      Scene.Command.resolve(OpenAddToPlaylist, Acked()),
    );
  });

  test("sidebar toggle button aria-expanded reflects sidebar.open", () => {
    Scene.scene(
      { update, view },
      Scene.with({ ...trackModel(), sidebar: { open: false, tracks: Option.none(), loadGen: 0 } }),
      Scene.expect(Scene.selector("#player-queue-toggle")).toHaveAttr("aria-expanded", "false"),
      Scene.click(Scene.selector("#player-queue-toggle")),
      // ToggleSidebar opening dispatches MutateBodyClass + FetchTrackDetails.
      Scene.Command.resolve(MutateBodyClass, Acked()),
      Scene.Command.resolve(
        FetchTrackDetails,
        ReceivedTrackDetails({ concertId: 1, loadGen: 1, tracksBusy: false, tracks: [] }),
      ),
      Scene.expect(Scene.selector("#player-queue-toggle")).toHaveAttr("aria-expanded", "true"),
    );
  });
});

describe("player sidebar — concert section", () => {
  const concertId = 42;

  test("sidebar concert section is empty when no concert active", () => {
    Scene.scene(
      { update, view },
      Scene.with(noPlayback),
      Scene.expect(Scene.selector("#sidebar-concert-section")).toExist(),
      // No ol rendered when concertId is null
      Scene.expect(Scene.selector("#sidebar-concert-section ol")).not.toExist(),
    );
  });

  test("reconstruction mode renders concert items", () => {
    const model: Model = {
      ...initialModel,
      playback: {
        ...initialPlayback,
        concertId,
        trackIdx: 0,
        concert: Option.some({
          id: concertId,
          pos: 0,
          items: [trackItem(0, "Blue Train", "/audio/0.mp3"), trackItem(1, "Moment's Notice", "/audio/1.mp3")],
        }),
      },
    };
    Scene.scene(
      { update, view },
      Scene.with(model),
      Scene.expect(Scene.selector(".track-list-concert-playback")).toExist(),
      Scene.expect(Scene.selector(".track-list-concert-playback")).toContainText("Blue Train"),
      Scene.expect(Scene.selector(".track-list-concert-playback")).toContainText("Moment's Notice"),
    );
  });

  test("reconstruction mode marks the currently-playing item", () => {
    const model: Model = {
      ...initialModel,
      playback: {
        ...initialPlayback,
        concertId,
        concert: Option.some({
          id: concertId,
          pos: 1, // second item is playing
          items: [trackItem(0, "First", "/a/0.mp3"), trackItem(1, "Second", "/a/1.mp3")],
        }),
      },
    };
    Scene.scene(
      { update, view },
      Scene.with(model),
      // Only the playing item has concert-item-playing class
      Scene.expect(Scene.selector(".concert-item-playing")).toContainText("Second"),
    );
  });

  test("interlude item renders with delete button but no like or add-to-playlist", () => {
    const model: Model = {
      ...initialModel,
      playback: {
        ...initialPlayback,
        concertId,
        concert: Option.some({
          id: concertId,
          pos: 0,
          items: [interludeItem(0, "Intro", "/i/0.mp3")],
        }),
      },
    };
    Scene.scene(
      { update, view },
      Scene.with(model),
      Scene.expect(Scene.selector(".concert-item-interlude .btn-track-listen")).toContainText("Intro"),
      Scene.expect(Scene.selector(".concert-item-interlude .btn-delete")).toExist(),
      // No like or add-to-playlist on interludes
      Scene.expect(Scene.selector(".concert-item-interlude .btn-like")).not.toExist(),
      Scene.expect(Scene.selector(".concert-item-interlude .btn-add-pl")).not.toExist(),
    );
  });

  test("whole-album mode renders track list from sidebar.tracks", () => {
    const sidebarTracks = [
      { index: 0, title: "Track A", available: true, is_video: false, liked: false },
      { index: 1, title: "Track B", available: true, is_video: false, liked: true },
    ];
    const model: Model = {
      ...initialModel,
      playback: { ...initialPlayback, concertId, trackIdx: 0 },
      sidebar: { open: true, tracks: Option.some({ tracksBusy: false, tracks: sidebarTracks }), loadGen: 1 },
    };
    Scene.scene(
      { update, view },
      Scene.with(model),
      Scene.expect(Scene.selector("#sidebar-concert-section .track-list")).toExist(),
      Scene.expect(Scene.selector("#sidebar-concert-section .track-list")).toContainText("Track A"),
      Scene.expect(Scene.selector("#sidebar-concert-section .track-list")).toContainText("Track B"),
    );
  });

  test("unavailable track has track-unavailable class and no action buttons", () => {
    const model: Model = {
      ...initialModel,
      playback: { ...initialPlayback, concertId },
      sidebar: {
        open: true,
        tracks: Option.some({
          tracksBusy: false,
          tracks: [{ index: 0, title: "Missing Track", available: false, is_video: false, liked: false }],
        }),
        loadGen: 1,
      },
    };
    Scene.scene(
      { update, view },
      Scene.with(model),
      Scene.expect(Scene.selector(".track-unavailable")).toContainText("Missing Track"),
      Scene.expect(Scene.selector(".track-unavailable .btn-like")).not.toExist(),
      Scene.expect(Scene.selector(".track-unavailable .btn-add-pl")).not.toExist(),
    );
  });

  test("liked sidebar track shows ★, unliked shows ☆", () => {
    const model: Model = {
      ...initialModel,
      playback: { ...initialPlayback, concertId },
      sidebar: {
        open: true,
        tracks: Option.some({
          tracksBusy: false,
          tracks: [
            { index: 0, title: "Liked", available: true, is_video: false, liked: true },
            { index: 1, title: "Unliked", available: true, is_video: false, liked: false },
          ],
        }),
        loadGen: 1,
      },
    };
    Scene.scene(
      { update, view },
      Scene.with(model),
      // First item: liked=true → ★
      // Second item: liked=false → ☆
      // The track list should contain both star glyphs
      Scene.expect(Scene.selector("#sidebar-concert-section .track-list")).toContainText("★"),
      Scene.expect(Scene.selector("#sidebar-concert-section .track-list")).toContainText("☆"),
    );
  });

  test("clicking like in whole-album sidebar track dispatches SidebarLikeTrack", () => {
    const model: Model = {
      ...initialModel,
      playback: { ...initialPlayback, concertId },
      sidebar: {
        open: true,
        tracks: Option.some({
          tracksBusy: false,
          tracks: [{ index: 0, title: "My Track", available: true, is_video: false, liked: false }],
        }),
        loadGen: 1,
      },
    };
    Scene.scene(
      { update, view },
      Scene.with(model),
      Scene.click(Scene.selector("#sidebar-concert-section .btn-like")),
      // After like click: liked flipped to true, commands dispatched
      Scene.Command.resolve(
        ToggleLikeRequest,
        CompletedLikeToggle({ concertId, trackIdx: 0, liked: true }),
      ),
      Scene.Command.resolve(SyncLikeButtonsExternal, Acked()),
    );
  });

  test("whole-album sidebar shows Loading... when tracks not yet loaded", () => {
    const model: Model = {
      ...initialModel,
      playback: { ...initialPlayback, concertId },
      sidebar: { open: true, tracks: Option.none(), loadGen: 1 },
    };
    Scene.scene(
      { update, view },
      Scene.with(model),
      Scene.expect(Scene.selector(".sidebar-loading")).toContainText("Loading"),
    );
  });

  test("OpenSidebar in whole-album mode dispatches FetchTrackDetails", () => {
    const model: Model = {
      ...initialModel,
      playback: { ...initialPlayback, concertId },
      sidebar: { open: false, tracks: Option.none(), loadGen: 0 },
    };
    Scene.scene(
      { update, view },
      Scene.with(model),
      Scene.click(Scene.selector("#player-queue-toggle")),
      // ToggleSidebar opening dispatches MutateBodyClass then FetchTrackDetails.
      Scene.Command.resolve(MutateBodyClass, Acked()),
      Scene.Command.resolve(
        FetchTrackDetails,
        ReceivedTrackDetails({ concertId, loadGen: 1, tracksBusy: false, tracks: [] }),
      ),
    );
  });
});

describe("player sidebar — queue section", () => {
  test("empty queue shows Nothing queued", () => {
    Scene.scene(
      { update, view },
      Scene.with(initialModel),
      Scene.expect(Scene.selector("#sidebar-queue-empty")).toBeVisible(),
      Scene.expect(Scene.selector("#sidebar-queue-empty")).toContainText("Nothing queued"),
    );
  });

  test("queue with one song shows title", () => {
    const model: Model = {
      ...initialModel,
      queue: [makeQueueEntry(1, 0, "Blue Train", false)],
    };
    Scene.scene(
      { update, view },
      Scene.with(model),
      Scene.expect(Scene.selector("#sidebar-queue-list .btn-play-queue")).toContainText("Blue Train"),
    );
  });

  test("group header row renders playlist name", () => {
    const model: Model = {
      ...initialModel,
      queue: [makeQueueEntry(1, 0, "So What", false, "Jazz Classics", 1)],
    };
    Scene.scene(
      { update, view },
      Scene.with(model),
      Scene.expect(Scene.selector("#sidebar-queue-list .queue-group-header")).toContainText("Jazz Classics"),
    );
  });

  test("playlist song has queue-song-nested class", () => {
    const model: Model = {
      ...initialModel,
      queue: [makeQueueEntry(1, 0, "So What", false, "Jazz Classics", 1)],
    };
    Scene.scene(
      { update, view },
      Scene.with(model),
      Scene.expect(Scene.selector("#sidebar-queue-list .queue-song-nested")).toContainText("So What"),
    );
  });

  test("remove button dequeues entry", () => {
    const model: Model = {
      ...initialModel,
      queue: [makeQueueEntry(1, 0, "Blue Train", false)],
    };
    Scene.scene(
      { update, view },
      Scene.with(model),
      Scene.click(Scene.selector("#sidebar-queue-list .btn-remove-queue")),
      // Dequeue dispatches no commands; view re-renders with empty queue
      Scene.expect(Scene.selector("#sidebar-queue-empty")).toBeVisible(),
    );
  });

  test("remove button shows the × glyph, not a trash icon", () => {
    const model: Model = {
      ...initialModel,
      queue: [makeQueueEntry(1, 0, "Blue Train", false)],
    };
    Scene.scene(
      { update, view },
      Scene.with(model),
      Scene.expect(Scene.selector("#sidebar-queue-list .btn-remove-queue")).toContainText("×"),
      Scene.expect(Scene.selector("#sidebar-queue-list .icon-trash")).toBeAbsent(),
    );
  });

  test("group-header remove dequeues the whole group", () => {
    const model: Model = {
      ...initialModel,
      queue: [
        makeQueueEntry(1, 0, "So What", false, "Jazz Classics", 3),
        makeQueueEntry(1, 1, "Blue in Green", false, "Jazz Classics", 3),
      ],
    };
    Scene.scene(
      { update, view },
      Scene.with(model),
      Scene.click(Scene.selector("#sidebar-queue-list .btn-remove-group")),
      // RemoveGroup dispatches no commands; both songs leave, queue is empty.
      Scene.expect(Scene.selector("#sidebar-queue-empty")).toBeVisible(),
    );
  });
});
