/**
 * A program the lifecycle tests run in a child process: it uses a client
 * in one of several ways, closes it, prints one result line, and then must
 * exit on its own. It never calls `process.exit`, so a timer, socket or
 * listener the client left behind would keep it running.
 *
 * Usage: `node child.js <mode> <url>`, where mode is `session`, `backoff`
 * or `unreachable`.
 *
 * @module
 */

import { Client, PopSubError, type ClientEvent } from "../../src/index.js";

/**
 * Waits for a time.
 *
 * @param ms - How long.
 * @returns A promise that settles after it.
 */
function delay(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/**
 * Connects, subscribes, receives a message, publishes, and closes.
 *
 * @param url - The server's URL.
 * @returns The result line.
 */
async function session(url: string): Promise<string> {
  const client = await Client.connect({ url, credentials: { username: "u", password: "p" } });
  const subscription = await client.subscribe("t", "new");
  const message = await subscription.next();
  const published = await client.publishConfirmed("t", "x");
  await client.close();
  const after = await subscription.next();
  return `message=${String(message?.seq)} published=${String(published.seq)} ended=${String(after === undefined)} stopped=${String(client.stopped !== null)}`;
}

/**
 * Connects, waits until the server drops the connection and refuses new
 * ones, makes a confirmed publish that can't complete, lets the client
 * retry for a while, then closes it mid-backoff.
 *
 * @param url - The server's URL.
 * @returns The result line.
 */
async function backoff(url: string): Promise<string> {
  const events: ClientEvent["type"][] = [];
  const client = await Client.connect(
    { url, credentials: { token: "t" }, reconnect: { initialMs: 20, maxMs: 50, maxAttempts: null } },
    (event) => events.push(event.type),
  );
  process.stdout.write("ready\n");
  while (!events.includes("disconnected")) {
    await delay(10);
  }
  const pending = client.publishConfirmed("t", "held while disconnected");
  await delay(200);
  await client.close();
  const outcome = await pending.then(
    () => "published",
    (error: unknown) => (error instanceof PopSubError ? error.kind : "other"),
  );
  return `events=${events.join(",")} pending=${outcome}`;
}

/**
 * Tries to connect to a port nothing listens on, with a short timeout.
 *
 * @param url - The URL to try.
 * @returns The result line.
 */
async function unreachable(url: string): Promise<string> {
  try {
    await Client.connect({
      url,
      credentials: { token: "t" },
      requestTimeoutMs: 300,
      reconnect: { initialMs: 20, maxMs: 40 },
    });
    return "connected";
  } catch (error) {
    return `error=${error instanceof PopSubError ? error.kind : "other"}`;
  }
}

/** Runs the mode named on the command line and prints its result. */
async function main(): Promise<void> {
  const [mode, url] = process.argv.slice(2);
  if (url === undefined) {
    throw new Error("usage: child.js <mode> <url>");
  }
  const runs: Record<string, (url: string) => Promise<string>> = { session, backoff, unreachable };
  const run = mode === undefined ? undefined : runs[mode];
  if (run === undefined) {
    throw new Error(`unknown mode ${String(mode)}`);
  }
  process.stdout.write(`${await run(url)}\n`);
}

main().catch((error: unknown) => {
  process.stderr.write(`${error instanceof Error ? (error.stack ?? error.message) : String(error)}\n`);
  process.exitCode = 1;
});
