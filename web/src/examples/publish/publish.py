import asyncio
import sys

from popsub import Client, ClientConfig, PasswordCredentials, ServerError


async def main(topic: str, text: str) -> int:
    config = ClientConfig("ws://127.0.0.1:8080", PasswordCredentials("alice", "alice-password"))
    async with await Client.connect(config) as client:
        try:
            stored = await client.publish_confirmed(topic, text)
        except ServerError as error:
            print(f"refused: {error.code}", file=sys.stderr)
            return 1
        print(f"stored on {topic} as #{stored.seq}")
        return 0


if __name__ == "__main__":
    sys.exit(asyncio.run(main(sys.argv[1], sys.argv[2])))
