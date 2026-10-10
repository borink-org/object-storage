// The borink-org/object-tests adapter of the TypeScript S3 client, in Bun.
// The grader starts it once per case, writes one message to its standard
// input, and reads one result from its standard output.

import { connect } from "node:net";
import { execute, type Call } from "./execute.ts";

/** Copies a body into the grader's socket at `address`, and returns its length. */
async function sink(body: ReadableStream<Uint8Array>, address: string): Promise<number> {
  const colon = address.lastIndexOf(":");
  const socket = connect(Number(address.slice(colon + 1)), address.slice(0, colon));
  await new Promise<void>((resolve, reject) => socket.once("connect", resolve).once("error", reject));
  let length = 0;
  for await (const chunk of body) {
    length += chunk.length;
    if (!socket.write(chunk)) {
      await new Promise<void>((resolve) => socket.once("drain", resolve));
    }
  }
  await new Promise<void>((resolve) => socket.end(resolve));
  return length;
}

try {
  const message = JSON.parse(await Bun.stdin.text()) as Call;
  const result = await execute(message, {
    fetch: (url, init) => fetch(url, init),
    liveCredentials: () => ({
      accessKeyId: process.env.AWS_ACCESS_KEY_ID!,
      secretAccessKey: process.env.AWS_SECRET_ACCESS_KEY!,
      ...(process.env.AWS_SESSION_TOKEN !== undefined && {
        sessionToken: process.env.AWS_SESSION_TOKEN,
      }),
    }),
    sink,
  });
  console.log(JSON.stringify(result));
} catch (error) {
  console.error(error);
  process.exit(2);
}
