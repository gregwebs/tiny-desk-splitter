* README.md for an overview of the project
* CONTRIBUTING.md for development
* CODING_STANDARDS.md for how to write code

## Curl

/usr/bin/curl may have TLS issues. Use /opt/homebrew/opt/curl/bin/curl

## Github auth

For write access to the repo, use the github skill in .claude/skills/github-app/SKILL.md
For read access, the repo is public.


# Workflow

## Agent Review

Reviews should be done be the engineeering-lead agent defined in .claude/agents/engineering-lead.md
Initial reviews should be adversarial. After an initial adversarial review, follow up reviews should not be adversarial.

## Bug Fix

Use /plan mode to investigate a bug in read-only mode.
If a bug is difficult, use /diagnosing-bugs to investigate it.

Generate a root cause analysis of the defect.
Save the root cause analysis on the ticket for the defect.
If there is no ticket, create one using /to-tickets.
Suggest how to fix the defect and also place that on the ticket.
A very simple straightforward fix can be implemented immediately.

## Improve Codebase Architecture

This is provided by the /improve-codebase-architecture skill.

## General workflow

- update the local main to the latest from origin and branch from that.
- Planning
  * If there is already a detailed implementation in an issue/spec you can skip planning.
  * Create a plan using the /to-spec skill
    * Describe the changes at different levels of detail
      * start with the high-level architecture of the changes
      * end with code implementation details
    * Create state diagrams for all state changes
      * This can be done with ascii tables, art, or an html artifact
    * Suggest breaking larger changes into iterative steps
    * Describe the future phases of coding (including tests and documentation) and verification, 
  * get feedback and rework the plan
    * Perform an Agent Review of any new features or non-trivial bug fixes
  * get approval before implementing
- Changing data
  * When updating database data, first create a backup of the existing database
- Coding
  * When deviating from the plan, ask for approval
  * Write tests before writing code and run tests frequently
  * Check the code using `cargo check` as frequently as possible
  * Run `just lint` and resolve all warnings
  * Documentation
    * Update and add technical documentation. It should be accessible by following links from the README.md
    * there should be one canonical place where something is documented (besides ./docs/change which is a snapshot in time)
      * check for references between documentation
    * Add state change diagrams to documentation
    * Put information from this change into a file in ./docs/change
      * If older change documentation is outdated, remove it.
    * Put
- Code review
  * Perform an Agent Review before verification
    * Do a follow up Agent Review of any changes made during verification
- Verification
  * verify manually that the changes work as expected in the application
  * start up a server on a separate port with a separate test database `--db` and a separate `--workdir` directory for saving concert information
  * test edge cases and failure modes in addition to the happy path
  * use Playwright to confirm visual/interaction aspects of the UI
    * maintain playwright scripts as e2e tests. this is only necessary where Foldkit tests are not adequate.
- Pull request
  * send a pull request using ./scripts/github/gh-app-pr-create.sh
  * for the commit and PR description point to what is added in ./docs/change
  * Check on the CI status after sending the PR. If there are failures, investigate them and change the PR following the Coding instructions.