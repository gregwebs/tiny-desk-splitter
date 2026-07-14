# Database Seed API Design

## Context

Test Control seed methods currently own both Hurl request defaulting and the
database writes needed to create listing, scraped-concert, and lifecycle
fixtures. Those seeds are useful outside Hurl, so the reusable fixture creation
belongs in the persistence test layer.

## Decisions So Far

- Add a test-only Database Seed API under `concert-tracker/src/db/`.
- Compile it only for Rust tests and Test Control builds:

  ```rust
  #[cfg(any(test, feature = "test-control"))]
  pub mod seeds;
  ```

- Use a Database Seed Context as the single argument that carries the SQLite
  connection and fixture-id allocator.
- Put fixture defaulting in the Database Seed API, not in `test_control.rs`.
- Seed input structs should implement `Default` and support `..Default::default()`.
- Hurl-facing seed structs should use struct-level `#[serde(default)]`.
- Avoid nested `required`/`defaults` wrappers. When a future seed requires
  scenario data, represent it with individual arguments or a non-defaultable
  required-fields struct.
- Do not preserve omitted-versus-explicit-null with a custom wrapper type in
  the seed layer. For `Option<T>` fields, Rust `Default` defines the omitted
  value and JSON `null` deserializes to `None`; those are distinct whenever the
  field's `Default` is `Some(...)`, and equivalent whenever it is `None`.
- Compose existing domain persistence functions for meaningful domain
  transitions. Direct SQL is allowed inside `db::seeds` only for fixture
  normalization where a domain function would emit misleading events or require
  irrelevant side effects.
- DB seed return values should optimize for DB and Rust test utility. Test
  Control adapts those values into whatever JSON response shape its API needs;
  it may often expose only an id.
- Keep existing `db::tests::{listing, seed, seed_with_album}` helpers as thin
  wrappers over `db::seeds` at first. Migrate call sites directly to
  `db::seeds` only where the new seed input improves readability.
- Update the existing accepted seed-default ADR and Hurl docs in the
  implementation change. Do not add a superseding ADR just for the
  omitted-versus-null seed semantics change.
- Expose seed operations as methods on `db::seeds::SeedContext`, which combines
  the SQLite connection reference and fixture-id allocator:

  ```rust
  let mut seeds = db::seeds::SeedContext::new(&conn);
  let listing = seeds.seed_listing(db::seeds::SeedListing {
      ..Default::default()
  })?;
  ```

## Open Questions

- None.

## Implementation

Landed as `concert-tracker/src/db/seeds.rs`, gated
`#[cfg(any(test, feature = "test-control"))]` per the design. `SeedContext<'a>`
holds a `&'a Connection` plus a `FixtureIds` allocator (a cloneable
`Arc<AtomicU64>` handle starting at `1`) and exposes `seed_listing`,
`seed_scraped_concert`, `seed_lifecycle_concert`, each taking a `Default`-able,
struct-level-`#[serde(default)]` input (`SeedListing`, `SeedScrapedConcert`,
`SeedLifecycleConcert`) and returning `crate::model::Concert`.
`concert-tracker/src/test_control.rs`'s `test.seed_*` RPC methods now take
these types directly as their JSON-RPC params (unchanged `param_kind = map` /
adapter contract) and delegate to a `SeedContext` built from one
process-lifetime `FixtureIds` (a `static LazyLock`), preserving ADR 0003's
"one counter for the process, not reset by `test.reset`" guarantee.
`db::tests::seed`/`seed_with_album` (`db/mod.rs`) became thin wrappers over
`SeedContext::seed_listing`, producing the identical row their ~70 existing
call sites already depend on.

An initial adversarial review (Codex, acting as the project's engineering
lead persona) found two contract gaps in a naive "move the logic unchanged"
port, both fixed in `db::seeds` with direct-SQL fixture normalization (which
the design explicitly permits inside this module):

1. **Null doesn't clear on reseed.** `upsert_listing` resolves `concert_date`/
   `teaser` via `COALESCE(excluded.x, concerts.x)` on a `source_url` conflict,
   so a seed's resolved `None` would silently preserve a prior value instead
   of clearing it. Every seed now follows the upsert with a direct `UPDATE`
   writing the already-resolved values — conditional on the values actually
   changing (`WHERE id = ?3 AND (concert_date IS NOT ?1 OR teaser IS NOT ?2)`),
   because `concerts` has an `AFTER UPDATE` trigger that bumps `updated_at`
   for any ordinary write, and a no-op normalization write must not move it.
2. **Lifecycle seeds didn't reset stale state on URL reuse.** Only the
   scraped-concert seed cleared prior download/split/archive state on a
   reused `source_url`; the lifecycle seed could leave a previously
   downloaded/split concert looking downloaded/split even when the new
   request asked for inert defaults. A shared `reset_fixture_lifecycle_state`
   helper (extended to also clear split timestamps and `media_duration`) now
   runs in both seeds before applying the requested transitions, with the
   same "skip when already inert" guard for `updated_at` stability.

Both gaps are covered by new `db::seeds` tests
(`seed_listing_reseed_with_null_clears_previously_set_fields`,
`seed_lifecycle_concert_reuse_resets_to_inert_defaults`). A follow-up
non-adversarial review of the plan found no further issues.

An adversarial code review of the implementation (Codex, engineering-lead
persona) then found three more issues, all fixed:

1. `reset_fixture_lifecycle_state` cleared `tracks_present` but not
   `tracks_liked` — a prior seed's liked-track state (set by the real
   product route, not a seed param) could survive a `source_url` reuse.
   Fixed by adding `tracks_liked` to both the `SET` clause and the
   already-inert guard; covered by extending
   `seed_lifecycle_concert_reuse_resets_to_inert_defaults` to set
   `tracks_liked` before reseeding and assert it clears.
2. `db::tests::seed` (`db/mod.rs`) was never actually rewired to delegate to
   `SeedContext` — an oversight where the plan (and this doc) described the
   intended change but the edit was skipped. Fixed: `seed` now builds a
   `SeedListing` from the existing `listing()` helper's values and calls
   `SeedContext::new(conn).seed_listing(...)`, producing the identical row
   its ~70 call sites already depend on. `seed_with_album` is unchanged
   (`seed()` + `update_metadata`, per the original plan).
3. The ADR and `hurl/README.md` both said explicit JSON `null` "always"
   deserializes to `None`, which is false for the plain `bool` fields
   `downloaded`/`split` (not `Option<bool>`) — `null` for those is invalid
   params, not a default. Fixed the wording in both docs to scope the claim
   to `Option<T>` fields and call out the `bool` fields explicitly.

Behavior changes from ADR 0003 (all verified against the existing `.hurl`
suite, which sends no explicit `null` for these fields and asserts no exact
generated teaser text):

- Explicit `null` for identity fields (`source_url`/`title`/`artist`/`album`)
  used to be rejected; it now deserializes and means "generate the default",
  identical to omitting the field.
- `set_list: null` used to mean "empty"; it now means "generate the default
  three-track list" — pass `set_list: []` for an explicitly empty list.
- The generated listing teaser dropped its `{n}` suffix (now the constant
  `"Test listing teaser"`), since uniqueness doesn't matter for it and this
  keeps its `Default` distinct from an explicit `null`.

Updated docs: `docs/adr/0003-test-control-seed-defaults.md` (in place, no
superseding ADR) and `hurl/README.md`'s "Test Control seed defaults" section
now describe the `Default`/`null` rule instead of the old
identity-fields-reject-null rule; `docs/backend-persistence.md` gained a
`db::seeds` module-map row and dependency-direction note.

Verification: `cargo nextest run --tests` (799 tests, including 14 new
`db::seeds` tests and the updated `test_control` test module), `just lint`
clean, and `just test-hurl` green across all six existing `.hurl` files with
no changes to any `.hurl` file — confirming the wire format is unchanged for
every existing request shape.
