# popsub_sdk

The Rust client for [PopSub](https://github.com/soumyadepp/popsub), a WebSocket publish/subscribe broker with per-topic permissions, durable sessions and at-least-once delivery.

The client handles what every application would otherwise write itself: logging in and refreshing the token, reconnecting with backoff (and not reconnecting when the server's close code says not to), resubscribing without gaps, resending confirmed publishes without duplicating them, acknowledging QoS 1 messages, and making sure each `(topic, seq)` reaches the application once.

```toml
[dependencies]
popsub_sdk = "0.1"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

```rust,no_run
use popsub_sdk::{Client, ClientConfig, Credentials, StartFrom};

#[tokio::main]
async fn main() -> Result<(), popsub_sdk::Error> {
    let config = ClientConfig::new("ws://127.0.0.1:8080", Credentials::password("alice", "secret"))
        .session("alice-laptop");
    let client = Client::connect(config).await?;

    let mut orders = client.subscribe("orders", StartFrom::Resume).await?;
    let stored = client.publish_confirmed("orders", "order 42 shipped").await?;
    println!("stored as seq {}", stored.seq);

    while let Some(message) = orders.next().await {
        println!("{} #{}: {}", message.topic, message.seq, message.payload);
    }
    Ok(())
}
```

`cargo run -p popsub_sdk --example subscribe` and `--example publish` run the same thing against a local server; the repository's [`examples/README.md`](https://github.com/soumyadepp/popsub/blob/main/examples/README.md) walks through it.

Bodies that aren't text go through `publish_bytes` and `publish_bytes_confirmed`, and come back through `Message::bytes`.

What the delivery guarantees are, and where they stop, is in the repository's [design document](https://github.com/soumyadepp/popsub/blob/main/docs/DESIGN.md). Version 0.x: the protocol and this API may still change between minor versions.

Licensed under Apache-2.0.
