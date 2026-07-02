// Imperative playlist UI that stays hand-written (no Foldkit): the /playlists
// list page (create), the /playlists/:id detail page (edit / delete / remove
// item / drag-drop reorder). The add-to-playlist sidebar panel is a Foldkit
// widget instead (see ./widget) and is wired up by ./index.ts, which also
// assembles window.Playlists from the functions exported here.
//
// The app uses hx-boost, so page bodies are swapped into #content rather than
// reloaded. All interaction is therefore wired via event delegation on
// `document` (so it survives swaps) or via inline onclick/onsubmit in the
// templates -- never via one-shot DOMContentLoaded listeners that a boost swap
// would bypass.
import {
  type CreatedPlaylistJson,
  createPlaylist,
  deletePlaylist as apiDeletePlaylist,
  readJson,
  removePlaylistItem,
  reorderPlaylistItems,
  updatePlaylist,
} from "../api/client";
import { byIdOfOrNull, byIdOrNull } from "../shared/dom";

declare global {
  interface Window {
    PLAYLIST_DEBUG?: boolean;
  }
}

export function trace(...args: unknown[]): void {
  if (window.PLAYLIST_DEBUG) console.debug("[playlists]", ...args);
}

// -- /playlists list page ----------------------------------------------------

export async function createFromForm(event: Event): Promise<boolean> {
  event.preventDefault();
  const input = byIdOfOrNull("new-playlist-name", HTMLInputElement);
  const name = input ? input.value.trim() : "";
  if (!name) return false;
  try {
    const resp = await createPlaylist({ name });
    if (!resp.ok) {
      alert("Couldn't create playlist: " + (await resp.text()));
      return false;
    }
    const { id } = await readJson<CreatedPlaylistJson>(resp);
    window.location.href = "/playlists/" + id;
  } catch (e) {
    trace("createFromForm failed", e);
    alert("Couldn't create playlist.");
  }
  return false;
}

// -- /playlists/:id detail page ----------------------------------------------

export function editDetails(): void {
  const form = byIdOrNull("playlist-edit-form");
  const header = document.querySelector<HTMLElement>(".playlist-detail-header");
  const desc = byIdOrNull("playlist-description");
  if (form) form.style.display = "";
  if (header) header.style.display = "none";
  if (desc) desc.style.display = "none";
}

export function cancelEdit(): void {
  const form = byIdOrNull("playlist-edit-form");
  const header = document.querySelector<HTMLElement>(".playlist-detail-header");
  const desc = byIdOrNull("playlist-description");
  if (form) form.style.display = "none";
  if (header) header.style.display = "";
  if (desc) desc.style.display = "";
}

export async function saveDetails(event: Event, id: number): Promise<boolean> {
  event.preventDefault();
  const name = byIdOfOrNull("edit-playlist-name", HTMLInputElement)?.value;
  const description = byIdOfOrNull("edit-playlist-description", HTMLInputElement)?.value;
  if (!name || !name.trim()) return false;
  try {
    const resp = await updatePlaylist(id, { name: name.trim(), description: description || "" });
    if (!resp.ok) {
      alert("Couldn't save: " + (await resp.text()));
      return false;
    }
    reloadDetail(id);
  } catch (e) {
    trace("saveDetails failed", e);
    alert("Couldn't save playlist.");
  }
  return false;
}

export async function deletePlaylist(id: number): Promise<void> {
  if (!confirm("Delete this playlist? (Its tracks and concerts are not deleted.)")) return;
  try {
    const resp = await apiDeletePlaylist(id);
    if (!resp.ok && resp.status !== 404) {
      alert("Couldn't delete: " + (await resp.text()));
      return;
    }
    navigate("/playlists");
  } catch (e) {
    trace("deletePlaylist failed", e);
    alert("Couldn't delete playlist.");
  }
}

export async function removeItem(playlistId: number, itemId: number): Promise<void> {
  try {
    const resp = await removePlaylistItem(playlistId, itemId);
    if (!resp.ok && resp.status !== 404) {
      alert("Couldn't remove item: " + (await resp.text()));
      return;
    }
    reloadDetail(playlistId);
  } catch (e) {
    trace("removeItem failed", e);
    alert("Couldn't remove item.");
  }
}

// Navigate via htmx when available (keeps the player bar / boost behavior),
// else fall back to a full load.
function navigate(path: string): void {
  if (window.htmx) {
    window.htmx.ajax("GET", path, { target: "#content", select: "#content", swap: "outerHTML" });
  } else {
    window.location.href = path;
  }
}
function reloadDetail(id: number): void {
  navigate("/playlists/" + id);
}

// -- Drag-and-drop reorder (event-delegated, survives hx-boost swaps) --------

let dragSrc: HTMLElement | null = null;

function getDragAfterElement(list: Element, y: number): HTMLElement | null {
  const items = Array.from(list.querySelectorAll<HTMLElement>(".playlist-item:not(.dragging)"));
  let closest: { offset: number; el: HTMLElement | null } = { offset: -Infinity, el: null };
  for (const child of items) {
    const box = child.getBoundingClientRect();
    const offset = y - box.top - box.height / 2;
    if (offset < 0 && offset > closest.offset) closest = { offset, el: child };
  }
  return closest.el;
}

async function persistOrder(list: HTMLElement): Promise<void> {
  const detail = list.closest(".playlist-detail");
  if (!detail) return;
  const id = Number(detail.getAttribute("data-playlist-id"));
  const itemIds = Array.from(list.querySelectorAll(".playlist-item")).map((li) =>
    parseInt(li.getAttribute("data-item-id") || "", 10),
  );
  try {
    const resp = await reorderPlaylistItems(id, itemIds);
    if (!resp.ok) {
      trace("reorder rejected, resyncing", resp.status);
      reloadDetail(id); // item set changed under us (e.g. 422); reload to resync.
    }
  } catch (e) {
    trace("persistOrder failed", e);
    reloadDetail(id);
  }
}

document.addEventListener("dragstart", (e) => {
  const li = e.target instanceof HTMLElement ? e.target.closest<HTMLElement>(".playlist-item") : null;
  if (!li) return;
  dragSrc = li;
  li.classList.add("dragging");
  if (e.dataTransfer) {
    e.dataTransfer.effectAllowed = "move";
    // Firefox requires data to be set for a drag to start.
    try {
      e.dataTransfer.setData("text/plain", li.getAttribute("data-item-id") || "");
    } catch {
      /* ignore */
    }
  }
});

document.addEventListener("dragend", () => {
  if (dragSrc) dragSrc.classList.remove("dragging");
  dragSrc = null;
});

document.addEventListener("dragover", (e) => {
  if (!dragSrc) return;
  const list = e.target instanceof HTMLElement ? e.target.closest<HTMLElement>("#playlist-items") : null;
  if (!list) return;
  e.preventDefault();
  const after = getDragAfterElement(list, e.clientY);
  if (after == null) list.appendChild(dragSrc);
  else list.insertBefore(dragSrc, after);
});

document.addEventListener("drop", (e) => {
  if (!dragSrc) return;
  const list = e.target instanceof HTMLElement ? e.target.closest<HTMLElement>("#playlist-items") : null;
  if (!list) return;
  e.preventDefault();
  persistOrder(list);
});
