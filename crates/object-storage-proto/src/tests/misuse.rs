// Encoders trust that the bytes they are passed are their template's. These
// tests pass them other bytes: every other template's, their own cut short,
// and their own with bytes changed at random. Each encoder must write a
// wrong request or refuse it, and never panic. A request that one writes is
// read through every accessor, and an S3 request is signed, also into a
// buffer that is not its own.

extern crate std;

use std::boxed::Box;
use std::rc::Rc;
use std::vec;
use std::vec::Vec;

use crate::azure::{self, AzureNamespace, BlockRef, BlockSource, Container};
use crate::s3::{self, Bucket, PartRef, PayloadSigning, Service};
use crate::sigv4::{Credentials, wipe_best_effort};
use crate::{
    ChecksumKind, ChecksumValue, ConditionKind, ConditionValue, CopyItem, CopyOptions,
    DeleteTarget, Digest, GetItem, GetOptions, ListInclude, ListItem, ListMarker, ListOptions,
    MetadataPair, ObjectItem, Payload, Placement, PutItem, PutOptions, Ranging, RequestedRange,
    Result, Revision, RevisionKind, StageOptions, Tag, Timestamps,
};

const KEY: &str = "dir/file name.sst";
const NOW: u64 = 1_760_000_000;
const NS: AzureNamespace = AzureNamespace::Unknown;
const TOKEN: &str = "eyJ0eXAiOiJKV1QifQ.eyJzdWIiOiJ0ZXN0In0.c2lnbmF0dXJl";
const META: [MetadataPair<'static>; 1] = [MetadataPair {
    name: "kind",
    value: "sst",
}];
const TAGS: [Tag<'static>; 1] = [Tag {
    key: "tier",
    value: "hot",
}];
const RANGE: RequestedRange = RequestedRange::Bounded {
    start: 10,
    end: 4106,
};
const ETAG: Option<ConditionValue<'static>> = Some(ConditionValue::ETag(b"\"abc\""));
const CRC64: Option<ChecksumValue<'static>> = Some(ChecksumValue::Base64("AAAAAAAAAAA="));
const BLOCKS: [BlockRef<'static>; 2] = [
    BlockRef {
        id: "YmxvY2stMDAwMDA=",
        source: BlockSource::Latest,
    },
    BlockRef {
        id: "YmxvY2stMDAwMDE=",
        source: BlockSource::Committed,
    },
];
const PARTS: [PartRef<'static>; 2] = [
    PartRef {
        number: 1,
        e_tag: b"\"a\"",
    },
    PartRef {
        number: 2,
        e_tag: b"\"b\"",
    },
];
const TARGETS: [DeleteTarget<'static>; 2] = [
    DeleteTarget {
        key: KEY,
        revision: None,
    },
    DeleteTarget {
        key: "dir/other",
        revision: Some(Revision::Version("v1")),
    },
];
const SHA: [u8; 32] = [7; 32];

// Whether `Template::check` takes the bytes.
type Check = Box<dyn Fn(&[u8]) -> bool>;

// Writes a request from the given bytes into the given buffer, reads it, and
// returns whether it was written.
type Encode = Box<dyn Fn(&[u8], &mut [u8]) -> bool>;

// Writes a request from the given Azure template and bytes, as `Encode`
// does from its own template.
type AzureEncode = Rc<dyn Fn(&azure::Template, &[u8], &mut [u8]) -> bool>;

// One encoder and its template.
struct Case {
    name: &'static str,
    bytes: Vec<u8>,
    check: Check,
    encode: Encode,
    // An Azure case's template, and its encoder of any template.
    azure: Option<(azure::Template, AzureEncode)>,
}

fn now() -> Timestamps {
    Timestamps::from_unix(NOW)
}

// Reads every part of a placed request, and of the request it places in
// `buf`, and returns whether there is one.
fn read(placed: Result<Placement>, buf: &[u8]) -> bool {
    let written = placed.is_ok();
    if let Ok(placed) = placed {
        let _ = (
            placed.method(),
            placed.url_span(),
            placed.head_span(),
            placed.fields_span(),
            placed.body_span(),
            placed.content_len(),
        );
        let request = placed.request(buf);
        let _ = request.method();
        let _ = request.url().len();
        let _ = request.headers().count();
        let _ = request.head().len();
        let _ = request.payload();
    }
    written
}

// Reads a written S3 request and signs it, in its own buffer and in one
// that is not, and returns whether there is one.
fn sign(unsigned: Result<s3::Unsigned>, buf: &mut [u8]) -> bool {
    let Ok(unsigned) = unsigned else {
        return false;
    };
    let mut other = [0u8; 37];
    for buf in [&mut *buf, &mut other[..]] {
        let _ = (unsigned.canonical(buf).len(), unsigned.len());
        let _ = (unsigned.body(buf), unsigned.signs_body());
        s3::set_body_hash(&unsigned, buf, &SHA);
        let _ = s3::set_body_checksum(&unsigned, buf, &Digest::crc64(7));
        read(Ok(s3::set_signature(&unsigned, buf, &[9; 32])), buf);
    }
    true
}

// Writes the template of the case `name` with `prepare` into a buffer of
// its size.
fn written<T>(name: &str, prepare: impl FnOnce(&mut [u8]) -> Result<T>) -> (T, Vec<u8>) {
    let mut bytes = vec![0; 16 * 1024];
    match prepare(&mut bytes) {
        Ok(template) => (template, bytes),
        Err(error) => panic!("{name}: the template is refused: {error:?}"),
    }
}

fn azure(
    name: &'static str,
    object: bool,
    prepare: impl FnOnce(&Container<'_>, &mut [u8]) -> Result<azure::Template>,
    encode: impl Fn(&azure::Template, &[u8], &str, &mut [u8]) -> bool + 'static,
) -> Case {
    let container = Container::new("https://account.blob.core.windows.net", "data").unwrap();
    let container = if object {
        container.object(KEY)
    } else {
        container
    };
    let (template, bytes) = written(name, |buf| prepare(&container, buf));
    let key = if object { "" } else { KEY };
    let encode: AzureEncode =
        Rc::new(move |template, bytes, buf| encode(template, bytes, key, buf));
    Case {
        name,
        bytes,
        check: Box::new(move |bytes| template.check(bytes).is_ok()),
        encode: {
            let encode = encode.clone();
            Box::new(move |bytes, buf| encode(&template, bytes, buf))
        },
        azure: Some((template, encode)),
    }
}

fn s3(
    name: &'static str,
    object: bool,
    prepare: impl FnOnce(&Bucket<'_>, &Credentials<'_>, &mut [u8]) -> Result<s3::Template>,
    encode: impl Fn(&s3::Template, &[u8], &str, &mut [u8]) -> bool + 'static,
) -> Case {
    let bucket = Bucket::new(
        "https://s3.eu-west-1.amazonaws.com",
        "data",
        "eu-west-1",
        Service::Aws,
    )
    .unwrap();
    let bucket = if object { bucket.object(KEY) } else { bucket };
    let credentials = Credentials::new("AKIDEXAMPLE", "secret/key", wipe_best_effort).unwrap();
    let (template, bytes) = written(name, |buf| prepare(&bucket, &credentials, buf));
    let key = if object { "" } else { KEY };
    Case {
        name,
        bytes,
        check: Box::new(move |bytes| template.check(bytes).is_ok()),
        encode: Box::new(move |bytes, buf| {
            // The signing key and the string to sign read the template's
            // bytes too.
            let credentials =
                Credentials::new("AKIDEXAMPLE", "secret/key", wipe_best_effort).unwrap();
            let _ = s3::signing_key(&template, bytes, &credentials, &now(), |_, _| [1; 32]);
            let mut prefix = [0; s3::STRING_TO_SIGN_PREFIX_LEN];
            let _ = s3::string_to_sign_prefix(&template, bytes, &now(), &mut prefix);
            encode(&template, bytes, key, buf)
        }),
        azure: None,
    }
}

fn get_item(key: &str) -> GetItem<'_> {
    GetItem {
        range: RANGE,
        condition_value: ETAG,
        revision: Some("2024-01-01"),
        ..GetItem::new(key)
    }
}

fn put_item(key: &str) -> PutItem<'_> {
    PutItem {
        condition_value: Some(ConditionValue::ETag(b"*")),
        checksum: CRC64,
        ..PutItem::new(key, Payload::Streamed { len: 4096 })
    }
}

fn copy_item(key: &str) -> CopyItem<'_> {
    CopyItem {
        source_revision: Some("2024-01-01"),
        source_condition_value: ETAG,
        source_range: RANGE,
        ..CopyItem::new(key, "dir/source.sst")
    }
}

fn list_item() -> ListItem<'static> {
    ListItem {
        marker: Some(ListMarker::Text("next")),
        ..ListItem::new("dir/")
    }
}

fn get(revision: RevisionKind) -> GetOptions {
    GetOptions {
        ranging: Ranging::Ranged,
        condition: ConditionKind::IfMatch,
        revision,
        ..GetOptions::new()
    }
}

fn put() -> PutOptions<'static> {
    PutOptions {
        condition: ConditionKind::IfNoneMatch,
        metadata: &META,
        checksum: Some(ChecksumKind::Crc64),
        ..PutOptions::default()
    }
}

fn list(include: ListInclude) -> ListOptions<'static> {
    ListOptions {
        delimiter: Some("/"),
        max_results: Some(100),
        include,
    }
}

// A copy into a block or a part, from a ranged source.
fn copy(source_revision: RevisionKind) -> CopyOptions<'static> {
    CopyOptions {
        source_revision,
        source_condition: ConditionKind::IfMatch,
        source_ranging: Ranging::Ranged,
        ..CopyOptions::default()
    }
}

const STAGE: StageOptions = StageOptions {
    checksum: Some(ChecksumKind::Crc64),
};

fn azure_cases() -> Vec<Case> {
    let snapshot = RevisionKind::Snapshot;
    vec![
        azure(
            "azure get",
            false,
            move |c, buf| azure::prepare_get(c, TOKEN, NS, &get(snapshot), buf),
            |t, b, key, buf| read(azure::encode_get(t, b, &get_item(key), &now(), buf), buf),
        ),
        azure(
            "azure get of one object",
            true,
            move |c, buf| azure::prepare_get(c, TOKEN, NS, &get(snapshot), buf),
            |t, b, key, buf| read(azure::encode_get(t, b, &get_item(key), &now(), buf), buf),
        ),
        azure(
            "azure put",
            false,
            |c, buf| azure::prepare_put(c, TOKEN, NS, &put(), buf),
            |t, b, key, buf| read(azure::encode_put(t, b, &put_item(key), &now(), buf), buf),
        ),
        azure(
            "azure list",
            false,
            |c, buf| azure::prepare_list(c, TOKEN, NS, &list(ListInclude::METADATA), buf),
            |t, b, _, buf| read(azure::encode_list(t, b, &list_item(), &now(), buf), buf),
        ),
        azure(
            "azure stage",
            false,
            |c, buf| azure::prepare_stage_block(c, TOKEN, NS, &STAGE, buf),
            |t, b, key, buf| {
                let item = azure::StageBlockItem {
                    checksum: CRC64,
                    ..azure::StageBlockItem::new(key, BLOCKS[0].id, Payload::Streamed { len: 8 })
                };
                read(azure::encode_stage_block(t, b, &item, &now(), buf), buf)
            },
        ),
        azure(
            "azure commit",
            false,
            |c, buf| {
                let options = azure::CommitOptions {
                    condition: ConditionKind::IfMatch,
                    metadata: &META,
                    content_md5: true,
                    ..azure::CommitOptions::default()
                };
                azure::prepare_commit_blocks(c, TOKEN, NS, &options, buf)
            },
            |t, b, key, buf| {
                let item = azure::CommitItem {
                    condition_value: ETAG,
                    content_md5: Some("1B2M2Y8AsgTpgAmY7PhCfg=="),
                    ..azure::CommitItem::new(key, &BLOCKS)
                };
                read(azure::encode_commit_blocks(t, b, &item, &now(), buf), buf)
            },
        ),
        azure(
            "azure stage from URL",
            false,
            move |c, buf| azure::prepare_stage_block_from_url(c, TOKEN, NS, &copy(snapshot), buf),
            |t, b, key, buf| {
                let item = azure::StageBlockFromUrlItem {
                    copy: copy_item(key),
                    id: BLOCKS[0].id,
                };
                read(
                    azure::encode_stage_block_from_url(t, b, &item, &now(), buf),
                    buf,
                )
            },
        ),
        azure(
            "azure abort copy",
            false,
            |c, buf| azure::prepare_abort_copy(c, TOKEN, NS, buf),
            |t, b, key, buf| {
                let item = azure::AbortCopyItem {
                    key,
                    copy_id: "copy-1",
                };
                read(azure::encode_abort_copy(t, b, &item, &now(), buf), buf)
            },
        ),
        azure(
            "azure set tags",
            false,
            |c, buf| azure::prepare_set_tags(c, TOKEN, NS, &TAGS, RevisionKind::Version, buf),
            |t, b, key, buf| {
                let item = ObjectItem {
                    revision: Some("v1"),
                    ..ObjectItem::new(key)
                };
                read(azure::encode_set_tags(t, b, &item, &now(), buf), buf)
            },
        ),
        azure(
            "azure delete many",
            false,
            |c, buf| azure::prepare_delete_many(c, TOKEN, NS, buf),
            |t, b, _, buf| read(azure::encode_delete_many(t, b, &TARGETS, &now(), buf), buf),
        ),
    ]
}

// A read of a version: S3 names versions, and no snapshots.
fn versioned(key: &str) -> GetItem<'_> {
    GetItem {
        revision: Some("v1"),
        ..get_item(key)
    }
}

fn s3_cases() -> Vec<Case> {
    let version = RevisionKind::Version;
    vec![
        s3(
            "s3 get",
            false,
            move |b, c, buf| s3::prepare_get(b, c, &get(version), buf),
            move |t, b, key, buf| sign(s3::encode_get(t, b, &versioned(key), &now(), buf), buf),
        ),
        s3(
            "s3 get of one object",
            true,
            move |b, c, buf| s3::prepare_get(b, c, &get(version), buf),
            move |t, b, key, buf| sign(s3::encode_get(t, b, &versioned(key), &now(), buf), buf),
        ),
        s3(
            "s3 put",
            false,
            |b, c, buf| s3::prepare_put(b, c, &put(), PayloadSigning::Signed, buf),
            |t, b, key, buf| {
                sign(
                    s3::encode_put(t, b, &put_item(key), Some(&SHA), &now(), buf),
                    buf,
                )
            },
        ),
        s3(
            "s3 list",
            false,
            |b, c, buf| s3::prepare_list(b, c, &list(ListInclude::OWNER), buf),
            |t, b, _, buf| {
                let item = ListItem {
                    start_after: Some("dir/0"),
                    ..list_item()
                };
                sign(s3::encode_list(t, b, &item, &now(), buf), buf)
            },
        ),
        s3(
            "s3 stage",
            false,
            |b, c, buf| s3::prepare_stage_part(b, c, &STAGE, PayloadSigning::Signed, buf),
            |t, b, key, buf| {
                let item = s3::StagePartItem {
                    checksum: CRC64,
                    ..s3::StagePartItem::new(key, "upload-1", 3, Payload::Streamed { len: 8 })
                };
                sign(
                    s3::encode_stage_part(t, b, &item, Some(&SHA), &now(), buf),
                    buf,
                )
            },
        ),
        s3(
            "s3 commit",
            false,
            |b, c, buf| {
                let options = s3::CommitOptions {
                    condition: ConditionKind::IfNoneMatch,
                    checksum: Some(ChecksumKind::Crc64),
                    sized: true,
                };
                s3::prepare_commit_parts(b, c, &options, buf)
            },
            |t, b, key, buf| {
                let item = s3::CommitItem {
                    condition_value: Some(ConditionValue::ETag(b"*")),
                    size: Some(1 << 20),
                    ..s3::CommitItem::new(key, "upload-1", &PARTS)
                };
                sign(s3::encode_commit_parts(t, b, &item, &now(), buf), buf)
            },
        ),
        s3(
            "s3 stage from a copy",
            false,
            move |b, c, buf| s3::prepare_stage_part_copy(b, c, &copy(version), buf),
            |t, b, key, buf| {
                let item = s3::StagePartCopyItem {
                    copy: CopyItem {
                        source_revision: Some("v1"),
                        ..copy_item(key)
                    },
                    upload_id: "upload-1",
                    number: 2,
                };
                sign(s3::encode_stage_part_copy(t, b, &item, &now(), buf), buf)
            },
        ),
        s3(
            "s3 delete many",
            false,
            |b, c, buf| s3::prepare_delete_many(b, c, Some(ChecksumKind::Crc64), buf),
            |t, b, _, buf| sign(s3::encode_delete_many(t, b, &TARGETS, &now(), buf), buf),
        ),
    ]
}

fn cases() -> Vec<Case> {
    let mut cases = azure_cases();
    cases.extend(s3_cases());
    cases
}

// A generator of the bytes to change, from a fixed seed.
struct XorShift(u64);

impl XorShift {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound.max(1) as u64) as usize
    }
}

// The bytes that `case` is fed besides its own: every other template's, its
// own cut short, its own with one to four bytes changed, and noise.
fn variants(case: &Case, all: &[Case], random: &mut XorShift) -> Vec<Vec<u8>> {
    let own = &case.bytes;
    // The template's text and table end about where the zeros its buffer
    // ends in start: the table's last byte is a zero too.
    let used = own
        .iter()
        .rposition(|&byte| byte != 0)
        .map_or(0, |end| end + 1);
    let mut variants: Vec<Vec<u8>> = all.iter().map(|other| other.bytes.clone()).collect();
    for cut in [
        0,
        1,
        7,
        8,
        9,
        used / 4,
        used / 2,
        used.saturating_sub(9),
        used.saturating_sub(1),
    ] {
        variants.push(own[..cut].to_vec());
    }
    for _ in 0..300 {
        let mut changed = own.clone();
        for _ in 0..=random.below(4) {
            let at = random.below(used + 8);
            if let Some(byte) = changed.get_mut(at) {
                // Mostly small changes, which keep a length plausible.
                *byte = match random.below(3) {
                    0 => *byte ^ 1,
                    1 => *byte ^ 0x80,
                    _ => random.next() as u8,
                };
            }
        }
        variants.push(changed);
    }
    for len in [64, used] {
        variants.push((0..len).map(|_| random.next() as u8).collect());
    }
    variants
}

#[test]
fn encoders_never_panic_on_bytes_that_are_not_their_templates() {
    let cases = cases();
    let mut random = XorShift(0x9E37_79B9_7F4A_7C15);
    let mut big = vec![0u8; 64 * 1024];
    let mut small = vec![0u8; 100];
    for case in &cases {
        assert!(
            (case.encode)(&case.bytes, &mut big),
            "{} writes no request from its own bytes",
            case.name
        );
        for bytes in variants(case, &cases, &mut random) {
            (case.encode)(&bytes, &mut big);
            (case.encode)(&bytes, &mut small);
        }
    }
}

#[test]
fn a_template_checks_its_own_bytes_and_no_others() {
    let cases = cases();
    for case in &cases {
        assert!(
            (case.check)(&case.bytes),
            "{} refuses its own bytes",
            case.name
        );
        // One changed byte anywhere in the text or the table.
        let used = case
            .bytes
            .iter()
            .rposition(|&byte| byte != 0)
            .map_or(0, |end| end + 1);
        for at in (0..used).step_by(7) {
            let mut changed = case.bytes.clone();
            changed[at] ^= 0x20;
            assert!(
                !(case.check)(&changed),
                "{} takes a changed byte",
                case.name
            );
        }
        for other in &cases {
            if other.bytes != case.bytes {
                assert!(
                    !(case.check)(&other.bytes),
                    "{} takes the bytes of {}",
                    case.name,
                    other.name
                );
            }
        }
    }
}

#[test]
fn a_stored_template_reads_back_as_itself_and_a_changed_one_is_refused() {
    let cases = cases();
    let azure: Vec<_> = cases
        .iter()
        .filter_map(|case| case.azure.clone().map(|azure| (case, azure)))
        .collect();
    let mut random = XorShift(0x2545_F491_4F6C_DD1D);
    let mut first = vec![0u8; 64 * 1024];
    let mut again = vec![0u8; 64 * 1024];
    for (case, (template, encode)) in &azure {
        let stored = template.to_bytes();
        let read = azure::Template::from_bytes(&stored)
            .unwrap_or_else(|error| panic!("{}: refused when read back: {error:?}", case.name));
        assert_eq!(
            read.to_bytes(),
            stored,
            "{} reads back as another",
            case.name
        );
        assert_eq!(read.bytes_len(), template.bytes_len(), "{}", case.name);
        assert!(read.check(&case.bytes).is_ok(), "{}", case.name);
        first.fill(0);
        again.fill(0);
        assert!(encode(template, &case.bytes, &mut first), "{}", case.name);
        assert!(encode(&read, &case.bytes, &mut again), "{}", case.name);
        assert!(
            first == again,
            "{} writes another request once read back",
            case.name
        );
        // A change anywhere, to a value or to the hash that ends them, is
        // refused.
        for _ in 0..400 {
            let mut changed = stored;
            for _ in 0..=random.below(3) {
                let at = random.below(stored.len());
                changed[at] = match random.below(3) {
                    0 => changed[at] ^ 1,
                    1 => changed[at].wrapping_add(1),
                    _ => random.next() as u8,
                };
            }
            assert!(
                changed == stored || azure::Template::from_bytes(&changed).is_err(),
                "{} reads back changed",
                case.name
            );
        }
    }
    let noise: [u8; azure::Template::STORED_LEN] = core::array::from_fn(|_| random.next() as u8);
    assert!(azure::Template::from_bytes(&noise).is_err());
}
