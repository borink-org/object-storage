// What a C program sees of this crate, checked against what Rust compiled.
//
// This test opens no socket. It checks every struct's layout as the C compiler
// computes it. It writes templates and requests into stack buffers, and reads
// one response head from headers in two separate buffers.

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

#include "borink/object_storage.h"

static int failures = 0;

#define CHECK(condition)                                                                 \
    do {                                                                                 \
        if (!(condition)) {                                                              \
            fprintf(stderr, "%s:%d: %s\n", __FILE__, __LINE__, #condition);              \
            failures += 1;                                                               \
        }                                                                                \
    } while (0)

static borink_bytes as_bytes(const char *text) {
    return (borink_bytes){(const uint8_t *)text, strlen(text)};
}

// The instant every request here is written at, formatted once in `main`
// as a program formats it once a second.
static borink_timestamps now;

// Every struct that crosses, measured by this compiler. `borink_layout_disagrees`
// compares it with what Rust compiled, field by field.
static void the_two_compilers_agree_on_every_struct(void) {
    const borink_layout measured = {
        .sizeof_azure_rejection = sizeof(borink_azure_rejection),
        .alignof_azure_rejection = _Alignof(borink_azure_rejection),
        .offsetof_azure_rejection_status = offsetof(borink_azure_rejection, status),
        .offsetof_azure_rejection_code = offsetof(borink_azure_rejection, code),
        .sizeof_bytes = sizeof(borink_bytes),
        .alignof_bytes = _Alignof(borink_bytes),
        .offsetof_bytes_ptr = offsetof(borink_bytes, ptr),
        .offsetof_bytes_len = offsetof(borink_bytes, len),
        .sizeof_bytes_mut = sizeof(borink_bytes_mut),
        .alignof_bytes_mut = _Alignof(borink_bytes_mut),
        .offsetof_bytes_mut_ptr = offsetof(borink_bytes_mut, ptr),
        .offsetof_bytes_mut_len = offsetof(borink_bytes_mut, len),
        .sizeof_span = sizeof(borink_span),
        .alignof_span = _Alignof(borink_span),
        .offsetof_span_start = offsetof(borink_span, start),
        .offsetof_span_len = offsetof(borink_span, len),
        .sizeof_maybe_span = sizeof(borink_maybe_span),
        .alignof_maybe_span = _Alignof(borink_maybe_span),
        .offsetof_maybe_span_present = offsetof(borink_maybe_span, present),
        .offsetof_maybe_span_span = offsetof(borink_maybe_span, span),
        .sizeof_maybe_bytes = sizeof(borink_maybe_bytes),
        .alignof_maybe_bytes = _Alignof(borink_maybe_bytes),
        .offsetof_maybe_bytes_present = offsetof(borink_maybe_bytes, present),
        .offsetof_maybe_bytes_bytes = offsetof(borink_maybe_bytes, bytes),
        .sizeof_maybe_u64 = sizeof(borink_maybe_u64),
        .alignof_maybe_u64 = _Alignof(borink_maybe_u64),
        .offsetof_maybe_u64_present = offsetof(borink_maybe_u64, present),
        .offsetof_maybe_u64_value = offsetof(borink_maybe_u64, value),
        .sizeof_maybe_u32 = sizeof(borink_maybe_u32),
        .alignof_maybe_u32 = _Alignof(borink_maybe_u32),
        .offsetof_maybe_u32_present = offsetof(borink_maybe_u32, present),
        .offsetof_maybe_u32_value = offsetof(borink_maybe_u32, value),
        .sizeof_status = sizeof(borink_status),
        .alignof_status = _Alignof(borink_status),
        .offsetof_status_code = offsetof(borink_status, code),
        .offsetof_status_detail = offsetof(borink_status, detail),
        .sizeof_container = sizeof(borink_container),
        .alignof_container = _Alignof(borink_container),
        .offsetof_container_endpoint = offsetof(borink_container, endpoint),
        .offsetof_container_name = offsetof(borink_container, name),
        .offsetof_container_allow_http = offsetof(borink_container, allow_http),
        .offsetof_container_object = offsetof(borink_container, object),
        .sizeof_timestamps = sizeof(borink_timestamps),
        .alignof_timestamps = _Alignof(borink_timestamps),
        .offsetof_timestamps_stored = offsetof(borink_timestamps, stored),
        .sizeof_template = sizeof(borink_template),
        .alignof_template = _Alignof(borink_template),
        .offsetof_template_stored = offsetof(borink_template, stored),
        .sizeof_prepared = sizeof(borink_prepared),
        .alignof_prepared = _Alignof(borink_prepared),
        .offsetof_prepared_status = offsetof(borink_prepared, status),
        .offsetof_prepared_required = offsetof(borink_prepared, required),
        .offsetof_prepared_template = offsetof(borink_prepared, template_),
        .sizeof_condition_value = sizeof(borink_condition_value),
        .alignof_condition_value = _Alignof(borink_condition_value),
        .offsetof_condition_value_form = offsetof(borink_condition_value, form),
        .offsetof_condition_value_e_tag = offsetof(borink_condition_value, e_tag),
        .offsetof_condition_value_seconds = offsetof(borink_condition_value, seconds),
        .sizeof_range = sizeof(borink_range),
        .alignof_range = _Alignof(borink_range),
        .offsetof_range_form = offsetof(borink_range, form),
        .offsetof_range_start = offsetof(borink_range, start),
        .offsetof_range_end = offsetof(borink_range, end),
        .sizeof_get_options = sizeof(borink_get_options),
        .alignof_get_options = _Alignof(borink_get_options),
        .offsetof_get_options_kind = offsetof(borink_get_options, kind),
        .offsetof_get_options_ranging = offsetof(borink_get_options, ranging),
        .offsetof_get_options_condition = offsetof(borink_get_options, condition),
        .offsetof_get_options_revision = offsetof(borink_get_options, revision),
        .sizeof_get_item = sizeof(borink_get_item),
        .alignof_get_item = _Alignof(borink_get_item),
        .offsetof_get_item_key = offsetof(borink_get_item, key),
        .offsetof_get_item_range = offsetof(borink_get_item, range),
        .offsetof_get_item_condition_value = offsetof(borink_get_item, condition_value),
        .offsetof_get_item_revision = offsetof(borink_get_item, revision),
        .sizeof_get_shape = sizeof(borink_get_shape),
        .alignof_get_shape = _Alignof(borink_get_shape),
        .offsetof_get_shape_kind = offsetof(borink_get_shape, kind),
        .offsetof_get_shape_range = offsetof(borink_get_shape, range),
        .offsetof_get_shape_condition = offsetof(borink_get_shape, condition),
        .sizeof_content_properties = sizeof(borink_content_properties),
        .alignof_content_properties = _Alignof(borink_content_properties),
        .offsetof_content_properties_content_type = offsetof(borink_content_properties, content_type),
        .offsetof_content_properties_content_encoding = offsetof(borink_content_properties, content_encoding),
        .offsetof_content_properties_content_language = offsetof(borink_content_properties, content_language),
        .offsetof_content_properties_content_disposition = offsetof(borink_content_properties, content_disposition),
        .offsetof_content_properties_cache_control = offsetof(borink_content_properties, cache_control),
        .sizeof_write_options = sizeof(borink_write_options),
        .alignof_write_options = _Alignof(borink_write_options),
        .offsetof_write_options_properties = offsetof(borink_write_options, properties),
        .offsetof_write_options_storage_class = offsetof(borink_write_options, storage_class),
        .sizeof_put_options = sizeof(borink_put_options),
        .alignof_put_options = _Alignof(borink_put_options),
        .offsetof_put_options_condition = offsetof(borink_put_options, condition),
        .offsetof_put_options_options = offsetof(borink_put_options, options),
        .offsetof_put_options_checksum = offsetof(borink_put_options, checksum),
        .sizeof_put_item = sizeof(borink_put_item),
        .alignof_put_item = _Alignof(borink_put_item),
        .offsetof_put_item_key = offsetof(borink_put_item, key),
        .offsetof_put_item_content_len = offsetof(borink_put_item, content_len),
        .offsetof_put_item_condition_value = offsetof(borink_put_item, condition_value),
        .offsetof_put_item_checksum = offsetof(borink_put_item, checksum),
        .sizeof_put_shape = sizeof(borink_put_shape),
        .alignof_put_shape = _Alignof(borink_put_shape),
        .offsetof_put_shape_condition = offsetof(borink_put_shape, condition),
        .sizeof_stage_options = sizeof(borink_stage_options),
        .alignof_stage_options = _Alignof(borink_stage_options),
        .offsetof_stage_options_checksum = offsetof(borink_stage_options, checksum),
        .sizeof_stage_block_item = sizeof(borink_stage_block_item),
        .alignof_stage_block_item = _Alignof(borink_stage_block_item),
        .offsetof_stage_block_item_key = offsetof(borink_stage_block_item, key),
        .offsetof_stage_block_item_id = offsetof(borink_stage_block_item, id),
        .offsetof_stage_block_item_content_len = offsetof(borink_stage_block_item, content_len),
        .offsetof_stage_block_item_checksum = offsetof(borink_stage_block_item, checksum),
        .sizeof_commit_options = sizeof(borink_commit_options),
        .alignof_commit_options = _Alignof(borink_commit_options),
        .offsetof_commit_options_condition = offsetof(borink_commit_options, condition),
        .offsetof_commit_options_options = offsetof(borink_commit_options, options),
        .offsetof_commit_options_content_md5 = offsetof(borink_commit_options, content_md5),
        .sizeof_block_ref = sizeof(borink_block_ref),
        .alignof_block_ref = _Alignof(borink_block_ref),
        .offsetof_block_ref_id = offsetof(borink_block_ref, id),
        .offsetof_block_ref_source = offsetof(borink_block_ref, source),
        .sizeof_commit_item = sizeof(borink_commit_item),
        .alignof_commit_item = _Alignof(borink_commit_item),
        .offsetof_commit_item_key = offsetof(borink_commit_item, key),
        .offsetof_commit_item_condition_value = offsetof(borink_commit_item, condition_value),
        .offsetof_commit_item_blocks = offsetof(borink_commit_item, blocks),
        .offsetof_commit_item_block_count = offsetof(borink_commit_item, block_count),
        .offsetof_commit_item_content_md5 = offsetof(borink_commit_item, content_md5),
        .sizeof_commit_shape = sizeof(borink_commit_shape),
        .alignof_commit_shape = _Alignof(borink_commit_shape),
        .offsetof_commit_shape_condition = offsetof(borink_commit_shape, condition),
        .sizeof_list_options = sizeof(borink_list_options),
        .alignof_list_options = _Alignof(borink_list_options),
        .offsetof_list_options_delimiter = offsetof(borink_list_options, delimiter),
        .offsetof_list_options_max_results = offsetof(borink_list_options, max_results),
        .offsetof_list_options_include = offsetof(borink_list_options, include),
        .sizeof_list_item = sizeof(borink_list_item),
        .alignof_list_item = _Alignof(borink_list_item),
        .offsetof_list_item_prefix = offsetof(borink_list_item, prefix),
        .offsetof_list_item_marker = offsetof(borink_list_item, marker),
        .sizeof_wire_request = sizeof(borink_wire_request),
        .alignof_wire_request = _Alignof(borink_wire_request),
        .offsetof_wire_request_status = offsetof(borink_wire_request, status),
        .offsetof_wire_request_required = offsetof(borink_wire_request, required),
        .offsetof_wire_request_method = offsetof(borink_wire_request, method),
        .offsetof_wire_request_url = offsetof(borink_wire_request, url),
        .offsetof_wire_request_head = offsetof(borink_wire_request, head),
        .offsetof_wire_request_fields = offsetof(borink_wire_request, fields),
        .offsetof_wire_request_body = offsetof(borink_wire_request, body),
        .offsetof_wire_request_payload_len = offsetof(borink_wire_request, payload_len),
        .sizeof_header = sizeof(borink_header),
        .alignof_header = _Alignof(borink_header),
        .offsetof_header_present = offsetof(borink_header, present),
        .offsetof_header_name = offsetof(borink_header, name),
        .offsetof_header_value = offsetof(borink_header, value),
        .sizeof_header_ref = sizeof(borink_header_ref),
        .alignof_header_ref = _Alignof(borink_header_ref),
        .offsetof_header_ref_name = offsetof(borink_header_ref, name),
        .offsetof_header_ref_value = offsetof(borink_header_ref, value),
        .sizeof_object_meta = sizeof(borink_object_meta),
        .alignof_object_meta = _Alignof(borink_object_meta),
        .offsetof_object_meta_size = offsetof(borink_object_meta, size),
        .offsetof_object_meta_e_tag = offsetof(borink_object_meta, e_tag),
        .offsetof_object_meta_last_modified = offsetof(borink_object_meta, last_modified),
        .offsetof_object_meta_version = offsetof(borink_object_meta, version),
        .offsetof_object_meta_content_encoding = offsetof(borink_object_meta, content_encoding),
        .offsetof_object_meta_content_type = offsetof(borink_object_meta, content_type),
        .sizeof_body_window = sizeof(borink_body_window),
        .alignof_body_window = _Alignof(borink_body_window),
        .offsetof_body_window_object_offset = offsetof(borink_body_window, object_offset),
        .offsetof_body_window_expected_len = offsetof(borink_body_window, expected_len),
        .offsetof_body_window_object_size = offsetof(borink_body_window, object_size),
        .sizeof_failure = sizeof(borink_failure),
        .alignof_failure = _Alignof(borink_failure),
        .offsetof_failure_status = offsetof(borink_failure, status),
        .offsetof_failure_class = offsetof(borink_failure, class_),
        .offsetof_failure_kind = offsetof(borink_failure, kind),
        .offsetof_failure_request_id = offsetof(borink_failure, request_id),
        .sizeof_outcome = sizeof(borink_outcome),
        .alignof_outcome = _Alignof(borink_outcome),
        .offsetof_outcome_kind = offsetof(borink_outcome, kind),
        .offsetof_outcome_meta = offsetof(borink_outcome, meta),
        .offsetof_outcome_body = offsetof(borink_outcome, body),
        .offsetof_outcome_failure = offsetof(borink_outcome, failure),
        .offsetof_outcome_error = offsetof(borink_outcome, error),
        .sizeof_list_entry = sizeof(borink_list_entry),
        .alignof_list_entry = _Alignof(borink_list_entry),
        .offsetof_list_entry_kind = offsetof(borink_list_entry, kind),
        .offsetof_list_entry_key = offsetof(borink_list_entry, key),
        .offsetof_list_entry_size = offsetof(borink_list_entry, size),
        .offsetof_list_entry_e_tag = offsetof(borink_list_entry, e_tag),
        .offsetof_list_entry_last_modified = offsetof(borink_list_entry, last_modified),
        .offsetof_list_entry_content_type = offsetof(borink_list_entry, content_type),
        .offsetof_list_entry_raw = offsetof(borink_list_entry, raw),
        .sizeof_properties = sizeof(borink_properties),
        .alignof_properties = _Alignof(borink_properties),
        .offsetof_properties_remaining = offsetof(borink_properties, remaining),
        .offsetof_properties_within = offsetof(borink_properties, within),
        .sizeof_property = sizeof(borink_property),
        .alignof_property = _Alignof(borink_property),
        .offsetof_property_present = offsetof(borink_property, present),
        .offsetof_property_name = offsetof(borink_property, name),
        .offsetof_property_value = offsetof(borink_property, value),
        .sizeof_property_set = sizeof(borink_property_set),
        .alignof_property_set = _Alignof(borink_property_set),
        .offsetof_property_set_mask = offsetof(borink_property_set, mask),
        .sizeof_fill = sizeof(borink_fill),
        .alignof_fill = _Alignof(borink_fill),
        .offsetof_fill_status = offsetof(borink_fill, status),
        .offsetof_fill_filled = offsetof(borink_fill, filled),
        .offsetof_fill_required = offsetof(borink_fill, required),
        .offsetof_fill_next_marker = offsetof(borink_fill, next_marker),
    };
    // A `borink_layout` is `size_t` fields alone, so a field this file forgot
    // to fill would be 0 and would be reported as a disagreement.
    CHECK(borink_layout_disagrees(&measured) == 0);
}

static borink_container located(void) {
    borink_container container = {0};
    container.endpoint = as_bytes("https://account.blob.core.windows.net");
    container.name = as_bytes("container");
    return container;
}

static const borink_get_options ranged_reads = {BORINK_GET_KIND_BYTES, BORINK_RANGING_RANGED,
                                                BORINK_CONDITION_NONE,
                                                BORINK_REVISION_KIND_CURRENT};

// Whether a request's field lines hold `name: value`, read one by one.
static bool holds_field(const uint8_t *buffer, const borink_wire_request *request,
                        const char *name, const char *value) {
    borink_bytes fields = {buffer + request->fields.start, request->fields.len};
    for (borink_header field = borink_next_header(&fields); field.present;
         field = borink_next_header(&fields)) {
        if (field.name.len == strlen(name) && memcmp(field.name.ptr, name, field.name.len) == 0) {
            return field.value.len == strlen(value) &&
                   memcmp(field.value.ptr, value, field.value.len) == 0;
        }
    }
    return false;
}

// The template is written once into storage this file owns, and each request
// from it into another, named by offset into it.
static void one_read_is_written_from_its_template(void) {
    const borink_container container = located();
    CHECK(borink_container_check(&container).code == 0);

    uint8_t template_buffer[512];
    const borink_prepared reads =
        borink_prepare_get(&container, as_bytes("token"), BORINK_AZURE_NAMESPACE_UNKNOWN, &ranged_reads,
                           (borink_bytes_mut){template_buffer, sizeof template_buffer});
    CHECK(reads.status.code == 0);
    CHECK(reads.required > 0 && reads.required <= sizeof template_buffer);
    const borink_bytes template_bytes = {template_buffer, reads.required};
    CHECK(borink_template_check(&reads.template_, template_bytes).code == 0);

    const borink_get_item item = {.key = as_bytes("object.bin"),
                                  .range = {BORINK_RANGE_FORM_BOUNDED, 2, 6}};
    uint8_t buffer[1024];
    const borink_wire_request request = borink_encode_get(
        &reads.template_, template_bytes, &item, &now, (borink_bytes_mut){buffer, sizeof buffer});

    CHECK(request.status.code == 0);
    CHECK(request.method == BORINK_METHOD_GET);
    CHECK(request.required <= sizeof buffer);
    CHECK(!request.body.present);
    CHECK(request.url.len == strlen("https://account.blob.core.windows.net/container/object.bin"));
    CHECK(memcmp(buffer + request.url.start,
                 "https://account.blob.core.windows.net/container/object.bin",
                 request.url.len) == 0);
    CHECK(memcmp(buffer + request.head.start, "GET /container/object.bin HTTP/1.1\r\n",
                 strlen("GET /container/object.bin HTTP/1.1\r\n")) == 0);
    CHECK(holds_field(buffer, &request, "range", "bytes=2-5"));
    CHECK(holds_field(buffer, &request, "authorization", "Bearer token"));

    // The shape is all a response needs, and it holds no pointer.
    const borink_get_shape shape = borink_get_item_shape(&item, &ranged_reads);
    CHECK(shape.kind == BORINK_GET_KIND_BYTES);
    CHECK(shape.range.form == BORINK_RANGE_FORM_BOUNDED);

    // The template is plain bytes, so a copy of it writes the same request.
    borink_template copied;
    memcpy(&copied, &reads.template_, sizeof copied);
    uint8_t again[1024];
    const borink_wire_request same = borink_encode_get(
        &copied, template_bytes, &item, &now, (borink_bytes_mut){again, sizeof again});
    CHECK(same.required == request.required);
    CHECK(memcmp(again, buffer, request.required) == 0);
}

// An empty buffer is refused with the exact size that the template or the
// request needs.
static void a_buffer_that_is_too_small_reports_the_size_it_needs(void) {
    const borink_container container = located();
    const borink_get_options options = {BORINK_GET_KIND_BYTES, BORINK_RANGING_WHOLE,
                                        BORINK_CONDITION_NONE, BORINK_REVISION_KIND_CURRENT};
    const borink_bytes_mut no_room = {NULL, 0};
    const borink_prepared sized =
        borink_prepare_get(&container, as_bytes("token"), BORINK_AZURE_NAMESPACE_UNKNOWN, &options, no_room);
    CHECK(sized.status.code == BORINK_ERROR_CODE_CAPACITY);
    CHECK(sized.required > 0 && sized.required <= 512);

    uint8_t template_buffer[512];
    const borink_prepared reads =
        borink_prepare_get(&container, as_bytes("token"), BORINK_AZURE_NAMESPACE_UNKNOWN, &options,
                           (borink_bytes_mut){template_buffer, sizeof template_buffer});
    CHECK(reads.required == sized.required);
    const borink_get_item item = {.key = as_bytes("object.bin"),
                                  .range = {BORINK_RANGE_FORM_WHOLE, 0, 0}};
    const borink_wire_request refused = borink_encode_get(
        &reads.template_, (borink_bytes){template_buffer, reads.required}, &item, &now, no_room);
    CHECK(refused.status.code == BORINK_ERROR_CODE_CAPACITY);
    CHECK(refused.required > 0);

    uint8_t sentence[128];
    const size_t length =
        borink_describe_status(refused.status, (borink_bytes_mut){sentence, sizeof sentence});
    CHECK(length > 0 && length <= sizeof sentence);
}

// The head reaches this crate as borrowed bytes, from wherever they are. These
// live in two separate arrays, and the outcome points back into both.
static void one_response_head_is_read_from_two_buffers(void) {
    const borink_get_shape shape = {BORINK_GET_KIND_BYTES,
                                    {BORINK_RANGE_FORM_WHOLE, 0, 0},
                                    BORINK_CONDITION_NONE};
    const char tag[] = "\"tag\"";
    const char identifier[] = "request-123";
    const borink_header_ref headers[] = {
        {as_bytes("ETag"), as_bytes(tag)},
        {as_bytes("Content-Length"), as_bytes("10")},
        {as_bytes("x-ms-request-id"), as_bytes(identifier)},
    };
    const borink_outcome outcome =
        borink_accept_get_head(shape, 200, headers, sizeof headers / sizeof headers[0]);

    CHECK(outcome.kind == BORINK_OUTCOME_KIND_BODY);
    CHECK(outcome.meta.e_tag.present);
    CHECK(outcome.meta.e_tag.bytes.ptr == (const uint8_t *)tag);
    CHECK(outcome.body.expected_len.present);
    CHECK(outcome.body.expected_len.value == 10);

    uint8_t sentence[128];
    const size_t length =
        borink_describe(&outcome, (borink_bytes_mut){sentence, sizeof sentence});
    CHECK(length > 0 && length <= sizeof sentence);
}

// A number that names no value of an enum stops the call rather than being read
// as the value that happens to be oldest.
static void an_unknown_number_is_refused(void) {
    const borink_container container = located();
    const borink_get_options options = {4095, BORINK_RANGING_WHOLE, BORINK_CONDITION_NONE,
                                        BORINK_REVISION_KIND_CURRENT};
    uint8_t buffer[512];
    const borink_prepared refused = borink_prepare_get(
        &container, as_bytes("token"), BORINK_AZURE_NAMESPACE_UNKNOWN, &options, (borink_bytes_mut){buffer, sizeof buffer});

    CHECK(refused.status.code == BORINK_ERROR_CODE_INVALID_REQUEST);
    CHECK(refused.required == 0);

    // A template that this library did not write is refused too.
    const borink_template zeros = {{0}};
    const borink_get_item item = {.key = as_bytes("object.bin"),
                                  .range = {BORINK_RANGE_FORM_WHOLE, 0, 0}};
    const borink_wire_request request = borink_encode_get(
        &zeros, (borink_bytes){buffer, sizeof buffer}, &item, &now,
        (borink_bytes_mut){buffer, 0});
    CHECK(request.status.code == BORINK_ERROR_CODE_INVALID_REQUEST);
}

// A blob written in blocks. Each block is staged on its own, and then one
// commit writes the block list from the program's array as its body.
static void a_blob_is_written_in_blocks(void) {
    const borink_container container = located();
    const borink_stage_options stage = {BORINK_CHECKSUM_KIND_NONE};
    uint8_t stage_buffer[512];
    const borink_prepared stages =
        borink_prepare_stage_block(&container, as_bytes("token"), BORINK_AZURE_NAMESPACE_UNKNOWN, &stage,
                                   (borink_bytes_mut){stage_buffer, sizeof stage_buffer});
    CHECK(stages.status.code == 0);

    const char *ids[] = {"YmxvY2stMDAwMDA=", "YmxvY2stMDAwMDE="};
    uint8_t buffer[1024];
    const borink_stage_block_item block = {
        .key = as_bytes("blob.bin"), .id = as_bytes(ids[0]), .content_len = 4096};
    const borink_wire_request staged =
        borink_encode_stage_block(&stages.template_, (borink_bytes){stage_buffer, stages.required},
                                  &block, &now, (borink_bytes_mut){buffer, sizeof buffer});
    CHECK(staged.status.code == 0);
    CHECK(staged.method == BORINK_METHOD_PUT);
    CHECK(staged.payload_len == 4096);
    CHECK(holds_field(buffer, &staged, "content-length", "4096"));

    const borink_header_ref etag[] = {{as_bytes("ETag"), as_bytes("\"block\"")}};
    CHECK(borink_accept_stage_block_head(201, etag, 1).kind == BORINK_OUTCOME_KIND_STAGED);

    const borink_commit_options commit = {.condition = BORINK_CONDITION_NONE};
    uint8_t commit_buffer[512];
    const borink_prepared commits =
        borink_prepare_commit_blocks(&container, as_bytes("token"), BORINK_AZURE_NAMESPACE_UNKNOWN, &commit,
                                     (borink_bytes_mut){commit_buffer, sizeof commit_buffer});
    CHECK(commits.status.code == 0);
    const borink_block_ref blocks[] = {
        {as_bytes(ids[0]), BORINK_BLOCK_SOURCE_LATEST},
        {as_bytes(ids[1]), BORINK_BLOCK_SOURCE_LATEST},
    };
    const borink_commit_item item = {
        .key = as_bytes("blob.bin"), .blocks = blocks, .block_count = 2};
    const borink_wire_request committed = borink_encode_commit_blocks(
        &commits.template_, (borink_bytes){commit_buffer, commits.required}, &item, &now,
        (borink_bytes_mut){buffer, sizeof buffer});
    CHECK(committed.status.code == 0);
    CHECK(committed.body.present);
    CHECK(committed.payload_len == committed.body.span.len);
    const char *list = "<Latest>YmxvY2stMDAwMDA=</Latest><Latest>YmxvY2stMDAwMDE=</Latest>"
                       "</BlockList>";
    CHECK(committed.body.span.len > strlen(list));
    CHECK(memcmp(buffer + committed.body.span.start + committed.body.span.len - strlen(list), list,
                 strlen(list)) == 0);

    const borink_commit_shape shape = borink_commit_item_shape(&item, &commit);
    CHECK(borink_accept_commit_blocks_head(shape, 201, etag, 1).kind ==
          BORINK_OUTCOME_KIND_COMMITTED);
}

// One page of a listing, read out of a buffer this file owns. The entries
// point back into that buffer, and the array is the budget.
static void one_page_is_read_out_of_a_body(void) {
    const borink_container container = located();
    const borink_list_options options = {as_bytes("/"), {true, 2}, 0};
    uint8_t template_buffer[512];
    const borink_prepared pages =
        borink_prepare_list(&container, as_bytes("token"), BORINK_AZURE_NAMESPACE_UNKNOWN, &options,
                            (borink_bytes_mut){template_buffer, sizeof template_buffer});
    CHECK(pages.status.code == 0);
    const borink_list_item item = {.prefix = as_bytes("directory/")};
    uint8_t buffer[1024];
    const borink_wire_request request =
        borink_encode_list(&pages.template_, (borink_bytes){template_buffer, pages.required},
                           &item, &now, (borink_bytes_mut){buffer, sizeof buffer});
    CHECK(request.status.code == 0);
    CHECK(request.method == BORINK_METHOD_GET);

    const borink_header_ref headers[] = {{as_bytes("Content-Length"), as_bytes("214")}};
    const borink_outcome outcome =
        borink_accept_list_head(200, headers, sizeof headers / sizeof headers[0]);
    CHECK(outcome.kind == BORINK_OUTCOME_KIND_PAGE);
    CHECK(outcome.body.expected_len.value == 214);

    // The body as the service sent it. Reading it decodes the text in place,
    // so it is a buffer of this program and not a literal.
    char body[] = "<EnumerationResults><Blobs>"
                  "<Blob><Name>a.txt</Name><Properties><Etag>0x1</Etag>"
                  "<Content-Length>4</Content-Length></Properties></Blob>"
                  "<Blob><Name>b.txt</Name><Properties><Etag>0x2</Etag>"
                  "<Content-Length>8</Content-Length></Properties></Blob>"
                  "</Blobs><NextMarker>next</NextMarker></EnumerationResults>";
    borink_list_entry entries[2] = {0};
    const borink_bytes_mut page = {(uint8_t *)body, strlen(body)};

    const borink_fill fill = borink_fill_listing(page, entries, 2);
    CHECK(fill.status.code == 0);
    CHECK(fill.filled == 2);
    CHECK(entries[0].kind == BORINK_ENTRY_KIND_OBJECT);
    CHECK(entries[0].key.len == strlen("a.txt"));
    CHECK(memcmp(entries[0].key.ptr, "a.txt", entries[0].key.len) == 0);
    CHECK(entries[0].size.present && entries[0].size.value == 4);
    CHECK(entries[0].e_tag.present);
    CHECK(entries[1].size.value == 8);
    CHECK(fill.next_marker.present);
    CHECK(memcmp(fill.next_marker.bytes.ptr, "next", fill.next_marker.bytes.len) == 0);
}

// An array smaller than the page is refused, and the fill says how many
// entries the page holds.
static void an_array_smaller_than_the_page_is_refused(void) {
    char body[] = "<EnumerationResults><Blobs>"
                  "<Blob><Name>a.txt</Name><Properties>"
                  "<Content-Length>4</Content-Length></Properties></Blob>"
                  "<Blob><Name>b.txt</Name><Properties>"
                  "<Content-Length>8</Content-Length></Properties></Blob>"
                  "</Blobs><NextMarker /></EnumerationResults>";
    borink_list_entry entries[1] = {0};
    const borink_bytes_mut page = {(uint8_t *)body, strlen(body)};

    const borink_fill fill = borink_fill_listing(page, entries, 1);
    CHECK(fill.status.code == BORINK_ERROR_CODE_CAPACITY);
    CHECK(fill.required == 2);
    CHECK(fill.filled == 0);
}

// A body that is not a page is refused, and no entry is reported.
static void a_body_that_is_not_a_page_is_refused(void) {
    char body[] = "<Error><Code>ServerBusy</Code></Error>";
    borink_list_entry entries[2] = {0};
    const borink_fill fill =
        borink_fill_listing((borink_bytes_mut){(uint8_t *)body, strlen(body)}, entries, 2);

    CHECK(fill.status.code == BORINK_ERROR_CODE_RESPONSE);
    CHECK(fill.filled == 0);
}

// What a listing lends back is read by the two calls beside it, so a C program
// writes neither the quoting nor the date parser itself.
static void the_helpers_read_what_a_listing_lends(void) {
    uint8_t room[32];
    const borink_maybe_bytes quoted =
        borink_quoted_etag(as_bytes("0x8DF0"), (borink_bytes_mut){room, sizeof room});
    CHECK(quoted.present);
    CHECK(quoted.bytes.len == strlen("\"0x8DF0\""));
    CHECK(memcmp(quoted.bytes.ptr, "\"0x8DF0\"", quoted.bytes.len) == 0);

    // Two bytes more than the tag is what it needs, and less writes nothing.
    const borink_maybe_bytes refused =
        borink_quoted_etag(as_bytes("0x8DF0"), (borink_bytes_mut){room, 6});
    CHECK(!refused.present);

    const borink_maybe_u64 read = borink_http_date_ms(as_bytes("Wed, 26 Aug 2026 12:00:00 GMT"));
    CHECK(read.present);
    CHECK(read.value == 1787745600000u);
    CHECK(!borink_http_date_ms(as_bytes("yesterday")).present);
}

// A value that no field of an entry carries is read out of the entry itself,
// by name or in one walk over it.
static void a_property_is_read_out_of_the_entry(void) {
    char body[] = "<EnumerationResults><Blobs><Blob><Name>a.txt</Name>"
                  "<Properties><Content-Length>4</Content-Length>"
                  "<AccessTier>Hot</AccessTier><Content-Encoding /></Properties>"
                  "</Blob></Blobs><NextMarker /></EnumerationResults>";
    borink_list_entry entries[1] = {0};
    const borink_fill fill = borink_fill_listing(
        (borink_bytes_mut){(uint8_t *)body, strlen(body)}, entries, 1);
    CHECK(fill.filled == 1);
    CHECK(entries[0].raw.len > 0);

    const borink_maybe_bytes tier =
        borink_entry_property(&entries[0], as_bytes("AccessTier"));
    CHECK(tier.present);
    CHECK(memcmp(tier.bytes.ptr, "Hot", tier.bytes.len) == 0);
    // An element that carries nothing is present and empty; one the entry
    // never wrote is absent.
    CHECK(borink_entry_property(&entries[0], as_bytes("Content-Encoding")).present);
    CHECK(borink_entry_property(&entries[0], as_bytes("Content-Encoding")).bytes.len == 0);
    CHECK(!borink_entry_property(&entries[0], as_bytes("Snapshot")).present);

    // The walk reports each value once and then stays ended.
    borink_properties walk = borink_entry_properties(&entries[0]);
    size_t found = 0;
    for (borink_property read = borink_next_property(&walk); read.present;
         read = borink_next_property(&walk)) {
        found += 1;
        CHECK(read.name.len > 0);
    }
    CHECK(found == 4);
    CHECK(!borink_next_property(&walk).present);

    // A reference is resolved into the caller's own buffer.
    uint8_t room[16];
    const borink_maybe_bytes decoded =
        borink_decode_property(as_bytes("a&amp;b"), (borink_bytes_mut){room, sizeof room});
    CHECK(decoded.present);
    CHECK(decoded.bytes.len == 3);
    CHECK(memcmp(decoded.bytes.ptr, "a&b", 3) == 0);
}

// The values of the properties a program asks for are read with the page,
// one row per entry, and the program never touches the entry's bytes.
static void the_properties_a_program_asks_for_are_read_with_the_page(void) {
    char body[] = "<EnumerationResults><Blobs>"
                  "<Blob><Name>a.txt</Name><Properties><Content-Length>4</Content-Length>"
                  "<Content-Encoding>gzip</Content-Encoding><AccessTier>Hot</AccessTier>"
                  "</Properties></Blob>"
                  "<Blob><Name>b.txt</Name><Properties><Content-Length>0</Content-Length>"
                  "<Content-Encoding /></Properties></Blob>"
                  "<BlobPrefix><Name>c/</Name></BlobPrefix>"
                  "</Blobs><NextMarker /></EnumerationResults>";

    borink_property_set wanted = {0};
    wanted = borink_property_set_with(wanted, BORINK_BLOB_PROPERTY_CONTENT_ENCODING);
    wanted = borink_property_set_with(wanted, BORINK_BLOB_PROPERTY_ACCESS_TIER);
    // A number that names no property changes nothing.
    wanted = borink_property_set_with(wanted, 60000);
    const size_t width = borink_property_set_len(wanted);
    CHECK(width == 2);
    const size_t tier = borink_property_slot(wanted, BORINK_BLOB_PROPERTY_ACCESS_TIER);
    const size_t encoding = borink_property_slot(wanted, BORINK_BLOB_PROPERTY_CONTENT_ENCODING);
    CHECK(tier == 0 && encoding == 1);
    CHECK(borink_property_slot(wanted, BORINK_BLOB_PROPERTY_BLOB_TYPE) == width);

    borink_list_entry entries[3] = {0};
    borink_maybe_bytes values[3 * 2] = {0};
    const borink_fill fill = borink_fill_listing_with(
        (borink_bytes_mut){(uint8_t *)body, strlen(body)}, entries, 3, wanted, values,
        sizeof values / sizeof values[0]);
    CHECK(fill.status.code == 0);
    CHECK(fill.filled == 3);

    const borink_maybe_bytes *a = &values[0 * width];
    CHECK(a[tier].present && memcmp(a[tier].bytes.ptr, "Hot", a[tier].bytes.len) == 0);
    CHECK(a[encoding].present && a[encoding].bytes.len == 4);
    // An element written empty is present and empty; one not written is absent.
    const borink_maybe_bytes *b = &values[1 * width];
    CHECK(!b[tier].present);
    CHECK(b[encoding].present && b[encoding].bytes.len == 0);
    // A group of keys gives no values.
    const borink_maybe_bytes *c = &values[2 * width];
    CHECK(!c[tier].present && !c[encoding].present);

    const borink_bytes name = borink_property_name(BORINK_BLOB_PROPERTY_CREATION_TIME);
    CHECK(name.len == strlen("Creation-Time"));
    CHECK(memcmp(name.ptr, "Creation-Time", name.len) == 0);
    CHECK(borink_property_name(60000).len == 0);
}

int main(void) {
    now = borink_timestamps_from_unix(1787400000);
    the_two_compilers_agree_on_every_struct();
    one_read_is_written_from_its_template();
    a_buffer_that_is_too_small_reports_the_size_it_needs();
    one_response_head_is_read_from_two_buffers();
    an_unknown_number_is_refused();
    a_blob_is_written_in_blocks();
    one_page_is_read_out_of_a_body();
    an_array_smaller_than_the_page_is_refused();
    a_body_that_is_not_a_page_is_refused();
    the_helpers_read_what_a_listing_lends();
    a_property_is_read_out_of_the_entry();
    the_properties_a_program_asks_for_are_read_with_the_page();

    if (failures != 0) {
        fprintf(stderr, "%d check(s) failed\n", failures);
        return 1;
    }
    return 0;
}
