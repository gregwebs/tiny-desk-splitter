// Playlist UI: the /playlists list page (create), the /playlists/:id detail page
// (edit / delete / remove item / drag-drop reorder), and — added in later phases —
// the add-to-playlist sidebar panel. Playback lives in player.js (it extends the
// queue); this file only ever *calls* Player.playPlaylist.
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

  window.Playlists = {
    createFromForm: createFromForm,
    editDetails: editDetails,
    cancelEdit: cancelEdit,
    saveDetails: saveDetails,
    deletePlaylist: deletePlaylist,
    removeItem: removeItem,
  };
})();
