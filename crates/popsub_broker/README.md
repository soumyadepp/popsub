# Popsub Broker

Contains the broker implementation (topics, subscriptions, message routing, persistence integration).

Public API:

- `Broker`: registers clients, subscribes and unsubscribes them, publishes messages and handles acks.

This crate is used by `popsub_transport` (WebSocket server) and the `popsub` binary.
