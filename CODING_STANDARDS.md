# Code Quality Guidelines

## DRY

Follow the DRY (Don't Repeat Yourself) Principle and Avoid Duplicating Code or Logic.
Avoid writing the same code more than once. Instead, reuse your code using functions, classes, modules, libraries, or other abstractions.

* ALWAYS maintain a single source of truth for configuration values — the same constant or config value defined in two places will diverge and cause bugs.
* ALWAYS prefer readability over DRY when the abstraction requires indirection that obscures what the code does — a small amount of duplication is often better than an obscure helper.
* NEVER use copy-paste as a first resort for new similar functionality — always check whether an existing abstraction can be extended or parameterized first.

Caution:
* Dont' quickly apply DRY to coincidentally similar code that serves different purposes. Unrelated concepts should not be coupled. In this case DRY should require a function that is generic with respect to either concern.
* It is okay to wait to use a shared abstraction until you have 3 concrete instances of the same logic if the value initially appears low for combining just 2 instances.

## Constants

Define thresholds and parameters as named constants.

## Types

Use strong typing. Make invalid states unrepresentable with types. Represent the problem domain properly with types. Use enums and case analysis.

Examples:
* After a string is validated/parsed it should be given a new type.
* Don't use dynamic types (e.g. as `any` in Go). Use generic types. If using a library that uses dynamic types, convert them to strong types as quickly as possible.
* Don't use the same type multiple times in a row for function arguments (these could get confused). Use a newtype for one of the arguments or use named arguments (via a struct or other language construct) for some of the function arguments.
* The callee should use validated types that the caller should produce. For example, if a list must be non-empty, we can create a non-empty list type.


## Error Handling

Use Result/Option but favor Result so there is information about failure modes.

Do not ignore errors. Logging an error at the error site is equivalent to ignoring it. Panicing at the error site is unsafe. An error should be passed up callers until it reaches an error handler that can deal with it. Catch all error handlers should only exist at a top level and must properly handle the error by terminating the program in an exit state or returning an error code. Error handlers may also log the error.

## Tracing

We should be able to roughly understand what is going on in a program by looking at the logs.
Add lots of debug level logging statements.
Programs should be able to set the log level via an environment variable or CLI.
Info level statements should show what is happening in the program at a high level.

## Testing

Refactor code into small testable functions. Write lots of tests without using mocks.
Write seeds for creating data that is needed for testing.

## Documentation

Comments state design constraints, invariants, and why. Not what the code does. Do not write comments that restate what the next line does. write or refactor code to make what it is doing easier to understand
