import { Client } from "popsub";

const count = Number(process.argv[2]);

const client = await Client.connect({
  url: "ws://127.0.0.1:8080",
  credentials: { username: "alice", password: "alice-password" },
});

for (let k = 1; k <= count; k++) {
  const topic = `invoices/${k}`;
  const stored = await client.publishConfirmed(topic, `invoice ${k}`);
  console.log(`queued ${topic} as #${stored.seq}`);
}

await client.close();
