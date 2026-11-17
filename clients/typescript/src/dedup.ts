/**
 * Per-topic delivery bookkeeping: the next `seq` the application expects
 * (or, for a shared subscription, the `seq`s it was handed), which `seq`s
 * it acknowledged recently, and which acknowledgements are held back while
 * its buffer is full. Pure, so it can be unit tested.
 *
 * @module
 */

/** How many acknowledged `seq`s, and for a shared subscription delivered `seq`s, a topic remembers. */
export const ACK_MEMORY = 4096;

/**
 * The `seq`s a topic acknowledged recently, so a duplicate the application
 * already acknowledged is acknowledged again. Bounded: the oldest are
 * forgotten first.
 */
export class AckMemory {
  /** The remembered `seq`s, in the order they were first inserted. */
  readonly #seen = new Set<number>();
  /** How many `seq`s to keep. */
  readonly #capacity: number;

  /**
   * Builds an empty memory.
   *
   * @param capacity - How many `seq`s to keep; `ACK_MEMORY` by default.
   */
  constructor(capacity: number = ACK_MEMORY) {
    this.#capacity = Math.max(1, capacity);
  }

  /**
   * Remembers an acknowledged `seq`, forgetting the oldest beyond the capacity.
   *
   * @param seq - The acknowledged message.
   */
  insert(seq: number): void {
    if (this.#seen.has(seq)) {
      return;
    }
    this.#seen.add(seq);
    if (this.#seen.size > this.#capacity) {
      const oldest = this.#seen.values().next();
      if (!oldest.done) {
        this.#seen.delete(oldest.value);
      }
    }
  }

  /**
   * Whether a `seq` is remembered as acknowledged.
   *
   * @param seq - The message.
   * @returns True if it was acknowledged and not yet forgotten.
   */
  has(seq: number): boolean {
    return this.#seen.has(seq);
  }

  /**
   * How many `seq`s are remembered.
   *
   * @returns The count.
   */
  get size(): number {
    return this.#seen.size;
  }
}

/**
 * What a topic knows about its deliveries. A message whose `seq` is below
 * `nextExpected` is a duplicate and never reaches the application.
 *
 * A shared subscription gets only some of a topic's messages, out of
 * order, so its tracker never uses `nextExpected`: a duplicate is a `seq`
 * found in a bounded set of the `seq`s already handed over.
 */
export class DeliveryTracker {
  /** Whether this tracks a shared subscription. */
  readonly shared: boolean;
  /** The next `seq` the application should see, or `null` before the first and always for a shared subscription. */
  nextExpected: number | null = null;
  /** The `seq`s handed to the application, for a shared subscription; the oldest are forgotten first. */
  readonly seen: AckMemory;
  /** The `seq`s acknowledged recently. */
  readonly acked: AckMemory;
  /**
   * QoS 1 `seq`s handed to the buffer in automatic mode whose
   * acknowledgement waits until the application takes them, because the
   * buffer was full when they arrived.
   */
  readonly deferred = new Set<number>();

  /**
   * Builds a tracker with nothing seen yet.
   *
   * @param ackMemory - How many acknowledged `seq`s, and delivered ones
   *   for a shared subscription, to remember.
   * @param shared - Whether this tracks a shared subscription.
   */
  constructor(ackMemory: number = ACK_MEMORY, shared = false) {
    this.acked = new AckMemory(ackMemory);
    this.seen = new AckMemory(ackMemory);
    this.shared = shared;
  }

  /**
   * Moves the next expected `seq` forward, never back. Used for
   * `subscribed.replay_from` and `truncated.first_available`. Does nothing
   * for a shared subscription, whose position is the group's, on the server.
   *
   * @param start - The `seq` delivery continues from.
   */
  moveTo(start: number): void {
    if (this.shared) {
      return;
    }
    this.nextExpected = this.nextExpected === null ? start : Math.max(this.nextExpected, start);
  }

  /**
   * Whether a delivered `seq` was already handed to the application.
   *
   * @param seq - The delivered message.
   * @returns True when it is below the next expected `seq`, or, for a
   *   shared subscription, among the remembered delivered `seq`s.
   */
  isDuplicate(seq: number): boolean {
    if (this.shared) {
      return this.seen.has(seq);
    }
    return this.nextExpected !== null && seq < this.nextExpected;
  }

  /**
   * Records that a message is handed to the application.
   *
   * @param seq - The message.
   */
  accept(seq: number): void {
    if (this.shared) {
      this.seen.insert(seq);
      return;
    }
    this.nextExpected = seq + 1;
  }

  /**
   * Whether a QoS 1 duplicate should be acknowledged again, so a lost
   * acknowledgement can't stall the subscription.
   *
   * @param seq - The duplicate.
   * @param automatic - Whether the client acknowledges on its own.
   * @returns True when it was acknowledged before, or in automatic mode
   *   unless its acknowledgement is being held back for a full buffer.
   */
  reacksDuplicate(seq: number, automatic: boolean): boolean {
    return this.acked.has(seq) || (automatic && !this.deferred.has(seq));
  }
}
