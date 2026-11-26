import asyncio

from popsub import Client, ClientConfig, PasswordCredentials


async def main() -> None:
    config = ClientConfig("ws://127.0.0.1:8080", PasswordCredentials("alice", "alice-password"))
    async with await Client.connect(config) as client:
        for k in range(1, 7):
            payload = f"job-{k}"
            await client.publish_confirmed("jobs/resize", payload)
            print(f"queued {payload}")


if __name__ == "__main__":
    asyncio.run(main())
