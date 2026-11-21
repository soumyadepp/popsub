/**
 * Publishes one message and waits until the server has stored it.
 *
 *     npm run build && node examples/publish.ts orders/ts 'hello from TypeScript'
 *
 * `POPSUB_URL` defaults to `ws://127.0.0.1:8080`, `POPSUB_USERNAME` to `alice` and
 * `POPSUB_USER_PASSWORD` to `alice-password`. The topic defaults to `orders/ts` and the message
 * to `hello from TypeScript`. Node 22.18 or newer runs the file as it is.
 *
 * @module
 */

import { Client } from "popsub";

const env = process.env;
const topic = process.argv[2] ?? "orders/ts";
const payload = process.argv[3] ?? "hello from TypeScript";

const client = await Client.connect({
  url: env.POPSUB_URL ?? "ws://127.0.0.1:8080",
  credentials: {
    username: env.POPSUB_USERNAME ?? "alice",
    password: env.POPSUB_USER_PASSWORD ?? "alice-password",
  },
});
try {
  const stored = await client.publishConfirmed(topic, payload);
  console.log(`stored on ${topic} as seq ${String(stored.seq)} (${stored.messageId})`);
} finally {
  await client.close();
}
