# Track delete swaps the whole card; detail-page bottom list loses its trash icons

## Problem

1. **Stale count**: Deleting a track via the trash icon in a concert card's
   expanded track list (listing page, or the card at the top of the detail
   page) updated the list but not the count on the "tracks" button. The
   delete endpoint returned only the `<ol class="track-list">` fragment and
   the button swapped `closest .track-list`, so the count — rendered into the
   card outside that target — was never refreshed.
2. **Simplification**: The detail page's bottom track list (shared
   `tracks.html` partial) had its own trash icons; removed to keep deletion
   in one place (the card's expandable list).

## Design

`POST /concerts/:id/tracks/:idx/delete` now responds with the **whole
concert card** and the trash button targets `closest .card` — the same
"mutate → swap card" pattern as want/ignore/archive/delete-split. The card
is rendered with the track list embedded expanded (`RowTemplate.tracks`
non-empty + `tracks-open` class) so the open list survives the swap.

The card render goes through `render_card_with_tracks(state, id, tracks)`
(`src/web/handlers.rs`), which forwards `has_archive_location` and
`scrape_pending` — the swapped card keeps its Archive button and polling
behavior.

The like (star) button was the only other consumer of the track-list-swap
response. It is now self-swapping: extracted to `templates/like_button.html`
with `hx-target="this"`, and `like_track` returns just the button. The
"liked rows hide their delete button" behavior is pure CSS
(`.track-list li:has(.btn-like.liked) .btn-delete`), so it survives the
button-only swap unchanged. This freed `tracks.html` to take a
`show_delete` flag: `true` for the card embed and `GET /concerts/:id/tracks`,
`false` for the detail page's bottom list.

`Player.deleteTrack()` (player-bar trash, `static/player.js`) consumes the
same card response: it swaps `#concert-{id}` and preserves the page's
expanded/collapsed state (when the list was collapsed, it empties
`#tracks-{id}` and drops `tracks-open` from the fresh card).

## Swap flows

| Action | Target ⇐ response |
|---|---|
| GET /tracks (toggleTracks) | `#tracks-{id}` ⇐ track list (unchanged) |
| POST .../delete (htmx trash) | `closest .card` ⇐ full card, list expanded, count fresh |
| POST .../delete (last track) | `closest .card` ⇐ card in not-split state (split record cleared) |
| POST .../delete (player bar) | manual `#concert-{id}` swap, expansion state preserved |
| POST .../like (htmx star) | `this` ⇐ like button only |
| POST .../like (player bar) | response ignored (in-place class flip, unchanged) |

## Card track-list state

```
                       toggleTracks click            delete (htmx or player, tracks remain)
  COLLAPSED  ──────────────────────────▶  EXPANDED ──────────────────────────────┐
  (#tracks-{id} empty,   ◀──────────────  (list in #tracks-{id},                 │ card re-rendered
   no .tracks-open)    toggleTracks click  card has .tracks-open)  ◀─────────────┘ EXPANDED, fresh count

  EXPANDED ── delete last available track ──▶ NOT-SPLIT CARD (split state cleared:
                                              no tracks row, Split button back)
```

## Files

- `templates/tracks.html` — `show_delete` guard; delete targets `closest .card`; like via include
- `templates/like_button.html` — new self-swapping star button
- `templates/concert_card.html` — `tracks-open` class + embedded list when `tracks` non-empty
- `templates/concert_detail.html` — bottom list rendered with `show_delete = false`
- `src/web/handlers.rs` — `render_card_with_tracks`, `RowTemplate.tracks`,
  `TracksTemplate.show_delete`, `LikeButtonTemplate`; `delete_track`/`like_track` responses
- `static/player.js` — `deleteTrack()` card swap
- `e2e/delete-track.spec.js`, `e2e/player-queue.spec.js` — updated/new coverage

## Tests

- Unit (handlers tests): card render with embedded tracks keeps the fresh
  `(count/total)` text, `tracks-open`, delete buttons, and the Archive button;
  `show_delete=false` drops trash icons but keeps listen/like; like button
  star states.
- e2e: count refresh after delete (listing + detail card), wired re-swapped
  delete buttons, detail bottom list has no trash icons, delete-all clears the
  split state, player-bar delete on a collapsed detail card refreshes the
  count without expanding the list.

Known pre-existing flake (unrelated): `player-queue.spec.js` "Inline video"
fold tests occasionally fail in full-suite runs (fails identically on a clean
tree); pass in isolation.
