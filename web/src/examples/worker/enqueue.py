import asyncio
import sys

from popsub import Client, ClientConfig, PasswordCredentials


async def main(count: int) -> None:
    config = ClientConfig("ws://127.0.0.1:8080", PasswordCredentials("alice", "alice-password"))
    async with await Client.connect(config) as client:
        for k in range(1, count + 1):
            topic = f"invoices/{k}"
            stored = await client.publish_confirmed(topic, f"invoice {k}")
            print(f"queued {topic} as #{stored.seq}")


if __name__ == "__main__":
    asyncio.run(main(int(sys.argv[1])))
