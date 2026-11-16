"""Asyncio client for PopSub, a WebSocket publish/subscribe broker.

`Client` keeps a connection to a PopSub server and handles what every
application would otherwise have to: logging in and refreshing the token,
reconnecting with backoff (and knowing from the server's close code when not
to), resubscribing without gaps, resending confirmed publishes without
duplicating them, acknowledging QoS 1 messages, and making sure each
`(topic, seq)` reaches the application once.

Example:
    import asyncio
    from popsub import Client, ClientConfig, PasswordCredentials, StartFrom

    async def main() -> None:
        config = ClientConfig("ws://127.0.0.1:8080", PasswordCredentials("alice", "secret"))
        async with await Client.connect(config) as client:
            orders = await client.subscribe("orders", StartFrom.new())
            stored = await client.publish_confirmed("orders", "order 42 shipped")
            print("stored as seq", stored.seq)
            async for message in orders:
                print(message.topic, message.seq, message.payload)

    asyncio.run(main())

The protocol is described in the repository's `docs/DESIGN.md` section 4.
"""

from popsub.admin import Admin
from popsub.client import Client
from popsub.config import (
    AckMode,
    ClientConfig,
    Credentials,
    PasswordCredentials,
    ReconnectPolicy,
    TokenCredentials,
)
from popsub.errors import (
    ClosedError,
    ConfigError,
    ConnectError,
    PopSubError,
    ProtocolError,
    RequestTimeoutError,
    ServerError,
)
from popsub.message import (
    AuthExpiring,
    AuthRefreshed,
    Connected,
    Disconnected,
    Event,
    EventStream,
    Message,
    Published,
    ServerErrorEvent,
    Stalled,
    Subscription,
    Truncated,
    Unsubscribed,
)
from popsub.protocol import (
    PROTOCOL_VERSION,
    Access,
    CloseCode,
    RoleKind,
    RoleSpec,
    StartFrom,
    TopicGrant,
    UserInfo,
)

__all__ = [
    "PROTOCOL_VERSION",
    "Access",
    "AckMode",
    "Admin",
    "AuthExpiring",
    "AuthRefreshed",
    "Client",
    "ClientConfig",
    "CloseCode",
    "ClosedError",
    "ConfigError",
    "ConnectError",
    "Connected",
    "Credentials",
    "Disconnected",
    "Event",
    "EventStream",
    "Message",
    "PasswordCredentials",
    "PopSubError",
    "ProtocolError",
    "Published",
    "ReconnectPolicy",
    "RequestTimeoutError",
    "RoleKind",
    "RoleSpec",
    "ServerError",
    "ServerErrorEvent",
    "Stalled",
    "StartFrom",
    "Subscription",
    "TokenCredentials",
    "TopicGrant",
    "Truncated",
    "Unsubscribed",
    "UserInfo",
]
