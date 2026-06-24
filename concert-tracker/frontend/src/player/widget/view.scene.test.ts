import { Option } from "effect";
import { Scene } from "foldkit";
import { describe, test } from "vitest";

import { DrainQueue, FetchNextTrackInfo, FetchPrevTrackInfo, OpenAddToPlaylist, PauseAudio, SyncLikeButtonsExternal, ToggleLikeRequest } from "./command";
import { Acked, CompletedLikeToggle, FailedNextTrackInfo, FailedPrevTrackInfo, ReceivedQueueDrainResult } from "./message";
import type { Model } from "./model";
import { initialModel, initialPlayback, StatusValue } from "./model";
import { update } from "./update";
import { view } from "./view";

// Foldkit Scene tests: render the view for a given Model and assert/interact
// through the accessible DOM.  Complements the Story tests (model-level) and
// the Playwright e2e (real browser).

const noPlayback: Model = initialModel;

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
      Scene.with({ ...trackModel(), sidebar: { open: false } }),
      Scene.expect(Scene.selector("#player-queue-toggle")).toHaveAttr("aria-expanded", "false"),
      Scene.click(Scene.selector("#player-queue-toggle")),
      Scene.expect(Scene.selector("#player-queue-toggle")).toHaveAttr("aria-expanded", "true"),
    );
  });
});
