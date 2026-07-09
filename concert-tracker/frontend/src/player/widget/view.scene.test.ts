import { Option } from "effect";
import { Scene } from "foldkit";
import type { Html } from "foldkit/html";
import { describe, expect, test } from "vitest";

import { makeQueueEntry } from "../core";
import {
  DrainQueue,
  FetchNextTrackInfo,
  FetchPrevTrackInfo,
  FetchTrackDetails,
  MutateBodyClass,
  OpenAddToPlaylist,
  PauseAudio,
  SeekAudio,
  SyncLikeButtonsExternal,
  ToggleLikeRequest,
} from "./command";
import {
  Acked,
  CompletedLikeToggle,
  FailedNextTrackInfo,
  FailedPrevTrackInfo,
  DrainedQueue,
  SucceededTrackDetails,
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
      // Both have a `display: none` CSS baseline (style.css) that an empty
      // inline style would leave in effect — pin the explicit shown/hidden
      // value the view must emit, not just the (baseline-blind) text content.
      Scene.expect(Scene.selector("#player-error")).toHaveStyle("display", "none"),
      Scene.expect(Scene.selector("#player-status")).toHaveStyle("display", "none"),
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
      Scene.expect(Scene.selector("#player-track")).toContainText("#1"),
      // Playing → ⏸
      Scene.expect(Scene.text("⏸")).toExist(),
    );
  });

  // Regression: #player-delete must gate on liked too (mirrors the old
  // player's `trackIdx == null || liked` guard) — a starred track's files
  // are protected from the player-bar delete button until unstarred.
  test("playing a liked track — delete hidden, like/add still visible", () => {
    Scene.scene(
      { update, view },
      Scene.with(trackModel({ liked: true })),
      Scene.expect(Scene.selector("#player-like")).toBeVisible(),
      Scene.expect(Scene.selector("#player-add-pl")).toBeVisible(),
      Scene.expect(Scene.selector("#player-delete")).not.toBeVisible(),
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

  // Regression test: concert-reconstruction playback of a video item always
  // has watchUrl: null (see watchUrlFor's ConcertItem case in update.ts), so
  // Watch must stay visible on isVideo alone — it doesn't need a URL, it only
  // folds out the inline video panel over the already-playing element.
  test("video item playing with watchUrl null (concert playback) — watch visible, open hidden", () => {
    Scene.scene(
      { update, view },
      Scene.with(trackModel({ isVideo: true, watchUrl: null })),
      Scene.expect(Scene.selector("#player-watch")).toBeVisible(),
      Scene.expect(Scene.selector("#player-open")).not.toBeVisible(),
    );
  });

  // Regression: the bar star rendered a hard-coded "btn-like" class, so the
  // ★/☆ text and aria-pressed flipped but the CSS `.liked` color never did
  // (issue #30). Assert the class alongside the text/state assertions below.
  test("liked track shows filled star ★; unliked shows ☆", () => {
    Scene.scene(
      { update, view },
      Scene.with(trackModel({ liked: true })),
      Scene.expect(Scene.selector("#player-like")).toContainText("★"),
      Scene.expect(Scene.selector("#player-like")).toHaveClass("liked"),
    );
    Scene.scene(
      { update, view },
      Scene.with(trackModel({ liked: false })),
      Scene.expect(Scene.selector("#player-like")).toContainText("☆"),
      Scene.expect(Scene.selector("#player-like")).not.toHaveClass("liked"),
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
      Scene.expect(Scene.selector("#player-queue-badge")).toBeVisible(),
      Scene.expect(Scene.selector("#player-queue-badge")).toHaveAttr("title", "Giant Steps\nNaima"),
    );
  });

  test("queue badge is empty when queue is empty", () => {
    Scene.scene(
      { update, view },
      Scene.with(trackModel()),
      Scene.expect(Scene.selector("#player-queue-badge")).toContainText(""),
      Scene.expect(Scene.selector("#player-queue-badge")).not.toBeVisible(),
      Scene.expect(Scene.selector("#player-queue-badge")).toHaveAttr("title", ""),
    );
  });

  test("queue badge shows 1 for a single-entry queue (post-dedup render)", () => {
    Scene.scene(
      { update, view },
      Scene.with({ ...trackModel(), queue: [makeQueueEntry(2, 3, "Giant Steps", false)] }),
      Scene.expect(Scene.selector("#player-queue-badge")).toContainText("1"),
      Scene.expect(Scene.selector("#player-queue-badge")).toBeVisible(),
      Scene.expect(Scene.selector("#player-queue-badge")).toHaveAttr("title", "Giant Steps"),
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

  // Regression: the Foldkit port hardcoded #player-seek to Disabled(true)
  // with no audio Subscription wired, so seek/time never worked — see
  // docs/change/2026-07-08-fix-failing-e2e-tests.md.
  test("seek is disabled and time reads 0:00 / 0:00 before any duration is known", () => {
    Scene.scene(
      { update, view },
      Scene.with(trackModel()),
      Scene.expect(Scene.selector("#player-seek")).toBeDisabled(),
      Scene.expect(Scene.selector("#player-time")).toContainText("0:00 / 0:00"),
    );
  });

  test("seek is enabled and valued once model.audioTime has a duration", () => {
    Scene.scene(
      { update, view },
      Scene.with(trackModel({}, { audioTime: { currentTime: 65, duration: 130 } })),
      Scene.expect(Scene.selector("#player-seek")).not.toBeDisabled(),
      Scene.expect(Scene.selector("#player-seek")).toHaveAttr("max", "130"),
      Scene.expect(Scene.selector("#player-seek")).toHaveAttr("value", "65"),
      Scene.expect(Scene.selector("#player-time")).toContainText("1:05 / 2:10"),
    );
  });

  test("typing into the seek slider dispatches Seek with the parsed seconds", () => {
    Scene.scene(
      { update, view },
      Scene.with(trackModel({}, { audioTime: { currentTime: 10, duration: 130 } })),
      Scene.type(Scene.selector("#player-seek"), "42"),
      // Command.resolve alone only matches by definition, not args (it would
      // pass even for a wrong seconds value) — expectHas the exact instance
      // first to actually prove the parsed value made it through.
      Scene.Command.expectHas(SeekAudio({ seconds: 42 })),
      Scene.Command.resolve(SeekAudio, Acked()),
    );
  });

  test("an unparseable seek input value is a no-op seek to the current position, not NaN", () => {
    Scene.scene(
      { update, view },
      Scene.with(trackModel({}, { audioTime: { currentTime: 10, duration: 130 } })),
      Scene.type(Scene.selector("#player-seek"), "not-a-number"),
      Scene.Command.expectHas(SeekAudio({ seconds: 10 })),
      Scene.Command.resolve(SeekAudio, Acked()),
    );
  });

  test("error status shows in #player-error", () => {
    Scene.scene(
      { update, view },
      Scene.with({ ...noPlayback, status: StatusValue.Error({ message: "Playback blocked" }) }),
      Scene.expect(Scene.selector("#player-error")).toContainText("Playback blocked"),
      Scene.expect(Scene.selector("#player-status")).toContainText(""),
      // Deliberate contract with the style.css `display: none` baseline —
      // see the note on the idle-state assertions above. Regression: the
      // Foldkit port set this text without a display style, so the CSS
      // baseline always won and the error never appeared in a real browser.
      Scene.expect(Scene.selector("#player-error")).toHaveStyle("display", "inline"),
      Scene.expect(Scene.selector("#player-status")).toHaveStyle("display", "none"),
    );
  });

  test("busy status shows in #player-status", () => {
    Scene.scene(
      { update, view },
      Scene.with({ ...noPlayback, status: StatusValue.Busy({ message: "Preparing…" }) }),
      Scene.expect(Scene.selector("#player-status")).toContainText("Preparing…"),
      Scene.expect(Scene.selector("#player-error")).toContainText(""),
      Scene.expect(Scene.selector("#player-status")).toHaveStyle("display", "inline"),
      Scene.expect(Scene.selector("#player-error")).toHaveStyle("display", "none"),
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

  // Regression guard: a plain click on #player-artist must be intercepted by
  // Player.openConcert (host shim) for an htmx partial swap, not fall through
  // to a full-page navigation that would kill playback (see e2e
  // back-navigation.spec.js "player-bar artist link").
  test("artist link wires onclick to the host shim's openConcert", () => {
    Scene.scene(
      { update, view },
      Scene.with(trackModel()),
      Scene.expect(Scene.selector("#player-artist")).toHaveAttr("onclick", "Player.openConcert(event)"),
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
      Scene.Command.resolve(DrainQueue, DrainedQueue({ played: Option.none(), skippedCount: 0, plan: "next-or-none" })),
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
        SucceededTrackDetails({ concertId: 1, loadGen: 1, tracksBusy: false, tracks: [] }),
      ),
      Scene.expect(Scene.selector("#player-queue-toggle")).toHaveAttr("aria-expanded", "true"),
    );
  });

  // Regression: #player-title/#player-track are Role("button") spans, not
  // native <button>s, so Enter activation has to be wired explicitly via
  // OnKeyDownPreventDefault — a keyboard user otherwise can't open the
  // sidebar from them at all. Deliberately Enter-only (not the usual ARIA
  // Enter+Space convention): both spans sit inside #player-bar, where Space
  // is claimed by the global playback shortcut (pauses, doesn't toggle the
  // sidebar) — see subscription.ts's keyboard entry and issue #28.
  test("pressing Enter on the title span opens the sidebar (keyboard activation)", () => {
    Scene.scene(
      { update, view },
      Scene.with({ ...trackModel(), sidebar: { open: false, tracks: Option.none(), loadGen: 0 } }),
      Scene.keydown(Scene.selector("#player-title"), "Enter"),
      Scene.Command.resolve(MutateBodyClass, Acked()),
      Scene.Command.resolve(
        FetchTrackDetails,
        SucceededTrackDetails({ concertId: 1, loadGen: 1, tracksBusy: false, tracks: [] }),
      ),
      Scene.expect(Scene.selector("#player-queue-toggle")).toHaveAttr("aria-expanded", "true"),
    );
  });

  test("pressing Enter on the track-number span opens the sidebar (keyboard activation)", () => {
    Scene.scene(
      { update, view },
      Scene.with({ ...trackModel(), sidebar: { open: false, tracks: Option.none(), loadGen: 0 } }),
      Scene.keydown(Scene.selector("#player-track"), "Enter"),
      Scene.Command.resolve(MutateBodyClass, Acked()),
      Scene.Command.resolve(
        FetchTrackDetails,
        SucceededTrackDetails({ concertId: 1, loadGen: 1, tracksBusy: false, tracks: [] }),
      ),
      Scene.expect(Scene.selector("#player-queue-toggle")).toHaveAttr("aria-expanded", "true"),
    );
  });

  test("pressing Space on the title span does not open the sidebar (reserved for the global playback shortcut)", () => {
    Scene.scene(
      { update, view },
      Scene.with({ ...trackModel(), sidebar: { open: false, tracks: Option.none(), loadGen: 0 } }),
      Scene.keydown(Scene.selector("#player-title"), " "),
      Scene.expect(Scene.selector("#player-queue-toggle")).toHaveAttr("aria-expanded", "false"),
    );
  });

  test("pressing Space on the track-number span does not open the sidebar (reserved for the global playback shortcut)", () => {
    Scene.scene(
      { update, view },
      Scene.with({ ...trackModel(), sidebar: { open: false, tracks: Option.none(), loadGen: 0 } }),
      Scene.keydown(Scene.selector("#player-track"), " "),
      Scene.expect(Scene.selector("#player-queue-toggle")).toHaveAttr("aria-expanded", "false"),
    );
  });

  test("play/pause button's accessible name reflects isPlaying", () => {
    Scene.scene(
      { update, view },
      Scene.with(trackModel({}, { isPlaying: false })),
      Scene.expect(Scene.selector("#player-play-pause")).toHaveAccessibleName("Play"),
    );
    Scene.scene(
      { update, view },
      Scene.with(trackModel({}, { isPlaying: true })),
      Scene.expect(Scene.selector("#player-play-pause")).toHaveAccessibleName("Pause"),
    );
  });

  test("error text is announced via role=alert", () => {
    Scene.scene(
      { update, view },
      Scene.with({ ...noPlayback, status: StatusValue.Error({ message: "Playback blocked" }) }),
      Scene.expect(Scene.selector("#player-error")).toHaveAttr("role", "alert"),
      Scene.expect(Scene.selector("#player-error")).toContainText("Playback blocked"),
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
        SucceededTrackDetails({ concertId, loadGen: 1, tracksBusy: false, tracks: [] }),
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

// ── Keyed list identity ──────────────────────────────────────────────────
//
// Foldkit's `keyed()` wraps each row so the vdom patches by identity, not
// position (see architecture.md's "Keyed Views" and checklist blind spot
// #16). These tests inspect the rendered VNode tree directly — Scene's
// DOM-level API doesn't expose snabbdom's `key`, which is never reflected
// as a DOM attribute — to verify the identity contract keyed() depends on:
// keys must be unique within a render, and stable for the same logical row
// across a position shift.

type VNode = NonNullable<Html>;

function findById(node: Html, id: string): VNode | null {
  if (node === null) return null;
  const props: Record<string, unknown> | undefined = node.data?.props;
  if (props?.id === id) return node;
  for (const child of node.children ?? []) {
    if (typeof child === "string") continue;
    const found = findById(child, id);
    if (found) return found;
  }
  return null;
}

function rowKeys(list: VNode): ReadonlyArray<PropertyKey> {
  return (list.children ?? []).flatMap((child) => (typeof child === "string" ? [] : (child.key ?? [])));
}

function findOl(node: Html): VNode | null {
  if (node === null) return null;
  if (node.sel === "ol") return node;
  for (const child of node.children ?? []) {
    if (typeof child === "string") continue;
    const found = findOl(child);
    if (found) return found;
  }
  return null;
}

describe("player sidebar — keyed row identity", () => {
  test("a queue row queued solo and again via a playlist group gets distinct keys", () => {
    const model: Model = {
      ...initialModel,
      queue: [
        makeQueueEntry(1, 0, "Solo Queue", false),
        makeQueueEntry(1, 0, "Playlist Queue", false, "Jazz Classics", 7),
      ],
    };
    const list = findById(view(model), "sidebar-queue-list");
    if (!list) throw new Error("expected #sidebar-queue-list to render");
    const keys = rowKeys(list);
    expect(new Set(keys).size).toBe(keys.length);
  });

  test("a concert-reconstruction track row's key is stable across a position shift", () => {
    const concertId = 1;
    const trackAtPos2: Model = {
      ...initialModel,
      playback: {
        ...initialPlayback,
        concertId,
        concert: Option.some({
          id: concertId,
          pos: 0,
          items: [
            interludeItem(0, "Intro", "/i/0.mp3"),
            interludeItem(1, "Intermission", "/i/1.mp3"),
            trackItem(5, "Track", "/a/5.mp3"),
          ],
        }),
      },
    };
    const trackAtPos0: Model = {
      ...initialModel,
      playback: {
        ...initialPlayback,
        concertId,
        concert: Option.some({
          id: concertId,
          pos: 0,
          items: [trackItem(5, "Track", "/a/5.mp3")],
        }),
      },
    };
    const olAtPos2 = findOl(findById(view(trackAtPos2), "sidebar-concert-section"));
    const olAtPos0 = findOl(findById(view(trackAtPos0), "sidebar-concert-section"));
    if (!olAtPos2 || !olAtPos0) throw new Error("expected the concert section's track list to render");
    const [, , keyAtPos2] = rowKeys(olAtPos2);
    const [keyAtPos0] = rowKeys(olAtPos0);
    expect(keyAtPos2).toBe(keyAtPos0);
  });

  test("a whole-album track's key differs across its available and unavailable states", () => {
    const concertId = 1;
    const trackList = (tracks: { index: number; title: string; available: boolean; is_video: boolean; liked: boolean }[]) => ({
      ...initialModel,
      playback: { ...initialPlayback, concertId, trackIdx: 0 },
      sidebar: { open: true, tracks: Option.some({ tracksBusy: false, tracks }), loadGen: 1 },
    });
    // Same track (index 5), rendered once while its file is missing and once
    // after it's downloaded — the row's button count changes (1 button vs.
    // 5), so patching one into the other by a shared key would misapply
    // event handlers to the wrong structure.
    const unavailable: Model = trackList([
      { index: 5, title: "Sixth", available: false, is_video: false, liked: false },
    ]);
    const available: Model = trackList([
      { index: 5, title: "Sixth", available: true, is_video: false, liked: false },
    ]);
    const olUnavailable = findOl(findById(view(unavailable), "sidebar-concert-section"));
    const olAvailable = findOl(findById(view(available), "sidebar-concert-section"));
    if (!olUnavailable || !olAvailable) throw new Error("expected the concert section's track list to render");
    const [keyUnavailable] = rowKeys(olUnavailable);
    const [keyAvailable] = rowKeys(olAvailable);
    expect(keyUnavailable).not.toBe(keyAvailable);
  });
});
