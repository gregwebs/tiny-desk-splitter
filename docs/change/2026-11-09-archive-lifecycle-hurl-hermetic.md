# Make `hurl/archive_lifecycle.hurl` hermetic (#193)

`hurl/archive_lifecycle.hurl` was the one non-hermetic file in the suite: it
configured `archive_location: /tmp/tds-hurl-archive`, the archive Job Run moved
the concert's directory there under the seeded album name `Archive Lifecycle`,
and nothing ever removed it. When a run aborted before the unarchive case (for
example because the archive Job Run missed the file's 1.5 s retry window while
Playwright ran in another shell), the next run's archive failed on the leftover:

```text
WARN concert_tracker::jobs::run: archive failed: Directory not empty (os error 39)
    key=JobKey { concert_id: 1, kind: Archive }
```

`rm -rf /tmp/tds-hurl-archive` was the only way out, and every run in between
failed at the same place.

## Fix

**A per-run scratch location.** `scripts/hurl-test.js` gives each run one scratch
root (`mkdtemp`) that now holds the DB, the workdir, and an `archive/` directory
outside the workdir, so no two runs can collide and a crashed run costs nothing.
The archive location reaches the file as the `{{archive_location}}` Hurl variable,
exactly like `{{base_url}}`/`{{test_control_url}}`; no `.hurl` file names a fixed
path of its own. The whole root is removed in `cleanupServer`, which is also the
`Ctrl-C`/startup-failure path — one cleanup for everything the run created. The
root is handed to `activeServer` before the first step that can throw, so a
failed `mkdir`/`spawn` still removes it instead of leaking it.

**Wait on the Job Run's terminal event, not the page.**
`POST /test/assert/concert_events` now polls for `present: ["archived"]` with
`absent: ["archive_error"]` — the terminal-state assert the README documents for
facts with no public HTTP surface, and the only one available here: archive is a
`JobKind`, not one of the Job Driver's `download`/`split`/`open` steps, so
`test.assert_job_observation` cannot see it. The window is 100 × 200 ms (20 s)
rather than 30 × 50 ms (1.5 s): enough for real filesystem work under load.
Unarchive waits the same way, on `archive_delete` (recorded when the unarchive
commits its cleared state). The listing assertions themselves no longer retry —
they run once, after the state is known to be terminal.

These two polls assert `jsonpath "$.error.message" not exists` rather than the
suite's usual `$.result.ok == true`. Hurl retries a failed assertion, so a
failing Job Run still costs the full window; what the idiom buys is the
diagnosis. Hurl prints the value it found, so the failure names both conditions
(same text, from a `/dev/null` archive location, with Hurl's own file/line gutter
replaced by `...`):

```text
   | POST {{test_control_url}}/test/assert/concert_events
   | ...
   | jsonpath "$.error.message" not exists
   |   actual:   string <concert 1 event mismatch: expected event "archived" to be present, found none; expected event "archive_error" to be absent, found 1>
   |   expected: not something
```

— a failed Job Run says so, instead of an opaque `actual: none` against
`$.result.ok` that reads the same whether the job is pending or broken.

## The issue's third symptom, which does not reproduce

The issue asked to clear `archive_location` at the top of the file as well as at
the end, on the premise that "settings survive `test.reset`". They do not:
`reset_test_data` ends with `UPDATE settings SET archive_location = NULL, theme =
'system'`, with a unit test asserting it
(`reset_clears_concerts_and_settings_but_leaves_the_settings_row`). This file
already begins with `test.reset`, so a top-of-file clear would be a no-op; the
premise is corrected in the file's header comment instead of adding the request.

That also bounds the claimed cascade: an abort after the location is configured
does leave it configured for the rest of the run (the later files mostly do not
reset), but no later `.hurl` file asserts anything about the Archived chip or
`filter=archived` today, so nothing breaks. The trailing clear stays: it leaves
the shared settings row as this file found it instead of relying on the later
files not minding, and it now runs on a file that no longer aborts on timing.

## Verification

No product code changed; `concert-tracker` and the frontend are untouched.

- `node scripts/hurl-test.js --glob 'hurl/archive_lifecycle.hurl'` — pass.
- Full suite `node scripts/hurl-test.js` — 14 files, 100%. (Request totals are
  not quoted: a retried poll changes them between runs.)
- Repeated under load (four busy loops on four cores) — the file passed 3/3,
  including attempts whose poll had to retry.
- The poll still bites: a throwaway file that seeded a concert and never archived
  it failed after its retries, printing the server's mismatch text in the
  assertion error, so the wait is a real check and not a vacuous `ok`.
- A `/dev/null` archive location (a Job Run that fails on the filesystem) prints
  `expected event "archive_error" to be absent, found 1` in the failure after the
  full window — the documented cost of having no fail-fast terminal probe.
- Abort safety, probed two ways: forcing a throw right after the scratch root is
  created, and pointing the spawn at a nonexistent executable. Both exit with
  the error and leave no `/tmp/tds-hurl-*` behind. The second probe needed a fix
  of its own: a spawn that never started emits `error`, not `exit`, so `exited`
  now covers that event and `stopServer` settles on either (otherwise cleanup
  waited for an `exit` that could never come).
- `Ctrl-C` during a run: the message loop finishes, the handler runs, and the
  scratch root is gone. No `/tmp/tds-hurl-*` scratch root survives a normal run
  either; the stale pre-change `/tmp/tds-hurl-archive` has been removed, and
  nothing references that path any more.
- The pre-existing leftover at the old fixed path is now unreachable: the file has
  no fixed path of its own, and `{{archive_location}}` used without the runner
  fails loudly (`you must set the variable archive_location`) rather than
  silently writing somewhere else.
