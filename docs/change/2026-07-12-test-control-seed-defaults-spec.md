# Test Control Seed Defaults Spec

## Problem Statement

The Hurl API tests now seed most fixture concerts through the Test Control
JSON-RPC API. Those seed calls are verbose because each one must spell out
conflict-avoiding values such as `source_url`, `title`, `artist`, `album`, and
often `set_list`, even when the test does not care about those values.

This makes `.hurl` files harder to scan. It also forces every test author to
manually maintain uniqueness across files, even though `just test-hurl` already
runs all files against one test-control server and scratch database.

## Solution

Add server-side defaults to the Test Control seed methods. A Hurl seed request
can keep the current JSON-RPC envelope but pass an empty flat params object:

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "test.seed_lifecycle_concert",
  "params": {}
}
```

The server allocates one fixture number from a monotonic process-local counter
and uses that number to generate conflict-free defaults. Explicit params remain
valid and override generated defaults:

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "test.seed_lifecycle_concert",
  "params": {
    "title": "Downloaded Filter Fixture",
    "downloaded": true
  }
}
```

The request shape remains a flat map. Existing Hurl calls must not need a
nested `params.params` object, camelCase names, or any other compatibility
change.

This change should land in two commits:

1. Add the server defaulting behavior, Rust tests, `hurl/README.md` updates,
   and ADR.
2. Simplify all existing Hurl files, removing boilerplate defaultable params
   while preserving explicit values that are asserted or improve scenario
   readability.

## User Stories

1. As a Hurl test author, I want to omit seed fields that are not relevant to a scenario, so that test setup is easier to read.
2. As a Hurl test author, I want generated `source_url`s to be conflict-free across the whole server run, so that separate `.hurl` files do not collide.
3. As a Hurl test author, I want explicit seed params to keep working, so that tests can still name important fixtures.
4. As a Hurl test author, I want explicit flat-map params to remain valid, so that existing tests do not break during rollout.
5. As a Hurl test author, I want generated fixture values to be readable in failures, so that defaulted data can still be traced back to a seed call.
6. As a Hurl test author, I want `test.reset` to clear DB/filesystem state without resetting the fixture counter, so that later generated defaults cannot reuse earlier identities in the same server process.
7. As a Hurl test author, I want `source_url` defaults to use a test domain, so that fake fixture URLs are not confused with real NPR pages.
8. As a Hurl test author, I want lifecycle booleans to default to `false`, so that default fixtures do not imply downloaded or split state.
9. As a Hurl test author, I want scraped and lifecycle concerts to have a three-track set list by default, so that common UI/API checks have realistic track data.
10. As a Hurl test author, I want to pass `null` for nullable domain fields, so that tests can still cover missing dates, teasers, timestamps, or media duration.
11. As a maintainer, I want default generation centralized in one helper, so that future changes to fixture naming do not scatter across seed methods.
12. As a maintainer, I want the JSON-RPC envelope left unchanged, so that defaulting does not get mixed with a separate protocol/wrapper decision.
13. As a reviewer, I want server behavior and Hurl cleanup in separate commits, so that the semantic API change can be reviewed independently from mechanical test edits.

## Implementation Decisions

- Modify `concert-tracker/src/test_control.rs`.
- Do not modify product routes, public API handlers, database schema, or Hurl runner process isolation.
- Leave `test.assert_concert_state` out of scope. Assertion defaulting needs a separate design.
- Keep the existing external JSON-RPC shape:

  ```json
  {
    "jsonrpc": "2.0",
    "id": 1,
    "method": "test.seed_listing",
    "params": {
      "source_url": "https://npr.org/c/listing-status-a",
      "title": "Listing Status A",
      "concert_date": "2024-01-15",
      "teaser": null
    }
  }
  ```

- Add a small shared fixture generator in `concert-tracker/src/test_control.rs`.
  The exact names can change during implementation, but the intended shape is:

  ```rust
  use std::sync::atomic::{AtomicU64, Ordering};

  struct TestControlFixtures {
      next_fixture: AtomicU64,
  }

  impl TestControlFixtures {
      fn new() -> Self {
          Self {
              next_fixture: AtomicU64::new(1),
          }
      }

      fn allocate(&self, kind: FixtureKind) -> FixtureIdentity {
          let n = self.next_fixture.fetch_add(1, Ordering::Relaxed);
          FixtureIdentity { kind, n }
      }
  }

  #[derive(Clone, Copy)]
  enum FixtureKind {
      Listing,
      Scraped,
      Lifecycle,
  }

  struct FixtureIdentity {
      kind: FixtureKind,
      n: u64,
  }
  ```

- Add fixture value methods that are the single source of truth for defaults:

  ```rust
  impl FixtureIdentity {
      fn source_url(&self) -> String {
          format!("https://example.test/tiny-desk/test-control-{}", self.n)
      }

      fn title(&self) -> String {
          match self.kind {
              FixtureKind::Listing => format!("Test Listing {}", self.n),
              FixtureKind::Scraped => format!("Test Scraped Concert {}", self.n),
              FixtureKind::Lifecycle => format!("Test Lifecycle Concert {}", self.n),
          }
      }

      fn artist(&self) -> String {
          match self.kind {
              FixtureKind::Listing => format!("Test Listing Artist {}", self.n),
              FixtureKind::Scraped => format!("Test Scraped Artist {}", self.n),
              FixtureKind::Lifecycle => format!("Test Lifecycle Artist {}", self.n),
          }
      }

      fn album(&self) -> String {
          match self.kind {
              FixtureKind::Listing => format!("Test Listing Album {}", self.n),
              FixtureKind::Scraped => format!("Test Scraped Album {}", self.n),
              FixtureKind::Lifecycle => format!("Test Lifecycle Album {}", self.n),
          }
      }

      fn teaser(&self) -> String {
          format!("Test listing teaser {}", self.n)
      }

      fn set_list(&self) -> Vec<String> {
          vec![
              format!("Test Control Track {}.1", self.n),
              format!("Test Control Track {}.2", self.n),
              format!("Test Control Track {}.3", self.n),
          ]
      }
  }
  ```

- Store the fixture generator on `TestControlServer`:

  ```rust
  pub struct TestControlServer {
      state: AppState,
      fixtures: TestControlFixtures,
  }

  impl TestControlServer {
      pub fn new(state: AppState) -> Self {
          Self {
              state,
              fixtures: TestControlFixtures::new(),
          }
      }
  }
  ```

- Do not reset `fixtures.next_fixture` from `reset_test_data` or
  `test.reset`.
- Allocate the fixture number before validation. Gaps after failed seed calls
  are allowed.
- Switch seed methods to explicit params structs where that improves default
  handling, but keep the external params object flat. The implementation should
  verify this with tests.
- Accept `params: {}` as the documented zero-boilerplate request. Supporting an
  omitted `params` property is optional and should not complicate the change.
- Default contracts:

  | Method | Omitted field defaults |
  |---|---|
  | `test.seed_listing` | `source_url`, `title`, fixed `concert_date`, generated `teaser` |
  | `test.seed_scraped_concert` | `source_url`, `title`, fixed `concert_date`, `artist`, `album`, three-track `set_list` |
  | `test.seed_lifecycle_concert` | all scraped defaults plus `downloaded = false`, `split = false`, no timestamps, no media duration |

- Use a fixed valid date for omitted `concert_date`, such as
  `"2026-01-01"`. Tests that need no date should pass `concert_date: null`.
- Nullable domain fields where explicit `null` should preserve missing state:
  `concert_date`, `teaser`, `set_list`, `auto_timestamps`,
  `user_timestamps`, `media_duration`.
- Identity text fields should be omitted or strings. Explicit `null` should be
  rejected for `source_url`, `title`, `artist`, and `album`.
- Existing seed result shapes remain unchanged:

  ```rust
  pub struct SeedListingResult {
      pub id: i64,
      pub source_url: String,
      pub title: String,
      pub concert_date: Option<String>,
  }

  pub struct SeedScrapedConcertResult {
      pub id: i64,
      pub source_url: String,
      pub title: String,
      pub album: String,
  }

  pub struct SeedLifecycleConcertResult {
      pub id: i64,
      pub source_url: String,
      pub title: String,
      pub album: String,
      pub downloaded: bool,
      pub split: bool,
  }
  ```

- Update all `.hurl` files under `hurl/` in the second commit. Remove
  boilerplate values whose only purpose was avoiding collisions or satisfying
  required seed params. Keep explicit values when they are asserted or make the
  scenario clearer.
- Update `hurl/README.md` to document:
  - seed params can be omitted by sending `params: {}`
  - `jsonrpc` and `id` stay as-is for now
  - explicit flat-map params still override defaults
  - explicit `null` is only for nullable domain fields
  - the counter is server-local, monotonic, and not reset by `test.reset`
  - generated URLs use `example.test`
  - default scraped/lifecycle set lists contain three tracks
- Add `docs/adr/0003-test-control-seed-defaults.md`.
- Link this change from `hurl/README.md` and link the ADR from this change doc.

## Testing Decisions

- Use the Test Control API boundary as the primary Rust test seam. The tests
  should verify externally visible seed behavior, not the atomic counter's
  internals.
- Add or update tests in `concert-tracker/src/test_control.rs` behind the
  `test-control` feature.
- Add regression coverage that a fully explicit flat-map request remains valid
  for each seed method.
- Add coverage that `params: {}` works for:
  - `test.seed_listing`
  - `test.seed_scraped_concert`
  - `test.seed_lifecycle_concert`
- Add coverage that generated `source_url`s are unique across multiple seed
  calls and across seed method kinds.
- Add coverage that `test.reset` does not reset the fixture counter.
- Add coverage that explicit overrides win over generated defaults.
- Add coverage that lifecycle defaults are inert:

  ```json
  {
    "downloaded": false,
    "split": false
  }
  ```

- Add coverage that default scraped/lifecycle set lists have three entries.
- Add coverage that explicit `null` preserves nullable domain absence where
  supported.
- Add coverage that explicit `null` for identity strings is rejected.
- Run the Hurl suite after the mechanical cleanup to prove simplified seed
  calls still drive public HTTP assertions correctly.

## Verification Commands

Run these after the server-defaulting commit:

```sh
cargo check -p concert-tracker --features test-control
cargo check -p concert-tracker
cargo test -p concert-tracker --features test-control test_control
just test-hurl
```

Run these before opening the PR:

```sh
just lint
```

Keep the release safety guard expectation from the existing Hurl documentation:

```sh
cargo build --release --bin concert-web --features test-control
```

That command should fail because `test-control` must not compile into release
builds.

## Out of Scope

- Removing the JSON-RPC `jsonrpc` field.
- Removing or auto-generating the JSON-RPC `id` field.
- Adding a Hurl preprocessor, macro layer, or wrapper endpoint.
- Changing product HTTP routes or public API contracts.
- Changing `test.assert_concert_state`.
- Adding file-writing fixture seeds for media-heavy Hurl tests.
- Making fixture numbers globally unique across separate `just test-hurl`
  invocations.

## Further Notes

This spec is an extension of the Hurl/Test Control design captured in
`docs/change/2026-07-11-hurl-web-integration-tests.md` and the JSON-RPC
dependency decision in `docs/adr/0001-jsonrpsee-for-test-control-api.md`.

The new ADR for this policy is
`docs/adr/0003-test-control-seed-defaults.md`.
