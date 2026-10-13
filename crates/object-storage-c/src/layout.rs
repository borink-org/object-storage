//! What a C compiler computes for the types in [`crate::types`], checked
//! against what Rust compiled.
//!
//! Nothing here needs reading line by line. `tests/abi.c` fills a [`Layout`]
//! with its own `sizeof`, `alignof` and `offsetof`, and
//! [`borink_layout_disagrees`] counts the fields that differ. The `const`
//! block at the end fails the build if an enum is renumbered on either side.

use crate::{ptr::items, types::*};

use borink_object_storage_proto as proto;
use core::mem::offset_of;

/// What a C compiler computes for the structs that cross this boundary.
///
/// Fill every field with the `sizeof`, `alignof` or `offsetof` that its name
/// gives, and pass it to `borink_layout_disagrees`.
#[repr(C)]
#[derive(Clone, Copy)]
#[allow(missing_docs, reason = "each field is named by what it measures")]
pub struct Layout {
    pub sizeof_azure_rejection: usize,
    pub alignof_azure_rejection: usize,
    pub offsetof_azure_rejection_status: usize,
    pub offsetof_azure_rejection_code: usize,
    pub sizeof_bytes: usize,
    pub alignof_bytes: usize,
    pub offsetof_bytes_ptr: usize,
    pub offsetof_bytes_len: usize,
    pub sizeof_bytes_mut: usize,
    pub alignof_bytes_mut: usize,
    pub offsetof_bytes_mut_ptr: usize,
    pub offsetof_bytes_mut_len: usize,
    pub sizeof_span: usize,
    pub alignof_span: usize,
    pub offsetof_span_start: usize,
    pub offsetof_span_len: usize,
    pub sizeof_maybe_span: usize,
    pub alignof_maybe_span: usize,
    pub offsetof_maybe_span_present: usize,
    pub offsetof_maybe_span_span: usize,
    pub sizeof_maybe_bytes: usize,
    pub alignof_maybe_bytes: usize,
    pub offsetof_maybe_bytes_present: usize,
    pub offsetof_maybe_bytes_bytes: usize,
    pub sizeof_maybe_u64: usize,
    pub alignof_maybe_u64: usize,
    pub offsetof_maybe_u64_present: usize,
    pub offsetof_maybe_u64_value: usize,
    pub sizeof_maybe_u32: usize,
    pub alignof_maybe_u32: usize,
    pub offsetof_maybe_u32_present: usize,
    pub offsetof_maybe_u32_value: usize,
    pub sizeof_status: usize,
    pub alignof_status: usize,
    pub offsetof_status_code: usize,
    pub offsetof_status_detail: usize,
    pub sizeof_container: usize,
    pub alignof_container: usize,
    pub offsetof_container_endpoint: usize,
    pub offsetof_container_name: usize,
    pub offsetof_container_allow_http: usize,
    pub offsetof_container_object: usize,
    pub sizeof_timestamps: usize,
    pub alignof_timestamps: usize,
    pub offsetof_timestamps_stored: usize,
    pub sizeof_template: usize,
    pub alignof_template: usize,
    pub offsetof_template_stored: usize,
    pub sizeof_prepared: usize,
    pub alignof_prepared: usize,
    pub offsetof_prepared_status: usize,
    pub offsetof_prepared_required: usize,
    pub offsetof_prepared_template: usize,
    pub sizeof_condition_value: usize,
    pub alignof_condition_value: usize,
    pub offsetof_condition_value_form: usize,
    pub offsetof_condition_value_e_tag: usize,
    pub offsetof_condition_value_seconds: usize,
    pub sizeof_range: usize,
    pub alignof_range: usize,
    pub offsetof_range_form: usize,
    pub offsetof_range_start: usize,
    pub offsetof_range_end: usize,
    pub sizeof_get_options: usize,
    pub alignof_get_options: usize,
    pub offsetof_get_options_kind: usize,
    pub offsetof_get_options_ranging: usize,
    pub offsetof_get_options_condition: usize,
    pub offsetof_get_options_revision: usize,
    pub sizeof_get_item: usize,
    pub alignof_get_item: usize,
    pub offsetof_get_item_key: usize,
    pub offsetof_get_item_range: usize,
    pub offsetof_get_item_condition_value: usize,
    pub offsetof_get_item_revision: usize,
    pub sizeof_get_shape: usize,
    pub alignof_get_shape: usize,
    pub offsetof_get_shape_kind: usize,
    pub offsetof_get_shape_range: usize,
    pub offsetof_get_shape_condition: usize,
    pub sizeof_content_properties: usize,
    pub alignof_content_properties: usize,
    pub offsetof_content_properties_content_type: usize,
    pub offsetof_content_properties_content_encoding: usize,
    pub offsetof_content_properties_content_language: usize,
    pub offsetof_content_properties_content_disposition: usize,
    pub offsetof_content_properties_cache_control: usize,
    pub sizeof_write_options: usize,
    pub alignof_write_options: usize,
    pub offsetof_write_options_properties: usize,
    pub offsetof_write_options_storage_class: usize,
    pub sizeof_put_options: usize,
    pub alignof_put_options: usize,
    pub offsetof_put_options_condition: usize,
    pub offsetof_put_options_options: usize,
    pub offsetof_put_options_checksum: usize,
    pub sizeof_put_item: usize,
    pub alignof_put_item: usize,
    pub offsetof_put_item_key: usize,
    pub offsetof_put_item_content_len: usize,
    pub offsetof_put_item_condition_value: usize,
    pub offsetof_put_item_checksum: usize,
    pub sizeof_put_shape: usize,
    pub alignof_put_shape: usize,
    pub offsetof_put_shape_condition: usize,
    pub sizeof_stage_options: usize,
    pub alignof_stage_options: usize,
    pub offsetof_stage_options_checksum: usize,
    pub sizeof_stage_block_item: usize,
    pub alignof_stage_block_item: usize,
    pub offsetof_stage_block_item_key: usize,
    pub offsetof_stage_block_item_id: usize,
    pub offsetof_stage_block_item_content_len: usize,
    pub offsetof_stage_block_item_checksum: usize,
    pub sizeof_commit_options: usize,
    pub alignof_commit_options: usize,
    pub offsetof_commit_options_condition: usize,
    pub offsetof_commit_options_options: usize,
    pub offsetof_commit_options_content_md5: usize,
    pub sizeof_block_ref: usize,
    pub alignof_block_ref: usize,
    pub offsetof_block_ref_id: usize,
    pub offsetof_block_ref_source: usize,
    pub sizeof_commit_item: usize,
    pub alignof_commit_item: usize,
    pub offsetof_commit_item_key: usize,
    pub offsetof_commit_item_condition_value: usize,
    pub offsetof_commit_item_blocks: usize,
    pub offsetof_commit_item_block_count: usize,
    pub offsetof_commit_item_content_md5: usize,
    pub sizeof_commit_shape: usize,
    pub alignof_commit_shape: usize,
    pub offsetof_commit_shape_condition: usize,
    pub sizeof_list_options: usize,
    pub alignof_list_options: usize,
    pub offsetof_list_options_delimiter: usize,
    pub offsetof_list_options_max_results: usize,
    pub offsetof_list_options_include: usize,
    pub sizeof_list_item: usize,
    pub alignof_list_item: usize,
    pub offsetof_list_item_prefix: usize,
    pub offsetof_list_item_marker: usize,
    pub sizeof_wire_request: usize,
    pub alignof_wire_request: usize,
    pub offsetof_wire_request_status: usize,
    pub offsetof_wire_request_required: usize,
    pub offsetof_wire_request_method: usize,
    pub offsetof_wire_request_url: usize,
    pub offsetof_wire_request_head: usize,
    pub offsetof_wire_request_fields: usize,
    pub offsetof_wire_request_body: usize,
    pub offsetof_wire_request_payload_len: usize,
    pub sizeof_header: usize,
    pub alignof_header: usize,
    pub offsetof_header_present: usize,
    pub offsetof_header_name: usize,
    pub offsetof_header_value: usize,
    pub sizeof_header_ref: usize,
    pub alignof_header_ref: usize,
    pub offsetof_header_ref_name: usize,
    pub offsetof_header_ref_value: usize,
    pub sizeof_object_meta: usize,
    pub alignof_object_meta: usize,
    pub offsetof_object_meta_size: usize,
    pub offsetof_object_meta_e_tag: usize,
    pub offsetof_object_meta_last_modified: usize,
    pub offsetof_object_meta_version: usize,
    pub offsetof_object_meta_content_encoding: usize,
    pub offsetof_object_meta_content_type: usize,
    pub sizeof_body_window: usize,
    pub alignof_body_window: usize,
    pub offsetof_body_window_object_offset: usize,
    pub offsetof_body_window_expected_len: usize,
    pub offsetof_body_window_object_size: usize,
    pub sizeof_failure: usize,
    pub alignof_failure: usize,
    pub offsetof_failure_status: usize,
    pub offsetof_failure_class: usize,
    pub offsetof_failure_kind: usize,
    pub offsetof_failure_request_id: usize,
    pub sizeof_outcome: usize,
    pub alignof_outcome: usize,
    pub offsetof_outcome_kind: usize,
    pub offsetof_outcome_meta: usize,
    pub offsetof_outcome_body: usize,
    pub offsetof_outcome_failure: usize,
    pub offsetof_outcome_error: usize,
    pub sizeof_list_entry: usize,
    pub alignof_list_entry: usize,
    pub offsetof_list_entry_kind: usize,
    pub offsetof_list_entry_key: usize,
    pub offsetof_list_entry_size: usize,
    pub offsetof_list_entry_e_tag: usize,
    pub offsetof_list_entry_last_modified: usize,
    pub offsetof_list_entry_content_type: usize,
    pub offsetof_list_entry_raw: usize,
    pub sizeof_properties: usize,
    pub alignof_properties: usize,
    pub offsetof_properties_remaining: usize,
    pub offsetof_properties_within: usize,
    pub sizeof_property: usize,
    pub alignof_property: usize,
    pub offsetof_property_present: usize,
    pub offsetof_property_name: usize,
    pub offsetof_property_value: usize,
    pub sizeof_property_set: usize,
    pub alignof_property_set: usize,
    pub offsetof_property_set_mask: usize,
    pub sizeof_fill: usize,
    pub alignof_fill: usize,
    pub offsetof_fill_status: usize,
    pub offsetof_fill_filled: usize,
    pub offsetof_fill_required: usize,
    pub offsetof_fill_next_marker: usize,
}

/// The layout that this crate compiled to.
pub(crate) fn layout() -> Layout {
    Layout {
        sizeof_azure_rejection: size_of::<AzureRejection>(),
        alignof_azure_rejection: align_of::<AzureRejection>(),
        offsetof_azure_rejection_status: offset_of!(AzureRejection, status),
        offsetof_azure_rejection_code: offset_of!(AzureRejection, code),
        sizeof_bytes: size_of::<Bytes>(),
        alignof_bytes: align_of::<Bytes>(),
        offsetof_bytes_ptr: offset_of!(Bytes, ptr),
        offsetof_bytes_len: offset_of!(Bytes, len),
        sizeof_bytes_mut: size_of::<BytesMut>(),
        alignof_bytes_mut: align_of::<BytesMut>(),
        offsetof_bytes_mut_ptr: offset_of!(BytesMut, ptr),
        offsetof_bytes_mut_len: offset_of!(BytesMut, len),
        sizeof_span: size_of::<Span>(),
        alignof_span: align_of::<Span>(),
        offsetof_span_start: offset_of!(Span, start),
        offsetof_span_len: offset_of!(Span, len),
        sizeof_maybe_span: size_of::<MaybeSpan>(),
        alignof_maybe_span: align_of::<MaybeSpan>(),
        offsetof_maybe_span_present: offset_of!(MaybeSpan, present),
        offsetof_maybe_span_span: offset_of!(MaybeSpan, span),
        sizeof_maybe_bytes: size_of::<MaybeBytes>(),
        alignof_maybe_bytes: align_of::<MaybeBytes>(),
        offsetof_maybe_bytes_present: offset_of!(MaybeBytes, present),
        offsetof_maybe_bytes_bytes: offset_of!(MaybeBytes, bytes),
        sizeof_maybe_u64: size_of::<MaybeU64>(),
        alignof_maybe_u64: align_of::<MaybeU64>(),
        offsetof_maybe_u64_present: offset_of!(MaybeU64, present),
        offsetof_maybe_u64_value: offset_of!(MaybeU64, value),
        sizeof_maybe_u32: size_of::<MaybeU32>(),
        alignof_maybe_u32: align_of::<MaybeU32>(),
        offsetof_maybe_u32_present: offset_of!(MaybeU32, present),
        offsetof_maybe_u32_value: offset_of!(MaybeU32, value),
        sizeof_status: size_of::<Status>(),
        alignof_status: align_of::<Status>(),
        offsetof_status_code: offset_of!(Status, code),
        offsetof_status_detail: offset_of!(Status, detail),
        sizeof_container: size_of::<Container>(),
        alignof_container: align_of::<Container>(),
        offsetof_container_endpoint: offset_of!(Container, endpoint),
        offsetof_container_name: offset_of!(Container, name),
        offsetof_container_allow_http: offset_of!(Container, allow_http),
        offsetof_container_object: offset_of!(Container, object),
        sizeof_timestamps: size_of::<Timestamps>(),
        alignof_timestamps: align_of::<Timestamps>(),
        offsetof_timestamps_stored: offset_of!(Timestamps, stored),
        sizeof_template: size_of::<Template>(),
        alignof_template: align_of::<Template>(),
        offsetof_template_stored: offset_of!(Template, stored),
        sizeof_prepared: size_of::<Prepared>(),
        alignof_prepared: align_of::<Prepared>(),
        offsetof_prepared_status: offset_of!(Prepared, status),
        offsetof_prepared_required: offset_of!(Prepared, required),
        offsetof_prepared_template: offset_of!(Prepared, template),
        sizeof_condition_value: size_of::<ConditionValue>(),
        alignof_condition_value: align_of::<ConditionValue>(),
        offsetof_condition_value_form: offset_of!(ConditionValue, form),
        offsetof_condition_value_e_tag: offset_of!(ConditionValue, e_tag),
        offsetof_condition_value_seconds: offset_of!(ConditionValue, seconds),
        sizeof_range: size_of::<Range>(),
        alignof_range: align_of::<Range>(),
        offsetof_range_form: offset_of!(Range, form),
        offsetof_range_start: offset_of!(Range, start),
        offsetof_range_end: offset_of!(Range, end),
        sizeof_get_options: size_of::<GetOptions>(),
        alignof_get_options: align_of::<GetOptions>(),
        offsetof_get_options_kind: offset_of!(GetOptions, kind),
        offsetof_get_options_ranging: offset_of!(GetOptions, ranging),
        offsetof_get_options_condition: offset_of!(GetOptions, condition),
        offsetof_get_options_revision: offset_of!(GetOptions, revision),
        sizeof_get_item: size_of::<GetItem>(),
        alignof_get_item: align_of::<GetItem>(),
        offsetof_get_item_key: offset_of!(GetItem, key),
        offsetof_get_item_range: offset_of!(GetItem, range),
        offsetof_get_item_condition_value: offset_of!(GetItem, condition_value),
        offsetof_get_item_revision: offset_of!(GetItem, revision),
        sizeof_get_shape: size_of::<GetShape>(),
        alignof_get_shape: align_of::<GetShape>(),
        offsetof_get_shape_kind: offset_of!(GetShape, kind),
        offsetof_get_shape_range: offset_of!(GetShape, range),
        offsetof_get_shape_condition: offset_of!(GetShape, condition),
        sizeof_content_properties: size_of::<ContentProperties>(),
        alignof_content_properties: align_of::<ContentProperties>(),
        offsetof_content_properties_content_type: offset_of!(ContentProperties, content_type),
        offsetof_content_properties_content_encoding: offset_of!(
            ContentProperties,
            content_encoding
        ),
        offsetof_content_properties_content_language: offset_of!(
            ContentProperties,
            content_language
        ),
        offsetof_content_properties_content_disposition: offset_of!(
            ContentProperties,
            content_disposition
        ),
        offsetof_content_properties_cache_control: offset_of!(ContentProperties, cache_control),
        sizeof_write_options: size_of::<WriteOptions>(),
        alignof_write_options: align_of::<WriteOptions>(),
        offsetof_write_options_properties: offset_of!(WriteOptions, properties),
        offsetof_write_options_storage_class: offset_of!(WriteOptions, storage_class),
        sizeof_put_options: size_of::<PutOptions>(),
        alignof_put_options: align_of::<PutOptions>(),
        offsetof_put_options_condition: offset_of!(PutOptions, condition),
        offsetof_put_options_options: offset_of!(PutOptions, options),
        offsetof_put_options_checksum: offset_of!(PutOptions, checksum),
        sizeof_put_item: size_of::<PutItem>(),
        alignof_put_item: align_of::<PutItem>(),
        offsetof_put_item_key: offset_of!(PutItem, key),
        offsetof_put_item_content_len: offset_of!(PutItem, content_len),
        offsetof_put_item_condition_value: offset_of!(PutItem, condition_value),
        offsetof_put_item_checksum: offset_of!(PutItem, checksum),
        sizeof_put_shape: size_of::<PutShape>(),
        alignof_put_shape: align_of::<PutShape>(),
        offsetof_put_shape_condition: offset_of!(PutShape, condition),
        sizeof_stage_options: size_of::<StageOptions>(),
        alignof_stage_options: align_of::<StageOptions>(),
        offsetof_stage_options_checksum: offset_of!(StageOptions, checksum),
        sizeof_stage_block_item: size_of::<StageBlockItem>(),
        alignof_stage_block_item: align_of::<StageBlockItem>(),
        offsetof_stage_block_item_key: offset_of!(StageBlockItem, key),
        offsetof_stage_block_item_id: offset_of!(StageBlockItem, id),
        offsetof_stage_block_item_content_len: offset_of!(StageBlockItem, content_len),
        offsetof_stage_block_item_checksum: offset_of!(StageBlockItem, checksum),
        sizeof_commit_options: size_of::<CommitOptions>(),
        alignof_commit_options: align_of::<CommitOptions>(),
        offsetof_commit_options_condition: offset_of!(CommitOptions, condition),
        offsetof_commit_options_options: offset_of!(CommitOptions, options),
        offsetof_commit_options_content_md5: offset_of!(CommitOptions, content_md5),
        sizeof_block_ref: size_of::<BlockRef>(),
        alignof_block_ref: align_of::<BlockRef>(),
        offsetof_block_ref_id: offset_of!(BlockRef, id),
        offsetof_block_ref_source: offset_of!(BlockRef, source),
        sizeof_commit_item: size_of::<CommitItem>(),
        alignof_commit_item: align_of::<CommitItem>(),
        offsetof_commit_item_key: offset_of!(CommitItem, key),
        offsetof_commit_item_condition_value: offset_of!(CommitItem, condition_value),
        offsetof_commit_item_blocks: offset_of!(CommitItem, blocks),
        offsetof_commit_item_block_count: offset_of!(CommitItem, block_count),
        offsetof_commit_item_content_md5: offset_of!(CommitItem, content_md5),
        sizeof_commit_shape: size_of::<CommitShape>(),
        alignof_commit_shape: align_of::<CommitShape>(),
        offsetof_commit_shape_condition: offset_of!(CommitShape, condition),
        sizeof_list_options: size_of::<ListOptions>(),
        alignof_list_options: align_of::<ListOptions>(),
        offsetof_list_options_delimiter: offset_of!(ListOptions, delimiter),
        offsetof_list_options_max_results: offset_of!(ListOptions, max_results),
        offsetof_list_options_include: offset_of!(ListOptions, include),
        sizeof_list_item: size_of::<ListItem>(),
        alignof_list_item: align_of::<ListItem>(),
        offsetof_list_item_prefix: offset_of!(ListItem, prefix),
        offsetof_list_item_marker: offset_of!(ListItem, marker),
        sizeof_wire_request: size_of::<WireRequest>(),
        alignof_wire_request: align_of::<WireRequest>(),
        offsetof_wire_request_status: offset_of!(WireRequest, status),
        offsetof_wire_request_required: offset_of!(WireRequest, required),
        offsetof_wire_request_method: offset_of!(WireRequest, method),
        offsetof_wire_request_url: offset_of!(WireRequest, url),
        offsetof_wire_request_head: offset_of!(WireRequest, head),
        offsetof_wire_request_fields: offset_of!(WireRequest, fields),
        offsetof_wire_request_body: offset_of!(WireRequest, body),
        offsetof_wire_request_payload_len: offset_of!(WireRequest, payload_len),
        sizeof_header: size_of::<Header>(),
        alignof_header: align_of::<Header>(),
        offsetof_header_present: offset_of!(Header, present),
        offsetof_header_name: offset_of!(Header, name),
        offsetof_header_value: offset_of!(Header, value),
        sizeof_header_ref: size_of::<HeaderRef>(),
        alignof_header_ref: align_of::<HeaderRef>(),
        offsetof_header_ref_name: offset_of!(HeaderRef, name),
        offsetof_header_ref_value: offset_of!(HeaderRef, value),
        sizeof_object_meta: size_of::<ObjectMeta>(),
        alignof_object_meta: align_of::<ObjectMeta>(),
        offsetof_object_meta_size: offset_of!(ObjectMeta, size),
        offsetof_object_meta_e_tag: offset_of!(ObjectMeta, e_tag),
        offsetof_object_meta_last_modified: offset_of!(ObjectMeta, last_modified),
        offsetof_object_meta_version: offset_of!(ObjectMeta, version),
        offsetof_object_meta_content_encoding: offset_of!(ObjectMeta, content_encoding),
        offsetof_object_meta_content_type: offset_of!(ObjectMeta, content_type),
        sizeof_body_window: size_of::<BodyWindow>(),
        alignof_body_window: align_of::<BodyWindow>(),
        offsetof_body_window_object_offset: offset_of!(BodyWindow, object_offset),
        offsetof_body_window_expected_len: offset_of!(BodyWindow, expected_len),
        offsetof_body_window_object_size: offset_of!(BodyWindow, object_size),
        sizeof_failure: size_of::<Failure>(),
        alignof_failure: align_of::<Failure>(),
        offsetof_failure_status: offset_of!(Failure, status),
        offsetof_failure_class: offset_of!(Failure, class),
        offsetof_failure_kind: offset_of!(Failure, kind),
        offsetof_failure_request_id: offset_of!(Failure, request_id),
        sizeof_outcome: size_of::<Outcome>(),
        alignof_outcome: align_of::<Outcome>(),
        offsetof_outcome_kind: offset_of!(Outcome, kind),
        offsetof_outcome_meta: offset_of!(Outcome, meta),
        offsetof_outcome_body: offset_of!(Outcome, body),
        offsetof_outcome_failure: offset_of!(Outcome, failure),
        offsetof_outcome_error: offset_of!(Outcome, error),
        sizeof_list_entry: size_of::<ListEntry>(),
        alignof_list_entry: align_of::<ListEntry>(),
        offsetof_list_entry_kind: offset_of!(ListEntry, kind),
        offsetof_list_entry_key: offset_of!(ListEntry, key),
        offsetof_list_entry_size: offset_of!(ListEntry, size),
        offsetof_list_entry_e_tag: offset_of!(ListEntry, e_tag),
        offsetof_list_entry_last_modified: offset_of!(ListEntry, last_modified),
        offsetof_list_entry_content_type: offset_of!(ListEntry, content_type),
        offsetof_list_entry_raw: offset_of!(ListEntry, raw),
        sizeof_properties: size_of::<Properties>(),
        alignof_properties: align_of::<Properties>(),
        offsetof_properties_remaining: offset_of!(Properties, remaining),
        offsetof_properties_within: offset_of!(Properties, within),
        sizeof_property: size_of::<Property>(),
        alignof_property: align_of::<Property>(),
        offsetof_property_present: offset_of!(Property, present),
        offsetof_property_name: offset_of!(Property, name),
        offsetof_property_value: offset_of!(Property, value),
        sizeof_property_set: size_of::<PropertySet>(),
        alignof_property_set: align_of::<PropertySet>(),
        offsetof_property_set_mask: offset_of!(PropertySet, mask),
        sizeof_fill: size_of::<Fill>(),
        alignof_fill: align_of::<Fill>(),
        offsetof_fill_status: offset_of!(Fill, status),
        offsetof_fill_filled: offset_of!(Fill, filled),
        offsetof_fill_required: offset_of!(Fill, required),
        offsetof_fill_next_marker: offset_of!(Fill, next_marker),
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
    assert!(GetKind::Head as u16 == proto::GetKind::Head as u16);

    assert!(RangeForm::Whole as u16 == proto::RangeForm::Whole as u16);
    assert!(RangeForm::Bounded as u16 == proto::RangeForm::Bounded as u16);
    assert!(RangeForm::Offset as u16 == proto::RangeForm::Offset as u16);
    assert!(RangeForm::Suffix as u16 == proto::RangeForm::Suffix as u16);

    assert!(Condition::None as u16 == proto::ConditionKind::None as u16);
    assert!(Condition::IfMatch as u16 == proto::ConditionKind::IfMatch as u16);
    assert!(Condition::IfNoneMatch as u16 == proto::ConditionKind::IfNoneMatch as u16);
    assert!(Condition::IfModifiedSince as u16 == proto::ConditionKind::IfModifiedSince as u16);
    assert!(Condition::IfUnmodifiedSince as u16 == proto::ConditionKind::IfUnmodifiedSince as u16);

    assert!(Ranging::Whole as u16 == proto::Ranging::Whole as u16);
    assert!(Ranging::Ranged as u16 == proto::Ranging::Ranged as u16);
    assert!(RevisionKind::Current as u16 == proto::RevisionKind::Current as u16);
    assert!(RevisionKind::Snapshot as u16 == proto::RevisionKind::Snapshot as u16);
    assert!(RevisionKind::Version as u16 == proto::RevisionKind::Version as u16);
    assert!(ChecksumKind::Md5 as u16 == proto::ChecksumKind::Md5 as u16);
    assert!(ChecksumKind::Crc64 as u16 == proto::ChecksumKind::Crc64 as u16);
    assert!(BlockSource::Uncommitted as u16 == proto::azure::BlockSource::Uncommitted as u16);
    assert!(BlockSource::Committed as u16 == proto::azure::BlockSource::Committed as u16);
    assert!(BlockSource::Latest as u16 == proto::azure::BlockSource::Latest as u16);
    assert!(AzureNamespace::Unknown as u16 == proto::AzureNamespace::Unknown as u16);
    assert!(AzureNamespace::Flat as u16 == proto::AzureNamespace::Flat as u16);
    assert!(AzureNamespace::Hierarchical as u16 == proto::AzureNamespace::Hierarchical as u16);

    assert!(EntryKind::Object as u16 == proto::EntryKind::Object as u16);
    assert!(EntryKind::Prefix as u16 == proto::EntryKind::Prefix as u16);
    assert!(EntryKind::Directory as u16 == proto::EntryKind::Directory as u16);
    assert!(EntryKind::DeleteMarker as u16 == proto::EntryKind::DeleteMarker as u16);

    // The properties, and that neither list has one the other lacks.
    assert!(proto::BlobProperty::COUNT == BlobProperty::Metadata as usize + 1);
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
    assert!(BlobProperty::Metadata as u16 == proto::BlobProperty::Metadata as u16);

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
    assert!(ServiceError::InvalidUpload as u16 == proto::ServiceErrorKind::InvalidUpload as u16);
    assert!(ServiceError::NoSuchUpload as u16 == proto::ServiceErrorKind::NoSuchUpload as u16);
    // A stored template is the core crate's, byte for byte.
    assert!(TEMPLATE_LEN == proto::azure::Template::STORED_LEN);
    assert!(TIMESTAMPS_LEN == proto::Timestamps::STORED_LEN);
};
