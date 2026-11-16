# PopSub protocol schema

Generated from the `popsub_protocol` crate, so it always matches what the server accepts and sends. Don't edit these files by hand: change the Rust types and run `make schema`. CI fails if they are out of date.

| File | Describes |
|---|---|
| `client.schema.json` | Every frame a client sends (JSON Schema draft 2020-12) |
| `server.schema.json` | Every frame the server sends (JSON Schema draft 2020-12) |
| `close-codes.json` | The WebSocket close codes the server sends, and whether a client should reconnect after each |

Every frame is a JSON object tagged by `type`; each `oneOf` entry is one frame type. Any client frame may carry an `id` (a number or a string), echoed in the reply. `x-protocol-version` is the protocol version the files describe.

Within a protocol version, frames, fields, error codes and close codes are only ever added, so a client generated from these files should ignore fields and frame types it doesn't know, and treat an unknown error code like `internal`. The rules and the meaning of each frame are in `docs/DESIGN.md` section 4.
