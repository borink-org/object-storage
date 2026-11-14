// The object-tests adapter of the TypeScript S3 client, as a worker that
// workerd runs. `forward.ts` posts each grader message to it, and it answers
// with the result, so that the client runs in the Workers runtime itself.

import { connect } from "cloudflare:sockets";
import { execute, type Call } from "../execute.ts";

/** Copies a body into the grader's socket at `address`, and returns its length. */
async function sink(body: ReadableStream<Uint8Array>, address: string): Promise<number> {
  const socket = connect(address);
  let length = 0;
  await body
    .pipeThrough(
      new TransformStream<Uint8Array, Uint8Array>({
        transform(chunk, controller) {
          length += chunk.length;
          controller.enqueue(chunk);
        },
      }),
    )
    .pipeTo(socket.writable);
  await socket.close();
  return length;
}

export default {
  async fetch(request: Request): Promise<Response> {
    try {
      const result = await execute((await request.json()) as Call, {
        fetch: (url, init) => fetch(url, init),
        liveCredentials: () => {
          throw new Error("the workerd adapter grades offline suites alone");
        },
        sink,
      });
      return Response.json(result);
    } catch (error) {
      return new Response(String(error instanceof Error ? error.stack : error), { status: 500 });
    }
  },
};
