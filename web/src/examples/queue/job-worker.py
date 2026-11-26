import asyncio
import contextlib
import sys

from popsub import AckMode, Client, ClientConfig, PasswordCredentials, StartFrom


async def main(name: str) -> None:
    config = ClientConfig(
        "ws://127.0.0.1:8080",
        PasswordCredentials("alice", "alice-password"),
        ack_mode=AckMode.MANUAL,
    )
    async with await Client.connect(config) as client:
        jobs = await client.subscribe_shared("jobs/resize", "resizers", StartFrom.new())
        async for job in jobs:
            print(f"{name} took {job.payload}")
            await asyncio.sleep(1)
            print(f"{name} finished {job.payload}")
            job.ack()


if __name__ == "__main__":
    with contextlib.suppress(KeyboardInterrupt):
        asyncio.run(main(sys.argv[1]))
