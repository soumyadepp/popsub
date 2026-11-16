/**
 * Reconnect delays: a ceiling that doubles from `initialMs` up to `maxMs`,
 * and a wait drawn at random between zero and that ceiling, so many clients
 * reconnecting after a server restart don't all arrive at once.
 *
 * @module
 */

/** The part of a reconnect policy that sets the delays. */
export interface BackoffPolicy {
  /** The ceiling before the first retry is `initialMs` doubled once. */
  initialMs: number;
  /** The highest ceiling. */
  maxMs: number;
}

/**
 * The longest wait before an attempt, before jitter.
 *
 * @param policy - The delays.
 * @param attempt - How many attempts in a row have failed, from 0.
 * @returns `initialMs` doubled `attempt` times (at most 30 doublings),
 *   capped at `maxMs`.
 */
export function backoffCeiling(policy: BackoffPolicy, attempt: number): number {
  const doublings = Math.min(Math.max(0, Math.floor(attempt)), 30);
  return Math.min(policy.initialMs * 2 ** doublings, policy.maxMs);
}

/**
 * The wait before an attempt: a random time between zero and the ceiling.
 *
 * @param policy - The delays.
 * @param attempt - How many attempts in a row have failed, from 0.
 * @param random - A source of numbers in `[0, 1)`; `Math.random` by default.
 * @returns The wait in milliseconds.
 */
export function jitteredDelay(
  policy: BackoffPolicy,
  attempt: number,
  random: () => number = Math.random,
): number {
  return backoffCeiling(policy, attempt) * random();
}
