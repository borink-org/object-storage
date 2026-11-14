// The errors a client throws.

/** What a failed request says about the object and its bucket. */
export type S3ErrorKind =
  /** The object, or the version of it, is absent. */
  | "not_found"
  /** The bucket is absent. */
  | "bucket_not_found"
  /**
   * The answer was 404 without a body, as to a HEAD, so it cannot say
   * whether the object or the bucket is absent.
   */
  | "missing"
  /** An `If-Match` or `If-Unmodified-Since` condition failed. */
  | "precondition"
  /** An `If-None-Match` or `If-Modified-Since` condition held on a read. */
  | "not_modified"
  /** The credentials or the signature were refused, or access was denied. */
  | "permission_denied"
  | "other";

/** S3 refused a request. */
export class S3Error extends Error {
  override name = "S3Error";

  constructor(
    readonly status: number,
    readonly kind: S3ErrorKind,
    /** The error code S3 named in its answer, such as `NoSuchKey`. */
    readonly code: string | undefined,
    message: string | undefined,
    /** The `x-amz-request-id` of the answer. */
    readonly requestId: string | undefined,
  ) {
    super(`${status}${code ? ` ${code}` : ""}${message ? `: ${message}` : ""}`);
  }
}

/** The client refused a call before it sent anything. */
export class RefusedError extends Error {
  override name = "RefusedError";

  constructor(
    /** The option or argument that the client refused, such as `range`. */
    readonly parameter: string,
    message: string,
  ) {
    super(`${parameter}: ${message}`);
  }
}

/** An answer that does not follow the protocol, such as a cut-short body. */
export class ProtocolError extends Error {
  override name = "ProtocolError";
}
