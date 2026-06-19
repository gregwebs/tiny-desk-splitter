// window.Playlists's public API and the AddTarget union it accepts —
// declared here (rather than in playlists.ts) so player.ts's addToPlaylist()
// can reference `window.Playlists.openAdd(...)` without depending on
// playlists.ts directly (entries don't import each other — see build.mjs).
// playlists.ts is the implementer; its exported api object must stay
// assignable to PlaylistsApi below.
export type AddTarget =
  | { type: "track"; concertId: number; trackIndex: number; label?: string | null }
  | { type: "concert"; concertId: number; label?: string | null }
  | { type: "playlist"; childPlaylistId: number; label?: string | null };

export interface PlaylistsApi {
  createFromForm(event: Event): Promise<boolean>;
  editDetails(): void;
  cancelEdit(): void;
  saveDetails(event: Event, id: number): Promise<boolean>;
  deletePlaylist(id: number): Promise<void>;
  removeItem(playlistId: number, itemId: number): Promise<void>;
  openAdd(target: AddTarget): Promise<void>;
  closeAdd(): void;
  filterPlaylists(query: string): void;
  filterKeydown(event: KeyboardEvent): void;
}

declare global {
  interface Window {
    Playlists?: PlaylistsApi;
  }
}
