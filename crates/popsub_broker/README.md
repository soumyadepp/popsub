# Popsub Broker

Contains the broker implementation (topics, subscriptions, message routing, persistence integration).

Public API:

- `Broker` — register clients, subscribe/unsubscribe clients, publish messages, handle acks.

This crate is used by `popsub_transport` (WebSocket server) and the `popsub` binary.
