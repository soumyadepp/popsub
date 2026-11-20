/**
 * A minimal PopSub server for tests, built on `node:http` with just enough
 * of RFC 6455 (unfragmented text and close frames) to talk to the client.
 * It answers `login`, `auth`, `subscribe`, `unsubscribe`, QoS 1 `publish`
 * and `ping` (unless told to stop answering pings), sends one QoS 1
 * message after each subscribe, records every request it receives, and
 * can drop connections and refuse new ones. A test can script it: name
 * the topics `authenticated` lists as resumed, have it answer `held: true`
 * as a server that holds a session's topics does, give frames to send right
 * after `authenticated` (what a durable session kept), replace the replies
 * to chosen requests, and send frames to every open connection. It never
 * sends WebSocket pings, so a client hears nothing from it unless it asks.
 *
 * @module
 */

import { createHash } from "node:crypto";
import { createServer, type IncomingMessage } from "node:http";
import type { AddressInfo } from "node:net";
import type { Duplex } from "node:stream";

/** The GUID RFC 6455 appends to the key for the accept header. */
const WEBSOCKET_GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/** A JSON frame, as the fake server receives and sends it. */
export type Frame = Record<string, unknown>;

/** The limits the fake server announces in `authenticated`, and its script. */
export interface FakeServerOptions {
  /** The announced `heartbeat_secs`; 30 by default. */
  heartbeatSecs?: number;
  /** The announced `idle_timeout_secs`; 90 by default. */
  idleTimeoutSecs?: number;
  /** The topics `authenticated.resumed` lists; none by default. */
  resumed?: string[];
  /** Whether `authenticated` carries `held: true`; left out by default. */
  held?: boolean;
  /** Frames sent right after every `authenticated`; none by default. */
  kept?: Frame[];
  /**
   * Replaces the replies to a request.
   *
   * @param request - The parsed request.
   * @returns The frames to send back, or `undefined` for the usual replies.
   */
  respond?: (request: Frame) => Frame[] | undefined;
}

/** A running fake server. */
export interface FakeServer {
  /** The URL clients connect to. */
  url: string;
  /** How many connections have authenticated so far. */
  authenticated(): number;
  /** The `type` of every request received so far, in order. */
  received(): string[];
  /** Every request received so far, in order. */
  requests(): Frame[];
  /**
   * Sends a frame to every open connection.
   *
   * @param frame - The frame.
   */
  send(frame: Frame): void;
  /** Destroys every open connection without a close frame; new ones are still accepted. */
  drop(): void;
  /**
   * Sets whether `ping` gets a `pong`.
   *
   * @param answer - False to leave pings unanswered.
   */
  answerPings(answer: boolean): void;
  /** Destroys every open connection without a close frame and refuses new ones. */
  dropAndRefuse(): void;
  /** Stops the server. */
  close(): Promise<void>;
}

/**
 * Encodes one unmasked server frame.
 *
 * @param opcode - The frame's opcode: 1 for text, 8 for close.
 * @param payload - The payload.
 * @returns The frame's bytes.
 */
function encode(opcode: number, payload: Buffer): Buffer {
  const length = payload.length;
  if (length < 126) {
    return Buffer.concat([Buffer.from([0x80 | opcode, length]), payload]);
  }
  const header = Buffer.alloc(4);
  header[0] = 0x80 | opcode;
  header[1] = 126;
  header.writeUInt16BE(length, 2);
  return Buffer.concat([header, payload]);
}

/**
 * Decodes the complete masked client frames at the start of a buffer.
 *
 * @param buffer - The bytes received so far.
 * @returns The frames found, and the bytes left over.
 */
function decode(buffer: Buffer): { frames: { opcode: number; payload: Buffer }[]; rest: Buffer } {
  const frames: { opcode: number; payload: Buffer }[] = [];
  let offset = 0;
  for (;;) {
    if (buffer.length - offset < 2) {
      break;
    }
    const first = buffer.readUInt8(offset);
    const second = buffer.readUInt8(offset + 1);
    let length = second & 0x7f;
    let cursor = offset + 2;
    if (length === 126) {
      if (buffer.length < cursor + 2) {
        break;
      }
      length = buffer.readUInt16BE(cursor);
      cursor += 2;
    } else if (length === 127) {
      if (buffer.length < cursor + 8) {
        break;
      }
      length = Number(buffer.readBigUInt64BE(cursor));
      cursor += 8;
    }
    const masked = (second & 0x80) !== 0;
    const mask = masked ? buffer.subarray(cursor, cursor + 4) : Buffer.alloc(4);
    cursor += masked ? 4 : 0;
    if (buffer.length < cursor + length) {
      break;
    }
    const payload = Buffer.from(buffer.subarray(cursor, cursor + length));
    for (let i = 0; i < payload.length; i += 1) {
      payload.writeUInt8(payload.readUInt8(i) ^ mask.readUInt8(i % 4), i);
    }
    frames.push({ opcode: first & 0x0f, payload });
    offset = cursor + length;
  }
  return { frames, rest: buffer.subarray(offset) };
}

/**
 * Builds the reply frames for one client request.
 *
 * @param request - The parsed request.
 * @param limits - The `heartbeat_secs` and `idle_timeout_secs` to announce.
 * @param resumed - The topics to list in `authenticated.resumed`.
 * @param held - Whether `authenticated` carries `held: true`.
 * @returns The frames to send back, in order.
 */
function reply(
  request: Frame,
  limits: { heartbeatSecs: number; idleTimeoutSecs: number },
  resumed: string[],
  held: boolean,
): Frame[] {
  const id = request.id;
  const topic = request.topic;
  switch (request.type) {
    case "login":
      return [{ type: "logged_in", id, token: "test-token" }];
    case "auth":
      return [
        {
          type: "authenticated",
          id,
          protocol: 1,
          resumed,
          ...(held ? { held: true } : {}),
          limits: {
            max_inflight: 100,
            session_expiry_secs: 60,
            heartbeat_secs: limits.heartbeatSecs,
            idle_timeout_secs: limits.idleTimeoutSecs,
            max_payload_bytes: 1_048_576,
            max_topic_bytes: 120,
          },
        },
      ];
    case "subscribe":
      return [
        { type: "subscribed", id, topic, replay_from: 1, replay_to: 0 },
        {
          type: "message",
          topic,
          seq: 1,
          message_id: "m-1",
          payload: "hello",
          qos: 1,
          published_at_ms: Date.now(),
        },
      ];
    case "unsubscribe":
      return [{ type: "unsubscribed", id, topic }];
    case "ping":
      return [{ type: "pong", id }];
    case "publish":
      return request.qos === 1 ? [{ type: "published", id, message_id: request.message_id, seq: 2 }] : [];
    default:
      return [];
  }
}

/**
 * Starts a fake server on a free loopback port.
 *
 * @param options - The limits to announce and the script to follow.
 * @returns The running server.
 */
export async function startFakeServer(options: FakeServerOptions = {}): Promise<FakeServer> {
  const limits = {
    heartbeatSecs: options.heartbeatSecs ?? 30,
    idleTimeoutSecs: options.idleTimeoutSecs ?? 90,
  };
  const sockets = new Set<Duplex>();
  const received: string[] = [];
  const requests: Frame[] = [];
  const resumed = options.resumed ?? [];
  const kept = options.kept ?? [];
  let refusing = false;
  let answeringPings = true;
  let authenticated = 0;
  const server = createServer((_request, response) => {
    response.writeHead(426).end();
  });
  server.on("upgrade", (request: IncomingMessage, socket: Duplex) => {
    const key = request.headers["sec-websocket-key"];
    if (refusing || typeof key !== "string") {
      socket.destroy();
      return;
    }
    sockets.add(socket);
    socket.on("close", () => sockets.delete(socket));
    socket.on("error", () => sockets.delete(socket));
    const accept = createHash("sha1")
      .update(key + WEBSOCKET_GUID)
      .digest("base64");
    socket.write(
      "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n" +
        `Sec-WebSocket-Accept: ${accept}\r\n\r\n`,
    );
    let pending: Buffer = Buffer.alloc(0);
    socket.on("data", (chunk: Buffer) => {
      const { frames, rest } = decode(Buffer.concat([pending, chunk]));
      pending = rest;
      for (const frame of frames) {
        if (frame.opcode === 8) {
          socket.end(encode(8, frame.payload.subarray(0, 2)));
          return;
        }
        if (frame.opcode !== 1) {
          continue;
        }
        const request = JSON.parse(frame.payload.toString("utf8")) as Frame;
        const type = typeof request.type === "string" ? request.type : "";
        received.push(type);
        requests.push(request);
        if (type === "auth") {
          authenticated += 1;
        }
        if (type === "ping" && !answeringPings) {
          continue;
        }
        const answers = options.respond?.(request) ?? reply(request, limits, resumed, options.held === true);
        for (const answer of type === "auth" ? [...answers, ...kept] : answers) {
          socket.write(encode(1, Buffer.from(JSON.stringify(answer))));
        }
      }
    });
  });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const { port } = server.address() as AddressInfo;
  return {
    url: `ws://127.0.0.1:${String(port)}`,
    authenticated: () => authenticated,
    received: () => [...received],
    requests: () => [...requests],
    send: (frame) => {
      for (const socket of sockets) {
        socket.write(encode(1, Buffer.from(JSON.stringify(frame))));
      }
    },
    drop: () => {
      for (const socket of sockets) {
        socket.destroy();
      }
    },
    answerPings: (answer) => {
      answeringPings = answer;
    },
    dropAndRefuse: () => {
      refusing = true;
      for (const socket of sockets) {
        socket.destroy();
      }
    },
    close: () =>
      new Promise<void>((resolve) => {
        refusing = true;
        for (const socket of sockets) {
          socket.destroy();
        }
        server.close(() => {
          resolve();
        });
      }),
  };
}
