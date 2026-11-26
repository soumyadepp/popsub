import { Client, PopSubError } from "popsub";

const [topic, text] = process.argv.slice(2);

const client = await Client.connect({
  url: "ws://127.0.0.1:8080",
  credentials: { username: "alice", password: "alice-password" },
});

try {
  const stored = await client.publishConfirmed(topic, text);
  console.log(`stored on ${topic} as #${stored.seq}`);
} catch (error) {
  if (!(error instanceof PopSubError && error.kind === "server")) {
    throw error;
  }
  console.error(`refused: ${error.code}`);
  process.exitCode = 1;
} finally {
  await client.close();
}
