import { Client } from "popsub";

const client = await Client.connect({
  url: "ws://127.0.0.1:8080",
  credentials: { username: "alice", password: "alice-password" },
  sessionId: "orders-worker",
});

const messages = await client.subscribe("orders/#", { seq: 1 });
await client.publishConfirmed("orders/42", "order 42 shipped");

for await (const message of messages) {
  console.log(`${message.topic} #${message.seq}: ${message.payload}`);
}
