// Playlist UI: the /playlists list page (create), the /playlists/:id detail page
// (edit / delete / remove item / drag-drop reorder), and the add-to-playlist sidebar
// panel. Playback lives in player.js (it extends the queue); this file only ever
// *calls* Player.playPlaylist.
//
// The app uses hx-boost, so page bodies are swapped into #content rather than
// reloaded. All interaction is therefore wired via event delegation on `document`
// (so it survives swaps) or via inline onclick/onsubmit in the templates — never
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

  // ── /playlists list page ────────────────────────────────────────────────────

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

  // ── /playlists/:id detail page ──────────────────────────────────────────────

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

  // ── Drag-and-drop reorder (event-delegated, survives hx-boost swaps) ─────────

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

  // ── Add-to-playlist sidebar panel ──────────────────────────────────────────

  let currentAddTarget = null;
  let allPlaylists = [];    // [{id, name}]
  let memberSet = new Set(); // playlist ids that already contain the target
  // Monotonic counter; incremented on each openAdd call. Each fetch closure
  // captures the token at call time and discards its result if the token has
  // changed (i.e. a newer openAdd was called before the fetch resolved).
  let addPanelToken = 0;
  // The action Enter should invoke in the current filter state, or null if
  // Enter is a no-op (ambiguous results — let the user click explicitly).
  let enterAction = null;
  // Whether the sidebar was open before openAdd was called. Used by closeAdd
  // to decide whether to close the sidebar when the add panel is dismissed.
  let sidebarWasOpen = false;

  // Detect sidebar close (player.js removes sidebar-open from body) while the
  // add panel is showing, and clear our state so reopening shows the queue.
  (function () {
    const obs = new MutationObserver(function () {
      if (!document.body.classList.contains("sidebar-open") && currentAddTarget) {
        // Sidebar was closed externally while add panel was active; reset
        // showing-add so reopening the sidebar shows the queue normally.
        const sidebar = document.getElementById("player-sidebar");
        if (sidebar) sidebar.classList.remove("showing-add");
        currentAddTarget = null;
        allPlaylists = [];
        memberSet = new Set();
        sidebarWasOpen = false;
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
    if (target.type === "track")    return n ? "Adding \u201c" + n + "\u201d to\u2026" : "Adding track to\u2026";
    if (target.type === "concert")  return n ? "Adding \u201c" + n + "\u201d to\u2026" : "Adding concert to\u2026";
    if (target.type === "playlist") return n ? "Nesting \u201c" + n + "\u201d into\u2026" : "Nesting playlist into\u2026";
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

  async function openAdd(target) {
    trace("openAdd", target);
    const token = ++addPanelToken;
    currentAddTarget = target;
    allPlaylists = [];
    memberSet = new Set();

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
    if (filter) { filter.value = ""; filter.focus(); }
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
      memberSet = new Set(memData.map(function (m) { return m.id; }));

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

    const filtered = allPlaylists.filter(function (p) {
      return !q || p.name.toLowerCase().indexOf(q) !== -1;
    });

    // Build non-member rows and track candidates for Enter-key logic.
    const nonMemberRows = []; // [{li, id, name, nameLower}]
    for (const pl of filtered) {
      const isMember = memberSet.has(pl.id);
      const li = document.createElement("li");
      li.className = "add-pl-row" + (isMember ? " add-pl-row-member" : "");

      const check = document.createElement("span");
      check.className = "add-pl-check";
      check.textContent = isMember ? "✓" : "";

      const nameEl = document.createElement("span");
      nameEl.className = "add-pl-name";
      nameEl.textContent = pl.name; // textContent — never innerHTML for untrusted data

      li.appendChild(check);
      li.appendChild(nameEl);
      if (!isMember) {
        const handler = (function (id, n) {
          return function () { addToPlaylist(id, n); };
        }(pl.id, pl.name));
        li.setAttribute("role", "button");
        li.setAttribute("tabindex", "0");
        li.addEventListener("click", handler);
        li.addEventListener("keydown", function (e) {
          if (e.key === "Enter" || e.key === " ") { e.preventDefault(); handler(); }
        });
        nonMemberRows.push({ li: li, id: pl.id, name: pl.name, nameLower: pl.name.toLowerCase() });
      }
      list.appendChild(li);
    }

    // "Create new 'query'" row — shown when there is a filter term that does NOT
    // exactly match an existing playlist name (exact match means the user is adding
    // to that playlist, not creating a new one).
    const exactNameExists = allPlaylists.some(function (p) { return p.name.toLowerCase() === q; });
    let createLi = null;
    if (q && !exactNameExists) {
      createLi = document.createElement("li");
      createLi.className = "add-pl-row add-pl-row-new";
      const check = document.createElement("span");
      check.className = "add-pl-check";
      check.textContent = "+";
      const label = document.createElement("span");
      label.className = "add-pl-name";
      label.textContent = "Create \u201c" + qRaw + "\u201d"; // textContent — no XSS
      createLi.appendChild(check);
      createLi.appendChild(label);
      createLi.setAttribute("role", "button");
      createLi.setAttribute("tabindex", "0");
      createLi.addEventListener("click", function () { createAndAdd(qRaw); });
      createLi.addEventListener("keydown", function (e) {
        if (e.key === "Enter" || e.key === " ") { e.preventDefault(); createAndAdd(qRaw); }
      });
      list.appendChild(createLi);
    } else if (filtered.length === 0) {
      // Empty state: no playlists at all.
      const li = document.createElement("li");
      li.className = "add-pl-row add-pl-row-new";
      const check = document.createElement("span");
      check.className = "add-pl-check";
      check.textContent = "+";
      const nameEl = document.createElement("span");
      nameEl.className = "add-pl-name";
      nameEl.textContent = "Create a new playlist";
      li.appendChild(check);
      li.appendChild(nameEl);
      li.setAttribute("role", "button");
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
    }

    // Determine which row (if any) gets the active highlight and owns Enter.
    //
    // Rule 1 — exact name match with a non-member row: highlight that row and
    //   make Enter add to that playlist. (An exact match to a *member* is a
    //   no-op; the track is already there.)
    // Rule 2 — no non-member rows are shown but a Create row is present (query
    //   typed, all matches are members or there are none): highlight the Create
    //   row and make Enter create-and-add.
    // Otherwise: no highlight, Enter does nothing (multiple choices → user must
    //   click).
    if (q) {
      const exactMatch = nonMemberRows.find(function (r) { return r.nameLower === q; });
      if (exactMatch) {
        exactMatch.li.classList.add("add-pl-row-active");
        const matchId = exactMatch.id, matchName = exactMatch.name;
        enterAction = function () { addToPlaylist(matchId, matchName); };
      } else if (nonMemberRows.length === 0 && createLi) {
        // Only the Create row is actionable.
        createLi.classList.add("add-pl-row-active");
        enterAction = function () { createAndAdd(qRaw); };
      }
    }
  }

  function filterPlaylists(query) {
    renderAddList(query);
  }

  function filterKeydown(event) {
    if (event.key !== "Enter") return;
    if (enterAction) {
      event.preventDefault();
      const action = enterAction; // capture before renderAddList resets it
      const filter = document.getElementById("add-pl-filter");
      if (filter) filter.value = "";
      renderAddList(""); // clear highlight, show full list
      action();          // async; re-renders again when fetch resolves
    } else if (!(event.target.value || "").trim()) {
      // Empty filter, no action → close the add panel (and sidebar if needed).
      event.preventDefault();
      closeAdd();
    }
  }

  function closeAdd() {
    trace("closeAdd");
    const sidebar = document.getElementById("player-sidebar");
    if (sidebar) sidebar.classList.remove("showing-add");
    if (!sidebarWasOpen && window.Player && window.Player.closeSidebar) {
      window.Player.closeSidebar();
    }
    currentAddTarget = null;
    allPlaylists = [];
    memberSet = new Set();
    sidebarWasOpen = false;
  }

  async function addToPlaylist(playlistId, playlistName) {
    if (!currentAddTarget) return;
    const body = addItemBody(currentAddTarget);
    if (!body) return;
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
      memberSet.add(playlistId);
      const filter = document.getElementById("add-pl-filter");
      renderAddList(filter ? filter.value : "");
    } catch (e) {
      trace("addToPlaylist failed", e);
      if (error) { error.textContent = "Couldn't add to playlist."; error.style.display = ""; }
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
})();
