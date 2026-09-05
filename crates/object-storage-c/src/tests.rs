//! Tests for the whole boundary, driven through the entry points.

use crate::{entry::*, layout::*, outcome::*, plan::*, ptr::*, sentence::*, types::*};
use borink_object_storage_proto as proto;
use borink_object_storage_proto::{
    BodyWindow as CoreBodyWindow, DeleteHeadOutcome, Error, Failure as CoreFailure,
    FailureClass as CoreFailureClass, GetHeadOutcome, InvalidPlan, ObjectMeta as CoreObjectMeta,
    PutHeadOutcome, RequestedRange, ResponseFault, ServiceErrorKind,
};
use std::format;
use std::string::{String, ToString};
use std::vec;
use std::vec::Vec;

// Two buffers, so that nothing here depends on one contiguous head.
const VALUES: &[u8] = b"\"etag\"Wed, 26 Aug 2026 12:00:00 GMTversion-1gzip";
const IDENTIFIER: &[u8] = b"request-123";
const ENDPOINT: &[u8] = b"https://account.blob.core.windows.net";
const CONTAINER: &[u8] = b"container";
const TOKEN: &[u8] = b"token";

fn unknown() -> Status {
    status_of(&UNKNOWN)
}

fn kind_of(kind: u16) -> Option<ServiceErrorKind> {
    ServiceErrorKind::from_discriminant(kind)
}

fn class_of(class: u16) -> Option<CoreFailureClass> {
    CoreFailureClass::from_discriminant(class)
}

fn e_tag() -> &'static [u8] {
    &VALUES[..6]
}

fn lent(value: &[u8]) -> Bytes {
    Bytes {
        ptr: value.as_ptr(),
        len: value.len(),
    }
}

fn writable(value: &mut [u8]) -> BytesMut {
    BytesMut {
        ptr: value.as_mut_ptr(),
        len: value.len(),
    }
}

fn opened(endpoint: &[u8], container: &[u8], token: &[u8]) -> Session {
    Session {
        endpoint: lent(endpoint),
        container: lent(container),
        token: lent(token),
    }
}

fn session() -> Session {
    opened(ENDPOINT, CONTAINER, TOKEN)
}

fn whole() -> Range {
    Range {
        form: RangeForm::Whole as u16,
        start: 0,
        end: 0,
    }
}

fn read_shape() -> GetShape {
    GetShape {
        kind: GetKind::Bytes as u16,
        range: whole(),
        condition: Condition::None as u16,
    }
}

fn write_shape() -> PutShape {
    PutShape {
        condition: Condition::None as u16,
    }
}

fn list_shape_of(max_results: Option<u32>) -> ListShape {
    ListShape {
        delimited: false,
        max_results: max_results.map_or_else(Default::default, |value| MaybeU32 {
            present: true,
            value,
        }),
    }
}

// One page, as Azure writes it: `count` objects, then the marker that names
// the page after it.
fn page(count: usize, next_marker: &str) -> Vec<u8> {
    let mut body = String::from("<EnumerationResults><Blobs>");
    for index in 0..count {
        body.push_str(&format!(
            "<Blob><Name>key-{index}</Name><Properties>\
             <Last-Modified>Wed, 26 Aug 2026 12:00:00 GMT</Last-Modified>\
             <Etag>0x{index}</Etag><Content-Length>{index}</Content-Length>\
             </Properties></Blob>"
        ));
    }
    body.push_str(&format!(
        "</Blobs><NextMarker>{next_marker}</NextMarker></EnumerationResults>"
    ));
    body.into_bytes()
}

// The entries of a fill, as the keys they name.
fn keys(entries: &[ListEntry], fill: &Fill) -> Vec<String> {
    entries[..fill.filled]
        .iter()
        .map(|entry| {
            // SAFETY: every caller passes entries that point into a body of
            // the test that is still live.
            let key = unsafe { slice(entry.key) };
            String::from_utf8(key.to_vec()).unwrap()
        })
        .collect()
}

fn header(name: &'static str, value: &'static [u8]) -> HeaderRef {
    HeaderRef {
        name: lent(name.as_bytes()),
        value: lent(value),
    }
}

fn text(outcome: &Outcome) -> String {
    let mut into = [0; 256];
    // SAFETY: `outcome` and `into` are both live, and nothing else reaches
    // the buffer while the call writes it.
    let length = unsafe { borink_describe(outcome, writable(&mut into)) };
    assert!(length <= into.len(), "{length}");
    String::from_utf8(into[..length].to_vec()).unwrap()
}

fn full_meta() -> CoreObjectMeta<'static> {
    CoreObjectMeta {
        size: Some(10),
        e_tag: Some(e_tag()),
        last_modified: Some(&VALUES[6..35]),
        version: Some(&VALUES[35..44]),
        content_encoding: Some(&VALUES[44..]),
    }
}

fn every_failure() -> Vec<CoreFailure<'static>> {
    let mut failures = Vec::new();
    for kind in [None, Some(ServiceErrorKind::NoSuchContainer)] {
        for id in [None, Some(IDENTIFIER)] {
            failures.push(CoreFailure {
                status: 503,
                class: CoreFailureClass::Server,
                kind,
                request_id: id,
            });
        }
    }
    failures
}

// The bytes of a value that a reading call borrowed.
fn borrowed(value: MaybeBytes) -> Option<&'static [u8]> {
    // SAFETY: every caller below passes a value that points into a `const`
    // of this module, which outlives the test.
    unsafe { maybe_slice(value) }
}

// Every value that the core crate returns has one twin, the twin carries
// every field of it, and every borrowed field points at the same bytes.
#[test]
fn every_read_outcome_crosses_whole() {
    let view = get_outcome(&GetHeadOutcome::Body {
        meta: full_meta(),
        body: CoreBodyWindow {
            object_offset: 2,
            expected_len: Some(4),
            object_size: Some(10),
        },
    });
    assert_eq!(view.kind, OutcomeKind::Body as u16);
    assert!(view.meta.size.present);
    assert_eq!(view.meta.size.value, 10);
    assert_eq!(view.meta.e_tag.bytes.ptr, e_tag().as_ptr());
    assert_eq!(borrowed(view.meta.e_tag), Some(e_tag()));
    assert!(view.meta.last_modified.present);
    assert!(view.meta.version.present);
    assert!(view.meta.content_encoding.present);
    assert_eq!(view.body.object_offset, 2);
    assert_eq!(view.body.expected_len.value, 4);
    assert_eq!(view.body.object_size.value, 10);

    let empty = get_outcome(&GetHeadOutcome::Body {
        meta: CoreObjectMeta::default(),
        body: CoreBodyWindow {
            object_offset: 0,
            expected_len: None,
            object_size: None,
        },
    });
    assert!(!empty.meta.size.present);
    assert!(!empty.meta.e_tag.present);
    assert_eq!(empty.meta.e_tag.bytes.len, 0);
    assert!(!empty.body.expected_len.present);

    let complete = get_outcome(&GetHeadOutcome::Complete { meta: full_meta() });
    assert_eq!(complete.kind, OutcomeKind::Complete as u16);
    assert!(complete.meta.e_tag.present);

    for tag in [None, Some(e_tag())] {
        let view = get_outcome(&GetHeadOutcome::NotModified { e_tag: tag });
        assert_eq!(view.kind, OutcomeKind::NotModified as u16);
        assert_eq!(view.meta.e_tag.present, tag.is_some());
    }

    assert_eq!(
        get_outcome(&GetHeadOutcome::PreconditionFailed).kind,
        OutcomeKind::PreconditionFailed as u16
    );

    // A missing object carries the error it named, and no status and no
    // category that the head never stated.
    for kind in [None, Some(ServiceErrorKind::NoSuchContainer)] {
        let view = get_outcome(&GetHeadOutcome::NotFound { kind });
        assert_eq!(view.kind, OutcomeKind::NotFound as u16);
        assert_eq!(kind_of(view.failure.kind), kind);
        assert_eq!(view.failure.status, 0);
        assert_eq!(view.failure.class, 0);
    }

    for object_size in [None, Some(10)] {
        let view = get_outcome(&GetHeadOutcome::RangeNotSatisfiable { object_size });
        assert_eq!(view.kind, OutcomeKind::RangeNotSatisfiable as u16);
        assert_eq!(number(view.body.object_size), object_size);
        assert_eq!(
            text(&view),
            GetHeadOutcome::RangeNotSatisfiable { object_size }.to_string()
        );
    }

    for failure in every_failure() {
        for (outcome, expected) in [
            (
                GetHeadOutcome::NeedErrorBody(failure),
                OutcomeKind::NeedErrorBody,
            ),
            (
                GetHeadOutcome::ServiceFailure(failure),
                OutcomeKind::ServiceFailure,
            ),
        ] {
            let view = get_outcome(&outcome);
            assert_eq!(view.kind, expected as u16);
            assert_eq!(view.failure.status, failure.status);
            assert_eq!(class_of(view.failure.class), Some(failure.class));
            assert_eq!(kind_of(view.failure.kind), failure.kind);
            assert_eq!(borrowed(view.failure.request_id), failure.request_id);
            assert_eq!(text(&view), outcome.to_string());
        }
    }
}

#[test]
fn every_write_and_removal_outcome_crosses_whole() {
    let created = put_outcome(&PutHeadOutcome::Created { meta: full_meta() });
    assert_eq!(created.kind, OutcomeKind::Done as u16);
    assert!(created.meta.e_tag.present);

    assert_eq!(
        delete_outcome(&DeleteHeadOutcome::Accepted).kind,
        OutcomeKind::Accepted as u16
    );

    for kind in [None, Some(ServiceErrorKind::NoSuchContainer)] {
        assert_eq!(
            kind_of(put_outcome(&PutHeadOutcome::NotFound { kind }).failure.kind),
            kind
        );
        assert_eq!(
            kind_of(
                delete_outcome(&DeleteHeadOutcome::NotFound { kind })
                    .failure
                    .kind
            ),
            kind
        );
    }

    // A failure says the same thing whichever operation it answers, so the
    // twin needs no field naming the operation.
    for failure in every_failure() {
        for (put, delete) in [
            (
                PutHeadOutcome::NeedErrorBody(failure),
                DeleteHeadOutcome::NeedErrorBody(failure),
            ),
            (
                PutHeadOutcome::ServiceFailure(failure),
                DeleteHeadOutcome::ServiceFailure(failure),
            ),
        ] {
            assert_eq!(text(&put_outcome(&put)), put.to_string());
            assert_eq!(text(&delete_outcome(&delete)), delete.to_string());
        }
    }
}

// The sentence for a failure, a missing object and an unsatisfiable range
// is the core crate's own. A settled outcome gets a literal, which names no
// operation because one twin answers all three.
#[test]
fn every_outcome_kind_says_something_of_its_own() {
    for kind in [
        ServiceErrorKind::NotFound,
        ServiceErrorKind::NoSuchContainer,
    ] {
        let outcome = GetHeadOutcome::NotFound { kind: Some(kind) };
        assert_eq!(text(&get_outcome(&outcome)), outcome.to_string());
    }
    // A head that named neither leaves both open, and one twin answers for
    // three operations, so the sentence says both.
    assert_eq!(
        text(&get_outcome(&GetHeadOutcome::NotFound { kind: None })),
        "the object or its container does not exist"
    );

    let mut said = Vec::new();
    for kind in [
        OutcomeKind::Body,
        OutcomeKind::Complete,
        OutcomeKind::NotModified,
        OutcomeKind::PreconditionFailed,
        OutcomeKind::Done,
        OutcomeKind::Accepted,
        OutcomeKind::Page,
    ] {
        let sentence = settled_sentence(Some(kind));
        assert!(!sentence.is_empty());
        assert_eq!(text(&only(kind)), sentence);
        said.push(sentence);
    }
    said.sort_unstable();
    said.dedup();
    assert_eq!(said.len(), 7);

    // A kind from a later version of this crate names nothing here.
    let mut later = only(OutcomeKind::Body);
    later.kind = 4095;
    assert_eq!(text(&later), settled_sentence(None));
}

// Every enum crosses as its number, and comes back the same value. A number
// that names nothing is refused, never read as another value.
#[test]
fn every_enum_crosses_by_its_number_and_refuses_the_rest() {
    for repr in 1..=u16::MAX {
        if let Some(kind) = kind_of(repr) {
            assert_eq!(kind_of(kind_view(Some(kind))), Some(kind), "{kind:?}");
            assert_eq!(kind_view(Some(kind)), repr);
        }
        if let Some(class) = class_of(repr) {
            assert_eq!(class_of(class as u16), Some(class), "{class:?}");
        }
        assert_eq!(
            outcome_kind_of(repr).map(|kind| kind as u16),
            outcome_kind_of(repr).map(|_| repr)
        );
    }
    assert_eq!(kind_of(kind_view(None)), None);
    assert_eq!(kind_of(4095), None);
    assert_eq!(class_of(4095), None);
    assert!(outcome_kind_of(0).is_none());
    assert!(outcome_kind_of(14).is_none());

    // The plan side, which crosses inwards and must refuse.
    for (kind, expected) in [
        (GetKind::Bytes as u16, Some(proto::GetKind::Bytes)),
        (GetKind::Metadata as u16, Some(proto::GetKind::Metadata)),
        (0, None),
        (4095, None),
    ] {
        let shape = GetShape {
            kind,
            ..read_shape()
        };
        assert_eq!(get_shape(&shape).map(|shape| shape.kind).ok(), expected);
    }
    for (form, expected) in [
        (RangeForm::Whole as u16, Some(RequestedRange::Whole)),
        (
            RangeForm::Bounded as u16,
            Some(RequestedRange::Bounded { start: 2, end: 6 }),
        ),
        (RangeForm::Offset as u16, Some(RequestedRange::Offset(2))),
        (RangeForm::Suffix as u16, Some(RequestedRange::Suffix(2))),
        (0, None),
    ] {
        let shape = GetShape {
            range: Range {
                form,
                start: 2,
                end: 6,
            },
            ..read_shape()
        };
        assert_eq!(get_shape(&shape).map(|shape| shape.range).ok(), expected);
    }
    for (condition, expected) in [
        (Condition::None as u16, Some(proto::ConditionKind::None)),
        (
            Condition::IfMatch as u16,
            Some(proto::ConditionKind::IfMatch),
        ),
        (
            Condition::IfNoneMatch as u16,
            Some(proto::ConditionKind::IfNoneMatch),
        ),
        (0, None),
    ] {
        assert_eq!(condition_kind(condition).ok(), expected);
        assert_eq!(
            put_shape(&PutShape { condition })
                .map(|shape| shape.condition)
                .ok(),
            expected
        );
    }
    for (kind, expected) in [
        (DeleteKind::Object as u16, Some(proto::DeleteKind::Object)),
        (
            DeleteKind::ObjectAndSnapshots as u16,
            Some(proto::DeleteKind::ObjectAndSnapshots),
        ),
        (
            DeleteKind::SnapshotsOnly as u16,
            Some(proto::DeleteKind::SnapshotsOnly),
        ),
        (0, None),
    ] {
        let shape = DeleteShape {
            kind,
            condition: Condition::None as u16,
        };
        assert_eq!(delete_shape(&shape).map(|shape| shape.kind).ok(), expected);
    }
    for kind in [
        proto::EntryKind::Object,
        proto::EntryKind::Prefix,
        proto::EntryKind::Directory,
    ] {
        let entry = proto::ListEntry {
            kind,
            ..Default::default()
        };
        assert_eq!(
            proto::EntryKind::from_discriminant(entry_view(&entry).kind),
            Some(kind)
        );
    }
    // A listing plan carries no enum, and an absent count is not a zero one.
    for max_results in [None, Some(1000)] {
        let shape = ListShape {
            delimited: true,
            max_results: max_results.map_or_else(Default::default, |value| MaybeU32 {
                present: true,
                value,
            }),
        };
        assert_eq!(
            list_shape(&shape).map(|shape| shape.max_results).ok(),
            Some(max_results)
        );
    }
}

// A number that this crate does not define stops the call, and says so.
#[test]
fn an_unknown_number_is_refused_rather_than_read_as_another_value() {
    let session = session();
    let shape = GetShape {
        kind: 4095,
        ..read_shape()
    };
    let mut buf = vec![0; 512];
    // SAFETY: every pointer below addresses a live value of this test.
    let refused = unsafe {
        borink_encode_get(
            &session,
            &shape,
            lent(b"object.bin"),
            lent(b""),
            writable(&mut buf),
            1_787_400_000,
        )
    };
    assert_eq!(refused.status, unknown());
    assert_eq!(refused.status.code, ErrorCode::InvalidPlan as u16);
    assert_eq!(refused.status.detail, InvalidPlan::Unknown as u16);
    assert_eq!(refused.required, 0);

    // SAFETY: as above, with no headers.
    let outcome = unsafe { borink_accept_get_head(&session, &shape, 200, core::ptr::null(), 0) };
    assert_eq!(outcome.kind, OutcomeKind::Invalid as u16);
    assert_eq!(outcome.error, unknown());
    assert_eq!(
        text(&outcome),
        Error::InvalidPlan(InvalidPlan::Unknown).to_string()
    );
}

// A pointer that was never filled in is refused as an invalid plan, and
// never read.
#[test]
fn a_null_pointer_is_refused_rather_than_read() {
    let session = session();
    let shape = read_shape();
    let mut buf = vec![0; 512];

    // SAFETY: the null pointers are the case under test, and the rest
    // address live values.
    unsafe {
        assert_eq!(borink_validate(core::ptr::null()), unknown());
        assert_eq!(
            borink_encode_get(
                core::ptr::null(),
                &shape,
                lent(b"object.bin"),
                lent(b""),
                writable(&mut buf),
                0,
            )
            .status,
            unknown()
        );
        assert_eq!(
            borink_encode_get(
                &session,
                core::ptr::null(),
                lent(b"object.bin"),
                lent(b""),
                writable(&mut buf),
                0,
            )
            .status,
            unknown()
        );
        assert_eq!(
            borink_encode_put(
                &session,
                core::ptr::null(),
                lent(b"object.bin"),
                lent(b""),
                writable(&mut buf),
                0,
                0,
            )
            .status,
            unknown()
        );
        assert_eq!(
            borink_encode_delete(
                &session,
                core::ptr::null(),
                lent(b"object.bin"),
                lent(b""),
                writable(&mut buf),
                0,
            )
            .status,
            unknown()
        );
        assert_eq!(
            borink_accept_get_head(&session, core::ptr::null(), 200, core::ptr::null(), 0).error,
            unknown()
        );
        assert_eq!(
            borink_accept_put_head(&session, core::ptr::null(), 201, core::ptr::null(), 0).error,
            unknown()
        );
        assert_eq!(
            borink_accept_delete_head(&session, core::ptr::null(), 202, core::ptr::null(), 0).error,
            unknown()
        );
        assert_eq!(
            borink_finish_get_error_body(&session, core::ptr::null(), lent(b"")).error,
            unknown()
        );
        assert_eq!(
            borink_finish_put_error_body(&session, core::ptr::null(), lent(b"")).error,
            unknown()
        );
        assert_eq!(
            borink_finish_delete_error_body(&session, core::ptr::null(), lent(b"")).error,
            unknown()
        );
        assert_eq!(
            borink_encode_list(
                &session,
                core::ptr::null(),
                lent(b"prefix/"),
                lent(b""),
                writable(&mut buf),
                0,
            )
            .status,
            unknown()
        );
        assert_eq!(
            borink_accept_list_head(core::ptr::null(), 200, core::ptr::null(), 0).error,
            unknown()
        );
        assert_eq!(
            borink_finish_list_error_body(&session, core::ptr::null(), lent(b"")).error,
            unknown()
        );
        assert_eq!(
            borink_fill_listing(
                core::ptr::null(),
                writable(&mut buf),
                core::ptr::null_mut(),
                0
            )
            .status,
            unknown()
        );
        // A sentence for nothing is no sentence, not a guess at one.
        assert_eq!(borink_describe(core::ptr::null(), writable(&mut buf)), 0);
    }
}

// Every error of the core crate crosses as two numbers and comes back as
// the same sentence.
#[test]
fn every_error_crosses_as_a_status() {
    let mut checked = 0;
    for code in 1..=u16::MAX {
        let Some(code) = proto::ErrorCode::from_discriminant(code) else {
            continue;
        };
        for detail in 0..=u16::MAX {
            let Some(error) = Error::from_parts(code, detail) else {
                continue;
            };
            let status = status_of(&error);
            assert_eq!(status.code, code as u16);
            assert_eq!(status.detail, detail);
            let mut into = [0; 256];
            // SAFETY: `into` is live and reached through nothing else.
            let length = unsafe { borink_describe_status(status, writable(&mut into)) };
            assert_eq!(
                String::from_utf8(into[..length].to_vec()).unwrap(),
                error.to_string(),
                "{error:?}"
            );
            checked += 1;
        }
    }
    // Every variant of the two inner enums, and the three that carry no
    // inner value.
    assert_eq!(checked, 3 + 10 + 4);
    assert_eq!(
        ResponseFault::from_discriminant(3).map(Error::Response),
        Error::from_parts(proto::ErrorCode::Response, 3)
    );
}

// A capacity error carries sizes rather than a discriminant, so it crosses
// as a code and the `required` field of the request head.
#[test]
fn a_buffer_that_is_too_small_reports_the_size_it_needs() {
    let session = session();
    let shape = read_shape();
    // SAFETY: the empty buffer is the case under test; the rest are live.
    let refused = unsafe {
        borink_encode_get(
            &session,
            &shape,
            lent(b"object.bin"),
            lent(b""),
            writable(&mut []),
            1_787_400_000,
        )
    };
    assert_eq!(refused.status.code, ErrorCode::Capacity as u16);
    assert!(refused.required > 0);

    let mut buf = vec![0; refused.required];
    // SAFETY: every pointer addresses a live value of this test.
    let written = unsafe {
        borink_encode_get(
            &session,
            &shape,
            lent(b"object.bin"),
            lent(b""),
            writable(&mut buf),
            1_787_400_000,
        )
    };
    assert_eq!(written.status.code, 0);
    assert_eq!(written.required, refused.required);
    assert_eq!(written.method, Method::Get as u16);
    assert_eq!(written.header_count, 3);
    let url = &buf[written.url.start..written.url.start + written.url.len];
    assert_eq!(
        core::str::from_utf8(url).unwrap(),
        "https://account.blob.core.windows.net/container/object.bin"
    );
    for index in 0..written.header_count {
        let header = written.headers[index];
        assert!(header.name.start + header.name.len <= buf.len());
        assert!(header.value.start + header.value.len <= buf.len());
    }
}

// A ranged, conditional read reaches the core crate from a stored shape and
// the bytes that go with it.
#[test]
fn a_stored_shape_carries_the_whole_plan() {
    let session = session();
    let shape = GetShape {
        kind: GetKind::Bytes as u16,
        range: Range {
            form: RangeForm::Bounded as u16,
            start: 2,
            end: 6,
        },
        condition: Condition::IfNoneMatch as u16,
    };
    let mut buf = vec![0; 512];
    // SAFETY: every pointer addresses a live value of this test.
    let head = unsafe {
        borink_encode_get(
            &session,
            &shape,
            lent(b"object.bin"),
            lent(b"\"etag\""),
            writable(&mut buf),
            1_787_400_000,
        )
    };
    assert_eq!(head.status.code, 0);
    let named = |name: &str| {
        (0..head.header_count).find_map(|index| {
            let header = head.headers[index];
            let read =
                |span: Span| core::str::from_utf8(&buf[span.start..span.start + span.len]).unwrap();
            (read(header.name) == name).then(|| read(header.value).to_string())
        })
    };
    assert_eq!(named("range").as_deref(), Some("bytes=2-5"));
    assert_eq!(named("if-none-match").as_deref(), Some("\"etag\""));
}

// The head reaches this crate as slices, from wherever the host keeps them.
// Nothing here is one buffer, and the outcome points back at each.
#[test]
fn a_head_crosses_as_slices_of_whatever_holds_it() {
    let session = session();
    let headers = [
        header("ETag", e_tag()),
        header("Content-Length", b"10"),
        header("x-ms-request-id", IDENTIFIER),
        // A name that is not text is none of the ones the core crate reads,
        // so it is skipped rather than refused.
        HeaderRef {
            name: lent(b"\xff"),
            value: lent(b"value"),
        },
    ];
    // SAFETY: every pointer addresses a live value of this test.
    let outcome = unsafe {
        borink_accept_get_head(
            &session,
            &read_shape(),
            200,
            headers.as_ptr(),
            headers.len(),
        )
    };
    assert_eq!(outcome.kind, OutcomeKind::Body as u16);
    assert_eq!(outcome.meta.e_tag.bytes.ptr, e_tag().as_ptr());
    assert!(outcome.body.expected_len.present);
    assert_eq!(outcome.body.expected_len.value, 10);
}

// The head asked for the error body, and the body names the error. The
// request id crosses as bytes the host still owns, both ways.
#[test]
fn the_error_body_finishes_what_the_head_left_open() {
    let session = session();
    let headers = [header("x-ms-request-id", IDENTIFIER)];
    // SAFETY: every pointer addresses a live value of this test.
    let outcome = unsafe {
        borink_accept_put_head(
            &session,
            &write_shape(),
            409,
            headers.as_ptr(),
            headers.len(),
        )
    };
    assert_eq!(outcome.kind, OutcomeKind::NeedErrorBody as u16);
    assert_eq!(outcome.failure.request_id.bytes.ptr, IDENTIFIER.as_ptr());

    // SAFETY: as above, and the body outlives the outcome it names.
    let finished = unsafe {
        borink_finish_put_error_body(
            &session,
            &outcome.failure,
            lent(b"<Error><Code>BlobAlreadyExists</Code></Error>"),
        )
    };
    assert_eq!(finished.kind, OutcomeKind::ServiceFailure as u16);
    assert_eq!(
        kind_of(finished.failure.kind),
        Some(ServiceErrorKind::AlreadyExists)
    );
    assert!(text(&finished).contains("already exists"));
    assert!(text(&finished).contains("request-123"));

    // A body that never arrived leaves the outcome final and unnamed.
    // SAFETY: as above.
    let unnamed = unsafe { borink_finish_put_error_body(&session, &outcome.failure, lent(b"")) };
    assert_eq!(unnamed.kind, OutcomeKind::ServiceFailure as u16);
    assert_eq!(kind_of(unnamed.failure.kind), None);
}

// A head that does not answer the plan is a status, not a sentence.
#[test]
fn an_invalid_head_carries_the_error_of_the_core_crate() {
    let session = session();
    // SAFETY: every pointer addresses a live value of this test.
    let outcome =
        unsafe { borink_accept_put_head(&session, &write_shape(), 412, core::ptr::null(), 0) };
    assert_eq!(outcome.kind, OutcomeKind::Invalid as u16);
    assert_eq!(outcome.error.code, ErrorCode::Response as u16);
    assert_eq!(outcome.error.detail, ResponseFault::Status as u16);
    assert_eq!(
        text(&outcome),
        Error::Response(ResponseFault::Status).to_string()
    );
}

#[test]
fn a_session_that_cannot_be_used_says_which_value_is_wrong() {
    for (endpoint, container, token, expected) in [
        (
            b"account.example".as_slice(),
            b"container".as_slice(),
            b"token".as_slice(),
            ErrorCode::InvalidEndpoint,
        ),
        (
            b"https://account.example",
            b"",
            b"token",
            ErrorCode::InvalidContainer,
        ),
        (
            b"https://account.example",
            b"container",
            b"",
            ErrorCode::InvalidToken,
        ),
        (b"\xff", b"container", b"token", ErrorCode::InvalidEndpoint),
    ] {
        let session = opened(endpoint, container, token);
        // SAFETY: every pointer addresses a live value of this test.
        let status = unsafe { borink_validate(&session) };
        assert_eq!(status.code, expected as u16);
        // A session that cannot build a request cannot read the answer to
        // one, and says the same thing when asked to.
        // SAFETY: as above.
        let refused = unsafe {
            borink_encode_get(
                &session,
                &read_shape(),
                lent(b"key"),
                lent(b""),
                writable(&mut []),
                0,
            )
        };
        assert_eq!(refused.status, status);
        // SAFETY: as above.
        let outcome =
            unsafe { borink_accept_get_head(&session, &read_shape(), 200, core::ptr::null(), 0) };
        assert_eq!(outcome.kind, OutcomeKind::Invalid as u16);
        assert_eq!(outcome.error, status);
    }
    // SAFETY: the session addresses `const` bytes of this module.
    assert_eq!(unsafe { borink_validate(&session()) }.code, 0);
}

// A sentence longer than the buffer is counted, not cut off silently.
#[test]
fn a_short_buffer_still_learns_the_length_of_the_sentence() {
    let outcome = get_outcome(&GetHeadOutcome::ServiceFailure(CoreFailure {
        status: 503,
        class: CoreFailureClass::Server,
        kind: None,
        request_id: Some(IDENTIFIER),
    }));
    let mut small = [0; 4];
    // SAFETY: both values are live and reached through nothing else.
    let length = unsafe { borink_describe(&outcome, writable(&mut small)) };
    assert!(length > small.len());
    let mut whole = vec![0; length];
    // SAFETY: as above.
    assert_eq!(
        unsafe { borink_describe(&outcome, writable(&mut whole)) },
        length
    );
}

// The layout check reports what it is given, so a C program that disagrees
// learns how many facts disagree rather than reading the wrong offset.
#[test]
fn the_layout_check_answers_for_the_layout_it_is_given() {
    let ours = layout();
    // SAFETY: `ours` is live, and null is the case the second call tests.
    unsafe {
        assert_eq!(borink_layout_disagrees(&ours), 0);
        let mut wrong = ours;
        wrong.sizeof_outcome += 1;
        wrong.offsetof_outcome_error += 1;
        assert_eq!(borink_layout_disagrees(&wrong), 2);
        assert_eq!(
            borink_layout_disagrees(core::ptr::null()),
            size_of::<Layout>() / size_of::<usize>()
        );
    }
}

// A listing asks for one page, and the head says that the page follows.
#[test]
fn a_listing_plan_reaches_the_wire_and_its_head_announces_the_page() {
    let session = session();
    let shape = ListShape {
        delimited: true,
        max_results: MaybeU32 {
            present: true,
            value: 2,
        },
    };
    let mut buf = vec![0; 512];
    // SAFETY: every pointer below addresses a live value of this test.
    let head = unsafe {
        borink_encode_list(
            &session,
            &shape,
            lent(b"directory/"),
            lent(b"marker-1"),
            writable(&mut buf),
            1_787_400_000,
        )
    };
    assert_eq!(head.status, Status::default());
    assert_eq!(head.method, Method::Get as u16);
    let url =
        String::from_utf8(buf[head.url.start..head.url.start + head.url.len].to_vec()).unwrap();
    assert_eq!(
        url,
        "https://account.blob.core.windows.net/container\
         ?restype=container&comp=list&prefix=directory%2F&delimiter=%2F\
         &marker=marker-1&maxresults=2"
    );

    let headers = [header("Content-Length", b"120")];
    // SAFETY: the headers are live, and the call takes no shape.
    let outcome = unsafe { borink_accept_list_head(&session, 200, headers.as_ptr(), 1) };
    assert_eq!(outcome.kind, OutcomeKind::Page as u16);
    assert!(outcome.body.expected_len.present);
    assert_eq!(outcome.body.expected_len.value, 120);
    assert_eq!(text(&outcome), "the page follows in the response body");
}

// The entries of a page point into the body that the fill read, and the
// values of each one cross whole.
#[test]
fn a_page_crosses_as_entries_that_point_into_the_body() {
    let session = session();
    let mut body = page(2, "next");
    let mut entries = [ListEntry::default(); 4];
    // SAFETY: the body and the array are live for the whole call, and nothing
    // else reaches them.
    let fill =
        unsafe { borink_fill_listing(&session, writable(&mut body), entries.as_mut_ptr(), 4) };

    assert_eq!(fill.status, Status::default());
    assert_eq!(fill.filled, 2);
    assert_eq!(keys(&entries, &fill), ["key-0", "key-1"]);
    assert_eq!(entries[0].kind, EntryKind::Object as u16);
    assert!(entries[0].size.present);
    assert_eq!(entries[1].size.value, 1);
    assert_eq!(borrowed(entries[1].e_tag), Some(b"0x1".as_slice()));
    assert_eq!(
        borrowed(entries[0].last_modified),
        Some(b"Wed, 26 Aug 2026 12:00:00 GMT".as_slice())
    );
    // The key is bytes of the body, not a copy of them.
    let start = entries[0].key.ptr as usize - body.as_ptr() as usize;
    assert_eq!(&body[start..start + entries[0].key.len], b"key-0");
    // SAFETY: the marker points into the body, which is still live.
    assert_eq!(
        unsafe { maybe_slice(fill.next_marker) },
        Some(b"next".as_slice())
    );
    // The entries after the ones it filled are untouched.
    assert_eq!(entries[2].key.len, 0);
}

// The array must hold the whole page. One that does not is refused, and
// `required` says how many entries the page holds. No entry of the array is
// reported, and the body cannot be read again.
#[test]
fn an_array_smaller_than_the_page_is_refused_with_the_count_it_holds() {
    let session = session();
    let mut body = page(40, "");
    let mut entries = [ListEntry::default(); 25];
    // SAFETY: the body and the array are live, and nothing else reaches them.
    let fill =
        unsafe { borink_fill_listing(&session, writable(&mut body), entries.as_mut_ptr(), 25) };
    assert_eq!(fill.status.code, ErrorCode::Capacity as u16);
    assert_eq!(fill.required, 40);
    assert_eq!(fill.filled, 0);
    assert!(!fill.next_marker.present);
}

// An array with no room is refused the same way, unless the page is empty.
#[test]
fn an_array_with_no_room_is_refused_unless_the_page_is_empty() {
    let session = session();
    let mut body = page(1, "next");
    // SAFETY: the body is live, and no array is passed.
    let fill =
        unsafe { borink_fill_listing(&session, writable(&mut body), core::ptr::null_mut(), 0) };
    assert_eq!(fill.status.code, ErrorCode::Capacity as u16);
    assert_eq!(fill.required, 1);

    let mut body = page(0, "next");
    // SAFETY: as above.
    let fill =
        unsafe { borink_fill_listing(&session, writable(&mut body), core::ptr::null_mut(), 0) };
    assert_eq!(fill.status, Status::default());
    assert_eq!(fill.filled, 0);
    assert!(fill.next_marker.present);
}

// A body that is not a page reports the fault of the core crate, and no
// entry of the array is reported.
#[test]
fn a_body_that_is_not_a_page_is_refused() {
    let session = session();
    let mut body = Vec::from(b"<Error><Code>ServerBusy</Code></Error>".as_slice());
    let mut entries = [ListEntry::default(); 2];
    // SAFETY: the body and the array are live, and nothing else reaches them.
    let fill =
        unsafe { borink_fill_listing(&session, writable(&mut body), entries.as_mut_ptr(), 2) };

    assert_eq!(fill.status.code, ErrorCode::Response as u16);
    assert_eq!(fill.status.detail, ResponseFault::Body as u16);
    assert_eq!(fill.filled, 0);
    let mut into = [0; 128];
    // SAFETY: the buffer is live and nothing else reaches it.
    let length = unsafe { borink_describe_status(fill.status, writable(&mut into)) };
    assert_eq!(
        String::from_utf8(into[..length].to_vec()).unwrap(),
        Error::Response(ResponseFault::Body).to_string()
    );
}

// A listing whose head named no error is finished by the body, exactly as a
// read is.
#[test]
fn a_listing_failure_is_finished_by_the_body() {
    let session = session();
    // SAFETY: the header bytes are static, and the session is live.
    let outcome = unsafe {
        let headers = [header("x-ms-request-id", IDENTIFIER)];
        borink_accept_list_head(&session, 404, headers.as_ptr(), 1)
    };
    assert_eq!(outcome.kind, OutcomeKind::NeedErrorBody as u16);

    let body = b"<Error><Code>ContainerNotFound</Code></Error>";
    // SAFETY: the failure and the body are live for the call.
    let finished = unsafe { borink_finish_list_error_body(&session, &outcome.failure, lent(body)) };
    assert_eq!(finished.kind, OutcomeKind::NotFound as u16);
    assert_eq!(
        kind_of(finished.failure.kind),
        Some(ServiceErrorKind::NoSuchContainer)
    );
}

// A listing plan is checked before any byte is written, and the reason names
// the field that was wrong.
#[test]
fn a_listing_plan_that_azure_would_refuse_is_refused_here() {
    let session = session();
    let mut buf = vec![0; 512];
    let shape = list_shape_of(Some(0));
    // SAFETY: every pointer below addresses a live value of this test.
    let refused = unsafe {
        borink_encode_list(
            &session,
            &shape,
            lent(b"prefix/"),
            lent(b""),
            writable(&mut buf),
            0,
        )
    };
    assert_eq!(refused.status.code, ErrorCode::InvalidPlan as u16);
    assert_eq!(refused.status.detail, InvalidPlan::MaxResults as u16);

    // An empty buffer states the size that the head needs, as every encode
    // call does.
    let shape = list_shape_of(None);
    // SAFETY: as above, with no buffer at all.
    let refused = unsafe {
        borink_encode_list(
            &session,
            &shape,
            lent(b"prefix/"),
            lent(b""),
            BytesMut {
                ptr: core::ptr::null_mut(),
                len: 0,
            },
            0,
        )
    };
    assert_eq!(refused.status.code, ErrorCode::Capacity as u16);
    assert!(refused.required > 0);
}

// A listing writes an entity tag without the quotes that a condition takes,
// and a date that only a parser reads. Both cross as the helpers that read
// them, so a C program writes neither itself.
#[test]
fn the_values_a_listing_lends_are_read_by_the_calls_beside_it() {
    let mut room = [0; 32];
    // SAFETY: both values are live for the call, and nothing else reaches the
    // buffer while it is written.
    let quoted = unsafe { borink_quoted_etag(lent(b"0x8DF0"), writable(&mut room)) };
    assert_eq!(borrowed(quoted), Some(b"\"0x8DF0\"".as_slice()));

    // A tag that already carries them crosses unchanged.
    // SAFETY: as above.
    let quoted = unsafe { borink_quoted_etag(lent(b"\"0x8DF0\""), writable(&mut room)) };
    assert_eq!(borrowed(quoted), Some(b"\"0x8DF0\"".as_slice()));

    // A buffer that cannot hold the quotes writes nothing.
    let mut short = [0; 6];
    // SAFETY: as above.
    let refused = unsafe { borink_quoted_etag(lent(b"0x8DF0"), writable(&mut short)) };
    assert!(!refused.present);

    // SAFETY: the date is live for the call.
    let read = unsafe { borink_http_date_ms(lent(b"Wed, 26 Aug 2026 12:00:00 GMT")) };
    assert!(read.present);
    assert_eq!(read.value, 1_787_745_600_000);

    // A value that is not a date, and one the head did not carry.
    // SAFETY: as above.
    let (bad, none) = unsafe {
        (
            borink_http_date_ms(lent(b"yesterday")),
            borink_http_date_ms(lent(b"")),
        )
    };
    assert!(!bad.present);
    assert!(!none.present);
}

// A property that the entry does not carry is read out of the entry's own
// bytes, one at a time or in one walk.
#[test]
fn a_property_of_an_entry_crosses_by_name_and_by_walk() {
    let session = session();
    let mut body = Vec::from(
        b"<EnumerationResults><Blobs><Blob><Name>a.txt</Name>\
          <VersionId>2026-09-01T19:08:11Z</VersionId><Properties>\
          <Content-Length>4</Content-Length><Content-Encoding />\
          <AccessTier>Hot</AccessTier></Properties>\
          <Metadata><colour>a&amp;b</colour></Metadata></Blob>\
          </Blobs><NextMarker /></EnumerationResults>"
            .as_slice(),
    );
    let mut entries = [ListEntry::default(); 1];
    // SAFETY: the body and the array are live, and nothing else reaches them.
    let fill =
        unsafe { borink_fill_listing(&session, writable(&mut body), entries.as_mut_ptr(), 1) };
    assert_eq!(fill.filled, 1);
    let entry = entries[0];
    assert_ne!(entry.raw.len, 0);

    // SAFETY: the entry points into the body, which is still live.
    let (tier, version, encoding, absent) = unsafe {
        (
            borink_entry_property(&entry, lent(b"AccessTier")),
            borink_entry_property(&entry, lent(b"VersionId")),
            borink_entry_property(&entry, lent(b"Content-Encoding")),
            borink_entry_property(&entry, lent(b"Snapshot")),
        )
    };
    assert_eq!(borrowed(tier), Some(b"Hot".as_slice()));
    assert_eq!(borrowed(version), Some(b"2026-09-01T19:08:11Z".as_slice()));
    // An element that carries nothing is present and empty; one the entry
    // never wrote is absent.
    assert_eq!(borrowed(encoding), Some(b"".as_slice()));
    assert!(!absent.present);

    // The walk reports each of them once, and the properties element itself
    // is never one of them.
    // SAFETY: as above, and the walk is a value of this test.
    let names = unsafe {
        let mut walk = borink_entry_properties(&entry);
        let mut names = Vec::new();
        loop {
            let found = borink_next_property(&mut walk);
            if !found.present {
                break;
            }
            names.push(String::from_utf8(slice(found.name).to_vec()).unwrap());
        }
        // A walk that ended stays ended.
        assert!(!borink_next_property(&mut walk).present);
        names
    };
    assert_eq!(
        names,
        [
            "Name",
            "VersionId",
            "Content-Length",
            "Content-Encoding",
            "AccessTier",
            "Metadata"
        ]
    );

    // A null entry reads nothing and walks nothing.
    // SAFETY: the null pointers are the case under test.
    unsafe {
        assert!(!borink_entry_property(core::ptr::null(), lent(b"AccessTier")).present);
        assert_eq!(borink_entry_properties(core::ptr::null()).remaining.len, 0);
        assert!(!borink_next_property(core::ptr::null_mut()).present);
    }
}

// A value that carries a reference is decoded into the caller's own buffer.
#[test]
fn a_listed_value_is_decoded_into_the_caller_s_buffer() {
    // One buffer per call: what a call returns is the buffer it wrote, so the
    // next call on the same buffer takes those bytes back.
    let (mut room, mut cramped, mut third) = ([0; 16], [0; 6], [0; 16]);
    // SAFETY: every value is live, and nothing else reaches the buffers.
    let (decoded, refused, unknown) = unsafe {
        (
            borink_decode_into(lent(b"a&amp;b"), writable(&mut room)),
            borink_decode_into(lent(b"a&amp;b"), writable(&mut cramped)),
            borink_decode_into(lent(b"a&nbsp;b"), writable(&mut third)),
        )
    };
    assert_eq!(borrowed(decoded), Some(b"a&b".as_slice()));
    // A buffer shorter than the value, and a reference that no listing
    // declares.
    assert!(!refused.present);
    assert!(!unknown.present);
}

/// The two lists of properties are built from one table, so the numbers and
/// the names agree, and a number that names none is refused everywhere.
#[test]
fn the_properties_cross_by_number_and_name() {
    for (number, property) in proto::BlobProperty::ALL.iter().enumerate() {
        let number = u16::try_from(number).unwrap();
        // SAFETY: the name is a static of this crate.
        let name = unsafe { slice(borink_property_name(number)) };
        assert_eq!(name, property.name().as_bytes());
        let set = borink_property_set_with(PropertySet::default(), number);
        assert_eq!(borink_property_set_len(set), 1);
        assert_eq!(borink_property_slot(set, number), 0);
    }
    let none = u16::try_from(proto::BlobProperty::ALL.len()).unwrap();
    assert_eq!(
        borink_property_set_with(PropertySet::default(), none),
        PropertySet::default()
    );
    // SAFETY: as above.
    assert_eq!(unsafe { slice(borink_property_name(none)) }, b"");
    assert_eq!(borink_property_slot(PropertySet::default(), none), 0);
}

/// The values a program asks for are written into its rows as the page is
/// read, and an array with fewer rows than entries is the capacity.
#[test]
fn the_values_a_program_asks_for_are_written_into_its_rows() {
    let session = session();
    let page = b"<EnumerationResults><Blobs>\
          <Blob><Name>a.txt</Name><Properties><Content-Length>4</Content-Length>\
          <AccessTier>Hot</AccessTier><Content-Encoding /></Properties></Blob>\
          <Blob><Name>b.txt</Name><Properties><Content-Length>0</Content-Length>\
          </Properties></Blob>\
          </Blobs><NextMarker /></EnumerationResults>";
    let mut wanted = PropertySet::default();
    wanted = borink_property_set_with(wanted, BlobProperty::ContentEncoding as u16);
    wanted = borink_property_set_with(wanted, BlobProperty::AccessTier as u16);
    let tier = borink_property_slot(wanted, BlobProperty::AccessTier as u16);
    let encoding = borink_property_slot(wanted, BlobProperty::ContentEncoding as u16);
    assert_eq!((tier, encoding), (0, 1));

    let mut body = Vec::from(page.as_slice());
    let mut entries = [ListEntry::default(); 2];
    let mut values = [MaybeBytes::default(); 4];
    // SAFETY: the body and both arrays are live, and nothing else reaches
    // them.
    let fill = unsafe {
        borink_fill_listing_with(
            &session,
            writable(&mut body),
            entries.as_mut_ptr(),
            2,
            wanted,
            values.as_mut_ptr(),
            4,
        )
    };
    assert_eq!(fill.status.code, 0);
    assert_eq!(fill.filled, 2);
    assert_eq!(borrowed(values[tier]), Some(b"Hot".as_slice()));
    assert_eq!(borrowed(values[encoding]), Some(b"".as_slice()));
    assert!(!values[2 + tier].present);
    assert!(!values[2 + encoding].present);

    // Rows for one entry only: that is the capacity, whatever `into` holds.
    let mut body = Vec::from(page.as_slice());
    let mut entries = [ListEntry::default(); 2];
    let mut values = [MaybeBytes::default(); 2];
    // SAFETY: as above.
    let fill = unsafe {
        borink_fill_listing_with(
            &session,
            writable(&mut body),
            entries.as_mut_ptr(),
            2,
            wanted,
            values.as_mut_ptr(),
            2,
        )
    };
    assert_eq!(fill.status.code, ErrorCode::Capacity as u16);
    assert_eq!(fill.required, 2);
}
