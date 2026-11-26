import asyncio
import contextlib

from popsub import Client, ClientConfig, PasswordCredentials, StartFrom


async def main() -> None:
    config = ClientConfig("ws://127.0.0.1:8080", PasswordCredentials("alice", "alice-password"))
    async with await Client.connect(config) as client:
        readings = await client.subscribe("sensors/+/temp", StartFrom.last(1))
        latest: dict[str, str] = {}
        async for message in readings:
            latest[message.topic.split("/")[1]] = message.payload
            print("  ".join(f"{room} {value}" for room, value in sorted(latest.items())))


if __name__ == "__main__":
    with contextlib.suppress(KeyboardInterrupt):
        asyncio.run(main())
