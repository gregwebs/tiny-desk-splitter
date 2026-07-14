# Test Control Seed Defaults

Status: Accepted

## Context

Hurl tests use the Test Control JSON-RPC API to seed fixture concerts before
asserting product HTTP behavior. As coverage moved from Rust integration tests
to Hurl, seed requests became noisy: many calls repeat `source_url`, `title`,
`artist`, `album`, and `set_list` values whose only purpose is satisfying the
seed API or avoiding unique-key conflicts.

The Hurl runner starts one `concert-web` process with one scratch database and
workdir, then runs all `.hurl` files against it with `--jobs 1`. Seed defaults
therefore need to avoid conflicts across the whole server process, not just
within one file.

## Decision

Test Control seed methods may generate deterministic defaults for omitted seed
parameters. Defaults are test-control-only and do not change product routes or
public API semantics.

Fixture defaulting and the DB writes it feeds live in the Database Seed API
(`concert-tracker/src/db/seeds.rs`, `cfg(any(test, feature = "test-control"))`)
rather than in Test Control itself — see
`docs/change/2026-07-13-db-seed-api-design.md`. Test Control's `test.seed_*`
methods take `db::seeds::SeedListing` / `SeedScrapedConcert` /
`SeedLifecycleConcert` directly as their JSON-RPC params and delegate to a
`db::seeds::SeedContext`.

The fixture-number allocator is `db::seeds::FixtureIds`, a cloneable handle
around a monotonic counter starting at `1`. Test Control holds one
process-lifetime `FixtureIds` (a `static LazyLock`) and shares clones of it
across every seed call, preserving this ADR's original guarantee: the counter
is not reset by `test.reset`, and a fresh `just test-hurl` invocation starts a
fresh server process and therefore a fresh counter. Rust tests that construct
a `SeedContext::new(&conn)` instead get their own fresh allocator, since a
single test's fixtures only need to be unique against themselves.

Generated values use the fixture number consistently across a seed request.
Each seed input struct implements `Default` and deserializes with
struct-level `#[serde(default)]`: a **missing** JSON field takes the struct's
`Default` value. For every field typed `Option<T>` (all fields except
`downloaded`/`split`, below), an explicit JSON **`null`** always deserializes
to `None`. Omitted and `null` differ exactly when a field's `Default` is
`Some(...)` — which is only true for `concert_date` (default
`Some("2026-01-01")`) and `teaser` (default `Some("Test listing teaser")`).
For those two fields, omitting them takes the default text, while `null`
stores a real SQL `NULL`. Every other `Option<T>` field (`source_url`,
`title`, `artist`, `album`, `set_list`, `auto_timestamps`, `user_timestamps`,
`media_duration`) defaults to `None`, so omitting the field and sending
explicit `null` are equivalent — both mean "generate a value" (identity
fields, `set_list`) or "leave it absent" (the timestamp/media fields). This
supersedes the previous rule in this ADR that explicit `null` was rejected
for identity fields; a seed call may now send `"source_url": null`, and it
behaves exactly like omitting `source_url`.

`downloaded`/`split` are plain `bool`, not `Option<bool>` — omitting them
takes the `false` default, but explicit `null` is not a valid `bool` and is
rejected as invalid params, the same as sending a string or number for them
would be.

Generated `source_url` values use the reserved testing domain
`https://example.test/`, not NPR-looking URLs. Scraped and lifecycle seeds
default to a three-track set list (pass `set_list: []` for an explicitly empty
list — `null`/omitted both mean "generate the default three tracks", per the
`Default`/`null` rule above). Lifecycle state remains inert by default:
`downloaded` and `split` default to `false`, and timestamp/media fields default
to absent unless requested.

The JSON-RPC envelope remains unchanged for this decision. Hurl requests should
continue to send `jsonrpc`, `id`, `method`, and at least `params: {}` for seed
calls. Removing or wrapping the JSON-RPC envelope is a separate design.

## Consequences

Hurl seed calls become shorter while retaining readable fixture names in the
seed result and rendered pages. Existing explicit flat-map requests remain
valid, so server defaulting can land before the Hurl suite is mechanically
simplified.

The monotonic counter may have gaps after failed seed requests. That is
acceptable because the counter is for conflict avoidance and diagnostics, not
for business identity.

Tests that assert specific rendered values should keep explicit parameters.
Defaults are for boilerplate fixture data, not for hiding scenario intent.
