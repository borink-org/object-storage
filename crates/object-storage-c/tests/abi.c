// What a C program sees of this crate, checked against what Rust compiled.
//
// This test opens no socket. It checks three things: that the C compiler lays
// every struct out as the Rust declarations promise, that one request head is
// written into a stack buffer, and that one response head is read from headers
// in two separate buffers.

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

// Every struct that crosses, measured by this compiler. `borink_layout_disagrees`
// compares it with what Rust compiled, field by field.
static void the_two_compilers_agree_on_every_struct(void) {
    const borink_layout measured = {
        .sizeof_bytes = sizeof(borink_bytes),
        .alignof_bytes = _Alignof(borink_bytes),
        .offsetof_bytes_len = offsetof(borink_bytes, len),
        .sizeof_bytes_mut = sizeof(borink_bytes_mut),
        .alignof_bytes_mut = _Alignof(borink_bytes_mut),
        .sizeof_span = sizeof(borink_span),
        .offsetof_span_len = offsetof(borink_span, len),
        .sizeof_maybe_bytes = sizeof(borink_maybe_bytes),
        .alignof_maybe_bytes = _Alignof(borink_maybe_bytes),
        .offsetof_maybe_bytes_bytes = offsetof(borink_maybe_bytes, bytes),
        .sizeof_maybe_u64 = sizeof(borink_maybe_u64),
        .alignof_maybe_u64 = _Alignof(borink_maybe_u64),
        .offsetof_maybe_u64_value = offsetof(borink_maybe_u64, value),
        .sizeof_status = sizeof(borink_status),
        .offsetof_status_detail = offsetof(borink_status, detail),
        .sizeof_session = sizeof(borink_session),
        .offsetof_session_container = offsetof(borink_session, container),
        .offsetof_session_token = offsetof(borink_session, token),
        .sizeof_range = sizeof(borink_range),
        .alignof_range = _Alignof(borink_range),
        .offsetof_range_start = offsetof(borink_range, start),
        .offsetof_range_end = offsetof(borink_range, end),
        .sizeof_get_shape = sizeof(borink_get_shape),
        .offsetof_get_shape_range = offsetof(borink_get_shape, range),
        .offsetof_get_shape_condition = offsetof(borink_get_shape, condition),
        .sizeof_put_shape = sizeof(borink_put_shape),
        .sizeof_delete_shape = sizeof(borink_delete_shape),
        .offsetof_delete_shape_condition = offsetof(borink_delete_shape, condition),
        .sizeof_request_header = sizeof(borink_request_header),
        .offsetof_request_header_value = offsetof(borink_request_header, value),
        .sizeof_request_head = sizeof(borink_request_head),
        .alignof_request_head = _Alignof(borink_request_head),
        .offsetof_request_head_required = offsetof(borink_request_head, required),
        .offsetof_request_head_method = offsetof(borink_request_head, method),
        .offsetof_request_head_url = offsetof(borink_request_head, url),
        .offsetof_request_head_header_count = offsetof(borink_request_head, header_count),
        .offsetof_request_head_headers = offsetof(borink_request_head, headers),
        .sizeof_header_ref = sizeof(borink_header_ref),
        .offsetof_header_ref_value = offsetof(borink_header_ref, value),
        .sizeof_object_meta = sizeof(borink_object_meta),
        .offsetof_object_meta_e_tag = offsetof(borink_object_meta, e_tag),
        .offsetof_object_meta_last_modified = offsetof(borink_object_meta, last_modified),
        .offsetof_object_meta_version = offsetof(borink_object_meta, version),
        .offsetof_object_meta_content_encoding = offsetof(borink_object_meta, content_encoding),
        .sizeof_body_window = sizeof(borink_body_window),
        .offsetof_body_window_expected_len = offsetof(borink_body_window, expected_len),
        .offsetof_body_window_object_size = offsetof(borink_body_window, object_size),
        .sizeof_failure = sizeof(borink_failure),
        .offsetof_failure_class = offsetof(borink_failure, class_),
        .offsetof_failure_kind = offsetof(borink_failure, kind),
        .offsetof_failure_request_id = offsetof(borink_failure, request_id),
        .sizeof_outcome = sizeof(borink_outcome),
        .alignof_outcome = _Alignof(borink_outcome),
        .offsetof_outcome_meta = offsetof(borink_outcome, meta),
        .offsetof_outcome_body = offsetof(borink_outcome, body),
        .offsetof_outcome_failure = offsetof(borink_outcome, failure),
        .offsetof_outcome_error = offsetof(borink_outcome, error),
    };
    // A `borink_layout` is `size_t` fields alone, so a field this file forgot
    // to fill would be 0 and would be reported as a disagreement.
    CHECK(borink_layout_disagrees(&measured) == 0);
}

static borink_session opened(void) {
    borink_session session;
    session.endpoint = as_bytes("https://account.blob.core.windows.net");
    session.container = as_bytes("container");
    session.token = as_bytes("token");
    return session;
}

// The head is written into storage this file owns, and named by offset into it.
static void one_request_head_is_written_into_a_stack_buffer(void) {
    const borink_session session = opened();
    CHECK(borink_validate(&session).code == 0);

    const borink_get_shape shape = {BORINK_GET_KIND_BYTES,
                                    {BORINK_RANGE_FORM_BOUNDED, 2, 6},
                                    BORINK_CONDITION_NONE};
    uint8_t buffer[1024];
    const borink_bytes no_bytes = {NULL, 0};
    const borink_request_head head =
        borink_encode_get(&session, &shape, as_bytes("object.bin"), no_bytes,
                          (borink_bytes_mut){buffer, sizeof buffer}, 1787400000);

    CHECK(head.status.code == 0);
    CHECK(head.method == BORINK_METHOD_GET);
    CHECK(head.header_count == 4);
    CHECK(head.required <= sizeof buffer);
    CHECK(head.url.len == strlen("https://account.blob.core.windows.net/container/object.bin"));
    CHECK(memcmp(buffer + head.url.start,
                 "https://account.blob.core.windows.net/container/object.bin",
                 head.url.len) == 0);

    bool ranged = false;
    for (size_t index = 0; index < head.header_count; index += 1) {
        const borink_request_header header = head.headers[index];
        CHECK(header.name.start + header.name.len <= head.required);
        CHECK(header.value.start + header.value.len <= head.required);
        if (header.name.len == strlen("range") &&
            memcmp(buffer + header.name.start, "range", header.name.len) == 0) {
            ranged = memcmp(buffer + header.value.start, "bytes=2-5", header.value.len) == 0;
        }
    }
    CHECK(ranged);
}

// An empty buffer is refused with the exact size that the head needs.
static void a_buffer_that_is_too_small_reports_the_size_it_needs(void) {
    const borink_session session = opened();
    const borink_get_shape shape = {BORINK_GET_KIND_BYTES,
                                    {BORINK_RANGE_FORM_WHOLE, 0, 0},
                                    BORINK_CONDITION_NONE};
    const borink_bytes no_bytes = {NULL, 0};
    const borink_bytes_mut no_room = {NULL, 0};
    const borink_request_head refused = borink_encode_get(
        &session, &shape, as_bytes("object.bin"), no_bytes, no_room, 1787400000);

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
    const borink_session session = opened();
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
    const borink_outcome outcome = borink_accept_get_head(
        &session, &shape, 200, headers, sizeof headers / sizeof headers[0]);

    CHECK(outcome.disposition == BORINK_DISPOSITION_BODY);
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
    const borink_session session = opened();
    const borink_get_shape shape = {4095, {BORINK_RANGE_FORM_WHOLE, 0, 0}, BORINK_CONDITION_NONE};
    uint8_t buffer[1024];
    const borink_bytes no_bytes = {NULL, 0};
    const borink_request_head refused =
        borink_encode_get(&session, &shape, as_bytes("object.bin"), no_bytes,
                          (borink_bytes_mut){buffer, sizeof buffer}, 1787400000);

    CHECK(refused.status.code == BORINK_ERROR_CODE_INVALID_PLAN);
    CHECK(refused.required == 0);
}

int main(void) {
    the_two_compilers_agree_on_every_struct();
    one_request_head_is_written_into_a_stack_buffer();
    a_buffer_that_is_too_small_reports_the_size_it_needs();
    one_response_head_is_read_from_two_buffers();
    an_unknown_number_is_refused();

    if (failures != 0) {
        fprintf(stderr, "%d check(s) failed\n", failures);
        return 1;
    }
    return 0;
}
