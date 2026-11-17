"""`Admin`, the account management requests, for clients logged in as an admin.

Each call is one request and its reply. None is queued while the client is
disconnected or resent after a reconnect, because repeating a change such as
`create_user` isn't safe: a call made while disconnected fails with
`ConnectError`, and so does one whose connection drops before the reply,
whether or not the server carried it out.
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Any

from popsub.errors import ProtocolError
from popsub.protocol import Access, RoleSpec, TopicInfo, UserInfo

if TYPE_CHECKING:
    from popsub.client import Client


class Admin:
    """The admin requests, from `Client.admin`.

    Every call raises `ServerError` with code `forbidden` unless the client
    is logged in as an enabled admin. Every call can also raise
    `ConnectError` (disconnected, or the connection dropped before the
    reply), `RequestTimeoutError` and `ClosedError`.
    """

    def __init__(self, client: Client) -> None:
        """Wraps a client.

        Args:
            client: The client the requests go through.
        """
        self._client = client

    async def create_user(self, username: str, password: str, role: RoleSpec) -> UserInfo:
        """Creates an account.

        Args:
            username: Its name.
            password: Its password.
            role: What it may do.

        Returns:
            The new account.

        Raises:
            ServerError: `user_exists`, `bad_request` for an invalid name,
                password or pattern, or `forbidden`.
        """
        return await self._user(
            {"type": "create_user", "username": username, "password": password, "role": role.to_json()}
        )

    async def get_user(self, username: str) -> UserInfo:
        """Reads an account.

        Args:
            username: The account.

        Returns:
            The account.

        Raises:
            ServerError: `user_not_found` or `forbidden`.
        """
        return await self._user({"type": "get_user", "username": username})

    async def list_users(self) -> list[UserInfo]:
        """Lists every account, sorted by name.

        Returns:
            The accounts.

        Raises:
            ServerError: `forbidden`.
            ProtocolError: When the server answers with something else.
        """
        reply = await self._client._request({"type": "list_users"})
        users = reply.get("users")
        if reply.get("type") != "users" or not isinstance(users, list):
            raise ProtocolError(f"expected users, got {reply.get('type')!r}")
        try:
            return [UserInfo.from_json(user) for user in users]
        except ValueError as error:
            raise ProtocolError(str(error)) from error

    async def set_role(self, username: str, role: RoleSpec) -> UserInfo:
        """Replaces an account's role.

        Live subscriptions the new role doesn't allow are ended.

        Args:
            username: The account.
            role: Its new role.

        Returns:
            The account as it now stands.

        Raises:
            ServerError: `user_not_found`, `bad_request` for an invalid
                pattern or for demoting the last enabled admin, or `forbidden`.
        """
        return await self._user({"type": "set_role", "username": username, "role": role.to_json()})

    async def grant(self, username: str, pattern: str, access: Access) -> UserInfo:
        """Grants an account a topic pattern, replacing any grant for exactly that pattern.

        Args:
            username: The account.
            pattern: The topic pattern.
            access: What it allows.

        Returns:
            The account as it now stands.

        Raises:
            ServerError: `user_not_found`, `bad_request` (an invalid pattern,
                an admin account, or publishing for a readonly one), or
                `forbidden`.
        """
        return await self._user(
            {"type": "grant", "username": username, "pattern": pattern, "access": Access(access).value}
        )

    async def revoke(self, username: str, pattern: str) -> UserInfo:
        """Removes one of an account's grants.

        Live subscriptions no longer allowed are ended.

        Args:
            username: The account.
            pattern: The pattern, exactly as granted.

        Returns:
            The account as it now stands.

        Raises:
            ServerError: `user_not_found`, `bad_request` when the account has
                no such grant, or `forbidden`.
        """
        return await self._user({"type": "revoke", "username": username, "pattern": pattern})

    async def set_password(self, username: str, password: str) -> UserInfo:
        """Sets an account's password.

        Tokens issued before it are cancelled, and the account's connections
        are closed so they log in again.

        Args:
            username: The account.
            password: The new password.

        Returns:
            The account.

        Raises:
            ServerError: `user_not_found`, `bad_request` for an invalid
                password, or `forbidden`.
        """
        return await self._user({"type": "set_password", "username": username, "password": password})

    async def set_enabled(self, username: str, enabled: bool) -> UserInfo:
        """Enables or disables an account.

        Disabling closes its connections with `account_disabled` and refuses
        its logins and tokens.

        Args:
            username: The account.
            enabled: Whether it may log in.

        Returns:
            The account as it now stands.

        Raises:
            ServerError: `user_not_found`, `bad_request` for disabling the
                last enabled admin, or `forbidden`.
        """
        return await self._user({"type": "set_enabled", "username": username, "enabled": enabled})

    async def delete_user(self, username: str) -> None:
        """Deletes an account and closes its connections with `account_disabled`.

        Args:
            username: The account.

        Raises:
            ServerError: `user_not_found`, `bad_request` for the last enabled
                admin, or `forbidden`.
            ProtocolError: When the server answers with something else.
        """
        reply = await self._client._request({"type": "delete_user", "username": username})
        if reply.get("type") != "user_deleted":
            raise ProtocolError(f"expected user_deleted, got {reply.get('type')!r}")

    async def list_topics(self) -> list[TopicInfo]:
        """Lists every topic that has a log, sorted by name.

        Returns:
            The topics.

        Raises:
            ServerError: `forbidden`.
            ProtocolError: When the server answers with something else.
        """
        reply = await self._client._request({"type": "list_topics"})
        topics = reply.get("topics")
        if reply.get("type") != "topics" or not isinstance(topics, list):
            raise ProtocolError(f"expected topics, got {reply.get('type')!r}")
        try:
            return [TopicInfo.from_json(topic) for topic in topics]
        except ValueError as error:
            raise ProtocolError(str(error)) from error

    async def get_topic(self, topic: str) -> TopicInfo:
        """Reads a topic's log bounds, message count and subscriber count.

        Args:
            topic: The topic.

        Returns:
            The topic.

        Raises:
            ServerError: `topic_not_found` when nothing was ever published
                to it, or `forbidden`.
            ProtocolError: When the server answers with something else.
        """
        return await self._topic({"type": "get_topic", "topic": topic})

    async def purge_topic(self, topic: str) -> TopicInfo:
        """Removes every message a topic holds.

        Its numbering carries on (the next publish gets `last_seq + 1`), live
        subscriptions stay, and a subscriber that hadn't caught up gets
        `truncated`.

        Args:
            topic: The topic.

        Returns:
            The topic after the purge.

        Raises:
            ServerError: `topic_not_found` or `forbidden`.
            ProtocolError: When the server answers with something else.
        """
        return await self._topic({"type": "purge_topic", "topic": topic})

    async def _topic(self, frame: dict[str, Any]) -> TopicInfo:
        """Sends a request answered by a `topic_info` frame.

        Args:
            frame: The request, without an id.

        Returns:
            The topic in the reply.

        Raises:
            ServerError: When the server refused it.
            ProtocolError: When the server answers with something else.
        """
        reply = await self._client._request(frame)
        if reply.get("type") != "topic_info":
            raise ProtocolError(f"expected topic_info, got {reply.get('type')!r}")
        try:
            return TopicInfo.from_json(reply.get("topic"))
        except ValueError as error:
            raise ProtocolError(str(error)) from error

    async def _user(self, frame: dict[str, Any]) -> UserInfo:
        """Sends a request answered by a `user` frame.

        Args:
            frame: The request, without an id.

        Returns:
            The account in the reply.

        Raises:
            ServerError: When the server refused it.
            ProtocolError: When the server answers with something else.
        """
        reply = await self._client._request(frame)
        if reply.get("type") != "user":
            raise ProtocolError(f"expected user, got {reply.get('type')!r}")
        try:
            return UserInfo.from_json(reply.get("user"))
        except ValueError as error:
            raise ProtocolError(str(error)) from error
