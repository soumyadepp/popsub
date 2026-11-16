# popsub_protocol

The WebSocket protocol: every frame clients and the server exchange. The server (transport, broker, binary) and client SDKs all use these types, so they can't drift apart. It depends only on `serde`; keep it that way so any client can use it.

## Files

- `src/lib.rs`: `PROTOCOL_VERSION`, `RequestId` (a number or a string, echoed back in the same form), and the evolution rules.
- `src/client.rs`: `ClientFrame` (an optional `id` plus a `ClientMessage`) and `StartFrom`.
- `src/close.rs`: `CloseCode`, the WebSocket close codes the server sends, with `should_reconnect`.
- `src/server.rs`: `ServerFrame` (an optional `id` plus a `ServerMessage`), `ErrorCode`, `Limits`, and the structs inside frames (`Delivery`, `Subscribed`, `Truncated`, `Stalled`).
- `src/tests.rs`: the exact JSON of every frame.

## Rules

- The `id` sits beside `type` in the same JSON object, through `#[serde(flatten)]`. Replies use `ServerFrame::reply(id, ...)`; frames the server sends on its own use `ServerFrame::event(...)`.
- Within a version, only add frames, fields and error codes. Optional fields need `#[serde(default)]` and usually `skip_serializing_if`. Removing or renaming anything needs a new `PROTOCOL_VERSION`.
- Unknown fields are ignored on parse (no `deny_unknown_fields`); clients rely on that to talk to newer servers.
- Close codes are part of the protocol too: never renumber one, and give a new one a `4xxx` number and a case in the close-code test.
- Every new frame or field gets an exact-JSON case in `src/tests.rs`, and the README protocol section and `docs/DESIGN.md` section 4 change in the same commit.

## Tests

- `make test-crate CRATE=popsub_protocol`.
