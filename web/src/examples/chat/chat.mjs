import { createInterface } from "node:readline";
import { Client } from "popsub";

const name = process.argv[2];

const client = await Client.connect({
  url: "ws://127.0.0.1:8080",
  credentials: { username: "alice", password: "alice-password" },
});

const messages = await client.subscribe("chat/lobby", { last: 20 });

(async () => {
  for await (const message of messages) {
    console.log(message.payload);
  }
})();

for await (const line of createInterface({ input: process.stdin })) {
  if (line !== "") {
    client.publish("chat/lobby", `${name}: ${line}`);
  }
}

await client.close();
