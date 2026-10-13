//! Azure Blob Storage for a C or C++ program that owns its memory and its I/O.
//!
//! This crate writes HTTP requests and reads HTTP response heads. It never
//! opens a socket, never reads the clock, and never allocates. Your program
//! supplies the buffers, the current time and the HTTP client.
//!
//! Include `borink/object_storage.h` and link the archive. A C++ program can
//! include `borink/object_storage.hpp` instead, which adds inline helpers over
//! the same declarations and needs no C++ runtime library.
//!
//! The calls mirror the `azure` module of the core crate,
//! `borink-object-storage-proto`, for reads, whole-object writes, writes in
//! blocks and listings. Its documentation says what each one sends and
//! refuses.
//!
//! # How a read works
//!
//! A read has four steps.
//!
//! 1. Fill in a `borink_container` with the endpoint and the container's
//!    name, and a `borink_get_options` with what every read does alike.
//!    Call `borink_prepare_get` with them and the token to write the
//!    template into a buffer of yours. Do this once, and keep the
//!    `borink_template` it returns beside the bytes it wrote: `required` of
//!    them, from the start of the buffer.
//! 2. Describe one read as a `borink_get_item`, and call `borink_encode_get`
//!    with the template, its bytes, the `borink_timestamps` of the current
//!    second and a second buffer. `borink_timestamps_from_unix` writes those
//!    once a second. It returns a `borink_wire_request`, which names the URL,
//!    the head and its header fields by offset into that buffer. Send them
//!    with your HTTP client; `borink_next_header` reads the fields one by
//!    one.
//! 3. Keep the read's `borink_get_shape` while the request is in flight.
//!    `borink_get_item_shape` returns it, and it holds no pointer.
//! 4. Name each response header with a `borink_header_ref` and call
//!    `borink_accept_get_head` with the shape. It returns a `borink_outcome`,
//!    and its `kind` says which outcome that is.
//!
//! A write has the same steps, with `borink_prepare_put`, `borink_encode_put`
//! and `borink_accept_put_head`. A write in blocks stages each block with
//! `borink_encode_stage_block` and commits them with
//! `borink_encode_commit_blocks`, whose block list is the request's body.
//!
//! A listing has one step more. Its head's kind is `Page`, so read the whole
//! response body into one buffer, and pass that buffer to
//! `borink_fill_listing`. It writes the entries of the page into your array
//! and names the page after this one. The array must hold the whole page; an
//! array of `max_results` entries always does.
//!
//! An outcome whose kind is `NeedErrorBody` is not final: Azure named no
//! error in the head. Read a bounded error body, and pass it with the
//! `failure` of that outcome to the operation's `borink_accept_*_error_body`.
//!
//! # Example
//!
//! ```c
//! borink_container container = {as_bytes(endpoint), as_bytes(name)};
//! borink_get_options options = {BORINK_GET_KIND_BYTES, BORINK_RANGING_WHOLE,
//!                               BORINK_CONDITION_NONE, BORINK_REVISION_KIND_CURRENT};
//! borink_prepared reads = borink_prepare_get(&container, as_bytes(token),
//!                                            BORINK_AZURE_NAMESPACE_UNKNOWN, &options,
//!                                            template_buffer);
//! if (reads.status.code != 0) { /* ... */ }
//! borink_bytes template_bytes = {template_buffer.ptr, reads.required};
//!
//! borink_timestamps now = borink_timestamps_from_unix(unix_seconds); // once a second
//! borink_get_item item = {key, {BORINK_RANGE_FORM_WHOLE, 0, 0}};
//! borink_wire_request request =
//!     borink_encode_get(&reads.template_, template_bytes, &item, &now, buffer);
//! if (request.status.code == BORINK_ERROR_CODE_CAPACITY) { /* grow to request.required */ }
//!
//! // ... send request.url and each header with your HTTP client ...
//!
//! borink_outcome outcome = borink_accept_get_head(
//!     borink_get_item_shape(&item, &options), status, headers, header_count);
//! if (outcome.kind == BORINK_OUTCOME_KIND_BODY) { /* read the body */ }
//! ```
//!
//! # Sizing the buffers
//!
//! A prepare or encode call refuses a buffer that is too small. It reports
//! `Capacity` in `status`, with the bytes it needs in `required`. Pass an
//! empty buffer to learn the size, and reuse a buffer of the largest.
//!
//! # Where each value lives
//!
//! A template is plain bytes: the `borink_template` value and the bytes it
//! was written into, both yours. Neither holds a pointer, so you may copy
//! them, or store them and read them back. `borink_template_check` checks
//! bytes once where they could be another template's. A `borink_timestamps`
//! is plain bytes too.
//!
//! The URL, the head and the body of a request are in your request buffer,
//! addressed by offsets. Resizing that buffer moves the bytes; the offsets
//! still address them. The content of a write stays where you keep it, and
//! you send it after the head.
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
//! No call returns a Rust `Result`, and nothing here throws.
//! `borink_container_check` and `borink_template_check` return a
//! `borink_status`, and `borink_prepared`, `borink_wire_request` and
//! `borink_outcome` each carry one.
//!
//! A status is two numbers. `code` is a `borink_error_code`, and `detail` is
//! the discriminant of the value inside it. Both are the core crate's own, and
//! `borink_describe_status` writes the sentence for a pair.
//!
//! A status names the part of the exchange that was wrong, not the value that
//! was wrong. You passed the values in, so read those to find the value.
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
//! Every entry point that takes a pointer is an `unsafe extern "C"` function
//! with a `# Safety` section naming what it requires. A null container,
//! template, options, item or timestamps is refused as `InvalidPlan`, never
//! dereferenced. A `borink_bytes` whose `len` is 0 may have a null `ptr`.

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
// compiler checks `layout` against it. Every read of a caller's pointer is in
// `ptr`, and every call of one is in `entry` and `layout`. The logic is in
// `step`, `plan`, `outcome` and `sentence`, which forbid `unsafe` code.
mod entry;
mod layout;
mod outcome;
mod plan;
mod ptr;
mod sentence;
mod step;
mod types;

#[cfg(test)]
mod tests;

pub use self::entry::*;
pub use self::layout::*;
pub use self::types::*;
