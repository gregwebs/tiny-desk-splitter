# Permission-efficient local HTTP verification

## Purpose

Manual API verification currently invokes `curl` with a complete loopback URL.
Because the port and path are part of one argument, a persistent approval for one
verification server does not match the next server. Add a repository-owned command
whose stable executable prefix can be approved once while the script constrains
requests to the local application.

## Implementation plan

- [x] Add `scripts/local-api-request.sh PORT PATH` as the public interface.
- [x] Validate that `PORT` is an integer from 1 through 65535.
- [x] Accept absolute request paths and reject whitespace/control characters
  so arguments cannot broaden the loopback request boundary.
- [x] Fix the host to `127.0.0.1` and the curl options to `-fsS`; default the
  method to GET while accepting an optional validated method and JSON body file.
- [x] Document the command in `CONTRIBUTING.md` as the canonical human-facing
  manual local HTTP verification command.
- [x] Instruct coding agents in `AGENTS.md` to use the helper instead of raw curl
  for local HTTP verification and identify its stable approval prefix.
- [x] Syntax-check, ShellCheck, and exercise successful and rejected requests.
- [x] Review the change, manually verify it against an isolated live application,
  and run the repository test and lint suites.

## State changes

The helper does not mutate repository state. Its default GET request is
read-only, while callers may explicitly select a mutating method against an
isolated application. Its execution state is:

```text
arguments received
       |
       v
port and absolute path valid? -- no --> usage/error, exit 2
       |
      yes
       |
       v
METHOD http://127.0.0.1:<port><path> [BODY_FILE]
       |
       +-- HTTP/connection failure --> curl diagnostic, nonzero exit
       |
       `-- success -----------------> response body, exit 0
```

## Verification plan

1. Confirm `--help` describes the two positional arguments and approval boundary.
2. Serve a known response on an isolated loopback port and retrieve it through the
   helper.
3. Confirm invalid ports, relative paths, extra arguments, and whitespace-bearing
   paths fail before invoking curl.
4. Run `bash -n`, ShellCheck, the full test suite, and the full lint suite.
5. Start `concert-web` with a temporary database and work directory on a separate
   port, then retrieve `/api/playlists` through the helper.

## Change record

Implemented the stable, GET-only loopback boundary and its agent/human guidance.
The CLI validation checks passed for help, invalid and out-of-range ports,
relative paths, whitespace-bearing paths, and excess arguments. `bash -n`,
ShellCheck under Homebrew Bash, Rust formatting/Clippy, TypeScript checking,
TypeScript linting, and all 324 TypeScript tests passed. The Rust suite reached
705 passing tests before the existing concurrent waveform test lost its temporary
fixture files. The isolated test passed on retry, followed by a clean full Rust
run: all 819 tests passed, confirming the initial failure was transient and
unrelated to this shell/documentation change.

The adversarial review found repeated port thresholds, incomplete control-byte
validation, and incomplete help text. Follow-up changes centralized the port
bounds, normalized leading zeroes before safe arithmetic, rejected control
characters, and documented the approval boundary in `--help`; both review axes
then passed. Manual verification started `concert-web` on port 43144 with a
temporary database and work directory. The approved helper returned `[]` from
`GET /api/playlists`, and a second invocation required no permission prompt.
The originally suggested `/api/concerts` example correctly surfaced curl's HTTP
failure because that GET route does not exist, so the lasting documentation now
uses the real read-only playlists endpoint.

The helper was later broadened from `/api` paths to any absolute request path
so the same allow-listed command can verify HTML routes such as `/concerts/1`.
It was also broadened to accept an optional HTTP method and JSON body file,
while remaining fixed to `127.0.0.1`; the two-argument form remains a GET.
