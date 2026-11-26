import { Client } from "popsub";

const client = await Client.connect({
  url: "ws://127.0.0.1:8080",
  credentials: { username: "alice", password: "alice-password" },
  sessionId: "invoice-worker",
  ackMode: "manual",
});

const invoices = await client.subscribe("invoices/#", { seq: 1 });

for await (const invoice of invoices) {
  console.log(`processing ${invoice.topic} #${invoice.seq}: ${invoice.payload}`);
  invoice.ack();
}
