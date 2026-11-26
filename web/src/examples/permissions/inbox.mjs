import { Client, PopSubError } from "popsub";

const username = process.argv[2];

const client = await Client.connect({
  url: "ws://127.0.0.1:8080",
  credentials: { username, password: `${username}-password` },
});

try {
  await client.publishConfirmed(`inbox/${username}/notes`, "hello");
} catch (error) {
  if (!(error instanceof PopSubError && error.kind === "server")) {
    throw error;
  }
  console.log(`publish refused: ${error.code}`);
}

const messages = await client.subscribe(`inbox/${username}/#`, { seq: 1 });

for await (const message of messages) {
  console.log(`${message.topic}: ${message.payload}`);
}
