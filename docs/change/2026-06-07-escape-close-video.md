# Escape key closes the inline video panel

## Problem

The persistent web player can show an inline video in `#player-video-panel`.
It already had three pointer dismissals — clicking the player-bar Watch button
again, clicking the in-panel minimize/close button, and clicking dead space
above the panel (`onOutsideVideoClick`) — but no keyboard way to close it.
Keyboard users expected the conventional Escape-to-dismiss shortcut, matching
the page-level Space play/pause shortcut (see
`2026-06-07-spacebar-pause.md`).

## Fix

Extend the existing document-level `keydown` handler (`onGlobalKeydown` in
`concert-tracker/static/player.js`) to handle plain Escape. When the inline
video panel is open, Escape folds it through the same `hideVideoPanel()` path
used by the Watch toggle and the outside-click dismissal.

Like those dismissals, Escape only collapses the panel — it does **not** stop or
pause playback. The `<video>` is the playing `#player-audio` element, so folding
the panel leaves audio/video playing untouched. Escape does not alter the queue,
change the current track, or record any database event.

Unlike the Space shortcut, Escape deliberately does not apply the full
interactive-target ignore filter: it must still close the panel when focus is on
a control inside the player (for example the in-bar Watch/close button). It only
defers to text-entry targets (`input`, `textarea`, `select`, `contenteditable`)
so native Escape (clear/blur) keeps working there. Modified Escape
(Ctrl/Cmd/Alt/Shift) is ignored, and the handler's existing
`e.defaultPrevented` guard means an Escape already consumed elsewhere is
respected.

Two small refactors support this without duplication:

- `isEditableTarget(target)` factors the text-entry check out of
  `isKeyboardShortcutIgnoredTarget()` so the Space and Escape paths share it.
- `isVideoPanelOpen()` centralizes the `#player-video-panel.open` lookup that the
  new Escape branch and `watch()` both need.

No database data or schema changes are involved, so no database backup is
required.

## State changes

```text
Global keydown: plain Escape

Current state                          Focus target                  Result
-------------------------------------  ----------------------------  --------------------------------
Video panel closed / no media          page/body                     no-op (native Escape)
Video panel open, audio/video playing  page/body                     panel folds, playback continues
Video panel open                       Watch/close button in bar     panel folds, playback continues
Video panel open                       input/textarea/contenteditable  native Escape, panel stays open
Modified Escape (Ctrl/Cmd/Alt/Shift)   any                           no-op
```

## Verification

`e2e/player-queue.spec.js` adds to the `Player keyboard shortcuts` group:

- Escape folds an open video panel while `#player-audio` keeps playing;
- Escape from a control inside the player (focused Watch button) still folds the
  panel;
- Escape with the panel closed is a no-op and does not disturb playback;
- Escape inside a `contenteditable` field does not fold the panel;
- modified Escape (`Shift+Escape`) does not fold the panel.

All five pass. The pre-existing `clicking dead space outside the player folds the
video` test remains flaky under the full parallel run (a `setTimeout(0)`
outside-click listener-attach race in `showVideoPanel()` that predates this
change); it passes in isolation and with `--repeat-each`.
