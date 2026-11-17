# PopSub Go client

A Go client for PopSub. It does what [`clients/README.md`](../README.md) asks of every PopSub client: it logs in and refreshes its token, reconnects with jittered backoff (and stops for good after close codes that rule reconnecting out), resubscribes each topic from the next `seq` it expects, resends confirmed publishes under the same request id and `message_id`, acknowledges QoS 1 messages, and hands each `(topic, seq)` to the application at most once. It passes the shared scenario suite.

The only third-party dependency is [`github.com/coder/websocket`](https://github.com/coder/websocket). Go 1.23 or later.

## Install

```bash
go get github.com/soumyadepp/popsub/clients/go
```

The package is named `popsub`.

## Usage

```go
ctx := context.Background()
client, err := popsub.Connect(ctx, popsub.Config{
	URL:         "ws://127.0.0.1:8080",
	Credentials: popsub.PasswordCredentials("alice", "secret-password"),
	SessionID:   "alice-laptop",
})
if err != nil {
	return err
}
defer client.Close()

orders, err := client.Subscribe(ctx, "orders", popsub.FromResume)
if err != nil {
	return err
}
stored, err := client.PublishConfirmed(ctx, "orders", "order 42 shipped")
if err != nil {
	return err
}
fmt.Println("stored as seq", stored.Seq)

for message := range orders.Messages() {
	fmt.Printf("%s #%d: %s\n", message.Topic, message.Seq, message.Payload)
}
```

## API

| Call | What it does |
|---|---|
| `Connect(ctx, Config) (*Client, error)` | Starts the client and waits for its first connection, up to `RequestTimeout` or until `ctx` ends. A rejected login fails at once. |
| `client.Subscribe(ctx, topic, from) (*Subscription, error)` | Subscribes and waits for `subscribed`. `from` is `FromResume`, `FromNew`, `FromLast(n)`, `FromSince(t)`, `FromSinceMs(ms)` or `FromSeq(seq)`. |
| `sub.Messages() <-chan Message` | The topic's messages in `seq` order. Closed when the application unsubscribes, the server ends the subscription, the topic is subscribed again, or the client stops. |
| `message.Ack()` | Acknowledges a QoS 1 message in `AckManual`. A no-op otherwise; safe to call twice. |
| `client.Unsubscribe(ctx, topic) error` | Unsubscribes and closes the topic's channel. |
| `client.Publish(topic, payload) error` | QoS 0, no confirmation, does not wait. Up to 10,000 are held while disconnected. |
| `client.PublishConfirmed(ctx, topic, payload) (Published, error)` | QoS 1 with a new UUID `message_id`; returns `Seq` and `MessageID` once stored. Resent after reconnects until confirmed. |
| `client.Admin()` | `CreateUser`, `GetUser`, `ListUsers`, `SetRole`, `Grant`, `Revoke`, `SetPassword`, `SetEnabled`, `DeleteUser`. Never queued while disconnected and never resent: a disconnect fails them with `KindConnect`. |
| `client.Events() <-chan Event` | `ConnectedEvent`, `DisconnectedEvent`, `TruncatedEvent`, `StalledEvent`, `AuthExpiringEvent`, `AuthRefreshedEvent`, `UnsubscribedEvent`, `ServerErrorEvent`. One channel per client, 256 deep; the oldest are dropped if the application falls behind. |
| `client.Close() error` | Closes the connection, fails what is still waiting with `KindClosed`, and returns once every goroutine the client started has exited. |
| `client.Done()`, `client.Err()` | Whether, and why, the client stopped for good. |

### Config

| Field | Default | Meaning |
|---|---|---|
| `URL` | required | `ws://` or `wss://` |
| `Credentials` | required | `PasswordCredentials(user, pass)` (logs in on every connection, refreshes before expiry) or `TokenCredentials(jwt)` (can't refresh) |
| `SessionID` | empty, ephemeral | A durable session to hold |
| `CleanStart` | false | Discard the session's subscriptions, on the first connection only |
| `AckMode` | `AckAuto` | `AckAuto` acknowledges QoS 1 as it is handed over; `AckManual` waits for `Message.Ack` |
| `Reconnect` | 250 ms to 30 s, forever | `Initial`, `Max`, `MaxAttempts` (0 keeps trying; `popsub.NoRetries` turns reconnecting off, so a failed first connection fails `Connect` after one attempt and a later drop stops the client). Each wait is random between zero and a ceiling that doubles from `Initial` to `Max` |
| `RequestTimeout` | 30 s | Limit for connect, subscribe, unsubscribe, confirmed publishes and admin calls, reconnects included |
| `SubscriptionBuffer` | 1024 | Messages buffered per subscription. When one is full the client stops reading the socket, so the server paces delivery |
| `HTTPClient` | `http.DefaultClient` | Used for the WebSocket handshake, for example to trust a private CA |

### Errors

Every call returns a `*popsub.Error` with a `Kind`: `server` (with `Code`, and `RetryAfter` for limits), `closed` (with `CloseCode`, 0 when closed by the application or out of attempts), `timeout`, `connect`, `protocol` or `config`. Read it with `errors.As`, or match the kind with `errors.Is(err, popsub.ErrClosed)` and the other sentinels. Timeouts wrap `context.DeadlineExceeded` or `context.Canceled`, and connection failures wrap the network error. `err.Temporary()` says whether retrying later might work. Server error codes this client doesn't know read as `CodeInternal`.

### Connection health

The client reads the socket continuously, which also answers the server's pings. If nothing arrives (pings included) for `idle_timeout_secs + heartbeat_secs` from the server's `limits`, it drops the connection and reconnects. While a subscription's buffer is full it stops reading on purpose, as the Rust client does; an application that stops draining a subscription for longer than the server's idle timeout gets disconnected and resumes after reconnecting.

## Scenario suite

The driver in `cmd/popsub-driver` speaks [`clients/DRIVER.md`](../DRIVER.md). It maps `max_attempts: null` to `MaxAttempts: 0` (keep trying) and `max_attempts: 0` to `popsub.NoRetries`. Build it from `clients/go`, then run the suite from the repository root (build the suite once with `cargo build -p popsub_scenarios`):

```bash
cd clients/go && go build -o bin/popsub-driver ./cmd/popsub-driver && cd ../..
target/debug/popsub-scenarios --driver "clients/go/bin/popsub-driver"
target/debug/popsub-scenarios --driver "clients/go/bin/popsub-driver" --scenario reconnect_resubscribes_without_gaps
```

`make scenarios` in `clients/go` does both.

## Development

Run every check from `clients/go`:

```bash
make check
```

It runs, and fails on the first problem:

| Step | Command |
|---|---|
| Formatting | `gofmt -l .` must print nothing |
| Vet | `go vet ./...` |
| Static analysis | `go run honnef.co/go/tools/cmd/staticcheck@v0.8.2 ./...` (pinned in the Makefile; not a module dependency) |
| Tests | `go test -race -count=1 ./...` |
| Driver build | `go build -o bin/popsub-driver ./cmd/popsub-driver` |

golangci-lint is not used. The tests run against an in-process fake server; `goleak` fails them if any goroutine outlives a test, and one test checks that `Close` stops everything the client started.
