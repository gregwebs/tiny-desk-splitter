# Port the player to Foldkit; retire ts-verify

## Summary

The audio player was the last imperative bundle in `concert-tracker/frontend`. The splitter
(`docs/change/2026-06-19-foldkit-eval.md`), the add-to-playlist panel
(`docs/change/2026-06-21-foldkit-add-panel.md`), and the playlists host glue were already
[Foldkit](https://foldkit.dev) (Effect-TS MVU) widgets; `player.js` stayed a hand-written,
unminified, diff-guarded artifact. This change ports it to Foldkit and, because that removes the
last reviewable-as-plain-text bundle, retires the `ts-verify` build-artifact diff guard.

Frontend — the old `src/player.ts` imperative module is replaced by a Foldkit widget:

- `src/player/core.ts` — pure player logic extracted first (queue math, now-playing
  resolution, formatting), unit-tested in `js-tests/player-core.test.ts`.
- `src/player/widget/{model,message,update,command,subscription,view,port,widget}.ts` — the MVU
  state machine: a `Model` of player + queue + sidebar-track state, a `Message` algebra, an
  `update` that returns Model + Commands, Commands for the HTTP/audio side effects,
  subscriptions for media events, and a `view`.
- `src/player/index.ts` + `src/player/mirror.ts` — host glue mounting the widget and mirroring
  `nowPlaying` back to the rest of the page.
- `src/player/widget/update.story.test.ts` (Story) and `view.scene.test.ts` (Scene) — Foldkit's
  browserless harness, matching the standard the splitter and add-panel already meet.

Backend — a new endpoint feeds the sidebar track list:

- `GET /concerts/:id/track-details` → `TrackDetailsResponse { tracks_busy, tracks }`, where each
  `TrackDetailItem` carries `{ index, title, available, is_video, liked }`
  (`concert-tracker/src/web/handlers.rs`, `src/model.rs::list_all_track_details`). Wired into
  `web/mod.rs` and the OpenAPI doc (`web/openapi.rs`); generated types regenerated into
  `frontend/src/generated/openapi.d.ts`, consumed via `frontend/src/api/client.ts`.

Build / tooling:

- `frontend/build.mjs` — all three bundles (`player`, `splitter`, `playlists`) now build through
  one minified `es2022` Foldkit config; the separate unminified `reviewableOptions` path is gone.
- `ts-verify` retired — removed from the `justfile`, the `.githooks/pre-push` hook, and the
  build.mjs rationale comment. With every bundle minified, a textual diff guard on the committed
  `.js` is no longer meaningful. The bundles stay committed, so `cargo build` remains Node-free
  (`include_str!` embeds them).
- `templates/layout.html` and `static/style.css` — sidebar/queue markup and styles the widget
  mounts against.

## Why

Foldkit's `Story` harness pins down exactly the behavior the imperative player made hard to
test: queue advancement, now-playing resolution, the like/availability sync, and the
busy-while-splitting gating, asserted as Model + Commands without a browser. Finishing the port
also collapses the build to a single bundle config and removes the `ts-verify` guard that only
ever applied to the one remaining unminified artifact.

The new `/concerts/:id/track-details` endpoint moves per-track availability, the video flag, and
liked status to a single typed response instead of the previous scattered template/imperative
derivation, so the widget can render the sidebar list from one fetch.

## Verification

- `just lint` — fmt, clippy, shellcheck, and `tsc --noEmit` (frontend + `js-tests` tsconfigs).
- `just test-ts` — the `js-tests/*` node unit suites (incl. `player-core.test.ts`) plus the
  Foldkit Story/Scene suites.
- Manual: play/queue/skip, like toggling, and the sidebar track list against a concert with a
  mix of available, missing, and video tracks; confirmed the busy state while a split is queued.
