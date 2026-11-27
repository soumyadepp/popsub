# Security policy

## Reporting a vulnerability

Please don't open a public issue, discussion or pull request for a vulnerability.

Report it privately instead:

1. Open the repository's **Security** tab.
2. Choose **Report a vulnerability**.
3. Describe what you found and how to reproduce it.

Only the maintainers can see the report. The fix is discussed and prepared in a private fork, then published as a GitHub security advisory.

## What to include

- The PopSub version, or the commit, you tested.
- What an attacker can do, and what they need first (a network path to the server, an account, a particular setting).
- The smallest set of frames, requests or settings that shows it.
- Whether you plan to disclose it yourself, and when.

## What happens next

| Step | When |
|---|---|
| We confirm we received the report | Within 7 days |
| We tell you whether we can reproduce it, and how serious we think it is | Within 14 days |
| A fixed release is published | As soon as a fix is ready, usually within 90 days of the report |

We'll keep you updated while we work on it. You'll be credited in the advisory and in `CHANGELOG.md` unless you'd rather not be.

Please wait for the fixed release before disclosing the issue publicly. If 90 days pass with no fix, you're free to disclose it.

## Supported versions

PopSub is at version 0.x. Only the latest minor release gets security fixes.

| Version | Fixed |
|---|---|
| Latest 0.x release | Yes |
| Older releases | No. Upgrade to the latest release |

## In scope

- The server (`popsub`), its WebSocket and health listeners, and its storage.
- The protocol crate and the client libraries for Rust, TypeScript, Python and Go.
- The release artifacts and the container image.

## Out of scope

- The published default JWT secret and admin password. The server refuses them on any address other than loopback.
- Load beyond the limits you configured, such as more connections than `broker.max_connections` allows. Report it if a limit doesn't hold.
- Anything that needs an admin account. Admins can already read and change everything.

## How PopSub is tested for security

- The parsers that read raw network input, client frames and PROXY protocol headers, are fuzzed in CI. See [Fuzzing](CONTRIBUTING.md#fuzzing).
- A test fails if a session's logs contain a username, password, session id or client address while masking is on.
- [`docs/DESIGN.md`](docs/DESIGN.md) section 10 lists the rate limits and lockouts.
