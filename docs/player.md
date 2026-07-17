# Player state and media commands

The persistent player is a Foldkit model-view-update widget. Its model describes renderable player
state, while browser media elements remain authoritative for immediate playback state.

## Playback-state invariant

`model.isPlaying` is a projection used to render controls and playback indicators. Native media
`play`, `pause`, and `ended` events are authoritative for successful immediate playback
transitions. Failure and terminal paths may defensively reset the projection to `false`, but
user-action toggle messages do not update it optimistically because `HTMLMediaElement.play()` can
be rejected.

This means `model.isPlaying` can briefly lag behind `#player-audio.paused`. The rapid host
`TogglePause` path therefore reads `audio.paused` inside the same Command effect that calls
`pause()` or `play()`. Explicit pause/resume flows continue to use `PauseAudio` or `ResumeAudio`;
other playback entry points retain their separately documented semantics.

```text
TogglePause host message
        |
        v
ToggleAudio Command ---- reads live audio.paused
        |                         |
        |                         +-- playing --> pause()
        |                         +-- paused  --> play()
        v
native play/pause event
        |
        v
model.isPlaying update --> rendered player controls
```

Keeping the live read and mutation in one Command prevents two rapid toggles from both choosing
the same action from a stale model snapshot. Command-effect tests cover the deterministic media
transition, while Playwright covers host-port delivery and effect scheduling in the real widget.

## Relevant code

- `concert-tracker/frontend/src/player/widget/command.ts`: media Command effects.
- `concert-tracker/frontend/src/player/widget/subscription.ts`: native media event subscriptions.
- `concert-tracker/frontend/src/player/widget/model.ts`: player model and invariants.
- `concert-tracker/frontend/src/player/widget/update/handleHostCommand.ts`: host-port dispatch.
