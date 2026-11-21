"""Publishes one message and waits until the server has stored it.

    python examples/publish.py orders/python 'hello from Python'

``POPSUB_URL`` defaults to ``ws://127.0.0.1:8080``, ``POPSUB_USERNAME`` to ``alice`` and
``POPSUB_USER_PASSWORD`` to ``alice-password``. The topic defaults to ``orders/python`` and the
message to ``hello from Python``.
"""

import asyncio
import os
import sys

from popsub import Client, ClientConfig, PasswordCredentials


async def main(topic: str, payload: str) -> None:
    """Connects, publishes the message with confirmation and prints where it was stored.

    Args:
        topic: The topic to publish to.
        payload: The message.
    """
    config = ClientConfig(
        os.environ.get("POPSUB_URL", "ws://127.0.0.1:8080"),
        PasswordCredentials(
            os.environ.get("POPSUB_USERNAME", "alice"),
            os.environ.get("POPSUB_USER_PASSWORD", "alice-password"),
        ),
    )
    async with await Client.connect(config) as client:
        stored = await client.publish_confirmed(topic, payload)
        print(f"stored on {topic} as seq {stored.seq} ({stored.message_id})")


if __name__ == "__main__":
    arguments = sys.argv[1:]
    asyncio.run(
        main(
            arguments[0] if arguments else "orders/python",
            arguments[1] if len(arguments) > 1 else "hello from Python",
        )
    )
