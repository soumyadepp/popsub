import { Client } from "popsub";

const client = await Client.connect({
  url: "ws://127.0.0.1:8080",
  credentials: { username: "alice", password: "alice-password" },
});

const messages = await client.subscribe("greetings");
console.log("waiting for messages on greetings");

for await (const message of messages) {
  console.log(`${message.topic} #${message.seq}: ${message.payload}`);
}
