# popsub_connection

The server's record of one connected socket, shared by the broker and transport, and its outgoing queue.

Exports:

- `Client`: connection id, outgoing queue, authenticated flag, username and durable session.
- `Outbox`: the connection's outgoing queue, with a byte budget and a close signal.
