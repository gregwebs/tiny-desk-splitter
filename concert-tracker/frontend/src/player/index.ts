// Player entry point (build entry `player` → static/player.js).
//
// Mounts the Foldkit player widget into #player-root and exposes
// window.Player as a thin synchronous shim that forwards every call to the
// widget's inbound command Port.
//
// Design constraints:
//  - `openSidebar()` MUST write body.classList synchronously before the async
//    send, because playlists/index.ts captures `sidebarWasOpen` before calling
//    it (see playlists/index.ts:openAdd and ./widget/port.ts's OpenSidebar doc).
//  - `nowPlaying()` is synchronous (splitter reads it inside a timeupdate
//    handler); satisfied by the module-scoped mirror in ./mirror.ts.
//  - All other return values (Promise<void>/Promise<boolean>/void) are either
//    fire-and-forget or have no callers reading their resolved values.
import { Runtime } from "foldkit";

import type { PlayerApi } from "../shared/player-api";
import { nativeClickShouldWin } from "./core";
import { nowPlaying as mirrorNowPlaying } from "./mirror";
import { type PlayerCommand, PlayerCommandValue } from "./widget/port";
import { makeElement } from "./widget/widget";

const root = document.getElementById("player-root");
const handle = root ? Runtime.embed(makeElement(root)) : null;

const send = (cmd: PlayerCommand) => {
  handle?.ports.command.send(cmd);
};

window.Player = {
  playAlbum(_btn, concertId) {
    send(PlayerCommandValue.PlayAlbum({ concertId }));
    return Promise.resolve();
  },

  playTrack(_btn, concertId, trackIdx) {
    send(PlayerCommandValue.PlayTrack({ concertId, trackIdx }));
    return Promise.resolve();
  },

  playTracks(_btn, concertId) {
    send(PlayerCommandValue.PlayTracks({ concertId }));
    return Promise.resolve();
  },

  startAlbum(_btn, concertId, recordListen = true) {
    send(PlayerCommandValue.StartAlbum({ concertId, recordListen }));
    return Promise.resolve(true);
  },

  startTrack(_btn, concertId, trackIdx) {
    send(PlayerCommandValue.StartTrack({ concertId, trackIdx }));
    return Promise.resolve(true);
  },

  togglePause() {
    send(PlayerCommandValue.TogglePause());
  },

  seek(val) {
    const seconds = typeof val === "string" ? parseFloat(val) : val;
    if (!isNaN(seconds)) send(PlayerCommandValue.Seek({ seconds }));
  },

  skipToNext() {
    send(PlayerCommandValue.SkipToNext());
    return Promise.resolve();
  },

  skipToPrev() {
    send(PlayerCommandValue.SkipToPrev());
    return Promise.resolve();
  },

  watch() {
    send(PlayerCommandValue.Watch());
  },

  openExternal() {
    send(PlayerCommandValue.OpenExternal());
    return Promise.resolve();
  },

  watchTrackDirect(_btn, concertId, trackIdx) {
    send(PlayerCommandValue.WatchTrackDirect({ concertId, trackIdx }));
    return Promise.resolve();
  },

  toggleLike() {
    send(PlayerCommandValue.ToggleLike());
    return Promise.resolve();
  },

  deleteTrack() {
    send(PlayerCommandValue.DeleteTrack());
    return Promise.resolve();
  },

  // #player-artist's onclick (view.ts). Navigation stays entirely in the host
  // shim rather than round-tripping through the widget's command Port: it
  // needs the click event (modifier-key/aux-button checks, preventDefault)
  // and the anchor itself as htmx's `source` (so htmx reads that element's
  // hx-target/hx-select/hx-swap/hx-push-url and does a partial #content swap
  // instead of a full-page navigation that would kill playback).
  openConcert(e) {
    if (!(e instanceof MouseEvent) || !(e.currentTarget instanceof HTMLAnchorElement)) return;
    const anchor = e.currentTarget;
    if (nativeClickShouldWin(e, anchor)) return;
    const href = anchor.getAttribute("href");
    if (!href || href === "#" || !window.htmx) return; // nothing playing → inert link
    e.preventDefault();
    window.htmx.ajax("GET", href, { source: anchor });
  },

  // Must write body class synchronously so playlists/index.ts:openAdd() sees
  // the correct sidebarWasOpen value when it captures state before this call.
  openSidebar() {
    document.body.classList.add("sidebar-open");
    send(PlayerCommandValue.OpenSidebar());
  },

  closeSidebar() {
    document.body.classList.remove("sidebar-open");
    send(PlayerCommandValue.CloseSidebar());
  },

  toggleSidebar() {
    send(PlayerCommandValue.ToggleSidebar());
  },

  sidebarDeleteTrack(concertId, trackIdx) {
    send(PlayerCommandValue.SidebarDeleteTrack({ concertId, trackIdx }));
    return Promise.resolve();
  },

  playQueueEntryNow(pos) {
    send(PlayerCommandValue.PlayQueueEntryNow({ pos }));
  },

  dequeue(pos) {
    send(PlayerCommandValue.Dequeue({ pos }));
  },

  enqueue(concertId, trackIdx, title, liked) {
    send(PlayerCommandValue.Enqueue({ concertId, trackIdx, title, liked }));
  },

  playAlbumAt(concertId, seconds) {
    send(PlayerCommandValue.PlayAlbumAt({ concertId, seconds }));
    return Promise.resolve();
  },

  nowPlaying() {
    return mirrorNowPlaying();
  },

  playPlaylist(playlistId) {
    send(PlayerCommandValue.PlayPlaylist({ playlistId }));
    return Promise.resolve();
  },

  addToPlaylist() {
    send(PlayerCommandValue.AddToPlaylist());
  },

  stopPlayback() {
    send(PlayerCommandValue.StopPlayback());
  },

  playConcert(id) {
    send(PlayerCommandValue.PlayConcert({ concertId: id }));
    return Promise.resolve();
  },

  playConcertFrom(id, pos) {
    send(PlayerCommandValue.PlayConcertFrom({ concertId: id, pos }));
    return Promise.resolve();
  },

  sidebarDeleteInterlude(concertId, interludeIdx) {
    send(PlayerCommandValue.SidebarDeleteInterlude({ concertId, interludeIdx }));
    return Promise.resolve();
  },
} satisfies PlayerApi;
