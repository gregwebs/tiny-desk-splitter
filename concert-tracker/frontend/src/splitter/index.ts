// Splitter timeline UI for editing per-track split points on the concert detail
// page. Ported from the original static/splitter.js DOM module; pure editor
// logic lives in ./core.ts (unit-tested directly, see js-tests/splitter.test.ts).
//
// The backend API (see docs/change/2026-06-12-user-split-timestamps.md):
//   GET  /concerts/:id/split-timestamps        -> SplitTimestampsResponse
//   POST /concerts/:id/split-timestamps        TimestampPayload
//   POST /concerts/:id/split-timestamps/reset
//
// Entry point is the inline Splitter.toggle(btn) onclick (see layout.html /
// concert_detail.html), so nothing here touches the DOM at module load time.
import {
  applyHandle,
  buildPayload,
  detach,
  formatTimecode,
  handlesFor,
  handleTime,
  initState,
  link,
  parseTimecode,
  setEnd,
  setStart,
  validate,
  type EditorState,
  type Handle,
} from "./core";
import {
  getJson,
  getJsonOrNull,
  sendJson,
  type MediaInfo,
  type SplitTimestampsResponse,
} from "../api/client";
// window.Player and window.htmx are declared ambiently by
// ../shared/player-api.ts and ../shared/globals.d.ts (picked up by tsc via
// tsconfig's "include", not by importing them — they emit no runtime code).

type HandleEl = HTMLDivElement & { _h: Handle };

interface SplitterDom {
  timeline: HTMLDivElement;
  playhead: HTMLDivElement;
  segs: HTMLDivElement[];
  gaps: HTMLDivElement[];
  handles: HandleEl[];
  rows: { startInput: HTMLInputElement; endInput: HTMLInputElement }[];
  boundaryBtns: HTMLButtonElement[];
}

interface SplitterSession {
  id: number;
  container: HTMLElement;
  state: EditorState | null;
  mediaUrl: string | null;
  playable: boolean;
  dom: SplitterDom | null;
  busy: boolean;
  globalPlayheadHandler: (() => void) | null;
  statusEl: HTMLSpanElement | null;
  submitBtn: HTMLButtonElement | null;
  revertBtn: HTMLButtonElement | null;
  resetBtn: HTMLButtonElement | null;
}

// Per-open session state.
let S: SplitterSession | null = null;

/** Non-null `S`, for code that only runs while a session is open. */
function session(): SplitterSession {
  if (!S) throw new Error("Splitter: no active session");
  return S;
}

/** Non-null `s.dom`, for code that only runs after render() has built it. */
function activeDom(s: SplitterSession): SplitterDom {
  if (!s.dom) throw new Error("Splitter: render() has not run yet");
  return s.dom;
}

function pct(state: EditorState, t: number): number {
  return state.duration > 0 ? (t / state.duration) * 100 : 0;
}

function setStatus(msg: string, kind?: "ok" | "error"): void {
  if (!S || !S.statusEl) return;
  S.statusEl.textContent = msg || "";
  S.statusEl.className = "splitter-status" + (kind ? " splitter-status-" + kind : "");
}

export async function toggle(btn?: HTMLButtonElement): Promise<void> {
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

function close(container: Element, btn?: HTMLButtonElement): void {
  if (S && S.globalPlayheadHandler) {
    const globalAudio = document.getElementById("player-audio");
    if (globalAudio) globalAudio.removeEventListener("timeupdate", S.globalPlayheadHandler);
  }
  container.classList.remove("splitter-open");
  container.innerHTML = "";
  if (btn) btn.textContent = "Edit track splits";
  S = null;
}

async function open(container: HTMLElement): Promise<void> {
  const id = Number(container.getAttribute("data-concert-id"));
  container.innerHTML = '<p class="splitter-status">Loading…</p>';
  S = {
    id,
    container,
    state: null,
    mediaUrl: null,
    playable: false,
    dom: null,
    busy: false,
    globalPlayheadHandler: null,
    statusEl: null,
    submitBtn: null,
    revertBtn: null,
    resetBtn: null,
  };
  try {
    const [tsResp, mediaResp] = await Promise.all([
      getJson<SplitTimestampsResponse>(`/concerts/${id}/split-timestamps`),
      getJsonOrNull<MediaInfo>(`/concerts/${id}/media-info`),
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

// Build the whole UI once for the current topology, then position everything.
function render(): void {
  const s = session();
  const st = s.state;
  if (!st) return; // unreachable: open() only calls render() once state is set
  const c = s.container;
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
  s.statusEl = status;
  s.submitBtn = submitBtn;
  s.revertBtn = revertBtn;
  s.resetBtn = resetBtn;

  // Timeline.
  const timeline = el("div", "splitter-timeline");
  timeline.addEventListener("pointerdown", onTimelineSeek);
  const playhead = el("div", "splitter-playhead");
  timeline.appendChild(playhead);
  const dom: SplitterDom = {
    timeline,
    playhead,
    segs: [],
    gaps: [],
    handles: [],
    rows: [],
    boundaryBtns: [],
  };
  s.dom = dom;

  st.tracks.forEach((t, i) => {
    const seg = el("div", "splitter-seg");
    seg.title = t.title;
    seg.appendChild(el("span", "splitter-seg-label", `${i + 1}. ${t.title}`));
    timeline.appendChild(seg);
    dom.segs.push(seg);
  });
  for (let i = 0; i < st.tracks.length - 1; i++) {
    const gap = el("div", "splitter-gap");
    timeline.appendChild(gap);
    dom.gaps.push(gap);
  }
  handlesFor(st).forEach((h) => {
    const handle = el("div", "splitter-handle splitter-handle-" + h.kind) as HandleEl;
    handle._h = h;
    handle.addEventListener("pointerdown", (e) => onHandleDown(e, handle));
    timeline.appendChild(handle);
    dom.handles.push(handle);
  });

  // Preview note for non-playable sources.
  let previewNote: HTMLParagraphElement | null = null;
  if (!s.playable) {
    previewNote = el(
      "p",
      "splitter-note",
      s.mediaUrl
        ? "Audio preview unavailable for this file format."
        : "Audio preview unavailable — source file not found.",
    );
  }

  // Boundary detach/link controls.
  const boundaries = el("div", "splitter-boundaries");
  for (let i = 0; i < st.tracks.length - 1; i++) {
    const row = el("div", "splitter-boundary");
    const btn = el("button", "splitter-detach");
    btn.type = "button";
    btn.dataset.boundary = String(i);
    btn.addEventListener("click", () => toggleBoundary(i));
    row.append(
      el("span", "splitter-boundary-label", `${st.tracks[i]!.title} → ${st.tracks[i + 1]!.title}`),
      btn,
    );
    boundaries.appendChild(row);
    dom.boundaryBtns.push(btn);
  }

  // Numeric table.
  const table = el("table", "splitter-table");
  const tbody = el("tbody");
  st.tracks.forEach((t, i) => {
    const tr = el("tr");
    tr.appendChild(el("td", "splitter-num", String(i + 1)));
    tr.appendChild(el("td", "splitter-title", t.title));
    const startCell = el("td");
    const startInput = inputFor(i, "start");
    const startPlay = previewBtn(() => previewAt(st.tracks[i]!.start));
    startCell.append(startInput, startPlay);
    const endCell = el("td");
    const endInput = inputFor(i, "end");
    const endPlay = previewBtn(() => previewAt(Math.max(0, st.tracks[i]!.end - 3)));
    endCell.append(endInput, endPlay);
    tr.append(startCell, endCell);
    tbody.appendChild(tr);
    dom.rows.push({ startInput, endInput });
  });
  const thead = el("thead");
  const htr = el("tr");
  ["#", "Track", "Start", "End (▶ auditions last 3s)"].forEach((h) =>
    htr.appendChild(el("th", null, h)),
  );
  thead.appendChild(htr);
  table.append(thead, tbody);

  c.append(toolbar, timeline);
  if (previewNote) c.appendChild(previewNote);
  c.append(boundaries, table);
  syncDom();
}

function inputFor(i: number, kind: "start" | "end"): HTMLInputElement {
  const input = el("input", "splitter-time");
  input.type = "text";
  input.inputMode = "decimal";
  input.addEventListener("change", () => onInputChange(i, kind, input));
  return input;
}

function previewBtn(fn: () => void): HTMLButtonElement {
  const b = el("button", "splitter-play", "▶");
  b.type = "button";
  b.title = "Play from here";
  if (session().playable) b.addEventListener("click", fn);
  else b.disabled = true;
  return b;
}

function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  cls?: string | null,
  text?: string | null,
): HTMLElementTagNameMap[K] {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text != null) e.textContent = text;
  return e;
}

// Reposition segments, gaps, handles, inputs, and boundary buttons from state
// without rebuilding the DOM (safe to call mid-drag).
function syncDom(): void {
  const s = session();
  const st = s.state;
  if (!st) return;
  const dom = activeDom(s);
  st.tracks.forEach((t, i) => {
    const seg = dom.segs[i]!;
    seg.style.left = pct(st, t.start) + "%";
    seg.style.width = pct(st, t.end - t.start) + "%";
  });
  for (let i = 0; i < st.tracks.length - 1; i++) {
    const gap = dom.gaps[i]!;
    const gapStart = st.tracks[i]!.end;
    const gapEnd = st.tracks[i + 1]!.start;
    const w = gapEnd - gapStart;
    if (w > 1e-6) {
      gap.style.display = "block";
      gap.style.left = pct(st, gapStart) + "%";
      gap.style.width = pct(st, w) + "%";
    } else {
      gap.style.display = "none";
    }
  }
  dom.handles.forEach((handle) => {
    handle.style.left = pct(st, handleTime(st, handle._h)) + "%";
  });
  dom.rows.forEach((row, i) => {
    if (document.activeElement !== row.startInput) {
      row.startInput.value = formatTimecode(st.tracks[i]!.start);
    }
    if (document.activeElement !== row.endInput) {
      row.endInput.value = formatTimecode(st.tracks[i]!.end);
    }
  });
  dom.boundaryBtns.forEach((btn, i) => {
    btn.textContent = st.linked[i] ? "Detach (add gap)" : "Link (remove gap)";
  });
  refreshValidity();
}

function refreshValidity(): void {
  const s = session();
  const st = s.state;
  if (!st) return;
  const errors = validate(st);
  const busy = s.busy;
  if (s.submitBtn) s.submitBtn.disabled = busy || errors.length > 0;
  if (s.revertBtn) s.revertBtn.disabled = busy;
  if (s.resetBtn) s.resetBtn.disabled = busy;
  if (!busy) {
    const first = errors[0];
    if (first) setStatus(first, "error");
    else setStatus("");
  }
}

// ── Interaction ─────────────────────────────────────────────────────────────

function timeFromClientX(clientX: number): number {
  const s = session();
  const st = s.state;
  if (!st) return 0;
  const rect = activeDom(s).timeline.getBoundingClientRect();
  const frac = Math.min(1, Math.max(0, (clientX - rect.left) / rect.width));
  return frac * st.duration;
}

function onHandleDown(e: PointerEvent, handle: HandleEl): void {
  e.preventDefault();
  e.stopPropagation(); // don't trigger timeline seek
  handle.setPointerCapture(e.pointerId);
  handle.classList.add("dragging");
  const move = (ev: PointerEvent) => {
    const s = session();
    if (s.state) applyHandle(s.state, handle._h, timeFromClientX(ev.clientX));
    syncDom();
  };
  const up = () => {
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

function onTimelineSeek(e: PointerEvent): void {
  if (!session().playable) return;
  previewAt(timeFromClientX(e.clientX));
}

function onInputChange(i: number, kind: "start" | "end", input: HTMLInputElement): void {
  const s = session();
  const st = s.state;
  if (!st) return;
  const v = parseTimecode(input.value);
  if (!Number.isFinite(v)) {
    // Reject: restore the prior value.
    input.value = formatTimecode(kind === "start" ? st.tracks[i]!.start : st.tracks[i]!.end);
    setStatus("Enter a time like 2:05.0", "error");
    return;
  }
  if (kind === "start") setStart(st, i, v);
  else setEnd(st, i, v);
  syncDom();
}

function toggleBoundary(i: number): void {
  const st = session().state;
  if (!st) return;
  if (st.linked[i]) detach(st, i);
  else link(st, i);
  render(); // topology changed: rebuild handles/gaps
}

// ── Preview audio ───────────────────────────────────────────────────────────

function previewAt(sec: number): void {
  const s = session();
  if (!s.playable || !s.state) return;
  window.Player?.playAlbumAt(s.id, Math.min(s.state.duration, Math.max(0, sec)));
}

function positionPlayhead(): void {
  if (!S || !S.dom) return;
  const ph = S.dom.playhead;
  const globalAudio = document.getElementById("player-audio") as HTMLMediaElement | null;
  if (!globalAudio || !window.Player || !S.state) {
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

async function submit(): Promise<void> {
  const st = session().state;
  if (!st) return;
  const errors = validate(st);
  const first = errors[0];
  if (first) {
    setStatus(first, "error");
    return;
  }
  await postJob(`/concerts/${session().id}/split-timestamps`, buildPayload(st), "Splitting…");
}

async function reset(): Promise<void> {
  await postJob(`/concerts/${session().id}/split-timestamps/reset`, undefined, "Resetting to auto…");
}

// Re-fetch the saved timestamps and build editor state from them (or null if
// there's nothing to split yet). Shared by revert() and resync().
async function fetchState(): Promise<EditorState | null> {
  const resp = await getJson<SplitTimestampsResponse>(
    `/concerts/${session().id}/split-timestamps`,
  );
  return initState(resp);
}

// Rebuild the editor from the saved timestamps, throwing away the user's
// unsaved in-editor edits. initState chooses resp.user || resp.auto (see
// initState above), so this lands on the last *saved* times — NOT the
// automated baseline that "Reset to auto" re-splits to.
async function revert(): Promise<void> {
  setBusy(true);
  setStatus("Discarding edits…");
  try {
    const state = await fetchState();
    if (!state) {
      setStatus("No saved times to restore.", "error");
      setBusy(false);
      return;
    }
    session().state = state;
    // Order is deliberate: render() runs syncDom -> refreshValidity while
    // busy is still true, so refreshValidity's `if (!busy)` guard does not
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

async function postJob(url: string, body: unknown, runningMsg: string): Promise<void> {
  setBusy(true);
  setStatus(runningMsg);
  try {
    const r = await sendJson(url, body, "POST");
    if (r.status === 202) {
      await r.json().catch(() => ({}));
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

function setBusy(busy: boolean): void {
  const s = session();
  s.busy = busy;
  if (s.submitBtn) s.submitBtn.disabled = busy || (s.state ? validate(s.state).length > 0 : true);
  if (s.revertBtn) s.revertBtn.disabled = busy;
  if (s.resetBtn) s.resetBtn.disabled = busy;
}

// Refresh the concert card so its in-progress split badge + 3s polling kick in.
function refreshCard(): void {
  const s = session();
  const card = document.getElementById("concert-" + s.id);
  if (card && window.htmx) {
    window.htmx.ajax("GET", `/concerts/${s.id}/status`, {
      target: "#concert-" + s.id,
      swap: "outerHTML",
    });
  }
}

// Re-pull timestamps after a 409 so the editor reflects whatever the running
// job is producing.
async function resync(): Promise<void> {
  try {
    const state = await fetchState();
    if (state) {
      session().state = state;
      render();
    }
  } catch {
    /* keep current view */
  }
}

export interface SplitterApi {
  toggle(btn?: HTMLButtonElement): Promise<void>;
}

const api: SplitterApi = { toggle };

declare global {
  interface Window {
    Splitter: SplitterApi;
  }
}

window.Splitter = api;
