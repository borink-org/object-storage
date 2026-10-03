# Changelog

This file lists the changes in each release of `borink-object-storage-proto` and `borink-object-storage-crypto`. Until 1.0, any release can break the API. Note: these changelogs are not human-written and only lightly reviewed before 1.0.

## Unreleased

### Added

- S3 directory buckets, as in S3 Express One Zone:
  - New variant `s3::Service::AwsDirectory`. Its requests are signed for `s3express` and go to virtual-hosted URLs.
  - New methods `Objects::encode_create_session`, `Objects::accept_create_session_head`, `Objects::accept_create_session_error_body` and `Objects::read_session`, which create a session.
  - New struct `s3::Session`, the credentials of a session, which `read_session` returns with the expiration in seconds since the Unix epoch, and enum `s3::SessionHeadOutcome`.
  - New method `Objects::with_session`, which returns a client that signs with the credentials of a session and sends its token in `x-amz-s3session-token`.
  - New function `layered::s3::create_session_requirements`.
- New variant `InvalidPlan::Delimiter`, for an empty listing delimiter, and for one other than `/` on a hierarchical-namespace Azure account or a directory bucket. `InvalidPlan::azure_rejection` names Azure's `DelimiterIsInvalidForHNS` for it.
- S3 uploads in parts, with CreateMultipartUpload, UploadPart, CompleteMultipartUpload, AbortMultipartUpload and ListParts:
  - New methods `Objects::encode_create_upload`, `Objects::accept_create_upload_head`, `Objects::accept_create_upload_error_body` and `Objects::read_upload_id`, with the plan `s3::PhysicalCreateUpload` and the outcome `s3::CreateUploadHeadOutcome`.
  - New methods `Objects::encode_stage_part`, `Objects::accept_stage_part_head` and `Objects::accept_stage_part_error_body`, with the plan `s3::PhysicalStagePart`.
  - New methods `Objects::encode_commit_parts`, `Objects::encode_commit_parts_from_iter`, `Objects::accept_commit_parts_head`, `Objects::accept_commit_parts_body` and `Objects::accept_commit_parts_error_body`, which take a `PhysicalCommit` and a list of `s3::PartRef`. S3 answers a commit with status 200 and writes the result or an error into the body. The head is therefore `CommitHeadOutcome::NeedResultBody`, and `accept_commit_parts_body` reads the body into the final outcome.
  - New methods `Objects::encode_abort_upload`, `Objects::accept_abort_upload_head` and `Objects::accept_abort_upload_error_body`, with the plan `s3::PhysicalAbortUpload`. They answer with a `DeleteHeadOutcome`.
  - New methods `Objects::encode_list_parts`, `Objects::accept_list_parts_head`, `Objects::accept_list_parts_error_body` and `Objects::fill_parts`, with the plan `s3::PhysicalListParts` and the entry `s3::Part`, which converts to a `PartRef`.
  - New constants `s3::MAX_PART_LEN`, `s3::MIN_PART_LEN` and `s3::MAX_PARTS`.
  - New field `PhysicalCommit::size`, the length of the object that the commit publishes. S3 sends it as `x-amz-mp-object-size` and refuses a commit whose parts add up to another length. Azure refuses a plan that sets it with `InvalidPlan::Option`.
  - New functions `layered::s3::create_upload_requirements`, `stage_part_requirements`, `commit_parts_requirements`, `abort_upload_requirements` and `list_parts_requirements`.
- New variant `CommitHeadOutcome::NeedResultBody`, which only S3 returns.
- New variant `ServiceErrorKind::NoSuchUpload`, for S3's `NoSuchUpload`. S3's `InvalidPart`, `InvalidPartOrder` and `EntityTooSmall` are `ServiceErrorKind::InvalidUpload`.
- New variant `InvalidPlan::UploadId`, for an empty upload ID.
- New variant `Method::Post`.
- New variant `Error::Service`, for an error document that S3 sends as the body of a success, with the error it names. New variant `ErrorCode::Service` and new method `Error::class`, which says whether a retry can help.
- Date conditions: new variants `ConditionKind::IfModifiedSince` and `ConditionKind::IfUnmodifiedSince`, whose `condition_value` is an HTTP date. A read that fails the first is `GetHeadOutcome::NotModified`. An S3 client for AWS refuses them on a write or a removal with `InvalidPlan::Condition`. The C ABI has `BORINK_CONDITION_IF_MODIFIED_SINCE` and `BORINK_CONDITION_IF_UNMODIFIED_SINCE`.
- New fields `ResponseHead::content_md5`, `content_language`, `content_disposition`, `cache_control` and `storage_class`, and the same fields on `ObjectMeta`, which every read and write outcome fills. `storage_class` holds `x-amz-storage-class` on S3 and `x-ms-access-tier` on Azure.
- Content properties, tags and storage classes on writes:
  - New struct `ContentProperties`, with `Content-Type`, `Content-Encoding`, `Content-Language`, `Content-Disposition` and `Cache-Control`, and new struct `Tag`. A property is ASCII, except a `Content-Type` on a flat Azure account and a `Content-Type` or `Content-Disposition` on an S3 general purpose bucket, which may be UTF-8.
  - New fields `WriteOptions::properties`, `tags` and `storage_class`, which a write of a whole object, an Azure commit and an S3 CreateMultipartUpload send. A stage, and an S3 commit, refuse them with `InvalidPlan::Option`.
  - New field `s3::PhysicalCreateUpload::options`.
  - New variants `InvalidPlan::ContentProperty` and `InvalidPlan::Tag`.
- Tags and access tiers:
  - New plan `PhysicalSetTags`, and new outcomes `UpdateHeadOutcome` and `TagsHeadOutcome`.
  - Azure: new methods `Blobs::encode_set_tier`, `accept_set_tier_head`, `encode_set_tags`, `accept_set_tags_head`, `accept_update_error_body`, `encode_get_tags`, `accept_get_tags_head`, `accept_get_tags_error_body` and `fill_tags`, and functions `layered::set_tier_requirements`, `set_tags_requirements` and `get_tags_requirements`.
  - S3: new methods `Objects::encode_put_tagging`, `accept_put_tagging_head`, `accept_update_error_body`, `encode_get_tagging`, `accept_get_tagging_head`, `accept_get_tagging_error_body` and `fill_tags`, and functions `layered::s3::put_tagging_requirements` and `get_tagging_requirements`.
- Removals of several objects in one request:
  - New plan `PhysicalDeleteMany`, new outcome `DeleteManyHeadOutcome`, and new variant `InvalidPlan::Keys`.
  - Azure Blob Batch: new methods `Blobs::encode_delete_many`, `accept_delete_many_head`, `accept_delete_many_error_body` and `fill_delete_results`, new struct `azure::BatchResult`, constant `azure::MAX_BATCH_KEYS` and function `layered::delete_many_requirements`. `fill_delete_results` takes the plan and the head, reads the `multipart/mixed` answer strictly, and gives each key the `DeleteHeadOutcome` that a single Delete Blob would have, with the head and the body of its response.
  - S3 DeleteObjects: the same methods on `s3::Objects`, new struct `s3::DeleteResult`, constant `s3::MAX_DELETE_KEYS` and function `layered::s3::delete_many_requirements`.
- S3 checksums other than an MD5:
  - New variants `TransactionalChecksum::Crc32`, `Crc32c`, `Sha1` and `Sha256`, and `ChecksumKind::Crc32`, `Crc32c`, `Sha1` and `Sha256`, which only S3 takes, as text or computed. An S3 write sends a CRC64 as `x-amz-checksum-crc64nvme`, big-endian, and each of these as its `x-amz-checksum-` header, signed. A part and a commit still take an MD5 alone, and Azure refuses the new kinds with `InvalidPlan::Option`.
  - New constructors `Digest::crc32`, `crc32c`, `sha1` and `sha256`. `checksum::BASE64_LEN` is 44, the base64 of a SHA-256.
  - `borink-object-storage-crypto`: new features `crc32`, `crc32c` and `sha1-rustcrypto`, with the providers `CRC32`, `CRC32C` and `SHA1_RUSTCRYPTO` over `Crc32`, `Crc32c` and `Sha1RustCrypto`. `Sha256RustCrypto` and `Sha256Minimal` implement `Checksum` as well, as the providers `SHA256_CHECKSUM_RUSTCRYPTO` and `SHA256_CHECKSUM_MINIMAL`.

- Snapshots and versions:
  - New enum `Revision`, a snapshot or a version, and new fields `PhysicalGet::revision` and `PhysicalDelete::revision`, which read or remove one. Azure sends it as `snapshot` or `versionid`, and S3 sends a version as `versionId`. New variant `InvalidPlan::Revision`, for an empty identifier, a snapshot on S3, and a removal of one that also names a `DeleteKind` other than `Object`.
  - New flags `ListInclude::SNAPSHOTS` and `ListInclude::VERSIONS`, which only Azure lists, and new method `ListInclude::intersects`.
  - Azure Snapshot Blob: new plan `azure::PhysicalSnapshot`, outcome `azure::SnapshotHeadOutcome`, methods `Blobs::encode_snapshot`, `accept_snapshot_head` and `accept_snapshot_error_body`, and function `layered::snapshot_requirements`.
  - New field `ResponseHead::snapshot`, from `x-ms-snapshot`.
- Copies that the service carries out:
  - New plan `PhysicalCopy`, with its source `CopySource` and shape `CopyShape`, new outcome `CopyHeadOutcome`, and new variant `InvalidPlan::CopySource`. Its `metadata` and `tags` are `None` to keep what the service gives a copy, or a list that replaces it, which on S3 may be empty.
  - Azure: new methods `Blobs::encode_copy` (Copy Blob), `encode_copy_from_url` (Copy Blob From URL), `accept_copy_head` and `accept_copy_error_body`; `encode_put_from_url` (Put Blob From URL), `accept_put_from_url_head` and `accept_put_from_url_error_body`; `encode_stage_block_from_url` (Put Block From URL), with the plan `azure::PhysicalStageBlockFromUrl`; and `encode_abort_copy` and `accept_abort_copy_head` (Abort Copy Blob). The From URL operations send the client's token in `x-ms-copy-source-authorization`. New functions `layered::copy_requirements`, `copy_from_url_requirements`, `put_from_url_requirements`, `stage_block_from_url_requirements` and `abort_copy_requirements`.
  - S3: new methods `Objects::encode_copy` (CopyObject), `accept_copy_head`, `accept_copy_body` and `accept_copy_error_body`, and `encode_stage_part_copy` (UploadPartCopy), `accept_stage_part_copy_head` and `accept_stage_part_copy_body`, with the plan `s3::PhysicalStagePartCopy`. S3 answers both with status 200 and writes the result or an error into the body. A client that holds the credentials of an S3 Express session refuses both, because AWS authorizes them by the caller's own credentials. New functions `layered::s3::copy_requirements` and `stage_part_copy_requirements`.
  - New variant `StageHeadOutcome::NeedResultBody`, which only an UploadPartCopy returns.
  - New fields `ResponseHead::copy_id` and `copy_status`, and the same on `ObjectMeta`, from `x-ms-copy-id` and `x-ms-copy-status`.
  - Azure's `SourceConditionNotMet` is `ServiceErrorKind::Precondition`.
- Versions on more operations:
  - New field `PhysicalSetTags::revision`, and a `revision` argument on `Blobs::encode_get_tags`, `s3::Objects::encode_get_tagging`, `layered::get_tags_requirements` and `layered::s3::get_tagging_requirements`, which read or replace the tags of a version, or on Azure those of a snapshot.
  - New struct `DeleteTarget`, a key with an optional revision. `PhysicalDeleteMany::keys` is now `objects`, a list of them: an Azure batch names the revision in each subrequest's path, and S3 as each object's `VersionId`. New fields `s3::DeleteResult::version`, `delete_marker` and `delete_marker_version`.
  - S3 listings of versions: a plan with `ListInclude::VERSIONS` sends a ListObjectVersions, whose page `fill_listing` reads. It continues from a `ListMarker::Version`, a key and a version, which `Listing::next_marker` hands out. New variant `EntryKind::DeleteMarker`, new methods `ListEntry::version` and `ListEntry::is_current_version`, and new variants `s3::ObjectProperty::VersionId` and `IsLatest`. A directory bucket refuses the flag. The C ABI has `BORINK_ENTRY_KIND_DELETE_MARKER`.
- Restores from an archive:
  - New plan `PhysicalRestore`, priority `RestorePriority` and outcome `RestoreHeadOutcome`.
  - S3 RestoreObject: new methods `Objects::encode_restore`, `accept_restore_head` and `accept_restore_error_body`, and function `layered::s3::restore_requirements`.
  - Azure rehydration, a Set Blob Tier with `x-ms-rehydrate-priority`: new methods `Blobs::encode_restore`, `accept_restore_head` and `accept_restore_error_body`, and function `layered::restore_requirements`.
  - New field `ResponseHead::restore_status`, and the same on `ObjectMeta`, from `x-amz-restore` or `x-ms-archive-status`.
### Changed

- Listings continue from a `ListMarker`: `PhysicalList::marker`, `PhysicalList::from_shape` and `Listing::next_marker` take or give `ListMarker::Text` for a marker of one text, and `ListMarker::Version` for an S3 listing of versions. `ListMarker::text` returns the text of the first kind. A client refuses the kind of marker that its listing does not hand out with `InvalidPlan::Marker`.
- `Container::new` and `s3::Bucket::new` refuse an `http://` endpoint with `Error::InvalidEndpoint`, because a request without TLS carries its token in clear. `Container::new_allowing_http` and `Bucket::new_allowing_http` take one, for a local emulator or a trusted network. The C ABI's `borink_session` has a new last field, `allow_http`, which a zeroed session leaves false, and the C++ `borink::session` and `Client::open` take a `borink::PlainHttp`.
- `PhysicalGet` and `PhysicalDelete` have the new public field `revision`, so a struct literal that names every field needs it. `from_shape` sets it to `None`.
- A missing container or bucket is never `NotFound`. Every outcome reports it as `ServiceFailure` with `ServiceErrorKind::NoSuchContainer`, so `NotFound` means only that the object, or on S3 the upload, is missing. A reader that maps `NotFound { .. }` to "absent" no longer reads a mistyped container as empty. One case stays ambiguous: S3 answers a HEAD in a missing bucket with the same bare 404 as a missing key, which is `GetHeadOutcome::NotFound { kind: None }`.
- The outcomes of requests that name no object lose their `NotFound` variant, because their 404 can only mean the container: `PutHeadOutcome`, `ListHeadOutcome`, `s3::CreateUploadHeadOutcome` and `s3::SessionHeadOutcome`. The C ABI no longer answers a write or a listing with `NotFound`.
- Both crates declare `rust-version = "1.97"` instead of `1.97.1`, so a toolchain of any 1.97 release builds them.
- Listings group keys at any delimiter. The field `PhysicalList::delimited` is now `delimiter`, which holds the delimiter text. `PhysicalList::from_shape` plans `/` for a delimited `ListShape`.
- An S3 client for a directory bucket refuses a listing prefix that does not end in `/` with `InvalidPlan::Prefix`, and sends a metadata value that a general purpose bucket would not store as given as an RFC 2047 encoded word, instead of refusing it.
- `s3::Objects::fill_listing`, `fill_listing_with` and `read_session` return `Error::Service` for a body that is an error document, instead of `Error::Response` with `ResponseFault::Body`. So do the new `read_upload_id` and `fill_parts`.
- The operations on parts share their plan and outcomes between Azure and S3, and are named for parts rather than blocks:
  - `azure::PhysicalCommitBlocks` is `PhysicalCommit`, at the crate root, and `CommitBlocksShape` is `CommitShape`. An S3 commit takes the same plan, and refuses its metadata, which S3 takes when it creates the upload.
  - `StageBlockHeadOutcome` is `StageHeadOutcome`, and its `Staged` variant carries the `e_tag` of the part, which S3 needs at the commit. Azure sends none.
  - `CommitBlocksHeadOutcome` is `CommitHeadOutcome`.
  - `ListBlocksHeadOutcome` is `ListPartsHeadOutcome`, and its `Blocks` variant is `Parts`.
  - `InvalidPlan::BlockId` is `InvalidPlan::PartId`, and `InvalidPlan::Blocks` is `InvalidPlan::Parts`. Their numbers are unchanged.

## 0.0.3 - 2026-09-29

### Added

- S3 and services that implement the S3 API, for get, head, put and delete:
  - New module `s3`.
  - New struct `s3::Bucket`, which holds an endpoint, a bucket name and a region. `Bucket::new` refuses what a signed request cannot carry.
  - New enum `s3::Service`. `Service::Aws` holds requests and responses to the rules that AWS documents, and `Service::Compatible` accepts what any S3-compatible service is known to send.
  - New enum `s3::Addressing`, for path-style or virtual-hosted URLs.
  - New method `Bucket::with_addressing`, which returns the bucket with that addressing. A bucket uses path-style addressing by default.
  - New struct `s3::Objects`, the client. Its `encode_get`, `encode_put` and `encode_delete` methods write a request head signed with AWS Signature Version 4. Its `accept_*` methods read the response, as the methods of `Blobs` do.
  - New method `Objects::with_signing_key`, which derives the signing key for one day once.
  - New method `Objects::with_checksum`, which registers a `ChecksumProvider` for the `Content-MD5` of a write.
  - New enum `s3::PayloadHash`, which `encode_put` takes. It says whether this crate computes the SHA-256 of the content, takes yours, or leaves the content unsigned.
  - New constants `s3::MAX_KEY_LEN`, `s3::MAX_PUT_LEN`, `s3::MAX_METADATA_LEN` and `s3::METADATA_PREFIX`.
  - New functions `s3::metadata_name`, `s3::error_code` and `s3::classify_error`.
  - New function `s3::metadata_value`, which decodes a metadata value that S3 returns as RFC 2047 encoded words. An S3 client sends a value outside ASCII, or one with a control character, in that form.
  - New module `layered::s3`, with `get_requirements`, `put_requirements` and `delete_requirements` for an `Objects` client.
- S3 listing, with ListObjectsV2. It takes a `PhysicalList` and returns a `ListHeadOutcome`, a `Listing` and `ListEntry` values, as on Azure:
  - New methods `Objects::encode_list`, `Objects::accept_list_head`, `Objects::accept_list_error_body` and `Objects::fill_listing`.
  - New method `Objects::fill_listing_with`, which reads the properties you name in the same pass as the page.
  - New enum `s3::ObjectProperty`, and new structs `s3::PropertySet` and `s3::PropertyValues`, for `fill_listing_with`.
  - New field `PhysicalList::start_after`. An Azure client refuses it.
  - New constant `ListInclude::OWNER`. An Azure client refuses it.
  - New function `layered::s3::list_requirements`.
  - New function `layered::iso8601_ms`, which reads the date of an S3 listing entry.
- AWS Signature Version 4:
  - New module `sigv4`.
  - New struct `sigv4::Credentials`, which holds an access key and an optional session token. `Credentials::new` also takes the function that wipes a client's copy of the secret access key.
  - New function `sigv4::wipe_best_effort`, which sets a buffer to zero on a best-effort basis. Pass it to `Credentials::new` only if you accept writes that the compiler may remove.
  - New struct `sigv4::Sha256Provider`, which holds the SHA-256 and HMAC-SHA256 that a client signs with.
  - New struct `sigv4::Sha256State`, in which a provider keeps a SHA-256 while it computes it.
  - New constants `sigv4::MAX_SECRET_LEN` and `sigv4::MAX_REGION_LEN`.
- New method `Timestamps::iso8601`, which returns the time as `YYYYMMDDTHHMMSSZ`.
- New field `ResponseHead::extended_request_id`, which holds the `x-amz-id-2` header. `ResponseHead::request_id` and `ResponseHead::version` also read `x-amz-request-id` and `x-amz-version-id`.
- New variants `Error::InvalidCredentials` and `Error::InvalidRegion`, with `ErrorCode::InvalidCredentials` and `ErrorCode::InvalidRegion`.
- New variant `InvalidPlan::MetadataTooLarge`, which an S3 client for AWS returns for metadata larger than `s3::MAX_METADATA_LEN`.
- SHA-256 in `borink-object-storage-crypto`:
  - New trait `Sha256` and function `sha256_provider`, which turn your own SHA-256 into a `Sha256Provider`.
  - New struct `Sha256RustCrypto` and constant `SHA256_RUSTCRYPTO`, over RustCrypto's `sha2` and `hmac`, under the `sha256-rustcrypto` feature.
  - New struct `Sha256Minimal` and constant `SHA256_MINIMAL`, over `hmac-sha256`, under the `sha256-minimal` feature.
  - New function `wipe`, which sets a buffer to zero with volatile writes. Pass it to `Credentials::new`.
  - New feature `zeroize`, under which `wipe` calls the `zeroize` crate. It also has RustCrypto wipe its SHA-256 and HMAC state.

### Changed

- In `borink-object-storage-crypto`, renamed the feature `md5` to `md5-rustcrypto`, the struct `Md5` to `Md5RustCrypto` and the constant `MD5` to `MD5_RUSTCRYPTO`.
- Renamed the method `Blobs::accept_error_body` to `accept_get_error_body`.
- The enum `BlobProperty` is `#[non_exhaustive]`, so that a property Azure adds later is not a breaking change.
- The `accept_*_error_body` methods of `Blobs` take the `Failure` of the outcome in place of its `status` and `request_id`.
- `Blobs::fill_listing` and `Blobs::fill_listing_with` read a `%` that begins no escape in an encoded name as the text `%`, as the WHATWG URL Standard does. They refused the page before.

## 0.0.2 - 2026-09-26

### Added

- New crate `borink-object-storage-crypto`. It implements the CRC-64/NVME checksum in software under the `crc64` feature, and MD5 through RustCrypto's `md-5` crate under the `md5` feature. Both features are off by default.
  - Register its constants `CRC64` and `MD5` with `Blobs::with_checksum`.
  - To register your own implementation, implement its `Checksum` trait and register `provider::<C>()`.
- Metadata on writes:
  - New struct `MetadataPair`, which holds the name and the value of one metadata pair.
  - New fields `PhysicalPut::metadata` and `PhysicalCommitBlocks::metadata`. Each takes a slice of `MetadataPair`, and the object stores those pairs.
  - `encode_put` and `encode_commit_blocks` refuse a name that is not an HTTP token. They also refuse a value that contains a control character or that starts or ends with a space.
- Metadata on reads:
  - New constant `azure::METADATA_PREFIX`, which holds `x-ms-meta-`, the prefix of the response headers that carry metadata.
  - New function `azure::metadata_name`, which returns the metadata name that a response header carries, or `None` for any other header.
- Metadata in listings:
  - New type `ListInclude`, which says what a listing reports for each object. Its only flag is `ListInclude::METADATA`.
  - New field `PhysicalList::include`, which takes a `ListInclude`.
  - New variant `BlobProperty::Metadata`. Select it to keep the metadata of each object when you read the listing.
  - New method `ListEntry::metadata`, which returns a `Metadata` iterator over the metadata pairs of the entry.
- Transactional checksums, which Azure compares against the content it receives:
  - New struct `WriteOptions`, which holds the options of a write.
  - New field `options` on `PhysicalPut`, `PhysicalStageBlock` and `PhysicalCommitBlocks`, which takes a `WriteOptions`.
  - New enum `TransactionalChecksum`, for the field `WriteOptions::checksum`. `Md5` and `Crc64` send a checksum that you computed, as base64 text. `Compute` asks this crate to compute one.
  - New module `checksum`, with `ChecksumKind`, `ChecksumProvider`, `ChecksumState` and `Digest`.
  - New method `Blobs::with_checksum`, which registers a `ChecksumProvider`. `Compute` needs a provider of its kind. Without one, the `encode_*` method returns `InvalidPlan::Option`.
  - New field `WriteOptions::declared_md5`, which sets the MD5 that Azure stores for an object written in blocks. Only `PhysicalCommitBlocks` accepts it.
- New constructor `PhysicalGet::head(key)`. It returns a `PhysicalGet` of kind `GetKind::Head`, which reads the properties and the metadata of `key` without its bytes.
- New constructor `PhysicalStageBlock::new(key, id)`, which returns a stage with no options.
- New constant `azure::MAX_URL_LEN`, which holds 32,759 bytes. That is the longest URL that Azure accepts.
- New variant `InvalidPlan::UrlTooLong`, which every `encode_*` method returns for a URL longer than `azure::MAX_URL_LEN`.
- The constants `azure::MAX_PUT_LEN` and `azure::MAX_STAGE_LEN` are now public.
- New sections in the crate docs. They say how to read the head and the body separately and how to follow a `NeedErrorBody` outcome. They say how to handle an outcome variant that your code does not name, and how to hold a `Blobs`. They also say that this crate never retries, and what a `*_requirements` call costs.

### Changed

- Renamed `GetKind::Metadata` to `GetKind::Head`, and `InvalidPlan::RangedMetadata` to `InvalidPlan::RangedHead`.
- Changed the type of `ObjectMeta::last_modified` from bytes to `&str`, the type of `ListEntry::last_modified`. `layered::http_date_ms` now takes a `&str`.
- `accept_error_body`, `accept_put_error_body` and `accept_delete_error_body` have a new first argument, the shape of the request.
- A 412 answer to a get, a put or a delete is now `PreconditionFailed` only if two things hold. The request carried a condition, and Azure names a failed condition. Any other 412, such as `LeaseIdMissing` on a leased blob, is now a service error with Azure's error code. A block-list commit already worked this way.

### Fixed

- A listing prefix on a flat account can now be longer than an object name. Before, it was refused. Now only `azure::MAX_URL_LEN` limits it.

## 0.0.1 - 2026-09-07

The first release. `borink-object-storage-proto` builds requests to Azure Blob Storage and reads their responses. It gets, heads, puts and deletes objects, stages and commits blocks, and lists objects. It does no I/O and allocates no memory.
