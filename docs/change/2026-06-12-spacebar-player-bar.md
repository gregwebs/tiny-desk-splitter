# Space pauses from any player-bar control

## Problem

After clicking a player-bar button (queue toggle, Next, Prev, Like, Delete,
title/track spans), focus stays on that element. The global Space shortcut
(added 2026-06-07) excluded all interactive targets, so Space re-activated the
focused button instead of pausing — clicking the queue toggle and then pressing
Space would re-toggle the sidebar rather than pause playback.

The same issue affected `#player-video-close`: clicking to minimize the video
would leave Space reopening the panel on the next press.

## Fix

Changed `isPlayerPlaybackShortcutTarget()` in `concert-tracker/static/player.js`
to treat the entire `#player-bar` and `#player-video-panel` as playback-shortcut
territory, matching YouTube/Spotify's Space-always-pauses convention.

`#player-seek` is unaffected because `isEditableTarget` catches `<input>` before
the `closest()` check. The inline video `<video>` element is caught by the
existing `target === audio` guard.

Removed Space from the inline `onkeydown` handlers on `#player-track` and
`#player-title` spans in `layout.html`. These are `role="button"` spans whose
inline handler previously called `preventDefault()` before the document listener
could see the event. Enter still toggles the sidebar (a11y preserved).

## State changes

```text
Global keydown: plain Space, media loaded

Focus target                          Before this fix              After
------------------------------------  ---------------------------  --------------------
page/body                             toggle playback              unchanged
#player-watch / inline video          toggle playback              unchanged
#player-queue-toggle                  re-toggles sidebar (bug)     toggle playback
#player-next / #player-prev           skips again (bug)            toggle playback
#player-like / #player-delete         re-fires action (bug)        toggle playback
#player-play-pause                    native re-click (same result) global toggle (same result)
#player-title / #player-track spans   opens/closes sidebar (bug)   toggle playback
#player-video-close                   re-opens video (bug)         toggle playback
#player-seek (range input)            ignored (native)             unchanged (ignored)
input/textarea/contenteditable        ignored (native typing)      unchanged
Enter on any player-bar button        native activation            unchanged
controls outside the player bar       ignored (native)             unchanged
```

## Verification

Added tests to `e2e/player-queue.spec.js` in the `Player keyboard shortcuts`
group:

- Space after clicking queue toggle pauses without re-toggling the sidebar
- Space after clicking queue toggle (sidebar closed) pauses without re-opening sidebar
- Space with focus on `#player-title` pauses without toggling the sidebar
- Enter on focused `#player-title` still toggles the sidebar (a11y regression guard)
- Space on focused play-pause button toggles exactly once (no double-toggle)
- Space on focused video-close button pauses without re-opening the video panel
- Space on focused Next button pauses without skipping again
