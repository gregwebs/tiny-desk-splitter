# Spacebar toggles active player playback

## Problem

The persistent web player could be paused with the player button, the currently
playing track button, or by clicking the inline video. Keyboard users expected a
page-level Space shortcut to toggle active playback, but Space either scrolled
the page or triggered the currently focused control. Two common focus states
needed special handling: after pressing Watch, focus remains on the Watch button,
and when the inline video itself is focused, Space should still control playback.

## Fix

Add a document-level `keydown` shortcut in `concert-tracker/static/player.js`.
The handler is bound once from `Player.init()` and only responds to plain Space
when the player has active media loaded.

The shortcut toggles playback through the same `togglePause()` path as the
player button and inline-video click. It does not alter the queue, change the
currently playing item, fold the inline video panel, or record any database
event. Existing media `play` / `pause` event handling updates the play/pause
button icon.

Ordinary page/form targets keep their normal keyboard behavior. The keyboard
ignore predicate follows the same interactive-target pattern used for inline
video click dismissal and also ignores `contenteditable` fields. The player
Watch button and the inline video element are explicit playback shortcut targets,
so Space toggles playback in those common player-focused states. The video
element is keyboard-focusable via `tabindex="0"`.

No database data or schema changes are involved, so no database backup is
required.

## State changes

```text
Global keydown: plain Space

Current state                       Focus target                 Result
----------------------------------  ---------------------------  -----------------------------
No player/media loaded              page/body                    no-op
Media playing, audio-only           page/body                    pause
Media playing, video panel closed   page/body                    pause, panel remains closed
Media playing, video panel open     page/body                    pause, panel remains open
Media paused with active source      page/body                    play
Media playing or paused              player Watch button          toggle playback, panel state unchanged
Media playing or paused              inline video                 toggle playback, panel state unchanged
Media playing                       ordinary input/button/link   global shortcut ignored
Media playing                       player seek control          global shortcut ignored
Focused play/pause button           Space                        native button behavior
Modified Space                      page/body                    no-op
Held/repeated Space                 page/body                    first keydown toggles; repeats do not toggle
```

```text
Playback state

Playing
  -- plain Space outside editable/interactive UI -->
Paused

Paused
  -- plain Space -->
Playing
```

## Verification

`e2e/player-queue.spec.js` adds a `Player keyboard shortcuts` group covering:

- body-focused Space pauses an audio track and updates the play button;
- body-focused Space pauses inline video while leaving the panel open;
- body-focused Space resumes paused media;
- Space after pressing Watch pauses inline video while leaving the panel open;
- video-focused Space toggles playback;
- Space focused on the seek control does not trigger the global pause shortcut;
- Space inside `contenteditable` text does not trigger the global pause shortcut;
- modified Space does not trigger the global pause shortcut;
- repeated Space keydown is prevented but does not toggle playback again;
- Space before media is loaded does not activate the player bar.

The tests explicitly focus `document.body` before exercising the global shortcut
so they do not accidentally test native button behavior from the previously
clicked track button.
