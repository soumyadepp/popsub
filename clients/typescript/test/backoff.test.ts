/**
 * Tests for the reconnect delays: the doubling ceiling, its cap, and the
 * jitter range.
 *
 * @module
 */

import assert from "node:assert/strict";
import { test } from "node:test";

import { backoffCeiling, jitteredDelay } from "../src/backoff.js";

/** The policy the tests use. */
const policy = { initialMs: 20, maxMs: 200 };

test("the ceiling doubles from initialMs with each attempt", () => {
  assert.deepEqual(
    [0, 1, 2, 3].map((attempt) => backoffCeiling(policy, attempt)),
    [20, 40, 80, 160],
  );
});

test("the ceiling never passes maxMs", () => {
  assert.equal(backoffCeiling(policy, 4), 200);
  assert.equal(backoffCeiling(policy, 30), 200);
  assert.equal(backoffCeiling(policy, 10_000), 200);
});

test("the ceiling stays finite for very large attempt counts", () => {
  assert.equal(backoffCeiling({ initialMs: 1, maxMs: Number.MAX_SAFE_INTEGER }, 1_000_000), 2 ** 30);
});

test("the jittered delay lies between zero and the ceiling", () => {
  assert.equal(
    jitteredDelay(policy, 2, () => 0),
    0,
  );
  assert.equal(
    jitteredDelay(policy, 2, () => 0.5),
    40,
  );
  for (let i = 0; i < 1000; i += 1) {
    const delay = jitteredDelay(policy, 3);
    assert.ok(delay >= 0 && delay < 160);
  }
});
