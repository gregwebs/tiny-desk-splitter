# Typed job runner

Implemented the first slice of the remaining web-integration Hurl migration:
job execution now goes through a typed `JobRunner` interface instead of direct
`download_cmd`, `split_cmd`, and `open_cmd` fields on `JobConfig`.

Production behavior is unchanged. The production runner still builds and runs
the existing `yt-dlp`, `live-set-splitter`, and opener subprocess commands via
the same logging path, while `start_download`, `start_split`, dependency-edge
handling, split timestamp persistence, and opener HTTP handlers call through
`JobConfig`'s typed runner methods.

This prepares the next slice to add a test-control Job Driver without forking
the product lifecycle orchestration. The architectural decision is recorded in
[`docs/adr/0005-typed-job-runner-for-test-control.md`](../adr/0005-typed-job-runner-for-test-control.md),
the canonical job architecture docs are in
[`docs/jobs.md`](../jobs.md),
and the full migration plan is in
[`docs/change/2026-07-14-remaining-web-integration-hurl-migration-spec.md`](2026-07-14-remaining-web-integration-hurl-migration-spec.md).

Verification performed:

- `cargo check -p concert-tracker`
- `cargo check -p concert-tracker --tests`
- `cargo check -p concert-tracker --features test-control`
- `cargo test -p concert-tracker jobs::download -- --nocapture`
- `cargo test -p concert-tracker jobs::split -- --nocapture`
- `cargo test -p concert-tracker jobs::prepare -- --nocapture`
- `cargo nextest run -p concert-tracker --test web_integration`
- `just test-rs`
- `just test-hurl`
- `just lint`
