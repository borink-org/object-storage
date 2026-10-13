// AWS Signature Version 4 for S3, with Web Crypto.

import { byteStringBytes, hex, reencode, utf8, type Bytes } from "./encoding.ts";

/** The credentials that sign requests. */
export interface Credentials {
  accessKeyId: string;
  secretAccessKey: string;
  /** The token of temporary credentials, sent as `x-amz-security-token`. */
  sessionToken?: string;
}

/** What a payload hash signs in place of the SHA-256 of a body. */
export const UNSIGNED_PAYLOAD = "UNSIGNED-PAYLOAD";

/** A request to sign. */
export interface SignInput {
  method: string;
  /** The host, with its port where the URL names one. */
  host: string;
  /** The path as the URL sends it, percent-encoded. */
  path: string;
  /** The query as the URL sends it, without the `?`. */
  query: string;
  /**
   * The headers the request sends, each a byte string. A name given twice,
   * or a value given as a list, is signed as one value joined by commas.
   */
  headers: [name: string, value: string][];
  /** The hex SHA-256 of the body, or [`UNSIGNED_PAYLOAD`]. */
  payloadHash: string;
  credentials: Credentials;
  region: string;
  service?: string;
  date: Date;
}

/** A signed request: the headers to add, and the canonical request. */
export interface Signature {
  /** `authorization`, `x-amz-date`, `x-amz-content-sha256` and the token. */
  headers: [name: string, value: string][];
  canonicalRequest: string;
}

const subtle = globalThis.crypto.subtle;

export async function sha256Hex(data: Bytes): Promise<string> {
  return hex(new Uint8Array(await subtle.digest("SHA-256", data)));
}

async function hmac(key: Bytes | CryptoKey, data: string): Promise<Bytes> {
  const cryptoKey =
    key instanceof Uint8Array
      ? await subtle.importKey("raw", key, { name: "HMAC", hash: "SHA-256" }, false, ["sign"])
      : key;
  return new Uint8Array(await subtle.sign("HMAC", cryptoKey, utf8(data)));
}

/** `20130524T000000Z`. */
export function amzDate(date: Date): string {
  return date.toISOString().replace(/[-:]/g, "").replace(/\.\d{3}/, "");
}

/** Signing keys by the day, region, service and secret they were derived for. */
const signingKeys = new Map<string, Promise<CryptoKey>>();

function signingKey(
  secret: string,
  day: string,
  region: string,
  service: string,
): Promise<CryptoKey> {
  const id = `${day}/${region}/${service}/${secret}`;
  let key = signingKeys.get(id);
  if (key === undefined) {
    key = (async () => {
      const dated = await hmac(utf8(`AWS4${secret}`), day);
      const regional = await hmac(dated, region);
      const serviced = await hmac(regional, service);
      const raw = await hmac(serviced, "aws4_request");
      return subtle.importKey("raw", raw, { name: "HMAC", hash: "SHA-256" }, false, ["sign"]);
    })();
    // A long-running process signs for one day at a time.
    if (signingKeys.size > 16) {
      signingKeys.clear();
    }
    signingKeys.set(id, key);
  }
  return key;
}

function canonicalQuery(query: string): string {
  return query
    .split("&")
    .filter((pair) => pair !== "")
    .map((pair) => {
      const equals = pair.indexOf("=");
      const name = equals < 0 ? pair : pair.slice(0, equals);
      const value = equals < 0 ? "" : pair.slice(equals + 1);
      return [reencode(name, false), reencode(value, false)] as const;
    })
    .sort(([nameA, valueA], [nameB, valueB]) =>
      nameA < nameB ? -1 : nameA > nameB ? 1 : valueA < valueB ? -1 : valueA > valueB ? 1 : 0,
    )
    .map(([name, value]) => `${name}=${value}`)
    .join("&");
}

/** Signs a request with SigV4, as S3 checks it. */
export async function sign(input: SignInput): Promise<Signature> {
  const service = input.service ?? "s3";
  const stamp = amzDate(input.date);
  const day = stamp.slice(0, 8);
  const added: [string, string][] = [
    ["x-amz-content-sha256", input.payloadHash],
    ["x-amz-date", stamp],
  ];
  if (input.credentials.sessionToken !== undefined) {
    added.push(["x-amz-security-token", input.credentials.sessionToken]);
  }

  const values = new Map<string, string[]>([["host", [input.host]]]);
  for (const [name, value] of [...input.headers, ...added]) {
    const lower = name.toLowerCase();
    // HTTP drops the whitespace at either end, and SigV4 folds runs of it.
    const folded = value.trim().replace(/ +/g, " ");
    values.set(lower, [...(values.get(lower) ?? []), folded]);
  }
  const names = [...values.keys()].sort();
  const signedHeaders = names.join(";");
  // S3 signs the path as it is sent, without normalizing it.
  const canonicalRequest = [
    input.method,
    reencode(input.path, true),
    canonicalQuery(input.query),
    names.map((name) => `${name}:${values.get(name)!.join(",")}\n`).join(""),
    signedHeaders,
    input.payloadHash,
  ].join("\n");

  const scope = `${day}/${input.region}/${service}/aws4_request`;
  const stringToSign = [
    "AWS4-HMAC-SHA256",
    stamp,
    scope,
    // A header value is a byte string, which the hash takes byte for byte.
    await sha256Hex(byteStringBytes(canonicalRequest)),
  ].join("\n");
  const key = await signingKey(input.credentials.secretAccessKey, day, input.region, service);
  const signature = hex(await hmac(key, stringToSign));
  const authorization =
    `AWS4-HMAC-SHA256 Credential=${input.credentials.accessKeyId}/${scope}, ` +
    `SignedHeaders=${signedHeaders}, Signature=${signature}`;
  return { headers: [["authorization", authorization], ...added], canonicalRequest };
}
