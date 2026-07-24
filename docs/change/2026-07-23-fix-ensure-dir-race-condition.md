# Fix `ensure_dir` TOCTOU race causing intermittent CI failures

## Status

Complete.

## Trigger

Discovered while getting CI green on PR #155 (an unrelated Playwright
flaky-test fix, issue #135). The `rust` job failed twice in a row with:

```
thread 'concert_split::tests::failed_first_split_publishes_completed_tracks_as_partial'
panicked at live-set-song-splitter/src/concert_split.rs:949:54:
called `Result::unwrap()` on an `Err` value: Failed to create directory: temp_frames

Caused by:
    File exists (os error 17)
```

PR #155 touches only `e2e/player-queue.spec.js` and a docs file — no Rust
code — so this failure could not have been caused by that change. Confirmed
this test passes on the latest `main` CI run at the time
(`95d8a22e753cea143a63694e5a6b768f92d899c5`), so it's an intermittent,
pre-existing bug rather than a fixed break — not "difficult" enough to
require a ticket first per the project's Bug Investigation workflow, since
the root cause and fix below are simple and immediately actionable.

## Root cause

`live-set-song-splitter/src/io.rs`'s `ensure_dir` used a check-then-create
pattern:

```rust
if !fs::exists(&path)? {
    fs::create_dir(&path)?;
}
```

This is a classic TOCTOU (time-of-check-to-time-of-use) race. `concert_split`
calls `io::ensure_dir("temp_frames")` (a fixed, non-test-unique relative
path — only the per-concert subdirectory `temp_frames/<album>` is unique,
see the comment at `concert_split.rs`'s test module) before deriving its
per-test scratch subdirectory. `cargo nextest` runs tests in parallel; when
two tests both call `ensure_dir("temp_frames")` close enough together, both
can observe "does not exist", and the loser's `fs::create_dir` then fails
with `ErrorKind::AlreadyExists` (os error 17). This is scheduling-dependent,
so it doesn't fail on every run — consistent with passing on `main`'s latest
CI run but failing (identically, twice in a row) on a different run/commit
with different concurrent test scheduling.

## Fix

`live-set-song-splitter/src/io.rs`: `ensure_dir` now performs a single
`fs::create_dir` and treats `ErrorKind::AlreadyExists` as success, removing
the check-then-act window entirely — this is the standard idempotent-mkdir
pattern. `overwrite_dir` (a separate function, remove-then-create semantics)
is unchanged: it isn't implicated in the observed failure and its callers'
paths are already documented as test-unique.

Added a regression test, `io::tests::ensure_dir_is_idempotent_under_concurrent_calls`:
spawns 16 threads all calling `ensure_dir` on the same fresh path inside a
`tempfile::tempdir()`, joins them, and asserts every call returned `Ok` and
the directory exists. This directly exercises the race window the old
implementation had.

## Verification

- `cargo test -p live-set-splitter --lib io::` — new test passes.
- `cargo nextest run --tests` (full workspace, matching `just test-rs`) —
  837/837 passed.
- `cargo fmt --check` — passed.
- `cargo clippy --workspace --all-targets -- -D warnings` — passed.
