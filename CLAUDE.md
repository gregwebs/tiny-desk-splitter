See README.md for an overview of the project

## Linting
- `just lint` — run the full standard lint suite (`cargo fmt --all -- --check` + `cargo clippy --workspace --all-targets -- -D warnings`)
- `just fmt` — auto-format the workspace

## Code Quality Guidelines
- **DRY**: Avoid code duplication. Use variables, functions, and modules to share code
- **Types**: Use strong typing. For example, after a string is validated/parsed it should be given a new type.
- **Testing**: Refactor code into small testable functions. Write lots of tests without using mocks.
- **Error Handling**: Use Result/Option. Do not ignore errors. An error should be passed up callers until it reaches an error handler that properly handles the error by terminating the program in an exit state or returning an HTTP error code.
- **Constants**: Define thresholds and parameters as named constants
- **Documentation**: When the purpose of code is not easy to determine, document it. First try to make the code itself easier to understand.
- **Tracing**: Add lots of debug level logging statements. Programs should be able to set the log level via an environment variable or CLI. Info level statements should show what is happening in the program at a high level.

# Workflow
- update the local main to the latest from origin and branch from that.
- Planning
  * Create a plan
    * Describe the changes at different levels of detail
      * start with the high-level architecture of the changes
      * end with code implementation details
    * Create state diagrams for all state changes
      * This can be done with ascii tables, art, or an html artifact
    * Suggest breaking larger changes into iterative steps
    * Describe the future phases of coding (including tests and documentation) and verification, 
  * get feedback and rework the plan
    * Have engineering-lead review any new features or non-trivial bug fixes
  * get approval before implementing
- Changing data
  * When updating database data, first create a backup of the existing database
- Coding
  * When deviating from the plan, ask for approval
  * Write tests before writing code and run tests frequently
  * Check the code using `cargo check` as frequently as possible
  * Run `just lint` and resolve all warnings
  * Documentation
    * Update technical documentation
    * Add state change diagrams to documentation
    * Create any new documentation files that are needed
    * Put information from this change into a file in ./docs/change
- Code review
  * Have engineeering-lead do a code review before verification
    * Do a follow up review of the changes made during verification
- Verification
  * verify manually that the changes work as expected in the application
  * start up a server on a separate port with a separate test database `--db` and a separate `--workdir` directory for saving concert information
  * test edge cases and failure modes in addition to the happy path
  * use Playwright to confirm visual/interaction aspects of the UI
    * maintain playwright scripts as e2e tests. this is only necessary where Foldkit tests are not adequate.
- Pull request
  * send a pull request using ./scripts/github/gh-app-pr-create.sh
  * for the commit and PR description point to what is added in ./docs/change 