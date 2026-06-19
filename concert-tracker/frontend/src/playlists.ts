// Playlist UI: the /playlists list page (create), the /playlists/:id detail page
// (edit / delete / remove item / drag-drop reorder), and the add-to-playlist sidebar
// panel. Playback lives in player.ts (it extends the queue); this file only ever
// *calls* Player.playPlaylist.
//
// The app uses hx-boost, so page bodies are swapped into #content rather than
// reloaded. All interaction is therefore wired via event delegation on `document`
// (so it survives swaps) or via inline onclick/onsubmit in the templates -- never
// via one-shot DOMContentLoaded listeners that a boost swap would bypass.
import {
  addPlaylistItem,
  concertMembership,
  createPlaylist,
  deletePlaylist as apiDeletePlaylist,
  listPlaylists,
  playlistNestedIn,
  removePlaylistItem,
  reorderPlaylistItems,
  trackMembership,
  updatePlaylist,
  type AddItemReq,
  type MembershipJson,
} from "./api/client";
import { byIdOrNull } from "./shared/dom";
import type { AddTarget, PlaylistsApi } from "./shared/playlists-api";
// window.Player is declared ambiently by ./shared/player-api.ts (picked up
// by tsc via tsconfig's "include").

declare global {
  interface Window {
    PLAYLIST_DEBUG?: boolean;
  }
}

function trace(...args: unknown[]): void {
  if (window.PLAYLIST_DEBUG) console.debug("[playlists]", ...args);
}

// -- /playlists list page ----------------------------------------------------

async function createFromForm(event: Event): Promise<boolean> {
  event.preventDefault();
  const input = byIdOrNull<HTMLInputElement>("new-playlist-name");
  const name = input ? input.value.trim() : "";
  if (!name) return false;
  try {
    const resp = await createPlaylist({ name });
    if (!resp.ok) {
      alert("Couldn't create playlist: " + (await resp.text()));
      return false;
    }
    const { id } = (await resp.json()) as { id: number };
    window.location.href = "/playlists/" + id;
  } catch (e) {
    trace("createFromForm failed", e);
    alert("Couldn't create playlist.");
  }
  return false;
}

// -- /playlists/:id detail page ----------------------------------------------

function editDetails(): void {
  const form = byIdOrNull("playlist-edit-form");
  const header = document.querySelector<HTMLElement>(".playlist-detail-header");
  const desc = byIdOrNull("playlist-description");
  if (form) form.style.display = "";
  if (header) header.style.display = "none";
  if (desc) desc.style.display = "none";
}

function cancelEdit(): void {
  const form = byIdOrNull("playlist-edit-form");
  const header = document.querySelector<HTMLElement>(".playlist-detail-header");
  const desc = byIdOrNull("playlist-description");
  if (form) form.style.display = "none";
  if (header) header.style.display = "";
  if (desc) desc.style.display = "";
}

async function saveDetails(event: Event, id: number): Promise<boolean> {
  event.preventDefault();
  const name = byIdOrNull<HTMLInputElement>("edit-playlist-name")?.value;
  const description = byIdOrNull<HTMLInputElement>("edit-playlist-description")?.value;
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

async function deletePlaylist(id: number): Promise<void> {
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

async function removeItem(playlistId: number, itemId: number): Promise<void> {
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
  const target = e.target as HTMLElement | null;
  const li = target?.closest?.<HTMLElement>(".playlist-item");
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
  const target = e.target as HTMLElement | null;
  const list = target?.closest?.<HTMLElement>("#playlist-items");
  if (!list) return;
  e.preventDefault();
  const after = getDragAfterElement(list, e.clientY);
  if (after == null) list.appendChild(dragSrc);
  else list.insertBefore(dragSrc, after);
});

document.addEventListener("drop", (e) => {
  if (!dragSrc) return;
  const target = e.target as HTMLElement | null;
  const list = target?.closest?.<HTMLElement>("#playlist-items");
  if (!list) return;
  e.preventDefault();
  persistOrder(list);
});

// -- Add-to-playlist sidebar panel -------------------------------------------
// AddTarget is declared in ./shared/playlists-api.ts (player.ts's
// addToPlaylist() constructs one without importing this file).

interface PlaylistRef {
  id: number;
  name: string;
}

interface ActionableRow {
  id: number | "new";
  el: HTMLLIElement;
  action: () => void;
}

let currentAddTarget: AddTarget | null = null;
let allPlaylists: PlaylistRef[] = [];
let memberMap = new Map<number, number>(); // playlist id -> item_id (from membership endpoint)
// Monotonic counter; incremented on each openAdd call. Each fetch closure
// captures the token at call time and discards its result if the token has
// changed (i.e. a newer openAdd was called before the fetch resolved).
let addPanelToken = 0;
// Whether the current activeId highlight was set by an exact-name / Create
// row match while the user was typing (true) or by arrow-key navigation
// (false).  Controls whether Enter clears the filter:
//   true  -> typing-originated: clear filter + show full list (the user just
//             confirmed a name match, no reason to keep the filter term).
//   false -> arrow-key: keep filter so the user can keep toggling the same
//            highlighted row with repeated Enter presses.
let activeFromTyping = false;
// Whether the sidebar was open before openAdd was called. Used by closeAdd
// to decide whether to close the sidebar when the add panel is dismissed.
let sidebarWasOpen = false;
// The playlist id (or "new" for the Create row) of the currently highlighted
// row, or null when no row is highlighted (focus stays in the filter input).
// Tracked by id rather than ordinal so the highlight survives re-renders.
let activeId: number | "new" | null = null;
// Ordered list of actionable rows built by renderAddList.
let actionableRows: ActionableRow[] = [];

// Reset all add-panel state. Called by both closeAdd and the MutationObserver
// that detects an external sidebar close.
function resetAddState(): void {
  currentAddTarget = null;
  allPlaylists = [];
  memberMap = new Map();
  activeFromTyping = false;
  sidebarWasOpen = false;
  activeId = null;
  actionableRows = [];
}

// Detect sidebar close (player.ts removes sidebar-open from body) while the
// add panel is showing, and clear our state so reopening shows the queue.
new MutationObserver(() => {
  if (!document.body.classList.contains("sidebar-open") && currentAddTarget) {
    // Sidebar was closed externally while add panel was active; reset
    // showing-add so reopening the sidebar shows the queue normally.
    const sidebar = byIdOrNull("player-sidebar");
    if (sidebar) sidebar.classList.remove("showing-add");
    resetAddState();
  }
}).observe(document.body, { attributes: true, attributeFilter: ["class"] });

/**
 * Fetch membership for `target` from the endpoint matching its type. Every
 * AddTarget variant has a membership endpoint (unlike the original
 * `membershipUrl`, which returned `null` for an unrecognized `target.type` —
 * the discriminated union makes that case statically unreachable).
 */
async function fetchMembership(target: AddTarget): Promise<MembershipJson[]> {
  switch (target.type) {
    case "track":
      return trackMembership(target.concertId, target.trackIndex);
    case "concert":
      return concertMembership(target.concertId);
    case "playlist":
      return playlistNestedIn(target.childPlaylistId);
  }
}

function targetLabel(target: AddTarget): string {
  const n = target.label || "";
  if (target.type === "track") return n ? `Adding “${n}” to…` : "Adding track to…";
  if (target.type === "concert") return n ? `Adding “${n}” to…` : "Adding concert to…";
  return n ? `Nesting “${n}” into…` : "Nesting playlist into…";
}

function addItemBody(target: AddTarget): AddItemReq {
  if (target.type === "track") {
    return { type: "track", concert_id: target.concertId, track_index: target.trackIndex };
  }
  if (target.type === "concert") {
    return { type: "concert", concert_id: target.concertId };
  }
  return { type: "playlist", child_playlist_id: target.childPlaylistId };
}

// Re-fetch membership from the server and rebuild memberMap.
// Returns true on success, false on network/server error.
// Guarded by addPanelToken so stale responses from a superseded openAdd are
// discarded.
async function reloadMembership(token: number): Promise<boolean> {
  if (!currentAddTarget) return false;
  try {
    const data = await fetchMembership(currentAddTarget);
    if (token !== addPanelToken) return false; // superseded
    memberMap = new Map(data.map((m) => [m.id, m.item_id]));
    return true;
  } catch (e) {
    trace("reloadMembership failed", e);
    return false;
  }
}

async function openAdd(target: AddTarget): Promise<void> {
  trace("openAdd", target);
  const token = ++addPanelToken;
  currentAddTarget = target;
  allPlaylists = [];
  memberMap = new Map();
  activeId = null;
  actionableRows = [];

  // Record whether the sidebar was already open so closeAdd can restore that state.
  sidebarWasOpen = document.body.classList.contains("sidebar-open");

  // Open sidebar via Player (single owner of sidebar-open state).
  window.Player?.openSidebar();

  // Swap to add panel by adding the CSS class that hides queue/concert sections.
  const sidebar = byIdOrNull("player-sidebar");
  if (sidebar) sidebar.classList.add("showing-add");

  // Set context label.
  const ctx = byIdOrNull("add-pl-context");
  if (ctx) ctx.textContent = targetLabel(target);

  // Reset filter, focus it so the user can type immediately.
  const filter = byIdOrNull<HTMLInputElement>("add-pl-filter");
  if (filter) {
    filter.value = "";
    filter.removeAttribute("aria-activedescendant");
    filter.focus();
  }
  const error = byIdOrNull("add-pl-error");
  if (error) error.style.display = "none";

  // Show loading indicator.
  const list = byIdOrNull("add-pl-list");
  if (list) {
    list.replaceChildren();
    const loading = document.createElement("li");
    loading.className = "add-pl-row add-pl-row-member";
    loading.style.justifyContent = "center";
    loading.textContent = "Loading…";
    list.appendChild(loading);
  }

  // Fetch all playlists and memberships in parallel. listPlaylists()/
  // fetchMembership() throw on a non-ok response (see api/client.ts), so a
  // single catch below covers both — no manual status checking needed.
  try {
    const [plData, memData] = await Promise.all([listPlaylists(), fetchMembership(target)]);
    // Discard result if a newer openAdd has been called since this fetch started.
    if (token !== addPanelToken) return;

    allPlaylists = plData.map((e) => ({ id: e.playlist.id, name: e.playlist.name }));
    memberMap = new Map(memData.map((m) => [m.id, m.item_id]));

    renderAddList(filter ? filter.value : "");
  } catch (e) {
    trace("openAdd fetch failed", e);
    if (token !== addPanelToken) return;
    if (error) {
      error.textContent = "Couldn't load playlists.";
      error.style.display = "";
    }
    if (list) list.replaceChildren();
  }
}

function renderAddList(query: string): void {
  const list = byIdOrNull("add-pl-list");
  if (!list) return;
  const q = (query || "").trim().toLowerCase();
  const qRaw = (query || "").trim(); // preserve case for create / label
  list.replaceChildren();
  // activeFromTyping is reset by filterPlaylists before each re-render;
  // arrow-key handlers reset it themselves when they move the highlight.
  actionableRows = []; // rebuilt below

  // ARIA listbox role on the container.
  list.setAttribute("role", "listbox");

  const filtered = allPlaylists.filter((p) => !q || p.name.toLowerCase().indexOf(q) !== -1);

  // Build rows into member and non-member groups first (without appending to the
  // DOM yet).  Display order depends on whether a filter is active:
  //   - No filter: members on top so they're visible even with many playlists.
  //   - Filtered:  non-members first (the likely add targets), then members below.
  // actionableRows is always pushed in the same order rows are appended, so
  // arrow-key navigation matches the visual top-to-bottom order in both states.
  const memberEntries: { li: HTMLLIElement; row: ActionableRow }[] = [];
  const nonMemberEntries: { li: HTMLLIElement; row: ActionableRow }[] = [];
  const nonMemberRows: { li: HTMLLIElement; id: number; name: string; nameLower: string }[] = [];

  for (const pl of filtered) {
    const isMember = memberMap.has(pl.id);
    const rowId = "add-pl-opt-" + pl.id;
    const li = document.createElement("li");
    li.className = "add-pl-row" + (isMember ? " add-pl-row-member" : "");
    li.id = rowId;
    li.setAttribute("role", "option");
    li.setAttribute("aria-selected", "false");

    const check = document.createElement("span");
    check.className = "add-pl-check";
    check.setAttribute("aria-hidden", "true");
    check.textContent = isMember ? "✓" : "";

    const nameEl = document.createElement("span");
    nameEl.className = "add-pl-name";
    nameEl.textContent = pl.name; // textContent -- never innerHTML for untrusted data

    li.appendChild(check);
    li.appendChild(nameEl);

    if (isMember) {
      // Member row: trash button removes; row click is a deliberate no-op.
      const trashBtn = document.createElement("button");
      trashBtn.type = "button";
      trashBtn.className = "add-pl-trash";
      trashBtn.setAttribute("aria-label", "Remove from playlist");
      trashBtn.title = "Remove from playlist";
      const trashIcon = document.createElement("span");
      trashIcon.className = "icon-trash";
      trashBtn.appendChild(trashIcon);
      const plId = pl.id;
      trashBtn.addEventListener("click", (e) => {
        e.stopPropagation(); // don't bubble to the no-op row handler
        removeFromPlaylist(plId);
      });
      li.appendChild(trashBtn);
      const row: ActionableRow = { id: pl.id, el: li, action: () => removeFromPlaylist(plId) };
      memberEntries.push({ li, row });
    } else {
      const plId = pl.id;
      const plName = pl.name;
      const handler = () => addToPlaylist(plId, plName);
      li.setAttribute("tabindex", "0");
      li.addEventListener("click", handler);
      li.addEventListener("keydown", (e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          handler();
        }
      });
      nonMemberRows.push({ li, id: pl.id, name: pl.name, nameLower: pl.name.toLowerCase() });
      const row: ActionableRow = { id: pl.id, el: li, action: handler };
      nonMemberEntries.push({ li, row });
    }
  }

  // Append entries to the DOM and register in actionableRows in one step so the
  // two arrays stay in sync (arrow-key nav walks actionableRows positionally).
  function appendEntries(entries: { li: HTMLLIElement; row: ActionableRow }[]): void {
    for (const entry of entries) {
      list!.appendChild(entry.li);
      actionableRows.push(entry.row);
    }
  }

  // "Create new 'query'" row (when filter term has no exact match) or
  // "Create a new playlist" empty-state row (when there are no playlists at all).
  // Kept as a single unit so both arms share one code path regardless of which
  // display branch calls us.  Sets createLi when the Create arm fires (used
  // by the enterAction rule below).
  const exactNameExists = allPlaylists.some((p) => p.name.toLowerCase() === q);
  let createLi: HTMLLIElement | null = null;
  function appendCreateOrEmpty(): void {
    if (q && !exactNameExists) {
      createLi = document.createElement("li");
      createLi.className = "add-pl-row add-pl-row-new";
      createLi.id = "add-pl-opt-new";
      createLi.setAttribute("role", "option");
      createLi.setAttribute("aria-selected", "false");
      const ck = document.createElement("span");
      ck.className = "add-pl-check";
      ck.setAttribute("aria-hidden", "true");
      ck.textContent = "+";
      const label = document.createElement("span");
      label.className = "add-pl-name";
      label.textContent = `Create “${qRaw}”`; // textContent -- no XSS
      createLi.appendChild(ck);
      createLi.appendChild(label);
      createLi.setAttribute("tabindex", "0");
      createLi.addEventListener("click", () => createAndAdd(qRaw));
      createLi.addEventListener("keydown", (e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          createAndAdd(qRaw);
        }
      });
      list!.appendChild(createLi);
      actionableRows.push({ id: "new", el: createLi, action: () => createAndAdd(qRaw) });
    } else if (filtered.length === 0) {
      // Empty state: no playlists exist at all.
      const li = document.createElement("li");
      li.className = "add-pl-row add-pl-row-new";
      li.id = "add-pl-opt-new";
      li.setAttribute("role", "option");
      li.setAttribute("aria-selected", "false");
      const ck = document.createElement("span");
      ck.className = "add-pl-check";
      ck.setAttribute("aria-hidden", "true");
      ck.textContent = "+";
      const nameEl = document.createElement("span");
      nameEl.className = "add-pl-name";
      nameEl.textContent = "Create a new playlist";
      li.appendChild(ck);
      li.appendChild(nameEl);
      li.setAttribute("tabindex", "0");
      const createEmpty = () => {
        const n = prompt("New playlist name:");
        if (n && n.trim()) createAndAdd(n.trim());
      };
      li.addEventListener("click", createEmpty);
      li.addEventListener("keydown", (e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          createEmpty();
        }
      });
      list!.appendChild(li);
      actionableRows.push({ id: "new", el: li, action: createEmpty });
    }
  }

  // Choose display order and append.  actionableRows ends up in visual order,
  // which is what the ArrowDown/ArrowUp handlers rely on.
  if (!q) {
    // No filter: members on top so the user can see what's already added.
    appendEntries(memberEntries);
    appendEntries(nonMemberEntries);
    appendCreateOrEmpty(); // only the empty-state arm can fire when q is empty
  } else {
    // Filtered: non-member matches first (likely add targets), then Create row,
    // then members at the bottom (already-added playlists sink out of the way).
    appendEntries(nonMemberEntries);
    appendCreateOrEmpty();
    appendEntries(memberEntries);
  }

  // Auto-highlight the exact-match row (or the Create row) when the user has
  // typed a query, but only when no arrow-key highlight is already active.
  //
  // Rule 1 -- exact name match with a non-member row: highlight it and mark
  //   the highlight as typing-originated so Enter will clear the filter.
  //   (An exact match to a *member* is a no-op -- the target is already there.)
  // Rule 2 -- no non-member rows visible but a Create row is present (unique
  //   new name, or all matches are members): highlight the Create row.
  // Otherwise -- no auto-highlight; Enter does nothing until the user clicks
  //   or arrow-keys to a row.
  if (q && activeId === null) {
    const exactMatch = nonMemberRows.find((r) => r.nameLower === q);
    if (exactMatch) {
      activeId = exactMatch.id; // highlight the matched non-member row
      activeFromTyping = true; // Enter clears the filter after adding
    } else if (nonMemberRows.length === 0 && createLi) {
      activeId = "new"; // highlight the Create row
      activeFromTyping = true;
    }
  }

  // Apply the active highlight (after the auto-match block so typing-set
  // highlights are reflected immediately without a second render).
  applyActiveHighlight();
}

// Apply (or remove) the active highlight based on activeId.
// Updates aria-selected and aria-activedescendant on the filter input.
function applyActiveHighlight(): void {
  const filter = byIdOrNull<HTMLInputElement>("add-pl-filter");
  let found = false;
  for (const row of actionableRows) {
    const isActive = row.id === activeId;
    row.el.classList.toggle("add-pl-row-active", isActive);
    row.el.setAttribute("aria-selected", isActive ? "true" : "false");
    if (isActive) {
      found = true;
      if (filter) filter.setAttribute("aria-activedescendant", row.el.id);
      row.el.scrollIntoView({ block: "nearest" });
    }
  }
  if (!found) {
    activeId = null;
    if (filter) filter.removeAttribute("aria-activedescendant");
  }
}

function filterPlaylists(query: string): void {
  // Each keystroke clears both the highlight and the provenance bit; the
  // auto-match block in renderAddList will recompute them for the new query.
  activeId = null;
  activeFromTyping = false;
  renderAddList(query);
}

// Enter-dispatch — two cases, distinguished by activeFromTyping:
//
// 1a. Typing-originated highlight (exact name match / Create row):
//     Clear the filter first, show the full list, then run the action.
//     (The user confirmed a name; no reason to keep the filter term.)
//
// 1b. Arrow-key highlight (user navigated explicitly):
//     Run the action WITHOUT clearing the filter, so the user can press
//     Enter again to toggle the same row (add → remove → add …).
//
// 2. No highlight, non-empty filter: no-op (ambiguous — let the user click).
//
// 3. No highlight, empty filter: close the panel.
function dispatchEnter(filterEl: HTMLInputElement | null): void {
  if (activeId !== null) {
    const row = actionableRows.find((r) => r.id === activeId);
    if (row) {
      // Capture action before any re-render (closures hold id/name by value).
      const action = row.action;
      if (activeFromTyping && filterEl) {
        // Typing-originated: clear filter + re-render before the async action.
        filterEl.value = "";
        activeId = null;
        activeFromTyping = false;
        renderAddList("");
      }
      action();
      return;
    }
  }
  if (!(filterEl ? filterEl.value : "").trim()) {
    closeAdd();
  }
}

function filterKeydown(event: KeyboardEvent): void {
  const filter = event.target as HTMLInputElement;
  if (event.key === "ArrowDown") {
    event.preventDefault();
    if (actionableRows.length === 0) return;
    if (activeId === null) {
      activeId = actionableRows[0]!.id;
    } else {
      const idx = actionableRows.findIndex((r) => r.id === activeId);
      if (idx < actionableRows.length - 1) activeId = actionableRows[idx + 1]!.id;
      // else already at the bottom -- clamp
    }
    // Arrow nav takes over: Enter should now keep the filter (repeated toggle).
    activeFromTyping = false;
    applyActiveHighlight();
  } else if (event.key === "ArrowUp") {
    event.preventDefault();
    if (activeId === null) return;
    const idx = actionableRows.findIndex((r) => r.id === activeId);
    if (idx <= 0) {
      // Already at or before the first row: return focus to filter-only mode.
      activeId = null;
      activeFromTyping = false;
      applyActiveHighlight();
    } else {
      activeId = actionableRows[idx - 1]!.id;
      activeFromTyping = false;
      applyActiveHighlight();
    }
  } else if (event.key === "Enter") {
    event.preventDefault();
    dispatchEnter(filter);
  }
}

function closeAdd(): void {
  trace("closeAdd");
  const sidebar = byIdOrNull("player-sidebar");
  if (sidebar) sidebar.classList.remove("showing-add");
  if (!sidebarWasOpen) window.Player?.closeSidebar();
  resetAddState();
}

async function addToPlaylist(playlistId: number, playlistName: string): Promise<void> {
  if (!currentAddTarget) return;
  const body = addItemBody(currentAddTarget);
  const token = addPanelToken;
  const error = byIdOrNull("add-pl-error");
  if (error) error.style.display = "none";
  trace("addToPlaylist", { playlistId, playlistName, target: currentAddTarget });
  try {
    const resp = await addPlaylistItem(playlistId, body);
    if (!resp.ok) {
      const msg = await resp.text();
      if (error) {
        error.textContent = "Couldn't add: " + msg;
        error.style.display = "";
      }
      return;
    }
    // Re-fetch membership so memberMap is authoritative (handles duplicates).
    const ok = await reloadMembership(token);
    if (!ok) return; // superseded or network error
    const filter = byIdOrNull<HTMLInputElement>("add-pl-filter");
    renderAddList(filter ? filter.value : "");
  } catch (e) {
    trace("addToPlaylist failed", e);
    if (error) {
      error.textContent = "Couldn't add to playlist.";
      error.style.display = "";
    }
  }
}

async function removeFromPlaylist(playlistId: number): Promise<void> {
  if (!currentAddTarget) return;
  const itemId = memberMap.get(playlistId);
  if (itemId == null) return;
  const token = addPanelToken;
  const error = byIdOrNull("add-pl-error");
  if (error) error.style.display = "none";
  trace("removeFromPlaylist", { playlistId, itemId, target: currentAddTarget });
  try {
    const resp = await removePlaylistItem(playlistId, itemId);
    // 404 is treated as success (concurrent removal).
    if (!resp.ok && resp.status !== 404) {
      const msg = await resp.text();
      if (error) {
        error.textContent = "Couldn't remove: " + msg;
        error.style.display = "";
      }
      return;
    }
    // Re-fetch membership so memberMap is authoritative.
    const ok = await reloadMembership(token);
    if (!ok) return; // superseded or network error
    const filter = byIdOrNull<HTMLInputElement>("add-pl-filter");
    renderAddList(filter ? filter.value : "");
  } catch (e) {
    trace("removeFromPlaylist failed", e);
    if (error) {
      error.textContent = "Couldn't remove from playlist.";
      error.style.display = "";
    }
  }
}

async function createAndAdd(name: string): Promise<void> {
  if (!currentAddTarget || !name) return;
  const error = byIdOrNull("add-pl-error");
  if (error) error.style.display = "none";
  trace("createAndAdd", { name, target: currentAddTarget });
  try {
    const plResp = await createPlaylist({ name });
    if (!plResp.ok) {
      const msg = await plResp.text();
      if (error) {
        error.textContent = "Couldn't create: " + msg;
        error.style.display = "";
      }
      return;
    }
    const { id } = (await plResp.json()) as { id: number };
    allPlaylists.push({ id, name });
    await addToPlaylist(id, name);
  } catch (e) {
    trace("createAndAdd failed", e);
    if (error) {
      error.textContent = "Couldn't create playlist.";
      error.style.display = "";
    }
  }
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
  filterPlaylists,
  filterKeydown,
};

window.Playlists = api;
