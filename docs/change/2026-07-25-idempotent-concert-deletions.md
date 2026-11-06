# Idempotent, interface-owned concert media deletions

Issue [#165](https://github.com/gregwebs/tiny-desk-splitter/issues/165) makes
concert media deletion an explicit `Concerts` operation. It continues
[Playback, listen, and like behaviour behind Concerts](2026-07-25-playback-listen-and-like.md).

## Changes

- Added `Concerts::delete_download`, `delete_redundant_source`, `delete_split`,
  `delete_track`, and `delete_interlude`, each returning a `ConcertState`-bearing
  `DeletionOutcome`.
- Added `DeletionConfirmation` (`Unconfirmed`/`Confirmed`) for the one deletion
  that needs it: repairing a recorded download whose file is already gone.
- Added `CommandRejection::SourceNotRedundant` so the coverage gate is a typed
  rejection instead of an outcome the adapter has to interpret.
- Made deletion idempotent at its source: `delete_download` on a concert with no
  recorded download, `delete_split` on a concert with no split state, and a
  repeated `delete_track`/`delete_interlude` all succeed without recording a
  second event.
- Moved interlude deletion out of the handler into
  `lifecycle::delete_interlude`, and moved canonical media mutation into
  Concert Media Inventory (`remove_track_files`, `remove_interlude_files`).
  Track deletion now removes the media whatever extension it carries instead of
  probing only `mp4`/`m4a`.
- Migrated the five delete handlers to the interface; `AppError` gained a
  `Conflict` variant for the coverage gate's 409.

## Preserved behaviour

- The delete-download confirmation step is unchanged: an unconfirmed request for
  a record whose file is gone still renders the confirm prompt, and
  `?force=true` still completes it and swaps the card.
- Successful deletions still swap the card in place (or, for interludes, the
  reconstruction sidebar) rather than refreshing the page, so the persistent
  player keeps playing.
- The coverage gate still answers 409 with the same explanation.
- `delete-split` no longer answers 400 for a concert with no split state. That is
  the approved retry semantics: the desired end state already holds.

## Tests

- `concerts.rs` covers: deletion, repeated deletion, confirmation vs. confirmed
  repair of a vanished file, the coverage gate rejecting then succeeding, split
  deletion retries, track deletion removing media and recording one event,
  invalid track identity rejecting, and interlude deletion including its retry.
- `lifecycle.rs` keeps its existing operation-level cover for the same paths.
