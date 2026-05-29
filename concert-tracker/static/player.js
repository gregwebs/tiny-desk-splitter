"use strict";

const Player = (() => {
  let audio = null;
  let bar = null;
  let state = { concertId: null, trackIdx: null, activeButton: null, isVideo: false, watchUrl: null, hasNext: false };
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
        `/concerts/${concertId}/tracks/${info.track_index}/watch`, info.has_next);
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

  function updateWatchButton(isVideo) {
    const btn = document.getElementById("player-watch");
    if (btn) btn.style.display = isVideo ? "inline-block" : "none";
  }

  // There is "something next" when the queue is non-empty or the current track
  // has a following track to auto-advance to. Disable the Next button otherwise
  // so clicking it cannot stop the current track with nothing to replace it.
  function updateNextButton() {
    const btn = document.getElementById("player-next");
    if (!btn) return;
    btn.disabled = queue.length === 0 && !state.hasNext;
  }

  async function play(btn, url, title, artist, concertId, trackIdx, listenUrl, isVideo, watchUrl, hasNext) {
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
    updateWatchButton(isVideo);
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

  async function playAlbum(btn, concertId) {
    cancelAutoAdvance();
    try {
      const resp = await fetch(`/concerts/${concertId}/media-info`);
      if (!resp.ok) {
        btn.classList.add("btn-listen-error");
        btn.textContent = "Error";
        return;
      }
      const info = await resp.json();
      if (!info.playable) {
        window.open(info.url, "_blank");
        return;
      }
      await play(btn, info.url, info.title, info.artist, concertId, null,
        `/concerts/${concertId}/listen`, info.is_video,
        `/concerts/${concertId}/watch`, info.has_next);
    } catch (e) {
      btn.classList.add("btn-listen-error");
      btn.textContent = "Error";
      tracing("playAlbum fetch failed", e);
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
    cancelAutoAdvance();
    try {
      const resp = await fetch(`/concerts/${concertId}/tracks/${trackIdx}/media-info`);
      if (!resp.ok) {
        btn.classList.add("btn-listen-error");
        btn.textContent = "Error";
        return;
      }
      const info = await resp.json();
      if (!info.playable) {
        window.open(info.url, "_blank");
        return;
      }
      await play(btn, info.url, info.title, info.artist, concertId, trackIdx,
        `/concerts/${concertId}/tracks/${trackIdx}/listen`, info.is_video,
        `/concerts/${concertId}/tracks/${trackIdx}/watch`, info.has_next);
    } catch (e) {
      btn.classList.add("btn-listen-error");
      btn.textContent = "Error";
      tracing("playTrack fetch failed", e);
    }
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
          `/concerts/${entry.concertId}/tracks/${entry.trackIdx}/watch`, info.has_next);
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

  async function watch() {
    if (!state.watchUrl) return;
    if (audio) audio.pause();
    clearPlaying();
    try {
      await fetch(state.watchUrl, { method: "POST" });
    } catch (e) {
      tracing("watch fetch failed", e);
    }
  }

  function markWatchError(btn) {
    if (!btn) return;
    btn.classList.add("btn-watch-error");
    btn.textContent = "Error";
  }

  async function watchDirect(btn, concertId) {
    if (audio && state.concertId === concertId && !audio.paused) {
      audio.pause();
      clearPlaying();
    }
    try {
      const resp = await fetch(`/concerts/${concertId}/watch`, { method: "POST" });
      if (!resp.ok) markWatchError(btn);
    } catch (e) {
      markWatchError(btn);
      tracing("watchDirect fetch failed", e);
    }
  }

  async function watchTrackDirect(btn, concertId, trackIdx) {
    if (audio && state.concertId === concertId && state.trackIdx === trackIdx && !audio.paused) {
      audio.pause();
      clearPlaying();
    }
    try {
      const resp = await fetch(`/concerts/${concertId}/tracks/${trackIdx}/watch`, { method: "POST" });
      if (!resp.ok) markWatchError(btn);
    } catch (e) {
      markWatchError(btn);
      tracing("watchTrackDirect fetch failed", e);
    }
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", init);
  } else {
    init();
  }

  return { playAlbum, playTrack, togglePause, seek, skipToNext, watch, watchDirect, watchTrackDirect };
})();
