# Splitter Timeline UI

## Problem

The user-split-timestamps API (`docs/change/2026-06-12-user-split-timestamps.md`)
let the backend re-cut a concert from user-supplied per-track timestamps, but
there was **no UI** for it — users could only accept the automatic split. This
change adds an inline timeline editor to the concert detail page so users can
nudge track boundaries and cut out talking between songs.

## What Changed

### Inline splitter on the concert detail page

A collapsible "Edit track splits" section appears on `concert_detail.html` when
the concert is **downloaded** and has a **non-empty set list**. It renders a
horizontal timeline spanning the full source duration, with a draggable handle
for every boundary plus a synced numeric table (`m:ss.s`) for precise entry.

- **Linked vs detached boundaries.** Adjacent tracks share one split point by
  default — a single handle that is simultaneously the end of track *i* and the
  start of track *i+1*. **Detach** splits it into two independent handles so the
  user can open a **gap** that belongs to no track (talking between songs).
  **Link** collapses the gap again.
- **Head/tail handles** trim the intro/outro (the server does not require the
  first track to start at 0).
- **Audio preview.** Clicking the timeline seeks + plays the album audio; each
  table row has ▶ buttons to audition a cut point. Preview routes through the
  global player bar (`Player.playAlbumAt`) so the now-playing title, seek bar,
  and timeline playhead all reflect the audition. Auditioning never records a
  listen event. Preview buttons are disabled (with a note) when the source
  isn't browser-playable (e.g. `.mkv`), but the editor still works.
- **Submit / Reset.** Submit POSTs to `/concerts/:id/split-timestamps`; Reset
  POSTs to `.../reset`. On `202` the concert card is refreshed so its existing
  in-progress badge + 3s polling reflect the running split. A reset `200`
  (`already-auto`) is surfaced as a no-op message. `409`/`422` bodies are shown
  inline; a `409` (busy) re-syncs the editor from a fresh GET.

### Boundary state diagram (interior boundary *i*)

```
 linked ──[Detach]──▶ detached (gap = 0)
   ▲                      │  drag handles apart
   └──────[Link]──────────┘  end[i] < start[i+1]   (gap belongs to no track)
```

### Backend: `media_duration` on the GET response

`GET /concerts/:id/split-timestamps` now returns `media_duration: Option<f64>`
(from the same `ffprobe_duration` the POST path uses). The timeline scale and the
`end <= duration` clamp are driven by this value — **independent of**
`media-info.playable` — so the editor works for valid-but-unplayable sources
(`.mkv`) and avoids browser-vs-ffprobe duration drift triggering a surprise
`422`. It degrades to `null` (the editor falls back to the last end time) when
the source is absent or `ffprobe` fails, rather than failing the request.

## Files changed

- `concert-tracker/src/web/handlers.rs` — `media_duration` field on
  `SplitTimestampsResponse`; GET handler restructured to release the DB lock
  before the async ffprobe; new `splitter_js()` static handler; serialization
  test.
- `concert-tracker/src/web/mod.rs` — `/static/splitter.js` route.
- `concert-tracker/static/splitter.js` *(new)* — timeline module. Pure,
  DOM-free helpers (`parseTimecode`/`formatTimecode`/`setStart`/`setEnd`/
  `detach`/`link`/`validate`/`buildPayload`, exposed under `_pure`) with a thin
  DOM/interaction layer over them.
- `concert-tracker/static/player.js` — `playAlbumAt(concertId, seconds)` starts
  whole-album playback and seeks to a position without recording a listen event;
  `nowPlaying()` returns a snapshot `{concertId, trackIdx}` for external callers;
  `recordListen` flag threaded through `play()` and `startAlbum()` to suppress
  the listen POST when called from the splitter.
- `concert-tracker/templates/concert_detail.html` — gated splitter section +
  container.
- `concert-tracker/templates/layout.html` — `splitter.js` script tag + splitter
  CSS.
- `js-tests/splitter.test.js` *(new)* + `package.json` `test:unit` script —
  `node --test` unit tests for the pure helpers.

## Testing

- Rust: `cargo test -p concert-tracker` (incl. the new serialization test).
- JS units: `npm run test:unit` (12 tests over the pure helpers, incl. N==1,
  linked/detached clamps, overlap/short/out-of-bounds validation).
- E2E (Playwright, `./e2e`): drag a linked handle (both sides move, `<1s`
  blocked), detach + open a gap + submit (`202`, re-cut reflects the gap),
  ruler click seeks/plays, numeric two-way sync, reset (`202` vs `200`
  `already-auto`), `.mkv`/non-playable source (editor renders, preview
  disabled), busy guard (`409` surfaced, controls disabled).

## Known limitation

The race documented in `2026-06-12-user-split-timestamps.md` (a queued Analyze
split can overwrite a user split) is **mitigated** by disabling Submit/Reset
while a split/download is in flight and surfacing `409 AlreadyRunning`, but not
eliminated.
