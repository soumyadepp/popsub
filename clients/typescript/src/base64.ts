/**
 * Standard padded base64 (RFC 4648 section 4), which is how a payload
 * published as bytes travels. Written out here because `atob` accepts
 * input the server would refuse (whitespace, missing padding) and
 * `Buffer` doesn't exist in browsers.
 *
 * @module
 */

/** The base64 alphabet, indexed by six-bit value. */
const ALPHABET = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/** The six-bit value of each alphabet character, by character code; -1 elsewhere. */
const VALUES: Int8Array = buildValues();

/**
 * Builds the table that maps a character code to its six-bit value.
 *
 * @returns The table, with -1 for every code outside the alphabet.
 */
function buildValues(): Int8Array {
  const values = new Int8Array(128).fill(-1);
  for (let i = 0; i < ALPHABET.length; i += 1) {
    values[ALPHABET.charCodeAt(i)] = i;
  }
  return values;
}

/**
 * Reads the six-bit value of one character of a base64 string.
 *
 * @param text - The base64 string.
 * @param index - The character's position.
 * @returns The value, or -1 when the character isn't in the alphabet.
 */
function valueAt(text: string, index: number): number {
  const code = text.charCodeAt(index);
  return code < VALUES.length ? (VALUES[code] ?? -1) : -1;
}

/**
 * Encodes bytes as standard padded base64.
 *
 * @param bytes - The bytes.
 * @returns The base64 text, a multiple of four characters long.
 */
export function encodeBase64(bytes: Uint8Array): string {
  const parts: string[] = [];
  let chunk = "";
  for (let i = 0; i < bytes.length; i += 3) {
    const first = bytes[i] ?? 0;
    const second = bytes[i + 1] ?? 0;
    const third = bytes[i + 2] ?? 0;
    const left = bytes.length - i;
    chunk += ALPHABET.charAt(first >> 2);
    chunk += ALPHABET.charAt(((first & 0x03) << 4) | (second >> 4));
    chunk += left > 1 ? ALPHABET.charAt(((second & 0x0f) << 2) | (third >> 6)) : "=";
    chunk += left > 2 ? ALPHABET.charAt(third & 0x3f) : "=";
    if (chunk.length >= 8192) {
      parts.push(chunk);
      chunk = "";
    }
  }
  parts.push(chunk);
  return parts.join("");
}

/**
 * Decodes standard padded base64, accepting only what an encoder writes:
 * a length that is a multiple of four, characters from the alphabet,
 * padding only at the end, and zero in the bits the padding leaves unused.
 *
 * @param text - The base64 text.
 * @returns The bytes, or `null` when the text isn't valid base64.
 */
export function decodeBase64(text: string): Uint8Array | null {
  if (text.length % 4 !== 0) {
    return null;
  }
  let padding = 0;
  if (text.endsWith("==")) {
    padding = 2;
  } else if (text.endsWith("=")) {
    padding = 1;
  }
  const bytes = new Uint8Array((text.length / 4) * 3 - padding);
  let written = 0;
  for (let i = 0; i < text.length; i += 4) {
    const last = i + 4 === text.length;
    const a = valueAt(text, i);
    const b = valueAt(text, i + 1);
    const c = last && padding === 2 ? 0 : valueAt(text, i + 2);
    const d = last && padding >= 1 ? 0 : valueAt(text, i + 3);
    if (a < 0 || b < 0 || c < 0 || d < 0) {
      return null;
    }
    if (last && ((padding === 2 && (b & 0x0f) !== 0) || (padding === 1 && (c & 0x03) !== 0))) {
      return null;
    }
    const group = (a << 18) | (b << 12) | (c << 6) | d;
    const count = last ? 3 - padding : 3;
    for (let k = 0; k < count; k += 1) {
      bytes[written] = (group >> (16 - 8 * k)) & 0xff;
      written += 1;
    }
  }
  return bytes;
}
