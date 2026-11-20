"""A minimal in-process PopSub server for the client's tests.

It speaks just enough of the protocol (login, auth, subscribe, unsubscribe,
publish, ack and `list_users`) to drive the client, runs in its own thread
with its own event loop so the test's event loop holds only the client's
tasks, and records every frame it receives.
"""

from __future__ import annotations

import json
import threading
from typing import Any

from websockets.sync.server import Server, ServerConnection, serve

PASSWORD = "right-password"
"""The only password the fake server accepts."""


class FakeServer:
    """A thread running a fake PopSub server on a free port.

    Attributes:
        received: Every frame received, in order.
        duplicate_deliveries: Send every message twice, as a server resending
            an unacknowledged message would.
        swallow_publishes: Neither store nor answer a publish, as when the
            connection drops before the server handles it.
        honours_hold: Answer an `auth` that carries `hold_resumed` with
            `held`, as a server that holds a session's topics does.
    """

    def __init__(self) -> None:
        """Builds a server that hasn't started."""
        self.received: list[dict[str, Any]] = []
        self.duplicate_deliveries = False
        self.swallow_publishes = False
        self.honours_hold = False
        self._logs: dict[str, list[dict[str, Any]]] = {}
        self._connections: list[ServerConnection] = []
        self._lock = threading.Lock()
        self._server: Server | None = None
        self._thread: threading.Thread | None = None

    @property
    def url(self) -> str:
        """The server's WebSocket URL.

        Returns:
            `ws://127.0.0.1:<port>`.

        Raises:
            RuntimeError: When the server hasn't started.
        """
        if self._server is None:
            raise RuntimeError("the fake server isn't running")
        port = self._server.socket.getsockname()[1]
        return f"ws://127.0.0.1:{port}"

    def start(self) -> None:
        """Starts listening on a free port in a background thread."""
        self._server = serve(self._handle, "127.0.0.1", 0)
        self._thread = threading.Thread(target=self._server.serve_forever, daemon=True)
        self._thread.start()

    def drop_connections(self, code: int = 1001) -> None:
        """Closes every open connection with a close code.

        Args:
            code: The close code.
        """
        with self._lock:
            connections = list(self._connections)
        for connection in connections:
            connection.close(code)

    def stop(self) -> None:
        """Closes every connection and stops listening."""
        self.drop_connections()
        if self._server is not None:
            self._server.shutdown()
        if self._thread is not None:
            self._thread.join(timeout=5)

    def frames(self, kind: str) -> list[dict[str, Any]]:
        """Lists the received frames of one type.

        Args:
            kind: The frame type, such as `subscribe`.

        Returns:
            The frames, in order.
        """
        with self._lock:
            return [frame for frame in self.received if frame.get("type") == kind]

    def _handle(self, connection: ServerConnection) -> None:
        """Serves one connection until it closes.

        Args:
            connection: The connection.
        """
        with self._lock:
            self._connections.append(connection)
        subscribed: set[str] = set()
        try:
            for raw in connection:
                frame = json.loads(raw)
                with self._lock:
                    self.received.append(frame)
                for reply in self._replies(frame, subscribed):
                    connection.send(json.dumps(reply))
        finally:
            with self._lock:
                self._connections.remove(connection)

    def _replies(self, frame: dict[str, Any], subscribed: set[str]) -> list[dict[str, Any]]:
        """Works out the frames that answer one request.

        Args:
            frame: The request.
            subscribed: This connection's subscribed topics, updated in place.

        Returns:
            The frames to send, in order, each carrying the request's id if
            it had one. Unknown requests get no reply.
        """
        handlers = {
            "login": self._login,
            "auth": self._auth,
            "subscribe": self._subscribe,
            "unsubscribe": self._unsubscribe,
            "publish": self._publish,
            "list_users": self._list_users,
        }
        handler = handlers.get(str(frame.get("type")))
        if handler is None:
            return []
        replies = handler(frame, subscribed)
        if "id" in frame and replies and replies[0].get("type") != "message":
            replies[0] = {**replies[0], "id": frame["id"]}
        return replies

    def _login(self, frame: dict[str, Any], subscribed: set[str]) -> list[dict[str, Any]]:
        """Answers a login.

        Args:
            frame: The login.
            subscribed: Unused.

        Returns:
            `logged_in` for the right password, `invalid_credentials` otherwise.
        """
        if frame.get("password") == PASSWORD:
            return [{"type": "logged_in", "token": "token"}]
        return [{"type": "error", "code": "invalid_credentials", "message": "no"}]

    def _auth(self, frame: dict[str, Any], subscribed: set[str]) -> list[dict[str, Any]]:
        """Answers an auth.

        Args:
            frame: The auth.
            subscribed: Unused.

        Returns:
            `authenticated` with limits and nothing resumed, and with `held`
            when the auth asked for the hold and the server honours it.
        """
        limits = {
            "max_inflight": 100,
            "session_expiry_secs": 60,
            "heartbeat_secs": 30,
            "idle_timeout_secs": 90,
            "max_payload_bytes": 1024,
            "max_topic_bytes": 120,
        }
        reply: dict[str, Any] = {"type": "authenticated", "protocol": 1, "limits": limits, "resumed": []}
        if self.honours_hold and frame.get("hold_resumed") is True:
            reply["held"] = True
        return [reply]

    def _subscribe(self, frame: dict[str, Any], subscribed: set[str]) -> list[dict[str, Any]]:
        """Answers a subscribe; only new messages are delivered.

        Args:
            frame: The subscribe.
            subscribed: This connection's subscribed topics, updated in place.

        Returns:
            `subscribed` starting after the stored messages.
        """
        topic = str(frame.get("topic", ""))
        subscribed.add(topic)
        with self._lock:
            stored = len(self._logs.setdefault(topic, []))
        return [{"type": "subscribed", "topic": topic, "replay_from": stored + 1, "replay_to": stored}]

    def _unsubscribe(self, frame: dict[str, Any], subscribed: set[str]) -> list[dict[str, Any]]:
        """Answers an unsubscribe.

        Args:
            frame: The unsubscribe.
            subscribed: This connection's subscribed topics, updated in place.

        Returns:
            `unsubscribed`.
        """
        topic = str(frame.get("topic", ""))
        subscribed.discard(topic)
        return [{"type": "unsubscribed", "topic": topic}]

    def _list_users(self, frame: dict[str, Any], subscribed: set[str]) -> list[dict[str, Any]]:
        """Answers `list_users`.

        Args:
            frame: Unused.
            subscribed: Unused.

        Returns:
            An empty `users` frame.
        """
        return [{"type": "users", "users": []}]

    def _publish(self, frame: dict[str, Any], subscribed: set[str]) -> list[dict[str, Any]]:
        """Stores a publish and delivers it to this connection if subscribed.

        Args:
            frame: The publish.
            subscribed: This connection's subscribed topics.

        Returns:
            `published` for QoS 1, then the delivery (twice when duplicating);
            nothing while publishes are swallowed.
        """
        if self.swallow_publishes:
            return []
        topic = str(frame.get("topic", ""))
        qos = int(frame.get("qos") or 0)
        with self._lock:
            log = self._logs.setdefault(topic, [])
            seq = len(log) + 1
            message = {
                "type": "message",
                "topic": topic,
                "seq": seq,
                "message_id": frame.get("message_id") or f"generated-{seq}",
                "payload": frame.get("payload", ""),
                "qos": qos,
                "published_at_ms": 1_794_800_000_000,
            }
            if "encoding" in frame:
                message["encoding"] = frame["encoding"]
            log.append(message)
        replies: list[dict[str, Any]] = []
        if qos == 1:
            replies.append({"type": "published", "message_id": message["message_id"], "seq": seq})
        if topic in subscribed:
            replies.extend([message, message] if self.duplicate_deliveries else [message])
        return replies
