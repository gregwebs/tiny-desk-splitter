# Use a typed job runner for test-control job behavior

Status: Accepted

Download, split, and opener jobs should run through a typed job-runner abstraction in both production and test-control builds. Production runners may keep spawning the existing subprocess commands, while test-control runners complete, fail, or block domain-level job steps deterministically for Hurl scenarios. This avoids encoding lifecycle behavior in shell snippets and keeps test-control completion semantics on the same architectural path as production job completion.
