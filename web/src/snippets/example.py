import asyncio

from popsub import Client, ClientConfig, PasswordCredentials, StartFrom


async def main() -> None:
    config = ClientConfig(
        "ws://127.0.0.1:8080",
        PasswordCredentials("alice", "alice-password"),
        session_id="orders-worker",
    )
    async with await Client.connect(config) as client:
        messages = await client.subscribe("orders/#", StartFrom.seq(1))
        await client.publish_confirmed("orders/42", "order 42 shipped")
        async for message in messages:
            print(f"{message.topic} #{message.seq}: {message.payload}")


try:
    asyncio.run(main())
except KeyboardInterrupt:
    pass
