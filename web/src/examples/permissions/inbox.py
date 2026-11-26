import asyncio
import contextlib
import sys

from popsub import Client, ClientConfig, PasswordCredentials, ServerError, StartFrom


async def main(username: str) -> None:
    config = ClientConfig(
        "ws://127.0.0.1:8080", PasswordCredentials(username, f"{username}-password")
    )
    async with await Client.connect(config) as client:
        try:
            await client.publish_confirmed(f"inbox/{username}/notes", "hello")
        except ServerError as error:
            print(f"publish refused: {error.code}")
        messages = await client.subscribe(f"inbox/{username}/#", StartFrom.seq(1))
        async for message in messages:
            print(f"{message.topic}: {message.payload}")


if __name__ == "__main__":
    with contextlib.suppress(KeyboardInterrupt):
        asyncio.run(main(sys.argv[1]))
