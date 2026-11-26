import asyncio
import contextlib
import sys
import time

from popsub import Client, ClientConfig, PasswordCredentials, StartFrom


async def main(minutes: int) -> None:
    config = ClientConfig("ws://127.0.0.1:8080", PasswordCredentials("alice", "alice-password"))
    since_ms = int(time.time() * 1000) - minutes * 60_000
    async with await Client.connect(config) as client:
        entries = await client.subscribe("audit/#", StartFrom.since_ms(since_ms))
        async for message in entries:
            seconds = (int(time.time() * 1000) - message.published_at_ms) // 1000
            print(f"{seconds} s ago  {message.topic}  {message.payload}")


if __name__ == "__main__":
    with contextlib.suppress(KeyboardInterrupt):
        asyncio.run(main(int(sys.argv[1])))
