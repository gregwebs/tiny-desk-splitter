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

The server will allocate one monotonic fixture number per seed request from a
server-local `AtomicU64`, starting at `1`. The counter is not reset by
`test.reset`; a fresh `just test-hurl` invocation starts a fresh server process
and therefore a fresh counter.

Generated values use the fixture number consistently across a seed request.
Explicit request parameters override generated defaults. Explicit `null` is
accepted only for nullable domain fields where missing state has test value;
identity text fields such as `source_url`, `title`, `artist`, and `album` must
be omitted or provided as strings.

Generated `source_url` values use the reserved testing domain
`https://example.test/`, not NPR-looking URLs. Scraped and lifecycle seeds
default to a three-track set list. Lifecycle state remains inert by default:
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
