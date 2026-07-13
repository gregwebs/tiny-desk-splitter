# Add a Test Control HTTP Adapter for Hurl

Status: Accepted

Hurl tests should use a Test Control HTTP Adapter on the same loopback-only
test-control listener as the raw JSON-RPC endpoint. The adapter accepts concise
`POST /test/...` requests, translates them into in-process JSON-RPC calls, and
keeps raw JSON-RPC available at the listener root for debugging adapter issues.

This keeps generated JSON-RPC methods as the server-side contract while making
Hurl files read like test setup instead of protocol boilerplate. The trade-off
is that the test-control listener exposes two request shapes for the same
underlying methods; documentation should treat adapter routes as the normal
Hurl authoring interface and raw JSON-RPC as an implementation/debug fallback.
