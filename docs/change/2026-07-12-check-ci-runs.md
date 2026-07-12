# Check CI Runs Helper

Renamed `scripts/check-playwright-job.sh` to `scripts/check-ci-runs.sh` and
expanded it from a Playwright-only helper into a general GitHub Actions check
runner summary.

## Changes

- Default invocation reports the latest GitHub Actions check run for every CI
  job on the selected commit.
- `--job <name>` filters to one job, for example `--job playwright`.
- `--wait` now waits for every selected job to complete successfully.
- Existing exit behavior is preserved: success returns 0, completed failures
  return 1, and pending or missing selected jobs return 2 without `--wait`.

## Examples

```sh
./scripts/check-ci-runs.sh
./scripts/check-ci-runs.sh --wait
./scripts/check-ci-runs.sh --job playwright --wait
```
