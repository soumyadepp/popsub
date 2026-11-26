import { Client } from "popsub";

const client = await Client.connect({
  url: "ws://127.0.0.1:8080",
  credentials: { username: "alice", password: "alice-password" },
});

for (let n = 1; n <= 6; n++) {
  const payload = `job-${n}`;
  await client.publishConfirmed("jobs/resize", payload);
  console.log(`queued ${payload}`);
}

await client.close();
