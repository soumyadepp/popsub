import { Client, PopSubError } from "popsub";

const username = process.argv[2];
const inbox = `inbox/${username}/#`;

const client = await Client.connect({
  url: "ws://127.0.0.1:8080",
  credentials: { username: "admin", password: "password" },
});

try {
  await client.admin().createUser(username, `${username}-password`, {
    kind: "user",
    topics: [{ pattern: inbox, access: "subscribe" }],
  });
  console.log(`created ${username}, may read ${inbox}`);
} catch (error) {
  if (!(error instanceof PopSubError && error.code === "user_exists")) {
    throw error;
  }
  console.log(`${username} already exists`);
} finally {
  await client.close();
}
