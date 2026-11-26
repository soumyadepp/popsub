import { Client } from "popsub";

const client = await Client.connect({
  url: "ws://127.0.0.1:8080",
  credentials: { username: "alice", password: "alice-password" },
});

const latest = new Map();
const readings = await client.subscribe("sensors/+/temp", { last: 1 });

for await (const reading of readings) {
  latest.set(reading.topic.split("/")[1], reading.payload);
  const rooms = [...latest.keys()].sort();
  console.log(rooms.map((room) => `${room} ${latest.get(room)}`).join("  "));
}
