//! Azure Blob Storage for a C or C++ program that owns its memory and its I/O.
//!
//! This crate builds HTTP request heads and reads HTTP response heads. It
//! never opens a socket, never reads the clock, and never allocates. Your
//! program supplies the buffer, the current time and the HTTP client.
//!
//! Include `borink/object_storage.h` and link the archive. A C++ program can
//! include `borink/object_storage.hpp` instead, which adds inline helpers over
//! the same declarations and needs no C++ runtime library.
//!
//! # How a read works
//!
//! A read has four steps.
//!
//! 1. Fill in a `borink_session` with the endpoint, the container and the
//!    token. Your program owns those bytes and keeps them.
//! 2. Describe the read as a `borink_get_shape`, and keep it while the request
//!    is in flight. It holds no pointer, so it outlives the key and ETag
//!    bytes.
//! 3. Call `borink_encode_get` to write the request head into your buffer. It
//!    returns a `borink_request_head`, which names the URL and each header by
//!    offset and length into that buffer. Send them with your HTTP client.
//! 4. Name each response header with a `borink_header_ref` and call
//!    `borink_accept_get_head` with the same `borink_get_shape`. It returns a
//!    `borink_outcome`, and its `kind` says which outcome that is.
//!
//! Pass the same shape to steps 3 and 4. The second call checks the response
//! against the plan, so you never restate what the shape already holds.
//!
//! A write has the same four steps, with `borink_put_shape`,
//! `borink_encode_put` and `borink_accept_put_head`. A removal has them with
//! `borink_delete_shape`, `borink_encode_delete` and
//! `borink_accept_delete_head`.
//!
//! An outcome whose kind is `NeedErrorBody` is not final. Azure named
//! no error in the head, so read a bounded error body and pass it, with the
//! `failure` of that outcome, to `borink_finish_get_error_body`.
//!
//! # Example
//!
//! ```c
//! borink_session session = {as_bytes(endpoint), as_bytes(container), as_bytes(token)};
//! borink_status opened = borink_validate(&session);
//! if (opened.code != 0) { /* ... */ }
//!
//! borink_get_shape shape = {BORINK_GET_KIND_BYTES, {BORINK_RANGE_FORM_WHOLE, 0, 0},
//!                           BORINK_CONDITION_NONE};
//! borink_request_head head =
//!     borink_encode_get(&session, &shape, key, no_bytes, buffer, now);
//! if (head.status.code == BORINK_ERROR_CODE_CAPACITY) { /* grow to head.required */ }
//!
//! // ... send head.url and head.headers with your HTTP client ...
//!
//! borink_outcome outcome =
//!     borink_accept_get_head(&session, &shape, status, headers, header_count);
//! if (outcome.kind == BORINK_OUTCOME_KIND_BODY) { /* read the body */ }
//! ```
//!
//! # Sizing the buffer
//!
//! `borink_encode_get` refuses a buffer that is too small. It reports
//! `Capacity` in `status`, and the number of bytes it needs in `required`.
//! Call it with an empty buffer to learn that number, then size one buffer per
//! session and reuse it.
//!
//! # Where each value lives
//!
//! The request head is in your buffer, so `borink_request_head` names its
//! parts by offset rather than by pointer. Resizing the buffer moves the
//! bytes; the offsets still address them.
//!
//! The response head stays wherever your HTTP library put it. A
//! `borink_header_ref` points at those bytes, and every borrowed field of the
//! outcome points into the same bytes. This crate copies no part of a head,
//! and requires no particular layout of one.
//!
//! Each borrowed field states under its own `# Lifetime` when it stops being
//! valid.
//!
//! # Reading a failure
//!
//! No call returns a Rust `Result`, and nothing here throws. `borink_validate`
//! returns a `borink_status`, and `borink_request_head` and `borink_outcome`
//! each carry one.
//!
//! A status is two numbers. `code` is a `borink_error_code`, and `detail` is
//! the discriminant of the value inside it. Both are the core crate's own, and
//! `borink_describe_status` writes the sentence for a pair.
//!
//! A status names the part of the exchange that was wrong, not the value that
//! was wrong. You passed the headers and the shape in, so read those to find
//! the value.
//!
//! A response that Azure sends in normal operation is not a failure. A missing
//! object, a failed condition and a throttle each arrive as an outcome
//! kind.
//!
//! Every other enum crosses as the number the core crate gives it, in both
//! directions. A number that this crate does not define is refused as
//! `InvalidPlan`, not read as another value.
//!
//! # What it costs
//!
//! Nothing here allocates, and nothing here reads a clock. Every call is total
//! over its inputs, so no call panics and none unwinds into your program.
//!
//! # Passing pointers
//!
//! Every entry point takes raw pointers, so each is an `unsafe extern "C"`
//! function with a `# Safety` section naming what it requires. A null
//! `session` or `shape` is refused as `InvalidPlan`, never dereferenced. A
//! `borink_bytes` whose `len` is 0 may have a null `ptr`.

#![no_std]

mod panic;

/// Links the standard library, for the panic handler and the unwinder that a
/// static archive carries on a hosted target, and for the test harness.
///
/// Nothing in this crate calls into it.
#[cfg(any(feature = "std", test))]
extern crate std;

// The source, in the order a reviewer might take it. `types` and `layout` need
// no line-by-line reading: cbindgen generates the header from `types`, and a C
// compiler checks `layout` against it. `ptr` holds every read of a caller's
// pointer. The logic is in `step`, `convert` and `sentence`, and `entry` is the
// shape they are called in.
mod convert;
mod entry;
mod layout;
mod ptr;
mod sentence;
mod step;
mod types;

#[cfg(test)]
mod tests;

pub use self::entry::*;
pub use self::layout::*;
pub use self::types::*;
