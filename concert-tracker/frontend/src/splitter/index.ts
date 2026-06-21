// Host glue for the splitter timeline editor. The Foldkit widget (./widget/)
// owns all editor state and rendering; this file is the thin imperative
// boundary that mounts/unmounts it via Runtime.embed and bridges its Ports
// to window.Player, #player-audio, and htmx — the same integration points
// the original hand-written splitter.ts used directly.
//
// Entry point is the inline Splitter.toggle(btn) onclick (see layout.html /
// concert_detail.html), so nothing here touches the DOM at module load time.
import { Runtime } from "foldkit";

import { makeElement, PLAYHEAD_HIDDEN } from "./widget";
// window.Player and window.htmx are declared ambiently by
// ../shared/player-api.ts and ../shared/globals.d.ts (picked up by tsc via
// tsconfig's "include", not by importing them — they emit no runtime code).

const embedWidget = (container: HTMLElement, concertId: number) =>
  Runtime.embed(makeElement(container, { concertId }));

interface SplitterSession {
  concertId: number;
  handle: ReturnType<typeof embedWidget>;
  playheadHandler: () => void;
}

// Per-open session state, mirroring the original module's `S` singleton:
// only one splitter panel is ever open at a time.
let session: SplitterSession | null = null;

/** The playhead position to push through the inbound Port: a 0–1 fraction of
 *  `audio.duration`, or `PLAYHEAD_HIDDEN` when nothing should be shown
 *  (paused, a different concert/track playing, or no global player). */
function playheadFractionFor(concertId: number, audio: HTMLMediaElement): number {
  if (!window.Player || audio.paused || !Number.isFinite(audio.duration) || audio.duration <= 0) {
    return PLAYHEAD_HIDDEN;
  }
  const nowPlaying = window.Player.nowPlaying();
  if (nowPlaying.concertId !== concertId || nowPlaying.trackIdx !== null) {
    return PLAYHEAD_HIDDEN;
  }
  return audio.currentTime / audio.duration;
}

// Refresh the concert card so its in-progress split badge + 3s polling kick
// in, mirroring the original module's refreshCard().
function refreshCard(concertId: number): void {
  const card = document.getElementById("concert-" + concertId);
  if (card && window.htmx) {
    window.htmx.ajax("GET", `/concerts/${concertId}/status`, {
      target: "#concert-" + concertId,
      swap: "outerHTML",
    });
  }
}

function open(container: HTMLElement, btn: HTMLButtonElement | undefined, concertId: number): void {
  container.classList.add("splitter-open");
  if (btn) btn.textContent = "Hide track splitter";

  // Runtime.embed takes ownership of the container it's given — it patches
  // the container's own attributes against the widget's root view, which
  // would strip the id/class CSS and toggle()/close() depend on (the same
  // reason the embedding example mounts into a dedicated empty slot rather
  // than a div the host also manages). Give the widget its own child mount
  // point instead of handing it #splitter directly.
  const mount = document.createElement("div");
  // Runtime.embed requires the container to have an id (used for HMR model
  // preservation) — without one it dies inside its own fiber, asynchronously
  // and silently, well after this synchronous call returns.
  mount.id = `splitter-widget-${concertId}`;
  container.replaceChildren(mount);
  const handle = embedWidget(mount, concertId);

  handle.ports.auditionAt.subscribe((time) => {
    void window.Player?.playAlbumAt(concertId, time);
  });
  handle.ports.cardDirty.subscribe(() => refreshCard(concertId));

  const globalAudio = document.getElementById("player-audio") as HTMLMediaElement | null;
  const playheadHandler = globalAudio
    ? () => handle.ports.playhead.send(playheadFractionFor(concertId, globalAudio))
    : () => undefined;
  if (globalAudio) globalAudio.addEventListener("timeupdate", playheadHandler);

  session = { concertId, handle, playheadHandler };
}

function close(container: HTMLElement, btn: HTMLButtonElement | undefined): void {
  if (session) {
    const globalAudio = document.getElementById("player-audio");
    if (globalAudio) globalAudio.removeEventListener("timeupdate", session.playheadHandler);
    session.handle.dispose();
  }
  // dispose() restores the mount div empty rather than removing it; drop it
  // so a reopened panel starts from a clean #splitter, same as the original.
  container.replaceChildren();
  container.classList.remove("splitter-open");
  if (btn) btn.textContent = "Edit track splits";
  session = null;
}

export function toggle(btn?: HTMLButtonElement): void {
  const container = document.getElementById("splitter");
  if (!container) return;
  if (container.classList.contains("splitter-open")) {
    close(container, btn);
    return;
  }
  const concertId = Number(container.getAttribute("data-concert-id"));
  if (Number.isNaN(concertId)) return;
  open(container, btn, concertId);
}

export interface SplitterApi {
  toggle(btn?: HTMLButtonElement): void;
}

const api: SplitterApi = { toggle };

declare global {
  interface Window {
    Splitter: SplitterApi;
  }
}

window.Splitter = api;
