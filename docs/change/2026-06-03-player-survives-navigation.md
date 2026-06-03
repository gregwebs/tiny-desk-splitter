# In-place navigation keeps the audio player alive

## Problem

Navigating between the listing page (`/`) and a concert detail page
(`/concerts/:id`) — or pressing the browser Back button — caused a brief
audible interruption in the JS player.

`layout.html` had `<body hx-boost="true">`, so every boosted navigation swapped
the **body's children**. Even with `hx-preserve="true"` on `#player-container`,
htmx detached and re-inserted the `<video id="player-audio">` node during the
swap. A detached media element is paused by the browser and drops its buffered
data, so `player.js` had to reload `src` and re-seek. That reload was the gap.

## Fix

Swap only a dedicated `#content` region; keep the player **outside** it so its
media node is never touched. `hx-boost` still handles URL push + Back/Forward.

- `layout.html`: content block wrapped in `<main id="content" hx-history-elt>`;
  `#player-container` is now a **sibling** of `#content` (never inside a swap
  target). `hx-history-elt` scopes history snapshot/restore to `#content` so
  Back/Forward also leaves the player alone.
- Every in-app navigation `<a>` carries the shared set
  `hx-target="#content" hx-select="#content" hx-swap="outerHTML show:window:top"`
  (header links, list filter chips, card titles, Jobs/job-log/delete-confirm
  links).
- The attributes go on the **anchors**, not on `<body>`/`#content`:
  `hx-target`/`hx-select` are inherited by descendants, and putting them on a
  common ancestor would break every action button (POST responses are card
  partials with no `#content`) and every self-targeting poll. Per-anchor
  placement avoids leakage; a missed link merely degrades to the old full-body
  swap rather than breaking.
- `hx-swap="outerHTML"` is required: boosted links default to `innerHTML`, which
  would nest the response's `#content` inside the existing one (duplicate id).
- The Jobs count `<span>` was a child of the `/jobs` anchor; moved to a sibling
  so it doesn't inherit `hx-select="#content"` (which would break its poll).

`player.js` was left unchanged. With the audio node never detached, its old
`navState`/`rebind`/`restorePlayback` workaround is inert (`rebind()`
early-returns while the node stays attached). Removing it is a future tidy-up.

## State: what gets swapped on navigation

```
                    BEFORE (hx-boost body swap)        AFTER (#content swap)
                    ---------------------------        ---------------------
  <body>            swapped (children replaced)        unchanged
    <header>          re-created                         unchanged
    content           re-created                         #content swapped
    #player-container re-created  -> audio DETACHED       unchanged -> audio LIVE
```

| Navigation            | Player node | Audible result |
|-----------------------|-------------|----------------|
| list -> detail        | preserved   | keeps playing  |
| detail -> Back        | preserved   | keeps playing  |
| Back -> Forward       | preserved   | keeps playing  |
| filter chip / header  | preserved   | keeps playing  |

## Verification

`e2e/back-navigation.spec.js` now tags the live `#player-audio` node and asserts
across list→detail, Back, and Back→Forward that (a) the *same* node survives
(`data-nav-marker`), (b) `currentTime` keeps advancing while `paused` is false,
and (c) exactly one `#content` element exists (guards the nesting bug). Full e2e
suite (59 tests) passes — action buttons, polling, queue, like/delete, and the
video panel are unaffected.
