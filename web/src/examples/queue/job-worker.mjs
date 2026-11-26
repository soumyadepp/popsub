import { setTimeout } from "node:timers/promises";
import { Client } from "popsub";

const name = process.argv[2];

const client = await Client.connect({
  url: "ws://127.0.0.1:8080",
  credentials: { username: "alice", password: "alice-password" },
  ackMode: "manual",
});

const jobs = await client.subscribeShared("jobs/resize", "resizers", "new");

for await (const job of jobs) {
  console.log(`${name} took ${job.payload}`);
  await setTimeout(1000);
  console.log(`${name} finished ${job.payload}`);
  job.ack();
}
