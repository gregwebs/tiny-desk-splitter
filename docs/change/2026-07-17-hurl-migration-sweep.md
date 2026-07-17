# Hurl migration sweep

Completed issue [#111](https://github.com/gregwebs/tiny-desk-splitter/issues/111),
the final documentation and verification slice of the
[remaining web integration Hurl migration](2026-07-14-remaining-web-integration-hurl-migration-spec.md).

## Scope and moved scenarios

The migration now exercises black-box product HTTP behavior against a real
`concert-web` process in `hurl/*.hurl`. The suite covers listing and lifecycle
state, playlists, media navigation and errors, download/split/opener job
chains, file-heavy routes, timestamp editing, playback reconstruction,
interlude deletion, and background scrape timing.

The Test Control API supplies only deterministic setup or internal-only
observations:

- generated database fixtures and domain-focused Scenario Seeds;
- Job Driver plans, blocked-step releases, and Job Observations;
- Scrape Driver plans, enqueue/release controls, and observations;
- concert-state and event assertions when no public route exposes the fact.

Production route validation and shared lifecycle orchestration remain on the
same path used outside tests. The lasting API, isolation rules, and examples
are canonical in [`hurl/README.md`](../../hurl/README.md).

## Final test boundary

`concert-tracker/tests/web_integration.rs` now contains three tests. They stay
Rust-only intentionally:

1. `detail_page_auto_scrape_failure_still_renders` drives the real inline
   outbound scrape failure path, which does not use the background Scrape
   Driver seam.
2. `prod_router_serves_embedded_js_without_livereload` compares production
   router construction and embedded assets with the dev-only behavior.
3. `served_openapi_spec_matches_built_api_doc` compares the served document
   with an in-process Rust value used by `openapi-dump`.

The former one-comment-per-migrated-test history was removed from the Rust
file. A single header breadcrumb points future contributors to the canonical
Hurl guide; each retained test keeps its architectural rationale beside it.

## Coverage state change

```text
Product HTTP behavior
  |
  `-- real concert-web process --------------------------> Hurl
        |                                                   |
        |-- public responses and fragments ----------------` 
        |-- deterministic fixture setup -> Scenario Seeds
        |-- download/split/open timing -> Job Driver
        |-- scrape queue timing ----------> Scrape Driver
        `-- internal-only facts ----------> Observations/assertions

In-process implementation consistency
  |
  |-- real inline scrape failure
  |-- production router construction
  `-- served OpenAPI == built OpenAPI --------------------> Rust integration
```

No application or persisted-data state transition changed in this sweep.

## Canonical documentation

- [`README.md`](../../README.md) links directly to the Hurl/Test Control guide.
- [`hurl/README.md`](../../hurl/README.md) owns running instructions, Test
  Control contracts, fixture semantics, driver constraints, coverage mapping,
  Rust-only exceptions, and CI behavior.
- [`docs/jobs.md`](../jobs.md) owns the lasting typed-runner architecture.
- [ADR 0005](../adr/0005-typed-job-runner-for-test-control.md) owns the decision
  to place production and deterministic execution behind one typed boundary.
- The parent migration spec and per-slice Change Records retain historical
  rationale and implementation detail; they are not the current API reference.

## Agent Review

The adversarial implementation-plan review required a more concrete contract
audit, a direct README link, replacement of chronological guide history with a
current coverage map, exact release-guard evidence, and an explicit decision
on Playwright. The plan was amended before the documentation sweep began.

The adversarial diff review found that current contracts still delegated to
ephemeral Change Records, README navigation was malformed, the Rust breadcrumb
duplicated retained rationale, and the plan/checklist state was incomplete.
The lasting documents now own their contracts, README has a valid repository
documentation index, the Rust file has exactly one migration breadcrumb, and
the plan reflects completed work. A non-adversarial follow-up confirmed those
fixes; its last canonicality observation removed the remaining Job Driver
design pointer to an ephemeral plan.

## Verification

This documentation-only ticket uses the complete Hurl run as live
manual-equivalent verification: the runner starts a real `concert-web` on
isolated ports with a scratch database/workdir, exercises its public routes,
and tears the process down. Playwright adds no ticket-specific signal because
no visual or browser interaction behavior changed.

All required checks passed on 2026-07-17:

| Check | Result |
|---|---|
| `cargo check -p concert-tracker --features test-control` | passed |
| `cargo check -p concert-tracker` | passed |
| `cargo build --bin concert-web --features test-control` | passed |
| `just test-hurl` | 12 files passed, 225 requests passed; real app and Test Control listeners used isolated loopback ports |
| `cargo nextest run -p concert-tracker --test web_integration` | 3 passed, 0 skipped |
| `just lint` | formatting, Clippy, shellcheck, TypeScript check, and oxlint passed |
| touched relative Markdown links | all resolved |

The release-safety command exited nonzero with status 101, as required:

```text
error: test-control must not be compiled into release builds
28 | compile_error!("test-control must not be compiled into release builds");
```

The diagnostic exactly matches the guard in
`concert-tracker/src/test_control.rs`; the expected failure was not masked by
an unrelated compilation error. The Hurl runner reported both live listener
URLs, completed all scenarios, and shut down cleanly. Playwright was not run
because this sweep changes no visual or browser interaction behavior.
