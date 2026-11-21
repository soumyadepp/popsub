"""Subscribes to a topic pattern with a durable session and prints every message.

Stop it and run it again: messages published while it was away arrive once it is back, each
once, because the session keeps its place.

    python examples/subscribe.py 'orders/#'

``POPSUB_URL`` defaults to ``ws://127.0.0.1:8080``, ``POPSUB_USERNAME`` to ``alice`` and
``POPSUB_USER_PASSWORD`` to ``alice-password``; the pattern defaults to ``orders/#``.
"""

import asyncio
import contextlib
import os
import sys

from popsub import Client, ClientConfig, PasswordCredentials, StartFrom


async def main(pattern: str) -> None:
    """Connects with a durable session, subscribes and prints each message until stopped.

    Args:
        pattern: The topic or topic pattern to subscribe to.
    """
    config = ClientConfig(
        os.environ.get("POPSUB_URL", "ws://127.0.0.1:8080"),
        PasswordCredentials(
            os.environ.get("POPSUB_USERNAME", "alice"),
            os.environ.get("POPSUB_USER_PASSWORD", "alice-password"),
        ),
        session_id="examples-subscriber",
    )
    async with await Client.connect(config) as client:
        messages = await client.subscribe(pattern, StartFrom.resume())
        print(f"subscribed to {pattern}; waiting for messages (Ctrl+C to stop)")
        async for message in messages:
            print(f"{message.topic} #{message.seq}: {message.payload}")


if __name__ == "__main__":
    with contextlib.suppress(KeyboardInterrupt):
        asyncio.run(main(sys.argv[1] if len(sys.argv) > 1 else "orders/#"))
