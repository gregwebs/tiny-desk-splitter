// Splitter timeline UI for editing per-track split points on the concert detail
// page. Adjacent tracks share a single split point by default (the boundary is
// "linked" — one handle is the end of track i and the start of track i+1). A
// boundary can be "detached" into two independent handles so the user can open a
// gap that belongs to no track (e.g. to cut out talking between songs).
//
// The backend API (see docs/change/2026-06-12-user-split-timestamps.md):
//   GET  /concerts/:id/split-timestamps        -> {set_list, auto, user, media_duration}
//   POST /concerts/:id/split-timestamps        {songs:[{title,start_time,end_time}]}
//   POST /concerts/:id/split-timestamps/reset
//
// The module touches no DOM at load time (entry point is the inline
// Splitter.toggle(btn) onclick), so it can also be require()d in Node to unit
// test the pure helpers exposed under `_pure`.
const Splitter = (() => {
  "use strict";

  // Mirror of MIN_SONG_DURATION_SECONDS in concert-tracker/src/split_timestamps.rs.
  // The server rejects any track shorter than this, so the editor never lets a
  // segment shrink below it.
  const MIN_SEG = 1.0;
  // Two boundary times within this many seconds are treated as a single linked
  // split point when loading stored timestamps.
  const LINK_EPS = 0.05;

  // ── Pure helpers (unit-tested; no DOM) ──────────────────────────────────────

  function clamp(v, lo, hi) {
    if (hi < lo) return lo;
    return v < lo ? lo : v > hi ? hi : v;
  }

  function round3(v) {
    return Math.round(v * 1000) / 1000;
  }

  // Parse "m:ss(.s)", "h:mm:ss", or a bare seconds number into seconds.
  // Returns NaN for anything unparseable so callers can reject it.
  function parseTimecode(str) {
    if (typeof str !== "string") return NaN;
    const s = str.trim();
    if (s === "") return NaN;
    if (s.indexOf(":") === -1) {
      const n = Number(s);
      return Number.isFinite(n) ? n : NaN;
    }
    const parts = s.split(":");
    if (parts.length > 3) return NaN;
    let total = 0;
    for (const p of parts) {
      if (p.trim() === "") return NaN;
      const n = Number(p);
      if (!Number.isFinite(n) || n < 0) return NaN;
      total = total * 60 + n;
    }
    return total;
  }

  // Format seconds as "m:ss.s" (one decimal) for the numeric inputs.
  function formatTimecode(sec) {
    if (!Number.isFinite(sec) || sec < 0) sec = 0;
    const m = Math.floor(sec / 60);
    const rest = sec - m * 60;
    const s = rest.toFixed(1).padStart(4, "0"); // "05.0"
    return m + ":" + s;
  }

  // Build editor state from the GET response. Prefers user timestamps over auto.
  // `duration` falls back to the last end time when media_duration is null
  // (e.g. ffprobe unavailable), so the timeline still has a right edge.
  function initState(resp) {
    const chosen = resp.user || resp.auto;
    if (!chosen || chosen.length === 0) return null;
    const tracks = chosen.map((t) => ({
      title: t.title,
      start: t.start_time,
      end: t.end_time,
    }));
    const lastEnd = tracks[tracks.length - 1].end;
    let duration = resp.media_duration;
    if (!Number.isFinite(duration) || duration < lastEnd) duration = lastEnd;
    const linked = [];
    for (let i = 0; i < tracks.length - 1; i++) {
      linked.push(Math.abs(tracks[i].end - tracks[i + 1].start) <= LINK_EPS);
    }
    return { duration, tracks, linked };
  }

  // Move track i's start, honouring the linked boundary to its left. Returns the
  // mutated state (mutates in place; callers pass a live state object).
  function setStart(state, i, value) {
    const t = state.tracks;
    const lower =
      i === 0 ? 0 : state.linked[i - 1] ? t[i - 1].start + MIN_SEG : t[i - 1].end;
    const upper = t[i].end - MIN_SEG;
    const v = clamp(value, lower, upper);
    t[i].start = v;
    if (i > 0 && state.linked[i - 1]) t[i - 1].end = v;
    return state;
  }

  // Move track i's end, honouring the linked boundary to its right.
  function setEnd(state, i, value) {
    const t = state.tracks;
    const last = t.length - 1;
    const upper =
      i === last ? state.duration : state.linked[i] ? t[i + 1].end - MIN_SEG : t[i + 1].start;
    const lower = t[i].start + MIN_SEG;
    const v = clamp(value, lower, upper);
    t[i].end = v;
    if (i < last && state.linked[i]) t[i + 1].start = v;
    return state;
  }

  // Split boundary i into two independent handles. Pure topology change — no
  // value is nudged, so the gap starts at zero until the user drags them apart.
  function detach(state, i) {
    state.linked[i] = false;
    return state;
  }

  // Re-couple boundary i, collapsing any gap by pulling track i+1's start back to
  // track i's end (which is always a valid start for i+1).
  function link(state, i) {
    state.linked[i] = true;
    state.tracks[i + 1].start = state.tracks[i].end;
    return state;
  }

  // Ordered list of draggable handles for the current topology. Each handle reads
  // and writes one boundary time via setStart/setEnd (which handle linking).
  function handlesFor(state) {
    const t = state.tracks;
    const last = t.length - 1;
    const out = [{ kind: "start", track: 0 }]; // head (trim intro)
    for (let i = 0; i < last; i++) {
      if (state.linked[i]) {
        out.push({ kind: "end", track: i, boundary: i, linked: true });
      } else {
        out.push({ kind: "end", track: i, boundary: i, linked: false });
        out.push({ kind: "start", track: i + 1, boundary: i, linked: false });
      }
    }
    out.push({ kind: "end", track: last }); // tail (trim outro)
    return out;
  }

  function handleTime(state, h) {
    return h.kind === "start" ? state.tracks[h.track].start : state.tracks[h.track].end;
  }

  function applyHandle(state, h, value) {
    return h.kind === "start" ? setStart(state, h.track, value) : setEnd(state, h.track, value);
  }

  // Validate against the same rules the server enforces. Returns error strings.
  function validate(state) {
    const errors = [];
    const t = state.tracks;
    for (let i = 0; i < t.length; i++) {
      if (t[i].start < -1e-6) errors.push(`${t[i].title}: starts before 0`);
      if (t[i].end - t[i].start < MIN_SEG - 1e-6)
        errors.push(`${t[i].title}: shorter than ${MIN_SEG}s`);
      if (t[i].end > state.duration + 1e-6)
        errors.push(`${t[i].title}: ends past media duration`);
      if (i < t.length - 1 && t[i].end > t[i + 1].start + 1e-6)
        errors.push(`${t[i].title} overlaps ${t[i + 1].title}`);
    }
    return errors;
  }

  function buildPayload(state) {
    return {
      songs: state.tracks.map((t) => ({
        title: t.title,
        start_time: round3(t.start),
        end_time: round3(t.end),
      })),
    };
  }

  // ── DOM module (browser only) ───────────────────────────────────────────────

  // Per-open session state. `state` is the pure editor state above.
  let S = null;

  function $(sel, root) {
    return (root || document).querySelector(sel);
  }

  function pct(state, t) {
    return state.duration > 0 ? (t / state.duration) * 100 : 0;
  }

  function setStatus(msg, kind) {
    if (!S || !S.statusEl) return;
    S.statusEl.textContent = msg || "";
    S.statusEl.className = "splitter-status" + (kind ? " splitter-status-" + kind : "");
  }

  async function toggle(btn) {
    const container = document.getElementById("splitter");
    if (!container) return;
    if (container.classList.contains("splitter-open")) {
      close(container, btn);
      return;
    }
    container.classList.add("splitter-open");
    if (btn) btn.textContent = "Hide track splitter";
    await open(container);
  }

  function close(container, btn) {
    if (S && S.globalPlayheadHandler) {
      const globalAudio = document.getElementById("player-audio");
      if (globalAudio) globalAudio.removeEventListener("timeupdate", S.globalPlayheadHandler);
    }
    container.classList.remove("splitter-open");
    container.innerHTML = "";
    if (btn) btn.textContent = "Edit track splits";
    S = null;
  }

  async function open(container) {
    const id = Number(container.getAttribute("data-concert-id"));
    container.innerHTML = '<p class="splitter-status">Loading…</p>';
    S = { id, container, state: null, mediaUrl: null, playable: false, dom: {}, busy: false, globalPlayheadHandler: null };
    try {
      const [tsResp, mediaResp] = await Promise.all([
        fetch(`/concerts/${id}/split-timestamps`).then(okJson),
        fetch(`/concerts/${id}/media-info`).then((r) => (r.ok ? r.json() : null)),
      ]);
      const state = initState(tsResp);
      if (!state) {
        container.innerHTML =
          '<p class="splitter-status">No split points yet — run an automatic split first, then come back to fine-tune them.</p>';
        return;
      }
      S.state = state;
      if (mediaResp && mediaResp.url) {
        S.mediaUrl = mediaResp.url;
        S.playable = !!mediaResp.playable;
      }
      render();
      const globalAudio = document.getElementById("player-audio");
      if (globalAudio) {
        const handler = () => positionPlayhead();
        S.globalPlayheadHandler = handler;
        globalAudio.addEventListener("timeupdate", handler);
      }
    } catch (e) {
      container.innerHTML =
        '<p class="splitter-status splitter-status-error">Could not load split timestamps.</p>';
      console.warn("[Splitter] load failed", e);
    }
  }

  function okJson(r) {
    if (!r.ok) throw new Error("HTTP " + r.status);
    return r.json();
  }

  // Build the whole UI once for the current topology, then position everything.
  function render() {
    const c = S.container;
    c.innerHTML = "";

    // Toolbar: status + actions.
    const toolbar = el("div", "splitter-toolbar");
    const status = el("span", "splitter-status");
    const submitBtn = el("button", "splitter-submit", "Split with these times");
    submitBtn.type = "button";
    submitBtn.addEventListener("click", submit);
    const revertBtn = el("button", "splitter-revert", "Discard my edits");
    revertBtn.type = "button";
    revertBtn.addEventListener("click", revert);
    const resetBtn = el("button", "splitter-reset", "Reset to auto");
    resetBtn.type = "button";
    resetBtn.addEventListener("click", reset);
    toolbar.append(status, submitBtn, revertBtn, resetBtn);
    S.statusEl = status;
    S.submitBtn = submitBtn;
    S.revertBtn = revertBtn;
    S.resetBtn = resetBtn;

    // Timeline.
    const timeline = el("div", "splitter-timeline");
    timeline.addEventListener("pointerdown", onTimelineSeek);
    const playhead = el("div", "splitter-playhead");
    timeline.appendChild(playhead);
    S.dom.timeline = timeline;
    S.dom.playhead = playhead;
    S.dom.segs = [];
    S.dom.gaps = [];
    S.dom.handles = [];

    S.state.tracks.forEach((t, i) => {
      const seg = el("div", "splitter-seg");
      seg.title = t.title;
      seg.appendChild(el("span", "splitter-seg-label", `${i + 1}. ${t.title}`));
      timeline.appendChild(seg);
      S.dom.segs.push(seg);
    });
    for (let i = 0; i < S.state.tracks.length - 1; i++) {
      const gap = el("div", "splitter-gap");
      timeline.appendChild(gap);
      S.dom.gaps.push(gap);
    }
    handlesFor(S.state).forEach((h) => {
      const handle = el("div", "splitter-handle splitter-handle-" + h.kind);
      handle._h = h;
      handle.addEventListener("pointerdown", (e) => onHandleDown(e, handle));
      timeline.appendChild(handle);
      S.dom.handles.push(handle);
    });

    // Preview note for non-playable sources.
    let previewNote = null;
    if (!S.playable) {
      previewNote = el(
        "p",
        "splitter-note",
        S.mediaUrl
          ? "Audio preview unavailable for this file format."
          : "Audio preview unavailable — source file not found."
      );
    }

    // Boundary detach/link controls.
    const boundaries = el("div", "splitter-boundaries");
    S.dom.boundaryBtns = [];
    for (let i = 0; i < S.state.tracks.length - 1; i++) {
      const row = el("div", "splitter-boundary");
      const btn = el("button", "splitter-detach");
      btn.type = "button";
      btn.dataset.boundary = String(i);
      btn.addEventListener("click", () => toggleBoundary(i));
      row.append(
        el(
          "span",
          "splitter-boundary-label",
          `${S.state.tracks[i].title} → ${S.state.tracks[i + 1].title}`
        ),
        btn
      );
      boundaries.appendChild(row);
      S.dom.boundaryBtns.push(btn);
    }

    // Numeric table.
    const table = el("table", "splitter-table");
    const tbody = el("tbody");
    S.dom.rows = [];
    S.state.tracks.forEach((t, i) => {
      const tr = el("tr");
      tr.appendChild(el("td", "splitter-num", String(i + 1)));
      tr.appendChild(el("td", "splitter-title", t.title));
      const startCell = el("td");
      const startInput = inputFor(i, "start");
      const startPlay = previewBtn(() => previewAt(S.state.tracks[i].start));
      startCell.append(startInput, startPlay);
      const endCell = el("td");
      const endInput = inputFor(i, "end");
      const endPlay = previewBtn(() => previewAt(Math.max(0, S.state.tracks[i].end - 3)));
      endCell.append(endInput, endPlay);
      tr.append(startCell, endCell);
      tbody.appendChild(tr);
      S.dom.rows.push({ startInput, endInput });
    });
    const thead = el("thead");
    const htr = el("tr");
    ["#", "Track", "Start", "End (▶ auditions last 3s)"].forEach((h) =>
      htr.appendChild(el("th", null, h))
    );
    thead.appendChild(htr);
    table.append(thead, tbody);

    c.append(toolbar, timeline);
    if (previewNote) c.appendChild(previewNote);
    c.append(boundaries, table);
    syncDom();
  }

  function inputFor(i, kind) {
    const input = el("input", "splitter-time");
    input.type = "text";
    input.inputMode = "decimal";
    input.addEventListener("change", () => onInputChange(i, kind, input));
    return input;
  }

  function previewBtn(fn) {
    const b = el("button", "splitter-play", "▶");
    b.type = "button";
    b.title = "Play from here";
    if (S.playable) b.addEventListener("click", fn);
    else b.disabled = true;
    return b;
  }

  function el(tag, cls, text) {
    const e = document.createElement(tag);
    if (cls) e.className = cls;
    if (text != null) e.textContent = text;
    return e;
  }

  // Reposition segments, gaps, handles, inputs, and boundary buttons from state
  // without rebuilding the DOM (safe to call mid-drag).
  function syncDom() {
    const st = S.state;
    st.tracks.forEach((t, i) => {
      const seg = S.dom.segs[i];
      seg.style.left = pct(st, t.start) + "%";
      seg.style.width = pct(st, t.end - t.start) + "%";
    });
    for (let i = 0; i < st.tracks.length - 1; i++) {
      const gap = S.dom.gaps[i];
      const gapStart = st.tracks[i].end;
      const gapEnd = st.tracks[i + 1].start;
      const w = gapEnd - gapStart;
      if (w > 1e-6) {
        gap.style.display = "block";
        gap.style.left = pct(st, gapStart) + "%";
        gap.style.width = pct(st, w) + "%";
      } else {
        gap.style.display = "none";
      }
    }
    S.dom.handles.forEach((handle) => {
      handle.style.left = pct(st, handleTime(st, handle._h)) + "%";
    });
    S.dom.rows.forEach((row, i) => {
      if (document.activeElement !== row.startInput)
        row.startInput.value = formatTimecode(st.tracks[i].start);
      if (document.activeElement !== row.endInput)
        row.endInput.value = formatTimecode(st.tracks[i].end);
    });
    if (S.dom.boundaryBtns) {
      S.dom.boundaryBtns.forEach((btn, i) => {
        btn.textContent = st.linked[i] ? "Detach (add gap)" : "Link (remove gap)";
      });
    }
    refreshValidity();
  }

  function refreshValidity() {
    const errors = validate(S.state);
    const busy = S.busy;
    if (S.submitBtn) S.submitBtn.disabled = busy || errors.length > 0;
    if (S.revertBtn) S.revertBtn.disabled = busy;
    if (S.resetBtn) S.resetBtn.disabled = busy;
    if (!busy) {
      if (errors.length > 0) setStatus(errors[0], "error");
      else setStatus("");
    }
  }

  // ── Interaction ─────────────────────────────────────────────────────────────

  function timeFromClientX(clientX) {
    const rect = S.dom.timeline.getBoundingClientRect();
    const frac = clamp((clientX - rect.left) / rect.width, 0, 1);
    return frac * S.state.duration;
  }

  function onHandleDown(e, handle) {
    e.preventDefault();
    e.stopPropagation(); // don't trigger timeline seek
    handle.setPointerCapture(e.pointerId);
    handle.classList.add("dragging");
    const move = (ev) => {
      applyHandle(S.state, handle._h, timeFromClientX(ev.clientX));
      syncDom();
    };
    const up = (ev) => {
      handle.releasePointerCapture(e.pointerId);
      handle.classList.remove("dragging");
      handle.removeEventListener("pointermove", move);
      handle.removeEventListener("pointerup", up);
      handle.removeEventListener("pointercancel", up);
    };
    handle.addEventListener("pointermove", move);
    handle.addEventListener("pointerup", up);
    handle.addEventListener("pointercancel", up);
  }

  function onTimelineSeek(e) {
    if (!S.playable) return;
    previewAt(timeFromClientX(e.clientX));
  }

  function onInputChange(i, kind, input) {
    const v = parseTimecode(input.value);
    if (!Number.isFinite(v)) {
      // Reject: restore the prior value.
      input.value = formatTimecode(kind === "start" ? S.state.tracks[i].start : S.state.tracks[i].end);
      setStatus("Enter a time like 2:05.0", "error");
      return;
    }
    if (kind === "start") setStart(S.state, i, v);
    else setEnd(S.state, i, v);
    syncDom();
  }

  function toggleBoundary(i) {
    if (S.state.linked[i]) detach(S.state, i);
    else link(S.state, i);
    render(); // topology changed: rebuild handles/gaps
  }

  // ── Preview audio ───────────────────────────────────────────────────────────

  function previewAt(sec) {
    if (!S.playable) return;
    if (window.Player && typeof window.Player.playAlbumAt === "function") {
      window.Player.playAlbumAt(S.id, clamp(sec, 0, S.state.duration));
    }
  }

  function positionPlayhead() {
    if (!S || !S.dom.playhead) return;
    const ph = S.dom.playhead;
    const globalAudio = document.getElementById("player-audio");
    if (!globalAudio || !window.Player || typeof window.Player.nowPlaying !== "function") {
      ph.style.display = "none";
      return;
    }
    const np = window.Player.nowPlaying();
    if (np.concertId !== S.id || np.trackIdx !== null || globalAudio.paused) {
      ph.style.display = "none";
      return;
    }
    ph.style.display = "block";
    ph.style.left = pct(S.state, globalAudio.currentTime) + "%";
  }

  // ── Submit / reset ──────────────────────────────────────────────────────────

  async function submit() {
    const errors = validate(S.state);
    if (errors.length > 0) {
      setStatus(errors[0], "error");
      return;
    }
    await postJob(`/concerts/${S.id}/split-timestamps`, buildPayload(S.state), "Splitting…");
  }

  async function reset() {
    await postJob(`/concerts/${S.id}/split-timestamps/reset`, null, "Resetting to auto…");
  }

  // Re-fetch the saved timestamps and build editor state from them (or null if
  // there's nothing to split yet). Shared by revert() and resync().
  async function fetchState() {
    const resp = await fetch(`/concerts/${S.id}/split-timestamps`).then(okJson);
    return initState(resp);
  }

  // Rebuild the editor from the saved timestamps, throwing away the user's
  // unsaved in-editor edits. initState chooses resp.user || resp.auto (see
  // initState above), so this lands on the last *saved* times — NOT the
  // automated baseline that "Reset to auto" re-splits to.
  async function revert() {
    setBusy(true);
    setStatus("Discarding edits…");
    try {
      const state = await fetchState();
      if (!state) {
        setStatus("No saved times to restore.", "error");
        setBusy(false);
        return;
      }
      S.state = state;
      // Order is deliberate: render() runs syncDom → refreshValidity while
      // S.busy is still true, so refreshValidity's `if (!busy)` guard does not
      // overwrite the success status set below. Then clear busy (re-enables
      // the freshly-built buttons), then set the status.
      render();
      setBusy(false);
      setStatus("Restored the last saved times.", "ok");
    } catch (e) {
      setStatus("Could not load saved times — please retry.", "error");
      setBusy(false);
      console.warn("[Splitter] revert failed", e);
    }
  }

  async function postJob(url, body, runningMsg) {
    setBusy(true);
    setStatus(runningMsg);
    try {
      const opts = { method: "POST" };
      if (body) {
        opts.headers = { "Content-Type": "application/json" };
        opts.body = JSON.stringify(body);
      }
      const r = await fetch(url, opts);
      if (r.status === 202) {
        const data = await r.json().catch(() => ({}));
        setStatus("Splitting… the track list will update when it finishes.", "ok");
        refreshCard();
        return;
      }
      if (r.status === 200) {
        // reset no-op: already on auto timestamps.
        setStatus("Already using the automatic split.", "ok");
        setBusy(false);
        return;
      }
      // 409 busy, 422 validation, etc. — surface the server's message.
      const text = await r.text();
      setStatus(text || `Request failed (${r.status})`, "error");
      setBusy(false);
      if (r.status === 409) resync();
    } catch (e) {
      setStatus("Network error — please retry.", "error");
      setBusy(false);
      console.warn("[Splitter] postJob failed", e);
    }
  }

  function setBusy(busy) {
    S.busy = busy;
    if (S.submitBtn) S.submitBtn.disabled = busy || validate(S.state).length > 0;
    if (S.revertBtn) S.revertBtn.disabled = busy;
    if (S.resetBtn) S.resetBtn.disabled = busy;
  }

  // Refresh the concert card so its in-progress split badge + 3s polling kick in.
  function refreshCard() {
    const card = document.getElementById("concert-" + S.id);
    if (card && window.htmx && typeof window.htmx.ajax === "function") {
      window.htmx.ajax("GET", `/concerts/${S.id}/status`, {
        target: "#concert-" + S.id,
        swap: "outerHTML",
      });
    }
  }

  // Re-pull timestamps after a 409 so the editor reflects whatever the running
  // job is producing.
  async function resync() {
    try {
      const state = await fetchState();
      if (state) {
        S.state = state;
        render();
      }
    } catch (e) {
      /* keep current view */
    }
  }

  const api = {
    toggle,
    // Pure helpers exposed for unit tests (Node require()).
    _pure: {
      MIN_SEG,
      clamp,
      round3,
      parseTimecode,
      formatTimecode,
      initState,
      setStart,
      setEnd,
      detach,
      link,
      handlesFor,
      handleTime,
      applyHandle,
      validate,
      buildPayload,
    },
  };

  if (typeof module !== "undefined" && module.exports) module.exports = api;
  return api;
})();
