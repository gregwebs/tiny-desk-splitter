# Tiny Desk Splitter

Tiny Desk Splitter tracks the lifecycle of Tiny Desk concerts from discovery through download, splitting, playback, and archiving.

## Language

**Job Request**:
A request to begin download, split, or archive work for a concert. A Job Request may be rejected before work is accepted, in which case it does not create lifecycle history or a failed job. Opening media is not a Job Request because it has no persistent job lifecycle.
_Avoid_: Job failure, Job Run

**Job Run**:
An accepted attempt to perform download, split, or archive work for a concert. A Job Run records that it started and reaches exactly one terminal outcome: succeeded, failed, or cancelled. Cancellation is recorded as a failed outcome with a cancellation reason. Opening media is not a Job Run because it has no persistent job lifecycle.
_Avoid_: Job Request, subprocess

**Failed Job**:
A Job Run that ended in failure or user cancellation and is retained for inspection in failed-job history. A rejected Job Request is not a Failed Job because it never became a Job Run and its error is returned synchronously.
_Avoid_: Rejected Job Request, validation error

**Test Control API**:
A test-only HTTP control surface mounted inside the `concert-web` process so black-box HTTP tests can arrange fixture state and inspect necessary postconditions without linking to application internals. It is compiled only for non-release test-control builds and started only when explicitly requested.
_Avoid_: Seed proxy, seed backdoor

**Test Control HTTP Adapter**:
A test-only HTTP convenience surface that accepts concise Hurl request shapes and translates them into Test Control API JSON-RPC calls without changing the underlying JSON-RPC method contracts.
_Avoid_: Seed proxy, shim, fake server

**Seed Result**:
The object returned by a Test Control API seed method that identifies the exact fixture data it created. Seed Results should reuse public API JSON shapes when practical, adding test-only fields only when tests need stable handles that the public API should not expose.
_Avoid_: Fixture lookup by assumed ID

**Database Seed API**:
A test-only persistence-layer API that creates reusable fixture database state for tests without going through HTTP. Database Seed API input types distinguish required scenario fields from defaultable fixture fields.
_Avoid_: Hurl seed helper, production seed

**Scenario Seed**:
A test-only fixture operation that may create coordinated application state across the database and scratch workdir files when the product behavior normally depends on both. Scenario Seeds should use existing application/domain helpers where practical and expose concrete scenario shapes through Test Control when Hurl needs them.
_Avoid_: DB-only seed assumption, arbitrary file mutation API

**Database Seed Context**:
A test-only object passed to Database Seed API calls that provides the database connection and allocates fixture IDs for defaulted fixture values.
_Avoid_: Global seed counter, implicit fixture state

**Assertion API**:
A Test Control API method that verifies or returns test-only facts that should not be exposed through the real product API. Assertion APIs keep Hurl tests focused on public behavior without forcing internal details into production response shapes.
_Avoid_: Test-only production fields

**Job Observation**:
A test-only fact recorded by the Job Driver API about domain job execution, such as started, completed, failed, blocked, or released counts for a concert and job kind. Job Observations support concurrency and dependency-edge assertions that are not fully visible through public product routes.
_Avoid_: Product job state, lifecycle status

**Black-box Product HTTP Behavior**:
Behavior observable through the real `concert-web` HTTP routes that a user or HTTP client can exercise, even when a Hurl test uses Test Control API calls to arrange fixture state or inspect test-only postconditions around it.
_Avoid_: Router internals, in-process consistency checks

**Job Driver API**:
A Test Control API surface for deterministically controlling test-only download, split, and opener job behavior while Hurl exercises the real product HTTP routes that start or observe those jobs. It models job outcomes and timing directly instead of encoding behavior as shell snippets.
_Avoid_: Fake shell command config, global job mocks

**Test-Control Job Run**:
A `concert-web` run started with the test-control feature and Test Control API enabled, where download, split, and opener behavior is driven by the Job Driver API instead of production subprocess commands.
_Avoid_: Production command smoke test, real downloader Hurl run

**Job Driver Completion**:
The test-control implementation of a job step after the product route has performed its normal pre-job validation and lifecycle checks. Job Driver Completion may create expected output files and persist normal success/failure state, but it must not bypass the route-level validation being exercised by the Hurl scenario.
_Avoid_: Handler shortcut, validation bypass

**Typed Job Runner**:
An internal job execution abstraction that represents download, split, and opener effects as typed domain operations instead of shell commands and process exit codes. Production implementations may still spawn subprocesses; test-control implementations can complete, fail, or block jobs deterministically.
_Avoid_: Shell-string test driver, command-level fake
