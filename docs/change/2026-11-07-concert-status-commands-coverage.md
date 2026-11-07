# Concert Status and notes command coverage

Issue [#161](https://github.com/gregwebs/tiny-desk-splitter/issues/161) (move
Concert Status and notes commands behind `Concerts`) was implemented with the
rest of the Concert application interface in
[the #177 contraction](2026-07-25-concert-catalog-and-status.md): `set_status`
and `set_notes` are desired-state commands on `Concerts`, the HTMX badge and
notes form are thin adapters over them, and `concert-db want`/`ignore` are
adapters over the same method. `docs/concerts.md` documents the commands, the
state-change diagram, and the rejection/operational-failure split.

Auditing the ticket's acceptance criteria against `main` after that contraction
left one unmet: *"Interface-level tests cover every status transition, retries,
missing concerts, and failure mapping."* Four gaps sat behind that sentence, and
the CLI half of *"Overlapping CLI commands use the same methods and invariants"*
had no automated coverage at all — `concert-db want`/`ignore` was untested, the
same shape of gap #160's audit found in `concert-db list`.

## The gaps

- The interface test walked three of the six directed transitions (Available →
  Wanted → Ignored → Available). Available → Ignored, Wanted → Available, and
  Ignored → Wanted were never exercised, and nothing checked how many
  status-change events each transition records or that the returned Concert State
  agrees with the next observation.
- Notes had no retry case, and nothing pinned that notes and Concert Status are
  independent: writing one must not clear the other.
- Failure mapping had no coverage. Adapters turn a rejection into a client error
  and a fault into a server error; the mapping was only exercised indirectly by
  whichever `prepare` calls other suites happened to make.
- `concert-db want`/`ignore` had no test, so the CLI's deliberate toggle
  behaviour — and the invariant it cannot break underneath — were unwatched.

## Changes

- `concerts.rs` now asserts the full status matrix. For every ordered pair of
  Available, Wanted, and Ignored it checks that the command result is the
  requested status, expressed by exactly one stored intent; that the returned
  state is the concert's actual state (a fresh observation agrees); that the
  transition appends exactly the one event the event-stream contract calls for,
  comparing whole event histories so an extra event of any kind also fails; and
  that repeating the request changes neither the status nor the history. The
  expected event per transition comes from a table written from the preserved
  event contract (`wanted`, `wanted_delete`, `ignored`, `ignored_delete`, or
  nothing), not from the storage code under test.
- The notes test now covers a repeated request and the returned-state equality,
  and a new test pins that notes and Concert Status are independent desired
  state in both directions: notes set while Ignored, and notes that already
  exist surviving becoming Ignored.
- `web/handlers.rs` pins the adapter mapping: every `CommandRejection` variant
  becomes a client error and only `ConcertCommandError::Operational` becomes a
  500. Collapsing a rejection into a server error is exactly the regression this
  criterion exists to catch.
- `hurl/detail_prepare_notes.hurl` gained seven black-box requests: notes that
  were stored before an ignore are still on the detail page afterwards; the card
  the notes endpoint swaps in still shows the Ignored badge and its "clear
  ignored" action; and a concert that does not exist answers 404 for `notes`,
  `ignore`, and `want` rather than reporting a server failure. The file went from
  8 to 15 requests.
- `concert-tracker/tests/cli_status.rs` runs the real binary against a real
  database file and pins what `concert-db want`/`ignore` mean to a person: the
  line printed on success, that a second invocation toggles back, that a
  Wanted ↔ Ignored swap leaves exactly one stored flag, and that a missing
  concert exits non-zero with `concert <id> not found` on stderr and no success
  line on stdout.

## Preserved behaviour

Nothing in the product changed: no route, markup, badge, notes form, CLI verb,
printed line, exit status, or stored flag. Only tests and their fixtures were
added.

One acceptance criterion is satisfied vacuously, and deliberately so:
*"Expected rejections … include latest state when available."* The only
rejection `set_status` and `set_notes` can produce is `NotFound`, where no
Concert State exists to attach; every command outcome that does have a state
afterwards returns it. Widening `CommandRejection` to carry a state would have
changed the error contract every adapter matches on, which is out of scope for a
compatibility slice.

## Tests

- `concerts::tests::every_status_transition_sets_one_exclusive_status_and_one_event`
  — all nine ordered pairs, three assertions each plus retry.
- `concerts::tests::set_notes_stores_the_desired_text_and_returns_state`,
  `concerts::tests::notes_and_status_are_independent_desired_state`.
- `web::handlers::tests::command_rejections_are_client_errors_and_faults_are_server_errors`.
- `hurl/detail_prepare_notes.hurl` — 8 → 15 requests.
- `tests/cli_status.rs` — 3 tests.

## Verification gates

Run in the mounted working directory, building into `<repo>/target` with the
default dev profile — no `CARGO_TARGET_DIR` redirection and no rustc-wrapper
override:

- `cargo fmt --all -- --check` — clean.
- `cargo clippy --workspace --all-targets -- -D warnings` — exit 0.
- `cargo nextest run --tests` — 916 passed, 0 skipped.
- `node scripts/hurl-test.js` — 14 files, all passing (278 requests in the last
  full run; the corpus total moves with the poll-and-retry files, so the request
  count records that run rather than a fixed baseline).
- `npx playwright test` — 173 passed.
- `./scripts/shellcheck.sh`, `./scripts/ts-check.sh` — exit 0 (no shell or
  TypeScript changed; run to confirm the standard lint suite still passes).

These counts came from a full run whose target directory sat outside the repo.
The recipe above is the one to use, and building into `<repo>/target` was
re-confirmed after this branch merged: from a cleaned `target/`,
`cargo build --bin concert-web --bin concert-db --features test-control`
finished in 3m53s and `node scripts/hurl-test.js --glob
hurl/detail_prepare_notes.hurl` ran 15 requests at 100% against that binary,
with no `CARGO_TARGET_DIR` or profile override set.

Mutation checks on the new assertions, each reverted afterwards:

- Swapping the `wanted_delete`/`ignored_delete` events in
  `set_concert_status` fails the transition matrix
  (`Wanted -> Available must append exactly ["import", "wanted",
  "wanted_delete"]`).
- Making `ConcertStatus::Wanted` set both stored flags fails it too
  (`Available -> Wanted`, the mutual-exclusivity assertion).
- Making `concert-db want` always store the requested status (never toggling
  back) fails `want_and_ignore_translate_a_toggle_into_the_desired_status`.
- Mapping `CommandRejection::Conflict` onto a server error fails the adapter
  mapping test (`… must be a client error, got 500 Internal Server Error`).
