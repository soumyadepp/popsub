import { setTimeout } from "node:timers/promises";
import { Client } from "popsub";

const room = process.argv[2];

const client = await Client.connect({
  url: "ws://127.0.0.1:8080",
  credentials: { username: "alice", password: "alice-password" },
});

for (let temp = 20; ; temp += 0.1) {
  client.publish(`sensors/${room}/temp`, temp.toFixed(1));
  await setTimeout(1000);
}
