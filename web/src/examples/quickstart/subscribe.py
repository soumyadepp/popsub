import asyncio
import contextlib

from popsub import Client, ClientConfig, PasswordCredentials


async def main() -> None:
    config = ClientConfig("ws://127.0.0.1:8080", PasswordCredentials("alice", "alice-password"))
    async with await Client.connect(config) as client:
        greetings = await client.subscribe("greetings")
        print("waiting for messages on greetings")
        async for message in greetings:
            print(f"{message.topic} #{message.seq}: {message.payload}")


if __name__ == "__main__":
    with contextlib.suppress(KeyboardInterrupt):
        asyncio.run(main())
