# Concert Catalog compatibility coverage

Issue [#160](https://github.com/gregwebs/tiny-desk-splitter/issues/160) (serve
existing browsing through Concert Catalog) was implemented with the rest of the
Concert application interface in
[the #177 contraction](2026-07-25-concert-catalog-and-status.md): the HTMX
listing and `concert-db list` both browse through `ConcertCatalog`, the filter
meanings are asserted once against the catalog, and `docs/concerts.md`
documents the seam and its planned rationale. Auditing the ticket's acceptance
criteria against `main` left exactly one unmet: *"Web and CLI compatibility
tests demonstrate unchanged user-visible results."* The web half was pinned by
`hurl/listing_status.hurl` for two of the five filters; the CLI half did not
exist — `concert-db list` had no automated test at all.

## Changes

- `concert-tracker/tests/cli_list.rs` runs the real binary as a child process
  against a real database file and pins the CLI's own compatibility surface: the
  line `[<id>] <title> | <status> | <download>+<split>`, its order (newest
  concert date first), the CLI-only meaning of `all` (everything, Ignored
  included) and of any filter name it does not know, and the Wanted / Available
  / Ignored filters. Fixtures are seeded through the persistence API rather than
  the Test Control seeds, because `db::seeds` is `cfg(any(test, feature =
  "test-control"))` and integration tests get neither by default.
- `hurl/listing_status.hurl` gained the three filter URLs it did not cover:
  Wanted, Available, and Tracks (`split`), each asserting the concert that must
  appear and one that must not, through the real routes.
- `scripts/cargo-target.js` resolves a cargo-built binary through
  `CARGO_TARGET_DIR` and `CARGO_BUILD_TARGET`, the same overrides cargo applies.
  `scripts/hurl-test.js` and `e2e/fixtures.js` use it instead of hardcoding
  `<repo>/target/debug/concert-web`. `hurl-test.js` also reports the resolved
  path when a successful build produced no binary, instead of a bare `ENOENT`
  from `spawn`.
- `Dockerfile.dev` and `hurl/README.md` record that the small-volume recipe
  (`CARGO_PROFILE_DEV_DEBUG=0` plus a scratch target dir) covers the gate
  scripts, not only `cargo build`.

The tooling fix is what made this verification possible at all: the sandbox's
mounted working directory is a 25.7 GiB volume holding 21.5 GiB of media, so the
~4.6 GiB debuginfo build never fit, while the documented workaround silently
failed because two scripts assumed the default target layout.

## Preserved behaviour

Nothing in the product changed: no route, filter meaning, markup, CLI line, or
error protocol. The catalog's filter semantics remain asserted only once, against
the catalog; the new CLI and hurl cases assert adapter wiring and the words a
user reads.

## Tests

- `tests/cli_list.rs`: three tests covering the unfiltered listing (order and
  format), the three CLI filters, and the fallback for unknown filter names.
- `hurl/listing_status.hurl`: three added cases (`?filter=wanted`,
  `?filter=available`, `?filter=split`), 13 → 17 requests.

## Verification gates

Run in the mounted working directory with `CARGO_PROFILE_DEV_DEBUG=0` (a 2.4 GiB
`target/` instead of 4.6 GiB), dependencies served from the shared kache store:

- `cargo fmt --all -- --check` — clean.
- `cargo clippy --workspace --all-targets -- -D warnings` — exit 0 (29 s).
- `cargo nextest run --tests` — 912 passed, 0 skipped (171 s).
- `node scripts/hurl-test.js` — 14 files, 270 requests, 100% (66 s).
- `npx playwright test` — 173 passed (2.6 m). One earlier run failed
  `player-queue.spec.js:257` on a visible `#player-error` ("Playback blocked");
  it passes in isolation and in the following full run, and this change touches
  no player code, so it is recorded as a sandbox flake rather than a regression.
- Mutation checks on the new assertions: swapping `Available`'s `cli_filter`
  mapping, changing the CLI's printed format, and pointing the catalog's `split`
  slug at everything each fail the intended test.

## Follow-on

- The kache store `~/.cache/kache` sits below its own free-space floor (785 MiB
  free of 8.1 GiB, 5.2 GiB of blobs against a 5.0 GiB budget), so the daemon is
  evicting entries that later builds have to recompile. A second store at
  `~/Library/Caches/kache/store` holds 159 GB on a 156 GB volume.
- `hurl/listing_status.hurl` still leaves `?filter=archived` to
  `hurl/archive_lifecycle.hurl`, which covers the filter through the archive
  lifecycle rather than through a listing fixture.
