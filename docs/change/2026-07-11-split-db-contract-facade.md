# Split SQLite Persistence: Contract the Facade

Implements [#68](https://github.com/gregwebs/tiny-desk-splitter/issues/68),
the *contract* step of the wider `db.rs` domain split
([#69](https://github.com/gregwebs/tiny-desk-splitter/issues/69)), following
[#66](https://github.com/gregwebs/tiny-desk-splitter/issues/66) (migrate web
callers) and [#67](https://github.com/gregwebs/tiny-desk-splitter/issues/67)
(migrate non-web callers), both merged. This is the last step: delete the
temporary compatibility re-exports and document the resulting module
structure. No SQL, event, transaction, or error-handling behavior changed.

## Scope

`concert-tracker/src/db/mod.rs` still carried a `pub use` block re-exporting
every domain function/type at the top level (`db::get_concert`, `db::open`,
`db::NewListing`, ...), marked "TEMPORARY ... Removed by #68" since #63. By
the time #66/#67 landed, zero production code referenced it — only four
`#[cfg(test)]` modules still imported through it, three of them (`db/mod.rs`
itself, `connection.rs`, and inline test modules in `handlers.rs` /
`web_integration.rs`) resolving facade names via `use super::*` or a grouped
`use crate::db::{self, MetadataUpdate, NewListing}` import.

4 files changed:

- `src/db/mod.rs` — deleted the 32-line re-export block; the shared
  `#[cfg(test)] pub mod tests` helpers (`events_for`, `listing`, `seed`,
  `seed_with_album`) now import `upsert_listing`, `get_concert_by_url`,
  `update_metadata`, `NewListing`, `MetadataUpdate` directly from
  `super::concerts` instead of relying on the removed re-exports plus
  `use super::*`. Also folded the dependency-direction doc comment's
  refactor-history narration ("pinned down here per #63, updated per #64...")
  into a pointer at the new `docs/backend-persistence.md`, keeping only the
  concert-reads-during-migration cycle constraint inline since it's a
  constraint on `db::connection` specifically.
- `src/db/connection.rs` — its test module's `use crate::db::{get_concert,
  mark_download_succeeded, set_notes, try_mark_download_started};` split into
  `use crate::db::concerts::{get_concert, set_notes};` and `use
  crate::db::lifecycle::{mark_download_succeeded,
  try_mark_download_started};`.
- `src/web/handlers.rs` and `tests/web_integration.rs` — each had one grouped
  import, `db::{self, MetadataUpdate, NewListing}`, left over from #66
  because the call sites inside those test modules already used
  domain-qualified paths (`db::concerts::upsert_listing(...)`) and only the
  *type* import stayed on the facade. Re-pointed to `db::{self,
  concerts::{MetadataUpdate, NewListing}}`.

No other file under `concert-tracker/src`, `concert-tracker/tests`, or
`concert-tracker/examples` referenced a top-level `db::` operation path —
confirmed by a repo-wide grep for all 8 facade-owned types and every
previously re-exported function name, scoped to exclude the 9 legitimate
`db::<domain>::...` prefixes. That grep is scoped to production/test code
only. Several `docs/change/*.md` entries dated before this change (e.g.
`2026-07-10-split-db-expand-module-shell.md`,
`2026-06-17-backfill-media-duration.md`) still name pre-refactor top-level
paths such as `db::open` or `db::get_split_timestamps` — left as-is
intentionally: per `AGENTS.md`, `docs/change` is a snapshot-in-time record of
what was true when each entry was written, not a canonical reference kept in
sync with the current API, and rewriting a historical entry's code paths
would misrepresent what that past change actually did. `docs/data.md` and
`docs/backend-persistence.md` (the canonical, currently-maintained
persistence docs) contain no top-level facade references.

## Documentation

Added `docs/backend-persistence.md`: the canonical persistence-layer
reference named in #69's design notes. Covers the 9-module map with type
ownership (`NewListing`/`MetadataUpdate` → `concerts`, `Theme`/`Settings` →
`settings`, `StoredSplitTimestamps` → `split_timestamps`,
`PlaylistError`/`PlaylistMembership` → `playlists`, `FailedJob` →
`failed_jobs`), the `connection`↔`events`↔`concerts` dependency-direction
constraint (moved out of `db/mod.rs`'s header comment, which now points at
this doc instead of repeating it), event-emission invariants (unconditional
vs. guarded-transition emission — a guarded no-op emits no event), the two
`db::playlists` transaction-scoped operations
(`add_playlist_item`/`reorder_playlist_items`), and an ASCII state diagram of
the shared started→succeeded/failed→cleared shape behind
download/split/archive lifecycles. Linked from `README.md` next to the
existing `docs/data.md` link. Where `docs/data.md` already documents schema
columns, the full event list, and lifecycle-transition prose in detail, the
new doc points there rather than duplicating it, per this repo's
one-canonical-place-per-fact convention.

## State changes

None. Deleting unused re-exports and fixing test imports to name their real
source module is a pure compile-time change; no SQL, event emission,
transaction scope, or timestamp format was touched.

## Verification

- Repo-wide grep for every facade symbol qualified as a bare `db::<name>`
  (excluding the 9 domain-module prefixes): zero matches in `src/` and
  `tests/`.
- `cargo check --workspace` — passes.
- `cargo test -p concert-tracker` — 479 lib tests + 76 integration tests, all
  passing (same counts as the #67 baseline, confirming no behavior change).
- `just lint` — `cargo fmt --check`, `cargo clippy --workspace --all-targets
  -- -D warnings`, shellcheck, and the frontend TypeScript/oxlint checks all
  pass with no warnings.
- Codex review of this change against #68's acceptance criteria found two
  issues, both fixed before the PR: (1) `docs/backend-persistence.md`
  incorrectly claimed `execute_batch` runs each migration step as one
  implicit transaction — verified against `rusqlite::Connection::execute_batch`
  (prepares and steps statements one at a time, no `BEGIN`/`COMMIT`) and the
  migration SQL files (no explicit transaction wrapping); corrected to
  describe per-statement autocommit and why migrations rely on idempotency
  instead of rollback. (2) This document's original wording implied the
  "zero references" grep covered docs, when it was scoped to
  `concert-tracker/src`/`tests`/`examples`; reworded above to state that
  scope explicitly and to explain why pre-#68 `docs/change` entries
  intentionally keep their original (now-historical) top-level `db::` paths.
