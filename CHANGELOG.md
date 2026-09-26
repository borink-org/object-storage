# Changelog

This file lists the changes in each release of `borink-object-storage-proto` and `borink-object-storage-crypto`. Until 1.0, any release can break the API.

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
