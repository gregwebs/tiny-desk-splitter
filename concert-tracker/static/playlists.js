// Playlist UI: the /playlists list page (create), the /playlists/:id detail page
// (edit / delete / remove item / drag-drop reorder), and the add-to-playlist sidebar
// panel. Playback lives in player.js (it extends the queue); this file only ever
// *calls* Player.playPlaylist.
//
// The app uses hx-boost, so page bodies are swapped into #content rather than
// reloaded. All interaction is therefore wired via event delegation on `document`
// (so it survives swaps) or via inline onclick/onsubmit in the templates -- never
// via one-shot DOMContentLoaded listeners that a boost swap would bypass.
(function () {
  "use strict";

  function trace() {
    if (window.PLAYLIST_DEBUG) console.debug.apply(console, ["[playlists]"].concat([].slice.call(arguments)));
  }

  async function postJson(url, body, method) {
    const resp = await fetch(url, {
      method: method || "POST",
      headers: { "Content-Type": "application/json" },
      body: body == null ? undefined : JSON.stringify(body),
    });
    return resp;
  }

  // -- /playlists list page ----------------------------------------------------

  async function createFromForm(event) {
    event.preventDefault();
    const input = document.getElementById("new-playlist-name");
    const name = input ? input.value.trim() : "";
    if (!name) return false;
    try {
      const resp = await postJson("/api/playlists", { name });
      if (!resp.ok) {
        alert("Couldn't create playlist: " + (await resp.text()));
        return false;
      }
      const { id } = await resp.json();
      window.location.href = "/playlists/" + id;
    } catch (e) {
      trace("createFromForm failed", e);
      alert("Couldn't create playlist.");
    }
    return false;
  }

  // -- /playlists/:id detail page ----------------------------------------------

  function editDetails() {
    const form = document.getElementById("playlist-edit-form");
    const header = document.querySelector(".playlist-detail-header");
    const desc = document.getElementById("playlist-description");
    if (form) form.style.display = "";
    if (header) header.style.display = "none";
    if (desc) desc.style.display = "none";
  }

  function cancelEdit() {
    const form = document.getElementById("playlist-edit-form");
    const header = document.querySelector(".playlist-detail-header");
    const desc = document.getElementById("playlist-description");
    if (form) form.style.display = "none";
    if (header) header.style.display = "";
    if (desc) desc.style.display = "";
  }

  async function saveDetails(event, id) {
    event.preventDefault();
    const name = (document.getElementById("edit-playlist-name") || {}).value;
    const description = (document.getElementById("edit-playlist-description") || {}).value;
    if (!name || !name.trim()) return false;
    try {
      const resp = await postJson("/api/playlists/" + id, { name: name.trim(), description: description || "" }, "PATCH");
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

  async function deletePlaylist(id) {
    if (!confirm("Delete this playlist? (Its tracks and concerts are not deleted.)")) return;
    try {
      const resp = await postJson("/api/playlists/" + id, null, "DELETE");
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

  async function removeItem(playlistId, itemId) {
    try {
      const resp = await postJson("/api/playlists/" + playlistId + "/items/" + itemId, null, "DELETE");
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
  function navigate(path) {
    if (window.htmx) window.htmx.ajax("GET", path, { target: "#content", select: "#content", swap: "outerHTML" });
    else window.location.href = path;
  }
  function reloadDetail(id) {
    navigate("/playlists/" + id);
  }

  // -- Drag-and-drop reorder (event-delegated, survives hx-boost swaps) --------

  let dragSrc = null;

  function getDragAfterElement(list, y) {
    const items = [].slice.call(list.querySelectorAll(".playlist-item:not(.dragging)"));
    let closest = { offset: -Infinity, el: null };
    for (const child of items) {
      const box = child.getBoundingClientRect();
      const offset = y - box.top - box.height / 2;
      if (offset < 0 && offset > closest.offset) closest = { offset, el: child };
    }
    return closest.el;
  }

  async function persistOrder(list) {
    const detail = list.closest(".playlist-detail");
    if (!detail) return;
    const id = detail.getAttribute("data-playlist-id");
    const itemIds = [].slice.call(list.querySelectorAll(".playlist-item")).map(function (li) {
      return parseInt(li.getAttribute("data-item-id"), 10);
    });
    try {
      const resp = await postJson("/api/playlists/" + id + "/items/reorder", { item_ids: itemIds });
      if (!resp.ok) {
        trace("reorder rejected, resyncing", resp.status);
        reloadDetail(id); // item set changed under us (e.g. 422); reload to resync.
      }
    } catch (e) {
      trace("persistOrder failed", e);
      reloadDetail(id);
    }
  }

  document.addEventListener("dragstart", function (e) {
    const li = e.target.closest && e.target.closest(".playlist-item");
    if (!li) return;
    dragSrc = li;
    li.classList.add("dragging");
    if (e.dataTransfer) {
      e.dataTransfer.effectAllowed = "move";
      // Firefox requires data to be set for a drag to start.
      try { e.dataTransfer.setData("text/plain", li.getAttribute("data-item-id") || ""); } catch (_) {}
    }
  });

  document.addEventListener("dragend", function () {
    if (dragSrc) dragSrc.classList.remove("dragging");
    dragSrc = null;
  });

  document.addEventListener("dragover", function (e) {
    if (!dragSrc) return;
    const list = e.target.closest && e.target.closest("#playlist-items");
    if (!list) return;
    e.preventDefault();
    const after = getDragAfterElement(list, e.clientY);
    if (after == null) list.appendChild(dragSrc);
    else list.insertBefore(dragSrc, after);
  });

  document.addEventListener("drop", function (e) {
    if (!dragSrc) return;
    const list = e.target.closest && e.target.closest("#playlist-items");
    if (!list) return;
    e.preventDefault();
    persistOrder(list);
  });

  // -- Add-to-playlist sidebar panel -------------------------------------------

  let currentAddTarget = null;
  let allPlaylists = [];     // [{id, name}]
  let memberMap = new Map(); // playlist id -> item_id (from membership endpoint)
  // Monotonic counter; incremented on each openAdd call. Each fetch closure
  // captures the token at call time and discards its result if the token has
  // changed (i.e. a newer openAdd was called before the fetch resolved).
  let addPanelToken = 0;
  // The action Enter should invoke in the current filter state, or null if
  // Enter is a no-op (ambiguous results -- let the user click explicitly).
  // Only consulted when activeId is null (no arrow-key highlight active).
  let enterAction = null;
  // Whether the sidebar was open before openAdd was called. Used by closeAdd
  // to decide whether to close the sidebar when the add panel is dismissed.
  let sidebarWasOpen = false;
  // The playlist id (or "new" for the Create row) of the currently highlighted
  // row, or null when no row is highlighted (focus stays in the filter input).
  // Tracked by id rather than ordinal so the highlight survives re-renders.
  let activeId = null;
  // Ordered list of actionable rows built by renderAddList.
  // Each entry: { id: <playlist id or "new">, el: <li element>, action: fn }
  let actionableRows = [];

  // Reset all add-panel state. Called by both closeAdd and the MutationObserver
  // that detects an external sidebar close.
  function resetAddState() {
    currentAddTarget = null;
    allPlaylists = [];
    memberMap = new Map();
    enterAction = null;
    sidebarWasOpen = false;
    activeId = null;
    actionableRows = [];
  }

  // Detect sidebar close (player.js removes sidebar-open from body) while the
  // add panel is showing, and clear our state so reopening shows the queue.
  (function () {
    const obs = new MutationObserver(function () {
      if (!document.body.classList.contains("sidebar-open") && currentAddTarget) {
        // Sidebar was closed externally while add panel was active; reset
        // showing-add so reopening the sidebar shows the queue normally.
        const sidebar = document.getElementById("player-sidebar");
        if (sidebar) sidebar.classList.remove("showing-add");
        resetAddState();
      }
    });
    obs.observe(document.body, { attributes: true, attributeFilter: ["class"] });
  }());

  function membershipUrl(target) {
    if (target.type === "track")
      return "/api/concerts/" + target.concertId + "/tracks/" + target.trackIndex + "/playlists";
    if (target.type === "concert")
      return "/api/concerts/" + target.concertId + "/playlists";
    if (target.type === "playlist")
      return "/api/playlists/" + target.childPlaylistId + "/nested-in";
    return null;
  }

  function targetLabel(target) {
    const n = target.label || "";
    if (target.type === "track")    return n ? "Adding “" + n + "” to…" : "Adding track to…";
    if (target.type === "concert")  return n ? "Adding “" + n + "” to…" : "Adding concert to…";
    if (target.type === "playlist") return n ? "Nesting “" + n + "” into…" : "Nesting playlist into…";
    return "";
  }

  function addItemBody(target) {
    if (target.type === "track")
      return { type: "track", concert_id: target.concertId, track_index: target.trackIndex };
    if (target.type === "concert")
      return { type: "concert", concert_id: target.concertId };
    if (target.type === "playlist")
      return { type: "playlist", child_playlist_id: target.childPlaylistId };
    return null;
  }

  // Re-fetch membership from the server and rebuild memberMap.
  // Returns true on success, false on network/server error.
  // Guarded by addPanelToken so stale responses from a superseded openAdd are
  // discarded.
  async function reloadMembership(token) {
    if (!currentAddTarget) return false;
    const url = membershipUrl(currentAddTarget);
    if (!url) { memberMap = new Map(); return true; }
    try {
      const resp = await fetch(url);
      if (token !== addPanelToken) return false; // superseded
      if (!resp.ok) throw new Error("membership fetch failed: " + resp.status);
      const data = await resp.json();
      if (token !== addPanelToken) return false; // superseded
      memberMap = new Map(data.map(function (m) { return [m.id, m.item_id]; }));
      return true;
    } catch (e) {
      trace("reloadMembership failed", e);
      return false;
    }
  }

  async function openAdd(target) {
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
    if (window.Player && window.Player.openSidebar) window.Player.openSidebar();

    // Swap to add panel by adding the CSS class that hides queue/concert sections.
    const sidebar = document.getElementById("player-sidebar");
    if (sidebar) sidebar.classList.add("showing-add");

    // Set context label.
    const ctx = document.getElementById("add-pl-context");
    if (ctx) ctx.textContent = targetLabel(target);

    // Reset filter, focus it so the user can type immediately.
    const filter = document.getElementById("add-pl-filter");
    if (filter) { filter.value = ""; filter.removeAttribute("aria-activedescendant"); filter.focus(); }
    const error = document.getElementById("add-pl-error");
    if (error) error.style.display = "none";

    // Show loading indicator.
    const list = document.getElementById("add-pl-list");
    if (list) {
      list.replaceChildren();
      const loading = document.createElement("li");
      loading.className = "add-pl-row add-pl-row-member";
      loading.style.justifyContent = "center";
      loading.textContent = "Loading…";
      list.appendChild(loading);
    }

    // Fetch all playlists and memberships in parallel.
    try {
      const memUrl = membershipUrl(target);
      const [plResp, memResp] = await Promise.all([
        fetch("/api/playlists"),
        memUrl ? fetch(memUrl) : Promise.resolve(null),
      ]);
      // Discard result if a newer openAdd has been called since this fetch started.
      if (token !== addPanelToken) return;

      if (!plResp.ok) throw new Error("playlists fetch failed: " + plResp.status);
      if (memResp && !memResp.ok) throw new Error("membership fetch failed: " + memResp.status);

      const plData = await plResp.json();
      const memData = memResp ? await memResp.json() : [];

      if (token !== addPanelToken) return; // recheck after json parsing

      allPlaylists = plData.map(function (e) { return { id: e.playlist.id, name: e.playlist.name }; });
      memberMap = new Map(memData.map(function (m) { return [m.id, m.item_id]; }));

      renderAddList(filter ? filter.value : "");
    } catch (e) {
      trace("openAdd fetch failed", e);
      if (token !== addPanelToken) return;
      if (error) { error.textContent = "Couldn't load playlists."; error.style.display = ""; }
      if (list) list.replaceChildren();
    }
  }

  function renderAddList(query) {
    const list = document.getElementById("add-pl-list");
    if (!list) return;
    const q = (query || "").trim().toLowerCase();
    const qRaw = (query || "").trim(); // preserve case for create / label
    list.replaceChildren();
    enterAction = null; // reset; re-computed below
    actionableRows = []; // rebuilt below

    // ARIA listbox role on the container.
    list.setAttribute("role", "listbox");

    const filtered = allPlaylists.filter(function (p) {
      return !q || p.name.toLowerCase().indexOf(q) !== -1;
    });

    // Build rows into member and non-member groups first (without appending to the
    // DOM yet).  Display order depends on whether a filter is active:
    //   - No filter: members on top so they're visible even with many playlists.
    //   - Filtered:  non-members first (the likely add targets), then members below.
    // actionableRows is always pushed in the same order rows are appended, so
    // arrow-key navigation matches the visual top-to-bottom order in both states.
    const memberEntries = [];    // { li, row } for playlists the target belongs to
    const nonMemberEntries = []; // { li, row } for other matching playlists
    const nonMemberRows = [];    // [{li, id, name, nameLower}] for enterAction exact-match

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
      check.textContent = isMember ? "\u2713" : "";

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
        (function (id) {
          trashBtn.addEventListener("click", function (e) {
            e.stopPropagation(); // don't bubble to the no-op row handler
            removeFromPlaylist(id);
          });
        }(pl.id));
        li.appendChild(trashBtn);
        const row = { id: pl.id, el: li, action: (function (id) {
          return function () { removeFromPlaylist(id); };
        }(pl.id)) };
        memberEntries.push({ li: li, row: row });
      } else {
        const handler = (function (id, n) {
          return function () { addToPlaylist(id, n); };
        }(pl.id, pl.name));
        li.setAttribute("tabindex", "0");
        li.addEventListener("click", handler);
        li.addEventListener("keydown", function (e) {
          if (e.key === "Enter" || e.key === " ") { e.preventDefault(); handler(); }
        });
        nonMemberRows.push({ li: li, id: pl.id, name: pl.name, nameLower: pl.name.toLowerCase() });
        const row = { id: pl.id, el: li, action: handler };
        nonMemberEntries.push({ li: li, row: row });
      }
    }

    // Append entries to the DOM and register in actionableRows in one step so the
    // two arrays stay in sync (arrow-key nav walks actionableRows positionally).
    function appendEntries(entries) {
      for (var i = 0; i < entries.length; i++) {
        list.appendChild(entries[i].li);
        actionableRows.push(entries[i].row);
      }
    }

    // "Create new 'query'" row (when filter term has no exact match) or
    // "Create a new playlist" empty-state row (when there are no playlists at all).
    // Kept as a single unit so both arms share one code path regardless of which
    // display branch calls us.  Sets createLi when the Create arm fires (used
    // by the enterAction rule below).
    const exactNameExists = allPlaylists.some(function (p) { return p.name.toLowerCase() === q; });
    let createLi = null;
    function appendCreateOrEmpty() {
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
        label.textContent = "Create \u201c" + qRaw + "\u201d"; // textContent -- no XSS
        createLi.appendChild(ck);
        createLi.appendChild(label);
        createLi.setAttribute("tabindex", "0");
        createLi.addEventListener("click", function () { createAndAdd(qRaw); });
        createLi.addEventListener("keydown", function (e) {
          if (e.key === "Enter" || e.key === " ") { e.preventDefault(); createAndAdd(qRaw); }
        });
        list.appendChild(createLi);
        actionableRows.push({ id: "new", el: createLi, action: function () { createAndAdd(qRaw); } });
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
        const createEmpty = function () {
          const n = prompt("New playlist name:");
          if (n && n.trim()) createAndAdd(n.trim());
        };
        li.addEventListener("click", createEmpty);
        li.addEventListener("keydown", function (e) {
          if (e.key === "Enter" || e.key === " ") { e.preventDefault(); createEmpty(); }
        });
        list.appendChild(li);
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

    // Apply the active highlight (tracked by playlist id across re-renders).
    applyActiveHighlight();

    // Determine which row (if any) owns Enter when no row is arrow-highlighted.
    //
    // Rule 1 -- exact name match with a non-member row: highlight that row and
    //   make Enter add to that playlist. (An exact match to a *member* is a
    //   no-op; the track is already there.)
    // Rule 2 -- no non-member rows are shown but a Create row is present (query
    //   typed, all matches are members or there are none): highlight the Create
    //   row and make Enter create-and-add.
    // Otherwise: no highlight, Enter does nothing (multiple choices -> user must
    //   click or arrow-down to select).
    if (q) {
      const exactMatch = nonMemberRows.find(function (r) { return r.nameLower === q; });
      if (exactMatch) {
        const matchId = exactMatch.id, matchName = exactMatch.name;
        enterAction = function () { addToPlaylist(matchId, matchName); };
      } else if (nonMemberRows.length === 0 && createLi) {
        enterAction = function () { createAndAdd(qRaw); };
      }
    }
  }

  // Apply (or remove) the active highlight based on activeId.
  // Updates aria-selected and aria-activedescendant on the filter input.
  function applyActiveHighlight() {
    const filter = document.getElementById("add-pl-filter");
    let found = false;
    for (let i = 0; i < actionableRows.length; i++) {
      const row = actionableRows[i];
      const isActive = (row.id === activeId);
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

  function filterPlaylists(query) {
    // Typing clears the arrow-key highlight so the user is back in filter mode.
    activeId = null;
    renderAddList(query);
  }

  // Explicit Enter-dispatch so precedence is clear and easy to follow:
  // 1. Arrow-highlighted row action (add or remove), filter NOT cleared.
  // 2. enterAction (exact-name add / create), filter cleared (original behavior).
  // 3. Empty filter -> close the panel.
  function dispatchEnter(filterEl) {
    if (activeId !== null) {
      const row = actionableRows.find(function (r) { return r.id === activeId; });
      if (row) { row.action(); return; }
    }
    if (enterAction) {
      const action = enterAction;
      if (filterEl) filterEl.value = "";
      renderAddList("");
      action();
      return;
    }
    if (!(filterEl ? filterEl.value : "").trim()) {
      closeAdd();
    }
  }

  function filterKeydown(event) {
    const filter = event.target;
    if (event.key === "ArrowDown") {
      event.preventDefault();
      if (actionableRows.length === 0) return;
      if (activeId === null) {
        activeId = actionableRows[0].id;
      } else {
        const idx = actionableRows.findIndex(function (r) { return r.id === activeId; });
        if (idx < actionableRows.length - 1) activeId = actionableRows[idx + 1].id;
        // else already at the bottom -- clamp
      }
      applyActiveHighlight();
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      if (activeId === null) return;
      const idx = actionableRows.findIndex(function (r) { return r.id === activeId; });
      if (idx <= 0) {
        // Already at or before the first row: return focus to filter-only mode.
        activeId = null;
        applyActiveHighlight();
      } else {
        activeId = actionableRows[idx - 1].id;
        applyActiveHighlight();
      }
    } else if (event.key === "Enter") {
      event.preventDefault();
      dispatchEnter(filter);
    }
  }

  function closeAdd() {
    trace("closeAdd");
    const sidebar = document.getElementById("player-sidebar");
    if (sidebar) sidebar.classList.remove("showing-add");
    if (!sidebarWasOpen && window.Player && window.Player.closeSidebar) {
      window.Player.closeSidebar();
    }
    resetAddState();
  }

  async function addToPlaylist(playlistId, playlistName) {
    if (!currentAddTarget) return;
    const body = addItemBody(currentAddTarget);
    if (!body) return;
    const token = addPanelToken;
    const error = document.getElementById("add-pl-error");
    if (error) error.style.display = "none";
    trace("addToPlaylist", { playlistId, playlistName, target: currentAddTarget });
    try {
      const resp = await postJson("/api/playlists/" + playlistId + "/items", body);
      if (!resp.ok) {
        const msg = await resp.text();
        if (error) { error.textContent = "Couldn't add: " + msg; error.style.display = ""; }
        return;
      }
      // Re-fetch membership so memberMap is authoritative (handles duplicates).
      const ok = await reloadMembership(token);
      if (!ok) return; // superseded or network error
      const filter = document.getElementById("add-pl-filter");
      renderAddList(filter ? filter.value : "");
    } catch (e) {
      trace("addToPlaylist failed", e);
      if (error) { error.textContent = "Couldn't add to playlist."; error.style.display = ""; }
    }
  }

  async function removeFromPlaylist(playlistId) {
    if (!currentAddTarget) return;
    const itemId = memberMap.get(playlistId);
    if (itemId == null) return;
    const token = addPanelToken;
    const error = document.getElementById("add-pl-error");
    if (error) error.style.display = "none";
    trace("removeFromPlaylist", { playlistId, itemId, target: currentAddTarget });
    try {
      const resp = await postJson("/api/playlists/" + playlistId + "/items/" + itemId, null, "DELETE");
      // 404 is treated as success (concurrent removal).
      if (!resp.ok && resp.status !== 404) {
        const msg = await resp.text();
        if (error) { error.textContent = "Couldn't remove: " + msg; error.style.display = ""; }
        return;
      }
      // Re-fetch membership so memberMap is authoritative.
      const ok = await reloadMembership(token);
      if (!ok) return; // superseded or network error
      const filter = document.getElementById("add-pl-filter");
      renderAddList(filter ? filter.value : "");
    } catch (e) {
      trace("removeFromPlaylist failed", e);
      if (error) { error.textContent = "Couldn't remove from playlist."; error.style.display = ""; }
    }
  }

  async function createAndAdd(name) {
    if (!currentAddTarget || !name) return;
    const error = document.getElementById("add-pl-error");
    if (error) error.style.display = "none";
    trace("createAndAdd", { name, target: currentAddTarget });
    try {
      const plResp = await postJson("/api/playlists", { name });
      if (!plResp.ok) {
        const msg = await plResp.text();
        if (error) { error.textContent = "Couldn't create: " + msg; error.style.display = ""; }
        return;
      }
      const { id } = await plResp.json();
      allPlaylists.push({ id, name });
      await addToPlaylist(id, name);
    } catch (e) {
      trace("createAndAdd failed", e);
      if (error) { error.textContent = "Couldn't create playlist."; error.style.display = ""; }
    }
  }

  window.Playlists = {
    createFromForm: createFromForm,
    editDetails: editDetails,
    cancelEdit: cancelEdit,
    saveDetails: saveDetails,
    deletePlaylist: deletePlaylist,
    removeItem: removeItem,
    openAdd: openAdd,
    closeAdd: closeAdd,
    filterPlaylists: filterPlaylists,
    filterKeydown: filterKeydown,
  };
}());
