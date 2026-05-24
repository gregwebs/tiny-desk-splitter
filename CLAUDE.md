See README.md for an overview of the project

## Code Style Guidelines
- **Imports**: Group standard library, external crates/packages, then local modules
- **Rust**: Use standard Rust formatting (`cargo fmt`)
- **Naming**: snake_case for variables/functions, CamelCase for types

## Code Quality Guidelines
- **DRY**: Avoid code duplication. Use variables, functions, and modules to share code
- **Types**: Use strong typing where possible. For example, after a string is validated/parsed it should be given a new type.
- **Testing**: Refactor code into small testable functions. Write lots of tests without using mocks.
- **Error Handling**: Use Result/Option. Do not ignore errors. An error should be passed up callers until it reaches an error handler that properly handles the error by terminating the program in an exit state or returning an HTTP error code.
- **Constants**: Define thresholds and parameters as named constants
- **Documentation**: When the purpose of code is not easy to determine, document it. First try to make the purpose easier to understand.
- **Tracing**: Add lots of debug level logging statements. Programs should be able to set the log level via an environment variable or CLI. Info level statements should show what is happening in the program at a high level.

# Workflow
- plan the changes and get approval for the changes
- check the code using `cargo check` as frequently as possible- every time a series of code changes is complete enough to pass `cargo check`.
- write tests for all new functions
- run tests after every series of changes
- update documentation
