//! Azure Blob Storage requests and responses.
//!
//! Structured-body framing is not implemented.

use core::fmt;

use crate::checksum::{BASE64_LEN, ChecksumKind, checksum_base64};
use crate::head::{
    ContentRange, accept_get_success, parse_content_range, read_decimal_header, read_text_header,
    strip_metadata_prefix,
};
use crate::http::{PlainHttp, valid_http_origin};
use crate::outcome::{FailureOutcome, failure_outcome, finish_with_body, not_found_in_head};
use crate::properties::Property;
use crate::properties::sealed::Sealed;
use crate::request::{ByteSink, HeaderValue, condition_header_name};
use crate::template::{FixedText, Hole, HoleValues, NO_TEXT, Param, ParamValue, TemplateBody};
use crate::url::{self, Parameter, QueryValue};
use crate::validate::{
    validate_checksum_item, validate_condition, validate_delimiter, validate_get_item,
    validate_get_options, validate_properties, validate_revision_item, validate_tags,
};
use crate::xml::read_error_code;
use crate::{
    Classification, ConditionKind, DeleteHeadOutcome, DeleteKind, DeleteOptions, DeleteShape,
    Error, Failure, GetHeadOutcome, GetItem, GetKind, GetOptions, GetShape, InvalidRequest,
    ListEntry, ListHeadOutcome, ListInclude, ListItem, ListMarker, ListOptions, Listing,
    MetadataPair, Method, ObjectItem, ObjectMeta, Payload, Placement, PutHeadOutcome, PutItem,
    PutOptions, PutShape, Ranging, RequestedRange, ResponseFault, ResponseHead, Result, Revision,
    RevisionKind, ServiceErrorKind, Timestamps, WriteOptions,
};
use template::{Describe, Detail, Field, FieldList, FieldValue, Holes};

mod batch;
mod blocks;
mod copy;
mod restore;
mod tags;
mod template;

pub use batch::{
    BatchResult, MAX_BATCH_KEYS, accept_delete_many_error_body, accept_delete_many_head,
    encode_delete_many, fill_delete_results, prepare_delete_many,
};
pub use blocks::{
    Block, BlockListKind, BlockRef, BlockSource, BlockState, CommitItem, CommitOptions,
    MAX_STAGE_LEN, StageBlockItem, accept_commit_blocks_error_body, accept_commit_blocks_head,
    accept_list_blocks_error_body, accept_list_blocks_head, accept_stage_block_error_body,
    accept_stage_block_head, block_id, encode_commit_blocks, encode_list_blocks,
    encode_stage_block, fill_blocks, max_blocks_in, prepare_commit_blocks, prepare_list_blocks,
    prepare_stage_block,
};
pub use copy::{
    AbortCopyItem, StageBlockFromUrlItem, accept_abort_copy_head, accept_copy_error_body,
    accept_copy_head, accept_put_from_url_error_body, accept_put_from_url_head, encode_abort_copy,
    encode_copy, encode_copy_from_url, encode_put_from_url, encode_stage_block_from_url,
    prepare_abort_copy, prepare_copy, prepare_copy_from_url, prepare_put_from_url,
    prepare_stage_block_from_url,
};
pub use restore::{
    accept_restore_error_body, accept_restore_head, encode_restore, prepare_restore,
};
pub use tags::{
    SetMetadataOptions, SetPropertiesOptions, accept_get_tags_error_body, accept_get_tags_head,
    accept_set_metadata_head, accept_set_properties_head, accept_set_tags_head,
    accept_set_tier_head, accept_update_error_body, encode_get_tags, encode_set_metadata,
    encode_set_properties, encode_set_tags, encode_set_tier, fill_tags, prepare_get_tags,
    prepare_set_metadata, prepare_set_properties, prepare_set_tags, prepare_set_tier,
};
pub use template::{Operation, Template};

/// The most recent Azure Storage version that every region supports.
///
/// See the [Azure Storage service version lifecycle](https://learn.microsoft.com/en-us/rest/api/storageservices/versioning-for-the-azure-storage-services).
pub const VERSION: &str = "2026-04-06";

/// The most bytes of URL that Azure reads: the scheme, the host, the path
/// and the query together.
///
/// Every encoding method refuses a longer URL with
/// [`InvalidRequest::UrlTooLong`]. Azure answers one with HTTP 414 and no
/// error code. Measured on a flat and on a hierarchical account, whose hosts
/// differ in length: 32,759 bytes is answered and 32,760 is refused, in the
/// path and in the query alike.
pub const MAX_URL_LEN: usize = 32_759;

// A flat-namespace account limits blob names to 1,024 characters.
// Azure counts a blob name in UTF-16 code units, so a character outside the
// basic plane counts twice. Measured: a name of 1024 two-byte characters is
// taken and one of 541 four-byte characters, which is 1041 code units, is
// refused with 400. Measured against the live service.
//
// A hierarchical-namespace account has no such limit. Measured: it stored a
// key of 32,689 units and read it back, and refused a longer one with 414, a
// request line too long, at 32,759 bytes of encoded URL. That bound is the
// URL's, and `MAX_URL_LEN` holds it. This crate applies the flat limit only
// when told the account is flat; a client that does not know sends the key,
// and the service answers for the account it is.
const MAX_BLOB_NAME_UNITS: usize = 1024;

// The most `/`-delimited segments Azure takes in a name. Its documentation
// gives 254; measurement gives this.
const MAX_BLOB_NAME_SEGMENTS: usize = 255;

/// The prefix of the header that carries one metadata pair.
pub const METADATA_PREFIX: &str = "x-ms-meta-";

/// Returns the metadata name that a response header carries, or [`None`]
/// for a header that carries no pair.
pub fn metadata_name(header: &str) -> Option<&str> {
    strip_metadata_prefix(header, METADATA_PREFIX)
}

// A bearer token, as `Authorization` and `x-ms-copy-source-authorization`
// carry it.
struct Bearer<'t>(&'t str);

impl HeaderValue for Bearer<'_> {
    fn write_to(self, out: &mut dyn ByteSink) {
        out.push(b"Bearer ");
        out.push(self.0.as_bytes());
    }
}

/// An Azure Blob endpoint and container name, both borrowed.
#[derive(Debug, Clone, Copy)]
pub struct Container<'a> {
    endpoint: &'a str,
    name: &'a str,
    // The object that templates from this container name, if they name one.
    object: Option<&'a str>,
}

impl<'a> Container<'a> {
    /// Creates a container reference from an HTTPS origin and a container
    /// name.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidEndpoint`] if `endpoint` is not an ASCII HTTPS
    /// origin. An `http://` origin is refused: see [`Self::new_allowing_http`].
    ///
    /// Returns [`Error::InvalidContainer`] if `name` is empty, or if it
    /// contains bytes that would change the structure of the request.
    pub fn new(endpoint: &'a str, name: &'a str) -> Result<Self> {
        Self::create(endpoint, name, PlainHttp::Refused)
    }

    /// Creates a container reference as [`Self::new`] does, from an HTTP
    /// origin as well as an HTTPS one, such as an emulator's
    /// `http://127.0.0.1:10000`.
    ///
    /// Without TLS, every request carries the bearer token in clear. Use
    /// this for a local emulator or a network you trust, never for an
    /// account on the internet.
    ///
    /// # Errors
    ///
    /// As [`Self::new`], except that an `http://` origin is taken.
    pub fn new_allowing_http(endpoint: &'a str, name: &'a str) -> Result<Self> {
        Self::create(endpoint, name, PlainHttp::Allowed)
    }

    fn create(endpoint: &'a str, name: &'a str, plain: PlainHttp) -> Result<Self> {
        if !valid_http_origin(endpoint, plain) {
            return Err(Error::InvalidEndpoint);
        }
        if name.is_empty()
            || name
                .bytes()
                .any(|byte| matches!(byte, b'/' | b'?' | b'#') || byte.is_ascii_control())
        {
            return Err(Error::InvalidContainer);
        }
        Ok(Self {
            endpoint,
            name,
            object: None,
        })
    }

    /// Names the object `key` of this container, for templates of that one
    /// object.
    ///
    /// A template prepared from it writes the key as fixed text, checked
    /// once, and so does each request's line: a request of it checks and
    /// encodes no key. Pass each encoding function an empty key, or this
    /// one; any other is [`InvalidRequest::Template`]. Keep one template for
    /// each object you read or write often, such as the files of a
    /// database. An operation that names no object, such as a listing,
    /// refuses a container that names one with [`InvalidRequest::Option`].
    #[must_use]
    pub const fn object(self, key: &'a str) -> Self {
        Self {
            object: Some(key),
            ..self
        }
    }
}

/// The kind of storage account that a client talks to.
///
/// Pass it to each `prepare_*` function, such as [`prepare_get`].
/// [`InvalidRequest::azure_rejection`] reads it. This crate never discovers
/// it: no request or response says which kind of account answered.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[repr(u16)]
pub enum AzureNamespace {
    /// The account's namespace is not known. A client refuses only what
    /// both kinds of account refuse, and sends the rest.
    #[default]
    Unknown = 0,
    /// A flat-namespace account.
    Flat = 1,
    /// A hierarchical-namespace account.
    Hierarchical = 2,
}

/// Azure's corresponding rejection for one local validation failure.
///
/// No request was sent. Other failures, such as authentication, may take
/// precedence if the request is sent. This is not a received response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AzureRejection {
    /// The corresponding HTTP status.
    pub status: u16,
    /// The service error code, stored in static memory.
    ///
    /// Empty for a 414, which Azure sends without a code.
    pub code: &'static str,
}

impl InvalidRequest {
    /// Returns the known Azure rejection corresponding to this reason.
    ///
    /// Returns `None` when no mapping is established for the namespace,
    /// including names or ranges Azure may accept with unwanted behavior.
    /// The local reason remains authoritative; this predicts neither error
    /// precedence nor the outcome of a request containing other faults.
    ///
    /// ```
    /// use borink_object_storage_proto::InvalidRequest;
    /// use borink_object_storage_proto::azure::AzureNamespace;
    /// let reason = InvalidRequest::KeyTooLong;
    /// let azure = reason.azure_rejection(AzureNamespace::Flat).unwrap();
    /// assert_eq!((azure.status, azure.code), (400, "OutOfRangeInput"));
    /// assert_eq!(reason.azure_rejection(AzureNamespace::Hierarchical), None);
    /// ```
    pub const fn azure_rejection(self, namespace: AzureNamespace) -> Option<AzureRejection> {
        let (status, code) = match (self, namespace) {
            (Self::MaxResults, _) => (400, "OutOfRangeQueryParameterValue"),
            (Self::KeyTooLong, AzureNamespace::Flat) => (400, "OutOfRangeInput"),
            (Self::UrlTooLong, _) => (414, ""),
            (Self::Delimiter, AzureNamespace::Hierarchical) => (400, "DelimiterIsInvalidForHNS"),
            _ => return None,
        };
        Some(AzureRejection { status, code })
    }
}

/// The result of reading the response head of a Snapshot Blob.
///
/// A head that reports a failure is one of these too.
/// [`accept_snapshot_head`] returns an [`Err`] only for a head it
/// cannot read.
// The crate allocates nothing and its outcomes are `Copy`, so the variant
// that carries the metadata cannot be boxed.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SnapshotHeadOutcome<'h> {
    /// Azure took the snapshot.
    Created {
        /// The snapshot, as `x-ms-snapshot` names it. Pass it as
        /// [`Revision::Snapshot`] to read or remove the snapshot.
        snapshot: &'h str,
        /// The metadata that the head states: the entity tag and the last
        /// modification of the object, and its version on an account that
        /// keeps versions.
        meta: ObjectMeta<'h>,
    },
    /// The condition did not hold, so Azure took no snapshot.
    PreconditionFailed,
    /// The object does not exist. A missing container is a
    /// [`Self::ServiceFailure`] with [`ServiceErrorKind::NoSuchContainer`].
    NotFound {
        /// The service's reason, if known.
        kind: Option<ServiceErrorKind>,
    },
    /// Read the error body to finish this response.
    NeedErrorBody(Failure<'h>),
    /// The service refused the request.
    ServiceFailure(Failure<'h>),
}

impl fmt::Display for SnapshotHeadOutcome<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Created { .. } => f.write_str("Azure took the snapshot"),
            Self::PreconditionFailed => f.write_str("a precondition on the request did not hold"),
            Self::NotFound { kind } => f.write_str(kind.map_or(
                "the object or the container does not exist",
                ServiceErrorKind::as_str,
            )),
            Self::NeedErrorBody(_) => f.write_str("read the response body to name the error"),
            Self::ServiceFailure(failure) => failure.fmt(f),
        }
    }
}

failure_outcome!(SnapshotHeadOutcome<'h>);

// The word for each include flag. The query writes them in this order,
// whatever order the set was built in.
const INCLUDE_WORDS: [(ListInclude, &str); 3] = [
    (ListInclude::METADATA, "metadata"),
    (ListInclude::SNAPSHOTS, "snapshots"),
    (ListInclude::VERSIONS, "versions"),
];

// The query parameter that names a snapshot or a version, or none for the
// object as it is now.
fn revision_parameter(revision: Option<Revision<'_>>) -> Parameter<'_> {
    match revision? {
        Revision::Snapshot(id) => url::encoded_parameter("snapshot", id),
        Revision::Version(id) => url::encoded_parameter("versionid", id),
    }
}

// Reads a head that reports a failure. Azure names its error in the
// `x-ms-error-code` header and repeats it in the body, so only a head
// without the header leaves the error to the body. A header that names a
// code this crate does not know is already decisive. A 404 that names its
// error is a missing object, or a missing container.
fn read_failure_head<'h, O: FailureOutcome<'h>>(head: &ResponseHead<'h>) -> O {
    match head.error_code {
        None => O::need_error_body(Failure::new(head.status, None, head.request_id)),
        Some(_) if head.status == 404 => not_found_in_head(head, kind_in_head(head)),
        Some(_) => O::service_failure(Failure::new(
            head.status,
            kind_in_head(head),
            head.request_id,
        )),
    }
}

fn kind_in_head<'h>(head: &ResponseHead<'h>) -> Option<ServiceErrorKind> {
    kind_for_code(head.error_code.unwrap_or_default().trim_ascii())
}

/// Borrows the native error code, preferring the header over the XML body.
/// Unknown codes are retained unchanged.
pub fn error_code<'a>(head: &ResponseHead<'a>, body: &'a [u8]) -> Option<&'a [u8]> {
    head.error_code
        .map(<[u8]>::trim_ascii)
        .or_else(|| read_error_code(body).map(str::as_bytes))
}

/// Classifies the Azure error that `head` names, or that `body` names if the
/// head carries no error code. Set `truncated` if a read limit stopped the
/// body early.
pub fn classify_error(head: &ResponseHead<'_>, body: &[u8], truncated: bool) -> Classification {
    let code = head
        .error_code
        .map(<[u8]>::trim_ascii)
        .or_else(|| read_error_code(body).map(|code| code.as_bytes()));
    match code.and_then(kind_for_code) {
        Some(kind) => Classification::Classified(kind),
        None if truncated => Classification::Incomplete,
        None => Classification::Unknown,
    }
}

fn kind_for_code(code: &[u8]) -> Option<ServiceErrorKind> {
    Some(match code {
        b"BlobNotFound" | b"ResourceNotFound" => ServiceErrorKind::NotFound,
        b"ContainerNotFound" => ServiceErrorKind::NoSuchContainer,
        b"BlobAlreadyExists" | b"ContainerAlreadyExists" => ServiceErrorKind::AlreadyExists,
        b"ConditionNotMet" | b"TargetConditionNotMet" | b"SourceConditionNotMet" => {
            ServiceErrorKind::Precondition
        }
        b"InvalidRange" => ServiceErrorKind::RangeNotSatisfiable,
        b"ServerBusy" => ServiceErrorKind::Throttled,
        b"OperationTimedOut" => ServiceErrorKind::Timeout,
        b"AuthenticationFailed"
        | b"AuthorizationFailure"
        | b"InvalidAuthenticationInfo"
        | b"AuthorizationPermissionMismatch"
        | b"InsufficientAccountPermissions" => ServiceErrorKind::Unauthorized,
        b"InternalError" | b"ServiceUnavailable" => ServiceErrorKind::Service,
        b"InvalidBlockList"
        | b"InvalidBlockId"
        | b"InvalidBlobOrBlock"
        | b"BlockCountExceedsLimit"
        | b"BlockListTooLong" => ServiceErrorKind::InvalidUpload,
        _ => return None,
    })
}

// Whether a write conditional on `If-None-Match` failed because an object
// exists. Azure answers that with 409 `BlobAlreadyExists`, not 412.
fn names_existing_object(
    status: u16,
    condition: ConditionKind,
    kind: Option<ServiceErrorKind>,
) -> bool {
    status == 409
        && condition == ConditionKind::IfNoneMatch
        && kind == Some(ServiceErrorKind::AlreadyExists)
}

// Whether a 412 is the request's failed condition: the request carried one, and
// Azure names the failure. Azure also answers 412 for other reasons, such as
// `LeaseIdMissing` on a leased blob.
fn names_failed_condition(status: u16, carried: bool, kind: Option<ServiceErrorKind>) -> bool {
    status == 412 && carried && kind == Some(ServiceErrorKind::Precondition)
}

// The error that a failed response body names, if it names one this crate
// recognizes.
fn kind_in_body(body: &[u8]) -> Option<ServiceErrorKind> {
    read_error_code(body).and_then(|code| kind_for_code(code.as_bytes()))
}

// The value of `x-ms-delete-snapshots` for a removal of `kind`, or none
// for the object alone.
const fn delete_snapshots(kind: DeleteKind) -> Option<&'static str> {
    match kind {
        DeleteKind::Object => None,
        DeleteKind::ObjectAndSnapshots => Some("include"),
        DeleteKind::SnapshotsOnly => Some("only"),
    }
}

// The length of a name as Azure counts it.
fn name_units(value: &str) -> usize {
    value.chars().map(char::len_utf16).sum()
}

/// Writes the template of reads with `options` from `container`, sent with
/// the bearer `token`, into `buf`.
///
/// `namespace` is the kind of account the reads go to. Pass
/// [`AzureNamespace::Unknown`] if you do not know it: the encoder then
/// refuses only the keys that both kinds refuse.
///
/// # Errors
///
/// Returns [`Error::InvalidToken`] if `token` is not usable as one HTTP
/// header value, and [`InvalidRequest::RangedHead`] for ranged HEADs.
///
/// Returns [`Error::Capacity`] if `buf` is too small, with the bytes it
/// needs.
pub fn prepare_get(
    container: &Container<'_>,
    token: &str,
    namespace: AzureNamespace,
    options: &GetOptions,
    buf: &mut [u8],
) -> Result<Template> {
    validate_get_options(options, true)?;
    template::prepare_template(
        container,
        token,
        namespace,
        &describe_get(options),
        Operation::Get,
        Detail::Get(*options),
        buf,
        |_| {},
    )
}

// The request of a read with `options`.
fn describe_get(options: &GetOptions) -> Describe<'static, 1> {
    Describe {
        method: match options.kind {
            GetKind::Bytes => Method::Get,
            GetKind::Head => Method::Head,
        },
        key: true,
        query: [revision_param(options.revision)],
        holes: Holes {
            range: options.ranging == Ranging::Ranged,
            condition: condition_header_name(options.condition),
            ..Holes::default()
        },
        fields: &[],
        metadata: &[],
        body: TemplateBody::None,
    }
}

/// Writes the read `item` at `now` from `template` into `buf`, and returns where it is.
///
/// `bytes` are the template's own, as [`prepare_get`] wrote them.
///
/// # Errors
///
/// Returns [`Error::InvalidRequest`] if `item` is not a read that the template
/// writes, or not a key that Azure takes: see [Object keys](crate#object-keys).
/// Azure serves no suffix range, which is [`InvalidRequest::UnsupportedRange`].
/// A URL longer than [`MAX_URL_LEN`] is [`InvalidRequest::UrlTooLong`]. A
/// template for another operation is [`InvalidRequest::Template`].
///
/// Returns [`Error::Capacity`] if `buf` is too small, with the bytes it
/// needs.
pub fn encode_get(
    template: &Template,
    bytes: &[u8],
    item: &GetItem<'_>,
    now: &Timestamps,
    buf: &mut [u8],
) -> Result<Placement> {
    let Detail::Get(options) = template.detail else {
        return Err(InvalidRequest::Template.into());
    };
    let key = template.request_key(bytes, item.key)?;
    if let RequestedRange::Suffix(_) = item.range {
        return Err(InvalidRequest::UnsupportedRange.into());
    }
    validate_get_item(&options, item)?;
    let values = HoleValues {
        now: Some(now),
        key,
        range: Some(item.range).filter(|range| *range != RequestedRange::Whole),
        condition: item.condition_value,
        revision: item.revision,
        ..HoleValues::default()
    };
    template::encode_template(
        template,
        bytes,
        &values,
        Payload::Slice(&[]),
        buf,
        &describe_get(&options),
    )
}

/// Reads the response head of a read and reports what to do next.
///
/// Pass the [`GetShape`] of the read. Every head that Azure sends becomes a
/// [`GetHeadOutcome`], including the heads that report a failure. Azure
/// names its errors in the `x-ms-error-code` header, so a failure needs the
/// body only when that header is missing: the outcome is then
/// [`GetHeadOutcome::NeedErrorBody`]. The answer to a HEAD has no body, so
/// a failure of a HEAD is final, and names no error if the header is
/// missing.
///
/// # Errors
///
/// Returns [`Error::Response`] if the head cannot be read against `shape`.
/// A `Content-Range` whose end is before its start is
/// [`ResponseFault::Head`]. A ranged read that Azure answers with status 200
/// is [`ResponseFault::Range`].
///
/// A 412 is [`GetHeadOutcome::PreconditionFailed`] only if the read carried
/// `If-Match` or `If-Unmodified-Since` and Azure names a failed condition.
/// Any other 412 is a service failure that names its code.
pub fn accept_get_head<'h>(shape: GetShape, head: ResponseHead<'h>) -> Result<GetHeadOutcome<'h>> {
    Ok(match accept_get(shape, head)? {
        GetHeadOutcome::NeedErrorBody(failure) if shape.kind == GetKind::Head => {
            accept_get_error_body(shape, failure, &[])
        }
        outcome => outcome,
    })
}

/// Finishes a [`GetHeadOutcome::NeedErrorBody`] with the response body.
///
/// Pass the [`GetShape`] of the read, the [`Failure`] of that outcome, and
/// the body that you read. Pass an empty body if you could not read one: the
/// outcome is then final with the error unnamed.
///
/// To tell a body that your read limit cut short from a body that names an
/// error this crate does not recognize, call [`classify_error`] instead.
pub fn accept_get_error_body<'h>(
    shape: GetShape,
    failure: Failure<'h>,
    body: &[u8],
) -> GetHeadOutcome<'h> {
    let kind = kind_in_body(body);
    // A read fails `If-None-Match` and `If-Modified-Since` with 304, so only
    // `If-Match` and `If-Unmodified-Since` fail with 412.
    if names_failed_condition(
        failure.status,
        shape.condition != ConditionKind::None && !shape.condition.fails_as_not_modified(),
        kind,
    ) {
        return GetHeadOutcome::PreconditionFailed;
    }
    finish_with_body(failure, kind)
}

/// Writes the template of whole-object writes with `options` into
/// `container`, sent with the bearer `token`, into `buf`.
///
/// Every write sends `x-ms-blob-type: BlockBlob`, and the metadata, the
/// content properties, the tags and the access tier of `options`.
///
/// # Errors
///
/// Returns [`Error::InvalidToken`] if `token` is not usable as one HTTP
/// header value. Returns [`Error::InvalidRequest`] for a metadata pair, a
/// content property or a tag that Azure would not store as given, or a
/// checksum other than an MD5 or a CRC64, which is [`InvalidRequest::Option`].
///
/// Returns [`Error::Capacity`] if `buf` is too small, with the bytes it
/// needs.
pub fn prepare_put(
    container: &Container<'_>,
    token: &str,
    namespace: AzureNamespace,
    options: &PutOptions<'_>,
    buf: &mut [u8],
) -> Result<Template> {
    validate_metadata(options.metadata)?;
    validate_properties(&options.options, utf8_properties(namespace))?;
    validate_tags(options.options.tags, azure_tag_char, Some((10, 128, 256)))?;
    if options.checksum.is_some_and(|kind| !kind.azure_takes()) {
        return Err(InvalidRequest::Option.into());
    }
    let mut fields = FieldList::new();
    fields.push(Field::new(
        "x-ms-blob-type",
        FieldValue::Bytes(b"BlockBlob"),
    ));
    push_stored_fields(&mut fields, &options.options);
    template::prepare_template(
        container,
        token,
        namespace,
        &describe_put(
            options.condition,
            options.checksum,
            fields.as_slice(),
            options.metadata,
        ),
        Operation::Put,
        Detail::Put {
            condition: options.condition,
            checksum: options.checksum,
        },
        buf,
        |_| {},
    )
}

// The request of a whole-object write of a template with `condition` and
// `checksum`, which sends `fields` and `metadata`.
fn describe_put<'p>(
    condition: ConditionKind,
    checksum: Option<ChecksumKind>,
    fields: &'p [Field<'p>],
    metadata: &'p [MetadataPair<'p>],
) -> Describe<'p, 0> {
    Describe {
        method: Method::Put,
        key: true,
        query: [],
        holes: Holes {
            content_length: Some(Hole::ContentLength),
            checksum: checksum.map(|kind| kind.header()),
            condition: condition_header_name(condition),
            ..Holes::default()
        },
        fields,
        metadata,
        body: TemplateBody::None,
    }
}

/// Writes the whole-object write `item` at `now` from `template` into `buf`,
/// and returns where it is.
///
/// `bytes` are the template's own, as [`prepare_put`] wrote them. The
/// request lends a [`Payload::Slice`] to your HTTP client, and leaves a
/// [`Payload::Streamed`] for you to send.
///
/// # Errors
///
/// Returns [`Error::InvalidRequest`] if `item` is not a write that the template
/// writes, not a key that Azure takes, or content longer than
/// [`MAX_PUT_LEN`], which is [`InvalidRequest::PayloadTooLarge`]. A checksum
/// that is not the base64 of a digest of the template's kind is
/// [`InvalidRequest::Checksum`].
///
/// Returns [`Error::Capacity`] if `buf` is too small, with the bytes it
/// needs.
pub fn encode_put(
    template: &Template,
    bytes: &[u8],
    item: &PutItem<'_>,
    now: &Timestamps,
    buf: &mut [u8],
) -> Result<Placement> {
    let Detail::Put {
        condition,
        checksum,
    } = template.detail
    else {
        return Err(InvalidRequest::Template.into());
    };
    let key = template.request_key(bytes, item.key)?;
    if item.content.len() > MAX_PUT_LEN {
        return Err(InvalidRequest::PayloadTooLarge.into());
    }
    validate_condition(condition, item.condition_value)?;
    validate_checksum_item(checksum, item.checksum)?;
    let mut text = [0; BASE64_LEN];
    let values = HoleValues {
        now: Some(now),
        key,
        condition: item.condition_value,
        content_length: Some(item.content.len()),
        checksum: checksum_base64(item.checksum, &mut text, false),
        ..HoleValues::default()
    };
    template::encode_template(
        template,
        bytes,
        &values,
        item.content,
        buf,
        &describe_put(condition, checksum, &[], &[]),
    )
}

/// Reads the response head of a write and reports what Azure did.
///
/// Pass the [`PutShape`] of the write. Every head that Azure sends becomes
/// a [`PutHeadOutcome`], including the heads that report a failure.
///
/// # Errors
///
/// Returns [`Error::Response`] if the head cannot be read against `shape`.
/// A success status that a write never returns is
/// [`ResponseFault::Status`].
///
/// A 412 is [`PutHeadOutcome::PreconditionFailed`] only if the write
/// carried a condition and Azure names a failed condition. Azure answers 412
/// for other reasons too, such as `LeaseIdMissing` on a leased blob, and
/// those are service failures that name their code.
pub fn accept_put_head<'h>(shape: PutShape, head: ResponseHead<'h>) -> Result<PutHeadOutcome<'h>> {
    match head.status {
        201 => Ok(PutHeadOutcome::Created {
            meta: ObjectMeta {
                last_modified: read_text_header(head.last_modified)?,
                ..head.object_meta()
            },
        }),
        412 if shape.condition != ConditionKind::None
            && kind_in_head(&head) == Some(ServiceErrorKind::Precondition) =>
        {
            Ok(PutHeadOutcome::PreconditionFailed)
        }
        409 if names_existing_object(409, shape.condition, kind_in_head(&head)) => {
            Ok(PutHeadOutcome::PreconditionFailed)
        }
        200..=299 => Err(ResponseFault::Status.into()),
        _ => Ok(read_failure_head(&head)),
    }
}

/// Finishes a [`PutHeadOutcome::NeedErrorBody`] with the response body.
///
/// This reads the body as [`accept_get_error_body`] does.
pub fn accept_put_error_body<'h>(
    shape: PutShape,
    failure: Failure<'h>,
    body: &[u8],
) -> PutHeadOutcome<'h> {
    let kind = kind_in_body(body);
    if names_failed_condition(failure.status, shape.condition != ConditionKind::None, kind)
        || names_existing_object(failure.status, shape.condition, kind)
    {
        return PutHeadOutcome::PreconditionFailed;
    }
    finish_with_body(failure, kind)
}

/// Writes the template of removals with `options` from `container`, sent
/// with the bearer `token`, into `buf`.
///
/// `namespace` is the kind of account the removals go to: see
/// [`prepare_get`].
///
/// # Errors
///
/// Returns [`Error::InvalidToken`] if `token` is not usable as one HTTP
/// header value. A removal of a snapshot or a version that takes more than
/// the object is [`InvalidRequest::Revision`]: it has no snapshots of its
/// own, and Azure refuses `x-ms-delete-snapshots` beside one.
///
/// Returns [`Error::Capacity`] if `buf` is too small, with the bytes it
/// needs.
pub fn prepare_delete(
    container: &Container<'_>,
    token: &str,
    namespace: AzureNamespace,
    options: &DeleteOptions,
    buf: &mut [u8],
) -> Result<Template> {
    if options.revision != RevisionKind::Current && options.kind != DeleteKind::Object {
        return Err(InvalidRequest::Revision.into());
    }
    // Azure refuses an object with snapshots when the header is absent,
    // which is the outcome a removal of the object alone asks for.
    let snapshots = delete_snapshots(options.kind).map(|snapshots| {
        Field::new(
            "x-ms-delete-snapshots",
            FieldValue::Bytes(snapshots.as_bytes()),
        )
    });
    let mut template = prepare_object(
        container,
        token,
        namespace,
        Operation::Delete,
        options.condition,
        options.revision,
        &ObjectText {
            fields: snapshots.as_slice(),
            ..ObjectText::default()
        },
        buf,
    )?;
    if let Detail::Object { delete, .. } = &mut template.detail {
        *delete = options.kind;
    }
    Ok(template)
}

/// Writes the removal `item` at `now` from `template` into `buf`, and
/// returns where it is.
///
/// `bytes` are the template's own, as [`prepare_delete`] wrote them. The
/// request has no content.
///
/// # Errors
///
/// Returns [`Error::InvalidRequest`] if `item` is not a removal that the
/// template writes, or not a key that Azure takes: see
/// [Object keys](crate#object-keys). A URL longer than [`MAX_URL_LEN`] is
/// [`InvalidRequest::UrlTooLong`]. A template for another operation is
/// [`InvalidRequest::Template`].
///
/// Returns [`Error::Capacity`] if `buf` is too small, with the bytes it
/// needs.
pub fn encode_delete(
    template: &Template,
    bytes: &[u8],
    item: &ObjectItem<'_>,
    now: &Timestamps,
    buf: &mut [u8],
) -> Result<Placement> {
    encode_object(template, bytes, Operation::Delete, item, now, buf)
}

// Writes the request of `operation` that names the one object `item`, from
// a template of that operation.
fn encode_object(
    template: &Template,
    bytes: &[u8],
    operation: Operation,
    item: &ObjectItem<'_>,
    now: &Timestamps,
    buf: &mut [u8],
) -> Result<Placement> {
    let Detail::Object {
        condition,
        revision,
        ..
    } = template.detail
    else {
        return Err(InvalidRequest::Template.into());
    };
    if template.operation != operation {
        return Err(InvalidRequest::Template.into());
    }
    let key = template.request_key(bytes, item.key)?;
    validate_revision_item(revision, item.revision)?;
    validate_condition(condition, item.condition_value)?;
    let values = HoleValues {
        now: Some(now),
        key,
        condition: item.condition_value,
        revision: item.revision,
        ..HoleValues::default()
    };
    template::encode_template(
        template,
        bytes,
        &values,
        Payload::Slice(&[]),
        buf,
        &describe_object(operation, condition, revision, None),
    )
}

// What every request of a template that names one object sends alike,
// beside its structure: the fixed fields and the metadata, a fixed
// parameter after `comp`, and the text of a fixed body.
#[derive(Default)]
struct ObjectText<'p> {
    fields: &'p [Field<'p>],
    metadata: &'p [MetadataPair<'p>],
    param: Param<'p>,
    body: Option<FixedText<'p>>,
}

// Writes the template of `operation`, which names one object, with the
// condition `condition` and the kind of revision `revision`.
#[allow(clippy::too_many_arguments)]
fn prepare_object(
    container: &Container<'_>,
    token: &str,
    namespace: AzureNamespace,
    operation: Operation,
    condition: ConditionKind,
    revision: RevisionKind,
    text: &ObjectText<'_>,
    buf: &mut [u8],
) -> Result<Template> {
    template::prepare_template(
        container,
        token,
        namespace,
        &describe_object(operation, condition, revision, Some(text)),
        operation,
        Detail::Object {
            condition,
            revision,
            delete: DeleteKind::Object,
        },
        buf,
        |_| {},
    )
}

// The request of `operation`, which names one object, for a template with
// `condition` and `revision`: its method, the parameter `comp` that names
// the operation, the fields that each request fills, and its body. `text`
// is the template's, and none for a request.
fn describe_object<'p>(
    operation: Operation,
    condition: ConditionKind,
    revision: RevisionKind,
    text: Option<&ObjectText<'p>>,
) -> Describe<'p, 3> {
    let (method, comp) = match operation {
        Operation::Delete => (Method::Delete, None),
        Operation::GetTags => (Method::Get, Some("tags")),
        Operation::ListBlocks => (Method::Get, Some("blocklist")),
        Operation::SetTier | Operation::Restore => (Method::Put, Some("tier")),
        Operation::SetMetadata => (Method::Put, Some("metadata")),
        Operation::SetProperties => (Method::Put, Some("properties")),
        Operation::SetTags => (Method::Put, Some("tags")),
        _ => (Method::Put, Some("snapshot")),
    };
    // A Set Blob Tags sends the body that its template holds.
    let tags = operation == Operation::SetTags;
    Describe {
        method,
        key: true,
        query: [
            comp.map(|comp| ("comp", ParamValue::Fixed(QueryValue::Literal(comp)))),
            text.and_then(|text| text.param),
            revision_param(revision),
        ],
        holes: Holes {
            content_length: tags.then_some(Hole::BodyLength),
            condition: condition_header_name(condition),
            ..Holes::default()
        },
        fields: text.map_or(&[], |text| text.fields),
        metadata: text.map_or(&[], |text| text.metadata),
        body: match tags {
            true => TemplateBody::Fixed(text.and_then(|text| text.body).unwrap_or(NO_TEXT)),
            false => TemplateBody::None,
        },
    }
}

/// Reads the response head of a removal and reports what Azure did.
///
/// Pass the [`DeleteShape`] of the removal. Every head that Azure sends
/// becomes a [`DeleteHeadOutcome`], including the heads that report a
/// failure.
///
/// # Errors
///
/// Returns [`Error::Response`] if the head cannot be read against `shape`.
/// A success status that a removal never returns is
/// [`ResponseFault::Status`].
///
/// A 412 is [`DeleteHeadOutcome::PreconditionFailed`] only if the removal
/// carried a condition and Azure names a failed condition. Azure answers
/// 412 for other reasons too, such as `LeaseIdMissing` on a leased blob,
/// and those are service failures that name their code.
pub fn accept_delete_head<'h>(
    shape: DeleteShape,
    head: ResponseHead<'h>,
) -> Result<DeleteHeadOutcome<'h>> {
    match head.status {
        202 => Ok(DeleteHeadOutcome::Accepted),
        412 if shape.condition != ConditionKind::None
            && kind_in_head(&head) == Some(ServiceErrorKind::Precondition) =>
        {
            Ok(DeleteHeadOutcome::PreconditionFailed)
        }
        200..=299 => Err(ResponseFault::Status.into()),
        _ => Ok(read_failure_head(&head)),
    }
}

/// Finishes a [`DeleteHeadOutcome::NeedErrorBody`] with the response body.
///
/// This is [`accept_get_error_body`] for a removal, and reads the body the
/// same way.
pub fn accept_delete_error_body<'h>(
    shape: DeleteShape,
    failure: Failure<'h>,
    body: &[u8],
) -> DeleteHeadOutcome<'h> {
    let kind = kind_in_body(body);
    if names_failed_condition(failure.status, shape.condition != ConditionKind::None, kind) {
        return DeleteHeadOutcome::PreconditionFailed;
    }
    finish_with_body(failure, kind)
}

/// What every Snapshot Blob of a template does alike.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SnapshotOptions<'o> {
    /// The metadata of each snapshot. With no pair, a snapshot has the
    /// object's metadata; with any, it has these pairs alone. The rules of
    /// [`PutOptions::metadata`](crate::PutOptions::metadata) apply.
    pub metadata: &'o [MetadataPair<'o>],
    /// The condition on the object, as a read carries it.
    pub condition: ConditionKind,
}

/// Writes the template of Snapshot Blobs with `options` into `buf`. Each
/// takes a read-only snapshot of an object as it is now.
///
/// # Errors
///
/// Returns [`Error::InvalidRequest`] for a metadata pair that [`prepare_put`]
/// refuses.
///
/// Returns [`Error::Capacity`] if `buf` is too small, with the bytes it
/// needs.
pub fn prepare_snapshot(
    container: &Container<'_>,
    token: &str,
    namespace: AzureNamespace,
    options: &SnapshotOptions<'_>,
    buf: &mut [u8],
) -> Result<Template> {
    validate_metadata(options.metadata)?;
    prepare_object(
        container,
        token,
        namespace,
        Operation::Snapshot,
        options.condition,
        RevisionKind::Current,
        &ObjectText {
            fields: &[Field::new("content-length", FieldValue::Bytes(b"0"))],
            metadata: options.metadata,
            ..ObjectText::default()
        },
        buf,
    )
}

/// Writes the Snapshot Blob of `item` at `now` from `template` into `buf`,
/// and returns where it is.
///
/// # Errors
///
/// Returns [`Error::InvalidRequest`] for a key that Azure refuses, see
/// [Object keys](crate#object-keys), for a condition value that the
/// template's condition does not take, for a revision, and for a template
/// of another operation.
///
/// Returns [`Error::Capacity`] if `buf` is too small, with the bytes it
/// needs.
pub fn encode_snapshot(
    template: &Template,
    bytes: &[u8],
    item: &ObjectItem<'_>,
    now: &Timestamps,
    buf: &mut [u8],
) -> Result<Placement> {
    encode_object(template, bytes, Operation::Snapshot, item, now, buf)
}

/// Reads the response head of a Snapshot Blob and reports what Azure did.
///
/// Pass the condition of the template. A 412 is
/// [`SnapshotHeadOutcome::PreconditionFailed`] only if it carries one and
/// Azure names a failed condition.
///
/// # Errors
///
/// Returns [`Error::Response`] if the head cannot be read. A success status
/// other than 201 is [`ResponseFault::Status`], and a 201 without
/// `x-ms-snapshot` is [`ResponseFault::Head`].
pub fn accept_snapshot_head<'h>(
    condition: ConditionKind,
    head: ResponseHead<'h>,
) -> Result<SnapshotHeadOutcome<'h>> {
    match head.status {
        201 => Ok(SnapshotHeadOutcome::Created {
            snapshot: read_text_header(head.snapshot)?.ok_or(ResponseFault::Head)?,
            meta: ObjectMeta {
                last_modified: read_text_header(head.last_modified)?,
                ..head.object_meta()
            },
        }),
        412 if condition != ConditionKind::None
            && kind_in_head(&head) == Some(ServiceErrorKind::Precondition) =>
        {
            Ok(SnapshotHeadOutcome::PreconditionFailed)
        }
        200..=299 => Err(ResponseFault::Status.into()),
        _ => Ok(read_failure_head(&head)),
    }
}

/// Finishes a [`SnapshotHeadOutcome::NeedErrorBody`] with the response body,
/// as [`accept_get_error_body`] does for a read.
pub fn accept_snapshot_error_body<'h>(
    condition: ConditionKind,
    failure: Failure<'h>,
    body: &[u8],
) -> SnapshotHeadOutcome<'h> {
    let kind = kind_in_body(body);
    if names_failed_condition(failure.status, condition != ConditionKind::None, kind) {
        return SnapshotHeadOutcome::PreconditionFailed;
    }
    finish_with_body(failure, kind)
}

/// Writes the template of listings with `options` of `container`, sent
/// with the bearer `token`, into `buf`.
///
/// `namespace` is the kind of account the listings go to: see
/// [`prepare_get`].
///
/// # Errors
///
/// Returns [`Error::InvalidToken`] if `token` is not usable as one HTTP
/// header value. Returns [`Error::InvalidRequest`] if `options` cannot become
/// an Azure request:
///
/// - [`InvalidRequest::MaxResults`] for a page of no entries.
/// - [`InvalidRequest::Delimiter`] for an empty delimiter, and for any but `/`
///   on a [`AzureNamespace::Hierarchical`] account.
/// - [`InvalidRequest::Option`] for [`ListInclude::OWNER`]: Azure names no
///   owner.
///
/// Returns [`Error::Capacity`] if `buf` is too small, with the bytes it
/// needs.
pub fn prepare_list(
    container: &Container<'_>,
    token: &str,
    namespace: AzureNamespace,
    options: &ListOptions<'_>,
    buf: &mut [u8],
) -> Result<Template> {
    if options.max_results == Some(0) {
        return Err(InvalidRequest::MaxResults.into());
    }
    // A flat account groups names at any text. A hierarchical one groups
    // them at `/` alone, and answers any other with 400
    // `DelimiterIsInvalidForHNS`.
    validate_delimiter(options.delimiter, namespace == AzureNamespace::Hierarchical)?;
    if options.include.contains(ListInclude::OWNER) {
        return Err(InvalidRequest::Option.into());
    }
    let mut words = [""; INCLUDE_WORDS.len()];
    let mut word_count = 0;
    for (flag, word) in INCLUDE_WORDS {
        if options.include.contains(flag) {
            words[word_count] = word;
            word_count += 1;
        }
    }
    template::prepare_template(
        container,
        token,
        namespace,
        &describe_list(Some((options, &words[..word_count]))),
        Operation::List,
        Detail::List,
        buf,
        |_| {},
    )
}

// The request of a page of a listing. `text` is the template's options
// and the words of what it includes, and none for a request: the
// parameters that every page sends alike come before the ones that a page
// may leave out, so a request writes none of them.
fn describe_list<'p>(text: Option<(&'p ListOptions<'p>, &'p [&'p str])>) -> Describe<'p, 7> {
    let options = text.map(|(options, _)| options);
    let words = text.map_or(&[][..], |(_, words)| words);
    Describe {
        method: Method::Get,
        key: false,
        query: [
            Some((
                "restype",
                ParamValue::Fixed(QueryValue::Literal("container")),
            )),
            Some(("comp", ParamValue::Fixed(QueryValue::Literal("list")))),
            options.and_then(|options| options.delimiter).map(|text| {
                (
                    "delimiter",
                    ParamValue::Fixed(QueryValue::Encoded(text.as_bytes())),
                )
            }),
            options
                .and_then(|options| options.max_results)
                .map(|count| ("maxresults", ParamValue::Fixed(QueryValue::Number(count)))),
            (!words.is_empty()).then_some(("include", ParamValue::Fixed(QueryValue::Words(words)))),
            Some(("prefix", ParamValue::Optional(Hole::Prefix))),
            Some(("marker", ParamValue::Optional(Hole::Marker))),
        ],
        holes: Holes::default(),
        fields: &[],
        metadata: &[],
        body: TemplateBody::None,
    }
}

/// Writes the page `item` at `now` from `template` into `buf`, and
/// returns where it is.
///
/// `bytes` are the template's own, as [`prepare_list`] wrote them. The
/// response carries the page as a document in its body: read it whole and
/// pass it to [`fill_listing`].
///
/// # Errors
///
/// Returns [`Error::InvalidRequest`] if `item` cannot become an Azure request:
///
/// - [`InvalidRequest::Marker`] for a marker that is not
///   [`ListMarker::Text`], or is empty.
/// - [`InvalidRequest::Option`] for [`ListItem::start_after`]: Azure starts a
///   listing only at its own marker.
/// - [`InvalidRequest::UrlTooLong`] for a URL longer than [`MAX_URL_LEN`].
///   A prefix is bounded by the URL alone.
/// - [`InvalidRequest::Template`] for a template for another operation.
///
/// Returns [`Error::Capacity`] if `buf` is too small, with the bytes it
/// needs.
pub fn encode_list(
    template: &Template,
    bytes: &[u8],
    item: &ListItem<'_>,
    now: &Timestamps,
    buf: &mut [u8],
) -> Result<Placement> {
    let Detail::List = template.detail else {
        return Err(InvalidRequest::Template.into());
    };
    // No rule of `validate_key` applies to a prefix. It is written into the
    // query, where nothing resolves a `..` and nothing drops a trailing dot,
    // and `dir.` is an honest prefix of `dir.txt`. Nor is it bounded like a
    // name: a flat account answered a prefix of 32,657 units, far past the
    // 1,024 it allows a name. What bounds a prefix is the URL. Measured
    // against the live service.
    //
    // Azure continues every listing from one marker, and hands out no empty
    // one.
    let marker = match item.marker {
        None => None,
        Some(ListMarker::Text(marker)) if !marker.is_empty() => Some(marker),
        Some(_) => return Err(InvalidRequest::Marker.into()),
    };
    if item.start_after.is_some() {
        return Err(InvalidRequest::Option.into());
    }
    let values = HoleValues {
        now: Some(now),
        prefix: Some(item.prefix),
        marker,
        ..HoleValues::default()
    };
    template::encode_template(
        template,
        bytes,
        &values,
        Payload::Slice(&[]),
        buf,
        &describe_list(None),
    )
}

/// Reads the response head of a listing and reports what Azure did.
///
/// A head that reports a failure is an outcome too, so it returns [`Ok`].
/// Only a head that cannot be read is an [`Err`].
///
/// # Errors
///
/// Returns [`Error::Response`] if the head cannot be read. A success
/// status that a listing never returns is [`ResponseFault::Status`].
pub fn accept_list_head<'h>(head: ResponseHead<'h>) -> Result<ListHeadOutcome<'h>> {
    match head.status {
        200 => Ok(ListHeadOutcome::Page {
            expected_len: read_decimal_header(head.content_length)?,
        }),
        201..=299 => Err(ResponseFault::Status.into()),
        _ => Ok(read_failure_head(&head)),
    }
}

/// Finishes a [`ListHeadOutcome::NeedErrorBody`] with the response body.
///
/// This is [`accept_get_error_body`] for a listing, and reads the
/// body the same way.
pub fn accept_list_error_body<'h>(failure: Failure<'h>, body: &[u8]) -> ListHeadOutcome<'h> {
    finish_with_body(failure, kind_in_body(body))
}

/// Reads a page out of the response body of a listing.
///
/// Pass the whole body that [`ListHeadOutcome::Page`] announced and an
/// array to write the entries into. Reading is destructive: a body that
/// has been read is no longer a document.
///
/// Your array must hold the whole page. An array of `max_results` entries
/// always does, because the service never writes more than it asked for.
/// The entries are written as [`ListEntry`], or as any type built from
/// one, so a binding fills its own array directly.
///
/// For fields not carried by `ListEntry`, use [`ListEntry::property`] or
/// [`fill_listing_with`], selecting [`BlobProperty`]
/// values such as `VersionId`, `IsCurrentVersion` and `Snapshot`.
///
/// # Errors
///
/// Returns [`Error::Capacity`] if the page holds more entries than the
/// array, with `required` set to the number it holds. The body has been
/// decoded by then and cannot be read again: ask the service for the page
/// again, with a larger array.
///
/// Returns [`Error::Response`] with [`ResponseFault::Body`] if `body` is
/// not a listing page. This reads only the XML that Azure writes. It refuses
/// a namespace prefix, a reference to an entity that no listing declares, an
/// entry tag with an attribute, and anything else that Azure does not write.
pub fn fill_listing<'b, E: From<ListEntry<'b>>>(
    body: &'b mut [u8],
    into: &mut [E],
) -> Result<Listing<'b>> {
    crate::xml::azure::fill_listing(body, into, PropertySet::default(), |entry, _| entry.into())
}

/// Reads a page the way [`fill_listing`] does, and hands you the
/// values of the properties in `wanted` as it goes.
///
/// `build` is called once per entry, with the entry and its values, and
/// what it returns is written into your array. So your entry type can
/// hold the two or three properties you care about, read in the same
/// pass as everything else. The values point into `body`, like the
/// entry. A group of keys gives no values.
///
/// Reading the page costs the same whatever the set holds, and the same
/// as reading it without one.
///
/// ```
/// # use borink_object_storage_proto::azure::{self, BlobProperty, PropertySet};
/// # use borink_object_storage_proto::{ListEntry, Result};
/// # fn read(body: &mut [u8]) -> Result<()> {
/// let wanted = PropertySet::of(&[
///     BlobProperty::VersionId, BlobProperty::IsCurrentVersion,
/// ]);
/// let mut entries = [(ListEntry::default(), None, None); 100];
/// azure::fill_listing_with(body, &mut entries, wanted, |entry, values| {
///     (entry, values.get(BlobProperty::VersionId),
///         values.get(BlobProperty::IsCurrentVersion))
/// })?;
/// # Ok(())
/// # }
/// ```
pub fn fill_listing_with<'b, E>(
    body: &'b mut [u8],
    into: &mut [E],
    wanted: PropertySet,
    build: impl FnMut(ListEntry<'b>, PropertyValues<'_, 'b>) -> E,
) -> Result<Listing<'b>> {
    crate::xml::azure::fill_listing(body, into, wanted, build)
}

// The fields that store the content properties, the tags and the access
// tier of a write with the object.
fn push_stored_fields<'p>(fields: &mut FieldList<'p>, options: &WriteOptions<'p>) {
    for (name, value) in options.properties.iter() {
        let header = match name {
            "cache-control" => "x-ms-blob-cache-control",
            "content-disposition" => "x-ms-blob-content-disposition",
            "content-encoding" => "x-ms-blob-content-encoding",
            "content-language" => "x-ms-blob-content-language",
            _ => "x-ms-blob-content-type",
        };
        fields.push(Field::new(header, FieldValue::Bytes(value.as_bytes())));
    }
    if !options.tags.is_empty() {
        fields.push(Field::new("x-ms-tags", FieldValue::Tags(options.tags)));
    }
    if let Some(tier) = options.storage_class {
        fields.push(Field::new(
            "x-ms-access-tier",
            FieldValue::Bytes(tier.as_bytes()),
        ));
    }
}

// The query parameter that names a template's snapshot or version.
fn revision_param(kind: RevisionKind) -> Param<'static> {
    match kind {
        RevisionKind::Current => None,
        RevisionKind::Snapshot => Some(("snapshot", ParamValue::Hole(Hole::Revision))),
        RevisionKind::Version => Some(("versionid", ParamValue::Hole(Hole::Revision))),
    }
}

// Reads the response head of a GET or a HEAD.
fn accept_get<'h>(shape: GetShape, head: ResponseHead<'h>) -> Result<GetHeadOutcome<'h>> {
    let ranged = shape.range != RequestedRange::Whole;
    match head.status {
        206 if !ranged => Err(ResponseFault::Range.into()),
        200 if ranged => Err(ResponseFault::Range.into()),
        // Azure serves no suffix, so this shape did not pass through
        // encode_get.
        206 if matches!(shape.range, RequestedRange::Suffix(_)) => Err(ResponseFault::Range.into()),
        200 | 206 => accept_get_success(shape, head),
        // A conditional status the request did not ask for is a contradiction,
        // not an outcome: nothing in the request explains it.
        304 if !shape.condition.fails_as_not_modified() => Err(ResponseFault::Status.into()),
        304 => Ok(GetHeadOutcome::NotModified { e_tag: head.e_tag }),
        // A 412 is the request's failed condition only if the request carried
        // one and Azure names it. Azure also answers 412 for other
        // reasons, which reach the caller as the service failure they are.
        412 if shape.condition != ConditionKind::None
            && !shape.condition.fails_as_not_modified()
            && kind_in_head(&head) == Some(ServiceErrorKind::Precondition) =>
        {
            Ok(GetHeadOutcome::PreconditionFailed)
        }
        416 => Ok(GetHeadOutcome::RangeNotSatisfiable {
            object_size: match head.content_range.map(parse_content_range) {
                None => None,
                // `bytes */N` is the only form 416 may carry.
                Some(Some(ContentRange::Unsatisfied { total })) => total,
                Some(_) => return Err(ResponseFault::Head.into()),
            },
        }),
        200..=299 => Err(ResponseFault::Status.into()),
        _ => Ok(read_failure_head(&head)),
    }
}

// Reject unsupported names before encoding, without losing the reason.
fn validate_key(key: &str, namespace: AzureNamespace) -> Result<()> {
    if key.is_empty() {
        return Err(InvalidRequest::EmptyKey.into());
    }
    if namespace == AzureNamespace::Flat && name_units(key) > MAX_BLOB_NAME_UNITS {
        return Err(InvalidRequest::KeyTooLong.into());
    }
    // Each pass over the bytes finds what one rule below looks for. None
    // stops early, so each tests many bytes at once. The `/` are counted in
    // runs of 255 bytes, whose count fits a byte.
    let bytes = key.as_bytes();
    let control = bytes
        .iter()
        .fold(false, |control, &byte| control | byte.is_ascii_control());
    let slashes: usize = bytes
        .chunks(255)
        .map(|run| {
            let count = run
                .iter()
                .fold(0u8, |count, &byte| count + u8::from(byte == b'/'));
            usize::from(count)
        })
        .sum();
    let dot_ends = bytes
        .iter()
        .zip(&bytes[1..])
        .fold(false, |dot_ends, (&byte, &next)| {
            dot_ends | (byte == b'.' && next == b'/')
        })
        || bytes.last() == Some(&b'.');
    // Azure refuses an ASCII control character in a name, with 400. Measured
    // for U+0001, U+000B, U+000C, U+000E and U+007F. Testing the bytes is
    // testing the characters: every byte of a character outside ASCII is 0x80
    // or above, and none of those is an ASCII control.
    if control {
        return Err(InvalidRequest::KeyControlCharacter.into());
    }
    // Azure takes a name of 255 `/`-delimited segments and refuses 256,
    // whatever the 254 in its documentation says. Measured by bisection
    // against the live service.
    // At most one segment per byte plus one; str lengths fit isize.
    if slashes + 1 > MAX_BLOB_NAME_SEGMENTS {
        return Err(InvalidRequest::KeyTooManySegments.into());
    }
    // Azure drops a dot from the end of every segment of a name: `dot.` is
    // stored as `dot`, and `dotseg./x` as `dotseg/x`. Measured; see the live
    // suite. A segment ends in a dot where a dot comes before a `/` or ends
    // the name.
    //
    // The same test covers a segment that is only dots. A host resolves `.`
    // and `..` out of the URL before it sends it, as the standard for URLs
    // requires, so those would name another object entirely; measured, as
    // `dots/../up` wrote `up`.
    if dot_ends {
        return Err(InvalidRequest::KeyWouldBeNormalized.into());
    }
    Ok(())
}

/// The most bytes that Azure writes in one `Put Blob` request.
///
/// [`encode_put`] refuses a longer payload with
/// [`InvalidRequest::PayloadTooLarge`]. Write a longer object in blocks. This
/// is a `u64` because it does not fit a 32-bit `usize`.
pub const MAX_PUT_LEN: u64 = 5000 * 1024 * 1024;

// The characters that Azure takes in the key and the value of a tag.
fn azure_tag_char(character: char) -> bool {
    character.is_ascii_alphanumeric() || " +-./:=_".contains(character)
}

// The content properties, tags and access tier that a write stores with the
// object, which `validate_options` checked.
// One header per pair, named with the prefix and the pair's own name. The
// options were validated, so each name and each value is usable in a header.
// Azure names a metadata pair with a C# identifier: ASCII letters, digits
// and underscores, and no leading digit. It refuses anything else with 400
// `InvalidMetadata`. The total size of the pairs is the service's to check;
// this crate is not told which limit applies to the account.
fn validate_metadata(metadata: &[MetadataPair<'_>]) -> Result<()> {
    for (index, pair) in metadata.iter().enumerate() {
        if pair.name.is_empty()
            || pair.name.starts_with(|first: char| first.is_ascii_digit())
            || !pair
                .name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return Err(InvalidRequest::MetadataName.into());
        }
        // A value is sent as one header value. HTTP drops the spaces at
        // either end of one, so a value with those would not be stored as it
        // was given. An empty value is a pair with no text, which Azure
        // stores.
        if !pair.value.is_ascii()
            || pair.value.bytes().any(|byte| byte.is_ascii_control())
            || pair.value.starts_with(' ')
            || pair.value.ends_with(' ')
        {
            return Err(InvalidRequest::MetadataValue.into());
        }
        // Azure matches a name without case, so two pairs that differ only in
        // case name the same pair and the request would say what it stores
        // twice. A request carries a handful of pairs, so each is compared
        // against the ones before it.
        if metadata[..index]
            .iter()
            .any(|earlier| earlier.name.eq_ignore_ascii_case(pair.name))
        {
            return Err(InvalidRequest::MetadataDuplicate.into());
        }
    }
    Ok(())
}

// The content properties that an account returns in UTF-8 as it got them.
fn utf8_properties(namespace: AzureNamespace) -> &'static [&'static str] {
    match namespace {
        AzureNamespace::Flat => &["content-type"],
        AzureNamespace::Hierarchical | AzureNamespace::Unknown => &[],
    }
}

// `has_bytes` says whether the encoder holds the content, which it does not
// for a streamed payload. `checksums` are the client's providers.

/// An element that a listing writes for a blob, other than the four that
/// every [`ListEntry`] carries.
///
/// Name the ones you want in a [`PropertySet`] and read a page with
/// [`azure::fill_listing_with`](crate::azure::fill_listing_with), which
/// hands you their values as it goes. Most are written under the properties
/// element; the ones marked otherwise stand beside it. Read anything that is
/// not listed here with [`ListEntry::properties`].
///
/// The page reader matches each of these by its whole start tag, in
/// `xml/azure/mod.rs`. A property added here is added to that match too, and a
/// test there checks that every one of these is matched.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum BlobProperty {
    /// The access tier: `Hot`, `Cool`, `Cold` or `Archive`.
    AccessTier,
    /// Whether the tier was inferred rather than set.
    AccessTierInferred,
    /// When the tier was last changed.
    AccessTierChangeTime,
    /// The progress of a rehydration out of the archive tier.
    ArchiveStatus,
    /// The access control list, on a hierarchical account listing with permissions.
    Acl,
    /// `BlockBlob`, `PageBlob` or `AppendBlob`.
    BlobType,
    /// When the blob was created, in the form of the `Last-Modified` header.
    CreationTime,
    /// The media type, as stored with the blob.
    ContentType,
    /// The content encoding, as stored with the blob.
    ContentEncoding,
    /// The content language, as stored with the blob.
    ContentLanguage,
    /// The CRC64 of the content, if the service holds one.
    ContentCrc64,
    /// The MD5 of the content, base64, if the service holds one.
    ContentMd5,
    /// The cache control directives, as stored with the blob.
    CacheControl,
    /// The content disposition, as stored with the blob.
    ContentDisposition,
    /// The identifier of the last copy operation onto this blob.
    CopyId,
    /// The state of that copy: `pending`, `success`, `aborted` or `failed`.
    CopyStatus,
    /// The URL that copy read from.
    CopySource,
    /// The bytes copied so far and the total, as `copied/total`.
    CopyProgress,
    /// When that copy finished.
    CopyCompletionTime,
    /// Why that copy failed or was aborted.
    CopyStatusDescription,
    /// When a soft-deleted blob was deleted.
    DeletedTime,
    /// Whether the entry is a soft-deleted blob. Written beside the properties element.
    Deleted,
    /// The encryption scope the blob is stored under.
    EncryptionScope,
    /// When the blob expires, on a hierarchical account.
    ExpiryTime,
    /// The owning group, on a hierarchical account listing with permissions.
    Group,
    /// Whether this version is the current one. Written beside the properties element.
    IsCurrentVersion,
    /// Whether the blob is an incremental copy of a page blob snapshot.
    IncrementalCopy,
    /// Until when the immutability policy holds.
    ImmutabilityPolicyUntilDate,
    /// The immutability policy: `unlocked` or `locked`.
    ImmutabilityPolicyMode,
    /// Whether the blob is leased: `locked` or `unlocked`.
    LeaseStatus,
    /// The state of the lease: `available`, `leased`, `expired`, `breaking` or `broken`.
    LeaseState,
    /// Whether the lease is `infinite` or `fixed`.
    LeaseDuration,
    /// Whether a legal hold is set.
    LegalHold,
    /// The owner, on a hierarchical account listing with permissions.
    Owner,
    /// The POSIX permissions, on a hierarchical account listing with permissions.
    Permissions,
    /// How many days a soft-deleted blob is kept.
    RemainingRetentionDays,
    /// The priority of a rehydration out of the archive tier.
    RehydratePriority,
    /// Whether the blob is encrypted at rest.
    ServerEncrypted,
    /// The snapshot's timestamp, on an entry that names a snapshot. Written
    /// beside the properties element.
    Snapshot,
    /// How many tags the blob has.
    TagCount,
    /// The version's identifier, on an account that keeps versions. Written
    /// beside the properties element.
    VersionId,
    /// The sequence number of a page blob.
    BlobSequenceNumber,
    /// The metadata pairs of the object, as the bytes between the tags of
    /// the `Metadata` element. Pass the value to [`Metadata::new`](crate::Metadata::new) to read
    /// the pairs. Azure writes the element only when the listing asked for
    /// [`ListInclude::METADATA`].
    Metadata,
}

impl BlobProperty {
    /// Every property, in the order of their numbers.
    pub const ALL: &[Self] = &[
        Self::AccessTier,
        Self::AccessTierInferred,
        Self::AccessTierChangeTime,
        Self::ArchiveStatus,
        Self::Acl,
        Self::BlobType,
        Self::CreationTime,
        Self::ContentType,
        Self::ContentEncoding,
        Self::ContentLanguage,
        Self::ContentCrc64,
        Self::ContentMd5,
        Self::CacheControl,
        Self::ContentDisposition,
        Self::CopyId,
        Self::CopyStatus,
        Self::CopySource,
        Self::CopyProgress,
        Self::CopyCompletionTime,
        Self::CopyStatusDescription,
        Self::DeletedTime,
        Self::Deleted,
        Self::EncryptionScope,
        Self::ExpiryTime,
        Self::Group,
        Self::IsCurrentVersion,
        Self::IncrementalCopy,
        Self::ImmutabilityPolicyUntilDate,
        Self::ImmutabilityPolicyMode,
        Self::LeaseStatus,
        Self::LeaseState,
        Self::LeaseDuration,
        Self::LegalHold,
        Self::Owner,
        Self::Permissions,
        Self::RemainingRetentionDays,
        Self::RehydratePriority,
        Self::ServerEncrypted,
        Self::Snapshot,
        Self::TagCount,
        Self::VersionId,
        Self::BlobSequenceNumber,
        Self::Metadata,
    ];

    /// The element name, as the service writes it.
    pub const fn name(self) -> &'static str {
        match self {
            Self::AccessTier => "AccessTier",
            Self::AccessTierInferred => "AccessTierInferred",
            Self::AccessTierChangeTime => "AccessTierChangeTime",
            Self::ArchiveStatus => "ArchiveStatus",
            Self::Acl => "Acl",
            Self::BlobType => "BlobType",
            Self::CreationTime => "Creation-Time",
            Self::ContentType => "Content-Type",
            Self::ContentEncoding => "Content-Encoding",
            Self::ContentLanguage => "Content-Language",
            Self::ContentCrc64 => "Content-CRC64",
            Self::ContentMd5 => "Content-MD5",
            Self::CacheControl => "Cache-Control",
            Self::ContentDisposition => "Content-Disposition",
            Self::CopyId => "CopyId",
            Self::CopyStatus => "CopyStatus",
            Self::CopySource => "CopySource",
            Self::CopyProgress => "CopyProgress",
            Self::CopyCompletionTime => "CopyCompletionTime",
            Self::CopyStatusDescription => "CopyStatusDescription",
            Self::DeletedTime => "DeletedTime",
            Self::Deleted => "Deleted",
            Self::EncryptionScope => "EncryptionScope",
            Self::ExpiryTime => "Expiry-Time",
            Self::Group => "Group",
            Self::IsCurrentVersion => "IsCurrentVersion",
            Self::IncrementalCopy => "IncrementalCopy",
            Self::ImmutabilityPolicyUntilDate => "ImmutabilityPolicyUntilDate",
            Self::ImmutabilityPolicyMode => "ImmutabilityPolicyMode",
            Self::LeaseStatus => "LeaseStatus",
            Self::LeaseState => "LeaseState",
            Self::LeaseDuration => "LeaseDuration",
            Self::LegalHold => "LegalHold",
            Self::Owner => "Owner",
            Self::Permissions => "Permissions",
            Self::RemainingRetentionDays => "RemainingRetentionDays",
            Self::RehydratePriority => "RehydratePriority",
            Self::ServerEncrypted => "ServerEncrypted",
            Self::Snapshot => "Snapshot",
            Self::TagCount => "TagCount",
            Self::VersionId => "VersionId",
            Self::BlobSequenceNumber => "x-ms-blob-sequence-number",
            Self::Metadata => "Metadata",
        }
    }

    // Whether the element holds other elements rather than one text. The
    // page reader reads such an element to its close tag and reports
    // everything between the tags.
    pub(crate) const fn holds_elements(self) -> bool {
        matches!(self, Self::Metadata)
    }
}

// from_bits shifts by COUNT, which must be below the u64 shift width.
const _: () = assert!(BlobProperty::ALL.len() < 64);

impl BlobProperty {
    /// How many properties there are, which is the most a set can hold.
    pub const COUNT: usize = Self::ALL.len();

    // The property that an element name stands for, if it is one of these.
    // Used where a tag was not matched whole: an unusual spelling, or an
    // element that is none of them.
    pub(crate) fn identify(name: &[u8]) -> Option<Self> {
        // The names of `Self::name`, matched whole: a switch on the length
        // and one compare.
        Some(match name {
            b"AccessTier" => Self::AccessTier,
            b"AccessTierInferred" => Self::AccessTierInferred,
            b"AccessTierChangeTime" => Self::AccessTierChangeTime,
            b"ArchiveStatus" => Self::ArchiveStatus,
            b"Acl" => Self::Acl,
            b"BlobType" => Self::BlobType,
            b"Creation-Time" => Self::CreationTime,
            b"Content-Type" => Self::ContentType,
            b"Content-Encoding" => Self::ContentEncoding,
            b"Content-Language" => Self::ContentLanguage,
            b"Content-CRC64" => Self::ContentCrc64,
            b"Content-MD5" => Self::ContentMd5,
            b"Cache-Control" => Self::CacheControl,
            b"Content-Disposition" => Self::ContentDisposition,
            b"CopyId" => Self::CopyId,
            b"CopyStatus" => Self::CopyStatus,
            b"CopySource" => Self::CopySource,
            b"CopyProgress" => Self::CopyProgress,
            b"CopyCompletionTime" => Self::CopyCompletionTime,
            b"CopyStatusDescription" => Self::CopyStatusDescription,
            b"DeletedTime" => Self::DeletedTime,
            b"Deleted" => Self::Deleted,
            b"EncryptionScope" => Self::EncryptionScope,
            b"Expiry-Time" => Self::ExpiryTime,
            b"Group" => Self::Group,
            b"IsCurrentVersion" => Self::IsCurrentVersion,
            b"IncrementalCopy" => Self::IncrementalCopy,
            b"ImmutabilityPolicyUntilDate" => Self::ImmutabilityPolicyUntilDate,
            b"ImmutabilityPolicyMode" => Self::ImmutabilityPolicyMode,
            b"LeaseStatus" => Self::LeaseStatus,
            b"LeaseState" => Self::LeaseState,
            b"LeaseDuration" => Self::LeaseDuration,
            b"LegalHold" => Self::LegalHold,
            b"Owner" => Self::Owner,
            b"Permissions" => Self::Permissions,
            b"RemainingRetentionDays" => Self::RemainingRetentionDays,
            b"RehydratePriority" => Self::RehydratePriority,
            b"ServerEncrypted" => Self::ServerEncrypted,
            b"Snapshot" => Self::Snapshot,
            b"TagCount" => Self::TagCount,
            b"VersionId" => Self::VersionId,
            b"x-ms-blob-sequence-number" => Self::BlobSequenceNumber,
            b"Metadata" => Self::Metadata,
            _ => return None,
        })
    }
}

impl Sealed for BlobProperty {}

impl Property for BlobProperty {
    const COUNT: usize = BlobProperty::ALL.len();

    fn number(self) -> u8 {
        self as u8
    }
}

/// The properties that one read of an Azure page is asked for.
///
/// Build one with [`PropertySet::of`] and pass it to [`fill_listing_with`].
pub type PropertySet = crate::properties::PropertySet<BlobProperty>;

/// The values that one entry of an Azure listing gave for the properties of a
/// [`PropertySet`].
pub type PropertyValues<'x, 'b> = crate::properties::PropertyValues<'x, 'b, BlobProperty>;

impl PropertySet {
    /// A set of these properties. Naming one twice is the same as once.
    pub const fn of(properties: &[BlobProperty]) -> Self {
        let mut bits = 0;
        let mut i = 0;
        while i < properties.len() {
            bits |= 1 << (properties[i] as u8);
            i += 1;
        }
        Self::new(bits)
    }
}

#[cfg(test)]
mod property_tests {
    use super::{BlobProperty, PropertySet};

    #[test]
    fn every_property_is_found_by_its_name_and_numbered_in_order() {
        for (index, property) in BlobProperty::ALL.iter().enumerate() {
            assert_eq!(*property as usize, index);
            assert_eq!(
                BlobProperty::identify(property.name().as_bytes()),
                Some(*property)
            );
        }
        assert_eq!(BlobProperty::identify(b"Name"), None);
        assert_eq!(BlobProperty::identify(b""), None);
    }

    #[test]
    fn a_set_numbers_its_members_in_the_order_the_enum_does() {
        let set = PropertySet::of(&[
            BlobProperty::ServerEncrypted,
            BlobProperty::AccessTier,
            BlobProperty::AccessTier,
            BlobProperty::CreationTime,
        ]);
        assert_eq!(set.len(), 3);
        assert_eq!(set.slot(BlobProperty::AccessTier), 0);
        assert_eq!(set.slot(BlobProperty::CreationTime), 1);
        assert_eq!(set.slot(BlobProperty::ServerEncrypted), 2);
        assert!(!set.contains(BlobProperty::BlobType));
        assert!(PropertySet::default().is_empty());
    }
}
