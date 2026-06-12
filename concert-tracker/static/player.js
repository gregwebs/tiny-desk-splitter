"use strict";

const Player = (() => {
  let audio = null;
  let bar = null;
  let state = { concertId: null, trackIdx: null, isVideo: false, watchUrl: null, hasNext: false, hasPrev: false, liked: false };
  let queue = [];
  let autoAdvanceController = null;
  let keyboardShortcutsBound = false;
  let sidebarLoadGen = 0;
  let sidebarConcertId = null;

  function onPlay() { setPlayPauseIcon(true); }
  function onPause() { setPlayPauseIcon(false); }

  function bindAudioEvents() {
    audio.addEventListener("timeupdate", onTimeUpdate);
    audio.addEventListener("loadedmetadata", onTimeUpdate);
    audio.addEventListener("ended", onEnded);
    audio.addEventListener("error", onError);
    audio.addEventListener("play", onPlay);
    audio.addEventListener("pause", onPause);
  }

  function init() {
    audio = document.getElementById("player-audio");
    bar = document.getElementById("player-bar");
    if (!audio || !bar) return;

    bindAudioEvents();
    bindKeyboardShortcuts();

    // Reveal the video minimize button on pointer activity over the panel (touchstart
    // too, since touch devices fire no mousemove).
    const videoPanel = document.getElementById("player-video-panel");
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
    document.body.addEventListener("htmx:afterSwap", syncLikeFromTrackList);
  }

  function bindKeyboardShortcuts() {
    if (keyboardShortcutsBound) return;
    document.addEventListener("keydown", onGlobalKeydown);
    keyboardShortcutsBound = true;
  }

  function isPlainSpaceKey(e) {
    return (
      (e.code === "Space" || e.key === " " || e.key === "Spacebar") &&
      !e.ctrlKey &&
      !e.metaKey &&
      !e.altKey &&
      !e.shiftKey
    );
  }

  function isPlainEscapeKey(e) {
    return (
      (e.code === "Escape" || e.key === "Escape" || e.key === "Esc") &&
      !e.ctrlKey &&
      !e.metaKey &&
      !e.altKey &&
      !e.shiftKey
    );
  }

  function isPlayerPlaybackShortcutTarget(target) {
    return target && (target === audio || target.id === "player-watch");
  }

  // True for text-entry targets where native key behavior (typing a space,
  // clearing/blurring on Escape) must win over the global player shortcuts.
  function isEditableTarget(target) {
    if (!target) return false;
    if (target.isContentEditable) return true;
    if (target.matches && target.matches("input, textarea, select")) return true;
    if (!target.closest) return false;
    const editable = target.closest("[contenteditable]");
    return !!(editable && editable.isContentEditable);
  }

  function isKeyboardShortcutIgnoredTarget(target) {
    if (!target) return false;
    if (isPlayerPlaybackShortcutTarget(target)) return false;
    if (isEditableTarget(target)) return true;
    if (!target.closest) return false;

    return !!target.closest(INTERACTIVE_SELECTOR);
  }

  function hasActiveMedia() {
    return audio && !!(audio.currentSrc || audio.getAttribute("src"));
  }

  function isMediaPlaying() {
    return hasActiveMedia() && !audio.paused && !audio.ended;
  }

  function onGlobalKeydown(e) {
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

    tracing(audio.paused ? "spacebar play" : "spacebar pause", {});
    togglePause();
  }

  // All .btn-like elements for a given concert track (card list, sidebar, etc.).
  // The hx-post URL is the stable per-track key present on every copy.
  function likeButtonsFor(concertId, trackIdx) {
    return document.querySelectorAll(`.btn-like[hx-post="/concerts/${concertId}/tracks/${trackIdx}/like"]`);
  }

  // Sync liked state to all copies of the track's star buttons, player star,
  // and any matching queue entries.
  function applyLike(concertId, trackIdx, liked) {
    likeButtonsFor(concertId, trackIdx).forEach(lb => {
      lb.classList.toggle("liked", liked);
      lb.textContent = liked ? "★" : "☆";
    });
    let queueDirty = false;
    queue.forEach(e => {
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

  // After an htmx:afterSwap (like-button outerHTML swap or whole-card swap),
  // propagate the new like state to all copies of that track's star. Reads the
  // live DOM rather than parsing the possibly-detached swapped-out element.
  function syncLikeFromTrackList(evt) {
    const target = evt && evt.detail && evt.detail.target;
    // When a concert card is swapped (track deleted via htmx), refresh the sidebar
    // so availability counts and greyed rows stay in sync.
    if (target && target.id === `concert-${state.concertId}` && isSidebarOpen() && state.concertId != null) {
      loadSidebarTracks(state.concertId);
    }
    let concertId = null;
    let trackIdx = null;
    if (target && target.getAttribute) {
      const hxPost = target.getAttribute("hx-post");
      const m = hxPost && hxPost.match(/\/concerts\/(\d+)\/tracks\/(\d+)\/like/);
      if (m) {
        concertId = parseInt(m[1], 10);
        trackIdx = parseInt(m[2], 10);
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
  function reassertPlayerUi() {
    reapplyPlaying();
    updateLikeStar();
    updateDeleteButton();
    // A card swap replaces the buttons of a concert whose prepare chain is in
    // flight; re-apply the pending mark and the disabled state (the server
    // also renders them disabled once its job state catches up).
    if (pendingPlay) {
      disableCardTracks(pendingPlay.concertId);
      markPreparing();
    }
  }

  function setPlayPauseIcon(playing) {
    const btn = document.getElementById("player-play-pause");
    if (btn) btn.textContent = playing ? "⏸" : "▶";
  }

  function showBar() {
    bar.classList.add("active");
    document.body.classList.add("player-active");
  }

  function hideError() {
    const el = document.getElementById("player-error");
    if (el) el.style.display = "none";
  }

  function showError(msg) {
    const el = document.getElementById("player-error");
    if (el) {
      el.textContent = msg;
      el.style.display = "inline";
    }
  }

  function updateInfo(title, artist, trackIdx, concertId) {
    const t = document.getElementById("player-title");
    const a = document.getElementById("player-artist");
    const n = document.getElementById("player-track");
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

  function onTimeUpdate() {
    const seek = document.getElementById("player-seek");
    const time = document.getElementById("player-time");
    if (!audio.duration) return;
    if (seek) seek.value = (audio.currentTime / audio.duration) * 100;
    if (time) time.textContent = formatTime(audio.currentTime) + " / " + formatTime(audio.duration);
  }

  // Play the next queued track, else auto-advance to the following track, else
  // we have reached the end of everything: collapse the inline video panel so
  // its frozen last frame doesn't cover the page and block selecting another
  // track. Shared by the natural end-of-track and load-error dead ends.
  async function advanceOrCollapse() {
    if (await playFromQueue()) return;
    if (await playNextTrack()) return;
    hideVideoPanel();
  }

  async function onEnded() {
    await advanceOrCollapse();
  }

  async function onError() {
    showError("Failed to load media");
    tracing("audio error", audio.error);
    await advanceOrCollapse();
  }

  function cancelAutoAdvance() {
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

  let pendingPlay = null; // { concertId, trackIdx, timer, deadline }
  const PREPARE_POLL_MS = 2000;
  // Downloads can take many minutes; give the whole chain a generous cap so
  // an abandoned poll loop can't run forever.
  const PREPARE_TIMEOUT_MS = 30 * 60 * 1000;

  function setStatus(msg) {
    const el = document.getElementById("player-status");
    if (!el) return;
    el.textContent = msg || "";
    el.style.display = msg ? "inline" : "none";
  }

  // Best-effort visual mark on the pending track's button; re-applied by
  // reassertPlayerUi after card swaps replace the button element.
  function markPreparing() {
    if (!pendingPlay) return;
    findTrackButtons(pendingPlay.concertId, pendingPlay.trackIdx)
      .forEach(btn => btn.classList.add("preparing"));
  }

  function clearPreparing() {
    document.querySelectorAll(".btn-track-listen.preparing").forEach(function (b) {
      b.classList.remove("preparing");
    });
  }

  // Disable the card's tracks button and track buttons immediately on click;
  // subsequent card swaps render them disabled server-side (tracks_busy).
  function disableCardTracks(concertId) {
    const card = document.getElementById("concert-" + concertId);
    if (!card) return;
    card.querySelectorAll(".btn-tracks, .btn-track-listen").forEach(function (b) {
      b.disabled = true;
    });
  }

  function cancelPendingPlay() {
    if (!pendingPlay) return;
    if (pendingPlay.timer) clearTimeout(pendingPlay.timer);
    pendingPlay = null;
    clearPreparing();
    setStatus("");
  }

  function failPendingPlay(msg) {
    tracing("preparePlay failed", { msg });
    cancelPendingPlay();
    showError(msg);
  }

  async function preparePlay(btn, concertId, trackIdx) {
    if (!audio) init();
    cancelPendingPlay();
    let resp;
    try {
      resp = await fetch(`/concerts/${concertId}/prepare`, { method: "POST" });
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
    const card = document.getElementById("concert-" + concertId);
    if (card && window.htmx) {
      htmx.ajax("GET", `/concerts/${concertId}/status`, {
        target: `#concert-${concertId}`,
        swap: "outerHTML",
      });
    }
    // POST /prepare returns the same JSON as prepare-status, so seed the
    // first status from it instead of waiting a full poll interval.
    const status = await resp.json().catch(() => null);
    if (status) {
      await applyPrepareStatus(status);
    } else if (pendingPlay) {
      pendingPlay.timer = setTimeout(pollPrepare, PREPARE_POLL_MS);
    }
  }

  // Act on one prepare-status payload: play when the pending track's file
  // exists, stop on job error or timeout, otherwise show progress and re-arm
  // the poll timer.
  async function applyPrepareStatus(s) {
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

  async function pollPrepare() {
    if (!pendingPlay) return;
    const { concertId } = pendingPlay;
    let s;
    try {
      const resp = await fetch(`/concerts/${concertId}/prepare-status`);
      if (!resp.ok) throw new Error("status " + resp.status);
      s = await resp.json();
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
  async function trackMediaInfo(concertId, trackIdx) {
    try {
      const resp = await fetch(`/concerts/${concertId}/tracks/${trackIdx}/media-info`);
      if (!resp.ok) return null;
      const info = await resp.json();
      return { title: info.title, liked: !!info.liked };
    } catch (e) {
      tracing("trackMediaInfo fetch failed", e);
      return null;
    }
  }

  // Returns true when a following track started playing, false otherwise (no
  // next track, fetch error, or aborted). Callers decide what to do when false;
  // this never stops playback itself.
  async function playNextTrack() {
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
      const resp = await fetch(
        `/concerts/${concertId}/tracks/${trackIdx}/next-media-info`,
        { signal }
      );
      if (!resp.ok) {
        setPlayPauseIcon(false);
        return false;
      }
      if (signal.aborted) return false;
      const info = await resp.json();

      const btn = findTrackButton(concertId, info.track_index);
      await play(btn, info.url, info.title, info.artist, concertId, info.track_index,
        `/concerts/${concertId}/tracks/${info.track_index}/listen`, info.is_video,
        `/concerts/${concertId}/tracks/${info.track_index}/watch`, info.has_next, info.liked, info.has_prev);
      return true;
    } catch (e) {
      if (e.name !== "AbortError") {
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
  async function playPrevTrack() {
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
      const resp = await fetch(
        `/concerts/${concertId}/tracks/${trackIdx}/prev-media-info`,
        { signal }
      );
      if (!resp.ok) {
        setPlayPauseIcon(false);
        return false;
      }
      if (signal.aborted) return false;
      const info = await resp.json();

      const btn = findTrackButton(concertId, info.track_index);
      await play(btn, info.url, info.title, info.artist, concertId, info.track_index,
        `/concerts/${concertId}/tracks/${info.track_index}/listen`, info.is_video,
        `/concerts/${concertId}/tracks/${info.track_index}/watch`, info.has_next, info.liked, info.has_prev);
      return true;
    } catch (e) {
      if (e.name !== "AbortError") {
        showError("Couldn't load previous track");
        tracing("playPrevTrack failed", e);
        setPlayPauseIcon(false);
      }
      return false;
    }
  }

  function tracing(label, obj) {
    if (obj) console.warn("[Player]", label, obj);
  }

  function formatTime(seconds) {
    const m = Math.floor(seconds / 60);
    const s = Math.floor(seconds % 60);
    return m + ":" + (s < 10 ? "0" : "") + s;
  }

  function findTrackButtons(concertId, trackIdx) {
    if (trackIdx != null) {
      return document.querySelectorAll(`[data-concert-id="${concertId}"][data-track-idx="${trackIdx}"]`);
    }
    return document.querySelectorAll(`[data-concert-id="${concertId}"][data-role="listen-album"]`);
  }

  function findTrackButton(concertId, trackIdx) {
    return findTrackButtons(concertId, trackIdx)[0] || null;
  }

  function clearPlaying() {
    document.querySelectorAll(".btn-track-listen.playing, .btn-listen.playing")
      .forEach(b => b.classList.remove("playing"));
  }

  function markPlaying(concertId, trackIdx) {
    clearPlaying();
    findTrackButtons(concertId, trackIdx).forEach(b => b.classList.add("playing"));
  }

  function reapplyPlaying() {
    if (state.concertId == null) return;
    if (!audio.paused) markPlaying(state.concertId, state.trackIdx);
  }

  // The Watch (toggle inline video) and Open (launch system player) buttons only
  // make sense for video tracks; hide both for audio-only playback.
  function updateMediaButtons(isVideo) {
    const display = isVideo ? "inline-block" : "none";
    const watch = document.getElementById("player-watch");
    const open = document.getElementById("player-open");
    if (watch) watch.style.display = display;
    if (open) open.style.display = display;
  }

  // How long the minimize button stays visible after the last mouse movement.
  const VIDEO_CONTROLS_IDLE_MS = 2500;
  let videoControlsTimer = null;

  // Reveal the minimize button on mouse movement (or a touch) while watching, then
  // fade it back out once the pointer goes idle.
  function isVideoPanelOpen() {
    const panel = document.getElementById("player-video-panel");
    return !!panel && panel.classList.contains("open");
  }

  function showVideoControls() {
    const panel = document.getElementById("player-video-panel");
    if (!panel || !panel.classList.contains("open")) return;
    panel.classList.add("controls-visible");
    clearTimeout(videoControlsTimer);
    videoControlsTimer = setTimeout(
      () => panel.classList.remove("controls-visible"), VIDEO_CONTROLS_IDLE_MS);
  }

  // A click on an interactive element is the user trying to *do* that thing (navigate,
  // play, queue, like, …), not dismiss the video — so those clicks perform their action
  // and leave the panel open. Only clicks on "dead space" fold the video.
  // Recognizes native controls and inline onclick handlers (the project's convention); a
  // future control bound only via addEventListener would need adding here to be exempted.
  const INTERACTIVE_SELECTOR =
    'a, button, input, select, textarea, label, [role="button"], [onclick]';

  // Pure: does a click on `target` fall on dead space outside the player, and so
  // dismiss the video? (false for clicks inside the player or on any interactive control)
  function clickShouldDismiss(target, container) {
    if (!container || !target || container.contains(target)) return false;
    if (target.closest && target.closest(INTERACTIVE_SELECTOR)) return false;
    return true;
  }

  // While the video panel is open, a click on the empty page area above it folds it
  // back down, like clicking Watch.
  function onOutsideVideoClick(e) {
    const container = document.getElementById("player-container");
    if (!clickShouldDismiss(e.target, container)) return;
    tracing("outsideClick dismiss video", { tag: e.target && e.target.tagName });
    hideVideoPanel();
  }

  function showVideoPanel() {
    const panel = document.getElementById("player-video-panel");
    if (!panel || panel.classList.contains("open")) return;
    tracing("showVideoPanel", {});
    panel.classList.add("open");
    // Attached synchronously: the click that opened the panel always comes
    // from a button (#player-watch or a track-list Watch button), which
    // clickShouldDismiss already exempts as an interactive control — so the
    // opening click can't bubble up and immediately re-close the panel.
    document.addEventListener("click", onOutsideVideoClick);
  }

  function hideVideoPanel() {
    const panel = document.getElementById("player-video-panel");
    if (!panel || !panel.classList.contains("open")) return;
    tracing("hideVideoPanel", {});
    panel.classList.remove("open");
    panel.classList.remove("controls-visible");
    clearTimeout(videoControlsTimer);
    document.removeEventListener("click", onOutsideVideoClick);
  }

  // Player-bar Watch button: fold the inline video panel up or down. The video
  // is the already-playing #player-audio element, so revealing it needs no resync.
  function watch() {
    if (isVideoPanelOpen()) hideVideoPanel();
    else showVideoPanel();
  }

  // Show the like star only while an individual track is playing (whole-album
  // playback has no per-track like), and reflect the current liked state.
  function updateLikeStar() {
    const star = document.getElementById("player-like");
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
  function updateDeleteButton() {
    const btn = document.getElementById("player-delete");
    if (!btn) return;
    btn.style.display = state.trackIdx == null || state.liked ? "none" : "inline-block";
  }

  function setLikeState(liked) {
    applyLike(state.concertId, state.trackIdx, liked);
  }

  // Player-bar like star: toggle the like for the currently-playing track.
  // Optimistically flips the UI, POSTs to the same /like endpoint the track-list
  // star uses, and reverts on failure. The HTML body is ignored — the in-place
  // button update already reflects the new state.
  async function toggleLike() {
    if (state.trackIdx == null) return;
    const concertId = state.concertId;
    const trackIdx = state.trackIdx;
    const next = !state.liked;
    setLikeState(next);
    try {
      const resp = await fetch(`/concerts/${concertId}/tracks/${trackIdx}/like`, { method: "POST" });
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

  // There is "something next" when the queue is non-empty or the current track
  // has a following track to auto-advance to. Disable the Next button otherwise
  // so clicking it cannot stop the current track with nothing to replace it.
  function updateNextButton() {
    const btn = document.getElementById("player-next");
    if (!btn) return;
    btn.disabled = queue.length === 0 && !state.hasNext;
  }

  // Disable the Back button when there is no earlier playable track to go to
  // (the first track, or whole-album playback which has no per-track nav).
  function updatePrevButton() {
    const btn = document.getElementById("player-prev");
    if (!btn) return;
    btn.disabled = !state.hasPrev;
  }

  async function play(btn, url, title, artist, concertId, trackIdx, listenUrl, isVideo, watchUrl, hasNext, liked, hasPrev) {
    if (!audio) init();
    if (!audio) return;

    hideError();
    setStatus("");
    showBar();
    updateInfo(title, artist, trackIdx, concertId);
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

    if (listenUrl) {
      fetch(listenUrl, { method: "POST" }).catch(() => {});
    }
  }

  // Fetch the whole-album media info and start playing it now (no enqueue).
  // Returns true when in-browser playback started; false on error or when the
  // file is not browser-playable (in which case it falls back to window.open).
  async function startAlbum(btn, concertId) {
    cancelAutoAdvance();
    try {
      const resp = await fetch(`/concerts/${concertId}/media-info`);
      if (!resp.ok) {
        if (btn) { btn.classList.add("btn-listen-error"); btn.textContent = "Error"; }
        return false;
      }
      const info = await resp.json();
      if (!info.playable) {
        window.open(info.url, "_blank");
        return false;
      }
      await play(btn, info.url, info.title, info.artist, concertId, null,
        `/concerts/${concertId}/listen`, info.is_video,
        `/concerts/${concertId}/watch`, info.has_next, info.liked, info.has_prev);
      return true;
    } catch (e) {
      if (btn) { btn.classList.add("btn-listen-error"); btn.textContent = "Error"; }
      tracing("startAlbum fetch failed", e);
      return false;
    }
  }

  async function playAlbum(btn, concertId) {
    await startAlbum(btn, concertId);
  }

  // Fetch a track's media info and start playing it now (no enqueue, no
  // toggle-pause). Returns true when in-browser playback started; false on
  // error or non-playable file (falls back to window.open).
  async function startTrack(btn, concertId, trackIdx) {
    cancelAutoAdvance();
    try {
      const resp = await fetch(`/concerts/${concertId}/tracks/${trackIdx}/media-info`);
      if (!resp.ok) {
        // Track file missing (not split yet, or deleted): enter the prepare
        // flow — download/split as needed and auto-play when it appears.
        await preparePlay(btn, concertId, trackIdx);
        return false;
      }
      const info = await resp.json();
      if (!info.playable) {
        window.open(info.url, "_blank");
        return false;
      }
      await play(btn, info.url, info.title, info.artist, concertId, trackIdx,
        `/concerts/${concertId}/tracks/${trackIdx}/listen`, info.is_video,
        `/concerts/${concertId}/tracks/${trackIdx}/watch`, info.has_next, info.liked, info.has_prev);
      return true;
    } catch (e) {
      if (btn) { btn.classList.add("btn-listen-error"); btn.textContent = "Error"; }
      tracing("startTrack fetch failed", e);
      return false;
    }
  }

  async function playTrack(btn, concertId, trackIdx) {
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
      enqueue(concertId, trackIdx, info.title || (btn ? btn.textContent.trim() : ""), info.liked);
      return;
    }
    await startTrack(btn, concertId, trackIdx);
  }

  // Resolve the first playable track index for a concert: track 0 normally, or
  // the next playable track after it when track 0 has been deleted/removed.
  // Reuses the server's next-media-info skip-deleted logic (the same one that
  // drives auto-advance) so the two stay in sync. Returns null when the concert
  // has no playable track.
  async function firstAvailableTrackIndex(concertId) {
    try {
      // In the common case (track 0 present) startTrack re-fetches this same
      // media-info; the extra GET on a single button click is intentional — not
      // worth threading a prefetched body through the shared startTrack path.
      const head = await fetch(`/concerts/${concertId}/tracks/0/media-info`);
      if (head.ok) return 0;
      const next = await fetch(`/concerts/${concertId}/tracks/0/next-media-info`);
      if (next.ok) return (await next.json()).track_index;
    } catch (e) {
      tracing("firstAvailableTrackIndex failed", e);
    }
    return null;
  }

  // Tracks button on the card: play the split tracks starting from the first
  // one that still exists (track 0 may have been deleted). When no track is
  // playable at all (not split yet, or everything deleted), enter the prepare
  // flow via track 0 — it downloads/splits and auto-plays when ready.
  async function playTracks(btn, concertId) {
    const trackIdx = await firstAvailableTrackIndex(concertId);
    if (trackIdx == null) {
      tracing("playTracks: no playable track, preparing", { concertId });
      await playTrack(btn, concertId, 0);
      return;
    }
    await playTrack(btn, concertId, trackIdx);
  }

  function togglePause() {
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

  function seek(val) {
    if (!audio || !audio.duration) return;
    audio.currentTime = (val / 100) * audio.duration;
  }

  function enqueue(concertId, trackIdx, title, liked) {
    if (queue.some(q => q.concertId === concertId && q.trackIdx === trackIdx)) {
      tracing("enqueue duplicate skipped", { concertId, trackIdx });
      return;
    }
    queue.push({ concertId, trackIdx, title, liked: !!liked });
    tracing("enqueue", { concertId, trackIdx, title, queueLength: queue.length });
    queueChanged();
  }

  async function playFromQueue() {
    while (queue.length > 0) {
      const entry = queue.shift();
      queueChanged();
      cancelAutoAdvance();
      tracing("playFromQueue", { concertId: entry.concertId, trackIdx: entry.trackIdx });

      try {
        const resp = await fetch(
          `/concerts/${entry.concertId}/tracks/${entry.trackIdx}/media-info`
        );
        if (!resp.ok) {
          tracing("playFromQueue track unavailable", { concertId: entry.concertId, trackIdx: entry.trackIdx });
          continue;
        }
        const info = await resp.json();
        if (!info.playable) continue;

        const btn = findTrackButton(entry.concertId, entry.trackIdx);
        await play(btn, info.url, info.title, info.artist, entry.concertId, entry.trackIdx,
          `/concerts/${entry.concertId}/tracks/${entry.trackIdx}/listen`, info.is_video,
          `/concerts/${entry.concertId}/tracks/${entry.trackIdx}/watch`, info.has_next, info.liked, info.has_prev);
        return true;
      } catch (e) {
        tracing("playFromQueue failed", e);
      }
    }
    return false;
  }

  async function skipToNext() {
    if (!audio) return;
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
  async function skipToPrev() {
    if (!audio) return;
    if (!state.hasPrev) {
      tracing("skipToPrev ignored: nothing previous", {});
      return;
    }
    tracing("skipToPrev", {});
    cancelAutoAdvance();
    audio.pause();
    await playPrevTrack();
  }

  function updateQueueBadge() {
    const badge = document.getElementById("player-queue-badge");
    if (!badge) return;
    if (queue.length > 0) {
      badge.textContent = queue.length;
      badge.style.visibility = "visible";
      badge.title = queue.map(q => q.title).join("\n");
    } else {
      badge.textContent = "";
      badge.style.visibility = "hidden";
      badge.title = "";
    }
  }

  // Link-out: launch the current file in the system player via the server's
  // `open`. state.watchUrl is the POST endpoint set by play(). This is the only
  // path that still records a server-side Watch event.
  async function openExternal() {
    if (!state.watchUrl) return;
    tracing("openExternal", { watchUrl: state.watchUrl });
    // Handing off to the system player: stop our playback so audio doesn't play
    // in both places at once.
    if (audio) audio.pause();
    try {
      await fetch(state.watchUrl, { method: "POST" });
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
  function openConcert(e) {
    if (e && (e.metaKey || e.ctrlKey || e.shiftKey)) return;
    if (e) e.preventDefault();
    if (state.concertId == null || !window.htmx) {
      tracing("openConcert skipped", { concertId: state.concertId, htmx: !!window.htmx });
      return;
    }
    window.htmx.ajax("GET", `/concerts/${state.concertId}`, { source: e.currentTarget });
  }

  // Track-list/detail Watch button: start this track playing inline and fold up
  // the video panel.
  async function watchTrackDirect(btn, concertId, trackIdx) {
    if (await startTrack(btn, concertId, trackIdx)) showVideoPanel();
  }

  function queueChanged() {
    renderQueue();
    updateQueueBadge();
    updateNextButton();
  }

  // Render the queue section of the sidebar using DOM APIs (textContent only —
  // titles are untrusted data and must never be set via innerHTML).
  function renderQueue() {
    const list = document.getElementById("sidebar-queue-list");
    const empty = document.getElementById("sidebar-queue-empty");
    if (!list) return;
    list.replaceChildren();
    if (queue.length === 0) {
      if (empty) empty.style.display = "";
      return;
    }
    if (empty) empty.style.display = "none";
    queue.forEach((entry, i) => {
      const li = document.createElement("li");
      li.className = "queue-item";

      const star = document.createElement("button");
      star.className = "btn-like" + (entry.liked ? " liked" : "");
      star.title = "Like";
      star.setAttribute("hx-post", `/concerts/${entry.concertId}/tracks/${entry.trackIdx}/like`);
      star.setAttribute("hx-target", "this");
      star.setAttribute("hx-swap", "outerHTML");
      star.textContent = entry.liked ? "★" : "☆";

      const titleSpan = document.createElement("span");
      titleSpan.className = "queue-title";
      titleSpan.textContent = entry.title;

      const playBtn = document.createElement("button");
      playBtn.className = "btn-queue-play";
      playBtn.title = "Play now";
      playBtn.textContent = "▶";
      playBtn.onclick = () => playQueueEntryNow(i);

      const removeBtn = document.createElement("button");
      removeBtn.className = "btn-queue-remove";
      removeBtn.title = "Remove from queue";
      removeBtn.textContent = "✕";
      removeBtn.onclick = () => dequeue(i);

      li.append(star, titleSpan, playBtn, removeBtn);
      list.appendChild(li);
    });
    if (window.htmx) window.htmx.process(list);
  }

  function dequeue(pos) {
    queue.splice(pos, 1);
    tracing("dequeue", { pos, queueLength: queue.length });
    queueChanged();
  }

  function playQueueEntryNow(pos) {
    const entry = queue.splice(pos, 1)[0];
    if (!entry) return;
    tracing("playQueueEntryNow", { pos, concertId: entry.concertId, trackIdx: entry.trackIdx });
    queueChanged();
    startTrack(null, entry.concertId, entry.trackIdx);
  }

  // Fetch `/concerts/:id/tracks?context=sidebar` and inject into the sidebar's
  // concert-tracks section. A generation counter guards against races when the
  // concert changes while a fetch is in flight.
  async function loadSidebarTracks(concertId) {
    if (concertId == null) return;
    const gen = ++sidebarLoadGen;
    const section = document.getElementById("sidebar-concert-tracks");
    const heading = document.getElementById("sidebar-concert-heading");
    if (!section) return;

    if (heading) {
      heading.textContent = document.getElementById("player-artist")?.textContent || "Concert tracks";
    }

    try {
      const resp = await fetch(`/concerts/${concertId}/tracks?context=sidebar`);
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

  function isSidebarOpen() {
    return document.body.classList.contains("sidebar-open");
  }

  function closeSidebar() {
    if (!isSidebarOpen()) return;
    document.body.classList.remove("sidebar-open");
    const toggle = document.getElementById("player-queue-toggle");
    if (toggle) toggle.setAttribute("aria-expanded", "false");
    tracing("closeSidebar", {});
  }

  function toggleSidebar() {
    const open = document.body.classList.toggle("sidebar-open");
    const toggle = document.getElementById("player-queue-toggle");
    if (toggle) toggle.setAttribute("aria-expanded", open ? "true" : "false");
    tracing("toggleSidebar", { open });
    if (open) {
      renderQueue();
      loadSidebarTracks(state.concertId);
    }
  }

  // Tear the player down completely: nothing is playing and there is nothing to
  // advance to. Used after deleting the last remaining track.
  function stopPlayback() {
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
    const concertTracks = document.getElementById("sidebar-concert-tracks");
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
    if (bar) bar.classList.remove("active");
    document.body.classList.remove("player-active");
    setPlayPauseIcon(false);
    updateLikeStar();
    updateDeleteButton();
    updateMediaButtons(false);
    updateNextButton();
    updatePrevButton();
  }

  // POST the delete, swap the refreshed card HTML in (if on page), return true on success.
  // Shared by the player-bar deleteTrack() and sidebar sidebarDeleteTrack().
  async function postDeleteTrack(concertId, trackIdx) {
    try {
      const resp = await fetch(`/concerts/${concertId}/tracks/${trackIdx}/delete`, { method: "POST" });
      if (!resp.ok) {
        showError("Delete failed");
        return false;
      }
      const html = await resp.text();
      // The response is the concert's full card; swap it in if visible on page.
      // List visibility is pure CSS so no open/closed state needs preserving.
      const card = document.getElementById("concert-" + concertId);
      if (card) {
        card.outerHTML = html;
        const fresh = document.getElementById("concert-" + concertId);
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
  async function advanceAfterDelete() {
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
  async function deleteTrack() {
    if (state.trackIdx == null) return;
    const concertId = state.concertId;
    const trackIdx = state.trackIdx;
    tracing("deleteTrack", { concertId, trackIdx });

    if (!(await postDeleteTrack(concertId, trackIdx))) return;

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
  // calls postDeleteTrack directly, then re-fetches the sidebar to reflect
  // new availability. Advances playback only when the deleted track was playing.
  async function sidebarDeleteTrack(concertId, trackIdx) {
    tracing("sidebarDeleteTrack", { concertId, trackIdx });
    const btn = document.querySelector(
      `#sidebar-concert-tracks .btn-delete[onclick*="sidebarDeleteTrack(${concertId}, ${trackIdx})"]`
    );
    if (btn) btn.disabled = true;

    const success = await postDeleteTrack(concertId, trackIdx);

    // Sidebar bypasses htmx card events, so refresh it explicitly.
    if (isSidebarOpen()) await loadSidebarTracks(concertId);

    if (!success) return;

    if (state.concertId === concertId && state.trackIdx === trackIdx) {
      await advanceAfterDelete();
    }
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", init);
  } else {
    init();
  }

  return { playAlbum, playTrack, playTracks, startAlbum, startTrack, togglePause, seek,
    skipToNext, skipToPrev, watch, openExternal, watchTrackDirect, toggleLike, deleteTrack,
    openConcert, toggleSidebar, sidebarDeleteTrack, playQueueEntryNow, dequeue };
})();
