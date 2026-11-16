/**
 * One connection attempt up to authentication: opening the WebSocket, then
 * request and reply exchanges (`login`, `auth`) before the connection is
 * handed to the engine. Frames that arrive after the last reply and before
 * the hand-over are kept, so none is lost between the two.
 *
 * @module
 */

import { PopSubError } from "./errors.js";
import {
  closeCode,
  encodeFrame,
  parseServerFrame,
  shouldReconnect,
  type ClientMessage,
  type ServerMessage,
} from "./protocol.js";

/** Why a connection attempt failed, and whether another attempt could help. */
export class ConnectFailure extends Error {
  /** The error to report. */
  readonly error: PopSubError;
  /** Whether retrying could help. */
  readonly retry: boolean;

  /**
   * Builds a failure.
   *
   * @param error - The error to report, which also becomes the cause.
   * @param retry - Whether retrying could help.
   */
  constructor(error: PopSubError, retry: boolean) {
    super(error.message, { cause: error });
    this.name = "ConnectFailure";
    this.error = error;
    this.retry = retry;
  }
}

/** Thrown into a waiting attempt when the application closes the client. */
export class Aborted extends Error {
  /** Builds the marker error. */
  constructor() {
    super("aborted");
    this.name = "Aborted";
  }
}

/** Where a served connection's frames and close go once it is handed over. */
export interface ConnectionHandlers {
  /**
   * Called with each text frame.
   *
   * @param text - The frame's text.
   */
  text(text: string): void;
  /**
   * Called once when the connection closes.
   *
   * @param code - The close event's code.
   */
  closed(code: number): void;
}

/** A pending wait for the socket to open or for one reply. */
interface Waiter {
  /** The request id whose reply is awaited, or `null` while waiting to open. */
  id: number | null;
  /** Settles the wait with a reply. */
  resolve(reply: ServerMessage | null): void;
  /** Fails the wait. */
  reject(reason: ConnectFailure | Aborted): void;
}

/** A WebSocket during its connection attempt. */
export class Handshake {
  /** The socket. */
  readonly socket: WebSocket;
  /** How long each step may take, in milliseconds. */
  readonly #timeoutMs: number;
  /** Whether the socket has opened. */
  #opened = false;
  /** Why the attempt can no longer succeed, once it can't. */
  #failure: ConnectFailure | Aborted | null = null;
  /** The current wait. */
  #waiter: Waiter | null = null;
  /** Whether frames are being kept for the hand-over. */
  #capturing = false;
  /** Frames kept for the hand-over. */
  #early: string[] = [];
  /** Where frames go after the hand-over. */
  #handlers: ConnectionHandlers | null = null;

  /**
   * Opens a WebSocket.
   *
   * @param url - The server's URL.
   * @param timeoutMs - How long each step may take.
   * @throws PopSubError of kind `config` when there is no global
   *   `WebSocket` or the URL is rejected.
   */
  constructor(url: string, timeoutMs: number) {
    if (typeof globalThis.WebSocket !== "function") {
      throw new PopSubError(
        "config",
        "this runtime has no global WebSocket; Node.js 22 or later is required",
      );
    }
    try {
      this.socket = new WebSocket(url);
    } catch (error) {
      const reason = error instanceof Error ? error.message : String(error);
      throw new PopSubError("config", `invalid url: ${reason}`, { cause: error });
    }
    this.#timeoutMs = timeoutMs;
    this.socket.onopen = () => {
      this.#onOpen();
    };
    this.socket.onmessage = (event: MessageEvent) => {
      this.#onMessage(event.data);
    };
    this.socket.onclose = (event: CloseEvent) => {
      this.#onClose(event.code);
    };
    this.socket.onerror = () => undefined;
  }

  /**
   * Waits for the socket to open.
   *
   * @throws ConnectFailure when it closes first or takes too long.
   * @throws Aborted when the application closed the client.
   */
  async opened(): Promise<void> {
    if (!this.#opened) {
      await this.#wait(null);
    }
  }

  /**
   * Sends a request and waits for the reply with its id. Other frames are ignored.
   *
   * @param id - The request id.
   * @param message - The request.
   * @returns The reply.
   * @throws ConnectFailure when the connection closes first or no reply
   *   arrives within the timeout.
   * @throws Aborted when the application closed the client.
   */
  async exchange(id: number, message: ClientMessage): Promise<ServerMessage> {
    this.#capturing = false;
    this.#early = [];
    if (this.#failure === null) {
      this.socket.send(encodeFrame(message, id));
    }
    const reply = await this.#wait(id);
    if (reply === null) {
      throw new ConnectFailure(new PopSubError("protocol", "no reply to the request"), true);
    }
    return reply;
  }

  /**
   * Fails the current wait because the application closed the client, and
   * closes the socket.
   */
  abort(): void {
    this.#fail(new Aborted());
    this.close();
  }

  /** Closes the socket and stops listening to it. */
  close(): void {
    this.#detach();
    try {
      this.socket.close(1000);
    } catch {
      return;
    }
  }

  /**
   * Hands the connection to the engine.
   *
   * @param handlers - Where its frames and close go from now on.
   * @returns The frames that arrived after the last reply, in order.
   */
  handOver(handlers: ConnectionHandlers): string[] {
    this.#handlers = handlers;
    const early = this.#early;
    this.#early = [];
    this.#capturing = false;
    return early;
  }

  /**
   * Starts a wait, failing at once if the attempt has already failed.
   *
   * @param id - The reply's request id, or `null` to wait for the socket to open.
   * @returns The reply, or `null` for the open.
   */
  #wait(id: number | null): Promise<ServerMessage | null> {
    return new Promise((resolve, reject) => {
      if (this.#failure !== null) {
        reject(this.#failure);
        return;
      }
      const timer = setTimeout(() => {
        this.#fail(new ConnectFailure(PopSubError.timeout(), true));
      }, this.#timeoutMs);
      this.#waiter = {
        id,
        resolve: (reply) => {
          clearTimeout(timer);
          this.#waiter = null;
          resolve(reply);
        },
        reject: (reason) => {
          clearTimeout(timer);
          this.#waiter = null;
          reject(reason);
        },
      };
    });
  }

  /**
   * Records why the attempt failed and fails the current wait.
   *
   * @param reason - The failure.
   */
  #fail(reason: ConnectFailure | Aborted): void {
    if (this.#failure === null) {
      this.#failure = reason;
    }
    this.#waiter?.reject(this.#failure);
  }

  /** Settles a wait for the socket to open. */
  #onOpen(): void {
    this.#opened = true;
    if (this.#waiter !== null && this.#waiter.id === null) {
      this.#waiter.resolve(null);
    }
  }

  /**
   * Routes a frame: to the engine after the hand-over, to the kept frames
   * after a reply, or to the waiting exchange if it is its reply.
   *
   * @param data - The frame's data; binary frames are ignored.
   */
  #onMessage(data: unknown): void {
    if (typeof data !== "string") {
      return;
    }
    if (this.#handlers !== null) {
      this.#handlers.text(data);
      return;
    }
    if (this.#capturing) {
      this.#early.push(data);
      return;
    }
    const waiter = this.#waiter;
    if (waiter?.id == null) {
      return;
    }
    const frame = parseServerFrame(data);
    if (frame !== null && frame.id === waiter.id) {
      this.#capturing = true;
      waiter.resolve(frame);
    }
  }

  /**
   * Handles the socket closing: tells the engine after the hand-over,
   * otherwise fails the attempt. Before the socket opened the server
   * couldn't be reached; after, the close code decides whether to retry.
   *
   * @param raw - The close event's code.
   */
  #onClose(raw: number): void {
    if (this.#handlers !== null) {
      const handlers = this.#handlers;
      this.#detach();
      handlers.closed(raw);
      return;
    }
    if (!this.#opened) {
      this.#fail(
        new ConnectFailure(
          new PopSubError("connect", `can't connect to the server (close code ${String(raw)})`),
          true,
        ),
      );
      return;
    }
    const code = closeCode(raw);
    this.#fail(new ConnectFailure(PopSubError.closed(code), code === null || shouldReconnect(code)));
  }

  /** Stops listening to the socket. */
  #detach(): void {
    this.#handlers = null;
    this.socket.onopen = null;
    this.socket.onmessage = null;
    this.socket.onclose = null;
    this.socket.onerror = null;
  }
}
