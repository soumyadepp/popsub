/**
 * Subscribes to a topic pattern with a durable session and prints every message.
 *
 * Stop it and run it again: messages published while it was away arrive once it is back, each
 * once, because the session keeps its place.
 *
 *     npm run build && node examples/subscribe.ts 'orders/#'
 *
 * `POPSUB_URL` defaults to `ws://127.0.0.1:8080`, `POPSUB_USERNAME` to `alice` and
 * `POPSUB_USER_PASSWORD` to `alice-password`; the pattern defaults to `orders/#`. Node 22.18 or
 * newer runs the file as it is.
 *
 * @module
 */

import { Client } from "popsub";

const env = process.env;
const pattern = process.argv[2] ?? "orders/#";

const client = await Client.connect({
  url: env.POPSUB_URL ?? "ws://127.0.0.1:8080",
  credentials: {
    username: env.POPSUB_USERNAME ?? "alice",
    password: env.POPSUB_USER_PASSWORD ?? "alice-password",
  },
  sessionId: "examples-subscriber",
});
process.once("SIGINT", () => {
  void client.close();
});

const messages = await client.subscribe(pattern, "resume");
console.log(`subscribed to ${pattern}; waiting for messages (Ctrl+C to stop)`);
for await (const message of messages) {
  console.log(`${message.topic} #${String(message.seq)}: ${message.payload}`);
}
