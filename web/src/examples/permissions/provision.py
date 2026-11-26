import asyncio
import sys

from popsub import (
    Access,
    Client,
    ClientConfig,
    PasswordCredentials,
    RoleKind,
    RoleSpec,
    ServerError,
    TopicGrant,
)


async def main(username: str) -> None:
    config = ClientConfig("ws://127.0.0.1:8080", PasswordCredentials("admin", "password"))
    async with await Client.connect(config) as client:
        inbox = f"inbox/{username}/#"
        role = RoleSpec(RoleKind.USER, (TopicGrant(inbox, Access.SUBSCRIBE),))
        try:
            await client.admin().create_user(username, f"{username}-password", role)
        except ServerError as error:
            if error.code != "user_exists":
                raise
            print(f"{username} already exists")
        else:
            print(f"created {username}, may read {inbox}")


if __name__ == "__main__":
    asyncio.run(main(sys.argv[1]))
