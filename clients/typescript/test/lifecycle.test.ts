/**
 * Tests that a closed client leaves nothing running: each case runs a
 * client in a child process against a fake server, closes it, and checks
 * that the process then exits on its own, which it can't while a timer,
 * socket or listener is still active.
 *
 * @module
 */

import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { after, before, test } from "node:test";
import { fileURLToPath } from "node:url";

import { startFakeServer, type FakeServer } from "./support/server.js";

/** The child program. */
const CHILD = fileURLToPath(new URL("./support/child.js", import.meta.url));

/** How long a child may take to finish and exit. */
const EXIT_WITHIN_MS = 5000;

/** The outcome of a child process. */
interface ChildResult {
  /** Its exit code, or `null` if it was killed. */
  code: number | null;
  /** Everything it printed to standard output. */
  stdout: string;
  /** Everything it printed to standard error. */
  stderr: string;
}

/**
 * Runs the child program and waits for it to exit, killing it if it takes
 * longer than `EXIT_WITHIN_MS`.
 *
 * @param mode - What the child does.
 * @param url - The server's URL.
 * @param onLine - Called with each line it prints.
 * @returns How it ended.
 */
function runChild(
  mode: string,
  url: string,
  onLine: (line: string) => void = () => undefined,
): Promise<ChildResult> {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [CHILD, mode, url], { stdio: ["ignore", "pipe", "pipe"] });
    let stdout = "";
    let stderr = "";
    child.stdout.setEncoding("utf8");
    child.stderr.setEncoding("utf8");
    child.stdout.on("data", (chunk: string) => {
      stdout += chunk;
      for (const line of chunk.split("\n")) {
        if (line !== "") {
          onLine(line);
        }
      }
    });
    child.stderr.on("data", (chunk: string) => {
      stderr += chunk;
    });
    const timer = setTimeout(() => child.kill("SIGKILL"), EXIT_WITHIN_MS);
    child.on("error", (error) => {
      clearTimeout(timer);
      reject(error);
    });
    child.on("exit", (code) => {
      clearTimeout(timer);
      resolve({ code, stdout, stderr });
    });
  });
}

/** The server the tests share; `session` and `unreachable` don't change it. */
let server: FakeServer;

before(async () => {
  server = await startFakeServer();
});

after(async () => {
  await server.close();
});

test("a closed client lets the process exit after a full session", async () => {
  const result = await runChild("session", server.url);
  assert.equal(result.code, 0, result.stderr);
  assert.equal(result.stdout.trim(), "message=1 published=2 ended=true stopped=true");
});

test("closing a client during reconnect backoff stops it and lets the process exit", async () => {
  const dropping = await startFakeServer();
  try {
    const result = await runChild("backoff", dropping.url, (line) => {
      if (line === "ready") {
        dropping.dropAndRefuse();
      }
    });
    assert.equal(result.code, 0, result.stderr);
    assert.equal(result.stdout.trim().split("\n").at(-1), "events=connected,disconnected pending=closed");
  } finally {
    await dropping.close();
  }
});

test("a connect that times out leaves nothing running", async () => {
  const closed = await startFakeServer();
  const url = closed.url;
  await closed.close();
  const result = await runChild("unreachable", url);
  assert.equal(result.code, 0, result.stderr);
  assert.equal(result.stdout.trim(), "error=timeout");
});
