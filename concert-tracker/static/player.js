"use strict";

const Player = (() => {
  let audio = null;
  let bar = null;
  let state = { concertId: null, trackIdx: null, activeButton: null, isVideo: false, watchUrl: null };

  function init() {
    audio = document.getElementById("player-audio");
    bar = document.getElementById("player-bar");
    if (!audio || !bar) return;

    audio.addEventListener("timeupdate", onTimeUpdate);
    audio.addEventListener("loadedmetadata", onTimeUpdate);
    audio.addEventListener("ended", onEnded);
    audio.addEventListener("error", onError);
    audio.addEventListener("play", () => setPlayPauseIcon(true));
    audio.addEventListener("pause", () => setPlayPauseIcon(false));

    document.body.addEventListener("htmx:afterSwap", reapplyPlaying);
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

  function updateInfo(title, artist) {
    const t = document.getElementById("player-title");
    const a = document.getElementById("player-artist");
    if (t) t.textContent = title;
    if (a) a.textContent = artist;
  }

  function onTimeUpdate() {
    const seek = document.getElementById("player-seek");
    const time = document.getElementById("player-time");
    if (!audio.duration) return;
    if (seek) seek.value = (audio.currentTime / audio.duration) * 100;
    if (time) time.textContent = formatTime(audio.currentTime) + " / " + formatTime(audio.duration);
  }

  function onEnded() {
    setPlayPauseIcon(false);
  }

  function onError() {
    showError("Failed to load media");
    tracing("audio error", audio.error);
  }

  function tracing(label, obj) {
    if (obj) console.warn("[Player]", label, obj);
  }

  function formatTime(seconds) {
    const m = Math.floor(seconds / 60);
    const s = Math.floor(seconds % 60);
    return m + ":" + (s < 10 ? "0" : "") + s;
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
    let selector;
    if (state.trackIdx != null) {
      selector = `[data-concert-id="${state.concertId}"][data-track-idx="${state.trackIdx}"]`;
    } else {
      selector = `[data-concert-id="${state.concertId}"][data-role="listen-album"]`;
    }
    const btn = document.querySelector(selector);
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

  async function play(btn, url, title, artist, concertId, trackIdx, listenUrl, isVideo, watchUrl) {
    if (!audio) init();
    if (!audio) return;

    hideError();
    showBar();
    updateInfo(title, artist);
    markPlaying(btn);

    state.concertId = concertId;
    state.trackIdx = trackIdx;
    state.isVideo = isVideo;
    state.watchUrl = watchUrl;
    updateWatchButton(isVideo);

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
        `/concerts/${concertId}/watch`);
    } catch (e) {
      btn.classList.add("btn-listen-error");
      btn.textContent = "Error";
      tracing("playAlbum fetch failed", e);
    }
  }

  async function playTrack(btn, concertId, trackIdx) {
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
        `/concerts/${concertId}/tracks/${trackIdx}/watch`);
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

  async function watchDirect(concertId) {
    if (audio && state.concertId === concertId && !audio.paused) {
      audio.pause();
      clearPlaying();
    }
    try {
      await fetch(`/concerts/${concertId}/watch`, { method: "POST" });
    } catch (e) {
      tracing("watchDirect fetch failed", e);
    }
  }

  async function watchTrackDirect(concertId, trackIdx) {
    if (audio && state.concertId === concertId && state.trackIdx === trackIdx && !audio.paused) {
      audio.pause();
      clearPlaying();
    }
    try {
      await fetch(`/concerts/${concertId}/tracks/${trackIdx}/watch`, { method: "POST" });
    } catch (e) {
      tracing("watchTrackDirect fetch failed", e);
    }
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", init);
  } else {
    init();
  }

  return { playAlbum, playTrack, togglePause, seek, watch, watchDirect, watchTrackDirect };
})();
