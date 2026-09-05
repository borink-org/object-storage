//! What a C compiler computes for the types in [`crate::types`], checked
//! against what Rust compiled.
//!
//! Nothing here needs reading line by line. `tests/abi.c` fills a [`Layout`]
//! with its own `sizeof`, `alignof` and `offsetof`, and
//! [`borink_layout_disagrees`] counts the fields that differ. The `const`
//! block at the end fails the build if an enum is renumbered on either side.

use crate::{ptr::items, types::*};

use borink_object_storage_proto as proto;

/// What a C compiler computes for the structs that cross this boundary.
///
/// Fill every field with the `sizeof`, `alignof` or `offsetof` that its name
/// gives, and pass it to `borink_layout_disagrees`.
#[repr(C)]
#[derive(Clone, Copy)]
#[allow(missing_docs, reason = "each field is named by what it measures")]
pub struct Layout {
    pub sizeof_bytes: usize,
    pub alignof_bytes: usize,
    pub offsetof_bytes_len: usize,
    pub sizeof_bytes_mut: usize,
    pub alignof_bytes_mut: usize,
    pub sizeof_span: usize,
    pub offsetof_span_len: usize,
    pub sizeof_maybe_bytes: usize,
    pub alignof_maybe_bytes: usize,
    pub offsetof_maybe_bytes_bytes: usize,
    pub sizeof_maybe_u64: usize,
    pub alignof_maybe_u64: usize,
    pub offsetof_maybe_u64_value: usize,
    pub sizeof_status: usize,
    pub offsetof_status_detail: usize,
    pub sizeof_session: usize,
    pub offsetof_session_container: usize,
    pub offsetof_session_token: usize,
    pub sizeof_range: usize,
    pub alignof_range: usize,
    pub offsetof_range_start: usize,
    pub offsetof_range_end: usize,
    pub sizeof_get_shape: usize,
    pub offsetof_get_shape_range: usize,
    pub offsetof_get_shape_condition: usize,
    pub sizeof_put_shape: usize,
    pub sizeof_delete_shape: usize,
    pub offsetof_delete_shape_condition: usize,
    pub sizeof_request_header: usize,
    pub offsetof_request_header_value: usize,
    pub sizeof_request_head: usize,
    pub alignof_request_head: usize,
    pub offsetof_request_head_required: usize,
    pub offsetof_request_head_method: usize,
    pub offsetof_request_head_url: usize,
    pub offsetof_request_head_header_count: usize,
    pub offsetof_request_head_headers: usize,
    pub sizeof_header_ref: usize,
    pub offsetof_header_ref_value: usize,
    pub sizeof_object_meta: usize,
    pub offsetof_object_meta_e_tag: usize,
    pub offsetof_object_meta_last_modified: usize,
    pub offsetof_object_meta_version: usize,
    pub offsetof_object_meta_content_encoding: usize,
    pub sizeof_body_window: usize,
    pub offsetof_body_window_expected_len: usize,
    pub offsetof_body_window_object_size: usize,
    pub sizeof_failure: usize,
    pub offsetof_failure_class: usize,
    pub offsetof_failure_kind: usize,
    pub offsetof_failure_request_id: usize,
    pub sizeof_outcome: usize,
    pub alignof_outcome: usize,
    pub offsetof_outcome_meta: usize,
    pub offsetof_outcome_body: usize,
    pub offsetof_outcome_failure: usize,
    pub offsetof_outcome_error: usize,
    pub sizeof_maybe_u32: usize,
    pub alignof_maybe_u32: usize,
    pub offsetof_maybe_u32_value: usize,
    pub sizeof_list_shape: usize,
    pub offsetof_list_shape_max_results: usize,
    pub sizeof_list_entry: usize,
    pub alignof_list_entry: usize,
    pub offsetof_list_entry_key: usize,
    pub offsetof_list_entry_size: usize,
    pub offsetof_list_entry_e_tag: usize,
    pub offsetof_list_entry_last_modified: usize,
    pub offsetof_list_entry_raw: usize,
    pub sizeof_properties: usize,
    pub alignof_properties: usize,
    pub offsetof_properties_within: usize,
    pub sizeof_property: usize,
    pub alignof_property: usize,
    pub offsetof_property_name: usize,
    pub offsetof_property_value: usize,
    pub sizeof_fill: usize,
    pub alignof_fill: usize,
    pub offsetof_fill_filled: usize,
    pub offsetof_fill_required: usize,
    pub offsetof_fill_next_marker: usize,
    pub sizeof_property_set: usize,
    pub alignof_property_set: usize,
}

/// The layout that this crate compiled to.
pub(crate) fn layout() -> Layout {
    use core::mem::offset_of;
    Layout {
        sizeof_bytes: size_of::<Bytes>(),
        alignof_bytes: align_of::<Bytes>(),
        offsetof_bytes_len: offset_of!(Bytes, len),
        sizeof_bytes_mut: size_of::<BytesMut>(),
        alignof_bytes_mut: align_of::<BytesMut>(),
        sizeof_span: size_of::<Span>(),
        offsetof_span_len: offset_of!(Span, len),
        sizeof_maybe_bytes: size_of::<MaybeBytes>(),
        alignof_maybe_bytes: align_of::<MaybeBytes>(),
        offsetof_maybe_bytes_bytes: offset_of!(MaybeBytes, bytes),
        sizeof_maybe_u64: size_of::<MaybeU64>(),
        alignof_maybe_u64: align_of::<MaybeU64>(),
        offsetof_maybe_u64_value: offset_of!(MaybeU64, value),
        sizeof_status: size_of::<Status>(),
        offsetof_status_detail: offset_of!(Status, detail),
        sizeof_session: size_of::<Session>(),
        offsetof_session_container: offset_of!(Session, container),
        offsetof_session_token: offset_of!(Session, token),
        sizeof_range: size_of::<Range>(),
        alignof_range: align_of::<Range>(),
        offsetof_range_start: offset_of!(Range, start),
        offsetof_range_end: offset_of!(Range, end),
        sizeof_get_shape: size_of::<GetShape>(),
        offsetof_get_shape_range: offset_of!(GetShape, range),
        offsetof_get_shape_condition: offset_of!(GetShape, condition),
        sizeof_put_shape: size_of::<PutShape>(),
        sizeof_delete_shape: size_of::<DeleteShape>(),
        offsetof_delete_shape_condition: offset_of!(DeleteShape, condition),
        sizeof_request_header: size_of::<RequestHeader>(),
        offsetof_request_header_value: offset_of!(RequestHeader, value),
        sizeof_request_head: size_of::<RequestHead>(),
        alignof_request_head: align_of::<RequestHead>(),
        offsetof_request_head_required: offset_of!(RequestHead, required),
        offsetof_request_head_method: offset_of!(RequestHead, method),
        offsetof_request_head_url: offset_of!(RequestHead, url),
        offsetof_request_head_header_count: offset_of!(RequestHead, header_count),
        offsetof_request_head_headers: offset_of!(RequestHead, headers),
        sizeof_header_ref: size_of::<HeaderRef>(),
        offsetof_header_ref_value: offset_of!(HeaderRef, value),
        sizeof_object_meta: size_of::<ObjectMeta>(),
        offsetof_object_meta_e_tag: offset_of!(ObjectMeta, e_tag),
        offsetof_object_meta_last_modified: offset_of!(ObjectMeta, last_modified),
        offsetof_object_meta_version: offset_of!(ObjectMeta, version),
        offsetof_object_meta_content_encoding: offset_of!(ObjectMeta, content_encoding),
        sizeof_body_window: size_of::<BodyWindow>(),
        offsetof_body_window_expected_len: offset_of!(BodyWindow, expected_len),
        offsetof_body_window_object_size: offset_of!(BodyWindow, object_size),
        sizeof_failure: size_of::<Failure>(),
        offsetof_failure_class: offset_of!(Failure, class),
        offsetof_failure_kind: offset_of!(Failure, kind),
        offsetof_failure_request_id: offset_of!(Failure, request_id),
        sizeof_outcome: size_of::<Outcome>(),
        alignof_outcome: align_of::<Outcome>(),
        offsetof_outcome_meta: offset_of!(Outcome, meta),
        offsetof_outcome_body: offset_of!(Outcome, body),
        offsetof_outcome_failure: offset_of!(Outcome, failure),
        offsetof_outcome_error: offset_of!(Outcome, error),
        sizeof_maybe_u32: size_of::<MaybeU32>(),
        alignof_maybe_u32: align_of::<MaybeU32>(),
        offsetof_maybe_u32_value: offset_of!(MaybeU32, value),
        sizeof_list_shape: size_of::<ListShape>(),
        offsetof_list_shape_max_results: offset_of!(ListShape, max_results),
        sizeof_list_entry: size_of::<ListEntry>(),
        alignof_list_entry: align_of::<ListEntry>(),
        offsetof_list_entry_key: offset_of!(ListEntry, key),
        offsetof_list_entry_size: offset_of!(ListEntry, size),
        offsetof_list_entry_e_tag: offset_of!(ListEntry, e_tag),
        offsetof_list_entry_last_modified: offset_of!(ListEntry, last_modified),
        offsetof_list_entry_raw: offset_of!(ListEntry, raw),
        sizeof_properties: size_of::<Properties>(),
        alignof_properties: align_of::<Properties>(),
        offsetof_properties_within: offset_of!(Properties, within),
        sizeof_property: size_of::<Property>(),
        alignof_property: align_of::<Property>(),
        offsetof_property_name: offset_of!(Property, name),
        offsetof_property_value: offset_of!(Property, value),
        sizeof_fill: size_of::<Fill>(),
        alignof_fill: align_of::<Fill>(),
        offsetof_fill_filled: offset_of!(Fill, filled),
        offsetof_fill_required: offset_of!(Fill, required),
        offsetof_fill_next_marker: offset_of!(Fill, next_marker),
        sizeof_property_set: size_of::<PropertySet>(),
        alignof_property_set: align_of::<PropertySet>(),
    }
}

/// Compares the layout a C compiler computed with the one this crate uses.
///
/// Returns the number of fields of `probe` that differ, and 0 when every one
/// agrees. Call it once at startup, or from a static assertion in your own
/// test, before you read a field of any struct here.
///
/// # Safety
///
/// `probe` must be null or point at one readable `borink_layout`. A null
/// `probe` counts every field as different.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn borink_layout_disagrees(probe: *const Layout) -> usize {
    let ours = layout();
    let fields = size_of::<Layout>() / size_of::<usize>();
    if probe.is_null() {
        return fields;
    }
    // SAFETY: the caller states that `probe` points at one readable value, and
    // `Layout` is `usize` fields alone, so reading it as those is its layout.
    let (theirs, ours) = unsafe {
        (
            items(probe.cast::<usize>(), fields),
            items(core::ptr::from_ref(&ours).cast::<usize>(), fields),
        )
    };
    theirs
        .iter()
        .zip(ours)
        .filter(|(theirs, ours)| theirs != ours)
        .count()
}

// Every enum above crosses as a number. These pin the two lists to each other:
// a value renumbered on either side stops this build.
const _: () = {
    assert!(BORINK_MAX_HEADERS == proto::MAX_HEADERS);

    assert!(ErrorCode::InvalidEndpoint as u16 == proto::ErrorCode::InvalidEndpoint as u16);
    assert!(ErrorCode::InvalidContainer as u16 == proto::ErrorCode::InvalidContainer as u16);
    assert!(ErrorCode::InvalidToken as u16 == proto::ErrorCode::InvalidToken as u16);
    assert!(ErrorCode::InvalidPlan as u16 == proto::ErrorCode::InvalidPlan as u16);
    assert!(ErrorCode::Capacity as u16 == proto::ErrorCode::Capacity as u16);
    assert!(ErrorCode::Response as u16 == proto::ErrorCode::Response as u16);

    assert!(Method::Get as u16 == proto::Method::Get as u16);
    assert!(Method::Head as u16 == proto::Method::Head as u16);
    assert!(Method::Put as u16 == proto::Method::Put as u16);
    assert!(Method::Delete as u16 == proto::Method::Delete as u16);

    assert!(GetKind::Bytes as u16 == proto::GetKind::Bytes as u16);
    assert!(GetKind::Metadata as u16 == proto::GetKind::Metadata as u16);

    assert!(RangeForm::Whole as u16 == proto::RangeForm::Whole as u16);
    assert!(RangeForm::Bounded as u16 == proto::RangeForm::Bounded as u16);
    assert!(RangeForm::Offset as u16 == proto::RangeForm::Offset as u16);
    assert!(RangeForm::Suffix as u16 == proto::RangeForm::Suffix as u16);

    assert!(Condition::None as u16 == proto::ConditionKind::None as u16);
    assert!(Condition::IfMatch as u16 == proto::ConditionKind::IfMatch as u16);
    assert!(Condition::IfNoneMatch as u16 == proto::ConditionKind::IfNoneMatch as u16);

    assert!(DeleteKind::Object as u16 == proto::DeleteKind::Object as u16);
    assert!(DeleteKind::ObjectAndSnapshots as u16 == proto::DeleteKind::ObjectAndSnapshots as u16);
    assert!(DeleteKind::SnapshotsOnly as u16 == proto::DeleteKind::SnapshotsOnly as u16);

    assert!(EntryKind::Object as u16 == proto::EntryKind::Object as u16);
    assert!(EntryKind::Prefix as u16 == proto::EntryKind::Prefix as u16);
    assert!(EntryKind::Directory as u16 == proto::EntryKind::Directory as u16);

    // The properties, and that neither list has one the other lacks.
    assert!(proto::BlobProperty::COUNT == BlobProperty::BlobSequenceNumber as usize + 1);
    assert!(BlobProperty::AccessTier as u16 == proto::BlobProperty::AccessTier as u16);
    assert!(
        BlobProperty::AccessTierInferred as u16 == proto::BlobProperty::AccessTierInferred as u16
    );
    assert!(
        BlobProperty::AccessTierChangeTime as u16
            == proto::BlobProperty::AccessTierChangeTime as u16
    );
    assert!(BlobProperty::ArchiveStatus as u16 == proto::BlobProperty::ArchiveStatus as u16);
    assert!(BlobProperty::Acl as u16 == proto::BlobProperty::Acl as u16);
    assert!(BlobProperty::BlobType as u16 == proto::BlobProperty::BlobType as u16);
    assert!(BlobProperty::CreationTime as u16 == proto::BlobProperty::CreationTime as u16);
    assert!(BlobProperty::ContentType as u16 == proto::BlobProperty::ContentType as u16);
    assert!(BlobProperty::ContentEncoding as u16 == proto::BlobProperty::ContentEncoding as u16);
    assert!(BlobProperty::ContentLanguage as u16 == proto::BlobProperty::ContentLanguage as u16);
    assert!(BlobProperty::ContentCrc64 as u16 == proto::BlobProperty::ContentCrc64 as u16);
    assert!(BlobProperty::ContentMd5 as u16 == proto::BlobProperty::ContentMd5 as u16);
    assert!(BlobProperty::CacheControl as u16 == proto::BlobProperty::CacheControl as u16);
    assert!(
        BlobProperty::ContentDisposition as u16 == proto::BlobProperty::ContentDisposition as u16
    );
    assert!(BlobProperty::CopyId as u16 == proto::BlobProperty::CopyId as u16);
    assert!(BlobProperty::CopyStatus as u16 == proto::BlobProperty::CopyStatus as u16);
    assert!(BlobProperty::CopySource as u16 == proto::BlobProperty::CopySource as u16);
    assert!(BlobProperty::CopyProgress as u16 == proto::BlobProperty::CopyProgress as u16);
    assert!(
        BlobProperty::CopyCompletionTime as u16 == proto::BlobProperty::CopyCompletionTime as u16
    );
    assert!(
        BlobProperty::CopyStatusDescription as u16
            == proto::BlobProperty::CopyStatusDescription as u16
    );
    assert!(BlobProperty::DeletedTime as u16 == proto::BlobProperty::DeletedTime as u16);
    assert!(BlobProperty::Deleted as u16 == proto::BlobProperty::Deleted as u16);
    assert!(BlobProperty::EncryptionScope as u16 == proto::BlobProperty::EncryptionScope as u16);
    assert!(BlobProperty::ExpiryTime as u16 == proto::BlobProperty::ExpiryTime as u16);
    assert!(BlobProperty::Group as u16 == proto::BlobProperty::Group as u16);
    assert!(BlobProperty::IsCurrentVersion as u16 == proto::BlobProperty::IsCurrentVersion as u16);
    assert!(BlobProperty::IncrementalCopy as u16 == proto::BlobProperty::IncrementalCopy as u16);
    assert!(
        BlobProperty::ImmutabilityPolicyUntilDate as u16
            == proto::BlobProperty::ImmutabilityPolicyUntilDate as u16
    );
    assert!(
        BlobProperty::ImmutabilityPolicyMode as u16
            == proto::BlobProperty::ImmutabilityPolicyMode as u16
    );
    assert!(BlobProperty::LeaseStatus as u16 == proto::BlobProperty::LeaseStatus as u16);
    assert!(BlobProperty::LeaseState as u16 == proto::BlobProperty::LeaseState as u16);
    assert!(BlobProperty::LeaseDuration as u16 == proto::BlobProperty::LeaseDuration as u16);
    assert!(BlobProperty::LegalHold as u16 == proto::BlobProperty::LegalHold as u16);
    assert!(BlobProperty::Owner as u16 == proto::BlobProperty::Owner as u16);
    assert!(BlobProperty::Permissions as u16 == proto::BlobProperty::Permissions as u16);
    assert!(
        BlobProperty::RemainingRetentionDays as u16
            == proto::BlobProperty::RemainingRetentionDays as u16
    );
    assert!(
        BlobProperty::RehydratePriority as u16 == proto::BlobProperty::RehydratePriority as u16
    );
    assert!(BlobProperty::ServerEncrypted as u16 == proto::BlobProperty::ServerEncrypted as u16);
    assert!(BlobProperty::Snapshot as u16 == proto::BlobProperty::Snapshot as u16);
    assert!(BlobProperty::TagCount as u16 == proto::BlobProperty::TagCount as u16);
    assert!(BlobProperty::VersionId as u16 == proto::BlobProperty::VersionId as u16);
    assert!(
        BlobProperty::BlobSequenceNumber as u16 == proto::BlobProperty::BlobSequenceNumber as u16
    );

    assert!(FailureClass::Auth as u16 == proto::FailureClass::Auth as u16);
    assert!(FailureClass::Throttled as u16 == proto::FailureClass::Throttled as u16);
    assert!(FailureClass::Server as u16 == proto::FailureClass::Server as u16);
    assert!(FailureClass::Redirect as u16 == proto::FailureClass::Redirect as u16);
    assert!(FailureClass::Other as u16 == proto::FailureClass::Other as u16);

    assert!(ServiceError::NotFound as u16 == proto::ServiceErrorKind::NotFound as u16);
    assert!(
        ServiceError::NoSuchContainer as u16 == proto::ServiceErrorKind::NoSuchContainer as u16
    );
    assert!(ServiceError::AlreadyExists as u16 == proto::ServiceErrorKind::AlreadyExists as u16);
    assert!(ServiceError::Unauthorized as u16 == proto::ServiceErrorKind::Unauthorized as u16);
    assert!(ServiceError::Precondition as u16 == proto::ServiceErrorKind::Precondition as u16);
    assert!(
        ServiceError::RangeNotSatisfiable as u16
            == proto::ServiceErrorKind::RangeNotSatisfiable as u16
    );
    assert!(ServiceError::Throttled as u16 == proto::ServiceErrorKind::Throttled as u16);
    assert!(ServiceError::Timeout as u16 == proto::ServiceErrorKind::Timeout as u16);
    assert!(ServiceError::Service as u16 == proto::ServiceErrorKind::Service as u16);
};
