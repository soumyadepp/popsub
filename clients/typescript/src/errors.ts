/**
 * The error every failing client call rejects or throws with.
 *
 * @module
 */

import type { CloseCode, ErrorCode } from "./protocol.js";

/**
 * Why a call failed.
 *
 * - `server`: the server refused the request; `code` says why.
 * - `closed`: the client has stopped for good; `closeCode` is the close code
 *   that stopped it, or `null` when the application closed it or it ran out
 *   of reconnect attempts.
 * - `timeout`: the request didn't complete within `requestTimeoutMs`.
 * - `connect`: the server couldn't be reached, or the connection dropped
 *   before an admin request's reply.
 * - `protocol`: the server sent something the client didn't expect.
 * - `config`: the configuration is invalid.
 */
export type ErrorKind = "server" | "closed" | "timeout" | "connect" | "protocol" | "config";

/** The extra fields a `PopSubError` may carry. */
export interface PopSubErrorDetails {
  /** The server's error code, for `server` errors. */
  code?: ErrorCode | null;
  /** Seconds to wait before trying again, for `locked_out` and `rate_limited`. */
  retryAfter?: number | null;
  /** The close code that stopped the client, for `closed` errors. */
  closeCode?: CloseCode | null;
  /** The error or other value that led to this one, if any. */
  cause?: unknown;
}

/** The server error codes that may succeed if the same request is retried later. */
const TEMPORARY_CODES: ReadonlySet<ErrorCode> = new Set<ErrorCode>([
  "rate_limited",
  "locked_out",
  "storage_unavailable",
  "session_unavailable",
  "internal",
]);

/** A failed client call. */
export class PopSubError extends Error {
  /** What kind of failure this is. */
  readonly kind: ErrorKind;
  /** The server's error code, for `server` errors, otherwise `null`. */
  readonly code: ErrorCode | null;
  /** Seconds to wait before retrying, for limits, otherwise `null`. */
  readonly retryAfter: number | null;
  /** The close code that stopped the client, for `closed` errors, otherwise `null`. */
  readonly closeCode: CloseCode | null;

  /**
   * Builds an error.
   *
   * @param kind - What kind of failure it is.
   * @param message - What went wrong, for people.
   * @param details - The code, retry delay or close code, where they apply.
   */
  constructor(kind: ErrorKind, message: string, details: PopSubErrorDetails = {}) {
    super(message, details.cause === undefined ? undefined : { cause: details.cause });
    this.name = "PopSubError";
    this.kind = kind;
    this.code = details.code ?? null;
    this.retryAfter = details.retryAfter ?? null;
    this.closeCode = details.closeCode ?? null;
  }

  /**
   * Builds the error for a refusal from the server.
   *
   * @param code - The error code.
   * @param message - The server's message.
   * @param retryAfter - Seconds to wait, when the server sent one.
   * @returns The error.
   */
  static server(code: ErrorCode, message: string, retryAfter: number | null = null): PopSubError {
    return new PopSubError("server", `the server refused the request (${code}): ${message}`, {
      code,
      retryAfter,
    });
  }

  /**
   * Builds the error for calls on a client that has stopped.
   *
   * @param closeCode - The close code that stopped it, if any.
   * @returns The error.
   */
  static closed(closeCode: CloseCode | null): PopSubError {
    const why = closeCode === null ? "" : ` (close code ${String(closeCode)})`;
    return new PopSubError("closed", `the client is closed${why}`, { closeCode });
  }

  /**
   * Builds the error for a request that took longer than the request timeout.
   *
   * @returns The error.
   */
  static timeout(): PopSubError {
    return new PopSubError("timeout", "the request timed out");
  }

  /**
   * Whether retrying the same call later might succeed.
   *
   * @returns True for timeouts, connection failures and the server's
   *   temporary refusals (`rate_limited`, `locked_out`,
   *   `storage_unavailable`, `session_unavailable`, `internal`).
   */
  isTemporary(): boolean {
    switch (this.kind) {
      case "timeout":
      case "connect":
        return true;
      case "server":
        return this.code !== null && TEMPORARY_CODES.has(this.code);
      default:
        return false;
    }
  }
}
