# @borink/object-storage (TypeScript)

An S3 client for TypeScript and JavaScript, built on `fetch` and Web Crypto alone. It has no runtime dependencies and uses no Node APIs, so it runs in Cloudflare Workers as well as in Bun, Deno, Node and browsers.

It is separate from the Rust crates in this repository. It covers the basic operations on one bucket, each a single request:

- `get`: read an object or a range of it, as a stream
- `head`: read what S3 says about an object, without its body
- `put`: write a whole object, from bytes or from a stream of a stated length
- `delete`: remove an object, or one version of it
- `list` and `listPages`: list objects with ListObjectsV2

It does not do uploads in parts, copies, batch removals, version listings, tagging calls, checksums or retries.

## Usage

```ts
import { S3Client, S3Error } from "@borink/object-storage";

const client = new S3Client({
  endpoint: "https://s3.eu-central-1.amazonaws.com",
  region: "eu-central-1",
  bucket: "my-bucket",
  credentials: { accessKeyId, secretAccessKey, sessionToken },
});

await client.put("notes/hello.txt", "hello", {
  contentType: "text/plain",
  metadata: { author: "me" },
});

const object = await client.get("notes/hello.txt", { range: { start: 0, end: 4 } });
console.log(object.range, await object.text()); // { start: 0, end: 4 } "hell"

try {
  await client.head("notes/absent.txt");
} catch (error) {
  if (error instanceof S3Error && error.kind === "missing") {
    // A HEAD answered 404.
  }
}

for await (const page of client.listPages({ prefix: "notes/", delimiter: "/" })) {
  console.log(page.objects.map((object) => object.key), page.prefixes);
}
```

In a Worker, a body can go from one request into S3 without being held in memory:

```ts
await client.put(key, {
  stream: request.body!,
  length: Number(request.headers.get("content-length")),
});
```

A stream is sent with `UNSIGNED-PAYLOAD`. Bytes, strings and `Blob`s are signed with their SHA-256. A request goes to `endpoint/bucket/key` by default. Pass `addressing: "virtual-host"` for `bucket.endpoint/key`.

## Errors

- `S3Error`: S3 refused the request. It has the `status`, the `code` S3 named, such as `NoSuchKey`, the `requestId`, and a `kind` that says what the answer means: `not_found`, `bucket_not_found`, `missing` (a 404 without a body, which cannot say whether the object or the bucket is absent), `precondition`, `not_modified`, `permission_denied` or `other`.
- `RefusedError`: the client refused a call before sending it, because S3 would not store it as given. Examples are a metadata value with a line break, two metadata names that differ only in case, an empty range, and a key with a `.` or `..` segment, which a URL cannot hold. `parameter` names the option.
- `ProtocolError`: an answer broke the protocol, such as a body shorter than its `Content-Length`, or a truncated listing without a continuation token.

## Behavior worth knowing

- Metadata values outside printable ASCII, or with spaces at either end, are sent as an RFC 2047 encoded word, which S3 decodes and stores. Encoded words in values that S3 returns are decoded.
- A `get` returns the bytes as S3 stores them, also for an object with a `Content-Encoding` such as `gzip`. The client asks Cloudflare Workers (`encodeResponseBody: "manual"`) and Bun (`decompress: false`) not to decode them. Node's `fetch` has no such option and decodes such a body.
- Content properties outside ASCII, such as a `Content-Disposition` filename, are sent as UTF-8. workerd writes a header string as UTF-8 and the other runtimes as one byte per character, so the client checks which one it runs on.
- Listings ask for `encoding-type=url` and decode the names.

## Tests

`object-tests/grade.sh` grades the client against [borink-org/object-tests](https://github.com/borink-org/object-tests): its recorded S3 answers, the SigV4 vectors, and bodies of gigabytes. It runs every suite twice, once in Bun and once inside workerd, the runtime of Cloudflare Workers. `object-tests/expected-unsupported.json` lists the cases outside the client's scope.

```nu
typescript/object-tests/grade.sh
```

It needs Bun, a Rust toolchain for the grader, and `bun install` in this directory, which also installs workerd.
