import asyncio
import contextlib
import sys

from popsub import Client, ClientConfig, PasswordCredentials


async def main(room: str) -> None:
    config = ClientConfig("ws://127.0.0.1:8080", PasswordCredentials("alice", "alice-password"))
    async with await Client.connect(config) as client:
        temperature = 20.0
        while True:
            await client.publish(f"sensors/{room}/temp", f"{temperature:.1f}")
            temperature += 0.1
            await asyncio.sleep(1)


if __name__ == "__main__":
    with contextlib.suppress(KeyboardInterrupt):
        asyncio.run(main(sys.argv[1]))
