/**
 * Tests for the topic pattern matcher used by wildcard subscriptions: `+`
 * is exactly one level and a trailing `#` is the rest.
 *
 * @module
 */

import assert from "node:assert/strict";
import { test } from "node:test";

import { isPattern, matchesPattern } from "../src/pattern.js";

test("a plain name matches only itself", () => {
  assert.equal(matchesPattern("orders", "orders"), true);
  assert.equal(matchesPattern("orders", "orders/eu"), false);
  assert.equal(matchesPattern("orders/eu", "orders"), false);
  assert.equal(matchesPattern("orders", "order"), false);
});

test("+ stands for exactly one whole level", () => {
  assert.equal(matchesPattern("sensors/+/temp", "sensors/attic/temp"), true);
  assert.equal(matchesPattern("sensors/+/temp", "sensors/attic/humidity"), false);
  assert.equal(matchesPattern("sensors/+/temp", "sensors/temp"), false);
  assert.equal(matchesPattern("sensors/+/temp", "sensors/a/b/temp"), false);
  assert.equal(matchesPattern("+", "orders"), true);
  assert.equal(matchesPattern("+", "orders/eu"), false);
  assert.equal(matchesPattern("orders/+", "orders"), false);
  assert.equal(matchesPattern("+/+", "orders/eu"), true);
});

test("a trailing # stands for the rest, including nothing", () => {
  assert.equal(matchesPattern("#", "anything/at/all"), true);
  assert.equal(matchesPattern("chat/#", "chat/room1/messages"), true);
  assert.equal(matchesPattern("chat/#", "chat/room1"), true);
  assert.equal(matchesPattern("chat/#", "chat"), true);
  assert.equal(matchesPattern("chat/#", "chatter/room1"), false);
  assert.equal(matchesPattern("chat/+/#", "chat/room1/a/b"), true);
  assert.equal(matchesPattern("chat/+/#", "chat"), false);
});

test("a string is a pattern when it has a wildcard", () => {
  assert.equal(isPattern("orders/#"), true);
  assert.equal(isPattern("sensors/+/temp"), true);
  assert.equal(isPattern("orders/eu"), false);
});
