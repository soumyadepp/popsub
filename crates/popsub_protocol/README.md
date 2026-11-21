# popsub_protocol

The wire protocol of [PopSub](https://github.com/soumyadepp/popsub), a WebSocket publish/subscribe broker: every frame clients and the server exchange, the error codes, the close codes and `PROTOCOL_VERSION`.

Frames are JSON objects tagged by `type`. A client frame may carry an `id`; the reply carries the same `id`. Frames the server sends on its own have none: deliveries, `truncated`, `stalled`, `auth_expiring`, an `unsubscribed` that ends a subscription the client didn't leave, and the `subscribed` for each topic a wildcard subscription joins. The crate depends only on `serde`, so any Rust client can use it. With the `schema` feature it produces a JSON Schema of every frame; the generated schema is also committed in the repository under [`docs/protocol`](https://github.com/soumyadepp/popsub/tree/main/docs/protocol), for clients in other languages.

Most applications want [`popsub_sdk`](https://crates.io/crates/popsub_sdk), the client built on these types, rather than this crate directly.

Within a protocol version, frames, fields and error codes are only ever added, and clients ignore what they don't know. Removing or changing anything needs a new version. Version 0.x of the crate: its Rust API may still change between minor versions.

Licensed under Apache-2.0.
