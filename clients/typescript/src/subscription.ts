/**
 * What the application receives: messages, and subscriptions that yield
 * them in `seq` order. A message carries its payload string as it
 * travelled and its `encoding`; `Message.bytes` returns the body as bytes
 * for either kind.
 *
 * @module
 */

import { decodeBase64 } from "./base64.js";
import { PopSubError } from "./errors.js";
import type { Encoding } from "./protocol.js";

/** The fields of a delivered message. */
export interface MessageFields {
  /** The topic it was published to. */
  topic: string;
  /** Its position in the topic. */
  seq: number;
  /** The publisher's id for it. */
  messageId: string;
  /** The body as it travelled: the text itself, or base64 when `encoding` says so. */
  payload: string;
  /** How `payload` encodes the message's bytes; `null` or left out for text. */
  encoding?: Encoding | null;
  /** 0 or 1. */
  qos: 0 | 1;
  /** When it was published, in milliseconds since the Unix epoch. */
  publishedAtMs: number;
}

/**
 * A message delivered on a subscription. Each `(topic, seq)` reaches the
 * application at most once per client.
 */
export class Message implements Required<MessageFields> {
  /** The topic it was published to. */
  readonly topic: string;
  /** Its position in the topic. */
  readonly seq: number;
  /** The publisher's id for it. */
  readonly messageId: string;
  /**
   * The body as it travelled: the text itself, or base64 when `encoding`
   * says so. `bytes` decodes it either way.
   */
  readonly payload: string;
  /**
   * How `payload` encodes the message's bytes: `null` for text, `"base64"`
   * for a message published as bytes.
   */
  readonly encoding: Encoding | null;
  /** 0 or 1. */
  readonly qos: 0 | 1;
  /** When it was published, in milliseconds since the Unix epoch. */
  readonly publishedAtMs: number;
  /** Sends the acknowledgement, for QoS 1 messages in manual mode; otherwise `null`. */
  readonly #acker: (() => void) | null;

  /**
   * Builds a message. Applications receive messages from a `Subscription`
   * rather than building them.
   *
   * @param fields - The message's fields.
   * @param acker - What `ack` calls, or `null` when there is nothing to acknowledge.
   */
  constructor(fields: MessageFields, acker: (() => void) | null) {
    this.topic = fields.topic;
    this.seq = fields.seq;
    this.messageId = fields.messageId;
    this.payload = fields.payload;
    this.encoding = fields.encoding ?? null;
    this.qos = fields.qos;
    this.publishedAtMs = fields.publishedAtMs;
    this.#acker = acker;
  }

  /**
   * The message's bytes: the payload's UTF-8 bytes for a text message, or
   * the decoded payload for one published as bytes.
   *
   * @returns The bytes the publisher sent, in a new array on every call.
   * @throws PopSubError of kind `protocol` when the payload says it is
   *   base64 and isn't. The server checks this on publish, so it means the
   *   record was stored some other way. The message itself was still
   *   delivered, and `payload` holds the string as it arrived.
   */
  bytes(): Uint8Array {
    if (this.encoding === null) {
      return new TextEncoder().encode(this.payload);
    }
    const decoded = decodeBase64(this.payload);
    if (decoded === null) {
      throw new PopSubError("protocol", "the payload isn't valid base64");
    }
    return decoded;
  }

  /**
   * Acknowledges the message, so the server stops resending it and a
   * durable session's cursor can move past it.
   *
   * Does nothing for QoS 0 messages and in automatic mode, where the client
   * acknowledges on its own. Calling it more than once is harmless. While
   * disconnected the acknowledgement is only remembered, and a redelivery
   * after reconnecting is acknowledged again.
   */
  ack(): void {
    this.#acker?.();
  }
}

/** What a channel tells the client about the application's use of it. */
export interface ChannelHooks {
  /** Called when the application takes a message. */
  taken(message: Message): void;
  /** Called when the application closes the subscription. */
  leave(): void;
}

/**
 * The client's side of a subscription: a queue of messages the application
 * hasn't taken yet, and the application's waiting `next` calls.
 */
export class Channel {
  /** The topic. */
  readonly topic: string;
  /** Queued messages; the live ones start at `#head`. */
  #items: (Message | undefined)[] = [];
  /** The index of the oldest queued message in `#items`. */
  #head = 0;
  /** `next` calls waiting for a message. */
  #waiters: ((message: Message | undefined) => void)[] = [];
  /** Whether nothing more will be pushed. */
  #ended = false;
  /** What to tell the client. */
  readonly #hooks: ChannelHooks;

  /**
   * Builds an empty channel.
   *
   * @param topic - The topic.
   * @param hooks - What to tell the client.
   */
  constructor(topic: string, hooks: ChannelHooks) {
    this.topic = topic;
    this.#hooks = hooks;
  }

  /**
   * How many messages wait for the application.
   *
   * @returns The count.
   */
  get size(): number {
    return this.#items.length - this.#head;
  }

  /**
   * Whether the channel has ended: nothing more will be added.
   *
   * @returns True once ended.
   */
  get ended(): boolean {
    return this.#ended;
  }

  /**
   * Adds a message, handing it straight to a waiting `next` call if there is one.
   *
   * @param message - The message.
   */
  push(message: Message): void {
    if (this.#ended) {
      return;
    }
    const waiter = this.#waiters.shift();
    if (waiter !== undefined) {
      this.#hooks.taken(message);
      waiter(message);
      return;
    }
    this.#items.push(message);
  }

  /**
   * Ends the channel. Messages already queued can still be taken; waiting
   * `next` calls get `undefined`.
   */
  end(): void {
    if (this.#ended) {
      return;
    }
    this.#ended = true;
    for (const waiter of this.#waiters.splice(0)) {
      waiter(undefined);
    }
  }

  /** Ends the channel and drops the messages still queued. */
  discard(): void {
    this.end();
    this.#items = [];
    this.#head = 0;
  }

  /**
   * Takes the next message, waiting for one if none is queued.
   *
   * @returns The message, or `undefined` once the channel has ended and is empty.
   */
  next(): Promise<Message | undefined> {
    const message = this.#items[this.#head];
    if (message !== undefined) {
      this.#items[this.#head] = undefined;
      this.#head += 1;
      if (this.#head > 1024 && this.#head * 2 > this.#items.length) {
        this.#items = this.#items.slice(this.#head);
        this.#head = 0;
      }
      this.#hooks.taken(message);
      return Promise.resolve(message);
    }
    if (this.#ended) {
      return Promise.resolve(undefined);
    }
    return new Promise((resolve) => this.#waiters.push(resolve));
  }

  /** Tells the client the application closed the subscription. */
  leave(): void {
    this.#hooks.leave();
  }
}

/**
 * The messages of one subscribed topic, in `seq` order. Iterate it with
 * `for await`, or call `next`.
 *
 * It ends when the server ends the subscription (see the `unsubscribed`
 * event), when the application unsubscribes, or when the client stops.
 * Breaking out of a `for await` loop over it, or calling `close`,
 * unsubscribes.
 */
export class Subscription implements AsyncIterable<Message> {
  /** The client's side of the subscription. */
  readonly #channel: Channel;

  /**
   * Wraps a channel. Applications get subscriptions from `Client.subscribe`.
   *
   * @param channel - The client's side of the subscription.
   */
  constructor(channel: Channel) {
    this.#channel = channel;
  }

  /**
   * The subscribed topic.
   *
   * @returns The topic name.
   */
  get topic(): string {
    return this.#channel.topic;
  }

  /**
   * How many messages wait to be taken.
   *
   * @returns The count.
   */
  get buffered(): number {
    return this.#channel.size;
  }

  /**
   * Waits for the next message.
   *
   * @returns The message, or `undefined` once the subscription has ended
   *   and every message it received has been taken.
   */
  next(): Promise<Message | undefined> {
    return this.#channel.next();
  }

  /**
   * Unsubscribes without waiting for the server's reply, and drops the
   * messages not yet taken. Does nothing if the topic was subscribed again
   * since this subscription was made.
   */
  close(): void {
    if (!this.#channel.ended) {
      this.#channel.leave();
    }
    this.#channel.discard();
  }

  /**
   * Iterates the messages until the subscription ends.
   *
   * @returns An iterator whose `return` (called by `break`) closes the subscription.
   */
  [Symbol.asyncIterator](): AsyncIterator<Message> {
    return {
      next: async (): Promise<IteratorResult<Message>> => {
        const message = await this.#channel.next();
        return message === undefined ? { value: undefined, done: true } : { value: message, done: false };
      },
      return: (): Promise<IteratorResult<Message>> => {
        this.close();
        return Promise.resolve({ value: undefined, done: true });
      },
    };
  }
}
