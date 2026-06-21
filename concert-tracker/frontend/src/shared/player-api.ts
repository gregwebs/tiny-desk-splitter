// window.Player's public API — the methods called from inline template
// handlers (layout.html, concert_card.html, tracks.html, ...) and from other
// entry points (splitter.ts uses playAlbumAt/nowPlaying; playlists.ts uses
// openSidebar/closeSidebar). player.ts is the implementer; its exported
// PlayerApi object must stay assignable to this interface — that's the
// contract every module sees via the `declare global` block below.
export interface PlayerNowPlaying {
  concertId: number | null;
  trackIdx: number | null;
}

export interface PlayerApi {
  playAlbum(btn: HTMLElement | null, concertId: number): Promise<void>;
  playTrack(btn: HTMLElement | null, concertId: number, trackIdx: number): Promise<void>;
  playTracks(btn: HTMLElement | null, concertId: number): Promise<void>;
  startAlbum(btn: HTMLElement | null, concertId: number, recordListen?: boolean): Promise<boolean>;
  startTrack(btn: HTMLElement | null, concertId: number, trackIdx: number): Promise<boolean>;
  togglePause(): void;
  seek(val: string | number): void;
  skipToNext(): Promise<void>;
  skipToPrev(): Promise<void>;
  watch(): void;
  openExternal(): Promise<void>;
  watchTrackDirect(btn: HTMLElement | null, concertId: number, trackIdx: number): Promise<void>;
  toggleLike(): Promise<void>;
  deleteTrack(): Promise<void>;
  openConcert(e?: Event): void;
  openSidebar(): void;
  closeSidebar(): void;
  toggleSidebar(): void;
  sidebarDeleteTrack(concertId: number, trackIdx: number): Promise<void>;
  playQueueEntryNow(pos: number): void;
  dequeue(pos: number): void;
  enqueue(concertId: number, trackIdx: number, title: string, liked: boolean): void;
  /** Start whole-album playback for `concertId`, seeking to `seconds`. Used by
   * the splitter timeline's preview (click-to-seek, audition buttons). */
  playAlbumAt(concertId: number, seconds: number): Promise<void>;
  /** Current playback position, used by the splitter to draw its playhead. */
  nowPlaying(): PlayerNowPlaying;
  playPlaylist(playlistId: number): Promise<void>;
  addToPlaylist(): void;
  stopPlayback(): void;
  playConcert(id: number): Promise<void>;
  playConcertFrom(id: number, pos: number): Promise<void>;
  sidebarDeleteInterlude(concertId: number, interludeIdx: number): Promise<void>;
}

declare global {
  interface Window {
    Player?: PlayerApi;
  }
}
