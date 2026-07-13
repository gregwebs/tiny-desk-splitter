# Test Control Generated Seed Methods

Slice 3 of the Test Control HTTP Adapter plan restores seed methods to
jsonrpsee-generated `#[rpc]` request-object methods. The server-side Test
Control API now declares `test.seed_listing`, `test.seed_scraped_concert`, and
`test.seed_lifecycle_concert` in the generated RPC trait instead of manually
registering them on `RpcModule`.

The Hurl-facing adapter routes stay flat:

```hurl
POST {{test_control_url}}/test/seed/listing
Content-Type: application/json
{
  "title": "Example"
}
```

The adapter wraps only seed route bodies under `params` before dispatching to
JSON-RPC in-process. Raw JSON-RPC at `{{test_control_url}}` therefore uses the
generated request-object shape:

```json
{
  "jsonrpc": "2.0",
  "id": "raw",
  "method": "test.seed_listing",
  "params": {
    "params": {
      "title": "Example"
    }
  }
}
```

Assertion routes remain flat, and `test.assert_concert_state` keeps its
existing generated multi-argument method shape.

Verification added:

- adapter translation tests prove `/test/seed/*` keeps accepting flat Hurl
  bodies while wrapping for generated seed RPC methods
- raw JSON-RPC tests prove generated seed methods accept nested
  `params.params` and reject the old flat raw shape
- `hurl/test_control_adapter.hurl` exercises the flat adapter seed route and
  the nested raw JSON-RPC debug fallback
