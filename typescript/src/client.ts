// A client of one S3 bucket.

import {
  headerText,
  isToken,
  metadataHeaderValue,
  metadataText,
  formDecode,
  runtimeHeaderValue,
  uriEncode,
  utf8,
  utf8ByteString,
  type Bytes,
} from "./encoding.ts";
import { ProtocolError, RefusedError, S3Error, type S3ErrorKind } from "./errors.ts";
import { sha256Hex, sign, UNSIGNED_PAYLOAD, type Credentials } from "./sign.ts";
import { childText, children, child, parseXml, XmlError, type XmlElement } from "./xml.ts";

/**
 * Where a request names the bucket: in the path, as
 * `https://s3.eu-central-1.amazonaws.com/bucket/key`, or in the host, as
 * `https://bucket.s3.eu-central-1.amazonaws.com/key`.
 */
export type Addressing = "path" | "virtual-host";

/** What the client calls to send a request, as `fetch` does. */
export type Fetch = (url: string, init: RequestInit) => Promise<Response>;

export interface S3ClientOptions {
  /** The service's URL, such as `https://s3.eu-central-1.amazonaws.com`. */
  endpoint: string;
  region: string;
  bucket: string;
  credentials: Credentials;
  /** `path` by default, which every S3-compatible service takes. */
  addressing?: Addressing;
  /** The `fetch` that sends requests, the global one by default. */
  fetch?: Fetch;
}

/** The properties that a write stores and a read returns as headers. */
export interface ContentProperties {
  contentType?: string;
  contentEncoding?: string;
  contentLanguage?: string;
  contentDisposition?: string;
  cacheControl?: string;
}

/** What a read or a HEAD says about an object. */
export interface ObjectInfo extends ContentProperties {
  /** The entity tag, quotes included. */
  etag: string;
  /** The size of the whole object, also on a read of a range of it. */
  size: number;
  lastModified?: Date;
  /** The metadata pairs, by their names in lowercase. */
  metadata: Record<string, string>;
  /** The version, in a bucket that has versioning. */
  version?: string;
  /** The storage class, which S3 leaves out for `STANDARD`. */
  storageClass?: string;
  /** The `x-amz-restore` header of an archived object. */
  restore?: string;
}

/** Bytes of an object: from `start` up to but not including `end`. */
export type ByteRange =
  | { start: number; end?: number }
  /** The last `suffix` bytes. */
  | { suffix: number };

/** The bytes of an object that a ranged read served, as `Content-Range` says. */
export interface ServedRange {
  start: number;
  /** Exclusive. */
  end: number;
}

export interface Conditions {
  ifMatch?: string;
  ifNoneMatch?: string;
  ifModifiedSince?: Date;
  ifUnmodifiedSince?: Date;
}

export interface RequestOptions {
  signal?: AbortSignal;
}

export interface HeadOptions extends Conditions, RequestOptions {
  version?: string;
}

export interface GetOptions extends HeadOptions {
  range?: ByteRange;
}

export interface GetResult extends ObjectInfo {
  /**
   * The body, which errors with a {@link ProtocolError} if it ends before
   * the length that the answer stated.
   */
  body: ReadableStream<Uint8Array>;
  /** The length of the body. */
  contentLength: number;
  /** The window of the object that a ranged read served. */
  range?: ServedRange;
  bytes(): Promise<Uint8Array>;
  text(): Promise<string>;
}

/**
 * The body of a write: bytes, which are signed, or a stream of a stated
 * length, which is sent with `UNSIGNED-PAYLOAD`.
 */
export type PutBody = string | ArrayBuffer | ArrayBufferView | Blob | StreamBody;

/** A stream of a stated length. */
export interface StreamBody {
  stream: ReadableStream<Uint8Array>;
  length: number;
}

function isStreamBody(body: PutBody): body is StreamBody {
  return (
    typeof body === "object" &&
    !(body instanceof Blob) &&
    !(body instanceof ArrayBuffer) &&
    !ArrayBuffer.isView(body)
  );
}

export interface PutOptions extends ContentProperties, RequestOptions {
  /** Metadata pairs. S3 matches names without case, and stores them lowercased. */
  metadata?: Record<string, string>;
  tags?: Record<string, string>;
  /** Such as `STANDARD_IA`. */
  storageClass?: string;
  /** Write only over this entity tag. */
  ifMatch?: string;
  /** `*` writes only where no object is. */
  ifNoneMatch?: string;
}

export interface PutResult {
  etag: string;
  version?: string;
}

export interface DeleteOptions extends RequestOptions {
  version?: string;
  ifMatch?: string;
}

export interface DeleteResult {
  /** The version removed, or the version of the delete marker written. */
  version?: string;
  /** Whether the removal wrote or removed a delete marker. */
  deleteMarker: boolean;
}

export interface ListOptions extends RequestOptions {
  prefix?: string;
  /** Groups the keys up to the first delimiter after the prefix into prefixes. */
  delimiter?: string;
  /** Lists the keys after this one. */
  startAfter?: string;
  /** The token of the page before, from {@link ListPage.continuationToken}. */
  continuationToken?: string;
  /** At most this many objects and prefixes; S3 lists up to 1,000. */
  maxKeys?: number;
  /** Lists the owner of each object. */
  fetchOwner?: boolean;
}

export interface ListedObject {
  key: string;
  size: number;
  /** The entity tag, quotes included. */
  etag: string;
  lastModified?: Date;
  storageClass?: string;
  /** The algorithm of the checksum that S3 keeps for the object. */
  checksumAlgorithm?: string;
  owner?: { id: string; displayName?: string };
}

export interface ListPage {
  objects: ListedObject[];
  /** The groups of keys that the delimiter made. */
  prefixes: string[];
  /** The token of the next page, absent on the last page. */
  continuationToken?: string;
}

type Body = Bytes | StreamBody;

/** Options of `fetch` that one runtime takes and the others ignore. */
interface RuntimeRequestInit {
  /** Node and Bun: send a stream body. */
  duplex?: "half";
  /** Cloudflare Workers: `manual` leaves a Content-Encoding to the caller. */
  encodeResponseBody?: "automatic" | "manual";
  /** Bun: `false` leaves a Content-Encoding to the caller. */
  decompress?: boolean;
}

interface RequestSpec extends RequestOptions {
  method: "GET" | "HEAD" | "PUT" | "DELETE";
  key?: string;
  query?: [string, string][];
  headers?: [string, string][];
  body?: Body;
}

const emptySha256 = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

function errorKind(status: number, code: string | undefined): S3ErrorKind {
  switch (status) {
    case 404:
      if (code === "NoSuchBucket") return "bucket_not_found";
      return code === undefined ? "missing" : "not_found";
    case 412:
      return "precondition";
    case 304:
      return "not_modified";
    case 401:
    case 403:
      return "permission_denied";
    default:
      return "other";
  }
}

function errorFromXml(status: number, root: XmlElement, requestId: string | null): S3Error {
  const code = childText(root, "Code");
  return new S3Error(
    status,
    errorKind(status, code),
    code,
    childText(root, "Message"),
    childText(root, "RequestId") ?? requestId ?? undefined,
  );
}

/** The error that an answer outside 2xx carries. */
async function failure(response: Response, method: string): Promise<S3Error> {
  const requestId = response.headers.get("x-amz-request-id");
  const text = method === "HEAD" ? "" : await response.text();
  if (text.trim() !== "") {
    try {
      const root = parseXml(text);
      if (root.name === "Error") {
        return errorFromXml(response.status, root, requestId);
      }
    } catch (error) {
      if (!(error instanceof XmlError)) throw error;
    }
  }
  const status = response.status;
  return new S3Error(status, errorKind(status, undefined), undefined, undefined, requestId ?? undefined);
}

/** Reads an XML answer of a success, which may hold an error all the same. */
async function successXml(response: Response): Promise<XmlElement> {
  const root = parseXml(await response.text());
  if (root.name === "Error") {
    throw errorFromXml(response.status, root, response.headers.get("x-amz-request-id"));
  }
  return root;
}

function optionalHeader(headers: Headers, name: string): string | undefined {
  const value = headers.get(name);
  return value === null ? undefined : headerText(value);
}

function objectInfo(headers: Headers, size: number): ObjectInfo {
  const metadata: Record<string, string> = {};
  headers.forEach((value, name) => {
    if (name.startsWith("x-amz-meta-") && name.length > "x-amz-meta-".length) {
      metadata[name.slice("x-amz-meta-".length)] = metadataText(value);
    }
  });
  const lastModified = headers.get("last-modified");
  const info: ObjectInfo = { etag: headers.get("etag") ?? "", size, metadata };
  const optional: [keyof ObjectInfo, string][] = [
    ["contentType", "content-type"],
    ["contentEncoding", "content-encoding"],
    ["contentLanguage", "content-language"],
    ["contentDisposition", "content-disposition"],
    ["cacheControl", "cache-control"],
    ["version", "x-amz-version-id"],
    ["storageClass", "x-amz-storage-class"],
    ["restore", "x-amz-restore"],
  ];
  for (const [field, header] of optional) {
    const value = optionalHeader(headers, header);
    if (value !== undefined) {
      (info as unknown as Record<string, string>)[field] = value;
    }
  }
  if (lastModified !== null) {
    info.lastModified = new Date(lastModified);
  }
  return info;
}

function contentLength(headers: Headers): number | undefined {
  const value = headers.get("content-length");
  return value !== null && /^\d+$/.test(value) ? Number(value) : undefined;
}

/**
 * A body that errors if it ends before `length` bytes, which a runtime does
 * not always notice.
 */
function checkedBody(body: ReadableStream<Uint8Array>, length: number): ReadableStream<Uint8Array> {
  let received = 0;
  return body.pipeThrough(
    new TransformStream<Uint8Array, Uint8Array>({
      transform(chunk, controller) {
        received += chunk.length;
        controller.enqueue(chunk);
      },
      flush(controller) {
        if (received !== length) {
          controller.error(
            new ProtocolError(`the body held ${received} bytes, and the answer stated ${length}`),
          );
        }
      },
    }),
  );
}

function checkNonNegativeInteger(value: number, parameter: string): void {
  if (!Number.isSafeInteger(value) || value < 0) {
    throw new RefusedError(parameter, `${value} is no byte offset`);
  }
}

/** The `Range` header of a range. */
function rangeHeader(range: ByteRange): string {
  if ("suffix" in range) {
    checkNonNegativeInteger(range.suffix, "range");
    return `bytes=-${range.suffix}`;
  }
  checkNonNegativeInteger(range.start, "range");
  if (range.end === undefined) {
    return `bytes=${range.start}-`;
  }
  checkNonNegativeInteger(range.end, "range");
  // HTTP has no range of no bytes, and S3 sends the whole object for an
  // inverted one.
  if (range.end <= range.start) {
    throw new RefusedError("range", "the range holds no bytes");
  }
  return `bytes=${range.start}-${range.end - 1}`;
}

/**
 * A header value of text that a write stores. HTTP drops whitespace at
 * either end of a value, and a control character is no valid value, so the
 * value would not read back.
 */
function storedHeaderValue(value: string, parameter: string): string {
  if (/[\x00-\x08\x0a-\x1f\x7f]/.test(value) || /^[ \t]|[ \t]$/.test(value)) {
    throw new RefusedError(parameter, "the value would not read back as written");
  }
  return utf8ByteString(value);
}

function httpDate(date: Date, parameter: string): string {
  if (Number.isNaN(date.getTime())) {
    throw new RefusedError(parameter, "invalid date");
  }
  return date.toUTCString();
}

function conditionHeaders(conditions: Conditions): [string, string][] {
  const headers: [string, string][] = [];
  if (conditions.ifMatch !== undefined) {
    headers.push(["if-match", storedHeaderValue(conditions.ifMatch, "ifMatch")]);
  }
  if (conditions.ifNoneMatch !== undefined) {
    headers.push(["if-none-match", storedHeaderValue(conditions.ifNoneMatch, "ifNoneMatch")]);
  }
  if (conditions.ifModifiedSince !== undefined) {
    headers.push(["if-modified-since", httpDate(conditions.ifModifiedSince, "ifModifiedSince")]);
  }
  if (conditions.ifUnmodifiedSince !== undefined) {
    headers.push([
      "if-unmodified-since",
      httpDate(conditions.ifUnmodifiedSince, "ifUnmodifiedSince"),
    ]);
  }
  return headers;
}

function metadataHeaders(metadata: Record<string, string>): [string, string][] {
  const seen = new Set<string>();
  return Object.entries(metadata).map(([name, value]) => {
    if (!isToken(name)) {
      throw new RefusedError("metadata", `${JSON.stringify(name)} is no header name`);
    }
    // S3 merges two names that differ only in case into one pair.
    const lower = name.toLowerCase();
    if (seen.has(lower)) {
      throw new RefusedError("metadata", `two names are ${JSON.stringify(lower)} without case`);
    }
    seen.add(lower);
    const header = metadataHeaderValue(value);
    if (header === "line_break") {
      throw new RefusedError("metadata", "S3 stores CR and LF as spaces");
    }
    return [`x-amz-meta-${name}`, utf8ByteString(header)];
  });
}

async function bodyBytes(body: Exclude<PutBody, StreamBody>): Promise<Bytes> {
  if (typeof body === "string") return utf8(body);
  if (body instanceof ArrayBuffer) return new Uint8Array(body);
  if (ArrayBuffer.isView(body)) {
    const view = new Uint8Array(body.buffer, body.byteOffset, body.byteLength);
    // A view of a SharedArrayBuffer is copied out of it.
    return view.buffer instanceof ArrayBuffer ? (view as Bytes) : view.slice();
  }
  return new Uint8Array(await body.arrayBuffer());
}

function emptyStream(): ReadableStream<Uint8Array> {
  return new ReadableStream({ start: (controller) => controller.close() });
}

/** A client of one S3 bucket. */
export class S3Client {
  readonly bucket: string;
  readonly region: string;
  readonly #credentials: Credentials;
  readonly #protocol: string;
  readonly #host: string;
  /** The path that the bucket's URLs start with, without a trailing `/`. */
  readonly #basePath: string;
  readonly #fetch: Fetch;

  constructor(options: S3ClientOptions) {
    const endpoint = new URL(options.endpoint);
    this.bucket = options.bucket;
    this.region = options.region;
    this.#credentials = options.credentials;
    this.#protocol = endpoint.protocol;
    const endpointPath = endpoint.pathname.replace(/\/+$/, "");
    if ((options.addressing ?? "path") === "path") {
      this.#host = endpoint.host;
      this.#basePath = `${endpointPath}/${uriEncode(options.bucket)}`;
    } else {
      this.#host = `${options.bucket}.${endpoint.host}`;
      this.#basePath = endpointPath;
    }
    this.#fetch = options.fetch ?? ((url, init) => globalThis.fetch(url, init));
  }

  #path(key: string | undefined): string {
    if (key === undefined) {
      return this.#basePath === "" ? "/" : this.#basePath;
    }
    if (key === "") {
      throw new RefusedError("key", "the key is empty");
    }
    // A URL resolves a segment of `.` or `..`, also escaped, so no request
    // reaches such a key.
    if (key.split("/").some((segment) => segment === "." || segment === "..")) {
      throw new RefusedError("key", "a URL cannot hold a segment . or ..");
    }
    return `${this.#basePath}/${uriEncode(key, true)}`;
  }

  async #send(spec: RequestSpec): Promise<Response> {
    const path = this.#path(spec.key);
    const query = (spec.query ?? [])
      .map(([name, value]) => `${uriEncode(name)}=${uriEncode(value)}`)
      .join("&");
    const headers = spec.headers ?? [];
    let payloadHash = emptySha256;
    let body: BodyInit | null = null;
    const init: RequestInit & RuntimeRequestInit = {
      method: spec.method,
      redirect: "manual",
      signal: spec.signal ?? null,
      // Hand back a body as S3 stores it, also with a Content-Encoding.
      encodeResponseBody: "manual",
      decompress: false,
    };
    if (spec.body instanceof Uint8Array) {
      payloadHash = await sha256Hex(spec.body);
      body = spec.body;
    } else if (spec.body !== undefined) {
      payloadHash = UNSIGNED_PAYLOAD;
      body = this.#fixedLength(spec.body.stream, spec.body.length);
      headers.push(["content-length", String(spec.body.length)]);
      init.duplex = "half";
    }
    const signature = await sign({
      method: spec.method,
      host: this.#host,
      path,
      query,
      headers,
      payloadHash,
      credentials: this.#credentials,
      region: this.region,
      date: new Date(),
    });
    const sent = new Headers();
    for (const [name, value] of [...headers, ...signature.headers]) {
      sent.append(name, runtimeHeaderValue(value));
    }
    init.headers = sent;
    init.body = body;
    const url = `${this.#protocol}//${this.#host}${path}${query === "" ? "" : `?${query}`}`;
    return this.#fetch(url, init);
  }

  /**
   * A stream that sends its stated length. Cloudflare Workers send a stream
   * of unknown length chunked, which S3 refuses, unless it goes through a
   * `FixedLengthStream`.
   */
  #fixedLength(stream: ReadableStream<Uint8Array>, length: number): ReadableStream<Uint8Array> {
    const FixedLengthStream = (
      globalThis as {
        FixedLengthStream?: new (length: number) => TransformStream<Uint8Array, Uint8Array>;
      }
    ).FixedLengthStream;
    return FixedLengthStream === undefined
      ? stream
      : stream.pipeThrough(new FixedLengthStream(length));
  }

  /** Reads an object, or the range of it that `options.range` names. */
  async get(key: string, options: GetOptions = {}): Promise<GetResult> {
    const headers = conditionHeaders(options);
    if (options.range !== undefined) {
      headers.push(["range", rangeHeader(options.range)]);
    }
    const response = await this.#send({
      method: "GET",
      key,
      query: options.version === undefined ? [] : [["versionId", options.version]],
      headers,
      signal: options.signal,
    });
    if (response.status !== 200 && response.status !== 206) {
      throw await failure(response, "GET");
    }

    const length = contentLength(response.headers);
    let size = length ?? 0;
    let range: ServedRange | undefined;
    const contentRange = /^bytes (\d+)-(\d+)\/(\d+|\*)$/.exec(
      response.headers.get("content-range") ?? "",
    );
    if (response.status === 206 && contentRange) {
      range = { start: Number(contentRange[1]), end: Number(contentRange[2]) + 1 };
      size = contentRange[3] === "*" ? range.end : Number(contentRange[3]);
    }
    let body = response.body ?? emptyStream();
    // A runtime that decodes a Content-Encoding changes the length.
    if (length !== undefined && response.headers.get("content-encoding") === null) {
      body = checkedBody(body, length);
    }
    const bytes = async () => new Uint8Array(await new Response(body).arrayBuffer());
    return {
      ...objectInfo(response.headers, size),
      body,
      contentLength: length ?? 0,
      ...(range && { range }),
      bytes,
      text: async () => new TextDecoder().decode(await bytes()),
    };
  }

  /** Reads what S3 says about an object, without its body. */
  async head(key: string, options: HeadOptions = {}): Promise<ObjectInfo> {
    const response = await this.#send({
      method: "HEAD",
      key,
      query: options.version === undefined ? [] : [["versionId", options.version]],
      headers: conditionHeaders(options),
      signal: options.signal,
    });
    if (response.status !== 200) {
      throw await failure(response, "HEAD");
    }
    return objectInfo(response.headers, contentLength(response.headers) ?? 0);
  }

  /** Writes a whole object. */
  async put(key: string, body: PutBody, options: PutOptions = {}): Promise<PutResult> {
    const headers = conditionHeaders({ ifMatch: options.ifMatch, ifNoneMatch: options.ifNoneMatch });
    const properties: [keyof ContentProperties, string][] = [
      ["contentType", "content-type"],
      ["contentEncoding", "content-encoding"],
      ["contentLanguage", "content-language"],
      ["contentDisposition", "content-disposition"],
      ["cacheControl", "cache-control"],
    ];
    for (const [field, header] of properties) {
      const value = options[field];
      if (value !== undefined) {
        headers.push([header, storedHeaderValue(value, field)]);
      }
    }
    if (options.storageClass !== undefined) {
      headers.push(["x-amz-storage-class", storedHeaderValue(options.storageClass, "storageClass")]);
    }
    if (options.tags !== undefined) {
      const tagging = Object.entries(options.tags)
        .map(([name, value]) => `${uriEncode(name)}=${uriEncode(value)}`)
        .join("&");
      headers.push(["x-amz-tagging", tagging]);
    }
    headers.push(...metadataHeaders(options.metadata ?? {}));

    const sent: Body = isStreamBody(body)
      ? (checkNonNegativeInteger(body.length, "body"), body)
      : await bodyBytes(body);
    const response = await this.#send({
      method: "PUT",
      key,
      headers,
      body: sent,
      signal: options.signal,
    });
    if (response.status !== 200) {
      throw await failure(response, "PUT");
    }
    // Read the body to its end, so that the connection can be used again.
    await response.arrayBuffer();
    const version = response.headers.get("x-amz-version-id");
    return {
      etag: response.headers.get("etag") ?? "",
      ...(version !== null && { version }),
    };
  }

  /**
   * Removes an object, or one version of it. S3 answers a removal of an
   * absent key as a success.
   */
  async delete(key: string, options: DeleteOptions = {}): Promise<DeleteResult> {
    const response = await this.#send({
      method: "DELETE",
      key,
      query: options.version === undefined ? [] : [["versionId", options.version]],
      headers: conditionHeaders({ ifMatch: options.ifMatch }),
      signal: options.signal,
    });
    if (response.status !== 204 && response.status !== 200) {
      throw await failure(response, "DELETE");
    }
    await response.arrayBuffer();
    const version = response.headers.get("x-amz-version-id");
    return {
      ...(version !== null && { version }),
      deleteMarker: response.headers.get("x-amz-delete-marker") === "true",
    };
  }

  /** Lists one page of the bucket's objects, with ListObjectsV2. */
  async list(options: ListOptions = {}): Promise<ListPage> {
    // Without `encoding-type=url`, S3 writes some characters of keys in
    // forms that XML 1.0 does not read back.
    const query: [string, string][] = [
      ["list-type", "2"],
      ["encoding-type", "url"],
    ];
    if (options.prefix !== undefined) query.push(["prefix", options.prefix]);
    if (options.delimiter !== undefined) query.push(["delimiter", options.delimiter]);
    if (options.startAfter !== undefined) query.push(["start-after", options.startAfter]);
    if (options.continuationToken !== undefined) {
      query.push(["continuation-token", options.continuationToken]);
    }
    if (options.maxKeys !== undefined) {
      checkNonNegativeInteger(options.maxKeys, "maxKeys");
      query.push(["max-keys", String(options.maxKeys)]);
    }
    if (options.fetchOwner) query.push(["fetch-owner", "true"]);

    const response = await this.#send({ method: "GET", query, signal: options.signal });
    if (response.status !== 200) {
      throw await failure(response, "GET");
    }
    const root = await successXml(response);
    if (root.name !== "ListBucketResult") {
      throw new ProtocolError(`a listing answered <${root.name}>`);
    }
    // A service may ignore `encoding-type`, and then names no EncodingType.
    const decode =
      childText(root, "EncodingType") === "url" ? formDecode : (text: string) => text;

    const objects = children(root, "Contents").map((entry) => {
      const listed: ListedObject = {
        key: decode(childText(entry, "Key") ?? ""),
        size: Number(childText(entry, "Size") ?? 0),
        etag: childText(entry, "ETag") ?? "",
      };
      const lastModified = childText(entry, "LastModified");
      if (lastModified !== undefined) listed.lastModified = new Date(lastModified);
      const storageClass = childText(entry, "StorageClass");
      if (storageClass !== undefined) listed.storageClass = storageClass;
      const checksumAlgorithm = childText(entry, "ChecksumAlgorithm");
      if (checksumAlgorithm !== undefined) listed.checksumAlgorithm = checksumAlgorithm;
      const owner = child(entry, "Owner");
      const ownerId = owner && childText(owner, "ID");
      if (owner !== undefined && ownerId !== undefined) {
        const displayName = childText(owner, "DisplayName");
        listed.owner = { id: ownerId, ...(displayName !== undefined && { displayName }) };
      }
      return listed;
    });
    const prefixes = children(root, "CommonPrefixes").map((group) =>
      decode(childText(group, "Prefix") ?? ""),
    );
    const page: ListPage = { objects, prefixes };
    if (childText(root, "IsTruncated") === "true") {
      const token = childText(root, "NextContinuationToken");
      if (token === undefined || token === "") {
        throw new ProtocolError("a truncated page named no continuation token");
      }
      page.continuationToken = token;
    }
    return page;
  }

  /** Lists every page, following each page's continuation token. */
  async *listPages(
    options: Omit<ListOptions, "continuationToken"> = {},
  ): AsyncGenerator<ListPage, void, undefined> {
    let continuationToken: string | undefined;
    do {
      const page = await this.list({
        ...options,
        ...(continuationToken !== undefined && { continuationToken }),
      });
      if (page.continuationToken !== undefined && page.continuationToken === continuationToken) {
        throw new ProtocolError("a page named itself as the next one");
      }
      continuationToken = page.continuationToken;
      yield page;
    } while (continuationToken !== undefined);
  }
}
