---
title: Errors and close codes
description: What each PopSub error code means, and which connection closes a client should reconnect after.
sidebar:
  order: 2
---

When a request fails, the server answers with an `error` frame that carries the request's `id`:

```json
{
  "id": 6,
  "type": "error",
  "code": "not_authorized",
  "message": "not authorized to publish to 'news/today'"
}
```

- Match on `code`, never on `message`.
- `locked_out` and `rate_limited` also carry `retry_after` in seconds.
- Later releases may add codes, so treat one you don't recognise as `internal`.

## Errors that only fail the request

The connection stays open after these.

| Code | Meaning |
|---|---|
| `bad_request` | The frame is malformed, or a field is out of range |
| `invalid_topic` | A bad topic name, or a malformed pattern |
| `not_authorized` | The account may not use that topic that way |
| `payload_too_large` | The payload is over `max_payload_bytes` |
| `rate_limited` | Publishing or registering too fast |
| `invalid_credentials` | Login failed |
| `locked_out` | Too many failed logins |
| `registration_failed` | Registration was refused |
| `storage_unavailable` | The server couldn't read or write its log |
| `forbidden` | An admin request from a non-admin |
| `user_not_found` | No such account |
| `user_exists` | That username is taken |
| `topic_not_found` | Nothing was ever published to that topic |
| `backup_exists` | A backup with that name already exists |
| `backup_unsupported` | The server's storage can't be backed up while it runs, such as the `memory` backend |

`invalid_credentials` and `registration_failed` never say whether a username exists.

A confirmed publish that got `storage_unavailable` was not stored, so retry it with the same
`message_id`.

## Errors that close the connection

The server sends the error, then closes with the code shown.

| Code | Close | Why |
|---|---|---|
| `unauthenticated` | 4002 | A request arrived before `auth` |
| `auth_failed` | 4001 | The token is invalid, expired or revoked |
| `auth_failed` | 4009 | The token's account is disabled or deleted |
| `invalid_session_id` | 4001 | The `session_id` is empty, too long or has control characters |
| `unsupported_protocol` | 4006 | The server only speaks protocol 1 |
| `session_unavailable` | 4007 | The session store was down |
| `internal` | 1011 | Something failed inside the server |

Two exceptions stay open:

- A second `auth` on a live connection with another user's token gets `auth_failed` and keeps
  its original token.
- An `internal` answering an admin request fails only that request.

## Close codes

| Code | Reason | Reconnect? |
|---|---|---|
| 1001 | `going_away` | Yes, with backoff. The server is shutting down. |
| 1009 | `message_too_big` | Yes, but don't resend that frame. |
| 1011 | `internal_error` | Yes, with backoff. |
| 1013 | `try_again_later` | Yes, with backoff. The server is at its connection limit. |
| 4001 | `auth_failed` | No. Log in again first. |
| 4002 | `unauthenticated` | Yes, and send `auth` first. |
| 4003 | `session_taken_over` | No. Another connection took the durable session. |
| 4004 | `token_expired` | Yes, after logging in again. |
| 4005 | `idle_timeout` | Yes. Nothing arrived from the client for 90 seconds (the default). |
| 4006 | `unsupported_protocol` | No. |
| 4007 | `session_unavailable` | Yes, with backoff. |
| 4008 | `slow_consumer` | Yes. The client fell too far behind reading. |
| 4009 | `account_disabled` | No. |

The client libraries follow this table for you, with one difference. After 4004 they only
reconnect if they were given a password, since a bare token can't be renewed.
