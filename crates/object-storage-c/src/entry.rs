//! The functions a C program calls.
//!
//! Each one reads every pointer it was passed in one `unsafe` block, under
//! the `# Safety` contract written above it. It hands the Rust values to
//! [`crate::step`], [`crate::inputs`] and [`crate::outcome`]. Those modules
//! forbid `unsafe` code, so what a pointer read needs is checked here and
//! nowhere else.
//!
//! The functions mirror the protocol crate's `azure` module: `borink_prepare_get`
//! is `azure::prepare_get`, `borink_encode_get` is `azure::encode_get`, and
//! so on for each operation.

use crate::inputs::{self, UNKNOWN};
use crate::outcome::{
    bytes, commit_outcome, get_outcome, invalid, list_outcome, maybe_bytes, maybe_number,
    properties_view, property_view, put_outcome, refused_fill, stage_outcome, status_of,
};
use crate::ptr;
use crate::sentence::{describe, describe_status};
use crate::step::{self, filling, filling_with, head_of, prepared, written};
use crate::types::*;

use borink_object_storage_proto::{self as proto, azure};

/// Reports what is wrong with `container`, if anything: what
/// `Container::new` refuses in the protocol crate.
///
/// A `code` of 0 means that a template can be prepared for it. Every
/// `borink_prepare_*` call makes the same check, so this exists to fail
/// early. The key of `object`, and the token, are checked when a template is
/// prepared.
///
/// # Safety
///
/// `container` must be null or point at one readable value whose values each
/// address their stated length.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_container_check(container: *const Container) -> Status {
    // SAFETY: the caller states the contract of this function.
    let container = unsafe { ptr::container(container) };
    step::container(container).map_or_else(|error| status_of(&error), |_| Status::default())
}

/// Returns the instant `unix_seconds` after the Unix epoch, formatted for the
/// dates that a request carries: `Timestamps::from_unix` in the protocol crate.
///
/// Call it once a second, and pass the result to every request of that
/// second. A value after `9999-12-31 23:59:59 UTC` becomes that instant.
#[unsafe(no_mangle)]
pub extern "C" fn borink_timestamps_from_unix(unix_seconds: u64) -> Timestamps {
    Timestamps {
        stored: proto::Timestamps::from_unix(unix_seconds).to_bytes(),
    }
}

/// Looks up Azure's corresponding rejection for a local validation failure.
///
/// `namespace` is a `borink_azure_namespace`. Unknown discriminants, other
/// error kinds and unmapped reasons return a zero status and empty code.
/// No request was sent; authentication or another fault could take precedence.
#[unsafe(no_mangle)]
pub extern "C" fn borink_azure_rejection_for(error: Status, namespace: u16) -> AzureRejection {
    let Ok(namespace) = step::namespace(namespace) else {
        return AzureRejection::default();
    };
    let reason = proto::ErrorCode::from_discriminant(error.code)
        .and_then(|code| proto::Error::from_parts(code, error.detail));
    let Some(proto::Error::InvalidRequest(reason)) = reason else {
        return AzureRejection::default();
    };
    reason
        .azure_rejection(namespace)
        .map_or_else(AzureRejection::default, |rejection| AzureRejection {
            status: rejection.status,
            code: bytes(rejection.code.as_bytes()),
        })
}

/// Writes the template of reads with `options` from `container`, sent with
/// the bearer `token`, into `buf`.
///
/// `token` is an Entra ID token, without the `Bearer ` prefix. The template
/// holds it, so a new token means preparing again.
///
/// `namespace` is a `borink_azure_namespace`: the kind of account the reads
/// go to. Pass `Unknown` if you do not know it, and each read refuses only
/// the keys that both kinds refuse.
///
/// Pass an empty `buf` to learn the size that the template needs. A ranged
/// HEAD is refused with `RangedHead`.
///
/// # Safety
///
/// `container` and `options` must each be null or point at one readable
/// value. Each value of `container`, and `token`, must address its stated
/// length. `buf` must address its stated length and be reached through
/// nothing else during the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_prepare_get(
    container: *const Container,
    token: Bytes,
    namespace: u16,
    options: *const GetOptions,
    buf: BytesMut,
) -> Prepared {
    // SAFETY: the caller states the contract of this function.
    let (container, token, options, buf) = unsafe {
        (
            ptr::container(container),
            ptr::slice(token),
            options.as_ref(),
            ptr::slice_mut(buf),
        )
    };
    prepared(step::container(container).and_then(|container| {
        let options = inputs::get_options(options.ok_or(UNKNOWN)?)?;
        azure::prepare_get(
            &container,
            step::token(token)?,
            step::namespace(namespace)?,
            &options,
            buf,
        )
    }))
}

/// Writes the template of whole-object writes with `options` into `buf`.
///
/// Every write sends `x-ms-blob-type: BlockBlob`, and the content properties
/// and the access tier of `options`. A checksum other than an MD5 or a CRC64
/// is refused with `Option`.
///
/// # Safety
///
/// As `borink_prepare_get`, and each value of `options` must address its
/// stated length.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_prepare_put(
    container: *const Container,
    token: Bytes,
    namespace: u16,
    options: *const PutOptions,
    buf: BytesMut,
) -> Prepared {
    // SAFETY: the caller states the contract of this function.
    let (container, token, options, buf) = unsafe {
        (
            ptr::container(container),
            ptr::slice(token),
            options
                .as_ref()
                .map(|options| (options, ptr::write_options(&options.options))),
            ptr::slice_mut(buf),
        )
    };
    prepared(step::container(container).and_then(|container| {
        let (options, write) = options.ok_or(UNKNOWN)?;
        let options = inputs::put_options(options, write)?;
        azure::prepare_put(
            &container,
            step::token(token)?,
            step::namespace(namespace)?,
            &options,
            buf,
        )
    }))
}

/// Writes the template of Put Blocks with `options` into `buf`.
///
/// A checksum other than an MD5 or a CRC64 is refused with `Option`.
///
/// # Safety
///
/// As `borink_prepare_get`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_prepare_stage_block(
    container: *const Container,
    token: Bytes,
    namespace: u16,
    options: *const StageOptions,
    buf: BytesMut,
) -> Prepared {
    // SAFETY: the caller states the contract of this function.
    let (container, token, options, buf) = unsafe {
        (
            ptr::container(container),
            ptr::slice(token),
            options.as_ref(),
            ptr::slice_mut(buf),
        )
    };
    prepared(step::container(container).and_then(|container| {
        let options = inputs::stage_options(options.ok_or(UNKNOWN)?)?;
        azure::prepare_stage_block(
            &container,
            step::token(token)?,
            step::namespace(namespace)?,
            &options,
            buf,
        )
    }))
}

/// Writes the template of Put Block Lists with `options` into `buf`.
///
/// # Safety
///
/// As `borink_prepare_put`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_prepare_commit_blocks(
    container: *const Container,
    token: Bytes,
    namespace: u16,
    options: *const CommitOptions,
    buf: BytesMut,
) -> Prepared {
    // SAFETY: the caller states the contract of this function.
    let (container, token, options, buf) = unsafe {
        (
            ptr::container(container),
            ptr::slice(token),
            options
                .as_ref()
                .map(|options| (options, ptr::write_options(&options.options))),
            ptr::slice_mut(buf),
        )
    };
    prepared(step::container(container).and_then(|container| {
        let (options, write) = options.ok_or(UNKNOWN)?;
        let options = inputs::commit_options(options, write)?;
        azure::prepare_commit_blocks(
            &container,
            step::token(token)?,
            step::namespace(namespace)?,
            &options,
            buf,
        )
    }))
}

/// Writes the template of the pages of a listing with `options` into `buf`.
///
/// A page of no entries is refused with `MaxResults`, and a delimiter other
/// than `/` on a hierarchical account with `Delimiter`. A container that names
/// one object is refused with `Option`: a listing names none.
///
/// # Safety
///
/// As `borink_prepare_get`, and `options->delimiter` must address its stated
/// length.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_prepare_list(
    container: *const Container,
    token: Bytes,
    namespace: u16,
    options: *const ListOptions,
    buf: BytesMut,
) -> Prepared {
    // SAFETY: the caller states the contract of this function.
    let (container, token, options, buf) = unsafe {
        (
            ptr::container(container),
            ptr::slice(token),
            options
                .as_ref()
                .map(|options| (options, ptr::slice(options.delimiter))),
            ptr::slice_mut(buf),
        )
    };
    prepared(step::container(container).and_then(|container| {
        let (options, delimiter) = options.ok_or(UNKNOWN)?;
        let options = inputs::list_options(options, delimiter)?;
        azure::prepare_list(
            &container,
            step::token(token)?,
            step::namespace(namespace)?,
            &options,
            buf,
        )
    }))
}

/// Checks that `bytes` are the bytes that `template` was written into.
///
/// Encoding trusts that they are, and checks nothing of them per request.
/// Check bytes once where they could be another template's, such as bytes
/// read back from storage. A `code` of 0 means that they are the template's
/// own, and `InvalidRequest` with `Template` that they are not.
///
/// # Safety
///
/// `template` must be null or point at one readable value. `bytes` must
/// address its stated length.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_template_check(template: *const Template, bytes: Bytes) -> Status {
    // SAFETY: the caller states the contract of this function.
    let (template, bytes) = unsafe { (template.as_ref(), ptr::slice(bytes)) };
    step::template(template)
        .and_then(|template| template.check(bytes))
        .map_or_else(|error| status_of(&error), |()| Status::default())
}

/// Writes the read `item` at `now` from `template` into `buf`.
///
/// `bytes` are the template's own, as `borink_prepare_get` wrote them. Pass
/// an empty `buf` to learn the size that the request needs.
///
/// A key that Azure refuses, a range that the template does not take and a
/// suffix range are refused with `InvalidRequest`. A template of another
/// operation is refused with `Template`.
///
/// # Safety
///
/// `template`, `item` and `now` must each be null or point at one readable
/// value, and each value of `item` must address its stated length. `bytes` must
/// address its stated length. `buf` must address its stated length and be
/// reached through nothing else during the call: not through `bytes`, nor
/// through a value of `item`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_encode_get(
    template: *const Template,
    bytes: Bytes,
    item: *const GetItem,
    now: *const Timestamps,
    buf: BytesMut,
) -> WireRequest {
    // SAFETY: the caller states the contract of this function.
    let (template, bytes, item, now, buf) = unsafe {
        (
            template.as_ref(),
            ptr::slice(bytes),
            item.as_ref().map(|item| {
                (
                    item,
                    ptr::slice(item.key),
                    ptr::slice(item.condition_value.e_tag),
                    ptr::slice(item.revision),
                )
            }),
            now.as_ref(),
            ptr::slice_mut(buf),
        )
    };
    written(step::template(template).and_then(|template| {
        let (item, key, e_tag, revision) = item.ok_or(UNKNOWN)?;
        let item = inputs::get_item(item, key, e_tag, revision)?;
        azure::encode_get(&template, bytes, &item, &step::timestamps(now)?, buf)
    }))
}

/// Writes the whole-object write `item` at `now` from `template`
/// into `buf`.
///
/// The head states `item->content_len`, and you send that content after
/// it. Content longer than Azure takes in one write is refused with
/// `PayloadTooLarge`. A checksum that is not the base64 of a digest of the
/// template's kind is refused with `Checksum`.
///
/// # Safety
///
/// As `borink_encode_get`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_encode_put(
    template: *const Template,
    bytes: Bytes,
    item: *const PutItem,
    now: *const Timestamps,
    buf: BytesMut,
) -> WireRequest {
    // SAFETY: the caller states the contract of this function.
    let (template, bytes, item, now, buf) = unsafe {
        (
            template.as_ref(),
            ptr::slice(bytes),
            item.as_ref().map(|item| {
                (
                    item,
                    ptr::slice(item.key),
                    ptr::slice(item.condition_value.e_tag),
                    ptr::slice(item.checksum),
                )
            }),
            now.as_ref(),
            ptr::slice_mut(buf),
        )
    };
    written(step::template(template).and_then(|template| {
        let (item, key, e_tag, checksum) = item.ok_or(UNKNOWN)?;
        let item = inputs::put_item(item, key, e_tag, checksum)?;
        azure::encode_put(&template, bytes, &item, &step::timestamps(now)?, buf)
    }))
}

/// Writes the Put Block `item` at `now` from `template` into `buf`.
///
/// The head states `item->content_len`, and you send that block after it. A
/// block ID that is not base64 of at most 64 bytes is refused with `PartId`.
///
/// # Safety
///
/// As `borink_encode_get`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_encode_stage_block(
    template: *const Template,
    bytes: Bytes,
    item: *const StageBlockItem,
    now: *const Timestamps,
    buf: BytesMut,
) -> WireRequest {
    // SAFETY: the caller states the contract of this function.
    let (template, bytes, item, now, buf) = unsafe {
        (
            template.as_ref(),
            ptr::slice(bytes),
            item.as_ref().map(|item| {
                (
                    item,
                    ptr::slice(item.key),
                    ptr::slice(item.id),
                    ptr::slice(item.checksum),
                )
            }),
            now.as_ref(),
            ptr::slice_mut(buf),
        )
    };
    written(step::template(template).and_then(|template| {
        let (item, key, id, checksum) = item.ok_or(UNKNOWN)?;
        let item = inputs::stage_item(item, key, id, checksum)?;
        azure::encode_stage_block(&template, bytes, &item, &step::timestamps(now)?, buf)
    }))
}

/// Writes the Put Block List `item` at `now` from `template` into
/// `buf`, with its block list as the body after the head.
///
/// More than 50,000 blocks are refused with `Parts`, and a block ID that is
/// not base64 of at most 64 bytes with `PartId`.
///
/// # Safety
///
/// As `borink_encode_get`, and `item->blocks` must address
/// `item->block_count` readable values whose IDs each address their stated
/// length.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_encode_commit_blocks(
    template: *const Template,
    bytes: Bytes,
    item: *const CommitItem,
    now: *const Timestamps,
    buf: BytesMut,
) -> WireRequest {
    // SAFETY: the caller states the contract of this function.
    let (template, bytes, item, now, buf) = unsafe {
        (
            template.as_ref(),
            ptr::slice(bytes),
            item.as_ref().map(|item| {
                (
                    item,
                    ptr::slice(item.key),
                    ptr::slice(item.condition_value.e_tag),
                    ptr::slice(item.content_md5),
                    ptr::blocks(item.blocks, item.block_count),
                )
            }),
            now.as_ref(),
            ptr::slice_mut(buf),
        )
    };
    written(step::template(template).and_then(|template| {
        let (item, key, e_tag, content_md5, blocks) = item.ok_or(UNKNOWN)?;
        let item = inputs::commit_item(item, key, e_tag, content_md5, blocks)?;
        azure::encode_commit_blocks(&template, bytes, &item, &step::timestamps(now)?, buf)
    }))
}

/// Writes the page `item` of a listing at `now` from `template`
/// into `buf`.
///
/// An empty `prefix` lists the whole container. Pass an empty `marker` for
/// the first page, and the `next_marker` of the last fill for every page
/// after it.
///
/// # Safety
///
/// As `borink_encode_get`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_encode_list(
    template: *const Template,
    bytes: Bytes,
    item: *const ListItem,
    now: *const Timestamps,
    buf: BytesMut,
) -> WireRequest {
    // SAFETY: the caller states the contract of this function.
    let (template, bytes, item, now, buf) = unsafe {
        (
            template.as_ref(),
            ptr::slice(bytes),
            item.as_ref()
                .map(|item| (ptr::slice(item.prefix), ptr::slice(item.marker))),
            now.as_ref(),
            ptr::slice_mut(buf),
        )
    };
    written(step::template(template).and_then(|template| {
        let (prefix, marker) = item.ok_or(UNKNOWN)?;
        let item = inputs::list_item(prefix, marker)?;
        azure::encode_list(&template, bytes, &item, &step::timestamps(now)?, buf)
    }))
}

/// Reads the next header field of a request, and steps `fields` past it.
///
/// Start with the bytes that a request's `fields` addresses in your buffer.
/// Each call reports one name and its value, as the request wrote them.
/// Write each one, unchanged and none twice; your HTTP client writes `host`
/// from the URL. A walk that has ended reports an absent header, and stays
/// ended.
///
/// # Safety
///
/// `fields` must be null or point at one writable value that addresses its
/// stated length.
///
/// # Lifetime
///
/// The name and the value point into the bytes that `fields` addressed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_next_header(fields: *mut Bytes) -> Header {
    // SAFETY: the caller states the contract of this function.
    let Some(fields) = (unsafe { fields.as_mut() }) else {
        return Header::default();
    };
    // SAFETY: as above, for the bytes that the walk has not read.
    let remaining = unsafe { ptr::slice(*fields) };
    match step::next_header(remaining) {
        Some((name, value, rest)) => {
            *fields = bytes(rest);
            Header {
                present: true,
                name: bytes(name),
                value: bytes(value),
            }
        }
        None => {
            *fields = Bytes::default();
            Header::default()
        }
    }
}

/// Returns the part of a read that holds no pointer, for a template with
/// `options`: its kind, its range and its condition.
///
/// Keep it while the request is in flight, and pass it to
/// `borink_accept_get_head` when the response arrives. A null `item` or
/// `options` gives a shape of zeros, which every call refuses.
///
/// # Safety
///
/// `item` and `options` must each be null or point at one readable value.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_get_item_shape(
    item: *const GetItem,
    options: *const GetOptions,
) -> GetShape {
    // SAFETY: the caller states the contract of this function.
    match unsafe { (item.as_ref(), options.as_ref()) } {
        (Some(item), Some(options)) => GetShape {
            kind: options.kind,
            range: item.range,
            condition: options.condition,
        },
        _ => GetShape {
            kind: 0,
            range: Range {
                form: 0,
                start: 0,
                end: 0,
            },
            condition: 0,
        },
    }
}

/// Returns the part of a write that holds no pointer, for a template with
/// `options`: its condition, which is the template's.
///
/// # Safety
///
/// As `borink_get_item_shape`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_put_item_shape(
    item: *const PutItem,
    options: *const PutOptions,
) -> PutShape {
    // SAFETY: the caller states the contract of this function.
    match unsafe { (item.as_ref(), options.as_ref()) } {
        (Some(_), Some(options)) => PutShape {
            condition: options.condition,
        },
        _ => PutShape { condition: 0 },
    }
}

/// Returns the part of a commit that holds no pointer, for a template with
/// `options`: its condition, which is the template's.
///
/// # Safety
///
/// As `borink_get_item_shape`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_commit_item_shape(
    item: *const CommitItem,
    options: *const CommitOptions,
) -> CommitShape {
    // SAFETY: the caller states the contract of this function.
    match unsafe { (item.as_ref(), options.as_ref()) } {
        (Some(_), Some(options)) => CommitShape {
            condition: options.condition,
        },
        _ => CommitShape { condition: 0 },
    }
}

/// Reads the response head of a read.
///
/// Pass the `shape` of the read, and one `borink_header_ref` per response
/// header. The outcome points into the same bytes as those headers.
///
/// # Safety
///
/// `headers` must address `header_count` readable values, each of whose
/// values addresses its stated length.
///
/// # Lifetime
///
/// The bytes that `headers` points at must stay valid, and must not move, for
/// as long as you use the returned outcome.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_accept_get_head(
    shape: GetShape,
    status: u16,
    headers: *const HeaderRef,
    header_count: usize,
) -> Outcome {
    // SAFETY: the caller states the contract of this function.
    let headers = unsafe { ptr::headers(headers, header_count) };
    inputs::get_shape(&shape)
        .and_then(|shape| azure::accept_get_head(shape, head_of(status, headers)))
        .map_or_else(invalid, |outcome| get_outcome(&outcome))
}

/// Finishes a read whose head asked for the error body.
///
/// Pass the `shape` that the head was read against, the `failure` of that
/// outcome and the body that you read. Pass an empty body if you read none:
/// the outcome is then final with the error unnamed.
///
/// # Safety
///
/// `failure.request_id` and `body` must each address their stated length.
///
/// # Lifetime
///
/// `failure.request_id` must still point at valid bytes, and they must stay
/// valid for as long as you use the returned outcome. So must `body`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_accept_get_error_body(
    shape: GetShape,
    failure: Failure,
    body: Bytes,
) -> Outcome {
    // SAFETY: the caller states the contract of this function.
    let (failure, body) = unsafe { (ptr::failure(&failure), ptr::slice(body)) };
    inputs::get_shape(&shape)
        .and_then(|shape| {
            Ok(azure::accept_get_error_body(
                shape,
                step::failure(failure)?,
                body,
            ))
        })
        .map_or_else(invalid, |outcome| get_outcome(&outcome))
}

/// Reads the response head of a whole-object write.
///
/// # Safety
///
/// As `borink_accept_get_head`.
///
/// # Lifetime
///
/// As `borink_accept_get_head`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_accept_put_head(
    shape: PutShape,
    status: u16,
    headers: *const HeaderRef,
    header_count: usize,
) -> Outcome {
    // SAFETY: the caller states the contract of this function.
    let headers = unsafe { ptr::headers(headers, header_count) };
    inputs::put_shape(&shape)
        .and_then(|shape| azure::accept_put_head(shape, head_of(status, headers)))
        .map_or_else(invalid, |outcome| put_outcome(&outcome))
}

/// Finishes a whole-object write whose head asked for the error body.
///
/// # Safety
///
/// As `borink_accept_get_error_body`.
///
/// # Lifetime
///
/// As `borink_accept_get_error_body`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_accept_put_error_body(
    shape: PutShape,
    failure: Failure,
    body: Bytes,
) -> Outcome {
    // SAFETY: the caller states the contract of this function.
    let (failure, body) = unsafe { (ptr::failure(&failure), ptr::slice(body)) };
    inputs::put_shape(&shape)
        .and_then(|shape| {
            Ok(azure::accept_put_error_body(
                shape,
                step::failure(failure)?,
                body,
            ))
        })
        .map_or_else(invalid, |outcome| put_outcome(&outcome))
}

/// Reads the response head of a Put Block.
///
/// This call takes no shape: a Put Block carries no condition.
///
/// # Safety
///
/// As `borink_accept_get_head`.
///
/// # Lifetime
///
/// As `borink_accept_get_head`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_accept_stage_block_head(
    status: u16,
    headers: *const HeaderRef,
    header_count: usize,
) -> Outcome {
    // SAFETY: the caller states the contract of this function.
    let headers = unsafe { ptr::headers(headers, header_count) };
    azure::accept_stage_block_head(head_of(status, headers))
        .map_or_else(invalid, |outcome| stage_outcome(&outcome))
}

/// Finishes a Put Block whose head asked for the error body.
///
/// # Safety
///
/// As `borink_accept_get_error_body`.
///
/// # Lifetime
///
/// As `borink_accept_get_error_body`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_accept_stage_block_error_body(
    failure: Failure,
    body: Bytes,
) -> Outcome {
    // SAFETY: the caller states the contract of this function.
    let (failure, body) = unsafe { (ptr::failure(&failure), ptr::slice(body)) };
    step::failure(failure)
        .map(|failure| azure::accept_stage_block_error_body(failure, body))
        .map_or_else(invalid, |outcome| stage_outcome(&outcome))
}

/// Reads the response head of a Put Block List.
///
/// # Safety
///
/// As `borink_accept_get_head`.
///
/// # Lifetime
///
/// As `borink_accept_get_head`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_accept_commit_blocks_head(
    shape: CommitShape,
    status: u16,
    headers: *const HeaderRef,
    header_count: usize,
) -> Outcome {
    // SAFETY: the caller states the contract of this function.
    let headers = unsafe { ptr::headers(headers, header_count) };
    inputs::commit_shape(&shape)
        .and_then(|shape| azure::accept_commit_blocks_head(shape, head_of(status, headers)))
        .map_or_else(invalid, |outcome| commit_outcome(&outcome))
}

/// Finishes a Put Block List whose head asked for the error body.
///
/// # Safety
///
/// As `borink_accept_get_error_body`.
///
/// # Lifetime
///
/// As `borink_accept_get_error_body`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_accept_commit_blocks_error_body(
    shape: CommitShape,
    failure: Failure,
    body: Bytes,
) -> Outcome {
    // SAFETY: the caller states the contract of this function.
    let (failure, body) = unsafe { (ptr::failure(&failure), ptr::slice(body)) };
    inputs::commit_shape(&shape)
        .and_then(|shape| {
            Ok(azure::accept_commit_blocks_error_body(
                shape,
                step::failure(failure)?,
                body,
            ))
        })
        .map_or_else(invalid, |outcome| commit_outcome(&outcome))
}

/// Reads the response head of a listing.
///
/// This call takes no shape: it checks nothing in the response against the
/// request.
///
/// # Safety
///
/// As `borink_accept_get_head`.
///
/// # Lifetime
///
/// As `borink_accept_get_head`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_accept_list_head(
    status: u16,
    headers: *const HeaderRef,
    header_count: usize,
) -> Outcome {
    // SAFETY: the caller states the contract of this function.
    let headers = unsafe { ptr::headers(headers, header_count) };
    azure::accept_list_head(head_of(status, headers))
        .map_or_else(invalid, |outcome| list_outcome(&outcome))
}

/// Finishes a listing whose head asked for the error body.
///
/// # Safety
///
/// As `borink_accept_get_error_body`.
///
/// # Lifetime
///
/// As `borink_accept_get_error_body`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_accept_list_error_body(failure: Failure, body: Bytes) -> Outcome {
    // SAFETY: the caller states the contract of this function.
    let (failure, body) = unsafe { (ptr::failure(&failure), ptr::slice(body)) };
    step::failure(failure)
        .map(|failure| azure::accept_list_error_body(failure, body))
        .map_or_else(invalid, |outcome| list_outcome(&outcome))
}

/// Reads a page out of the response body of a listing.
///
/// Pass the whole body that the `Page` outcome announced, and an array of
/// `capacity` entries to write it into. Reading is destructive. This call
/// decodes the text of the body where it stands, so a body that has been read
/// is no longer a document.
///
/// Your array must hold the whole page. An array of `max_results` entries
/// always does. A smaller one is refused with `Capacity`, and `required` says
/// how many entries the page holds; the body cannot be read again by then.
///
/// # Safety
///
/// `body` must address its stated length and be reached through nothing else
/// during the call. `into` must address `capacity` writable entries.
///
/// # Lifetime
///
/// Every entry, and `next_marker`, point into `body`. They are valid until you
/// release or reuse that buffer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_fill_listing(
    body: BytesMut,
    into: *mut ListEntry,
    capacity: usize,
) -> Fill {
    // SAFETY: the caller states the contract of this function.
    let (body, into) = unsafe { (ptr::slice_mut(body), ptr::items_mut(into, capacity)) };
    filling(body, into).unwrap_or_else(|error| refused_fill(&error))
}

/// Reads a page the way `borink_fill_listing` does, and keeps the values of
/// the properties in `wanted` as it goes.
///
/// `values` is your array of rows: one row per entry of `into`, each
/// `borink_property_set_len(wanted)` values long, so `capacity` rows in all.
/// The value that entry `i` gave for a property `p` is
/// `values[i * len + borink_property_slot(wanted, p)]`. It is absent where
/// the entry wrote no such element, and present and empty where it wrote the
/// element empty. A group of keys has every value absent. If `values` holds
/// fewer rows than `capacity`, that many rows is the capacity.
///
/// Reading the page costs the same as `borink_fill_listing`, whatever the
/// set holds.
///
/// # Safety
///
/// As `borink_fill_listing`, and `values` must address `value_capacity`
/// writable values.
///
/// # Lifetime
///
/// Every value points into `body`, like the entries.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_fill_listing_with(
    body: BytesMut,
    into: *mut ListEntry,
    capacity: usize,
    wanted: PropertySet,
    values: *mut MaybeBytes,
    value_capacity: usize,
) -> Fill {
    let wanted = proto::azure::PropertySet::from_bits(wanted.mask);
    let width = wanted.len();
    // Rows the values array has room for, when the set names anything.
    let rows = value_capacity
        .checked_div(width)
        .map_or(capacity, |held| capacity.min(held));
    // SAFETY: the caller states the contract of this function.
    let (body, into, values) = unsafe {
        (
            ptr::slice_mut(body),
            ptr::items_mut(into, rows),
            // width == 0 gives zero; otherwise rows <= value_capacity / width.
            ptr::items_mut(values, rows * width),
        )
    };
    filling_with(body, into, wanted, values).unwrap_or_else(|error| refused_fill(&error))
}

/// Adds a property to a set.
///
/// `property` is a `borink_blob_property`. A number that names no property
/// leaves the set as it was.
#[unsafe(no_mangle)]
pub extern "C" fn borink_property_set_with(set: PropertySet, property: u16) -> PropertySet {
    match blob_property(property) {
        Some(property) => PropertySet {
            mask: set.mask | proto::azure::PropertySet::of(&[property]).bits(),
        },
        None => set,
    }
}

/// Returns how many properties a set holds, which is how many values each
/// row of `borink_fill_listing_with` has.
#[unsafe(no_mangle)]
pub extern "C" fn borink_property_set_len(set: PropertySet) -> usize {
    proto::azure::PropertySet::from_bits(set.mask).len()
}

/// Returns where a property's value stands in a row: its rank among the
/// set's members, in the order of their numbers.
///
/// For a property the set does not hold, or a number that names none, this
/// is the length of the row: one past its last value.
#[unsafe(no_mangle)]
pub extern "C" fn borink_property_slot(set: PropertySet, property: u16) -> usize {
    let set = proto::azure::PropertySet::from_bits(set.mask);
    match blob_property(property) {
        Some(property) if set.contains(property) => set.slot(property),
        _ => set.len(),
    }
}

/// Returns the element name of a property, as the service writes it. Empty
/// for a number that names no property.
#[unsafe(no_mangle)]
pub extern "C" fn borink_property_name(property: u16) -> Bytes {
    let name = blob_property(property).map_or("", proto::azure::BlobProperty::name);
    Bytes {
        ptr: name.as_ptr(),
        len: name.len(),
    }
}

// The protocol crate's property with this number, which is its position in the
// list both crates are built from.
fn blob_property(number: u16) -> Option<proto::azure::BlobProperty> {
    proto::azure::BlobProperty::ALL
        .get(usize::from(number))
        .copied()
}

/// Returns the value that one entry gave for a property.
///
/// The name is matched exactly, against the elements of the entry and of its
/// properties element: `AccessTier` and `Creation-Time` on Azure. An absent
/// value means the entry wrote no such property, which is not the same fact as
/// a property it wrote empty.
///
/// Each call reads the entry again. Read more than one or two with
/// `borink_entry_properties`, which reads it once.
///
/// The three values that the entry carries are not read back this way. Reading
/// the page decoded them where they stood, so the element that held one now
/// holds the decoded text and what the decoding left behind. Read `key`,
/// `e_tag` and `last_modified` from the entry.
///
/// # Safety
///
/// `entry` must be null or point at one readable value whose `raw` addresses
/// its stated length. `name` must address its stated length.
///
/// # Lifetime
///
/// The value points into the body that the entry points into.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_entry_property(entry: *const ListEntry, name: Bytes) -> MaybeBytes {
    // SAFETY: the caller states the contract of this function.
    let (raw, name) = unsafe {
        match entry.as_ref() {
            Some(entry) => (ptr::slice(entry.raw), ptr::slice(name)),
            None => return MaybeBytes::default(),
        }
    };
    maybe_bytes(
        proto::Properties::new(raw)
            .find(|(found, _)| *found == name)
            .map(|(_, value)| value),
    )
}

/// Starts a walk over the values that one entry holds.
///
/// Step it with `borink_next_property`, which reports one value per call and
/// reads the entry once over the whole walk. A null `entry` starts a walk that
/// has already ended.
///
/// # Safety
///
/// As `borink_entry_property`.
///
/// # Lifetime
///
/// The walk points into the body that the entry points into.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_entry_properties(entry: *const ListEntry) -> Properties {
    // SAFETY: the caller states the contract of this function.
    let raw = unsafe {
        match entry.as_ref() {
            Some(entry) => ptr::slice(entry.raw),
            None => return Properties::default(),
        }
    };
    properties_view(proto::Properties::new(raw))
}

/// Reads the next value of a walk, and steps the walk past it.
///
/// An element that holds other elements, such as the metadata of an object,
/// reports those bytes as its value. The properties element is stepped into
/// rather than reported. A walk that has ended reports an absent value, and
/// stays ended.
///
/// # Safety
///
/// `walk` must be null or point at one writable `borink_properties` whose
/// `remaining` addresses its stated length.
///
/// # Lifetime
///
/// The value points into the body that the walk points into.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_next_property(walk: *mut Properties) -> Property {
    // SAFETY: the caller states the contract of this function.
    let Some(walk) = (unsafe { walk.as_mut() }) else {
        return Property::default();
    };
    // SAFETY: as above, for the bytes that the walk has not read.
    let mut reading =
        proto::Properties::from_parts(unsafe { ptr::slice(walk.remaining) }, walk.within);
    let found = property_view(reading.next());
    *walk = properties_view(reading);
    found
}

/// Writes the text of a listed value with its references resolved.
///
/// Use this on a value that `borink_entry_property` returned, which holds the
/// bytes that the service wrote. XML writes an `&` as `&amp;`, and a character
/// that the document cannot carry as `&#233;`.
///
/// Copies `value` into `into` and returns what it wrote, which is never longer
/// than `value`. An absent value means that `into` is shorter than `value`, or
/// that `value` holds a reference that no listing declares.
///
/// # Safety
///
/// `value` must address its stated length. `into` must address its stated
/// length and be reached through nothing else during the call.
///
/// # Lifetime
///
/// The bytes returned are `into`, so they are valid until you release or
/// reuse it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_decode_property(value: Bytes, into: BytesMut) -> MaybeBytes {
    // SAFETY: the caller states the contract of this function.
    let (value, into) = unsafe { (ptr::slice(value), ptr::slice_mut(into)) };
    maybe_bytes(proto::decode_property(value, into))
}

/// Writes an entity tag from a listing in the quoted form that HTTP defines.
///
/// A listing writes an entity tag without the quotes that the `ETag` header
/// carries. Pass `borink_list_entry.e_tag` here to get the form that a
/// condition takes. `into` needs at most two bytes more than `listed`, and a
/// shorter one writes nothing and returns an absent value.
///
/// # Safety
///
/// `listed` must address its stated length. `into` must address its stated
/// length and be reached through nothing else during the call.
///
/// # Lifetime
///
/// The bytes returned are `into`, so they are valid until you release or
/// reuse it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_quoted_etag(listed: Bytes, into: BytesMut) -> MaybeBytes {
    // SAFETY: the caller states the contract of this function.
    let (listed, into) = unsafe { (ptr::slice(listed), ptr::slice_mut(into)) };
    maybe_bytes(proto::quoted_etag(listed, into))
}

/// Reads an HTTP date as milliseconds since the Unix epoch.
///
/// Pass `borink_list_entry.last_modified`, or the same value of a
/// `borink_object_meta`. A value that is not an RFC 1123 date is absent.
///
/// # Safety
///
/// `value` must address its stated length.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_http_date_ms(value: Bytes) -> MaybeU64 {
    // SAFETY: the caller states the contract of this function.
    let value = unsafe { ptr::slice(value) };
    maybe_number(
        core::str::from_utf8(value)
            .ok()
            .and_then(proto::http_date_ms),
    )
}

/// Writes one sentence naming what `outcome` says.
///
/// Returns the length of the whole sentence, which may be longer than `into`.
/// The part that fits is written. A null `outcome` writes nothing and returns
/// 0.
///
/// # Safety
///
/// `outcome` must be null or point at one readable value whose borrowed fields
/// still address valid bytes. `into` must address its stated length and be
/// reached through nothing else during the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_describe(outcome: *const Outcome, into: BytesMut) -> usize {
    // SAFETY: the caller states the contract of this function.
    let Some(outcome) = (unsafe { outcome.as_ref() }) else {
        return 0;
    };
    // SAFETY: as above, for the identifier the outcome borrows and for `into`.
    let (request_id, into) = unsafe {
        (
            ptr::maybe_slice(outcome.failure.request_id),
            ptr::slice_mut(into),
        )
    };
    describe(outcome, request_id, into)
}

/// Writes one sentence naming what `status` says.
///
/// Returns the length of the whole sentence, exactly as `borink_describe`.
///
/// # Safety
///
/// `into` must address its stated length and be reached through nothing else
/// during the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_describe_status(status: Status, into: BytesMut) -> usize {
    // SAFETY: the caller states the contract of this function.
    describe_status(status, unsafe { ptr::slice_mut(into) })
}
