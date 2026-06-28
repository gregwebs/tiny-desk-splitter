import { Schema as S } from "effect";
import { Port } from "foldkit";
import { ts } from "foldkit/schema";

// PORT
//
// window.Player's 29 methods (minus nowPlaying, a synchronous mirror read —
// see ../mirror.ts) become one tagged union flowing through a single inbound
// Port, dispatched inside update via Match.tagsExhaustive (CommandReceived in
// ./message.ts), rather than 28 named Ports. `btn: HTMLElement | null`
// arguments are dropped from every variant — DOM element references can't
// cross a Schema-encoded Port boundary. Where `btn` fed user-visible text
// (e.g. preparePlay's "Preparing “<button text>”…"), the host shim
// (commit 7) must read it before emitting and pass the text along instead;
// where it was purely a best-effort visual marker with no decision content
// (play()'s unused `_btn`, startAlbum/startTrack's error-class toggle), it is
// simply dropped — see ./command.ts's MarkPreparingExternal /
// DisableCardTracksExternal, which look the element up by data-attribute
// instead of taking a reference.
//
// No outbound Port: the widget reaches htmx/DOM/fetch directly via
// Effect.sync/Effect.tryPromise Commands (../command.ts), same as the rest of
// this bundle already does (api/client, window.htmx).

const PlayAlbum = ts("PlayAlbum", { concertId: S.Number });
const PlayTrack = ts("PlayTrack", { concertId: S.Number, trackIdx: S.Number });
const PlayTracks = ts("PlayTracks", { concertId: S.Number });
const StartAlbum = ts("StartAlbum", { concertId: S.Number, recordListen: S.Boolean });
const StartTrack = ts("StartTrack", { concertId: S.Number, trackIdx: S.Number });
const TogglePause = ts("TogglePause");
/** `val` is `string | number` in PlayerApi (HTML range-input `.value` is a
 *  string); the host shim normalizes to a number before emitting, since a
 *  Schema union purely for that coercion buys nothing. */
const Seek = ts("Seek", { seconds: S.Number });
const SkipToNext = ts("SkipToNext");
const SkipToPrev = ts("SkipToPrev");
const Watch = ts("Watch");
const OpenExternal = ts("OpenExternal");
const WatchTrackDirect = ts("WatchTrackDirect", { concertId: S.Number, trackIdx: S.Number });
const ToggleLike = ts("ToggleLike");
const DeleteTrack = ts("DeleteTrack");
/** player.ts's `openConcert(e?)` reads the event for the modifier-key skip
 *  (meta/ctrl/shift → let the native href win) and `preventDefault()`/the
 *  htmx `source` element; none of that can cross the Port boundary, so the
 *  host shim resolves the modifier-key check itself and only emits this when
 *  it has decided to proceed. */
const OpenConcert = ts("OpenConcert");
const OpenSidebar = ts("OpenSidebar");
const CloseSidebar = ts("CloseSidebar");
const ToggleSidebar = ts("ToggleSidebar");
const SidebarDeleteTrack = ts("SidebarDeleteTrack", { concertId: S.Number, trackIdx: S.Number });
const PlayQueueEntryNow = ts("PlayQueueEntryNow", { pos: S.Number });
const Dequeue = ts("Dequeue", { pos: S.Number });
const RemoveGroup = ts("RemoveGroup", { groupId: S.Number });
const Enqueue = ts("Enqueue", {
  concertId: S.Number,
  trackIdx: S.Number,
  title: S.String,
  liked: S.Boolean,
});
const PlayAlbumAt = ts("PlayAlbumAt", { concertId: S.Number, seconds: S.Number });
const PlayPlaylist = ts("PlayPlaylist", { playlistId: S.Number });
const AddToPlaylist = ts("AddToPlaylist");
const StopPlayback = ts("StopPlayback");
const PlayConcert = ts("PlayConcert", { concertId: S.Number });
const PlayConcertFrom = ts("PlayConcertFrom", { concertId: S.Number, pos: S.Number });
const SidebarDeleteInterlude = ts("SidebarDeleteInterlude", { concertId: S.Number, interludeIdx: S.Number });
/** Like/unlike a specific track from the sidebar track list (whole-album or
 *  reconstruction mode). Syncs bar star and concert items in the Model so all
 *  copies stay in sync without an extra round trip. */
const SidebarLikeTrack = ts("SidebarLikeTrack", { concertId: S.Number, trackIdx: S.Number });
/** Open the add-to-playlist panel for a specific sidebar track row (distinct
 *  from AddToPlaylist which uses the currently-playing track from playback). */
const SidebarAddToPlaylist = ts("SidebarAddToPlaylist", {
  concertId: S.Number,
  trackIdx: S.Number,
  label: S.String,
});

export const PlayerCommand = S.Union([
  PlayAlbum,
  PlayTrack,
  PlayTracks,
  StartAlbum,
  StartTrack,
  TogglePause,
  Seek,
  SkipToNext,
  SkipToPrev,
  Watch,
  OpenExternal,
  WatchTrackDirect,
  ToggleLike,
  DeleteTrack,
  OpenConcert,
  OpenSidebar,
  CloseSidebar,
  ToggleSidebar,
  SidebarDeleteTrack,
  PlayQueueEntryNow,
  Dequeue,
  RemoveGroup,
  Enqueue,
  PlayAlbumAt,
  PlayPlaylist,
  AddToPlaylist,
  StopPlayback,
  PlayConcert,
  PlayConcertFrom,
  SidebarDeleteInterlude,
  SidebarLikeTrack,
  SidebarAddToPlaylist,
]);
export type PlayerCommand = typeof PlayerCommand.Type;

export const PlayerCommandValue = {
  PlayAlbum,
  PlayTrack,
  PlayTracks,
  StartAlbum,
  StartTrack,
  TogglePause,
  Seek,
  SkipToNext,
  SkipToPrev,
  Watch,
  OpenExternal,
  WatchTrackDirect,
  ToggleLike,
  DeleteTrack,
  OpenConcert,
  OpenSidebar,
  CloseSidebar,
  ToggleSidebar,
  SidebarDeleteTrack,
  PlayQueueEntryNow,
  Dequeue,
  RemoveGroup,
  Enqueue,
  PlayAlbumAt,
  PlayPlaylist,
  AddToPlaylist,
  StopPlayback,
  PlayConcert,
  PlayConcertFrom,
  SidebarDeleteInterlude,
  SidebarLikeTrack,
  SidebarAddToPlaylist,
};

export const ports = {
  inbound: {
    command: Port.inbound(PlayerCommand),
  },
};
