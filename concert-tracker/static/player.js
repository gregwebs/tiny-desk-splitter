"use strict";

const Player = (() => {
  let audio = null;
  let bar = null;
  let state = { concertId: null, trackIdx: null, activeButton: null, isVideo: false, watchUrl: null, hasNext: false, hasPrev: false, liked: false };
  let queue = [];
  let autoAdvanceController = null;

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
      updateDeleteButton();
    }
  }

  // After an in-place #content swap or a Back/Forward history restore re-creates
  // the listen buttons, re-assert the JS-driven player UI: the playing-track
  // highlight and the #player-like / #player-delete display (the player bar
  // itself is never swapped, so the audio keeps playing untouched).
  function reassertPlayerUi() {
    reapplyPlaying();
    updateLikeStar();
    updateDeleteButton();
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

  // How long the minimize button stays visible after the last mouse movement.
  const VIDEO_CONTROLS_IDLE_MS = 2500;
  let videoControlsTimer = null;

  // Reveal the minimize button on mouse movement (or a touch) while watching, then
  // fade it back out once the pointer goes idle.
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
    // Defer attaching the outside-click listener to the next tick: watchTrackDirect()
    // opens the panel from a track-list button that lives outside #player-container, so
    // attaching synchronously would let that very click bubble up and re-close it.
    setTimeout(() => document.addEventListener("click", onOutsideVideoClick), 0);
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

  // Show the delete button only while an individual track is playing (no
  // per-track delete for whole-album playback) and the track is not starred —
  // a starred track is protected from deletion until it is unstarred.
  function updateDeleteButton() {
    const btn = document.getElementById("player-delete");
    if (!btn) return;
    btn.style.display = state.trackIdx == null || state.liked ? "none" : "inline-block";
  }

  // Set the liked state on the player star and mirror it onto the playing
  // track's track-list button when that row is on the page (in-place, no
  // re-render). No-op for the row when it isn't present (cross-concert safe).
  function setLikeState(liked) {
    state.liked = liked;
    updateLikeStar();
    updateDeleteButton();
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
    showBar();
    updateInfo(title, artist, trackIdx);
    markPlaying(btn);

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
        `/concerts/${concertId}/watch`, info.has_next, info.liked, info.has_prev);
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
        `/concerts/${concertId}/tracks/${trackIdx}/watch`, info.has_next, info.liked, info.has_prev);
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

  // Album "Play" button next to the track count: play the split tracks starting
  // from the first one that still exists (track 0 may have been deleted).
  async function playTracks(btn, concertId) {
    const trackIdx = await firstAvailableTrackIndex(concertId);
    if (trackIdx == null) {
      btn.classList.add("btn-listen-error");
      btn.textContent = "Error";
      tracing("playTracks: no playable track", { concertId });
      return;
    }
    await playTrack(btn, concertId, trackIdx);
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

  // Track-list/detail Watch button: start this track playing inline and fold up
  // the video panel.
  async function watchTrackDirect(btn, concertId, trackIdx) {
    if (await startTrack(btn, concertId, trackIdx)) showVideoPanel();
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
    queue = [];
    updateQueueBadge();
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

  // Player-bar Delete button: delete the currently-playing track's files (no
  // confirmation, matching the track-list button), refresh the on-page track
  // list, then advance to the next track — or stop if nothing is next.
  async function deleteTrack() {
    if (state.trackIdx == null) return;
    const concertId = state.concertId;
    const trackIdx = state.trackIdx;
    tracing("deleteTrack", { concertId, trackIdx });

    let resp;
    try {
      resp = await fetch(`/concerts/${concertId}/tracks/${trackIdx}/delete`, { method: "POST" });
    } catch (e) {
      showError("Delete failed");
      tracing("deleteTrack fetch failed", e);
      return;
    }
    if (!resp.ok) {
      showError("Delete failed");
      return;
    }
    const html = await resp.text();

    // Refresh the on-page track list (if the deleted row is present) so it shows
    // the track as unavailable. No-op when playing a track whose row isn't on the
    // current page (cross-concert safe).
    const onPage = findTrackButton(concertId, trackIdx);
    const list = onPage && onPage.closest(".track-list");
    if (list) {
      // outerHTML detaches the old node; capture the parent first so we can
      // re-query the fresh list and let htmx process its hx-* attributes (each
      // concert's list lives in its own container, so one .track-list per parent).
      const parent = list.parentNode;
      list.outerHTML = html;
      const fresh = parent && parent.querySelector(".track-list");
      if (fresh && window.htmx) window.htmx.process(fresh);
    }

    // The delete succeeded server-side, but if playback moved on while the POST
    // was in flight (track ended, or the user switched), do not disturb whatever
    // is playing now — just leave the refreshed list.
    if (state.concertId !== concertId || state.trackIdx !== trackIdx) {
      tracing("deleteTrack: playback moved on, not advancing", {});
      return;
    }

    // Advance like the Next button (state left intact so "next" is computed
    // after the deleted index); stop if there is nothing to advance to.
    cancelAutoAdvance();
    if (audio) audio.pause();
    const played = await playFromQueue();
    if (!played) {
      const advanced = await playNextTrack();
      if (!advanced) stopPlayback();
    }
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", init);
  } else {
    init();
  }

  return { playAlbum, playTrack, playTracks, startAlbum, startTrack, togglePause, seek,
    skipToNext, skipToPrev, watch, openExternal, watchTrackDirect, toggleLike, deleteTrack };
})();
