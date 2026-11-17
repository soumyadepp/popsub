/**
 * Tests for delivery bookkeeping: duplicates by `seq`, moving the next
 * expected `seq` forward only, re-acknowledging duplicates, and the
 * bounded acknowledgement memory.
 *
 * @module
 */

import assert from "node:assert/strict";
import { test } from "node:test";

import { AckMemory, DeliveryTracker } from "../src/dedup.js";

test("nothing is a duplicate before the first delivery", () => {
  const tracker = new DeliveryTracker();
  assert.equal(tracker.isDuplicate(1), false);
  assert.equal(tracker.isDuplicate(500), false);
});

test("a seq below the next expected one is a duplicate", () => {
  const tracker = new DeliveryTracker();
  tracker.accept(1);
  tracker.accept(2);
  assert.equal(tracker.nextExpected, 3);
  assert.equal(tracker.isDuplicate(1), true);
  assert.equal(tracker.isDuplicate(2), true);
  assert.equal(tracker.isDuplicate(3), false);
});

test("replay_from and first_available only move the next expected seq forward", () => {
  const tracker = new DeliveryTracker();
  tracker.moveTo(5);
  assert.equal(tracker.nextExpected, 5);
  tracker.moveTo(3);
  assert.equal(tracker.nextExpected, 5);
  tracker.moveTo(9);
  assert.equal(tracker.isDuplicate(8), true);
});

test("a duplicate acknowledged before is acknowledged again in manual mode", () => {
  const tracker = new DeliveryTracker();
  tracker.accept(1);
  tracker.accept(2);
  tracker.acked.insert(1);
  assert.equal(tracker.reacksDuplicate(1, false), true);
  assert.equal(tracker.reacksDuplicate(2, false), false);
});

test("every duplicate is acknowledged again in automatic mode unless its ack is held back", () => {
  const tracker = new DeliveryTracker();
  tracker.accept(1);
  tracker.accept(2);
  tracker.deferred.add(2);
  assert.equal(tracker.reacksDuplicate(1, true), true);
  assert.equal(tracker.reacksDuplicate(2, true), false);
});

test("the acknowledgement memory forgets the oldest seq beyond its capacity", () => {
  const memory = new AckMemory(3);
  for (const seq of [1, 2, 3, 2, 4]) {
    memory.insert(seq);
  }
  assert.equal(memory.size, 3);
  assert.equal(memory.has(1), false);
  assert.deepEqual(
    [2, 3, 4].map((seq) => memory.has(seq)),
    [true, true, true],
  );
});
