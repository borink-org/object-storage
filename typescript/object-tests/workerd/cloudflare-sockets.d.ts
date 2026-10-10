// The part of the `cloudflare:sockets` module that the worker uses.
declare module "cloudflare:sockets" {
  export interface Socket {
    readonly writable: WritableStream<Uint8Array>;
    close(): Promise<void>;
  }
  export function connect(address: string): Socket;
}
