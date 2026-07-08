# Fix flaky `wait_for_split` test helper

## Problem

`download_double_click_does_not_drop_split_edge` failed once (but not
reliably) with:

```
assertion `left == right` failed
  left: String("splitting")
 right: "split"
```

This was a filesystem-vs-database race in the test helper `wait_for_split`
(`concert-tracker/tests/web_integration.rs`), not a product bug.

`GET /concerts/:id/prepare-status` (`concert-tracker/src/web/handlers.rs`)
mixes two sources of truth by design: `tracks_present` is checked against the
filesystem, while `split` is read from the database. The split job
(`concert-tracker/src/jobs/split.rs::run_split`) writes the track file to disk
first, then only afterwards locks the DB and calls `mark_split_succeeded`.
`wait_for_split` polled solely on `tracks_present`, so a poll landing in the
(normally sub-millisecond) window between the file appearing and the DB write
would return while the DB still said `splitting`, failing the very next
`split == "split"` assertion. All four callers of `wait_for_split` shared this
latent race; the double-click test just happened to hit it.

## Change

`wait_for_split` now waits for **both** all tracks present on disk **and**
`split == "split"` in the polled response before returning, so it can no
longer hand control back mid-race.

## State transition

| Poll observes | Old behavior | New behavior |
|---|---|---|
| Track file(s) on disk, DB still `splitting` | Returns immediately (race window) | Keeps polling |
| Track file(s) on disk, DB `split` | Returns | Returns |

## Verification

- `cargo check -p concert-tracker --tests`
- `cargo test -p concert-tracker --test web_integration` — 75 passed, 0 failed
- Stress loop: ran `cargo test -p concert-tracker --test web_integration download_ -q`
  40 times in a row with no failures, to confirm the race window is closed
- `just lint` — clean
