import asyncio
import sys

from popsub import Client, ClientConfig, PasswordCredentials, StartFrom, Subscription


async def show(messages: Subscription) -> None:
    async for message in messages:
        print(message.payload)


async def main(name: str) -> None:
    config = ClientConfig("ws://127.0.0.1:8080", PasswordCredentials("alice", "alice-password"))
    async with await Client.connect(config) as client:
        messages = await client.subscribe("chat/lobby", StartFrom.last(20))
        printer = asyncio.create_task(show(messages))
        while line := await asyncio.to_thread(sys.stdin.readline):
            text = line.rstrip("\r\n")
            if text:
                await client.publish("chat/lobby", f"{name}: {text}")
    await printer


if __name__ == "__main__":
    asyncio.run(main(sys.argv[1]))
