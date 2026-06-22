import { Schema as S } from "effect";
import { Port } from "foldkit";

import { AddTarget } from "./model";

// PORT
//
// The add panel lives in the page-lifetime #player-sidebar, opened/closed by
// the host (and by player.ts). The widget is mounted once and driven through
// these Ports rather than mounted/disposed per open.

export const ports = {
  inbound: {
    /** Host opened the panel for a target (Playlists.openAdd, called by the
     *  templates and player.ts). */
    opened: Port.inbound(AddTarget),
    /** Host detected the sidebar was closed externally (its MutationObserver)
     *  and is telling us to reset. */
    closed: Port.inbound(S.Void),
    /** Host resolved the empty-state prompt() with a non-blank name. */
    newName: Port.inbound(S.String),
  },
  outbound: {
    /** Ask the host to tear down the sidebar chrome (remove showing-add,
     *  Player.closeSidebar() unless the sidebar was already open). */
    requestClose: Port.outbound(S.Void),
    /** Ask the host to prompt() for a new playlist name (the empty state has
     *  no filter text to use); the host replies via the inbound `newName`. */
    requestNewName: Port.outbound(S.Void),
  },
};
