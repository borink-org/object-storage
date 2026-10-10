// What the borink-org/object-tests adapters of the TypeScript S3 client share:
// the mapping of one grader message onto the client, and of what the client
// returns onto the result. It uses only what the library does, so that it runs
// in Bun and in workerd alike. See the grader's REFERENCE.md for the protocol.

import {
  ProtocolError,
  RefusedError,
  S3Client,
  S3Error,
  sign,
  UNSIGNED_PAYLOAD,
  type ByteRange,
  type Credentials,
  type Fetch,
  type GetResult,
  type ObjectInfo,
  type PutBody,
  type PutOptions,
} from "../src/index.ts";

/** The version of the object-tests protocol this adapter speaks. */
const PROTOCOL_VERSION = 1;

export type Json = null | boolean | number | string | Json[] | { [key: string]: Json };
export type Call = Record<string, Json>;
export type Result = Record<string, Json>;

/** What the runtime that hosts the adapter provides. */
export interface Host {
  /** Sends the client's requests, and hands back bodies as stored. */
  fetch: Fetch;
  /** The credentials of a live run. */
  liveCredentials(): Credentials;
  /**
   * Copies a body into the grader's socket at `address`, and returns its
   * length. Without it, a call that names a sink is unsupported.
   */
  sink?(body: ReadableStream<Uint8Array>, address: string): Promise<number>;
}

const ok = (value: Result): Result => ({ outcome: "ok", value });

function unsupported(reason: string, parameter?: string): Result {
  return { outcome: "unsupported", reason, ...(parameter !== undefined && { parameter }) };
}

/** `ifMatch` as the protocol names it, `if_match`. */
function snakeCase(name: string): string {
  return name.replace(/[A-Z]/g, (letter) => `_${letter.toLowerCase()}`);
}

/** The call fields the library takes, by operation. */
const mappedFields: Record<string, string[]> = {
  get: ["key", "range", "if_match", "if_none_match", "if_modified_since", "if_unmodified_since", "version", "body_sink"],
  head: ["key", "if_match", "if_none_match", "if_modified_since", "if_unmodified_since", "version"],
  put: [
    "key",
    "body_base64",
    "body",
    "if_match",
    "if_none_match",
    // Refused below: the library's API takes no date condition on a write.
    "if_modified_since",
    "if_unmodified_since",
    "metadata",
    "content_type",
    "content_encoding",
    "content_language",
    "content_disposition",
    "cache_control",
    "tags",
    "storage_class",
  ],
  delete: ["key", "version", "if_match"],
  list: ["prefix", "page_size"],
  list_page: ["prefix", "continuation_token", "start_after", "delimiter", "page_size", "fetch_owner"],
};

function text(call: Call, field: string): string | undefined {
  const value = call[field];
  return typeof value === "string" ? value : undefined;
}

function number(call: Call, field: string): number | undefined {
  const value = call[field];
  return typeof value === "number" ? value : undefined;
}

function decodeBase64(encoded: string): Uint8Array<ArrayBuffer> {
  return Uint8Array.from(atob(encoded), (char) => char.charCodeAt(0));
}

function encodeBase64(bytes: Uint8Array): string {
  let text = "";
  for (let at = 0; at < bytes.length; at += 0x8000) {
    text += String.fromCharCode(...bytes.subarray(at, at + 0x8000));
  }
  return btoa(text);
}

function hex(bytes: Uint8Array): string {
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

function errorResult(error: unknown): Result {
  if (error instanceof S3Error) {
    return {
      outcome: "error",
      status: error.status,
      kind: error.kind === "bucket_not_found" ? "container_not_found" : error.kind,
      ...(error.code !== undefined && { code: error.code }),
      reason: error.message,
    };
  }
  if (error instanceof RefusedError) {
    return { outcome: "refused", kind: "refused", parameter: snakeCase(error.parameter), reason: error.message };
  }
  if (error instanceof ProtocolError) {
    return { outcome: "error", kind: "other", reason: error.message };
  }
  return { outcome: "error", kind: "transport", reason: String(error) };
}

function conditions(call: Call) {
  const date = (field: string) => {
    const value = text(call, field);
    return value === undefined ? undefined : new Date(value);
  };
  return {
    ifMatch: text(call, "if_match"),
    ifNoneMatch: text(call, "if_none_match"),
    ifModifiedSince: date("if_modified_since"),
    ifUnmodifiedSince: date("if_unmodified_since"),
  };
}

function range(call: Call): ByteRange | undefined {
  const value = call.range as Call | undefined;
  if (value === undefined) return undefined;
  const suffix = number(value, "suffix");
  if (suffix !== undefined) return { suffix };
  const end = number(value, "end");
  return { start: number(value, "start")!, ...(end !== undefined && { end }) };
}

function infoValue(info: ObjectInfo): Result {
  const value: Result = { etag: info.etag, metadata: info.metadata };
  const fields: [keyof ObjectInfo, string][] = [
    ["contentType", "content_type"],
    ["contentEncoding", "content_encoding"],
    ["contentLanguage", "content_language"],
    ["contentDisposition", "content_disposition"],
    ["cacheControl", "cache_control"],
    ["version", "version"],
    ["storageClass", "storage_class"],
    ["restore", "restore_status"],
  ];
  for (const [field, name] of fields) {
    const fieldValue = info[field];
    if (typeof fieldValue === "string") value[name] = fieldValue;
  }
  return value;
}

async function getValue(result: GetResult, call: Call, host: Host): Promise<Result> {
  const value = infoValue(result);
  if (result.range !== undefined) {
    value.content_range = `bytes ${result.range.start}-${result.range.end - 1}/${result.size}`;
  }
  const address = text(call, "body_sink");
  if (address !== undefined) {
    value.size = await host.sink!(result.body, address);
  } else {
    const bytes = await result.bytes();
    value.body_base64 = encodeBase64(bytes);
    value.size = bytes.length;
  }
  return value;
}

/** A pattern repeated to a length, generated as it is read. */
function repeated(pattern: Uint8Array, length: number): ReadableStream<Uint8Array> {
  const block = new Uint8Array(Math.ceil((64 * 1024) / Math.max(pattern.length, 1) + 1) * pattern.length);
  for (let at = 0; at < block.length; at += pattern.length) block.set(pattern, at);
  let offset = 0;
  let remaining = length;
  return new ReadableStream({
    pull(controller) {
      if (remaining === 0) {
        controller.close();
        return;
      }
      const count = Math.min(remaining, block.length - offset);
      controller.enqueue(block.slice(offset, offset + count));
      offset = (offset + count) % pattern.length;
      remaining -= count;
    },
  });
}

function putBody(call: Call): PutBody {
  const body = call.body as Call | undefined;
  if (body === undefined) return decodeBase64(text(call, "body_base64") ?? "");
  const data = body.data as Call;
  const length = number(data, "length")!;
  return { stream: repeated(decodeBase64(text(data, "pattern_base64")!), length), length };
}

async function put(client: S3Client, call: Call): Promise<Result> {
  for (const field of ["if_modified_since", "if_unmodified_since"]) {
    if (call[field] !== undefined) {
      return { outcome: "refused", kind: "no_date_condition", parameter: field };
    }
  }
  const options: PutOptions = {};
  const properties = ["content_type", "content_encoding", "content_language", "content_disposition", "cache_control", "storage_class"] as const;
  for (const field of properties) {
    const value = text(call, field);
    if (value !== undefined) {
      (options as Record<string, string>)[field.replace(/_(.)/g, (_, letter: string) => letter.toUpperCase())] = value;
    }
  }
  if (call.if_match !== undefined) options.ifMatch = text(call, "if_match")!;
  if (call.if_none_match !== undefined) options.ifNoneMatch = text(call, "if_none_match")!;
  if (call.metadata !== undefined) options.metadata = call.metadata as Record<string, string>;
  const tags = call.tags;
  if (Array.isArray(tags)) {
    const pairs = tags.map((tag) => [text(tag as Call, "key")!, text(tag as Call, "value")!] as const);
    if (new Set(pairs.map(([key]) => key)).size < pairs.length) {
      return unsupported("tags are a record, which names a key once", "tags");
    }
    options.tags = Object.fromEntries(pairs);
  } else if (tags !== undefined) {
    options.tags = tags as Record<string, string>;
  }
  const result = await client.put(text(call, "key")!, putBody(call), options);
  return ok({ etag: result.etag, ...(result.version !== undefined && { version: result.version }) });
}

async function listPage(client: S3Client, call: Call): Promise<Result> {
  const page = await client.list({
    prefix: text(call, "prefix"),
    delimiter: text(call, "delimiter"),
    startAfter: text(call, "start_after"),
    continuationToken: text(call, "continuation_token"),
    maxKeys: number(call, "page_size"),
    fetchOwner: call.fetch_owner === true,
  });
  const entries = page.objects.map((object) => {
    const entry: Result = { key: object.key, size: object.size, etag: object.etag };
    if (object.lastModified !== undefined) {
      entry.last_modified = object.lastModified.toISOString().replace(/\.\d{3}Z$/, "Z");
    }
    if (object.storageClass !== undefined) entry.storage_class = object.storageClass;
    if (object.checksumAlgorithm !== undefined) entry.checksum_algorithm = object.checksumAlgorithm;
    if (object.owner !== undefined) entry.owner_id = object.owner.id;
    return entry;
  });
  return ok({ entries, prefixes: page.prefixes, continuation_token: page.continuationToken ?? "" });
}

async function listAll(client: S3Client, call: Call): Promise<Result> {
  const keys: string[] = [];
  for await (const page of client.listPages({ prefix: text(call, "prefix"), maxKeys: number(call, "page_size") })) {
    keys.push(...page.objects.map((object) => object.key));
  }
  return ok({ keys });
}

/** Signs a request that the call names, for the signing vectors. */
async function signVector(call: Call): Promise<Result> {
  if (call.signing_algorithm !== undefined && call.signing_algorithm !== "sigv4") {
    return unsupported("the library signs with SigV4 alone", "signing_algorithm");
  }
  const url = /^[a-z]+:\/\/([^/?]+)([^?]*)(?:\?(.*))?$/.exec(text(call, "url")!)!;
  const headers: [string, string][] = [];
  for (const [name, value] of Object.entries(call.headers as Record<string, Json>)) {
    for (const each of Array.isArray(value) ? value : [value]) headers.push([name, each as string]);
  }
  const body = decodeBase64(text(call, "body_base64") ?? "");
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", body));
  const credentials = call.credentials as Call;
  const signature = await sign({
    method: text(call, "method")!,
    host: url[1]!,
    path: url[2] || "/",
    query: url[3] ?? "",
    headers,
    payloadHash: text(call, "payload_hash") ?? hex(digest),
    credentials: {
      accessKeyId: text(credentials, "access_key_id")!,
      secretAccessKey: text(credentials, "secret_access_key")!,
      ...(credentials.session_token !== undefined && { sessionToken: text(credentials, "session_token")! }),
    },
    region: text(call, "region")!,
    service: text(call, "service") ?? "s3",
    date: new Date(text(call, "clock")!),
  });
  return ok({
    authorization: signature.headers.find(([name]) => name === "authorization")![1],
    canonical_request: signature.canonicalRequest,
    headers: Object.fromEntries(signature.headers),
  });
}

/** Performs the call of one grader message, and returns its result. */
export async function execute(message: Call, host: Host): Promise<Result> {
  if (message.version !== PROTOCOL_VERSION) {
    throw new Error(`object-tests sent protocol version ${message.version}, and this adapter speaks ${PROTOCOL_VERSION}`);
  }
  const call = message.call as Call;
  const op = text(call, "op")!;
  if (message.provider !== "s3") {
    return unsupported("the library speaks S3 alone");
  }
  if (op === "s3.sign") return signVector(call);
  const fields = mappedFields[op];
  if (fields === undefined) {
    return unsupported(`the library has no ${op} operation`);
  }
  const unmapped = Object.keys(call).find((field) => field !== "op" && field !== "credential_mode" && !fields.includes(field));
  if (unmapped !== undefined) {
    return unsupported(`the library takes no ${unmapped}`, unmapped);
  }
  if (call.body_sink !== undefined && host.sink === undefined) {
    return unsupported("this host opens no socket to the grader", "body_sink");
  }
  const endpoint = message.endpoint as Call;
  if (endpoint.directory_bucket === true) {
    return unsupported("the library has no sessions of directory buckets");
  }

  const credentials =
    message.mode !== "live"
      ? { accessKeyId: "offline", secretAccessKey: "offline-placeholder-secret" }
      : call.credential_mode === "invalid"
        ? { accessKeyId: "AKIDINVALID", secretAccessKey: "invalid" }
        : host.liveCredentials();
  const client = new S3Client({
    endpoint: text(endpoint, "url")!,
    region: text(endpoint, "region") ?? "us-east-1",
    bucket: text(endpoint, "bucket")!,
    credentials,
    fetch: host.fetch,
  });

  const key = text(call, "key")!;
  const version = text(call, "version");
  try {
    switch (op) {
      case "get": {
        const requested = range(call);
        const result = await client.get(key, {
          ...conditions(call),
          ...(requested !== undefined && { range: requested }),
          ...(version !== undefined && { version }),
        });
        return ok(await getValue(result, call, host));
      }
      case "head": {
        const info = await client.head(key, { ...conditions(call), ...(version !== undefined && { version }) });
        return ok({ ...infoValue(info), size: info.size });
      }
      case "put":
        return await put(client, call);
      case "delete":
        await client.delete(key, {
          ...(version !== undefined && { version }),
          ...(call.if_match !== undefined && { ifMatch: text(call, "if_match")! }),
        });
        return ok({});
      case "list":
        return await listAll(client, call);
      case "list_page":
        return await listPage(client, call);
    }
  } catch (error) {
    return errorResult(error);
  }
  return unsupported(`the library has no ${op} operation`);
}

