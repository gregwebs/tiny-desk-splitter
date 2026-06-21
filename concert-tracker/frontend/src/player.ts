// The persistent player bar + sidebar (queue + concert-track list): owns the
// single <video> element used for both audio and video playback, the
// playback queue, auto-advance, prepare/download polling, concert
// reconstruction playback, like/delete, and keyboard shortcuts. Ported from
// the original static/player.js.
//
// The player bar lives outside #content (see layout.html), so hx-boost swaps
// never detach it and audio keeps playing across navigation. Functions below
// re-assert the JS-driven UI (playing-track highlight, like/delete buttons)
// after a swap via reassertPlayerUi — see init()'s htmx event listeners.
import {
  fetchSidebarTracks,
  getConcertPlayback,
  getMediaInfo,
  getNextTrackMediaInfo,
  getPrepareStatus,
  getPrevTrackMediaInfo,
  getTrackMediaInfoOrNull,
  getPlaylist,
  isSourcePlayback,
  postDeleteInterlude,
  postDeleteTrack,
  postEvent,
  postLikeTrack,
  postPrepare,
  type ConcertPlaybackResponse,
  type PlaybackItemJson,
  type PrepareStatus,
} from "./api/client";
import { byIdOrNull } from "./shared/dom";
import type { PlayerApi, PlayerNowPlaying } from "./shared/player-api";
// window.Playlists is declared ambiently by ./shared/playlists-api.ts.

// ── State ────────────────────────────────────────────────────────────────────

interface ConcertPlaybackState {
  id: number;
  items: PlaybackItemJson[];
  pos: number;
}

interface PlayerState {
  concertId: number | null;
  trackIdx: number | null;
  isVideo: boolean;
  watchUrl: string | null;
  hasNext: boolean;
  hasPrev: boolean;
  liked: boolean;
  concert: ConcertPlaybackState | null;
}

interface QueueEntry {
  concertId: number;
  trackIdx: number;
  title: string;
  liked: boolean;
  playlistName: string | null;
  groupId: number | null;
}

interface PendingPlay {
  concertId: number;
  trackIdx: number;
  timer: ReturnType<typeof setTimeout> | null;
  deadline: number;
}

// `audio`/`bar` are assigned exactly once, by init(), which runs synchronously
// at module load (player.js is the last <script> in <body>, after #player-audio
// and #player-bar — see layout.html) — before any user interaction or htmx
// event can call into the public API. Functions below that don't have their
// own `if (!audio) return;` guard rely on that invariant via `audio!`/`bar!`.
let audio: HTMLMediaElement | null = null;
let bar: HTMLElement | null = null;
let state: PlayerState = {
  concertId: null,
  trackIdx: null,
  isVideo: false,
  watchUrl: null,
  hasNext: false,
  hasPrev: false,
  liked: false,
  concert: null,
};
let queue: QueueEntry[] = [];
// Monotonic id minted once per playPlaylist call so a playlist's queued tracks
// form one visually-grouped, separately-removable block. Lifetime-only — never
// persisted or compared across reloads.
let nextGroupId = 1;
let autoAdvanceController: AbortController | null = null;
let keyboardShortcutsBound = false;
let sidebarLoadGen = 0;
let sidebarConcertId: number | null = null;

function onPlay(): void {
  setPlayPauseIcon(true);
}
function onPause(): void {
  setPlayPauseIcon(false);
}

function bindAudioEvents(el: HTMLMediaElement): void {
  el.addEventListener("timeupdate", onTimeUpdate);
  el.addEventListener("loadedmetadata", onTimeUpdate);
  el.addEventListener("ended", onEnded);
  el.addEventListener("error", onError);
  el.addEventListener("play", onPlay);
  el.addEventListener("pause", onPause);
}

// ── Sidebar resize + width persistence ──────────────────────────────────────

const SIDEBAR_WIDTH_KEY = "sidebarWidth";
const SIDEBAR_MIN_WIDTH = 240;
const SIDEBAR_MAX_WIDTH = 600;

// Pure + unit-testable: the 240/600 clamp with no DOM access.
function clampSidebarWidth(px: number): number {
  return Math.max(SIDEBAR_MIN_WIDTH, Math.min(SIDEBAR_MAX_WIDTH, Math.round(px)));
}

// Write the clamped width to the CSS variable that drives sidebar width,
// body margin, and the video panel offset — one var reflows all three.
function applySidebarWidth(px: number): number {
  const w = clampSidebarWidth(px);
  document.documentElement.style.setProperty("--sidebar-width", w + "px");
  return w;
}

// Restore the saved preferred width on load (ignores missing/corrupt values).
function loadSidebarWidth(): void {
  try {
    const v = parseInt(window.localStorage.getItem(SIDEBAR_WIDTH_KEY) || "", 10);
    if (Number.isFinite(v)) applySidebarWidth(v);
  } catch {
    /* storage may be unavailable */
  }
}

function initSidebarResize(): void {
  const handle = byIdOrNull("sidebar-resize");
  if (!handle) return;
  let dragging = false;
  let moved = false;
  let lastW = 0;
  handle.addEventListener("pointerdown", (e) => {
    dragging = true;
    moved = false;
    // Seed from the live computed width so a click-without-drag never persists 0.
    lastW =
      parseInt(
        getComputedStyle(document.documentElement).getPropertyValue("--sidebar-width"),
        10,
      ) || SIDEBAR_MIN_WIDTH;
    handle.setPointerCapture(e.pointerId);
    document.body.classList.add("sidebar-resizing");
    e.preventDefault();
  });
  handle.addEventListener("pointermove", (e) => {
    if (!dragging) return;
    moved = true;
    // The sidebar is position:fixed; left:0, so clientX directly equals the desired width.
    lastW = applySidebarWidth(e.clientX);
  });
  const end = () => {
    if (!dragging) return;
    dragging = false;
    document.body.classList.remove("sidebar-resizing");
    if (moved) {
      // only persist on a real drag, not a bare click on the handle
      try {
        window.localStorage.setItem(SIDEBAR_WIDTH_KEY, String(lastW));
      } catch {
        /* storage may be unavailable */
      }
    }
    tracing("sidebarResize", { width: lastW, moved });
  };
  handle.addEventListener("pointerup", end);
  handle.addEventListener("pointercancel", end);
}

// ── Initialisation ──────────────────────────────────────────────────────────

function init(): void {
  audio = byIdOrNull<HTMLMediaElement>("player-audio");
  bar = byIdOrNull("player-bar");
  if (!audio || !bar) return;

  loadSidebarWidth();
  initSidebarResize();
  bindAudioEvents(audio);
  bindKeyboardShortcuts();

  // Reveal the video minimize button on pointer activity over the panel (touchstart
  // too, since touch devices fire no mousemove).
  const videoPanel = byIdOrNull("player-video-panel");
  if (videoPanel) {
    videoPanel.addEventListener("mousemove", showVideoControls);
    videoPanel.addEventListener("touchstart", showVideoControls, { passive: true });
  }

  // Navigation swaps only #content; the player lives outside it and is never
  // detached, so the audio keeps playing on its own. These handlers only
  // re-assert the JS-driven UI (playing-track highlight, like/delete state)
  // after an in-place swap or a Back/Forward history restore re-creates the
  // listen buttons inside #content. (historyRestore does not fire afterSettle.)
  document.body.addEventListener("htmx:afterSettle", reassertPlayerUi);
  document.body.addEventListener("htmx:historyRestore", reassertPlayerUi);

  // Reverse like-sync: when a track list re-renders (track-list star toggled,
  // or a track deleted), propagate liked state across all copies of that
  // track's star buttons (card list, sidebar, queue) and the player star.
  document.body.addEventListener("htmx:afterSwap", syncLikeFromTrackList as EventListener);
}

function bindKeyboardShortcuts(): void {
  if (keyboardShortcutsBound) return;
  document.addEventListener("keydown", onGlobalKeydown);
  keyboardShortcutsBound = true;
}

function isPlainSpaceKey(e: KeyboardEvent): boolean {
  return (
    (e.code === "Space" || e.key === " " || e.key === "Spacebar") &&
    !e.ctrlKey &&
    !e.metaKey &&
    !e.altKey &&
    !e.shiftKey
  );
}

function isPlainEscapeKey(e: KeyboardEvent): boolean {
  return (
    (e.code === "Escape" || e.key === "Escape" || e.key === "Esc") &&
    !e.ctrlKey &&
    !e.metaKey &&
    !e.altKey &&
    !e.shiftKey
  );
}

function isPlayerPlaybackShortcutTarget(target: EventTarget | null): boolean {
  if (!target) return false;
  if (target === audio) return true;
  if (isEditableTarget(target)) return false;
  if (!(target instanceof Element) || !target.closest) return false;
  return !!target.closest("#player-bar, #player-video-panel");
}

// True for text-entry targets where native key behavior (typing a space,
// clearing/blurring on Escape) must win over the global player shortcuts.
function isEditableTarget(target: EventTarget | null): boolean {
  if (!target) return false;
  if (!(target instanceof Element)) return false;
  if ((target as HTMLElement).isContentEditable) return true;
  if (target.matches && target.matches("input, textarea, select")) return true;
  if (!target.closest) return false;
  const editable = target.closest<HTMLElement>("[contenteditable]");
  return !!(editable && editable.isContentEditable);
}

function isKeyboardShortcutIgnoredTarget(target: EventTarget | null): boolean {
  if (!target) return false;
  if (isPlayerPlaybackShortcutTarget(target)) return false;
  if (isEditableTarget(target)) return true;
  if (!(target instanceof Element) || !target.closest) return false;

  return !!target.closest(INTERACTIVE_SELECTOR);
}

function hasActiveMedia(): boolean {
  return !!audio && !!(audio.currentSrc || audio.getAttribute("src"));
}

function isMediaPlaying(): boolean {
  return hasActiveMedia() && !audio!.paused && !audio!.ended;
}

// True when the player is not actively occupying a track — nothing loaded, or
// the last track has naturally ended. A paused mid-track is NOT idle (append
// behind it, don't hijack). Used by playPlaylist to decide whether to auto-start.
function playerIdle(): boolean {
  return !hasActiveMedia() || (!!audio && audio.ended);
}

function onGlobalKeydown(e: KeyboardEvent): void {
  if (e.defaultPrevented) return;

  // Escape folds the inline video panel, like clicking Watch or dead space.
  // It must work even when a control inside the panel (e.g. the close button)
  // is focused, so it skips the interactive-target filter and only defers to
  // text fields, where native Escape (clear/blur) should win.
  if (isPlainEscapeKey(e)) {
    if (isEditableTarget(e.target)) return;
    if (!isVideoPanelOpen()) return;
    e.preventDefault();
    tracing("escape close video", {});
    hideVideoPanel();
    return;
  }

  if (!isPlainSpaceKey(e)) return;
  if (isKeyboardShortcutIgnoredTarget(e.target)) return;
  if (!hasActiveMedia()) return;

  e.preventDefault();
  if (e.repeat) return;

  tracing(audio!.paused ? "spacebar play" : "spacebar pause", {});
  togglePause();
}

// All .btn-like elements for a given concert track (card list, sidebar, etc.).
// The hx-post URL is the stable per-track key present on every copy.
function likeButtonsFor(concertId: number, trackIdx: number | null): NodeListOf<HTMLElement> {
  return document.querySelectorAll<HTMLElement>(
    `.btn-like[hx-post="/concerts/${concertId}/tracks/${trackIdx}/like"]`,
  );
}

// Sync liked state to all copies of the track's star buttons, player star,
// and any matching queue entries.
function applyLike(concertId: number | null, trackIdx: number | null, liked: boolean): void {
  if (concertId == null) return;
  likeButtonsFor(concertId, trackIdx).forEach((lb) => {
    lb.classList.toggle("liked", liked);
    lb.textContent = liked ? "★" : "☆";
  });
  let queueDirty = false;
  queue.forEach((e) => {
    if (e.concertId === concertId && e.trackIdx === trackIdx && e.liked !== liked) {
      e.liked = liked;
      queueDirty = true;
    }
  });
  if (queueDirty) renderQueue();
  if (state.concertId === concertId && state.trackIdx === trackIdx) {
    state.liked = liked;
    updateLikeStar();
    updateDeleteButton();
  }
}

interface HtmxSwapEvent extends Event {
  detail: { target?: Element };
}

// After an htmx:afterSwap (like-button outerHTML swap or whole-card swap),
// propagate the new like state to all copies of that track's star. Reads the
// live DOM rather than parsing the possibly-detached swapped-out element.
function syncLikeFromTrackList(evt: HtmxSwapEvent): void {
  const target = evt.detail?.target;
  // When a concert card is swapped (track deleted via htmx), refresh the sidebar
  // so availability counts and greyed rows stay in sync.
  if (
    target &&
    target.id === `concert-${state.concertId}` &&
    isSidebarOpen() &&
    state.concertId != null
  ) {
    loadSidebarTracks(state.concertId);
  }
  let concertId: number | null = null;
  let trackIdx: number | null = null;
  if (target && target.getAttribute) {
    const hxPost = target.getAttribute("hx-post");
    const m = hxPost && hxPost.match(/\/concerts\/(\d+)\/tracks\/(\d+)\/like/);
    if (m) {
      concertId = parseInt(m[1]!, 10);
      trackIdx = parseInt(m[2]!, 10);
    }
  }
  // Fallback: when a whole card is swapped (delete/status), re-check the playing track.
  if (concertId == null) {
    concertId = state.concertId;
    trackIdx = state.trackIdx;
  }
  if (concertId == null) return;
  const lb = likeButtonsFor(concertId, trackIdx)[0];
  if (!lb) return;
  const liked = lb.classList.contains("liked");
  applyLike(concertId, trackIdx, liked);
}

// After an in-place #content swap or a Back/Forward history restore re-creates
// the listen buttons, re-assert the JS-driven player UI: the playing-track
// highlight and the #player-like / #player-delete display (the player bar
// itself is never swapped, so the audio keeps playing untouched).
function reassertPlayerUi(): void {
  reapplyPlaying();
  updateLikeStar();
  updateDeleteButton();
  updateAddButton();
  // A card swap replaces the buttons of a concert whose prepare chain is in
  // flight; re-apply the pending mark and the disabled state (the server
  // also renders them disabled once its job state catches up).
  if (pendingPlay) {
    disableCardTracks(pendingPlay.concertId);
    markPreparing();
  }
}

function setPlayPauseIcon(playing: boolean): void {
  const btn = byIdOrNull("player-play-pause");
  if (btn) btn.textContent = playing ? "⏸" : "▶";
}

function showBar(): void {
  bar!.classList.add("active");
  document.body.classList.add("player-active");
}

function hideError(): void {
  const el = byIdOrNull("player-error");
  if (el) el.style.display = "none";
}

function showError(msg: string): void {
  const el = byIdOrNull("player-error");
  if (el) {
    el.textContent = msg;
    el.style.display = "inline";
  }
}

function updateInfo(
  title: string,
  artist: string,
  trackIdx: number | null,
  concertId: number | null,
): void {
  const t = byIdOrNull("player-title");
  const a = byIdOrNull<HTMLAnchorElement>("player-artist");
  const n = byIdOrNull("player-track");
  if (t) t.textContent = title;
  if (a) {
    a.textContent = artist;
    // Point the artist link at the concert detail page. This href is only the
    // native fallback (middle-click / Cmd-click "open in new tab"); a plain
    // click is handled by openConcert(), which does an htmx partial swap so
    // playback continues.
    if (concertId != null) a.setAttribute("href", `/concerts/${concertId}`);
  }
  if (n) {
    // track_index is the 0-based set-list position; null for whole-album playback.
    if (trackIdx != null) {
      n.textContent = "#" + (trackIdx + 1);
      n.style.display = "inline-block";
    } else {
      n.textContent = "";
      n.style.display = "none";
    }
  }
}

// Show or hide the playlist-context label in the player bar. Called from play()
// with the playlistName carried on the queue entry, so the label appears while
// playlist tracks are playing and clears automatically when playback moves to a
// non-playlist source (startAlbum / startTrack / set-list auto-advance).
function updatePlaylistLabel(name: string | null): void {
  const el = byIdOrNull("player-playlist");
  if (!el) return;
  if (name) {
    el.textContent = "♫ " + name;
    el.style.display = "block";
  } else {
    el.textContent = "";
    el.style.display = "none";
  }
}

function onTimeUpdate(): void {
  const seek = byIdOrNull<HTMLInputElement>("player-seek");
  const time = byIdOrNull("player-time");
  if (!Number.isFinite(audio!.duration) || audio!.duration <= 0) return;
  if (seek) {
    seek.max = String(Math.ceil(audio!.duration)); // set once-ish; ceil so the end is reachable
    seek.value = String(audio!.currentTime);
  }
  if (time) time.textContent = formatTime(audio!.currentTime) + " / " + formatTime(audio!.duration);
}

// Play the next queued track, else auto-advance to the following track, else
// we have reached the end of everything: collapse the inline video panel so
// its frozen last frame doesn't cover the page and block selecting another
// track. Shared by the natural end-of-track and load-error dead ends.
async function advanceOrCollapse(): Promise<void> {
  // Concert reconstruction mode: advance within the concert item list first.
  if (state.concert) {
    await advanceConcert();
    return;
  }
  if (await playFromQueue()) return;
  if (await playNextTrack()) return;
  hideVideoPanel();
}

async function onEnded(): Promise<void> {
  await advanceOrCollapse();
}

async function onError(): Promise<void> {
  showError("Failed to load media");
  tracing("audio error", audio!.error);
  await advanceOrCollapse();
}

function cancelAutoAdvance(): void {
  if (autoAdvanceController) {
    autoAdvanceController.abort();
    autoAdvanceController = null;
  }
}

// ── Prepare flow: play a track that doesn't exist on disk yet ────────────
// Clicking a missing track POSTs /prepare (which chains download → split as
// needed), then polls /prepare-status until the track file appears and
// auto-plays it. Lives in Player state (outside #content) so it survives
// the htmx card swaps driven by the card's own status polling.

let pendingPlay: PendingPlay | null = null;
const PREPARE_POLL_MS = 2000;
// Downloads can take many minutes; give the whole chain a generous cap so
// an abandoned poll loop can't run forever.
const PREPARE_TIMEOUT_MS = 30 * 60 * 1000;

function setStatus(msg: string): void {
  const el = byIdOrNull("player-status");
  if (!el) return;
  el.textContent = msg || "";
  el.style.display = msg ? "inline" : "none";
}

// Best-effort visual mark on the pending track's button; re-applied by
// reassertPlayerUi after card swaps replace the button element.
function markPreparing(): void {
  if (!pendingPlay) return;
  findTrackButtons(pendingPlay.concertId, pendingPlay.trackIdx).forEach((btn) =>
    btn.classList.add("preparing"),
  );
}

function clearPreparing(): void {
  document.querySelectorAll(".btn-track-listen.preparing").forEach((b) => {
    b.classList.remove("preparing");
  });
}

// Disable the card's tracks button and track buttons immediately on click;
// subsequent card swaps render them disabled server-side (tracks_busy).
function disableCardTracks(concertId: number): void {
  const card = byIdOrNull("concert-" + concertId);
  if (!card) return;
  card.querySelectorAll<HTMLButtonElement>(".btn-tracks, .btn-track-listen").forEach((b) => {
    b.disabled = true;
  });
}

function cancelPendingPlay(): void {
  if (!pendingPlay) return;
  if (pendingPlay.timer) clearTimeout(pendingPlay.timer);
  pendingPlay = null;
  clearPreparing();
  setStatus("");
}

function failPendingPlay(msg: string): void {
  tracing("preparePlay failed", { msg });
  cancelPendingPlay();
  showError(msg);
}

async function preparePlay(
  btn: HTMLElement | null,
  concertId: number,
  trackIdx: number,
): Promise<void> {
  if (!audio) init();
  cancelPendingPlay();
  let resp: Response;
  try {
    resp = await postPrepare(concertId);
  } catch (e) {
    showError("Prepare failed");
    tracing("preparePlay fetch failed", e);
    return;
  }
  if (!resp.ok) {
    showError("Prepare failed");
    tracing("preparePlay non-ok", { status: resp.status });
    return;
  }
  hideError();
  pendingPlay = {
    concertId,
    trackIdx,
    timer: null,
    deadline: Date.now() + PREPARE_TIMEOUT_MS,
  };
  tracing("preparePlay started", { concertId, trackIdx });
  if (bar) showBar();
  const title = btn && btn.textContent ? btn.textContent.trim() : "track";
  setStatus(`Preparing “${title}”…`);
  disableCardTracks(concertId);
  markPreparing();
  // The card only self-polls when it was rendered with a job in progress;
  // this job just started, so refresh the card once to kick off its status
  // polling (downloading/splitting badges, disabled buttons, final state).
  const card = byIdOrNull("concert-" + concertId);
  if (card && window.htmx) {
    window.htmx.ajax("GET", `/concerts/${concertId}/status`, {
      target: `#concert-${concertId}`,
      swap: "outerHTML",
    });
  }
  // POST /prepare returns the same JSON as prepare-status, so seed the
  // first status from it instead of waiting a full poll interval.
  const status = (await resp.json().catch(() => null)) as PrepareStatus | null;
  if (status) {
    await applyPrepareStatus(status);
  } else if (pendingPlay) {
    pendingPlay.timer = setTimeout(pollPrepare, PREPARE_POLL_MS);
  }
}

// Act on one prepare-status payload: play when the pending track's file
// exists, stop on job error or timeout, otherwise show progress and re-arm
// the poll timer.
async function applyPrepareStatus(s: PrepareStatus): Promise<void> {
  if (!pendingPlay) return;
  const { concertId, trackIdx } = pendingPlay;
  if (s.tracks_present && s.tracks_present[trackIdx]) {
    const p = pendingPlay;
    cancelPendingPlay();
    tracing("preparePlay ready, playing", { concertId, trackIdx });
    await playTrack(findTrackButton(p.concertId, p.trackIdx), p.concertId, p.trackIdx);
    return;
  }
  if (s.download === "download-error" || s.split === "split-error") {
    failPendingPlay("Preparing tracks failed");
    return;
  }
  if (Date.now() > pendingPlay.deadline) {
    failPendingPlay("Preparing tracks timed out");
    return;
  }
  setStatus(s.split === "splitting" ? "Preparing… (splitting)" : "Preparing… (downloading)");
  pendingPlay.timer = setTimeout(pollPrepare, PREPARE_POLL_MS);
}

async function pollPrepare(): Promise<void> {
  if (!pendingPlay) return;
  const { concertId } = pendingPlay;
  let s: PrepareStatus;
  try {
    s = await getPrepareStatus(concertId);
  } catch (e) {
    // Transient (server restart, network blip): keep polling until the cap.
    tracing("pollPrepare fetch failed", e);
    if (Date.now() > pendingPlay.deadline) {
      failPendingPlay("Preparing tracks timed out");
      return;
    }
    pendingPlay.timer = setTimeout(pollPrepare, PREPARE_POLL_MS);
    return;
  }
  await applyPrepareStatus(s);
}

// Whether the track's file exists right now (media-info 404s when missing).
// Returns { title, liked } from media-info, or null if the track file is
// missing or unreachable. Used by the enqueue path to capture title/liked
// without a separate fetch.
async function trackMediaInfo(
  concertId: number,
  trackIdx: number,
): Promise<{ title: string; liked: boolean } | null> {
  try {
    const info = await getTrackMediaInfoOrNull(concertId, trackIdx);
    if (!info) return null;
    return { title: info.title, liked: !!info.liked };
  } catch (e) {
    tracing("trackMediaInfo fetch failed", e);
    return null;
  }
}

// Returns true when a following track started playing, false otherwise (no
// next track, fetch error, or aborted). Callers decide what to do when false;
// this never stops playback itself.
async function playNextTrack(): Promise<boolean> {
  if (state.trackIdx == null || state.concertId == null) {
    setPlayPauseIcon(false);
    return false;
  }

  cancelAutoAdvance();
  autoAdvanceController = new AbortController();
  const signal = autoAdvanceController.signal;
  const concertId = state.concertId;
  const trackIdx = state.trackIdx;

  try {
    const info = await getNextTrackMediaInfo(concertId, trackIdx, signal);
    if (signal.aborted) return false;

    const btn = findTrackButton(concertId, info.track_index ?? null);
    await play(
      btn,
      info.url,
      info.title,
      info.artist,
      concertId,
      info.track_index ?? null,
      `/concerts/${concertId}/tracks/${info.track_index}/listen`,
      info.is_video,
      `/concerts/${concertId}/tracks/${info.track_index}/watch`,
      info.has_next,
      info.liked,
      info.has_prev,
    );
    return true;
  } catch (e) {
    if (!(e instanceof DOMException && e.name === "AbortError")) {
      showError("Couldn't load next track");
      tracing("playNextTrack failed", e);
      setPlayPauseIcon(false);
    }
    return false;
  }
}

// Returns true when the preceding playable track started playing, false
// otherwise (no previous track, fetch error, or aborted). Like playNextTrack,
// this never stops playback itself.
async function playPrevTrack(): Promise<boolean> {
  if (state.trackIdx == null || state.concertId == null) {
    setPlayPauseIcon(false);
    return false;
  }

  cancelAutoAdvance();
  autoAdvanceController = new AbortController();
  const signal = autoAdvanceController.signal;
  const concertId = state.concertId;
  const trackIdx = state.trackIdx;

  try {
    const info = await getPrevTrackMediaInfo(concertId, trackIdx, signal);
    if (signal.aborted) return false;

    const btn = findTrackButton(concertId, info.track_index ?? null);
    await play(
      btn,
      info.url,
      info.title,
      info.artist,
      concertId,
      info.track_index ?? null,
      `/concerts/${concertId}/tracks/${info.track_index}/listen`,
      info.is_video,
      `/concerts/${concertId}/tracks/${info.track_index}/watch`,
      info.has_next,
      info.liked,
      info.has_prev,
    );
    return true;
  } catch (e) {
    if (!(e instanceof DOMException && e.name === "AbortError")) {
      showError("Couldn't load previous track");
      tracing("playPrevTrack failed", e);
      setPlayPauseIcon(false);
    }
    return false;
  }
}

function tracing(label: string, obj?: unknown): void {
  if (obj) console.warn("[Player]", label, obj);
}

function formatTime(seconds: number): string {
  const m = Math.floor(seconds / 60);
  const s = Math.floor(seconds % 60);
  return m + ":" + (s < 10 ? "0" : "") + s;
}

function findTrackButtons(concertId: number, trackIdx: number | null): NodeListOf<HTMLElement> {
  if (trackIdx != null) {
    return document.querySelectorAll<HTMLElement>(
      `[data-concert-id="${concertId}"][data-track-idx="${trackIdx}"]`,
    );
  }
  return document.querySelectorAll<HTMLElement>(
    `[data-concert-id="${concertId}"][data-role="listen-album"]`,
  );
}

function findTrackButton(concertId: number, trackIdx: number | null): HTMLElement | null {
  return findTrackButtons(concertId, trackIdx)[0] ?? null;
}

function clearPlaying(): void {
  document
    .querySelectorAll(".btn-track-listen.playing, .btn-listen.playing")
    .forEach((b) => b.classList.remove("playing"));
}

function markPlaying(concertId: number, trackIdx: number | null): void {
  clearPlaying();
  findTrackButtons(concertId, trackIdx).forEach((b) => b.classList.add("playing"));
}

function markPlayingInterlude(concertId: number, interludeIdx: number): void {
  document
    .querySelectorAll(`[data-concert-id="${concertId}"][data-interlude-idx="${interludeIdx}"]`)
    .forEach((b) => b.classList.add("playing"));
}

function reapplyPlaying(): void {
  if (state.concertId == null) return;
  if (!audio!.paused) {
    if (state.concert) {
      const item = state.concert.items[state.concert.pos];
      if (item && item.kind === "interlude") {
        markPlaying(state.concertId, null); // clears without marking album btn
        if (item.interlude_index != null) {
          markPlayingInterlude(state.concert.id, item.interlude_index);
        }
        return;
      }
    }
    markPlaying(state.concertId, state.trackIdx);
  }
}

// The Watch (toggle inline video) and Open (launch system player) buttons only
// make sense for video tracks; hide both for audio-only playback.
function updateMediaButtons(isVideo: boolean): void {
  const display = isVideo ? "inline-block" : "none";
  const watchBtn = byIdOrNull("player-watch");
  const open = byIdOrNull("player-open");
  if (watchBtn) watchBtn.style.display = display;
  if (open) open.style.display = display;
}

// How long the minimize button stays visible after the last mouse movement.
const VIDEO_CONTROLS_IDLE_MS = 2500;
let videoControlsTimer: ReturnType<typeof setTimeout> | null = null;

// Reveal the minimize button on mouse movement (or a touch) while watching, then
// fade it back out once the pointer goes idle.
function isVideoPanelOpen(): boolean {
  const panel = byIdOrNull("player-video-panel");
  return !!panel && panel.classList.contains("open");
}

function showVideoControls(): void {
  const panel = byIdOrNull("player-video-panel");
  if (!panel || !panel.classList.contains("open")) return;
  panel.classList.add("controls-visible");
  if (videoControlsTimer) clearTimeout(videoControlsTimer);
  videoControlsTimer = setTimeout(() => panel.classList.remove("controls-visible"), VIDEO_CONTROLS_IDLE_MS);
}

// A click on an interactive element is the user trying to *do* that thing (navigate,
// play, queue, like, …), not dismiss the video — so those clicks perform their action
// and leave the panel open. Only clicks on "dead space" fold the video.
// Recognizes native controls and inline onclick handlers (the project's convention); a
// future control bound only via addEventListener would need adding here to be exempted.
const INTERACTIVE_SELECTOR = 'a, button, input, select, textarea, label, [role="button"], [onclick]';

// Pure: does a click on `target` fall on dead space outside the player, and so
// dismiss the video? (false for clicks inside the player or on any interactive control)
function clickShouldDismiss(target: EventTarget | null, container: Element | null): boolean {
  if (!container || !target) return false;
  if (!(target instanceof Node)) return false;
  if (container.contains(target)) return false;
  if (target instanceof Element && target.closest && target.closest(INTERACTIVE_SELECTOR)) {
    return false;
  }
  return true;
}

// While the video panel is open, a click on the empty page area above it folds it
// back down, like clicking Watch.
function onOutsideVideoClick(e: MouseEvent): void {
  const container = byIdOrNull("player-container");
  if (!clickShouldDismiss(e.target, container)) return;
  tracing("outsideClick dismiss video", { tag: e.target instanceof Element ? e.target.tagName : null });
  hideVideoPanel();
}

function showVideoPanel(): void {
  const panel = byIdOrNull("player-video-panel");
  if (!panel || panel.classList.contains("open")) return;
  tracing("showVideoPanel", {});
  panel.classList.add("open");
  // Attached synchronously: the click that opened the panel always comes
  // from a button (#player-watch or a track-list Watch button), which
  // clickShouldDismiss already exempts as an interactive control — so the
  // opening click can't bubble up and immediately re-close the panel.
  document.addEventListener("click", onOutsideVideoClick);
}

function hideVideoPanel(): void {
  const panel = byIdOrNull("player-video-panel");
  if (!panel || !panel.classList.contains("open")) return;
  tracing("hideVideoPanel", {});
  panel.classList.remove("open");
  panel.classList.remove("controls-visible");
  if (videoControlsTimer) clearTimeout(videoControlsTimer);
  document.removeEventListener("click", onOutsideVideoClick);
}

// Player-bar Watch button: fold the inline video panel up or down. The video
// is the already-playing #player-audio element, so revealing it needs no resync.
function watch(): void {
  if (isVideoPanelOpen()) hideVideoPanel();
  else showVideoPanel();
}

// Show the add-to-playlist "+" button only while an individual track is playing
// (whole-album playback has no single-track target). Mirrors updateLikeStar.
function updateAddButton(): void {
  const btn = byIdOrNull("player-add-pl");
  if (!btn) return;
  btn.style.display = state.trackIdx == null ? "none" : "inline-block";
}

// Open the add-to-playlist sidebar panel for the currently-playing track.
// Reads state directly (same as toggleLike). No-op for whole-album playback.
function addToPlaylist(): void {
  if (state.trackIdx == null || state.concertId == null) return;
  if (!window.Playlists) {
    tracing("addToPlaylist: Playlists not available", {});
    return;
  }
  const label = (byIdOrNull("player-title")?.textContent || "").trim();
  tracing("addToPlaylist", { concertId: state.concertId, trackIdx: state.trackIdx, label });
  window.Playlists.openAdd({
    type: "track",
    concertId: state.concertId,
    trackIndex: state.trackIdx,
    label,
  });
}

// Show the like star only while an individual track is playing (whole-album
// playback has no per-track like), and reflect the current liked state.
function updateLikeStar(): void {
  const star = byIdOrNull("player-like");
  if (!star) return;
  if (state.trackIdx == null) {
    star.style.display = "none";
    return;
  }
  star.style.display = "inline-block";
  star.textContent = state.liked ? "★" : "☆";
  star.classList.toggle("liked", state.liked);
}

// Show the delete button only while an individual track is playing (no
// per-track delete for whole-album playback) and the track is not starred —
// a starred track is protected from deletion until it is unstarred.
function updateDeleteButton(): void {
  const btn = byIdOrNull("player-delete");
  if (!btn) return;
  btn.style.display = state.trackIdx == null || state.liked ? "none" : "inline-block";
}

function setLikeState(liked: boolean): void {
  applyLike(state.concertId, state.trackIdx, liked);
}

// Player-bar like star: toggle the like for the currently-playing track.
// Optimistically flips the UI, POSTs to the same /like endpoint the track-list
// star uses, and reverts on failure. The HTML body is ignored — the in-place
// button update already reflects the new state.
async function toggleLike(): Promise<void> {
  if (state.trackIdx == null || state.concertId == null) return;
  const concertId = state.concertId;
  const trackIdx = state.trackIdx;
  const next = !state.liked;
  setLikeState(next);
  try {
    const resp = await postLikeTrack(concertId, trackIdx);
    if (!resp.ok) throw new Error("like POST failed: " + resp.status);
  } catch (e) {
    // Only revert if the user hasn't moved on to a different track meanwhile.
    if (state.concertId === concertId && state.trackIdx === trackIdx) {
      setLikeState(!next);
    }
    showError("Like failed");
    tracing("toggleLike failed", e);
  }
}

// There is "something next" when in concert mode with a next item, the queue
// is non-empty, or the current track has a following track to auto-advance to.
// Disable the Next button otherwise so clicking it cannot stop the current
// track with nothing to replace it.
function updateNextButton(): void {
  const btn = byIdOrNull<HTMLButtonElement>("player-next");
  if (!btn) return;
  if (state.concert) {
    btn.disabled = state.concert.pos + 1 >= state.concert.items.length;
  } else {
    btn.disabled = queue.length === 0 && !state.hasNext;
  }
}

// Disable the Back button when there is no earlier item to go back to.
function updatePrevButton(): void {
  const btn = byIdOrNull<HTMLButtonElement>("player-prev");
  if (!btn) return;
  if (state.concert) {
    btn.disabled = state.concert.pos <= 0;
  } else {
    btn.disabled = !state.hasPrev;
  }
}

async function play(
  // Unused (also true of the original): callers pass the clicked button for
  // signature parity with startAlbum/startTrack, but play() never reads it.
  _btn: HTMLElement | null,
  url: string,
  title: string,
  artist: string,
  concertId: number,
  trackIdx: number | null,
  listenUrl: string | null,
  isVideo: boolean,
  watchUrl: string | null,
  hasNext: boolean,
  liked: boolean,
  hasPrev: boolean,
  recordListen = true,
  playlistName: string | null = null,
): Promise<void> {
  if (!audio) init();
  if (!audio) return;

  hideError();
  setStatus("");
  showBar();
  updateInfo(title, artist, trackIdx, concertId);
  updatePlaylistLabel(playlistName);
  // Clear concert mode first so non-concert callers always get a clean slate.
  // playConcertItem restores state.concert immediately after play() returns.
  state.concert = null;
  markPlaying(concertId, trackIdx);

  state.concertId = concertId;
  state.trackIdx = trackIdx;
  state.isVideo = isVideo;
  state.watchUrl = watchUrl;
  state.hasNext = !!hasNext;
  state.hasPrev = !!hasPrev;
  state.liked = !!liked;
  updateLikeStar();
  updateDeleteButton();
  updateAddButton();
  updateMediaButtons(isVideo);
  // An audio-only track can't be watched; collapse the panel if it was open.
  // A video track keeps the panel open so auto-advance keeps showing video.
  if (!isVideo) hideVideoPanel();
  updateNextButton();
  updatePrevButton();

  if (isSidebarOpen() && concertId !== sidebarConcertId) {
    loadSidebarTracks(concertId);
  }

  audio.src = url;
  try {
    await audio.play();
  } catch (e) {
    showError("Playback blocked");
    tracing("play() rejected", e);
    return;
  }

  if (listenUrl && recordListen) {
    postEvent(listenUrl).catch(() => {});
  }
}

// Fetch the whole-album media info and start playing it now (no enqueue).
// Returns true when in-browser playback started; false on error or when the
// file is not browser-playable (in which case it falls back to window.open).
async function startAlbum(
  btn: HTMLElement | null,
  concertId: number,
  recordListen = true,
): Promise<boolean> {
  cancelAutoAdvance();
  try {
    const info = await getMediaInfo(concertId);
    if (!info.playable) {
      window.open(info.url, "_blank");
      return false;
    }
    await play(
      btn,
      info.url,
      info.title,
      info.artist,
      concertId,
      null,
      `/concerts/${concertId}/listen`,
      info.is_video,
      `/concerts/${concertId}/watch`,
      info.has_next,
      info.liked,
      info.has_prev,
      recordListen,
    );
    return true;
  } catch (e) {
    if (btn instanceof HTMLElement) {
      btn.classList.add("btn-listen-error");
      btn.textContent = "Error";
    }
    tracing("startAlbum fetch failed", e);
    return false;
  }
}

async function playAlbum(btn: HTMLElement | null, concertId: number): Promise<void> {
  await startAlbum(btn, concertId);
}

// Fetch a track's media info and start playing it now (no enqueue, no
// toggle-pause). Returns true when in-browser playback started; false on
// error or non-playable file (falls back to window.open).
async function startTrack(
  btn: HTMLElement | null,
  concertId: number,
  trackIdx: number,
): Promise<boolean> {
  cancelAutoAdvance();
  try {
    const info = await getTrackMediaInfoOrNull(concertId, trackIdx);
    if (!info) {
      // Track file missing (not split yet, or deleted): enter the prepare
      // flow — download/split as needed and auto-play when it appears.
      await preparePlay(btn, concertId, trackIdx);
      return false;
    }
    if (!info.playable) {
      window.open(info.url, "_blank");
      return false;
    }
    await play(
      btn,
      info.url,
      info.title,
      info.artist,
      concertId,
      trackIdx,
      `/concerts/${concertId}/tracks/${trackIdx}/listen`,
      info.is_video,
      `/concerts/${concertId}/tracks/${trackIdx}/watch`,
      info.has_next,
      info.liked,
      info.has_prev,
    );
    return true;
  } catch (e) {
    if (btn instanceof HTMLElement) {
      btn.classList.add("btn-listen-error");
      btn.textContent = "Error";
    }
    tracing("startTrack fetch failed", e);
    return false;
  }
}

async function playTrack(
  btn: HTMLElement | null,
  concertId: number,
  trackIdx: number,
): Promise<void> {
  if (state.concertId === concertId && state.trackIdx === trackIdx && audio) {
    togglePause();
    return;
  }
  if (isMediaPlaying()) {
    // A missing track must still enter the prepare flow while something
    // else is playing; it gets enqueued once its file appears (pollPrepare
    // re-enters playTrack, which then lands in the enqueue branch).
    const info = await trackMediaInfo(concertId, trackIdx);
    if (!info) {
      await preparePlay(btn, concertId, trackIdx);
      return;
    }
    enqueue(concertId, trackIdx, info.title || (btn ? (btn.textContent || "").trim() : ""), info.liked);
    return;
  }
  await startTrack(btn, concertId, trackIdx);
}

// Resolve the first playable track index for a concert: track 0 normally, or
// the next playable track after it when track 0 has been deleted/removed.
// Reuses the server's next-media-info skip-deleted logic (the same one that
// drives auto-advance) so the two stay in sync. Returns null when the concert
// has no playable track.
async function firstAvailableTrackIndex(concertId: number): Promise<number | null> {
  try {
    // In the common case (track 0 present) startTrack re-fetches this same
    // media-info; the extra GET on a single button click is intentional — not
    // worth threading a prefetched body through the shared startTrack path.
    const head = await getTrackMediaInfoOrNull(concertId, 0);
    if (head) return 0;
    const next = await getNextTrackMediaInfo(concertId, 0).catch(() => null);
    if (next) return next.track_index ?? null;
  } catch (e) {
    tracing("firstAvailableTrackIndex failed", e);
  }
  return null;
}

// Tracks button on the card: play the split tracks starting from the first
// one that still exists (track 0 may have been deleted). When no track is
// playable at all (not split yet, or everything deleted), enter the prepare
// flow via track 0 — it downloads/splits and auto-plays when ready.
async function playTracks(btn: HTMLElement | null, concertId: number): Promise<void> {
  const trackIdx = await firstAvailableTrackIndex(concertId);
  if (trackIdx == null) {
    tracing("playTracks: no playable track, preparing", { concertId });
    await playTrack(btn, concertId, 0);
    return;
  }
  await playTrack(btn, concertId, trackIdx);
}

function togglePause(): void {
  if (!audio) return;
  if (audio.paused) {
    audio.play().catch((e) => {
      showError("Playback blocked");
      tracing("togglePause play rejected", e);
    });
  } else {
    audio.pause();
  }
}

function seek(val: string | number): void {
  if (!audio || !Number.isFinite(audio.duration) || audio.duration <= 0) return;
  audio.currentTime = Number(val);
}

let pendingSeekHandler: (() => void) | null = null;

// Set audio.currentTime when metadata is available, cancelling any prior
// pending seek so rapid preview clicks can't seek to a stale earlier time.
function seekWhenReady(seconds: number): void {
  if (pendingSeekHandler) {
    audio!.removeEventListener("loadedmetadata", pendingSeekHandler);
    pendingSeekHandler = null;
  }
  if (audio!.readyState >= 1) {
    audio!.currentTime = seconds;
  } else {
    pendingSeekHandler = () => {
      pendingSeekHandler = null;
      audio!.currentTime = seconds;
    };
    audio!.addEventListener("loadedmetadata", pendingSeekHandler, { once: true });
  }
}

// Start whole-album playback for concertId and seek to `seconds`. If the
// album is already current (bar showing, no individual track selected),
// just seek + resume; otherwise start fresh via startAlbum. Never records
// a listen event so splitter preview auditions don't spam the event log.
async function playAlbumAt(concertId: number, seconds: number): Promise<void> {
  if (!audio) init();
  if (!audio) return;
  if (state.concertId === concertId && state.trackIdx === null) {
    seekWhenReady(seconds);
    if (audio.paused) {
      audio.play().catch((e) => {
        showError("Playback blocked");
        tracing("playAlbumAt play rejected", e);
      });
    }
    return;
  }
  const started = await startAlbum(null, concertId, false);
  if (!started) return;
  seekWhenReady(seconds);
}

// Snapshot of what is currently playing. Returns { concertId, trackIdx }
// where trackIdx is null for whole-album playback.
function nowPlaying(): PlayerNowPlaying {
  return { concertId: state.concertId, trackIdx: state.trackIdx };
}

// Shared entry constructor so enqueue and playPlaylist both produce the same shape.
// playlistName is null for ad-hoc queued tracks and non-null when the entry came
// from a playlist (used by play() to show/clear the bar label). groupId is non-null
// only for playlist tracks; a contiguous run of entries sharing a groupId renders as
// one grouped block in the queue sidebar (see renderQueue).
function makeQueueEntry(
  concertId: number,
  trackIdx: number,
  title: string,
  liked: boolean,
  playlistName: string | null = null,
  groupId: number | null = null,
): QueueEntry {
  return {
    concertId,
    trackIdx,
    title,
    liked: !!liked,
    playlistName: playlistName || null,
    groupId: groupId || null,
  };
}

function enqueue(concertId: number, trackIdx: number, title: string, liked: boolean): void {
  if (queue.some((q) => q.concertId === concertId && q.trackIdx === trackIdx)) {
    tracing("enqueue duplicate skipped", { concertId, trackIdx });
    return;
  }
  queue.push(makeQueueEntry(concertId, trackIdx, title, liked));
  tracing("enqueue", { concertId, trackIdx, title, queueLength: queue.length });
  queueChanged();
}

// Load a playlist by id and append all its available resolved tracks to the
// queue. If the player is idle (nothing loaded, or the last track has ended)
// start playing immediately; otherwise let the current track finish first.
async function playPlaylist(playlistId: number): Promise<void> {
  tracing("playPlaylist", { playlistId });
  try {
    const data = await getPlaylist(playlistId);
    const name = data.playlist.name;
    const tracks = (data.resolved_tracks || []).filter((t) => t.available);
    if (tracks.length === 0) {
      showError("Nothing to play in this playlist");
      tracing("playPlaylist empty", { playlistId, name });
      return;
    }
    // Mint one groupId for this entire play action so all enqueued tracks form a
    // single removable group in the queue sidebar (see renderQueue/removeGroup).
    const groupId = nextGroupId++;
    for (const t of tracks) {
      if (t.track_index == null) continue; // available implies a track index; guard for the type
      queue.push(makeQueueEntry(t.concert_id, t.track_index, t.title, false, name, groupId));
    }
    tracing("playPlaylist enqueued", {
      playlistId,
      name,
      groupId,
      count: tracks.length,
      queueLength: queue.length,
    });
    queueChanged();
    if (playerIdle()) await playFromQueue();
  } catch (e) {
    showError("Couldn't load playlist");
    tracing("playPlaylist failed", e);
  }
}

async function playFromQueue(): Promise<boolean> {
  while (queue.length > 0) {
    const entry = queue.shift()!;
    queueChanged();
    cancelAutoAdvance();
    tracing("playFromQueue", { concertId: entry.concertId, trackIdx: entry.trackIdx });

    try {
      const info = await getTrackMediaInfoOrNull(entry.concertId, entry.trackIdx);
      if (!info) {
        tracing("playFromQueue track unavailable", {
          concertId: entry.concertId,
          trackIdx: entry.trackIdx,
        });
        continue;
      }
      if (!info.playable) continue;

      const btn = findTrackButton(entry.concertId, entry.trackIdx);
      await play(
        btn,
        info.url,
        info.title,
        info.artist,
        entry.concertId,
        entry.trackIdx,
        `/concerts/${entry.concertId}/tracks/${entry.trackIdx}/listen`,
        info.is_video,
        `/concerts/${entry.concertId}/tracks/${entry.trackIdx}/watch`,
        info.has_next,
        info.liked,
        info.has_prev,
        true,
        entry.playlistName,
      );
      return true;
    } catch (e) {
      tracing("playFromQueue failed", e);
    }
  }
  return false;
}

async function skipToNext(): Promise<void> {
  if (!audio) return;
  // Concert reconstruction mode: advance within the concert item list.
  if (state.concert) {
    cancelAutoAdvance();
    audio.pause();
    tracing("skipToNext concert", { pos: state.concert.pos });
    await advanceConcert();
    return;
  }
  // Defensive guard mirroring updateNextButton(): never pause the current
  // track when there is nothing queued and nothing to auto-advance to.
  if (queue.length === 0 && !state.hasNext) {
    tracing("skipToNext ignored: nothing next", {});
    return;
  }
  tracing("skipToNext", { queueLength: queue.length });
  cancelAutoAdvance();
  audio.pause();

  const played = await playFromQueue();
  if (!played) playNextTrack();
}

// Back button: go to the preceding playable track in the set list. Defensively
// guarded like skipToNext so it can't pause the current track with nothing to
// replace it. The queue (a forward play-ahead list) is left untouched.
async function skipToPrev(): Promise<void> {
  if (!audio) return;
  // Concert reconstruction mode: go back within the concert item list.
  if (state.concert) {
    if (state.concert.pos <= 0) {
      tracing("skipToPrev concert: at start", {});
      return;
    }
    cancelAutoAdvance();
    audio.pause();
    tracing("skipToPrev concert", { pos: state.concert.pos });
    await playConcertItem(state.concert.pos - 1);
    return;
  }
  if (!state.hasPrev) {
    tracing("skipToPrev ignored: nothing previous", {});
    return;
  }
  tracing("skipToPrev", {});
  cancelAutoAdvance();
  audio.pause();
  await playPrevTrack();
}

function updateQueueBadge(): void {
  const badge = byIdOrNull("player-queue-badge");
  if (!badge) return;
  if (queue.length > 0) {
    badge.textContent = String(queue.length);
    badge.style.visibility = "visible";
    badge.title = queue.map((q) => q.title).join("\n");
  } else {
    badge.textContent = "";
    badge.style.visibility = "hidden";
    badge.title = "";
  }
}

// Link-out: launch the current file in the system player via the server's
// `open`. state.watchUrl is the POST endpoint set by play(). This is the only
// path that still records a server-side Watch event.
async function openExternal(): Promise<void> {
  if (!state.watchUrl) return;
  tracing("openExternal", { watchUrl: state.watchUrl });
  // Handing off to the system player: stop our playback so audio doesn't play
  // in both places at once.
  if (audio) audio.pause();
  try {
    await postEvent(state.watchUrl);
  } catch (e) {
    showError("Couldn't open externally");
    tracing("openExternal fetch failed", e);
  }
}

// Player-bar artist link: navigate to the playing concert's detail page via an
// htmx partial swap of #content so the player keeps playing (a full-page nav
// would reload the page and stop playback). Modifier-clicks fall through to the
// native href so "open in new tab" still works, matching htmx boost's behavior.
// htmx reads hx-target/hx-select/hx-swap/hx-push-url from the source element.
function openConcert(e?: Event): void {
  const mouseEvent = e as MouseEvent | undefined;
  if (mouseEvent && (mouseEvent.metaKey || mouseEvent.ctrlKey || mouseEvent.shiftKey)) return;
  if (e) e.preventDefault();
  if (state.concertId == null || !window.htmx) {
    tracing("openConcert skipped", { concertId: state.concertId, htmx: !!window.htmx });
    return;
  }
  const source = e?.currentTarget instanceof Element ? e.currentTarget : undefined;
  window.htmx.ajax("GET", `/concerts/${state.concertId}`, source ? { source } : {});
}

// Track-list/detail Watch button: start this track playing inline and fold up
// the video panel.
async function watchTrackDirect(
  btn: HTMLElement | null,
  concertId: number,
  trackIdx: number,
): Promise<void> {
  if (await startTrack(btn, concertId, trackIdx)) showVideoPanel();
}

function queueChanged(): void {
  renderQueue();
  updateQueueBadge();
  updateNextButton();
}

// Shared button factory: all queue icon-buttons (▶ play, ✕ remove, group ✕)
// share the same shape — only className/title/glyph/handler differ.
function makeIconButton(
  className: string,
  title: string,
  glyph: string,
  onClick: () => void,
): HTMLButtonElement {
  const b = document.createElement("button");
  b.className = className;
  b.title = title;
  b.textContent = glyph;
  b.onclick = onClick;
  return b;
}

// Render the queue section of the sidebar using DOM APIs (textContent only —
// titles are untrusted data and must never be set via innerHTML).
//
// Queue entries whose groupId is non-null and contiguous form a playlist group:
// a header row (playlist name + single ✕ to remove the whole group) followed by
// indented song rows. Ad-hoc entries (groupId===null) render exactly as before.
// The list is reversed (highest index at top) and bottom-scrolled as before.
function renderQueue(): void {
  const list = byIdOrNull("sidebar-queue-list");
  const empty = byIdOrNull("sidebar-queue-empty");
  if (!list) return;
  list.replaceChildren();
  if (queue.length === 0) {
    if (empty) empty.style.display = "";
    return;
  }
  if (empty) empty.style.display = "none";

  // prevGroupId tracks the last seen groupId so we emit one header per contiguous run.
  // Seeded to undefined (not null) because null is the meaningful "ad-hoc" value.
  let prevGroupId: number | null | undefined = undefined;
  // headeredGroups guards the load-bearing contiguity assumption: if a groupId
  // reappears non-contiguously (future non-tail insert), log rather than silently
  // splitting it into two headers.
  const headeredGroups = new Set<number>();

  for (let i = queue.length - 1; i >= 0; i--) {
    const entry = queue[i]!;

    // Emit a group header whenever we enter a new playlist group (groupId changes).
    if (entry.groupId !== null && entry.groupId !== prevGroupId) {
      if (headeredGroups.has(entry.groupId)) {
        tracing("renderQueue non-contiguous group — group split across queue", {
          groupId: entry.groupId,
        });
      }
      headeredGroups.add(entry.groupId);
      prevGroupId = entry.groupId;

      const header = document.createElement("li");
      header.className = "queue-group";

      const nameSpan = document.createElement("span");
      nameSpan.className = "queue-group-name";
      nameSpan.textContent = entry.playlistName || "Playlist";

      // Capture groupId in a const so the closure is stable across loop iterations.
      const gid = entry.groupId;
      const groupRemoveBtn = makeIconButton("btn-queue-remove", "Remove playlist from queue", "✕", () =>
        removeGroup(gid),
      );

      header.append(nameSpan, groupRemoveBtn);
      list.appendChild(header);
    } else if (entry.groupId === null) {
      prevGroupId = null;
    }

    // Song row — nested (indented) when it belongs to a group.
    const li = document.createElement("li");
    li.className = entry.groupId !== null ? "queue-item nested" : "queue-item";

    const idx = i;
    const playBtn = makeIconButton("btn-queue-play", "Play now", "▶", () => playQueueEntryNow(idx));
    const removeBtn = makeIconButton("btn-queue-remove", "Remove from queue", "✕", () => dequeue(idx));

    const titleSpan = document.createElement("span");
    titleSpan.className = "queue-title";
    titleSpan.textContent = entry.title;

    const star = document.createElement("button");
    star.className = "btn-like" + (entry.liked ? " liked" : "");
    star.title = "Like";
    star.setAttribute("hx-post", `/concerts/${entry.concertId}/tracks/${entry.trackIdx}/like`);
    star.setAttribute("hx-target", "this");
    star.setAttribute("hx-swap", "outerHTML");
    star.textContent = entry.liked ? "★" : "☆";

    li.append(playBtn, removeBtn, titleSpan, star);
    list.appendChild(li);
  }
  if (window.htmx) window.htmx.process(list);
  const queueSection = byIdOrNull("sidebar-queue-section");
  if (queueSection) queueSection.scrollTop = queueSection.scrollHeight;
}

function dequeue(pos: number): void {
  queue.splice(pos, 1);
  tracing("dequeue", { pos, queueLength: queue.length });
  queueChanged();
}

// Remove all remaining queue entries belonging to a playlist group in one action.
function removeGroup(groupId: number): void {
  queue = queue.filter((q) => q.groupId !== groupId);
  tracing("removeGroup", { groupId, queueLength: queue.length });
  queueChanged();
}

function playQueueEntryNow(pos: number): void {
  const entry = queue.splice(pos, 1)[0];
  if (!entry) return;
  tracing("playQueueEntryNow", { pos, concertId: entry.concertId, trackIdx: entry.trackIdx });
  queueChanged();
  startTrack(null, entry.concertId, entry.trackIdx);
}

// Fetch `/concerts/:id/tracks?context=sidebar` (or `&playback=concert` in
// concert reconstruction mode) and inject into the sidebar's concert-tracks
// section. A generation counter guards against races when the concert changes
// while a fetch is in flight.
async function loadSidebarTracks(concertId: number | null): Promise<void> {
  if (concertId == null) return;
  const gen = ++sidebarLoadGen;
  const section = byIdOrNull("sidebar-concert-tracks");
  const heading = byIdOrNull("sidebar-concert-heading");
  if (!section) return;

  if (heading) {
    heading.textContent = byIdOrNull("player-artist")?.textContent || "Concert tracks";
  }

  // In concert reconstruction mode, show the interleaved song+interlude list.
  const concertPlaybackMode = !!(state.concert && state.concert.id === concertId);

  try {
    const resp = await fetchSidebarTracks(concertId, { concertPlayback: concertPlaybackMode });
    if (gen !== sidebarLoadGen) return;
    if (!resp.ok) {
      sidebarConcertId = null;
      const err = document.createElement("p");
      err.className = "sidebar-load-error";
      err.textContent = "Couldn't load tracks";
      section.replaceChildren(err);
      return;
    }
    const html = await resp.text();
    if (gen !== sidebarLoadGen) return;
    section.innerHTML = html;
    if (window.htmx) window.htmx.process(section);
    reapplyPlaying();
    sidebarConcertId = concertId;
  } catch (e) {
    if (gen !== sidebarLoadGen) return;
    tracing("loadSidebarTracks failed", e);
    sidebarConcertId = null;
    const err = document.createElement("p");
    err.className = "sidebar-load-error";
    err.textContent = "Couldn't load tracks";
    section.replaceChildren(err);
  }
}

function isSidebarOpen(): boolean {
  return document.body.classList.contains("sidebar-open");
}

function closeSidebar(): void {
  if (!isSidebarOpen()) return;
  document.body.classList.remove("sidebar-open");
  const toggle = byIdOrNull("player-queue-toggle");
  if (toggle) toggle.setAttribute("aria-expanded", "false");
  tracing("closeSidebar", {});
}

function openSidebar(): void {
  if (isSidebarOpen()) return;
  document.body.classList.add("sidebar-open");
  const toggle = byIdOrNull("player-queue-toggle");
  if (toggle) toggle.setAttribute("aria-expanded", "true");
  renderQueue();
  loadSidebarTracks(state.concertId);
  tracing("openSidebar", {});
}

function toggleSidebar(): void {
  const open = document.body.classList.toggle("sidebar-open");
  const toggle = byIdOrNull("player-queue-toggle");
  if (toggle) toggle.setAttribute("aria-expanded", open ? "true" : "false");
  tracing("toggleSidebar", { open });
  if (open) {
    renderQueue();
    loadSidebarTracks(state.concertId);
  }
}

// Tear the player down completely: nothing is playing and there is nothing to
// advance to. Used after deleting the last remaining track.
function stopPlayback(): void {
  tracing("stopPlayback", {});
  cancelAutoAdvance();
  if (audio) {
    audio.pause();
    // Clearing via removeAttribute + load() avoids audio.src = "" which
    // resolves to the page URL and fires a spurious error -> auto-advance.
    audio.removeAttribute("src");
    audio.load();
  }
  clearPlaying();
  hideVideoPanel();
  closeSidebar();
  const concertTracks = byIdOrNull("sidebar-concert-tracks");
  if (concertTracks) concertTracks.replaceChildren();
  sidebarConcertId = null;
  queue = [];
  queueChanged();
  state.concertId = null;
  state.trackIdx = null;
  state.isVideo = false;
  state.watchUrl = null;
  state.hasNext = false;
  state.hasPrev = false;
  state.liked = false;
  state.concert = null;
  if (bar) bar.classList.remove("active");
  document.body.classList.remove("player-active");
  setPlayPauseIcon(false);
  updateLikeStar();
  updateDeleteButton();
  updateAddButton();
  updateMediaButtons(false);
  updateNextButton();
  updatePrevButton();
}

// POST the delete, swap the refreshed card HTML in (if on page), return true on success.
// Shared by the player-bar deleteTrack() and sidebar sidebarDeleteTrack().
async function postDeleteTrackRequest(concertId: number, trackIdx: number): Promise<boolean> {
  try {
    const resp = await postDeleteTrack(concertId, trackIdx);
    if (!resp.ok) {
      showError("Delete failed");
      return false;
    }
    const html = await resp.text();
    // The response is the concert's full card; swap it in if visible on page.
    // List visibility is pure CSS so no open/closed state needs preserving.
    const card = byIdOrNull("concert-" + concertId);
    if (card) {
      card.outerHTML = html;
      const fresh = byIdOrNull("concert-" + concertId);
      if (fresh && window.htmx) window.htmx.process(fresh);
    }
    return true;
  } catch (e) {
    showError("Delete failed");
    tracing("postDeleteTrack fetch failed", e);
    return false;
  }
}

// Advance playback after the currently-playing track has been deleted.
async function advanceAfterDelete(): Promise<void> {
  cancelAutoAdvance();
  if (audio) audio.pause();
  const played = await playFromQueue();
  if (!played) {
    const advanced = await playNextTrack();
    if (!advanced) stopPlayback();
  }
}

// Player-bar Delete button: delete the currently-playing track's files (no
// confirmation, matching the track-list button), refresh the concert's
// on-page card, then advance to the next track — or stop if nothing is next.
async function deleteTrack(): Promise<void> {
  if (state.trackIdx == null || state.concertId == null) return;
  const concertId = state.concertId;
  const trackIdx = state.trackIdx;
  tracing("deleteTrack", { concertId, trackIdx });

  if (!(await postDeleteTrackRequest(concertId, trackIdx))) return;

  // If playback moved on while the POST was in flight, do not disturb whatever
  // is playing now — just leave the refreshed list.
  if (state.concertId !== concertId || state.trackIdx !== trackIdx) {
    tracing("deleteTrack: playback moved on, not advancing", {});
    return;
  }

  await advanceAfterDelete();
}

// Sidebar trash button: delete a track from the sidebar track list.
// Unlike the htmx card-trash button, there is no .card ancestor, so this
// calls postDeleteTrackRequest directly, then re-fetches the sidebar to reflect
// new availability. Advances playback only when the deleted track was playing.
async function sidebarDeleteTrack(concertId: number, trackIdx: number): Promise<void> {
  tracing("sidebarDeleteTrack", { concertId, trackIdx });
  const btn = document.querySelector<HTMLButtonElement>(
    `#sidebar-concert-tracks .btn-delete[onclick*="sidebarDeleteTrack(${concertId}, ${trackIdx})"]`,
  );
  if (btn) btn.disabled = true;

  const success = await postDeleteTrackRequest(concertId, trackIdx);

  // If in concert reconstruction mode for this concert, refresh the items
  // array so advanceConcert navigates to the correct next item.
  if (success && state.concert && state.concert.id === concertId) {
    await refreshConcertItems(concertId);
  }

  // Sidebar bypasses htmx card events, so refresh it explicitly.
  if (isSidebarOpen()) await loadSidebarTracks(concertId);

  if (!success) return;

  if (state.concertId === concertId && state.trackIdx === trackIdx) {
    // In concert reconstruction mode the items list was already refreshed above;
    // pos now points at the next item (or past the end). Play it directly rather
    // than using advanceAfterDelete() which is not concert-aware.
    if (state.concert && state.concert.id === concertId) {
      await playConcertPosOrEnd();
    } else {
      await advanceAfterDelete();
    }
  }
}

// ── Concert reconstruction playback ─────────────────────────────────────────

// After a delete+refresh where the currently-playing item was removed:
// pos still points at the same index (refreshConcertItems found no URL match
// and left it unchanged), which is now the "next" item in the refreshed list.
// Play it, or end the concert if nothing remains at that position.
async function playConcertPosOrEnd(): Promise<void> {
  const concert = state.concert;
  if (!concert) return;
  if (concert.pos < concert.items.length) {
    await playConcertItem(concert.pos);
  } else {
    state.concert = null;
    hideVideoPanel();
  }
}

// Re-fetch the items array for the current concert-playback state and update
// state.concert.items + pos so advanceConcert navigates to the right item.
async function refreshConcertItems(concertId: number): Promise<void> {
  if (!state.concert || state.concert.id !== concertId) return;
  try {
    const data: ConcertPlaybackResponse = await getConcertPlayback(concertId);
    if (isSourcePlayback(data)) return;
    if (!state.concert || state.concert.id !== concertId) return; // raced
    const currentItem = state.concert.items[state.concert.pos];
    const currentUrl = currentItem && currentItem.url;
    state.concert.items = data.items;
    // Re-find pos by URL so navigation stays correct after items shift.
    if (currentUrl) {
      const newPos = data.items.findIndex((item) => item.url === currentUrl);
      if (newPos >= 0) state.concert.pos = newPos;
    }
    updateNextButton();
    updatePrevButton();
  } catch (e) {
    tracing("refreshConcertItems failed", e);
  }
}

// Play a single item within the concert reconstruction sequence (by position).
// Saves and restores state.concert because play() clears it.
async function playConcertItem(pos: number): Promise<void> {
  if (!state.concert) return;
  const concert = state.concert;
  const item = concert.items[pos];
  if (!item) return;

  const isInterlude = item.kind === "interlude";
  const trackIdx = isInterlude ? null : (item.track_index ?? null);
  const listenUrl = isInterlude ? null : `/concerts/${concert.id}/tracks/${trackIdx}/listen`;
  const hasPrevItem = pos > 0;
  const hasNextItem = pos + 1 < concert.items.length;

  // play() will clear state.concert; save it so we can restore it after.
  const savedConcert: ConcertPlaybackState = { id: concert.id, items: concert.items, pos };

  await play(
    null,
    item.url,
    item.title,
    item.artist || "",
    concert.id,
    trackIdx,
    listenUrl,
    item.is_video,
    null,
    hasNextItem,
    item.liked || false,
    hasPrevItem,
  );

  // Restore concert mode and fix interlude highlighting (play() marks via
  // findTrackButtons which falls back to album buttons for trackIdx=null).
  state.concert = savedConcert;
  if (isInterlude) {
    clearPlaying();
    if (item.interlude_index != null) markPlayingInterlude(concert.id, item.interlude_index);
  }
  updateNextButton();
  updatePrevButton();

  // Refresh sidebar in concert mode so it shows interludes.
  if (isSidebarOpen()) {
    await loadSidebarTracks(concert.id);
  }
}

// Advance to the next concert item, or clear concert mode + hide video at end.
async function advanceConcert(): Promise<void> {
  const concert = state.concert;
  if (!concert) return;
  const next = concert.pos + 1;
  if (next >= concert.items.length) {
    state.concert = null;
    hideVideoPanel();
    return;
  }
  concert.pos = next;
  await playConcertItem(next);
}

// Fetch /concerts/:id/concert-playback and start playback:
// - source present → whole-album play (existing path, no concert state)
// - reconstruction → set state.concert and start from item 0
async function playConcert(id: number): Promise<void> {
  cancelAutoAdvance();
  try {
    const data = await getConcertPlayback(id);

    if (isSourcePlayback(data)) {
      // Source file present: whole-album play via existing startAlbum path.
      // state.concert will be null (play() clears it), which is correct here.
      const info = data.source;
      if (!info.playable) {
        window.open(info.url, "_blank");
        return;
      }
      await play(
        null,
        info.url,
        info.title,
        info.artist,
        id,
        null,
        `/concerts/${id}/listen`,
        info.is_video,
        `/concerts/${id}/watch`,
        info.has_next,
        info.liked,
        info.has_prev,
      );
      return;
    }

    if (data.items && data.items.length > 0) {
      // Set up concert state before playConcertItem (which saves + restores it).
      state.concert = { id, items: data.items, pos: 0 };
      await playConcertItem(0);
      return;
    }

    showError("Nothing to play");
  } catch (e) {
    showError("Couldn't start concert");
    tracing("playConcert failed", e);
  }
}

// Jump to a specific position in the reconstruction sidebar (called by
// onclick in concert_playback_tracks.html).
async function playConcertFrom(id: number, pos: number): Promise<void> {
  if (!state.concert || state.concert.id !== id) {
    // Not in concert mode for this concert: start fresh.
    try {
      const data = await getConcertPlayback(id);
      if (isSourcePlayback(data) || !data.items || data.items.length === 0) {
        showError("Nothing to play");
        return;
      }
      state.concert = { id, items: data.items, pos };
    } catch (e) {
      showError("Couldn't load concert");
      tracing("playConcertFrom fetch failed", e);
      return;
    }
  } else {
    state.concert.pos = pos;
  }
  await playConcertItem(pos);
}

// Delete an interlude file from the reconstruction sidebar, then re-sync.
async function sidebarDeleteInterlude(concertId: number, interludeIdx: number): Promise<void> {
  tracing("sidebarDeleteInterlude", { concertId, interludeIdx });
  const btn = document.querySelector<HTMLButtonElement>(
    `#sidebar-concert-tracks .btn-delete[onclick*="sidebarDeleteInterlude(${concertId}, ${interludeIdx})"]`,
  );
  if (btn) btn.disabled = true;

  let wasPlayingThis = false;
  if (state.concert && state.concert.id === concertId) {
    const cur = state.concert.items[state.concert.pos];
    wasPlayingThis = !!(cur && cur.kind === "interlude" && cur.interlude_index === interludeIdx);
  }

  try {
    const resp = await postDeleteInterlude(concertId, interludeIdx);
    if (!resp.ok) {
      showError("Delete failed");
      if (btn) btn.disabled = false;
      return;
    }
  } catch (e) {
    showError("Delete failed");
    tracing("sidebarDeleteInterlude fetch failed", e);
    if (btn) btn.disabled = false;
    return;
  }

  // Refresh items so navigation stays consistent.
  if (state.concert && state.concert.id === concertId) {
    await refreshConcertItems(concertId);
  }

  // Refresh sidebar HTML.
  if (isSidebarOpen()) await loadSidebarTracks(concertId);

  // If this interlude was playing, play the next item (pos now points at it
  // after the refresh — advancing with advanceConcert() would skip it).
  if (wasPlayingThis) {
    await playConcertPosOrEnd();
  }
}

// ── Bootstrap ────────────────────────────────────────────────────────────────

if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", init);
} else {
  init();
}

const api: PlayerApi = {
  playAlbum,
  playTrack,
  playTracks,
  startAlbum,
  startTrack,
  togglePause,
  seek,
  skipToNext,
  skipToPrev,
  watch,
  openExternal,
  watchTrackDirect,
  toggleLike,
  deleteTrack,
  openConcert,
  openSidebar,
  closeSidebar,
  toggleSidebar,
  sidebarDeleteTrack,
  playQueueEntryNow,
  dequeue,
  enqueue,
  playAlbumAt,
  nowPlaying,
  playPlaylist,
  addToPlaylist,
  stopPlayback,
  playConcert,
  playConcertFrom,
  sidebarDeleteInterlude,
};

window.Player = api;
