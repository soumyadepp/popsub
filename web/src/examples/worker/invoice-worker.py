import asyncio
import contextlib

from popsub import AckMode, Client, ClientConfig, PasswordCredentials, StartFrom


async def main() -> None:
    config = ClientConfig(
        "ws://127.0.0.1:8080",
        PasswordCredentials("alice", "alice-password"),
        session_id="invoice-worker",
        ack_mode=AckMode.MANUAL,
    )
    async with await Client.connect(config) as client:
        invoices = await client.subscribe("invoices/#", StartFrom.seq(1))
        async for message in invoices:
            print(f"processing {message.topic} #{message.seq}: {message.payload}")
            message.ack()


if __name__ == "__main__":
    with contextlib.suppress(KeyboardInterrupt):
        asyncio.run(main())
