//! Tests for the whole boundary, driven through the entry points.

use crate::{convert::*, entry::*, layout::*, ptr::*, sentence::*, types::*};
use borink_object_storage_proto as proto;
use borink_object_storage_proto::{
    BodyWindow as CoreBodyWindow, DeleteHeadOutcome, Error, Failure as CoreFailure,
    FailureClass as CoreFailureClass, GetHeadOutcome, InvalidPlan, ObjectMeta as CoreObjectMeta,
    PutHeadOutcome, RequestedRange, ResponseFault, ServiceErrorKind,
};
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
    ] {
        let sentence = settled_sentence(Some(kind));
        assert!(!sentence.is_empty());
        assert_eq!(text(&only(kind)), sentence);
        said.push(sentence);
    }
    said.sort_unstable();
    said.dedup();
    assert_eq!(said.len(), 6);

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
    assert!(outcome_kind_of(13).is_none());

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
    assert_eq!(checked, 3 + 7 + 3);
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
