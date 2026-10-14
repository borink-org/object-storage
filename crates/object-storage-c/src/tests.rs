//! Tests for the whole boundary, driven through the entry points.

use crate::{entry::*, inputs::*, layout::*, outcome::*, ptr::*, sentence::*, types::*};
use borink_object_storage_proto as proto;
use borink_object_storage_proto::{
    BodyWindow as CoreBodyWindow, CommitHeadOutcome, Error, Failure as CoreFailure,
    FailureClass as CoreFailureClass, GetHeadOutcome, InvalidRequest, ObjectMeta as CoreObjectMeta,
    PutHeadOutcome, RequestedRange, ResponseFault, ServiceErrorKind, StageHeadOutcome, azure,
};
use std::format;
use std::string::{String, ToString};
use std::vec;
use std::vec::Vec;

// Two buffers, so that nothing here depends on one contiguous head.
const VALUES: &[u8] = b"\"etag\"Wed, 26 Aug 2026 12:00:00 GMTversion-1gzip";
const IDENTIFIER: &[u8] = b"request-123";
const ENDPOINT: &str = "https://account.blob.core.windows.net";
const CONTAINER: &str = "container";
const TOKEN: &str = "token";
const NOW: u64 = 1_787_400_000;
const KEY: &str = "dir/object one.bin";
const MD5: &str = "1B2M2Y8AsgTpgAmY7PhCfg==";
const BLOCK_IDS: [&str; 2] = ["YmxvY2stMDAwMDA=", "YmxvY2stMDAwMDE="];

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

fn located(endpoint: &[u8], name: &[u8]) -> Container {
    Container {
        endpoint: lent(endpoint),
        name: lent(name),
        allow_http: false,
        object: Bytes::default(),
    }
}

fn container() -> Container {
    located(ENDPOINT.as_bytes(), CONTAINER.as_bytes())
}

fn token() -> Bytes {
    lent(TOKEN.as_bytes())
}

// The instant every request here is written at.
fn now() -> Timestamps {
    borink_timestamps_from_unix(NOW)
}

fn proto_container() -> proto::azure::Container<'static> {
    proto::azure::Container::new(ENDPOINT, CONTAINER).unwrap()
}

fn whole() -> Range {
    Range {
        form: RangeForm::Whole as u16,
        start: 0,
        end: 0,
    }
}

fn read_options() -> GetOptions {
    GetOptions {
        kind: GetKind::Bytes as u16,
        ranging: Ranging::Whole as u16,
        condition: Condition::None as u16,
        revision: RevisionKind::Current as u16,
    }
}

fn read_item(key: &[u8]) -> GetItem {
    GetItem {
        key: lent(key),
        range: whole(),
        condition_value: ConditionValue::default(),
        revision: Bytes::default(),
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

fn tag(value: &[u8]) -> ConditionValue {
    ConditionValue {
        form: ConditionForm::ETag as u16,
        e_tag: lent(value),
        seconds: 0,
    }
}

// A template that a prepare call wrote, and its bytes: as many as the call
// said it takes.
struct Kept {
    template: Template,
    bytes: Vec<u8>,
}

impl Kept {
    fn bytes(&self) -> Bytes {
        lent(&self.bytes)
    }
}

// Prepares a template into a buffer of the size that an empty one learns.
fn kept(prepare: impl Fn(BytesMut) -> Prepared) -> Kept {
    let sized = prepare(writable(&mut []));
    assert_eq!(
        sized.status.code,
        ErrorCode::Capacity as u16,
        "{:?}",
        sized.status
    );
    let mut bytes = vec![0; sized.required];
    let written = prepare(writable(&mut bytes));
    assert_eq!(written.status, Status::default());
    assert_eq!(written.required, bytes.len());
    Kept {
        template: written.template,
        bytes,
    }
}

fn reads(options: &GetOptions) -> Kept {
    let container = container();
    // SAFETY: every pointer addresses a live value of the caller.
    kept(|buf| unsafe { borink_prepare_get(&container, token(), 0, options, buf) })
}

// Writes a request into a buffer of the size that an empty one learns.
fn request(encode: impl Fn(BytesMut) -> WireRequest) -> (WireRequest, Vec<u8>) {
    let sized = encode(writable(&mut []));
    assert_eq!(
        sized.status.code,
        ErrorCode::Capacity as u16,
        "{:?}",
        sized.status
    );
    let mut buf = vec![0; sized.required];
    let written = encode(writable(&mut buf));
    assert_eq!(written.status, Status::default());
    assert_eq!(written.required, buf.len());
    (written, buf)
}

fn part(buf: &[u8], span: Span) -> &[u8] {
    &buf[span.start..span.start + span.len]
}

// A request as its URL, its head and the body it wrote, from either crate.
#[derive(Debug, PartialEq, Eq)]
struct Sent {
    url: String,
    head: String,
    body: Vec<u8>,
    payload_len: u64,
}

fn sent(request: &WireRequest, buf: &[u8]) -> Sent {
    Sent {
        url: String::from_utf8(part(buf, request.url).to_vec()).unwrap(),
        head: String::from_utf8(part(buf, request.head).to_vec()).unwrap(),
        body: if request.body.present {
            part(buf, request.body.span).to_vec()
        } else {
            Vec::new()
        },
        payload_len: request.payload_len,
    }
}

fn proto_sent(placed: proto::Result<proto::Placement>, buf: &[u8]) -> Sent {
    let placed = placed.expect("the protocol crate writes the request");
    let request = placed.request(buf);
    Sent {
        url: request.url().to_string(),
        head: String::from_utf8(request.head().to_vec()).unwrap(),
        body: request.payload().bytes().unwrap_or_default().to_vec(),
        payload_len: placed.content_len(),
    }
}

// The header fields of a request, read one by one.
fn fields(request: &WireRequest, buf: &[u8]) -> Vec<(String, String)> {
    let mut walk = lent(part(buf, request.fields));
    let mut read = Vec::new();
    loop {
        // SAFETY: the walk addresses `buf`, which is live.
        let header = unsafe { borink_next_header(&mut walk) };
        if !header.present {
            break;
        }
        // SAFETY: the name and the value point into `buf`.
        let (name, value) = unsafe { (slice(header.name), slice(header.value)) };
        read.push((
            String::from_utf8(name.to_vec()).unwrap(),
            String::from_utf8(value.to_vec()).unwrap(),
        ));
    }
    read
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
        last_modified: Some(core::str::from_utf8(&VALUES[6..35]).unwrap()),
        version: Some(&VALUES[35..44]),
        content_encoding: Some(&VALUES[44..]),
        content_type: Some(b"text/plain; charset=utf-8"),
        ..CoreObjectMeta::default()
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

#[test]
fn every_write_and_block_outcome_crosses_whole() {
    let created = put_outcome(&PutHeadOutcome::Created { meta: full_meta() });
    assert_eq!(created.kind, OutcomeKind::Created as u16);
    assert!(created.meta.e_tag.present);

    for tag in [None, Some(e_tag())] {
        let staged = stage_outcome(&StageHeadOutcome::Staged { e_tag: tag });
        assert_eq!(staged.kind, OutcomeKind::Staged as u16);
        assert_eq!(borrowed(staged.meta.e_tag), tag);
    }

    let committed = commit_outcome(&CommitHeadOutcome::Committed { meta: full_meta() });
    assert_eq!(committed.kind, OutcomeKind::Committed as u16);
    assert_eq!(borrowed(committed.meta.version), full_meta().version);
    assert_eq!(
        commit_outcome(&CommitHeadOutcome::PreconditionFailed).kind,
        OutcomeKind::PreconditionFailed as u16
    );

    for kind in [None, Some(ServiceErrorKind::NotFound)] {
        let stage = stage_outcome(&StageHeadOutcome::NotFound { kind });
        let commit = commit_outcome(&CommitHeadOutcome::NotFound { kind });
        assert_eq!(stage.kind, OutcomeKind::NotFound as u16);
        assert_eq!(kind_of(stage.failure.kind), kind);
        assert_eq!(kind_of(commit.failure.kind), kind);
    }

    // Azure answers neither with a result body: only S3 does.
    assert_eq!(
        stage_outcome(&StageHeadOutcome::NeedResultBody { expected_len: None }).kind,
        OutcomeKind::Unsupported as u16
    );

    // A failure says the same thing whichever operation it answers, so the
    // twin needs no field naming the operation.
    for failure in every_failure() {
        let put = PutHeadOutcome::ServiceFailure(failure);
        let stage = StageHeadOutcome::ServiceFailure(failure);
        let commit = CommitHeadOutcome::ServiceFailure(failure);
        assert_eq!(text(&put_outcome(&put)), put.to_string());
        assert_eq!(text(&stage_outcome(&stage)), stage.to_string());
        assert_eq!(text(&commit_outcome(&commit)), commit.to_string());
    }
}

// The sentence for a failure, a missing object and an unsatisfiable range
// is the protocol crate's own. A settled outcome gets a literal, which names no
// operation because one twin answers them all.
#[test]
fn every_outcome_kind_says_something_of_its_own() {
    let outcome = GetHeadOutcome::NotFound {
        kind: Some(ServiceErrorKind::NotFound),
    };
    assert_eq!(text(&get_outcome(&outcome)), outcome.to_string());
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
        OutcomeKind::Created,
        OutcomeKind::Staged,
        OutcomeKind::Committed,
        OutcomeKind::Page,
    ] {
        let sentence = settled_sentence(Some(kind));
        assert!(!sentence.is_empty());
        assert_eq!(text(&only_kind(kind)), sentence);
        said.push(sentence);
    }
    said.sort_unstable();
    said.dedup();
    assert_eq!(said.len(), 8);

    // A kind from a later version of this crate names nothing here.
    let mut later = only_kind(OutcomeKind::Body);
    later.kind = 4095;
    assert_eq!(text(&later), settled_sentence(None));
}

// Every number a sweep below visits, which is every `u16` under `cargo
// test`. Miri interprets each iteration, and a sweep proves something about
// numbers rather than about memory, so under Miri it visits the range the
// discriminants live in and the largest `u16` for the edge. The highest
// discriminant any of these enums defines is 33.
fn sweep() -> impl Iterator<Item = u16> {
    let last = if cfg!(miri) { 64 } else { u16::MAX };
    (1..=last).chain((last != u16::MAX).then_some(u16::MAX))
}

// Every enum crosses as its number, and comes back the same value. A number
// that names nothing is refused, never read as another value.
#[test]
fn every_enum_crosses_by_its_number_and_refuses_the_rest() {
    let (mut kinds, mut classes, mut outcomes) = (0, 0, 0);
    for repr in sweep() {
        if let Some(kind) = kind_of(repr) {
            assert_eq!(kind_of(kind_view(Some(kind))), Some(kind), "{kind:?}");
            assert_eq!(kind_view(Some(kind)), repr);
            kinds += 1;
        }
        if let Some(class) = class_of(repr) {
            assert_eq!(class_of(class as u16), Some(class), "{class:?}");
            classes += 1;
        }
        assert_eq!(
            outcome_kind_of(repr).map(|kind| kind as u16),
            outcome_kind_of(repr).map(|_| repr)
        );
        outcomes += usize::from(outcome_kind_of(repr).is_some());
    }
    // How many numbers the sweep found. A variant added to one of these
    // enums, but left out of the call that reads a number into it, is a
    // variant no number reaches and the loop above never sees. These three
    // fail until someone states the new count. They also check `sweep`
    // itself: a discriminant above its Miri bound would be missing here.
    assert_eq!((kinds, classes, outcomes), (11, 5, 14));
    assert_eq!(kind_of(kind_view(None)), None);
    assert_eq!(kind_of(4095), None);
    assert_eq!(class_of(4095), None);
    assert!(outcome_kind_of(0).is_none());
    assert!(outcome_kind_of(15).is_none());

    // The input side, which crosses inwards and must refuse.
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
        let read = Range {
            form,
            start: 2,
            end: 6,
        };
        assert_eq!(requested_range(&read).ok(), expected);
    }
    for (kind, expected) in [
        (GetKind::Bytes as u16, Some(proto::GetKind::Bytes)),
        (GetKind::Head as u16, Some(proto::GetKind::Head)),
        (0, None),
        (4095, None),
    ] {
        let options = GetOptions {
            kind,
            ..read_options()
        };
        assert_eq!(
            get_options(&options).map(|options| options.kind).ok(),
            expected
        );
    }
    for (condition, expected) in [
        (Condition::None as u16, Some(proto::ConditionKind::None)),
        (
            Condition::IfMatch as u16,
            Some(proto::ConditionKind::IfMatch),
        ),
        (
            Condition::IfUnmodifiedSince as u16,
            Some(proto::ConditionKind::IfUnmodifiedSince),
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
        assert_eq!(
            commit_shape(&CommitShape { condition })
                .map(|shape| shape.condition)
                .ok(),
            expected
        );
    }
    for kind in [
        proto::EntryKind::Object,
        proto::EntryKind::Prefix,
        proto::EntryKind::Directory,
        proto::EntryKind::DeleteMarker,
    ] {
        let entry = proto::ListEntry {
            kind,
            content_type: Some("application/octet-stream"),
            ..Default::default()
        };
        assert_eq!(
            proto::EntryKind::from_discriminant(entry_view(&entry).kind),
            Some(kind)
        );
        assert_eq!(
            borrowed(entry_view(&entry).content_type),
            entry.content_type.map(str::as_bytes)
        );
    }
    // How many numbers the readers above accept. The tables say what each
    // number means, and these say that nothing else is read at all: a
    // number no variant defines is refused, whatever it is. The counts fail
    // when a variant is added to one of these enums, which is a variant the
    // tables have never crossed.
    let mut counts = [0usize; 9];
    for repr in core::iter::once(0).chain(sweep()) {
        let read = |options: GetOptions| usize::from(get_options(&options).is_ok());
        counts[0] += read(GetOptions {
            kind: repr,
            ..read_options()
        });
        counts[1] += read(GetOptions {
            ranging: repr,
            ..read_options()
        });
        counts[2] += read(GetOptions {
            revision: repr,
            ..read_options()
        });
        counts[3] += usize::from(
            requested_range(&Range {
                form: repr,
                start: 2,
                end: 6,
            })
            .is_ok(),
        );
        counts[4] += usize::from(condition_kind(repr).is_ok());
        counts[5] += usize::from(checksum_kind(repr).is_ok());
        counts[6] += usize::from(azure::BlockSource::from_discriminant(repr).is_some());
        counts[7] += usize::from(proto::EntryKind::from_discriminant(repr).is_some());
        let value = ConditionValue {
            form: repr,
            ..ConditionValue::default()
        };
        counts[8] += usize::from(condition_value(&value, b"").is_ok());
    }
    // A checksum and a condition value count their none, 0, too.
    assert_eq!(counts, [2, 2, 3, 4, 5, 7, 3, 4, 3]);

    // A listing asks for what the three flags name, and no other bit.
    for (include, expected) in [
        (0, Some(proto::ListInclude::default())),
        (
            ListInclude::Metadata as u32,
            Some(proto::ListInclude::METADATA),
        ),
        (
            ListInclude::Snapshots as u32 | ListInclude::Versions as u32,
            Some(proto::ListInclude::SNAPSHOTS | proto::ListInclude::VERSIONS),
        ),
        (2, None),
        (1 << 31, None),
    ] {
        let options = ListOptions {
            include,
            ..ListOptions::default()
        };
        assert_eq!(
            list_options(&options, b"")
                .map(|options| options.include)
                .ok(),
            expected
        );
    }
    // An absent count is not a zero one.
    for max_results in [None, Some(1000)] {
        let options = ListOptions {
            max_results: max_results.map_or_else(Default::default, |value| MaybeU32 {
                present: true,
                value,
            }),
            ..ListOptions::default()
        };
        assert_eq!(
            list_options(&options, b"")
                .map(|options| options.max_results)
                .ok(),
            Some(max_results)
        );
    }
}

// A number that this crate does not define stops the call, and says so.
#[test]
fn an_unknown_number_is_refused_rather_than_read_as_another_value() {
    let container = container();
    let mut buf = vec![0; 4096];
    let options = GetOptions {
        kind: 4095,
        ..read_options()
    };
    // SAFETY: every pointer below addresses a live value of this test.
    let (refused, unknown_namespace) = unsafe {
        (
            borink_prepare_get(&container, token(), 0, &options, writable(&mut buf)),
            borink_prepare_get(&container, token(), 7, &read_options(), writable(&mut buf)),
        )
    };
    for refused in [refused, unknown_namespace] {
        assert_eq!(refused.status, unknown());
        assert_eq!(refused.status.code, ErrorCode::InvalidRequest as u16);
        assert_eq!(refused.status.detail, InvalidRequest::Unknown as u16);
        assert_eq!(refused.required, 0);
    }

    // A condition value of a form this crate does not define.
    let reads = reads(&GetOptions {
        condition: Condition::IfMatch as u16,
        ..read_options()
    });
    let item = GetItem {
        condition_value: ConditionValue {
            form: 9,
            ..ConditionValue::default()
        },
        ..read_item(KEY.as_bytes())
    };
    // SAFETY: as above.
    let refused = unsafe {
        borink_encode_get(
            &reads.template,
            reads.bytes(),
            &item,
            &now(),
            writable(&mut buf),
        )
    };
    assert_eq!(refused.status, unknown());

    let shape = GetShape {
        kind: 4095,
        ..read_shape()
    };
    // SAFETY: as above, with no headers.
    let outcome = unsafe { borink_accept_get_head(shape, 200, core::ptr::null(), 0) };
    assert_eq!(outcome.kind, OutcomeKind::Invalid as u16);
    assert_eq!(outcome.error, unknown());
    assert_eq!(
        text(&outcome),
        Error::InvalidRequest(InvalidRequest::Unknown).to_string()
    );
}

// A pointer that was never filled in is refused as an invalid request, and
// never read.
#[test]
fn a_null_pointer_is_refused_rather_than_read() {
    let container = container();
    let reads = reads(&read_options());
    let item = read_item(KEY.as_bytes());
    let mut buf = vec![0; 4096];

    // SAFETY: the null pointers are the case under test, and the rest
    // address live values.
    unsafe {
        assert_eq!(borink_container_check(core::ptr::null()), unknown());
        let prepared = [
            borink_prepare_get(
                core::ptr::null(),
                token(),
                0,
                &read_options(),
                writable(&mut buf),
            ),
            borink_prepare_get(
                &container,
                token(),
                0,
                core::ptr::null(),
                writable(&mut buf),
            ),
            borink_prepare_put(
                &container,
                token(),
                0,
                core::ptr::null(),
                writable(&mut buf),
            ),
            borink_prepare_stage_block(
                &container,
                token(),
                0,
                core::ptr::null(),
                writable(&mut buf),
            ),
            borink_prepare_commit_blocks(
                &container,
                token(),
                0,
                core::ptr::null(),
                writable(&mut buf),
            ),
            borink_prepare_list(
                &container,
                token(),
                0,
                core::ptr::null(),
                writable(&mut buf),
            ),
        ];
        for prepared in prepared {
            assert_eq!(prepared.status, unknown());
        }
        assert_eq!(
            borink_template_check(core::ptr::null(), reads.bytes()),
            unknown()
        );
        let written = [
            borink_encode_get(
                core::ptr::null(),
                reads.bytes(),
                &item,
                &now(),
                writable(&mut buf),
            ),
            borink_encode_get(
                &reads.template,
                reads.bytes(),
                core::ptr::null(),
                &now(),
                writable(&mut buf),
            ),
            borink_encode_put(
                &reads.template,
                reads.bytes(),
                core::ptr::null(),
                &now(),
                writable(&mut buf),
            ),
            borink_encode_stage_block(
                &reads.template,
                reads.bytes(),
                core::ptr::null(),
                &now(),
                writable(&mut buf),
            ),
            borink_encode_commit_blocks(
                &reads.template,
                reads.bytes(),
                core::ptr::null(),
                &now(),
                writable(&mut buf),
            ),
            borink_encode_list(
                &reads.template,
                reads.bytes(),
                core::ptr::null(),
                &now(),
                writable(&mut buf),
            ),
        ];
        for written in written {
            assert_eq!(written.status, unknown());
        }
        assert_eq!(
            borink_encode_get(
                &reads.template,
                reads.bytes(),
                &item,
                core::ptr::null(),
                writable(&mut buf)
            )
            .status,
            unknown()
        );
        let garbled = Timestamps {
            stored: [0xFF; TIMESTAMPS_LEN],
        };
        assert_eq!(
            borink_encode_get(
                &reads.template,
                reads.bytes(),
                &item,
                &garbled,
                writable(&mut buf)
            )
            .status,
            unknown()
        );
        assert!(!borink_next_header(core::ptr::null_mut()).present);
        assert_eq!(borink_get_item_shape(&item, core::ptr::null()).kind, 0);
        assert_eq!(
            borink_put_item_shape(core::ptr::null(), core::ptr::null()).condition,
            0
        );
        assert_eq!(
            borink_commit_item_shape(core::ptr::null(), core::ptr::null()).condition,
            0
        );
        assert_eq!(
            borink_fill_listing(writable(&mut buf), core::ptr::null_mut(), 0)
                .status
                .code,
            ErrorCode::Response as u16
        );
        // A sentence for nothing is no sentence, not a guess at one.
        assert_eq!(borink_describe(core::ptr::null(), writable(&mut buf)), 0);
    }

    // A failure whose category this crate does not define is refused too.
    let failure = Failure {
        status: 503,
        class: 4095,
        ..Failure::default()
    };
    // SAFETY: the failure lends nothing, and the body is empty.
    let outcomes = unsafe {
        [
            borink_accept_get_error_body(read_shape(), failure, lent(b"")),
            borink_accept_put_error_body(write_shape(), failure, lent(b"")),
            borink_accept_stage_block_error_body(failure, lent(b"")),
            borink_accept_commit_blocks_error_body(
                CommitShape {
                    condition: Condition::None as u16,
                },
                failure,
                lent(b""),
            ),
            borink_accept_list_error_body(failure, lent(b"")),
        ]
    };
    for outcome in outcomes {
        assert_eq!(outcome.error, unknown());
    }
}

// Every request that crosses is the one that the protocol crate writes from the
// same values: the same URL, the same head and the same body.
#[test]
fn each_request_is_the_one_the_proto_crate_writes() {
    let container = container();
    let core = proto_container();
    let core_now = proto::Timestamps::from_unix(NOW);
    let mut room = vec![0; 4096];
    let mut written = vec![0; 4096];
    let version = "2026-09-01T19:08:11.0000000Z";

    // A ranged read of a version, if it changed.
    let options = GetOptions {
        ranging: Ranging::Ranged as u16,
        condition: Condition::IfNoneMatch as u16,
        revision: RevisionKind::Version as u16,
        ..read_options()
    };
    let item = GetItem {
        range: Range {
            form: RangeForm::Bounded as u16,
            start: 2,
            end: 6,
        },
        condition_value: tag(b"\"etag\""),
        revision: lent(version.as_bytes()),
        ..read_item(KEY.as_bytes())
    };
    let reads = reads(&options);
    // SAFETY: every pointer addresses a live value of this test.
    let (ours, buf) = request(|buf| unsafe {
        borink_encode_get(&reads.template, reads.bytes(), &item, &now(), buf)
    });
    let core_options = proto::GetOptions {
        ranging: proto::Ranging::Ranged,
        condition: proto::ConditionKind::IfNoneMatch,
        revision: proto::RevisionKind::Version,
        ..proto::GetOptions::new()
    };
    let template = azure::prepare_get(
        &core,
        TOKEN,
        proto::azure::AzureNamespace::Unknown,
        &core_options,
        &mut room,
    )
    .unwrap();
    let core_item = proto::GetItem {
        range: RequestedRange::Bounded { start: 2, end: 6 },
        condition_value: Some(proto::ConditionValue::ETag(b"\"etag\"")),
        revision: Some(version),
        ..proto::GetItem::new(KEY)
    };
    assert_eq!(
        sent(&ours, &buf),
        proto_sent(
            azure::encode_get(&template, &room, &core_item, &core_now, &mut written),
            &written
        )
    );
    assert_eq!(ours.method, Method::Get as u16);
    assert!(!ours.body.present);

    // A HEAD, if the object changed since a date.
    let options = GetOptions {
        kind: GetKind::Head as u16,
        condition: Condition::IfModifiedSince as u16,
        ..read_options()
    };
    let item = GetItem {
        condition_value: ConditionValue {
            form: ConditionForm::Time as u16,
            e_tag: Bytes::default(),
            seconds: NOW - 60,
        },
        ..read_item(KEY.as_bytes())
    };
    let heads = self::reads(&options);
    // SAFETY: as above.
    let (ours, buf) = request(|buf| unsafe {
        borink_encode_get(&heads.template, heads.bytes(), &item, &now(), buf)
    });
    let core_options = proto::GetOptions {
        condition: proto::ConditionKind::IfModifiedSince,
        ..proto::GetOptions::head()
    };
    let template = azure::prepare_get(
        &core,
        TOKEN,
        proto::azure::AzureNamespace::Unknown,
        &core_options,
        &mut room,
    )
    .unwrap();
    let core_item = proto::GetItem {
        condition_value: Some(proto::ConditionValue::Time(NOW - 60)),
        ..proto::GetItem::new(KEY)
    };
    assert_eq!(
        sent(&ours, &buf),
        proto_sent(
            azure::encode_get(&template, &room, &core_item, &core_now, &mut written),
            &written
        )
    );
    assert_eq!(ours.method, Method::Head as u16);

    // A write only if the key holds no object, with a CRC64 and two values
    // that it stores with the blob.
    let options = PutOptions {
        condition: Condition::IfNoneMatch as u16,
        options: WriteOptions {
            properties: ContentProperties {
                content_type: lent(b"text/plain"),
                ..ContentProperties::default()
            },
            storage_class: lent(b"Cool"),
        },
        checksum: ChecksumKind::Crc64 as u16,
    };
    let item = PutItem {
        key: lent(KEY.as_bytes()),
        content_len: 4,
        condition_value: tag(b"*"),
        checksum: lent(b"AAAAAAAAAAA="),
    };
    // SAFETY: as above.
    let writes = kept(|buf| unsafe { borink_prepare_put(&container, token(), 0, &options, buf) });
    // SAFETY: as above.
    let (ours, buf) = request(|buf| unsafe {
        borink_encode_put(&writes.template, writes.bytes(), &item, &now(), buf)
    });
    let core_options = proto::PutOptions {
        condition: proto::ConditionKind::IfNoneMatch,
        options: proto::WriteOptions {
            properties: proto::ContentProperties {
                content_type: Some("text/plain"),
                ..proto::ContentProperties::new()
            },
            storage_class: Some("Cool"),
            ..proto::WriteOptions::new()
        },
        checksum: Some(proto::ChecksumKind::Crc64),
        ..proto::PutOptions::default()
    };
    let template = azure::prepare_put(
        &core,
        TOKEN,
        proto::azure::AzureNamespace::Unknown,
        &core_options,
        &mut room,
    )
    .unwrap();
    let core_item = proto::PutItem {
        condition_value: Some(proto::ConditionValue::ETag(b"*")),
        checksum: Some(proto::ChecksumValue::Base64("AAAAAAAAAAA=")),
        ..proto::PutItem::new(KEY, proto::Payload::Streamed { len: 4 })
    };
    assert_eq!(
        sent(&ours, &buf),
        proto_sent(
            azure::encode_put(&template, &room, &core_item, &core_now, &mut written),
            &written
        )
    );
    assert_eq!(ours.payload_len, 4);
    // SAFETY: as above.
    let shape = unsafe { borink_put_item_shape(&item, &options) };
    assert_eq!(shape.condition, Condition::IfNoneMatch as u16);

    // One block, with its MD5.
    let options = StageOptions {
        checksum: ChecksumKind::Md5 as u16,
    };
    let item = StageBlockItem {
        key: lent(KEY.as_bytes()),
        id: lent(BLOCK_IDS[0].as_bytes()),
        content_len: 8,
        checksum: lent(MD5.as_bytes()),
    };
    // SAFETY: as above.
    let stages =
        kept(|buf| unsafe { borink_prepare_stage_block(&container, token(), 0, &options, buf) });
    // SAFETY: as above.
    let (ours, buf) = request(|buf| unsafe {
        borink_encode_stage_block(&stages.template, stages.bytes(), &item, &now(), buf)
    });
    let core_options = proto::StageOptions {
        checksum: Some(proto::ChecksumKind::Md5),
    };
    let template = azure::prepare_stage_block(
        &core,
        TOKEN,
        proto::azure::AzureNamespace::Unknown,
        &core_options,
        &mut room,
    )
    .unwrap();
    let core_item = azure::StageBlockItem {
        checksum: Some(proto::ChecksumValue::Base64(MD5)),
        ..azure::StageBlockItem::new(KEY, BLOCK_IDS[0], proto::Payload::Streamed { len: 8 })
    };
    assert_eq!(
        sent(&ours, &buf),
        proto_sent(
            azure::encode_stage_block(&template, &room, &core_item, &core_now, &mut written),
            &written
        )
    );

    // A commit of two blocks, if the blob is the one it was, with the MD5 of
    // the whole blob. Its block list is the request's body.
    let options = CommitOptions {
        condition: Condition::IfMatch as u16,
        content_md5: true,
        ..CommitOptions::default()
    };
    let blocks = [
        BlockRef {
            id: lent(BLOCK_IDS[0].as_bytes()),
            source: BlockSource::Latest as u16,
        },
        BlockRef {
            id: lent(BLOCK_IDS[1].as_bytes()),
            source: BlockSource::Committed as u16,
        },
    ];
    let item = CommitItem {
        key: lent(KEY.as_bytes()),
        condition_value: tag(b"\"etag\""),
        blocks: blocks.as_ptr(),
        block_count: blocks.len(),
        content_md5: lent(MD5.as_bytes()),
    };
    // SAFETY: as above.
    let commits =
        kept(|buf| unsafe { borink_prepare_commit_blocks(&container, token(), 0, &options, buf) });
    // SAFETY: as above.
    let (ours, buf) = request(|buf| unsafe {
        borink_encode_commit_blocks(&commits.template, commits.bytes(), &item, &now(), buf)
    });
    let core_options = azure::CommitOptions {
        condition: proto::ConditionKind::IfMatch,
        content_md5: true,
        ..azure::CommitOptions::default()
    };
    let template = azure::prepare_commit_blocks(
        &core,
        TOKEN,
        proto::azure::AzureNamespace::Unknown,
        &core_options,
        &mut room,
    )
    .unwrap();
    let core_blocks = [
        azure::BlockRef {
            id: BLOCK_IDS[0],
            source: azure::BlockSource::Latest,
        },
        azure::BlockRef {
            id: BLOCK_IDS[1],
            source: azure::BlockSource::Committed,
        },
    ];
    let core_item = azure::CommitItem {
        condition_value: Some(proto::ConditionValue::ETag(b"\"etag\"")),
        content_md5: Some(MD5),
        ..azure::CommitItem::new(KEY, &core_blocks)
    };
    let theirs = proto_sent(
        azure::encode_commit_blocks(&template, &room, &core_item, &core_now, &mut written),
        &written,
    );
    assert_eq!(sent(&ours, &buf), theirs);
    assert!(ours.body.present);
    assert_eq!(ours.payload_len, theirs.body.len() as u64);
    assert!(theirs.body.starts_with(b"<?xml"), "{:?}", theirs.body);
    // SAFETY: as above.
    let shape = unsafe { borink_commit_item_shape(&item, &options) };
    assert_eq!(shape.condition, Condition::IfMatch as u16);

    // A page of a delimited listing, after a marker, with the metadata and
    // the versions of each object.
    let options = ListOptions {
        delimiter: lent(b"/"),
        max_results: MaybeU32 {
            present: true,
            value: 2,
        },
        include: ListInclude::Metadata as u32 | ListInclude::Versions as u32,
    };
    let item = ListItem {
        prefix: lent(b"directory/"),
        marker: lent(b"marker-1"),
    };
    // SAFETY: as above.
    let pages = kept(|buf| unsafe { borink_prepare_list(&container, token(), 0, &options, buf) });
    // SAFETY: as above.
    let (ours, buf) = request(|buf| unsafe {
        borink_encode_list(&pages.template, pages.bytes(), &item, &now(), buf)
    });
    let core_options = proto::ListOptions {
        delimiter: Some("/"),
        max_results: Some(2),
        include: proto::ListInclude::METADATA | proto::ListInclude::VERSIONS,
    };
    let template = azure::prepare_list(
        &core,
        TOKEN,
        proto::azure::AzureNamespace::Unknown,
        &core_options,
        &mut room,
    )
    .unwrap();
    let core_item = proto::ListItem {
        marker: Some(proto::ListMarker::Text("marker-1")),
        ..proto::ListItem::new("directory/")
    };
    let ours = sent(&ours, &buf);
    assert_eq!(
        ours,
        proto_sent(
            azure::encode_list(&template, &room, &core_item, &core_now, &mut written),
            &written
        )
    );
    assert!(ours.url.starts_with(
        "https://account.blob.core.windows.net/container\
         ?restype=container&comp=list&delimiter=%2F&maxresults=2&include="
    ));
    assert!(
        ours.url.ends_with("&prefix=directory%2F&marker=marker-1"),
        "{}",
        ours.url
    );
}

// A request's header fields are read one by one, out of the request's own
// buffer, and a walk that ended stays ended.
#[test]
fn the_header_fields_of_a_request_are_read_one_by_one() {
    let reads = reads(&GetOptions {
        ranging: Ranging::Ranged as u16,
        ..read_options()
    });
    let item = GetItem {
        range: Range {
            form: RangeForm::Bounded as u16,
            start: 2,
            end: 6,
        },
        ..read_item(b"object.bin")
    };
    // SAFETY: every pointer addresses a live value of this test.
    let (request, buf) = request(|buf| unsafe {
        borink_encode_get(&reads.template, reads.bytes(), &item, &now(), buf)
    });
    let read = fields(&request, &buf);
    let names: Vec<&str> = read.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        names,
        ["authorization", "x-ms-version", "x-ms-date", "range"]
    );
    assert_eq!(read[0].1, "Bearer token");
    assert_eq!(read[3].1, "bytes=2-5");
    assert_eq!(
        String::from_utf8(part(&buf, request.url).to_vec()).unwrap(),
        "https://account.blob.core.windows.net/container/object.bin"
    );
    // Each name and value points into the request's buffer.
    let mut walk = lent(part(&buf, request.fields));
    // SAFETY: the walk addresses `buf`, which is live.
    let first = unsafe { borink_next_header(&mut walk) };
    let start = first.name.ptr as usize - buf.as_ptr() as usize;
    assert!(start >= request.fields.start);
    // SAFETY: as above.
    unsafe {
        while borink_next_header(&mut walk).present {}
        assert!(!borink_next_header(&mut walk).present);
    }
    assert_eq!(walk.len, 0);
}

// A buffer that is too small is refused with the exact size it needs, for
// a template and for a request alike.
#[test]
fn a_buffer_that_is_too_small_reports_the_size_it_needs() {
    let container = container();
    let options = read_options();
    // SAFETY: every pointer addresses a live value of this test.
    let sized = unsafe { borink_prepare_get(&container, token(), 0, &options, writable(&mut [])) };
    assert_eq!(sized.status.code, ErrorCode::Capacity as u16);
    let mut short = vec![0; sized.required - 1];
    // SAFETY: as above.
    let refused =
        unsafe { borink_prepare_get(&container, token(), 0, &options, writable(&mut short)) };
    assert_eq!(refused.status.code, ErrorCode::Capacity as u16);
    assert_eq!(refused.required, sized.required);

    let reads = reads(&options);
    let item = read_item(b"object.bin");
    // SAFETY: as above.
    let sized = unsafe {
        borink_encode_get(
            &reads.template,
            reads.bytes(),
            &item,
            &now(),
            writable(&mut []),
        )
    };
    assert_eq!(sized.status.code, ErrorCode::Capacity as u16);
    let mut short = vec![0; sized.required - 1];
    // SAFETY: as above.
    let refused = unsafe {
        borink_encode_get(
            &reads.template,
            reads.bytes(),
            &item,
            &now(),
            writable(&mut short),
        )
    };
    assert_eq!(refused.status.code, ErrorCode::Capacity as u16);
    assert_eq!(refused.required, sized.required);
    let mut sentence = [0; 128];
    // SAFETY: the buffer is live and nothing else reaches it.
    let length = unsafe { borink_describe_status(refused.status, writable(&mut sentence)) };
    assert!(length > 0 && length <= sentence.len());
}

// A template of one object holds its key, so each request names it with an
// empty key or with that key, and no other.
#[test]
fn a_template_of_one_object_writes_its_key() {
    let container = Container {
        object: lent(KEY.as_bytes()),
        ..container()
    };
    let options = read_options();
    // SAFETY: every pointer addresses a live value of this test.
    let reads = kept(|buf| unsafe { borink_prepare_get(&container, token(), 0, &options, buf) });
    for key in [b"".as_slice(), KEY.as_bytes()] {
        let item = read_item(key);
        // SAFETY: as above.
        let (request, buf) = request(|buf| unsafe {
            borink_encode_get(&reads.template, reads.bytes(), &item, &now(), buf)
        });
        assert_eq!(
            String::from_utf8(part(&buf, request.url).to_vec()).unwrap(),
            "https://account.blob.core.windows.net/container/dir/object%20one.bin"
        );
    }
    let item = read_item(b"other.bin");
    let mut buf = vec![0; 1024];
    // SAFETY: as above.
    let refused = unsafe {
        borink_encode_get(
            &reads.template,
            reads.bytes(),
            &item,
            &now(),
            writable(&mut buf),
        )
    };
    assert_eq!(refused.status, status_of(&InvalidRequest::Template.into()));

    // A listing names no object.
    // SAFETY: as above.
    let refused = unsafe {
        borink_prepare_list(
            &container,
            token(),
            0,
            &ListOptions::default(),
            writable(&mut buf),
        )
    };
    assert_eq!(refused.status, status_of(&InvalidRequest::Option.into()));
}

// A template holds no pointer: it can be copied and stored, and its bytes
// are checked once where they could be another template's.
#[test]
fn a_stored_template_is_checked_against_its_bytes() {
    let reads = reads(&read_options());
    let pages = kept(|buf| {
        // SAFETY: every pointer addresses a live value of this test.
        unsafe { borink_prepare_list(&container(), token(), 0, &ListOptions::default(), buf) }
    });
    // A copy of the template's bytes is the template too.
    let copied = Template {
        stored: reads.template.stored,
    };
    // SAFETY: every pointer addresses a live value of this test.
    unsafe {
        assert_eq!(
            borink_template_check(&copied, reads.bytes()),
            Status::default()
        );
        assert_eq!(
            borink_template_check(&copied, pages.bytes()),
            status_of(&InvalidRequest::Template.into())
        );
    }
    // A template of another operation is refused, and so is one this crate
    // did not write.
    let item = read_item(b"object.bin");
    let mut buf = vec![0; 1024];
    let template = |stored| Template { stored };
    for (template, bytes) in [
        (pages.template, pages.bytes()),
        (Template::default(), reads.bytes()),
        (template([0xA5; TEMPLATE_LEN]), reads.bytes()),
    ] {
        // SAFETY: as above.
        let refused =
            unsafe { borink_encode_get(&template, bytes, &item, &now(), writable(&mut buf)) };
        assert_eq!(refused.status, status_of(&InvalidRequest::Template.into()));
    }
    // Bytes that are not the template's write a wrong request or are
    // refused, and never panic.
    // SAFETY: as above.
    let _ = unsafe {
        borink_encode_get(
            &reads.template,
            pages.bytes(),
            &item,
            &now(),
            writable(&mut buf),
        )
    };
}

// A commit checks each of its blocks before the protocol crate reads any.
#[test]
fn a_commit_refuses_a_block_it_cannot_read() {
    let commits = kept(|buf| {
        // SAFETY: every pointer addresses a live value of this test.
        unsafe {
            borink_prepare_commit_blocks(
                &container(),
                token(),
                0,
                &CommitOptions {
                    condition: Condition::None as u16,
                    ..CommitOptions::default()
                },
                buf,
            )
        }
    });
    let good = BlockRef {
        id: lent(BLOCK_IDS[0].as_bytes()),
        source: BlockSource::Latest as u16,
    };
    let mut buf = vec![0; 1024];
    for (block, expected) in [
        (
            BlockRef {
                id: lent(b"\xff\xfe=="),
                ..good
            },
            InvalidRequest::PartId,
        ),
        (BlockRef { source: 9, ..good }, InvalidRequest::Unknown),
        (
            BlockRef {
                id: lent(b"not base64!"),
                ..good
            },
            InvalidRequest::PartId,
        ),
    ] {
        let blocks = [good, block];
        let item = CommitItem {
            key: lent(KEY.as_bytes()),
            condition_value: ConditionValue::default(),
            blocks: blocks.as_ptr(),
            block_count: blocks.len(),
            content_md5: Bytes::default(),
        };
        // SAFETY: as above.
        let refused = unsafe {
            borink_encode_commit_blocks(
                &commits.template,
                commits.bytes(),
                &item,
                &now(),
                writable(&mut buf),
            )
        };
        assert_eq!(refused.status, status_of(&expected.into()), "{expected:?}");
    }
    // No blocks commits an empty blob.
    let item = CommitItem {
        key: lent(KEY.as_bytes()),
        condition_value: ConditionValue::default(),
        blocks: core::ptr::null(),
        block_count: 0,
        content_md5: Bytes::default(),
    };
    // SAFETY: as above, with no blocks.
    let (written, buf) = request(|buf| unsafe {
        borink_encode_commit_blocks(&commits.template, commits.bytes(), &item, &now(), buf)
    });
    assert!(written.body.present);
    assert!(part(&buf, written.body.span).ends_with(b"<BlockList></BlockList>"));
}

// The shape of a read carries its options and its range, and nothing that
// points anywhere.
#[test]
fn the_shape_of_a_read_carries_its_options_and_its_range() {
    let options = GetOptions {
        kind: GetKind::Head as u16,
        condition: Condition::IfMatch as u16,
        ..read_options()
    };
    let item = GetItem {
        range: Range {
            form: RangeForm::Offset as u16,
            start: 7,
            end: 0,
        },
        ..read_item(b"object.bin")
    };
    // SAFETY: both values are live.
    let shape = unsafe { borink_get_item_shape(&item, &options) };
    let core = get_shape(&shape).unwrap();
    assert_eq!(core.kind, proto::GetKind::Head);
    assert_eq!(core.range, RequestedRange::Offset(7));
    assert_eq!(core.condition, proto::ConditionKind::IfMatch);
}

// A Put Block and a Put Block List are read from their heads, and a commit
// whose blocks Azure refused names that in its body.
#[test]
fn the_heads_of_a_write_in_blocks_are_read() {
    let headers = [
        header("ETag", e_tag()),
        header("x-ms-request-id", IDENTIFIER),
    ];
    // SAFETY: every pointer addresses a live value of this test.
    let (staged, committed, refused) = unsafe {
        (
            borink_accept_stage_block_head(201, headers.as_ptr(), headers.len()),
            borink_accept_commit_blocks_head(
                CommitShape {
                    condition: Condition::None as u16,
                },
                201,
                headers.as_ptr(),
                headers.len(),
            ),
            borink_accept_commit_blocks_head(
                CommitShape {
                    condition: Condition::None as u16,
                },
                400,
                headers.as_ptr(),
                headers.len(),
            ),
        )
    };
    assert_eq!(staged.kind, OutcomeKind::Staged as u16);
    assert_eq!(staged.meta.e_tag.bytes.ptr, e_tag().as_ptr());
    assert_eq!(committed.kind, OutcomeKind::Committed as u16);
    assert_eq!(borrowed(committed.meta.e_tag), Some(e_tag()));
    assert_eq!(refused.kind, OutcomeKind::NeedErrorBody as u16);

    let body = b"<Error><Code>InvalidBlockList</Code></Error>";
    // SAFETY: the failure lends the request id, which is static, and the
    // body is live.
    let finished = unsafe {
        borink_accept_commit_blocks_error_body(
            CommitShape {
                condition: Condition::None as u16,
            },
            refused.failure,
            lent(body),
        )
    };
    assert_eq!(finished.kind, OutcomeKind::ServiceFailure as u16);
    assert_eq!(
        kind_of(finished.failure.kind),
        Some(ServiceErrorKind::InvalidUpload)
    );
    assert_eq!(borrowed(finished.failure.request_id), Some(IDENTIFIER));

    // A stage answered 200 is not one Azure sends.
    // SAFETY: no headers.
    let odd = unsafe { borink_accept_stage_block_head(200, core::ptr::null(), 0) };
    assert_eq!(odd.kind, OutcomeKind::Invalid as u16);
    assert_eq!(odd.error.detail, ResponseFault::Status as u16);
    // SAFETY: as above.
    let failed = unsafe { borink_accept_stage_block_error_body(refused.failure, lent(body)) };
    assert_eq!(
        kind_of(failed.failure.kind),
        Some(ServiceErrorKind::InvalidUpload)
    );
}

#[test]
fn a_container_that_cannot_be_used_says_which_value_is_wrong() {
    let mut buf = vec![0; 4096];
    // What the container's check refuses, and the prepare call with it.
    for (endpoint, name, object, expected) in [
        (
            b"account.example".as_slice(),
            b"container".as_slice(),
            b"".as_slice(),
            status_of(&Error::InvalidEndpoint),
        ),
        (
            b"https://account.example",
            b"",
            b"",
            status_of(&Error::InvalidContainer),
        ),
        (
            b"\xff",
            b"container",
            b"",
            status_of(&Error::InvalidEndpoint),
        ),
        (
            b"https://account.example",
            b"container",
            b"\xff",
            status_of(&InvalidRequest::KeyNotUtf8.into()),
        ),
    ] {
        let container = Container {
            object: lent(object),
            ..located(endpoint, name)
        };
        // SAFETY: every pointer addresses a live value of this test.
        let (status, refused) = unsafe {
            (
                borink_container_check(&container),
                borink_prepare_get(&container, token(), 0, &read_options(), writable(&mut buf)),
            )
        };
        assert_eq!(status, expected);
        // A container that cannot write a template says the same thing when
        // asked to.
        assert_eq!(refused.status, status);
    }
    // The token, and the key of the one object, are checked where a
    // template is prepared, as the protocol crate checks them.
    let normalized = Container {
        object: lent(b"dir/../up"),
        ..container()
    };
    // SAFETY: as above.
    unsafe {
        assert_eq!(borink_container_check(&normalized), Status::default());
        assert_eq!(
            borink_prepare_get(&normalized, token(), 0, &read_options(), writable(&mut buf)).status,
            status_of(&InvalidRequest::KeyWouldBeNormalized.into())
        );
        for token in [b"".as_slice(), b"\xff", b"two\nlines"] {
            assert_eq!(
                borink_prepare_get(
                    &container(),
                    lent(token),
                    0,
                    &read_options(),
                    writable(&mut buf)
                )
                .status,
                status_of(&Error::InvalidToken)
            );
        }
        assert_eq!(borink_container_check(&container()), Status::default());
    }
}

// A listing request is checked before any byte is written, and the reason names
// the field that was wrong.
#[test]
fn a_listing_plan_that_azure_would_refuse_is_refused_here() {
    let container = container();
    let mut buf = vec![0; 1024];
    for (options, expected) in [
        (
            ListOptions {
                max_results: MaybeU32 {
                    present: true,
                    value: 0,
                },
                ..ListOptions::default()
            },
            status_of(&InvalidRequest::MaxResults.into()),
        ),
        (
            ListOptions {
                delimiter: lent(b"\xff"),
                ..ListOptions::default()
            },
            status_of(&InvalidRequest::Delimiter.into()),
        ),
        (
            ListOptions {
                include: 2,
                ..ListOptions::default()
            },
            unknown(),
        ),
    ] {
        // SAFETY: every pointer below addresses a live value of this test.
        let refused =
            unsafe { borink_prepare_list(&container, token(), 0, &options, writable(&mut buf)) };
        assert_eq!(refused.status, expected);
    }
    // A hierarchical account groups keys at `/` alone.
    let options = ListOptions {
        delimiter: lent(b"|"),
        ..ListOptions::default()
    };
    // SAFETY: as above.
    let refused = unsafe {
        borink_prepare_list(
            &container,
            token(),
            AzureNamespace::Hierarchical as u16,
            &options,
            writable(&mut buf),
        )
    };
    assert_eq!(refused.status, status_of(&InvalidRequest::Delimiter.into()));
}

#[test]
fn a_listing_head_announces_the_page() {
    let headers = [header("Content-Length", b"120")];
    // SAFETY: the headers are live, and the call takes no shape.
    let outcome = unsafe { borink_accept_list_head(200, headers.as_ptr(), 1) };
    assert_eq!(outcome.kind, OutcomeKind::Page as u16);
    assert!(outcome.body.expected_len.present);
    assert_eq!(outcome.body.expected_len.value, 120);
    assert_eq!(text(&outcome), "the page follows in the response body");
}

#[test]
fn local_rejections_keep_the_reason_and_map_only_known_service_responses() {
    for namespace in [
        AzureNamespace::Unknown,
        AzureNamespace::Flat,
        AzureNamespace::Hierarchical,
    ] {
        for reason in [
            InvalidRequest::MaxResults,
            InvalidRequest::KeyTooLong,
            InvalidRequest::UnsupportedRange,
        ] {
            let status = status_of(&Error::InvalidRequest(reason));
            let azure = borink_azure_rejection_for(status, namespace as u16);
            let expected = match reason {
                InvalidRequest::MaxResults => Some("OutOfRangeQueryParameterValue"),
                InvalidRequest::KeyTooLong if namespace as u16 == AzureNamespace::Flat as u16 => {
                    Some("OutOfRangeInput")
                }
                _ => None,
            };
            assert_eq!(azure.status, if expected.is_some() { 400 } else { 0 });
            // SAFETY: a returned code points at static bytes, or has length zero.
            assert_eq!(
                unsafe { slice(azure.code) },
                expected.unwrap_or("").as_bytes()
            );
        }
    }
    assert_eq!(borink_azure_rejection_for(Status::default(), 0).status, 0);
    let status = status_of(&Error::InvalidRequest(InvalidRequest::MaxResults));
    assert_eq!(borink_azure_rejection_for(status, u16::MAX).status, 0);

    let reads = reads(&read_options());
    for (key, reason) in [
        (b"".as_slice(), InvalidRequest::EmptyKey),
        (b"\xff", InvalidRequest::KeyNotUtf8),
    ] {
        let item = read_item(key);
        // SAFETY: inputs are live; the output capacity is zero.
        let written = unsafe {
            borink_encode_get(
                &reads.template,
                reads.bytes(),
                &item,
                &now(),
                writable(&mut []),
            )
        };
        assert_eq!(written.status, status_of(&Error::InvalidRequest(reason)));
    }
}

// Every value that the protocol crate returns has one twin, the twin carries
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
    assert_eq!(borrowed(view.meta.content_type), full_meta().content_type);
    assert_eq!(
        view.meta.content_type.bytes.ptr,
        full_meta().content_type.unwrap().as_ptr()
    );
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
        assert_eq!(optional_u64(view.body.object_size), object_size);
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

// Every error of the protocol crate crosses as two numbers and comes back as
// the same sentence.
#[test]
fn every_error_crosses_as_a_status() {
    let mut checked = 0;
    for code in sweep() {
        let Some(code) = proto::ErrorCode::from_discriminant(code) else {
            continue;
        };
        for detail in core::iter::once(0).chain(sweep()) {
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
    // The five codes that carry no inner value, every variant of the three
    // inner enums, and a service error that names no known error.
    assert_eq!(checked, 5 + 33 + 4 + 11 + 1);
    assert_eq!(
        ResponseFault::from_discriminant(3).map(Error::Response),
        Error::from_parts(proto::ErrorCode::Response, 3)
    );
}

// The head reaches this crate as slices, from wherever the host keeps them.
// Nothing here is one buffer, and the outcome points back at each.
#[test]
fn a_head_crosses_as_slices_of_whatever_holds_it() {
    let content_type = b"text/plain; charset=utf-8";
    let headers = [
        header("ETag", e_tag()),
        header("Content-Length", b"10"),
        header("Content-Type", content_type),
        header("x-ms-request-id", IDENTIFIER),
        // A name that is not text is none of the ones the protocol crate reads,
        // so it is skipped rather than refused.
        HeaderRef {
            name: lent(b"\xff"),
            value: lent(b"value"),
        },
    ];
    // SAFETY: every pointer addresses a live value of this test.
    let outcome =
        unsafe { borink_accept_get_head(read_shape(), 200, headers.as_ptr(), headers.len()) };
    assert_eq!(outcome.kind, OutcomeKind::Body as u16);
    assert_eq!(outcome.meta.e_tag.bytes.ptr, e_tag().as_ptr());
    assert_eq!(
        borrowed(outcome.meta.content_type),
        Some(content_type.as_slice())
    );
    assert_eq!(outcome.meta.content_type.bytes.ptr, content_type.as_ptr());
    assert!(outcome.body.expected_len.present);
    assert_eq!(outcome.body.expected_len.value, 10);
}

// The head asked for the error body, and the body names the error. The
// request id crosses as bytes the host still owns, both ways.
#[test]
fn the_error_body_finishes_what_the_head_left_open() {
    let headers = [header("x-ms-request-id", IDENTIFIER)];
    // SAFETY: every pointer addresses a live value of this test.
    let outcome =
        unsafe { borink_accept_put_head(write_shape(), 409, headers.as_ptr(), headers.len()) };
    assert_eq!(outcome.kind, OutcomeKind::NeedErrorBody as u16);
    assert_eq!(outcome.failure.request_id.bytes.ptr, IDENTIFIER.as_ptr());

    // SAFETY: as above, and the body outlives the outcome it names.
    let finished = unsafe {
        borink_accept_put_error_body(
            write_shape(),
            outcome.failure,
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
    let unnamed =
        unsafe { borink_accept_put_error_body(write_shape(), outcome.failure, lent(b"")) };
    assert_eq!(unnamed.kind, OutcomeKind::ServiceFailure as u16);
    assert_eq!(kind_of(unnamed.failure.kind), None);
}

// A 412 is the request's failed condition only if the request carried one and
// Azure names it, whether in the head or only in the body.
#[test]
fn a_412_is_a_failed_condition_only_when_azure_names_one() {
    let conditional = PutShape {
        condition: Condition::IfMatch as u16,
    };
    let lease = [header("x-ms-error-code", b"LeaseIdMissing")];
    // SAFETY: every pointer addresses a live value of this test.
    let refused = unsafe { borink_accept_put_head(conditional, 412, lease.as_ptr(), lease.len()) };
    assert_eq!(refused.kind, OutcomeKind::ServiceFailure as u16);

    // SAFETY: as above.
    let unnamed = unsafe { borink_accept_put_head(conditional, 412, core::ptr::null(), 0) };
    assert_eq!(unnamed.kind, OutcomeKind::NeedErrorBody as u16);
    // SAFETY: as above, and the body outlives the outcome it names.
    let failed = unsafe {
        borink_accept_put_error_body(
            conditional,
            unnamed.failure,
            lent(b"<Error><Code>ConditionNotMet</Code></Error>"),
        )
    };
    assert_eq!(failed.kind, OutcomeKind::PreconditionFailed as u16);
}

// A head that does not answer the request is a status, not a sentence.
#[test]
fn an_invalid_head_carries_the_error_of_the_core_crate() {
    // SAFETY: every pointer addresses a live value of this test. A write
    // answers 201, never 200.
    let outcome = unsafe { borink_accept_put_head(write_shape(), 200, core::ptr::null(), 0) };
    assert_eq!(outcome.kind, OutcomeKind::Invalid as u16);
    assert_eq!(outcome.error.code, ErrorCode::Response as u16);
    assert_eq!(outcome.error.detail, ResponseFault::Status as u16);
    assert_eq!(
        text(&outcome),
        Error::Response(ResponseFault::Status).to_string()
    );
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
    let ours = compiled_layout();
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

// The entries of a page point into the body that the fill read, and the
// values of each one cross whole.
#[test]
fn a_page_crosses_as_entries_that_point_into_the_body() {
    let mut body = page(2, "next");
    let mut entries = [ListEntry::default(); 4];
    // SAFETY: the body and the array are live for the whole call, and nothing
    // else reaches them.
    let fill = unsafe { borink_fill_listing(writable(&mut body), entries.as_mut_ptr(), 4) };

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
    let mut body = page(40, "");
    let mut entries = [ListEntry::default(); 25];
    // SAFETY: the body and the array are live, and nothing else reaches them.
    let fill = unsafe { borink_fill_listing(writable(&mut body), entries.as_mut_ptr(), 25) };
    assert_eq!(fill.status.code, ErrorCode::Capacity as u16);
    assert_eq!(fill.required, 40);
    assert_eq!(fill.filled, 0);
    assert!(!fill.next_marker.present);
}

// An array with no room is refused the same way, unless the page is empty.
#[test]
fn an_array_with_no_room_is_refused_unless_the_page_is_empty() {
    let mut body = page(1, "next");
    // SAFETY: the body is live, and no array is passed.
    let fill = unsafe { borink_fill_listing(writable(&mut body), core::ptr::null_mut(), 0) };
    assert_eq!(fill.status.code, ErrorCode::Capacity as u16);
    assert_eq!(fill.required, 1);

    let mut body = page(0, "next");
    // SAFETY: as above.
    let fill = unsafe { borink_fill_listing(writable(&mut body), core::ptr::null_mut(), 0) };
    assert_eq!(fill.status, Status::default());
    assert_eq!(fill.filled, 0);
    assert!(fill.next_marker.present);
}

// A body that is not a page reports the fault of the protocol crate, and no
// entry of the array is reported.
#[test]
fn a_body_that_is_not_a_page_is_refused() {
    let mut body = Vec::from(b"<Error><Code>ServerBusy</Code></Error>".as_slice());
    let mut entries = [ListEntry::default(); 2];
    // SAFETY: the body and the array are live, and nothing else reaches them.
    let fill = unsafe { borink_fill_listing(writable(&mut body), entries.as_mut_ptr(), 2) };

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
// read is. A listing names no object, so a missing container is a failure.
#[test]
fn a_listing_failure_is_finished_by_the_body() {
    // SAFETY: the header bytes are static.
    let outcome = unsafe {
        let headers = [header("x-ms-request-id", IDENTIFIER)];
        borink_accept_list_head(404, headers.as_ptr(), 1)
    };
    assert_eq!(outcome.kind, OutcomeKind::NeedErrorBody as u16);

    let body = b"<Error><Code>ContainerNotFound</Code></Error>";
    // SAFETY: the failure and the body are live for the call.
    let finished = unsafe { borink_accept_list_error_body(outcome.failure, lent(body)) };
    assert_eq!(finished.kind, OutcomeKind::ServiceFailure as u16);
    assert_eq!(
        kind_of(finished.failure.kind),
        Some(ServiceErrorKind::NoSuchContainer)
    );
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
    let fill = unsafe { borink_fill_listing(writable(&mut body), entries.as_mut_ptr(), 1) };
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
            borink_decode_property(lent(b"a&amp;b"), writable(&mut room)),
            borink_decode_property(lent(b"a&amp;b"), writable(&mut cramped)),
            borink_decode_property(lent(b"a&nbsp;b"), writable(&mut third)),
        )
    };
    assert_eq!(borrowed(decoded), Some(b"a&b".as_slice()));
    // A buffer shorter than the value, and a reference that no listing
    // declares.
    assert!(!refused.present);
    assert!(!unknown.present);
}

/// The two lists of properties are built from one table, so the numbers and
/// the names agree. A number that names none is refused everywhere.
#[test]
fn the_properties_cross_by_number_and_name() {
    for (number, property) in proto::azure::BlobProperty::ALL.iter().enumerate() {
        let number = u16::try_from(number).unwrap();
        // SAFETY: the name is a static of this crate.
        let name = unsafe { slice(borink_property_name(number)) };
        assert_eq!(name, property.name().as_bytes());
        let set = borink_property_set_with(PropertySet::default(), number);
        assert_eq!(borink_property_set_len(set), 1);
        assert_eq!(borink_property_slot(set, number), 0);
    }
    let none = u16::try_from(proto::azure::BlobProperty::ALL.len()).unwrap();
    assert_eq!(
        borink_property_set_with(PropertySet::default(), none),
        PropertySet::default()
    );
    // SAFETY: as above.
    assert_eq!(unsafe { slice(borink_property_name(none)) }, b"");
    assert_eq!(borink_property_slot(PropertySet::default(), none), 0);
}

/// The values a program asks for are written into its rows as the page is
/// read. An array with fewer rows than entries is the capacity.
#[test]
fn the_values_a_program_asks_for_are_written_into_its_rows() {
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
