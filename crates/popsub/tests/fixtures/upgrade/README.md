# Upgrade fixtures

Each folder here is a real data directory written by a past PopSub release. Nothing in it is hand-made.

`tests/upgrade.rs` opens every folder with the current code and checks that nothing was lost. If a change stops PopSub from reading data an older release wrote, that test fails, before any user's server refuses to start after upgrading.

## What's in a folder

Each one is what a server's `storage.path` holds:

| File | What it is |
|---|---|
| `topics/t<hex>/*.seg` | A topic's messages. The folder name is the topic name in hex |
| `topics/t<hex>/head.json` | The topic's oldest kept and newest message numbers |
| `sessions.log` | Durable sessions and how far each has acknowledged |
| `users.json` | Accounts. Every password is `<username>-password` |

The `.seg` files are binary on purpose. 0.1.0 and 0.2.0 stored messages as JSON, and 0.3.0 switched to a compact binary form. Both formats have to keep working, and only the old releases can write them.

## What the data is

Every folder holds the same content, written by `scripts/upgrade-fixture`:

- `orders`: 12 messages (one base64), trimmed to the last 9
- `sensors/1/temp`: 3 messages
- `purged`: 2 messages, then all removed
- Session `alice|laptop`: subscribed to two topics, acknowledged up to 7 on `orders`
- Accounts `admin`, `rita`, `viewer`, and `gone` (disabled)

## Adding one

At each release, before the next one can change the format:

```bash
make upgrade-fixture VERSION=0.4.0
```

Commit the new folder. Never edit or regenerate an existing one: it stands for what that release wrote.
