# Tiny Desk Splitter

Tiny Desk Splitter tracks the lifecycle of Tiny Desk concerts from discovery through download, splitting, playback, and archiving.

## Language

**Test Control API**:
A test-only HTTP control surface mounted inside the `concert-web` process so black-box HTTP tests can arrange fixture state and inspect necessary postconditions without linking to application internals. It is compiled only for non-release test-control builds and started only when explicitly requested.
_Avoid_: Seed proxy, seed backdoor

**Seed Result**:
The object returned by a Test Control API seed method that identifies the exact fixture data it created. Seed Results should reuse public API JSON shapes when practical, adding test-only fields only when tests need stable handles that the public API should not expose.
_Avoid_: Fixture lookup by assumed ID

**Assertion API**:
A Test Control API method that verifies or returns test-only facts that should not be exposed through the real product API. Assertion APIs keep Hurl tests focused on public behavior without forcing internal details into production response shapes.
_Avoid_: Test-only production fields
