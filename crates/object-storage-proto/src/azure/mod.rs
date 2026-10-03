//! Azure Blob Storage requests and responses.
//!
//! Structured-body framing is not implemented.

use core::fmt;

use crate::checksum::{ChecksumKind, ChecksumProvider, KINDS};
use crate::common::{
    ContentRange, FailureOutcome, accept_success, decimal_header, encoded, failure,
    finish_with_body, meta_of, missing, parse_content_range, push_checksum, push_condition,
    text_header, trim_ascii, valid_header, validate_checksum, validate_condition,
    validate_properties, validate_revision, validate_tags, write_range, write_tags,
};
use crate::request::{ByteSink, HeadWriter, U64Decimal, Writer};
use crate::url::{self, Parameter, QueryValue};
use crate::{
    Classification, ConditionKind, DeleteHeadOutcome, DeleteKind, DeleteShape, Error, Failure,
    GetHeadOutcome, GetKind, GetShape, HeaderSpan, InvalidPlan, ListEntry, ListHeadOutcome,
    ListInclude, Listing, MetadataPair, Method, ObjectMeta, Payload, PhysicalDelete, PhysicalGet,
    PhysicalList, PhysicalPut, PropertySet, PropertyValues, PutHeadOutcome, PutShape,
    RequestedRange, ResponseFault, ResponseHead, Result, Revision, ServiceErrorKind, Timestamps,
    TransactionalChecksum, WireRequest, WriteOptions,
};

mod batch;
mod blocks;
mod copy;
mod tags;

pub use batch::{BatchResult, MAX_BATCH_KEYS};
pub use blocks::{
    Block, BlockListKind, BlockRef, BlockResponseHead, BlockSource, BlockState, MAX_STAGE_LEN,
    PhysicalListBlocks, PhysicalStageBlock,
};
pub use copy::PhysicalStageBlockFromUrl;

/// The most recent Azure Storage version that every region supports.
///
/// See the [Azure Storage service version lifecycle](https://learn.microsoft.com/en-us/rest/api/storageservices/versioning-for-the-azure-storage-services).
pub const VERSION: &str = "2026-04-06";

/// The most bytes of URL that Azure reads: the scheme, the host, the path
/// and the query together.
///
/// Every encoding method refuses a longer URL with
/// [`InvalidPlan::UrlTooLong`]. Azure answers one with HTTP 414 and no
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
    let (prefix, name) = header.split_at_checked(METADATA_PREFIX.len())?;
    (prefix.eq_ignore_ascii_case(METADATA_PREFIX) && !name.is_empty()).then_some(name)
}

/// An Azure Blob endpoint and container name, both borrowed.
#[derive(Debug, Clone, Copy)]
pub struct Container<'a> {
    endpoint: &'a str,
    name: &'a str,
}

impl<'a> Container<'a> {
    /// Creates a container reference from an origin and a container name.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidEndpoint`] if `endpoint` is not an ASCII HTTP
    /// or HTTPS origin.
    ///
    /// Returns [`Error::InvalidContainer`] if `name` is empty, or if it
    /// contains bytes that would change the structure of the request.
    pub fn new(endpoint: &'a str, name: &'a str) -> Result<Self> {
        if !crate::http::valid_http_origin(endpoint) {
            return Err(Error::InvalidEndpoint);
        }
        if name.is_empty()
            || name
                .bytes()
                .any(|byte| matches!(byte, b'/' | b'?' | b'#') || byte.is_ascii_control())
        {
            return Err(Error::InvalidContainer);
        }
        Ok(Self { endpoint, name })
    }
}

/// The Azure Blob operations that one bearer token authorizes.
///
/// This is a small borrowed value, and it is [`Copy`]. Create it once per
/// token. Creating it for every request also works: each creation checks
/// the token as a header value, and nothing else.
///
/// Every method that encodes a request takes the current time in `now`,
/// because this crate never reads the clock.
#[derive(Clone, Copy)]
pub struct Blobs<'a> {
    container: Container<'a>,
    pub(crate) token: &'a str,
    pub(crate) namespace: AzureNamespace,
    pub(crate) checksums: [Option<ChecksumProvider>; KINDS],
}

impl core::fmt::Debug for Blobs<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Blobs")
            .field("container", &self.container)
            .field("token", &"<redacted>")
            .field("namespace", &self.namespace)
            .field("checksums", &self.checksums)
            .finish()
    }
}

/// The kind of storage account that a client talks to.
///
/// [`Blobs::with_namespace`] sets it and [`InvalidPlan::azure_rejection`]
/// reads it. This crate never discovers it: no request or response says
/// which kind of account answered.
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

impl InvalidPlan {
    /// Returns the known Azure rejection corresponding to this reason.
    ///
    /// Returns `None` when no mapping is established for the namespace,
    /// including names or ranges Azure may accept with unwanted behavior.
    /// The local reason remains authoritative; this predicts neither error
    /// precedence nor the outcome of a request containing other faults.
    ///
    /// ```
    /// use borink_object_storage_proto::{AzureNamespace, InvalidPlan};
    /// let reason = InvalidPlan::KeyTooLong;
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

impl<'a> Blobs<'a> {
    /// Creates a client from a container and a bearer token.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidToken`] if `token` is not usable as one HTTP
    /// header value.
    pub fn new(container: Container<'a>, token: &'a str) -> Result<Self> {
        if !valid_header(token.as_bytes()) {
            return Err(Error::InvalidToken);
        }
        Ok(Self {
            container,
            token,
            namespace: AzureNamespace::Unknown,
            checksums: [None; KINDS],
        })
    }

    /// Returns this client with the account's namespace set.
    ///
    /// A client told it is on a flat account refuses a key of more than 1,024
    /// UTF-16 code units as [`InvalidPlan::KeyTooLong`]. Any other client
    /// sends it and reports what the service answers.
    pub const fn with_namespace(mut self, namespace: AzureNamespace) -> Self {
        self.namespace = namespace;
        self
    }

    /// Returns this client with `provider` registered for the kind that it
    /// computes.
    ///
    /// A write that asks for [`TransactionalChecksum::Compute`] of that kind
    /// then has the encoder compute the checksum. Register a provider for
    /// each kind you compute: the encoder refuses `Compute` of a kind with no
    /// provider as [`InvalidPlan::Option`]. Registering a kind twice keeps
    /// the later provider. A checksum that you pass as text needs no
    /// provider.
    ///
    /// Azure takes an MD5 and a CRC64, and the encoder refuses `Compute` of
    /// any other kind with [`InvalidPlan::Option`]. The
    /// `borink-object-storage-crypto` crate has providers for both.
    pub const fn with_checksum(mut self, provider: ChecksumProvider) -> Self {
        self.checksums[provider.kind().slot()] = Some(provider);
        self
    }

    /// Writes the request head for `get` into `buf`.
    ///
    /// This method allocates nothing. It writes the URL and the header values
    /// into `buf`, records their spans in `headers`, and returns a
    /// [`WireRequest`] that borrows both buffers.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`] if `get` cannot become an Azure
    /// request. This method validates the plan before it writes any byte, so
    /// it never reports an invalid plan as a capacity error.
    ///
    /// Returns [`Error::Capacity`] if `buf` or `headers` is too small, with
    /// the required bytes and header slots. Grow both buffers and retry, or call
    /// [`layered::get_requirements`](crate::layered::get_requirements) first.
    pub fn encode_get<'r>(
        &self,
        buf: &'r mut [u8],
        headers: &'r mut [HeaderSpan],
        get: &PhysicalGet<'_>,
        now: &Timestamps,
    ) -> Result<WireRequest<'r>> {
        validate_get(get, self.namespace)?;
        let mut head = HeadWriter::new(buf, headers);
        let query = [revision_parameter(get.revision)];
        self.build(&mut head, Some(get.key), &query, get.range, now)?;
        push_condition(&mut head, get.condition, get.condition_value);
        let method = match get.kind {
            GetKind::Bytes => Method::Get,
            GetKind::Head => Method::Head,
        };
        encoded(head, method, Payload::Slice(&[]))
    }

    /// Writes the request head for `put` into `buf`.
    ///
    /// The head states the length of `content`. If you pass
    /// [`Payload::Slice`], the returned request borrows those bytes and copies
    /// none of them. If you pass [`Payload::Streamed`], the request carries no
    /// content and you send the stated number of bytes yourself.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`] if `put` cannot become an Azure request,
    /// or if `content` is longer than Azure writes in one request. This method
    /// validates the plan before it writes any byte, so it never reports an
    /// invalid plan as a capacity error.
    ///
    /// Returns [`Error::Capacity`] if `buf` or `headers` is too small, with
    /// the required bytes and header slots. Grow both buffers and retry, or call
    /// [`layered::put_requirements`](crate::layered::put_requirements) first.
    pub fn encode_put<'r>(
        &self,
        buf: &'r mut [u8],
        headers: &'r mut [HeaderSpan],
        put: &PhysicalPut<'_>,
        content: Payload<'r>,
        now: &Timestamps,
    ) -> Result<WireRequest<'r>> {
        validate_put(put, content, self)?;
        let length = content.len();
        let mut head = HeadWriter::new(buf, headers);
        self.build(&mut head, Some(put.key), &[], RequestedRange::Whole, now)?;
        head.header("x-ms-blob-type", |out| out.push(b"BlockBlob"));
        // The content length is head bytes like any other, so it is written
        // into the caller's buffer rather than formatted at send time.
        head.header("content-length", |out| {
            out.push(U64Decimal::new(length).as_bytes());
        });
        push_checksum(&mut head, put.options.checksum, &self.checksums, |sum| {
            sum.update(content.bytes().unwrap_or(&[]));
        });
        push_stored(&mut head, &put.options);
        push_metadata(&mut head, put.metadata);
        push_condition(&mut head, put.condition, put.condition_value);
        encoded(head, Method::Put, content)
    }

    // The parts that every request head carries, in the order that they are
    // written into the caller's buffer. Each part is one range of that buffer.
    // `key` is `None` for a request that names the container alone, and the
    // query is written in the order it is given.
    //
    // The URL is the one part whose length depends on the endpoint and the
    // container as well as on the plan, so it is the one rule that cannot be
    // checked on the plan alone. It is counted into nothing first, so that
    // a refusal is reported before any byte reaches the caller's buffer.
    pub(crate) fn build(
        &self,
        head: &mut HeadWriter<'_>,
        key: Option<&str>,
        query: &[Parameter<'_>],
        range: RequestedRange,
        now: &Timestamps,
    ) -> Result<()> {
        let mut counted = Writer::new(&mut []);
        self.write_url(&mut counted, key, query);
        if counted.position() > MAX_URL_LEN {
            return Err(InvalidPlan::UrlTooLong.into());
        }
        head.url(|out| self.write_url(out, key, query));
        head.header("authorization", |out| {
            out.push(b"Bearer ");
            out.push(self.token.as_bytes());
        });
        head.header("x-ms-date", |out| out.push(now.rfc1123().as_bytes()));
        head.header("x-ms-version", |out| out.push(VERSION.as_bytes()));
        if range != RequestedRange::Whole {
            head.header("range", |out| write_range(out, range));
        }
        Ok(())
    }

    fn write_url(&self, out: &mut dyn ByteSink, key: Option<&str>, query: &[Parameter<'_>]) {
        out.push(self.container.endpoint.as_bytes());
        self.write_path(out, key);
        url::write_query_in_url(out, query);
    }

    // The path of the URL: the container, and the object if `key` names one.
    pub(crate) fn write_path(&self, out: &mut dyn ByteSink, key: Option<&str>) {
        out.push(b"/");
        out.push(self.container.name.as_bytes());
        if let Some(key) = key {
            out.push(b"/");
            for part in url::encode_object_key(key) {
                out.push(part);
            }
        }
    }

    /// Reads a response head and reports what to do next.
    ///
    /// Pass the same `shape` that you passed to [`Self::encode_get`]. This
    /// method checks the head against that plan, so you never restate what the
    /// plan already holds.
    ///
    /// Every head that Azure sends becomes a [`GetHeadOutcome`], including the
    /// heads that report a failure. Azure names its errors in the
    /// `x-ms-error-code` header, so this method needs no part of the response
    /// body and returns the named error with the outcome. If Azure sent no
    /// such header, the outcome names no error: call [`classify_error`] with
    /// the response body to read the error code from there.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Response`] if the head cannot be read against `shape`.
    /// A `Content-Range` whose end is before its start is
    /// [`ResponseFault::Head`]. A ranged plan that Azure answers with status
    /// 200 is [`ResponseFault::Range`].
    ///
    /// A 412 is [`GetHeadOutcome::PreconditionFailed`] only if the plan
    /// carried `If-Match` and Azure names a failed condition. Any other 412 is
    /// a service failure that names its code.
    pub fn accept_get_head<'h>(
        &self,
        shape: GetShape,
        head: ResponseHead<'h>,
    ) -> Result<GetHeadOutcome<'h>> {
        let ranged = shape.range != RequestedRange::Whole;
        match head.status {
            206 if !ranged => Err(ResponseFault::Range.into()),
            200 if ranged => Err(ResponseFault::Range.into()),
            // Azure serves no suffix, so this shape did not pass through
            // encode_get.
            206 if matches!(shape.range, RequestedRange::Suffix(_)) => {
                Err(ResponseFault::Range.into())
            }
            200 | 206 => accept_success(shape, head),
            // A conditional status the plan did not ask for is a contradiction,
            // not an outcome: nothing in the plan explains it.
            304 if !shape.condition.fails_as_not_modified() => Err(ResponseFault::Status.into()),
            304 => Ok(GetHeadOutcome::NotModified { e_tag: head.e_tag }),
            // A 412 is the plan's failed condition only if the plan carried
            // one and Azure names it. Azure also answers 412 for other
            // reasons, which reach the caller as the service failure they are.
            412 if shape.condition != ConditionKind::None
                && !shape.condition.fails_as_not_modified()
                && named(&head) == Some(ServiceErrorKind::Precondition) =>
            {
                Ok(GetHeadOutcome::PreconditionFailed)
            }
            // Azure repeats the header's code in the body, so only a
            // missing header is worth a body read. A header naming a code
            // this crate does not know is already decisive.
            404 if head.error_code.is_none() => Ok(GetHeadOutcome::NeedErrorBody(failure(
                404,
                None,
                head.request_id,
            ))),
            404 => Ok(missing(&head, named(&head))),
            416 => Ok(GetHeadOutcome::RangeNotSatisfiable {
                object_size: match head.content_range.map(parse_content_range) {
                    None => None,
                    // `bytes */N` is the only form 416 may carry.
                    Some(Some(ContentRange::Unsatisfied { total })) => total,
                    Some(_) => return Err(ResponseFault::Head.into()),
                },
            }),
            200..=299 => Err(ResponseFault::Status.into()),
            status if head.error_code.is_none() => Ok(GetHeadOutcome::NeedErrorBody(failure(
                status,
                None,
                head.request_id,
            ))),
            status => Ok(GetHeadOutcome::ServiceFailure(failure(
                status,
                named(&head),
                head.request_id,
            ))),
        }
    }

    /// Finishes a [`GetHeadOutcome::NeedErrorBody`] with the response body.
    ///
    /// Pass the `shape` that you passed to [`Self::accept_get_head`], the
    /// [`Failure`] of that outcome, and the body that you
    /// read. The body names the error, exactly as the `x-ms-error-code`
    /// header would have. Pass an empty body if you could not read one: the
    /// outcome is then final with the error unnamed.
    ///
    /// To tell a body that your read limit cut short from a body that names an
    /// error this crate does not recognize, call [`classify_error`] instead.
    pub fn accept_get_error_body<'h>(
        &self,
        shape: GetShape,
        failure: Failure<'h>,
        body: &[u8],
    ) -> GetHeadOutcome<'h> {
        let kind = body_kind(body);
        // A read fails `If-None-Match` and `If-Modified-Since` with 304, so
        // only `If-Match` and `If-Unmodified-Since` fail with 412.
        if names_failed_condition(
            failure.status,
            shape.condition != ConditionKind::None && !shape.condition.fails_as_not_modified(),
            kind,
        ) {
            return GetHeadOutcome::PreconditionFailed;
        }
        finish_with_body(failure, kind)
    }

    /// Writes the request head for `delete` into `buf`.
    ///
    /// The request has no content. Azure removes the object it names and
    /// nothing else: see [`PhysicalDelete`] for what that excludes.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`] if `delete` cannot become an Azure
    /// request. This method validates the plan before it writes any byte, so
    /// it never reports an invalid plan as a capacity error.
    ///
    /// Returns [`Error::Capacity`] if `buf` or `headers` is too small, with
    /// the required bytes and header slots. Grow both buffers and retry, or call
    /// [`layered::delete_requirements`](crate::layered::delete_requirements)
    /// first.
    pub fn encode_delete<'r>(
        &self,
        buf: &'r mut [u8],
        headers: &'r mut [HeaderSpan],
        delete: &PhysicalDelete<'_>,
        now: &Timestamps,
    ) -> Result<WireRequest<'r>> {
        validate_delete(delete, self.namespace)?;
        let mut head = HeadWriter::new(buf, headers);
        let query = [revision_parameter(delete.revision)];
        self.build(
            &mut head,
            Some(delete.key),
            &query,
            RequestedRange::Whole,
            now,
        )?;
        if let Some(value) = delete_snapshots(delete.kind) {
            head.header("x-ms-delete-snapshots", |out| out.push(value.as_bytes()));
        }
        push_condition(&mut head, delete.condition, delete.condition_value);
        encoded(head, Method::Delete, Payload::Slice(&[]))
    }

    /// Reads the response head of a removal and reports what Azure did.
    ///
    /// Pass the same `shape` that you passed to [`Self::encode_delete`]. This
    /// method checks the head against that plan, so you never restate what the
    /// plan already holds.
    ///
    /// Every head that Azure sends becomes a [`DeleteHeadOutcome`], including
    /// the heads that report a failure.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Response`] if the head cannot be read against `shape`.
    /// A success status that a removal never returns is
    /// [`ResponseFault::Status`].
    ///
    /// A 412 is [`DeleteHeadOutcome::PreconditionFailed`] only if the plan
    /// carried a condition and Azure names a failed condition. Azure answers
    /// 412 for other reasons too, such as `LeaseIdMissing` on a leased blob,
    /// and those are service failures that name their code.
    pub fn accept_delete_head<'h>(
        &self,
        shape: DeleteShape,
        head: ResponseHead<'h>,
    ) -> Result<DeleteHeadOutcome<'h>> {
        match head.status {
            202 => Ok(DeleteHeadOutcome::Accepted),
            412 if shape.condition != ConditionKind::None
                && named(&head) == Some(ServiceErrorKind::Precondition) =>
            {
                Ok(DeleteHeadOutcome::PreconditionFailed)
            }
            404 if head.error_code.is_none() => Ok(DeleteHeadOutcome::NeedErrorBody(failure(
                404,
                None,
                head.request_id,
            ))),
            404 => Ok(missing(&head, named(&head))),
            200..=299 => Err(ResponseFault::Status.into()),
            status if head.error_code.is_none() => Ok(DeleteHeadOutcome::NeedErrorBody(failure(
                status,
                None,
                head.request_id,
            ))),
            status => Ok(DeleteHeadOutcome::ServiceFailure(failure(
                status,
                named(&head),
                head.request_id,
            ))),
        }
    }

    /// Finishes a [`DeleteHeadOutcome::NeedErrorBody`] with the response body.
    ///
    /// This is [`Self::accept_get_error_body`] for a removal, and reads the
    /// body the same way.
    pub fn accept_delete_error_body<'h>(
        &self,
        shape: DeleteShape,
        failure: Failure<'h>,
        body: &[u8],
    ) -> DeleteHeadOutcome<'h> {
        let kind = body_kind(body);
        if names_failed_condition(failure.status, shape.condition != ConditionKind::None, kind) {
            return DeleteHeadOutcome::PreconditionFailed;
        }
        finish_with_body(failure, kind)
    }

    /// Writes the request head of a Snapshot Blob into `buf`, which takes a
    /// read-only snapshot of the object as it is now.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`] for a key that [`Self::encode_get`]
    /// refuses, a metadata pair that [`Self::encode_put`] refuses, or an
    /// invalid condition.
    ///
    /// Returns [`Error::Capacity`] if `buf` or `headers` is too small, with
    /// the required bytes and header slots, or call
    /// [`layered::snapshot_requirements`](crate::layered::snapshot_requirements)
    /// first.
    pub fn encode_snapshot<'r>(
        &self,
        buf: &'r mut [u8],
        headers: &'r mut [HeaderSpan],
        plan: &PhysicalSnapshot<'_>,
        now: &Timestamps,
    ) -> Result<WireRequest<'r>> {
        validate_key(plan.key, self.namespace)?;
        validate_metadata(plan.metadata)?;
        validate_condition(plan.condition, plan.condition_value)?;
        let mut head = HeadWriter::new(buf, headers);
        self.build(
            &mut head,
            Some(plan.key),
            &[Some(("comp", QueryValue::Literal("snapshot")))],
            RequestedRange::Whole,
            now,
        )?;
        head.header("content-length", |out| out.push(b"0"));
        push_metadata(&mut head, plan.metadata);
        push_condition(&mut head, plan.condition, plan.condition_value);
        encoded(head, Method::Put, Payload::Slice(&[]))
    }

    /// Reads the response head of a Snapshot Blob and reports what Azure
    /// did.
    ///
    /// Pass the condition of the plan. A 412 is
    /// [`SnapshotHeadOutcome::PreconditionFailed`] only if the plan carried
    /// one and Azure names a failed condition.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Response`] if the head cannot be read. A success
    /// status other than 201 is [`ResponseFault::Status`], and a 201 without
    /// `x-ms-snapshot` is [`ResponseFault::Head`].
    pub fn accept_snapshot_head<'h>(
        &self,
        condition: ConditionKind,
        head: ResponseHead<'h>,
    ) -> Result<SnapshotHeadOutcome<'h>> {
        match head.status {
            201 => Ok(SnapshotHeadOutcome::Created {
                snapshot: text_header(head.snapshot)?.ok_or(ResponseFault::Head)?,
                meta: ObjectMeta {
                    last_modified: text_header(head.last_modified)?,
                    ..meta_of(head)
                },
            }),
            412 if condition != ConditionKind::None
                && named(&head) == Some(ServiceErrorKind::Precondition) =>
            {
                Ok(SnapshotHeadOutcome::PreconditionFailed)
            }
            404 if head.error_code.is_some() => Ok(missing(&head, named(&head))),
            200..=299 => Err(ResponseFault::Status.into()),
            status if head.error_code.is_none() => Ok(SnapshotHeadOutcome::NeedErrorBody(failure(
                status,
                None,
                head.request_id,
            ))),
            status => Ok(SnapshotHeadOutcome::ServiceFailure(failure(
                status,
                named(&head),
                head.request_id,
            ))),
        }
    }

    /// Finishes a [`SnapshotHeadOutcome::NeedErrorBody`] with the response
    /// body.
    ///
    /// This is [`Self::accept_get_error_body`] for a Snapshot Blob, and reads
    /// the body the same way.
    pub fn accept_snapshot_error_body<'h>(
        &self,
        condition: ConditionKind,
        failure: Failure<'h>,
        body: &[u8],
    ) -> SnapshotHeadOutcome<'h> {
        let kind = body_kind(body);
        if names_failed_condition(failure.status, condition != ConditionKind::None, kind) {
            return SnapshotHeadOutcome::PreconditionFailed;
        }
        finish_with_body(failure, kind)
    }

    /// Reads the response head of a write and reports what Azure did.
    ///
    /// Pass the same `shape` that you passed to [`Self::encode_put`]. This
    /// method checks the head against that plan, so you never restate what the
    /// plan already holds.
    ///
    /// Every head that Azure sends becomes a [`PutHeadOutcome`], including the
    /// heads that report a failure.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Response`] if the head cannot be read against `shape`.
    /// A success status that a write never returns is
    /// [`ResponseFault::Status`].
    ///
    /// A 412 is [`PutHeadOutcome::PreconditionFailed`] only if the plan carried
    /// a condition and Azure names a failed condition. Azure answers 412 for
    /// other reasons too, such as `LeaseIdMissing` on a leased blob, and those
    /// are service failures that name their code.
    pub fn accept_put_head<'h>(
        &self,
        shape: PutShape,
        head: ResponseHead<'h>,
    ) -> Result<PutHeadOutcome<'h>> {
        match head.status {
            201 => Ok(PutHeadOutcome::Created {
                meta: ObjectMeta {
                    last_modified: text_header(head.last_modified)?,
                    ..meta_of(head)
                },
            }),
            412 if shape.condition != ConditionKind::None
                && named(&head) == Some(ServiceErrorKind::Precondition) =>
            {
                Ok(PutHeadOutcome::PreconditionFailed)
            }
            404 if head.error_code.is_none() => Ok(PutHeadOutcome::NeedErrorBody(failure(
                404,
                None,
                head.request_id,
            ))),
            404 => Ok(missing(&head, named(&head))),
            200..=299 => Err(ResponseFault::Status.into()),
            status if head.error_code.is_none() => Ok(PutHeadOutcome::NeedErrorBody(failure(
                status,
                None,
                head.request_id,
            ))),
            status => Ok(PutHeadOutcome::ServiceFailure(failure(
                status,
                named(&head),
                head.request_id,
            ))),
        }
    }

    /// Finishes a [`PutHeadOutcome::NeedErrorBody`] with the response body.
    ///
    /// This is [`Self::accept_get_error_body`] for a write, and reads the
    /// body the same way.
    pub fn accept_put_error_body<'h>(
        &self,
        shape: PutShape,
        failure: Failure<'h>,
        body: &[u8],
    ) -> PutHeadOutcome<'h> {
        let kind = body_kind(body);
        if names_failed_condition(failure.status, shape.condition != ConditionKind::None, kind) {
            return PutHeadOutcome::PreconditionFailed;
        }
        finish_with_body(failure, kind)
    }

    /// Writes the request head for one page of `list` into `buf`.
    ///
    /// The response carries the page as a document in its body: read it whole
    /// and pass it to [`Self::fill_listing`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPlan`] if `list` cannot become an Azure
    /// request. Azure lists from no key and names no owner, so a plan that
    /// sets [`PhysicalList::start_after`] or [`ListInclude::OWNER`] is refused
    /// with [`InvalidPlan::Option`]. This method validates the plan before it
    /// writes any byte, so it never reports an invalid plan as a capacity
    /// error.
    ///
    /// Returns [`Error::Capacity`] if `buf` or `headers` is too small, with
    /// the required bytes and header slots. Grow both buffers and retry, or call
    /// [`layered::list_requirements`](crate::layered::list_requirements)
    /// first.
    pub fn encode_list<'r>(
        &self,
        buf: &'r mut [u8],
        headers: &'r mut [HeaderSpan],
        list: &PhysicalList<'_>,
        now: &Timestamps,
    ) -> Result<WireRequest<'r>> {
        validate_list(list, self.namespace)?;
        let mut words = [""; INCLUDE_WORDS.len()];
        let mut word_count = 0;
        for (flag, word) in INCLUDE_WORDS {
            if list.include.contains(flag) {
                words[word_count] = word;
                word_count += 1;
            }
        }
        // The query is written in this order every time, so a caller can
        // compare the URL byte for byte. Azure signs none of it.
        let query = [
            Some(("restype", QueryValue::Literal("container"))),
            Some(("comp", QueryValue::Literal("list"))),
            (!list.prefix.is_empty())
                .then_some(("prefix", QueryValue::Encoded(list.prefix.as_bytes()))),
            list.delimiter
                .map(|delimiter| ("delimiter", QueryValue::Encoded(delimiter.as_bytes()))),
            list.marker
                .map(|marker| ("marker", QueryValue::Encoded(marker.as_bytes()))),
            list.max_results
                .map(|max_results| ("maxresults", QueryValue::Number(max_results))),
            (word_count != 0).then(|| ("include", QueryValue::Words(&words[..word_count]))),
        ];

        let mut head = HeadWriter::new(buf, headers);
        self.build(&mut head, None, &query, RequestedRange::Whole, now)?;
        encoded(head, Method::Get, Payload::Slice(&[]))
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
    pub fn accept_list_head<'h>(&self, head: ResponseHead<'h>) -> Result<ListHeadOutcome<'h>> {
        match head.status {
            200 => Ok(ListHeadOutcome::Page {
                expected_len: decimal_header(head.content_length)?,
            }),
            404 if head.error_code.is_none() => Ok(ListHeadOutcome::NeedErrorBody(failure(
                404,
                None,
                head.request_id,
            ))),
            404 => Ok(missing(&head, named(&head))),
            201..=299 => Err(ResponseFault::Status.into()),
            status if head.error_code.is_none() => Ok(ListHeadOutcome::NeedErrorBody(failure(
                status,
                None,
                head.request_id,
            ))),
            status => Ok(ListHeadOutcome::ServiceFailure(failure(
                status,
                named(&head),
                head.request_id,
            ))),
        }
    }

    /// Finishes a [`ListHeadOutcome::NeedErrorBody`] with the response body.
    ///
    /// This is [`Self::accept_get_error_body`] for a listing, and reads the
    /// body the same way.
    pub fn accept_list_error_body<'h>(
        &self,
        failure: Failure<'h>,
        body: &[u8],
    ) -> ListHeadOutcome<'h> {
        finish_with_body(failure, body_kind(body))
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
    /// [`Self::fill_listing_with`], selecting [`BlobProperty`](crate::BlobProperty)
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
    /// not a listing page. This reads the grammar that Azure writes, not XML
    /// at large: a namespace prefix, a reference to an entity no listing
    /// declares, an entry tag spelled with an attribute, and anything else
    /// Azure does not write are refused rather than guessed at.
    pub fn fill_listing<'b, E: From<ListEntry<'b>>>(
        &self,
        body: &'b mut [u8],
        into: &mut [E],
    ) -> Result<Listing<'b>> {
        crate::xml::azure::fill_listing(body, into, PropertySet::default(), |entry, _| entry.into())
    }

    /// Reads a page the way [`Self::fill_listing`] does, and hands you the
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
    /// # use borink_object_storage_proto::{
    /// #     Blobs, ListEntry, PropertySet, BlobProperty, Result,
    /// # };
    /// # fn read(blobs: &Blobs<'_>, body: &mut [u8]) -> Result<()> {
    /// let wanted = PropertySet::of(&[
    ///     BlobProperty::VersionId, BlobProperty::IsCurrentVersion,
    /// ]);
    /// let mut entries = [(ListEntry::default(), None, None); 100];
    /// blobs.fill_listing_with(body, &mut entries, wanted, |entry, values| {
    ///     (entry, values.get(BlobProperty::VersionId),
    ///         values.get(BlobProperty::IsCurrentVersion))
    /// })?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn fill_listing_with<'b, E>(
        &self,
        body: &'b mut [u8],
        into: &mut [E],
        wanted: PropertySet,
        build: impl FnMut(ListEntry<'b>, PropertyValues<'_, 'b>) -> E,
    ) -> Result<Listing<'b>> {
        crate::xml::azure::fill_listing(body, into, wanted, build)
    }
}

/// One Snapshot Blob: a read-only copy of an object as it is now, which
/// Azure keeps beside the object until the snapshot is removed.
///
/// Read a snapshot with [`Revision::Snapshot`] in [`PhysicalGet::revision`],
/// and remove it with one in [`PhysicalDelete::revision`]. A hierarchical
/// account takes no snapshot: Azure refuses with 409
/// `FeatureNotYetSupportedForHierarchicalNamespaceAccounts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysicalSnapshot<'a> {
    /// The object key, under the rules of [`PhysicalGet::key`].
    pub key: &'a str,
    /// The metadata of the snapshot. With no pair, the snapshot has the
    /// object's metadata; with any, it has these pairs alone. The rules of
    /// [`PhysicalPut::metadata`] apply.
    pub metadata: &'a [MetadataPair<'a>],
    /// The condition on the object, as a read carries it.
    pub condition: ConditionKind,
    /// What `condition` compares against: see [`ConditionKind`].
    pub condition_value: Option<&'a [u8]>,
}

impl<'a> PhysicalSnapshot<'a> {
    /// Creates a plan that takes a snapshot of `key`, with the object's
    /// metadata and no condition.
    pub const fn new(key: &'a str) -> Self {
        Self {
            key,
            metadata: &[],
            condition: ConditionKind::None,
            condition_value: None,
        }
    }
}

/// The result of reading the response head of a Snapshot Blob.
///
/// A head that reports a failure is one of these too.
/// [`Blobs::accept_snapshot_head`] returns an [`Err`] only for a head it
/// cannot read.
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

impl<'h> FailureOutcome<'h> for SnapshotHeadOutcome<'h> {
    fn not_found(failure: Failure<'h>) -> Self {
        Self::NotFound { kind: failure.kind }
    }

    fn service_failure(failure: Failure<'h>) -> Self {
        Self::ServiceFailure(failure)
    }
}

// The word for each include flag. The query writes them in this order,
// whatever order the set was built in.
const INCLUDE_WORDS: [(ListInclude, &str); 3] = [
    (ListInclude::METADATA, "metadata"),
    (ListInclude::SNAPSHOTS, "snapshots"),
    (ListInclude::VERSIONS, "versions"),
];

// The query parameter that names a snapshot or a version, or none for the
// object as it is now.
pub(crate) fn revision_parameter(revision: Option<Revision<'_>>) -> Parameter<'_> {
    match revision? {
        Revision::Snapshot(id) => Some(("snapshot", QueryValue::Encoded(id.as_bytes()))),
        Revision::Version(id) => Some(("versionid", QueryValue::Encoded(id.as_bytes()))),
    }
}

pub(crate) fn named<'h>(head: &ResponseHead<'h>) -> Option<ServiceErrorKind> {
    kind_for_code(trim_ascii(head.error_code.unwrap_or_default()))
}

/// Borrows the native error code, preferring the header over the XML body.
/// Unknown codes are retained unchanged.
pub fn error_code<'a>(head: &ResponseHead<'a>, body: &'a [u8]) -> Option<&'a [u8]> {
    head.error_code
        .map(trim_ascii)
        .or_else(|| crate::xml::error_code(body).map(str::as_bytes))
}

/// Classifies the Azure error that `head` names, or that `body` names if the
/// head carries no error code. Set `truncated` if a read limit stopped the
/// body early.
pub fn classify_error(head: &ResponseHead<'_>, body: &[u8], truncated: bool) -> Classification {
    let code = head
        .error_code
        .map(trim_ascii)
        .or_else(|| crate::xml::error_code(body).map(|code| code.as_bytes()));
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

// Whether a 412 is the plan's failed condition: the plan carried one, and
// Azure names the failure. Azure also answers 412 for other reasons, such as
// `LeaseIdMissing` on a leased blob.
pub(crate) fn names_failed_condition(
    status: u16,
    carried: bool,
    kind: Option<ServiceErrorKind>,
) -> bool {
    status == 412 && carried && kind == Some(ServiceErrorKind::Precondition)
}

// The error that a failed response body names, if it names one this crate
// recognizes.
pub(crate) fn body_kind(body: &[u8]) -> Option<ServiceErrorKind> {
    crate::xml::error_code(body).and_then(|code| kind_for_code(code.as_bytes()))
}

fn delete_snapshots(kind: DeleteKind) -> Option<&'static str> {
    match kind {
        // Azure refuses an object with snapshots when the header is absent,
        // which is the outcome a plan that names the object alone asks for.
        DeleteKind::Object => None,
        DeleteKind::ObjectAndSnapshots => Some("include"),
        DeleteKind::SnapshotsOnly => Some("only"),
    }
}

// The length of a name as Azure counts it.
fn name_units(value: &str) -> usize {
    value.chars().map(char::len_utf16).sum()
}

// Reject unsupported names before encoding, without losing the reason.
pub(crate) fn validate_key(key: &str, namespace: AzureNamespace) -> Result<()> {
    if key.is_empty() {
        return Err(InvalidPlan::EmptyKey.into());
    }
    if namespace == AzureNamespace::Flat && name_units(key) > MAX_BLOB_NAME_UNITS {
        return Err(InvalidPlan::KeyTooLong.into());
    }
    // Azure refuses an ASCII control character in a name, with 400. Measured
    // for U+0001, U+000B, U+000C, U+000E and U+007F. Testing the bytes is
    // testing the characters: every byte of a character outside ASCII is 0x80
    // or above, and none of those is an ASCII control.
    if key.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(InvalidPlan::KeyControlCharacter.into());
    }
    // Azure takes a name of 255 `/`-delimited segments and refuses 256,
    // whatever the 254 in its documentation says. Measured by bisection
    // against the live service.
    // At most one segment per byte plus one; str lengths fit isize.
    if key.matches('/').count() + 1 > MAX_BLOB_NAME_SEGMENTS {
        return Err(InvalidPlan::KeyTooManySegments.into());
    }
    // Azure drops a dot from the end of every segment of a name: `dot.` is
    // stored as `dot`, and `dotseg./x` as `dotseg/x`. Measured; see the live
    // suite.
    //
    // The same test covers a segment that is only dots. A host resolves `.`
    // and `..` out of the URL before it sends it, as the standard for URLs
    // requires, so those would name another object entirely; measured, as
    // `dots/../up` wrote `up`.
    if key.split('/').any(|segment| segment.ends_with('.')) {
        return Err(InvalidPlan::KeyWouldBeNormalized.into());
    }
    Ok(())
}

fn validate_get(get: &PhysicalGet<'_>, namespace: AzureNamespace) -> Result<()> {
    validate_key(get.key, namespace)?;
    match get.range {
        RequestedRange::Bounded { start, end } if start >= end => {
            return Err(InvalidPlan::Range.into());
        }
        RequestedRange::Suffix(_) => return Err(InvalidPlan::UnsupportedRange.into()),
        RequestedRange::Whole => {}
        _ if get.kind == GetKind::Head => {
            return Err(InvalidPlan::RangedHead.into());
        }
        _ => {}
    }
    validate_revision(get.revision, true)?;
    validate_condition(get.condition, get.condition_value)
}

/// The most bytes that Azure writes in one `Put Blob` request.
///
/// [`Blobs::encode_put`] refuses a longer payload with
/// [`InvalidPlan::PayloadTooLarge`]. Write a longer object in blocks. This
/// is a `u64` because it does not fit a 32-bit `usize`.
pub const MAX_PUT_LEN: u64 = 5000 * 1024 * 1024;

fn validate_put(put: &PhysicalPut<'_>, content: Payload<'_>, client: &Blobs<'_>) -> Result<()> {
    validate_key(put.key, client.namespace)?;
    if content.len() > MAX_PUT_LEN {
        return Err(InvalidPlan::PayloadTooLarge.into());
    }
    validate_metadata(put.metadata)?;
    validate_options(
        &put.options,
        Write::Whole,
        content.bytes().is_some(),
        client,
    )?;
    validate_condition(put.condition, put.condition_value)
}

// Checks a checksum that Azure takes: an MD5 or a CRC64, as text or
// computed.
pub(crate) fn validate_azure_checksum(
    checksum: Option<TransactionalChecksum<'_>>,
    has_bytes: bool,
    checksums: &[Option<ChecksumProvider>; KINDS],
) -> Result<()> {
    if let Some(TransactionalChecksum::Compute(kind)) = checksum
        && !kind.azure_takes()
    {
        return Err(InvalidPlan::Option.into());
    }
    validate_checksum(checksum, has_bytes, checksums)
}

// The characters that Azure takes in the key and the value of a tag.
pub(crate) fn azure_tag_char(character: char) -> bool {
    character.is_ascii_alphanumeric() || " +-./:=_".contains(character)
}

// The content properties, tags and access tier that a write stores with the
// object, which `validate_options` checked.
pub(crate) fn push_stored(head: &mut HeadWriter<'_>, options: &WriteOptions<'_>) {
    for (name, value) in options.properties.iter() {
        head.header_parts(
            |out| {
                out.push(b"x-ms-blob-");
                out.push(name.as_bytes());
            },
            |out| out.push(value.as_bytes()),
        );
    }
    if !options.tags.is_empty() {
        head.header("x-ms-tags", |out| write_tags(out, options.tags));
    }
    if let Some(tier) = options.storage_class {
        head.header("x-ms-access-tier", |out| out.push(tier.as_bytes()));
    }
}

// One header per pair, named with the prefix and the pair's own name. The
// plan was validated, so each name and each value is usable in a header.
pub(crate) fn push_metadata(head: &mut HeadWriter<'_>, metadata: &[MetadataPair<'_>]) {
    for pair in metadata {
        head.header_parts(
            |out| {
                out.push(METADATA_PREFIX.as_bytes());
                out.push(pair.name.as_bytes());
            },
            |out| out.push(pair.value.as_bytes()),
        );
    }
}

// Azure names a metadata pair with a C# identifier: ASCII letters, digits
// and underscores, and no leading digit. It refuses anything else with 400
// `InvalidMetadata`. The total size of the pairs is the service's to check;
// this crate is not told which limit applies to the account.
pub(crate) fn validate_metadata(metadata: &[MetadataPair<'_>]) -> Result<()> {
    for (index, pair) in metadata.iter().enumerate() {
        if pair.name.is_empty()
            || pair.name.starts_with(|first: char| first.is_ascii_digit())
            || !pair
                .name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return Err(InvalidPlan::MetadataName.into());
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
            return Err(InvalidPlan::MetadataValue.into());
        }
        // Azure matches a name without case, so two pairs that differ only in
        // case name the same pair and the request would say what it stores
        // twice. A plan carries a handful of pairs, so each is compared
        // against the ones before it.
        if metadata[..index]
            .iter()
            .any(|earlier| earlier.name.eq_ignore_ascii_case(pair.name))
        {
            return Err(InvalidPlan::MetadataDuplicate.into());
        }
    }
    Ok(())
}

// The writes that take options. `validate_options` refuses an option on a
// write that does not take it. `Copy` is a Copy Blob or a Copy Blob From
// URL, which take the source's content properties, and `FromUrl` a Put
// Blob From URL, which takes the plan's.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Write {
    Whole,
    Stage,
    Commit,
    Copy,
    FromUrl,
}

// `has_bytes` says whether the encoder holds the content, which it does not
// for a streamed payload. `checksums` are the client's providers.
pub(crate) fn validate_options(
    options: &WriteOptions<'_>,
    write: Write,
    has_bytes: bool,
    client: &Blobs<'_>,
) -> Result<()> {
    validate_azure_checksum(options.checksum, has_bytes, &client.checksums)?;
    // A copy sends no content, so it has nothing to sum, and a Copy Blob
    // takes no content property.
    let copy = matches!(write, Write::Copy | Write::FromUrl);
    if (copy && options.checksum.is_some())
        || (write == Write::Copy && !options.properties.is_empty())
    {
        return Err(InvalidPlan::Option.into());
    }
    // A block is not an object, so it stores neither properties nor tags.
    let stored = !options.properties.is_empty()
        || !options.tags.is_empty()
        || options.storage_class.is_some();
    if stored && write == Write::Stage {
        return Err(InvalidPlan::Option.into());
    }
    // A flat account returns a Content-Type in UTF-8 as it got it. A
    // hierarchical one takes it but returns each byte as a character, so
    // `é` comes back as the byte e9. Both refuse a Content-Disposition
    // outside ASCII with 400 InvalidMetadata.
    let utf8: &[&str] = match client.namespace {
        AzureNamespace::Flat => &["content-type"],
        AzureNamespace::Hierarchical | AzureNamespace::Unknown => &[],
    };
    validate_properties(options, utf8)?;
    validate_tags(options.tags, azure_tag_char, Some((10, 128, 256)))?;
    if let Some(text) = options.declared_md5 {
        // A whole-object write stores the MD5 that Azure checked, and a block
        // is not an object. Only a commit declares one.
        if write != Write::Commit {
            return Err(InvalidPlan::Option.into());
        }
        ChecksumKind::Md5.check_base64(text)?;
    }
    Ok(())
}

fn validate_list(list: &PhysicalList<'_>, namespace: AzureNamespace) -> Result<()> {
    // No rule of `validate_key` applies to a prefix. It is written into the
    // query, where nothing resolves a `..` and nothing drops a trailing dot,
    // and `dir.` is an honest prefix of `dir.txt`. Nor is it bounded like a
    // name: a flat account answered a prefix of 32,657 units, far past the
    // 1,024 it allows a name. What bounds a prefix is the URL, which `build`
    // checks. Measured against the live service.
    if list.marker.is_some_and(str::is_empty) {
        return Err(InvalidPlan::Marker.into());
    }
    if list.max_results == Some(0) {
        return Err(InvalidPlan::MaxResults.into());
    }
    // A flat account groups names at any text. A hierarchical one groups
    // them at `/` alone, and answers any other with 400
    // `DelimiterIsInvalidForHNS`.
    if let Some(delimiter) = list.delimiter
        && (delimiter.is_empty() || (namespace == AzureNamespace::Hierarchical && delimiter != "/"))
    {
        return Err(InvalidPlan::Delimiter.into());
    }
    // Azure starts a listing only at its own marker, and lists no owner.
    if list.start_after.is_some() || list.include.contains(ListInclude::OWNER) {
        return Err(InvalidPlan::Option.into());
    }
    Ok(())
}

fn validate_delete(delete: &PhysicalDelete<'_>, namespace: AzureNamespace) -> Result<()> {
    validate_key(delete.key, namespace)?;
    validate_revision(delete.revision, true)?;
    // A snapshot or a version has no snapshots of its own, and Azure refuses
    // `x-ms-delete-snapshots` beside one.
    if delete.revision.is_some() && delete.kind != DeleteKind::Object {
        return Err(InvalidPlan::Revision.into());
    }
    validate_condition(delete.condition, delete.condition_value)
}
