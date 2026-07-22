# Add permission-efficient GitHub Actions inspection

GitHub Actions status monitoring and failed-job reruns already used stable,
allowlistable repository scripts. Inspecting workflow metadata or downloading a
failed job log still required raw API calls and shell-level GitHub App token
handling, causing additional sandbox permission prompts.

This change adds two narrow authenticated commands:

```text
./scripts/github/gh-app-actions-run-view.sh RUN_ID
./scripts/github/gh-app-actions-job-log.sh JOB_ID
```

The run helper prints a concise summary by default and supports JSON output.
The job-log helper prints the complete log or writes it to an explicit output
path. Both validate numeric IDs, infer the repository from `origin`, accept an
explicit `--repo`, and contain short-lived GitHub App credentials inside the
shared helper library.

The `github-actions-ci` skill now directs agents to these scripts instead of
raw `curl`, direct API calls, `gh`, or shell-level token plumbing. Their stable
command prefixes can be approved once and reused for later CI diagnosis.

## Verification

- ShellCheck and Bash syntax validation passed for both scripts and the shared
  GitHub App library.
- `gh-app-actions-run-view.sh` successfully summarized a real workflow run.
- `gh-app-actions-job-log.sh` successfully downloaded a 106 KB real job log.
- Both scripts' help and argument contracts were exercised.
- The generic skill validator could not run because its Python environment
  lacks `PyYAML`; the existing valid two-field skill frontmatter was unchanged.
