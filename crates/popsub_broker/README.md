# Popsub Broker

Contains the broker implementation (topics, subscriptions, message routing, persistence integration).

Public API:

- `SharedBroker`: the cloneable handle everything uses. Publishes, subscribes, unsubscribes, acknowledges and attaches durable sessions, sending each topic's publishes and subscribes through that topic's writer task, and runs the retry, retention and session expiry loops.
- `Broker`: the synchronous engine behind `SharedBroker`'s lock. It holds the connected clients and subscriptions, delivers records, and tracks acknowledgements and retries. It doesn't publish or touch storage.

This crate is used by `popsub_transport` (WebSocket server), the `popsub` binary and `popsub_scenarios`.
