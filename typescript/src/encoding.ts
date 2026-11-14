// Text and byte encodings that S3 requests and answers use.
//
// A header value here is a byte string: one character per byte, as the Fetch
// API's `Headers` holds it. Text outside ASCII goes into a header as its UTF-8
// bytes, one character each.

/** Bytes in an `ArrayBuffer` of their own, which Web Crypto and fetch take. */
export type Bytes = Uint8Array<ArrayBuffer>;

const encoder = new TextEncoder();
const strictDecoder = new TextDecoder("utf-8", { fatal: true });
const lossyDecoder = new TextDecoder("utf-8");

export function utf8(text: string): Bytes {
  return encoder.encode(text);
}

/** Text as a byte string of its UTF-8 bytes. */
export function utf8ByteString(text: string): string {
  return byteString(utf8(text));
}

export function byteString(bytes: Uint8Array): string {
  let text = "";
  for (let at = 0; at < bytes.length; at += 0x8000) {
    text += String.fromCharCode(...bytes.subarray(at, at + 0x8000));
  }
  return text;
}

/** The bytes of a byte string. Every character must be at most U+00FF. */
export function byteStringBytes(text: string): Bytes {
  const bytes = new Uint8Array(text.length);
  for (let at = 0; at < text.length; at++) {
    bytes[at] = text.charCodeAt(at);
  }
  return bytes;
}

/** The text of a byte string of UTF-8 bytes. */
export function utf8Text(value: string): string {
  return lossyDecoder.decode(byteStringBytes(value));
}

/**
 * How the runtime writes a header string onto the wire. The Fetch standard
 * takes one character per byte, and refuses a character past U+00FF, as Bun,
 * Deno, Node and browsers do. workerd takes any text and writes its UTF-8.
 */
export type HeaderStrings = "bytes" | "utf-8";

export const headerStrings: HeaderStrings = (() => {
  try {
    new Headers([["x", "\u0100"]]);
    return "utf-8";
  } catch {
    return "bytes";
  }
})();

/** A header value, a byte string, as the runtime's `Headers` takes it. */
export function runtimeHeaderValue(value: string): string {
  return headerStrings === "utf-8" ? utf8Text(value) : value;
}

/**
 * The text of a header value that a response carries: its bytes read as
 * UTF-8, or as ISO-8859-1 if they are not UTF-8.
 */
export function headerText(value: string): string {
  for (let at = 0; at < value.length; at++) {
    if (value.charCodeAt(at) > 0xff) {
      // The runtime decoded the bytes already.
      return value;
    }
  }
  try {
    return strictDecoder.decode(byteStringBytes(value));
  } catch {
    return value;
  }
}

export function hex(bytes: Uint8Array): string {
  let text = "";
  for (const byte of bytes) {
    text += byte.toString(16).padStart(2, "0");
  }
  return text;
}

export function base64(bytes: Uint8Array): string {
  return btoa(byteString(bytes));
}

/** Decodes standard base64, or returns `undefined` for text that is not. */
export function decodeBase64(text: string): Bytes | undefined {
  try {
    return byteStringBytes(atob(text));
  } catch {
    return undefined;
  }
}

function isUnreserved(byte: number): boolean {
  return (
    (byte >= 0x41 && byte <= 0x5a) || // A-Z
    (byte >= 0x61 && byte <= 0x7a) || // a-z
    (byte >= 0x30 && byte <= 0x39) || // 0-9
    byte === 0x2d || // -
    byte === 0x2e || // .
    byte === 0x5f || // _
    byte === 0x7e // ~
  );
}

function percentEncodeBytes(bytes: Uint8Array, keepSlash: boolean): string {
  let out = "";
  for (const byte of bytes) {
    if (isUnreserved(byte) || (keepSlash && byte === 0x2f)) {
      out += String.fromCharCode(byte);
    } else {
      out += "%" + byte.toString(16).toUpperCase().padStart(2, "0");
    }
  }
  return out;
}

/**
 * Percent-encodes text as SigV4 does: every byte of its UTF-8 but the
 * unreserved characters, in uppercase hex. `keepSlash` keeps `/` as it is,
 * as in a path.
 */
export function uriEncode(text: string, keepSlash = false): string {
  return percentEncodeBytes(utf8(text), keepSlash);
}

function hexValue(code: number): number {
  if (code >= 0x30 && code <= 0x39) return code - 0x30;
  if (code >= 0x41 && code <= 0x46) return code - 0x37;
  if (code >= 0x61 && code <= 0x66) return code - 0x57;
  return -1;
}

/**
 * The bytes that percent-encoded text names. A `%` that begins no escape is
 * text, as in URL percent-decoding. `plusIsSpace` reads `+` as a space, as a
 * form does.
 */
function percentDecodeBytes(text: string, plusIsSpace: boolean): Uint8Array {
  const raw = utf8(text);
  const out = new Uint8Array(raw.length);
  let length = 0;
  for (let at = 0; at < raw.length; at++) {
    const byte = raw[at]!;
    if (byte === 0x25) {
      const high = hexValue(raw[at + 1] ?? -1);
      const low = hexValue(raw[at + 2] ?? -1);
      if (high >= 0 && low >= 0) {
        out[length++] = high * 16 + low;
        at += 2;
        continue;
      }
    }
    out[length++] = plusIsSpace && byte === 0x2b ? 0x20 : byte;
  }
  return out.subarray(0, length);
}

/** Decodes a name that S3 lists with `encoding-type=url`. */
export function formDecode(text: string): string {
  return lossyDecoder.decode(percentDecodeBytes(text, true));
}

/**
 * Writes an already encoded path or query part again as SigV4 encodes it:
 * decodes its escapes and encodes its bytes.
 */
export function reencode(text: string, keepSlash: boolean): string {
  return percentEncodeBytes(percentDecodeBytes(text, false), keepSlash);
}

// RFC 2047 encoded words, which carry text that one header value cannot hold.
// S3 decodes one in a metadata value when it stores the value, and returns a
// stored value outside ASCII as one.

/**
 * Whether a space-separated token of `text` starts with `=?` and ends with
 * `?=`. S3 reads each such token as an encoded word, and decodes it or
 * refuses the write.
 */
function looksEncoded(text: string): boolean {
  return text.split(" ").some((token) => token.startsWith("=?") && token.endsWith("?="));
}

/** Why a metadata value cannot be stored so that it reads back as written. */
export type MetadataValueProblem = "line_break";

/**
 * The header value that stores `value`: the value itself where S3 stores it
 * unchanged, or one encoded word of its UTF-8.
 *
 * A value needs a word if it holds a byte outside printable ASCII other
 * than a tab, starts or ends with a space or a tab, which HTTP drops, or
 * holds a token that S3 would decode as a word. S3 stores CR and LF as
 * spaces even inside a word, so a value with one is refused.
 */
export function metadataHeaderValue(value: string): string | MetadataValueProblem {
  if (/[\r\n]/.test(value)) {
    return "line_break";
  }
  const raw =
    /^[\x20-\x7e\t]*$/.test(value) && !/^[ \t]|[ \t]$/.test(value) && !looksEncoded(value);
  return raw ? value : `=?UTF-8?B?${base64(utf8(value))}?=`;
}

function decodeWord(word: string): Uint8Array | undefined {
  const match = /^=\?([^?]*)\?([^?]*)\?([^?]*)\?=$/.exec(word);
  if (!match || match[1]!.toLowerCase() !== "utf-8") {
    return undefined;
  }
  const text = match[3]!;
  switch (match[2]) {
    case "B":
    case "b":
      return decodeBase64(text);
    case "Q":
    case "q": {
      const bytes: number[] = [];
      for (let at = 0; at < text.length; at++) {
        const char = text[at]!;
        if (char === "_") {
          bytes.push(0x20);
        } else if (char === "=") {
          const high = hexValue(text.charCodeAt(at + 1));
          const low = hexValue(text.charCodeAt(at + 2));
          if (high < 0 || low < 0) return undefined;
          bytes.push(high * 16 + low);
          at += 2;
        } else {
          bytes.push(text.charCodeAt(at));
        }
      }
      return new Uint8Array(bytes);
    }
    default:
      return undefined;
  }
}

/**
 * The text of a metadata value that a response carries. A value of one or
 * more UTF-8 encoded words separated by whitespace is decoded, and its words
 * joined without it. Any other value, including a word that does not decode,
 * is returned as its header text.
 */
export function metadataText(value: string): string {
  const words = value.split(/[ \t]+/).filter((word) => word !== "");
  if (words.length > 0) {
    const decoded = words.map((word) => decodeWord(word));
    if (decoded.every((bytes) => bytes !== undefined)) {
      const joined = new Uint8Array(decoded.reduce((sum, bytes) => sum + bytes!.length, 0));
      let at = 0;
      for (const bytes of decoded) {
        joined.set(bytes!, at);
        at += bytes!.length;
      }
      try {
        return strictDecoder.decode(joined);
      } catch {
        // Not UTF-8: not a value this client wrote.
      }
    }
  }
  return headerText(value);
}

/** Whether `name` is an HTTP token, which a header name must be. */
export function isToken(name: string): boolean {
  return /^[!#$%&'*+\-.^_`|~0-9A-Za-z]+$/.test(name);
}
