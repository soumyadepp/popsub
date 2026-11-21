# Examples

Each client library has the same two small programs beside it:

| Language | Subscriber | Publisher |
|---|---|---|
| Rust | [`crates/popsub_sdk/examples/subscribe.rs`](../crates/popsub_sdk/examples/subscribe.rs) | [`publish.rs`](../crates/popsub_sdk/examples/publish.rs) |
| TypeScript | [`clients/typescript/examples/subscribe.ts`](../clients/typescript/examples/subscribe.ts) | [`publish.ts`](../clients/typescript/examples/publish.ts) |
| Python | [`clients/python/examples/subscribe.py`](../clients/python/examples/subscribe.py) | [`publish.py`](../clients/python/examples/publish.py) |
| Go | [`clients/go/examples/subscribe`](../clients/go/examples/subscribe/main.go) | [`publish`](../clients/go/examples/publish/main.go) |

The subscriber holds a durable session and prints every message on a topic pattern (`orders/#` unless you pass another). The publisher sends one message with confirmation and prints the `seq` it was stored as. They speak the same protocol, so any subscriber hears any publisher.

Each one reads `POPSUB_URL` (default `ws://127.0.0.1:8080`), `POPSUB_USERNAME` (default `alice`) and `POPSUB_USER_PASSWORD` (default `alice-password`), which match the walkthrough below.

## Walkthrough

You need a server, an account, and two terminals.

1. **Start a server.** From a release download, `./popsub server`; from this repository, `make server`. It listens on `127.0.0.1:8080` with the default admin `admin` / `password`, which only works on localhost. If port 8080 is taken, start it with `SERVER_PORT=8090` and set `POPSUB_URL=ws://127.0.0.1:8090` for everything below.

2. **Create the account the examples use.** In another terminal (`cargo run -p popsub --` in place of `popsub` from a checkout):

   ```bash
   POPSUB_PASSWORD=password POPSUB_NEW_PASSWORD=alice-password popsub users add alice --topic 'orders/#'
   ```

3. **Start a subscriber**, in any language. Run these from the repository root; the TypeScript one needs Node 22.18 or newer, which runs `.ts` files as they are:

   ```bash
   cargo run -p popsub_sdk --example subscribe
   cd clients/python && pip install -e . && python examples/subscribe.py
   cd clients/go && go run ./examples/subscribe
   cd clients/typescript && npm install && npm run build && node examples/subscribe.ts
   ```

4. **Publish from the others**:

   ```bash
   cargo run -p popsub_sdk --example publish -- orders/rust 'hello from Rust'
   cd clients/python && python examples/publish.py orders/python 'hello from Python'
   cd clients/go && go run ./examples/publish orders/go 'hello from Go'
   cd clients/typescript && node examples/publish.ts orders/ts 'hello from TypeScript'
   ```

   The subscriber prints each one under its own topic, because it subscribed to the pattern.

5. **See the session keep its place.** Stop the subscriber with Ctrl+C, publish a few more messages, and start a subscriber again, in the same language or another. It prints the messages it missed, each once, then carries on with new ones. Every example uses the session id `examples-subscriber`, so only one subscriber holds it at a time: starting a second one takes the session over and the first stops.

Things to try next: publish to `news/today`, which alice has no grant for, and see `not_authorized`; restart the server between publishes and watch the clients reconnect on their own.

Outside this repository, install the published package (`cargo add popsub_sdk`, `npm install popsub`, `pip install popsub`, `go get github.com/soumyadepp/popsub/clients/go`) and copy an example into your project; each one only imports the library.
