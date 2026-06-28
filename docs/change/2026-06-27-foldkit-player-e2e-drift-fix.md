# Fix Foldkit player e2e drift + restore the queue group-remove button

## Summary

The player Foldkit port (`docs/change/2026-06-25-foldkit-player.md`) renamed and restructured the
queue and sidebar markup but did not update the Playwright e2e suite, and `ci.yml` intentionally
does not run e2e, so the breakage stayed invisible: a swath of `sidebar.spec.js`,
`playlists.spec.js`, and `concert-reconstruction.spec.js` asserted selectors the widget no longer
emits. The port also dropped the group-level queue-remove button (the single "×" that removes a
whole queued playlist group at once), even though the `removeGroup` logic survived in `core.ts`.

This change:

- **Restores the group-remove regression.** A `RemoveGroup` player command (`port.ts`) wired to
  the existing `core.ts` `removeGroup` (`update.ts`), rendered as a `.btn-remove-group` "×" button
  on the queue group-header row, with the group name wrapped in a `.queue-group-name` span
  (`view.ts`). Kept internal (an `OnClick` dispatch like the per-song `Dequeue`); no new public
  `PlayerApi` surface, since nothing outside the widget triggers it.
- **Migrates the drifted e2e selectors** to the Foldkit markup:
  `#sidebar-concert-tracks`→`#sidebar-concert-section`; `.queue-item`→`.queue-song` /
  `.queue-item.nested`→`.queue-song-nested`; `.queue-title` (removed, the title is now the text of
  `.btn-play-queue`); `.btn-queue-remove`→`.btn-remove-queue`; `.btn-queue-play`→`.btn-play-queue`;
  `.queue-group`/`.queue-group-name`→the flat `<ol>` with `.queue-group-header` rows; the group ✕
  click → `.queue-group-header .btn-remove-group`; remove glyph `✕` (U+2715) → `×` (U+00D7).
- **Cleans up the orphaned CSS** (`static/style.css`) that still matched the old class names, and
  preserves the nested-row indent under `.queue-song-nested`.
- **Backfills in-process coverage** so the queue behaviors are pinned without a browser:
  - Story: `RemoveGroup` removes every entry sharing the `groupId`; `PlayQueueEntryNow` dequeues
    and fetches the track; `SkipToNext`/`SkipToPrev` are no-ops (no `PauseAudio`) when there is
    nothing next/prev and the queue is empty (the guard the disabled Next/Back buttons reflect).
  - Scene: the empty-queue state shows the exact text "Nothing queued"; the remove button renders
    the "×" glyph and no trash icon; the badge reads "1" for a single-entry queue; clicking a
    group-header remove dequeues the whole group.

## Why

The drift was a latent failure: every affected e2e test would fail the moment the suite ran,
because the assertions reference DOM the Foldkit widget no longer produces. The group-remove
button was a genuine feature regression, not a rename. Restoring it (the logic already existed)
plus migrating the specs makes the e2e suite reflect reality again, and the in-process backfills
raise Story/Scene/core coverage of the queue to the bar the splitter and add-panel already meet
(`docs/change/2026-06-22-splitter-foldkit-tests.md`).

## Verification

- `just test-ts` — core (68) + Story/Scene (145, including the new player backfills) all green.
- `cargo fmt --check`, `cargo clippy`, `just ts-check` — clean. (`shellcheck` not installed in the
  dev sandbox; no shell scripts changed.)
- `just ts-build` + `cargo build` — `static/player.js` rebuilt and re-embedded; the new
  `.btn-remove-group` / `.queue-group-header` / `.btn-play-queue` classes are present and the old
  `.btn-queue-*` / `.queue-item` / `#sidebar-concert-tracks` tokens are gone.
- Playwright: the migrated `sidebar.spec.js`, `playlists.spec.js`, and `concert-reconstruction.spec.js`
  run against a fresh fixture server (separate `--db`/`--workdir`/port per test).

## Follow-up (out of scope)

- The drift was invisible because CI skips e2e. A tracked follow-up should run the e2e suite (or a
  smoke subset) in CI so a future Foldkit port cannot silently break it again.
- A separate pruning pass will remove the now-redundant Playwright tests whose behavior these
  in-process backfills fully cover, per `docs/change/2026-06-22-prune-redundant-playwright.md`.
