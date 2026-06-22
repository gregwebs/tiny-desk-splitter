# Foldkit Story/Scene tests for the splitter widget

## Summary

The splitter was the first widget ported to [Foldkit](https://foldkit.dev) (Effect-TS MVU)
(`docs/change/2026-06-19-foldkit-eval.md`'s spike), but it shipped without any model-level or
view-level tests — only the pure `core.ts` unit tests (`js-tests/splitter.test.ts`) and the
Playwright e2e suite (`e2e/splitter.spec.js`) covered it. The add-to-playlist panel, ported
second, picked up Foldkit's own `Story`/`Scene` test harness
(`docs/change/2026-06-21-foldkit-add-panel.md`). This change brings the splitter widget up to
the same standard, adding:

- `concert-tracker/frontend/src/splitter/widget/update.story.test.ts` — 24 `Story` cases
  exercising every branch of `update.ts`: load success/empty/failure, drag press/move/release
  (including the linked-boundary co-move and the not-dragging no-op), time-input parsing (valid
  and unparseable), boundary detach/link (including gap collapse), audition (and its
  while-dragging no-op), submit/reset with the full `handleSplitJobResult` status-code branching
  (202 queued, 200 no-op, 409 conflict-resync, other-as-error), all three revert-edit outcomes,
  resync, and playhead updates.
- `concert-tracker/frontend/src/splitter/widget/view.scene.test.ts` — 8 `Scene` cases over
  `view.ts`: the `Ready` toolbar/table/boundary controls, the three non-`Ready` phase messages,
  a surfaced `StatusError`, a real click-driven Detach → Link → Detach round trip through
  `update`, and both preview-unavailable note variants (unsupported format vs. missing file).

No production code changed — `core.ts`, `update.ts`, `view.ts`, etc. are untouched.

## Why

`update.ts`'s status-code branching, the clone-then-mutate editor boundary (`withClonedEditor`),
and the drag state machine are exactly the class of behavior Foldkit's `Story` harness is built
to pin down without a browser: feed a Model + a Message, assert the resulting Model and which
Commands fired, resolve each Command, assert again. That coverage existed for the add-panel but
not the splitter, despite the splitter being the original spike.

## Verification

- `cd concert-tracker/frontend && npm run test:story` — all 4 vitest files (2 existing + 2 new)
  green, 46 tests total (24 + 8 new, 10 + 4 existing for the add-panel).
- `just test-ts` — the 32 `node:test` unit cases (`js-tests/*.test.ts`) plus the 46 Story/Scene
  cases all pass together.
- `just lint` — fmt, clippy, `tsc --noEmit` (both the frontend and `js-tests` tsconfigs, so the
  new test files type-check), the esbuild rebuild, and the `ts-verify` diff guard on
  `static/player.js` are all clean. `static/splitter.js` is unaffected (test files aren't part
  of the esbuild entry graph).
