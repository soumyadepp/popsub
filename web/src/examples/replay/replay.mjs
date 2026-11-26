import { Client } from "popsub";

const minutes = Number(process.argv[2]);

const client = await Client.connect({
  url: "ws://127.0.0.1:8080",
  credentials: { username: "alice", password: "alice-password" },
});

const events = await client.subscribe("audit/#", { sinceMs: Date.now() - minutes * 60_000 });

for await (const event of events) {
  const seconds = Math.floor((Date.now() - event.publishedAtMs) / 1000);
  console.log(`${seconds} s ago  ${event.topic}  ${event.payload}`);
}
