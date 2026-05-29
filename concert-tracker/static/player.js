"use strict";

const Player = (() => {
  let audio = null;
  let bar = null;
  let state = { concertId: null, trackIdx: null, activeButton: null, isVideo: false, watchUrl: null, hasNext: false, liked: false };
  let queue = [];
  let autoAdvanceController = null;
  // Snapshot of playback taken just before an htmx body swap / history save.
  // hx-boost navigation (especially the browser Back button restoring a cached
  // page) detaches and re-creates #player-audio. A detached media element is
  // paused by the browser and its currentTime can reset, so by the time rebind()
  // runs the old node is unreliable. We capture src/time/playing while the audio
  // is still live and restore from that instead.
  let navState = null;

  function captureNavState() {
    if (audio && audio.src) {
      navState = { src: audio.src, time: audio.currentTime, playing: !audio.paused };
    }
  }

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

  function unbindAudioEvents(el) {
    el.removeEventListener("timeupdate", onTimeUpdate);
    el.removeEventListener("loadedmetadata", onTimeUpdate);
    el.removeEventListener("ended", onEnded);
    el.removeEventListener("error", onError);
    el.removeEventListener("play", onPlay);
    el.removeEventListener("pause", onPause);
  }

  function init() {
    audio = document.getElementById("player-audio");
    bar = document.getElementById("player-bar");
    if (!audio || !bar) return;

    bindAudioEvents();

    // Capture playback state before the DOM swap detaches the audio element.
    document.body.addEventListener("htmx:beforeSwap", captureNavState);
    document.body.addEventListener("htmx:beforeHistorySave", captureNavState);

    // Restore after a swap (boosted navigation) or a history (Back/Forward)
    // restore; the latter does not always fire afterSettle.
    document.body.addEventListener("htmx:afterSettle", restorePlayback);
    document.body.addEventListener("htmx:historyRestore", restorePlayback);

    // Reverse like-sync: when a track list re-renders (track-list star toggled,
    // or a track deleted), re-read the liked state of the playing track so the
    // player star matches. findTrackButton only matches the current DOM, so this
    // is a no-op when the playing track's row isn't on the page (cross-concert).
    document.body.addEventListener("htmx:afterSwap", syncLikeFromTrackList);
  }

  // Locate the .btn-like in the same row as the playing track's listen button.
  function currentTrackLikeButton() {
    if (state.trackIdx == null) return null;
    const btn = findTrackButton(state.concertId, state.trackIdx);
    const li = btn && btn.closest("li");
    return li ? li.querySelector(".btn-like") : null;
  }

  function syncLikeFromTrackList() {
    const lb = currentTrackLikeButton();
    if (!lb) return;
    const liked = lb.classList.contains("liked");
    if (liked !== state.liked) {
      state.liked = liked;
      updateLikeStar();
    }
  }

  function restorePlayback() {
    rebind();
    // Even when hx-preserve keeps the same audio node, detaching/reattaching it
    // during the swap leaves it paused (position is retained). Resume if we were
    // playing before the swap so the Back button doesn't silently stop playback.
    if (audio && navState && navState.playing && audio.src && audio.paused) {
      showBar();
      audio.play().catch(() => {});
    }
    reapplyPlaying();
    // #player-like lives in the preserved container but its display/text are
    // JS-driven, so re-assert it after Back/Forward and boosted swaps.
    updateLikeStar();
  }

  // Restore src/position onto the current audio node, seeking once metadata is
  // available (setting currentTime before then is ignored by the browser).
  function loadInto(el, src, time, play) {
    el.src = src;
    const seek = () => {
      if (time) {
        try { el.currentTime = time; } catch (e) { tracing("seek failed", e); }
      }
      if (play) {
        showBar();
        el.play().catch(() => {});
      }
    };
    if (el.readyState >= 1) seek();
    else el.addEventListener("loadedmetadata", seek, { once: true });
  }

  function rebind() {
    if (!audio) return;
    if (document.body.contains(audio)) return;

    // The old node was detached by the swap; prefer the pre-swap snapshot since
    // a detached media element reports paused with a reset currentTime.
    const wasPlaying = navState ? navState.playing : !audio.paused;
    const oldSrc = navState ? navState.src : audio.src;
    const oldTime = navState ? navState.time : audio.currentTime;

    unbindAudioEvents(audio);
    audio.pause();

    audio = document.getElementById("player-audio");
    bar = document.getElementById("player-bar");
    if (!audio || !bar) return;

    bindAudioEvents();

    if (oldSrc) {
      loadInto(audio, oldSrc, oldTime, wasPlaying);
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

  function updateInfo(title, artist, trackIdx) {
    const t = document.getElementById("player-title");
    const a = document.getElementById("player-artist");
    const n = document.getElementById("player-track");
    if (t) t.textContent = title;
    if (a) a.textContent = artist;
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

  async function onEnded() {
    const played = await playFromQueue();
    if (!played) playNextTrack();
  }

  async function onError() {
    showError("Failed to load media");
    tracing("audio error", audio.error);
    const played = await playFromQueue();
    if (!played) playNextTrack();
  }

  function cancelAutoAdvance() {
    if (autoAdvanceController) {
      autoAdvanceController.abort();
      autoAdvanceController = null;
    }
  }

  async function playNextTrack() {
    if (state.trackIdx == null || state.concertId == null) {
      setPlayPauseIcon(false);
      return;
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
        return;
      }
      if (signal.aborted) return;
      const info = await resp.json();

      const btn = findTrackButton(concertId, info.track_index);
      await play(btn, info.url, info.title, info.artist, concertId, info.track_index,
        `/concerts/${concertId}/tracks/${info.track_index}/listen`, info.is_video,
        `/concerts/${concertId}/tracks/${info.track_index}/watch`, info.has_next, info.liked);
    } catch (e) {
      if (e.name !== "AbortError") {
        tracing("playNextTrack failed", e);
        setPlayPauseIcon(false);
      }
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

  function findTrackButton(concertId, trackIdx) {
    if (trackIdx != null) {
      return document.querySelector(`[data-concert-id="${concertId}"][data-track-idx="${trackIdx}"]`);
    }
    return document.querySelector(`[data-concert-id="${concertId}"][data-role="listen-album"]`);
  }

  function clearPlaying() {
    if (state.activeButton) {
      state.activeButton.classList.remove("playing");
      state.activeButton = null;
    }
  }

  function markPlaying(btn) {
    clearPlaying();
    if (btn) {
      btn.classList.add("playing");
      state.activeButton = btn;
    }
  }

  function reapplyPlaying() {
    if (state.concertId == null) return;
    const btn = findTrackButton(state.concertId, state.trackIdx);
    if (btn && !audio.paused) {
      clearPlaying();
      btn.classList.add("playing");
      state.activeButton = btn;
    }
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

  function showVideoPanel() {
    const panel = document.getElementById("player-video-panel");
    if (!panel) return;
    tracing("showVideoPanel", {});
    panel.classList.add("open");
  }

  function hideVideoPanel() {
    const panel = document.getElementById("player-video-panel");
    if (!panel || !panel.classList.contains("open")) return;
    tracing("hideVideoPanel", {});
    panel.classList.remove("open");
  }

  // Player-bar Watch button: fold the inline video panel up or down. The video
  // is the already-playing #player-audio element, so revealing it needs no resync.
  function watch() {
    const panel = document.getElementById("player-video-panel");
    if (!panel) return;
    if (panel.classList.contains("open")) hideVideoPanel();
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

  // Set the liked state on the player star and mirror it onto the playing
  // track's track-list button when that row is on the page (in-place, no
  // re-render). No-op for the row when it isn't present (cross-concert safe).
  function setLikeState(liked) {
    state.liked = liked;
    updateLikeStar();
    const lb = currentTrackLikeButton();
    if (lb) {
      lb.classList.toggle("liked", liked);
      lb.textContent = liked ? "★" : "☆";
    }
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

  async function play(btn, url, title, artist, concertId, trackIdx, listenUrl, isVideo, watchUrl, hasNext, liked) {
    if (!audio) init(); else rebind();
    if (!audio) return;

    hideError();
    showBar();
    updateInfo(title, artist, trackIdx);
    markPlaying(btn);

    state.concertId = concertId;
    state.trackIdx = trackIdx;
    state.isVideo = isVideo;
    state.watchUrl = watchUrl;
    state.hasNext = !!hasNext;
    state.liked = !!liked;
    updateLikeStar();
    updateMediaButtons(isVideo);
    // An audio-only track can't be watched; collapse the panel if it was open.
    // A video track keeps the panel open so auto-advance keeps showing video.
    if (!isVideo) hideVideoPanel();
    updateNextButton();

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
        btn.classList.add("btn-listen-error");
        btn.textContent = "Error";
        return false;
      }
      const info = await resp.json();
      if (!info.playable) {
        window.open(info.url, "_blank");
        return false;
      }
      await play(btn, info.url, info.title, info.artist, concertId, null,
        `/concerts/${concertId}/listen`, info.is_video,
        `/concerts/${concertId}/watch`, info.has_next, info.liked);
      return true;
    } catch (e) {
      btn.classList.add("btn-listen-error");
      btn.textContent = "Error";
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
        btn.classList.add("btn-listen-error");
        btn.textContent = "Error";
        return false;
      }
      const info = await resp.json();
      if (!info.playable) {
        window.open(info.url, "_blank");
        return false;
      }
      await play(btn, info.url, info.title, info.artist, concertId, trackIdx,
        `/concerts/${concertId}/tracks/${trackIdx}/listen`, info.is_video,
        `/concerts/${concertId}/tracks/${trackIdx}/watch`, info.has_next, info.liked);
      return true;
    } catch (e) {
      btn.classList.add("btn-listen-error");
      btn.textContent = "Error";
      tracing("startTrack fetch failed", e);
      return false;
    }
  }

  async function playTrack(btn, concertId, trackIdx) {
    if (state.concertId === concertId && state.trackIdx === trackIdx && audio) {
      togglePause();
      return;
    }
    if (audio && !audio.paused && !audio.ended) {
      enqueue(concertId, trackIdx, btn.textContent.trim());
      return;
    }
    await startTrack(btn, concertId, trackIdx);
  }

  function togglePause() {
    if (!audio) return;
    if (audio.paused) {
      audio.play();
    } else {
      audio.pause();
    }
  }

  function seek(val) {
    if (!audio || !audio.duration) return;
    audio.currentTime = (val / 100) * audio.duration;
  }

  function enqueue(concertId, trackIdx, title) {
    if (queue.some(q => q.concertId === concertId && q.trackIdx === trackIdx)) {
      tracing("enqueue duplicate skipped", { concertId, trackIdx });
      return;
    }
    queue.push({ concertId, trackIdx, title });
    tracing("enqueue", { concertId, trackIdx, title, queueLength: queue.length });
    updateQueueBadge();
    updateNextButton();
  }

  async function playFromQueue() {
    while (queue.length > 0) {
      const entry = queue.shift();
      updateQueueBadge();
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
          `/concerts/${entry.concertId}/tracks/${entry.trackIdx}/watch`, info.has_next, info.liked);
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

  function updateQueueBadge() {
    const badge = document.getElementById("player-queue-badge");
    if (!badge) return;
    if (queue.length > 0) {
      badge.textContent = queue.length;
      badge.style.display = "inline-flex";
      badge.title = queue.map(q => q.title).join("\n");
    } else {
      badge.textContent = "";
      badge.style.display = "none";
      badge.title = "";
    }
  }

  // Link-out: launch the current file in the system player via the server's
  // `open`. state.watchUrl is the POST endpoint set by play(). This is the only
  // path that still records a server-side Watch event.
  async function openExternal() {
    if (!state.watchUrl) return;
    tracing("openExternal", { watchUrl: state.watchUrl });
    try {
      await fetch(state.watchUrl, { method: "POST" });
    } catch (e) {
      tracing("openExternal fetch failed", e);
    }
  }

  // Row/album Watch button: start the concert playing inline and fold up the
  // video panel. Interrupts whatever is playing (explicit intent to watch now).
  async function watchDirect(btn, concertId) {
    if (await startAlbum(btn, concertId)) showVideoPanel();
  }

  // Track-list/detail Watch button: start this track playing inline and fold up
  // the video panel.
  async function watchTrackDirect(btn, concertId, trackIdx) {
    if (await startTrack(btn, concertId, trackIdx)) showVideoPanel();
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", init);
  } else {
    init();
  }

  return { playAlbum, playTrack, startAlbum, startTrack, togglePause, seek, skipToNext,
    watch, openExternal, watchDirect, watchTrackDirect, toggleLike };
})();
