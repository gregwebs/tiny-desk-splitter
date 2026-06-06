# Dismiss the video panel by clicking outside it, plus an on-hover minimize button

## Problem

The JS player reveals an inline video in `#player-video-panel` when the user
clicks "Watch". Folding the video back down (a minimize — audio keeps playing)
was only possible by clicking "Watch" again on the player bar, a small target
that is easy to miss. Two gaps:

1. The panel is anchored to the bottom of the viewport and, for a landscape clip
   on a tall screen, is shorter than the viewport — leaving empty page area
   above it that did nothing when clicked.
2. When the video fills the whole viewport (tall clip / small screen) there is no
   empty area to click at all, and no in-video control to dismiss it.

## Fix

Two behaviors, both folding the panel via the existing `hideVideoPanel()` (which
only removes the `.open` class — the `<video id="player-audio">` stays laid out
and keeps playing audio).

### Click-outside-to-minimize (`player.js`)

A document-level `click` listener folds the panel when the click lands on **dead
space** outside the player. Decided with the user: a click on an *interactive*
element (link, button, card control) performs its own action and leaves the
video open — this preserves the existing "panel stays open across navigation"
behavior and matches the user's "click the empty space above" intent. The
decision is factored into a small pure predicate (covered by the e2e tests
below; the project has no JS unit-test harness):

```js
const INTERACTIVE_SELECTOR =
  'a, button, input, select, textarea, label, [role="button"], [onclick]';

function clickShouldDismiss(target, container) {
  if (!container || !target || container.contains(target)) return false;
  if (target.closest && target.closest(INTERACTIVE_SELECTOR)) return false;
  return true;
}
```

`#player-container` wraps both the video panel and the bar, so clicks on bar
controls never dismiss. The listener is attached **deferred to the next tick** in
`showVideoPanel()` and removed in `hideVideoPanel()`. The defer is required
because `watchTrackDirect()` opens the panel from a track-list "Watch" button
that lives *outside* `#player-container`; attaching synchronously would let that
very opening click bubble up and immediately re-close the panel.

### On-hover minimize button (`layout.html` + `player.js`)

A `#player-video-close` (×) button absolutely positioned in the panel's
upper-left corner. It reuses the already-exported `Player.watch()` toggle (the
button is only clickable while the panel is open, so the toggle folds it) — no
new export. It is hidden (`opacity:0; pointer-events:none`) until JS reveals it:
`showVideoControls()` adds a `controls-visible` class on `mousemove` / `touchstart`
over the panel and clears it after `VIDEO_CONTROLS_IDLE_MS` (2500 ms) of
inactivity. A `:focus` rule also reveals it for keyboard users. Because the panel
is `overflow:hidden` and collapses to `max-height:0`, the button is clipped to
nothing while folded.

## State: what dismisses the open video panel

```
  click on …                         result
  ---------------------------------  -------------------------------------------
  empty page background (dead space)  panel folds (audio keeps playing)
  a link / card button / track ctrl   element's action runs; panel STAYS open
  the video itself                    toggles pause (unchanged); STAYS open
  the × minimize button               panel folds (audio keeps playing)
  player-bar Watch / other controls   Watch toggles; others act; STAYS open
```

```
  minimize button visibility
  --------------------------
  panel folded            -> clipped (max-height:0), not shown
  open, pointer idle       -> opacity 0 (hidden)
  open + mousemove/touch   -> controls-visible -> opacity 1  (2.5s timer)
  open + button focused    -> opacity 1 (keyboard reach)
```

## Verification

`e2e/player-queue.spec.js` ("Inline video") adds five tests:

- clicking dead space (`#content`) folds the video; audio keeps playing;
- clicking an interactive control outside the player (expanding another
  concert's tracks) does **not** fold it;
- the minimize button is hidden while idle, revealed on `mousemove`, and folds
  the panel when clicked (audio keeps playing);
- the minimize button fades back out after the idle timeout;
- opening the panel from a Watch control outside `#player-container` keeps it
  open (guards the deferred-attach against immediate self-dismiss).

The existing "video panel stays open across Back/Forward navigation" test still
passes, confirming navigation is unaffected. The full Inline-video group is
green (1 retry needed for a `--single-process` Chromium crash, a known
sandbox-only flake, not a logic failure).
