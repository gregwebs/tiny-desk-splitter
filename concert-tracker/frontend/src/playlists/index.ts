// Playlist UI entry point (build entry `playlists` -> static/playlists.js).
// Assembles window.Playlists from the imperative list/detail/drag handlers in
// ./pages plus the add-to-playlist sidebar panel, which is now a Foldkit MVU
// widget (./widget) mounted via Runtime.embed. This file is the thin host glue
// between window.Playlists / window.Player / the #player-sidebar DOM and the
// widget's typed Ports.
//
// The add panel lives in the page-lifetime #player-sidebar (a sibling of
// #content, so it survives hx-boost swaps). Unlike the splitter — mounted and
// disposed per toggle — this widget is mounted once on first openAdd and kept
// for the page lifetime; open/close flows over its Ports.
import { Runtime } from "foldkit";

import { byIdOrNull } from "../shared/dom";
import type { AddTarget, PlaylistsApi } from "../shared/playlists-api";
import {
  cancelEdit,
  createFromForm,
  deletePlaylist,
  editDetails,
  removeItem,
  saveDetails,
  trace,
} from "./pages";
import { makeElement } from "./widget";
// window.Player is declared ambiently by ../shared/player-api.ts.

const embedWidget = (container: HTMLElement) => Runtime.embed(makeElement(container));
type WidgetHandle = ReturnType<typeof embedWidget>;

// The single embedded widget, mounted lazily on the first openAdd and kept for
// the page lifetime. null until then.
let handle: WidgetHandle | null = null;
// Whether the sidebar was already open when the panel was opened. Host-owned
// (re-captured on every openAdd, before the sidebar is forced open), so closing
// the panel restores the prior sidebar state.
let sidebarWasOpen = false;

// Remove the add-panel chrome and restore the prior sidebar state. Shared by
// the widget's outbound requestClose (its "×" / Enter-on-empty) and closeAdd.
function tearDownChrome(): void {
  const sidebar = byIdOrNull("player-sidebar");
  if (sidebar) sidebar.classList.remove("showing-add");
  if (!sidebarWasOpen) window.Player?.closeSidebar();
}

// Mount the widget once into a dedicated child of #sidebar-add-section.
// Runtime.embed requires the container to have a non-empty id (else it dies
// inside its own fiber) and takes ownership of the container's attributes, so
// we never hand it the section element itself.
function ensureMounted(): WidgetHandle | null {
  if (handle) return handle;
  const section = byIdOrNull("sidebar-add-section");
  if (!section) return null;
  let mount = byIdOrNull("add-pl-widget-root");
  if (!mount) {
    mount = document.createElement("div");
    mount.id = "add-pl-widget-root";
    section.appendChild(mount);
  }
  const h = embedWidget(mount);
  h.ports.requestClose.subscribe(() => tearDownChrome());
  h.ports.requestNewName.subscribe(() => {
    const name = prompt("New playlist name:");
    if (name && name.trim()) h.ports.newName.send(name.trim());
  });
  handle = h;
  return h;
}

// Detect the sidebar being closed externally (player.ts removes sidebar-open
// from body, e.g. its close "×" or Player.toggleSidebar) while the add panel is
// showing, and tell the widget to reset. Only fires when showing-add is present
// AND sidebar-open was removed — matching the old MutationObserver's contract.
new MutationObserver(() => {
  const sidebar = byIdOrNull("player-sidebar");
  if (
    sidebar?.classList.contains("showing-add") &&
    !document.body.classList.contains("sidebar-open")
  ) {
    sidebar.classList.remove("showing-add");
    handle?.ports.closed.send(undefined);
  }
}).observe(document.body, { attributes: true, attributeFilter: ["class"] });

function openAdd(target: AddTarget): Promise<void> {
  trace("openAdd", target);
  // Capture the prior sidebar state BEFORE opening it (every call).
  sidebarWasOpen = document.body.classList.contains("sidebar-open");
  // Open the sidebar (Player is the single owner of sidebar-open state) and
  // swap to the add panel.
  window.Player?.openSidebar();
  const sidebar = byIdOrNull("player-sidebar");
  if (sidebar) sidebar.classList.add("showing-add");
  const h = ensureMounted();
  h?.ports.opened.send(target);
  return Promise.resolve();
}

function closeAdd(): void {
  trace("closeAdd");
  tearDownChrome();
  handle?.ports.closed.send(undefined);
}

const api: PlaylistsApi = {
  createFromForm,
  editDetails,
  cancelEdit,
  saveDetails,
  deletePlaylist,
  removeItem,
  openAdd,
  closeAdd,
};

window.Playlists = api;
