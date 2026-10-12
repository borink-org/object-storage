// Every operation's layout, run as a request's pass, writes what walking its
// template's table writes: `template::write_request` checks that for each
// request it writes in a test. These tests write one or more requests of
// every operation on both services, so that the check covers each layout.
// A template of one object writes the same request as a template of every
// object does for that object's key.

extern crate std;

use std::vec;
use std::vec::Vec;

use crate::azure::{self, BlockListKind, BlockRef, BlockSource, Container};
use crate::s3::{self, Bucket, PartRef, PayloadSigning, Service};
use crate::sigv4::{Credentials, wipe_best_effort};
use crate::{
    AzureNamespace, ChecksumKind, ChecksumValue, ConditionKind, ConditionValue, ContentProperties,
    CopyItem, CopyOptions, DeleteKind, DeleteOptions, DeleteTarget, GetItem, GetKind, GetOptions,
    ListInclude, ListItem, ListMarker, ListOptions, MetadataPair, ObjectItem, Payload, PutItem,
    PutOptions, Ranging, RequestedRange, RestoreOptions, RestorePriority, Result, RevisionKind,
    StageOptions, Tag, Timestamps, WriteOptions,
};

const KEY: &str = "dir/file name.sst";
const SOURCE: &str = "dir/source.sst";
const NOW: u64 = 1_760_000_000;
const MD5: &str = "1B2M2Y8AsgTpgAmY7PhCfg==";
const CRC64: &str = "AAAAAAAAAAA=";
const META: [MetadataPair<'static>; 2] = [
    MetadataPair {
        name: "Owner",
        value: "team a",
    },
    MetadataPair {
        name: "kind",
        value: "sst",
    },
];
const TAGS: [Tag<'static>; 1] = [Tag {
    key: "tier",
    value: "hot",
}];

fn stored() -> WriteOptions<'static> {
    WriteOptions {
        properties: ContentProperties {
            content_type: Some("application/octet-stream"),
            ..ContentProperties::default()
        },
        ..WriteOptions::default()
    }
}

// Writes a template with `prepare` into a buffer of its size.
fn written<T>(prepare: impl Fn(&mut [u8]) -> Result<T>) -> (T, Vec<u8>) {
    let mut bytes = vec![0; 16 * 1024];
    let template =
        prepare(&mut bytes).unwrap_or_else(|error| panic!("the template is refused: {error:?}"));
    (template, bytes)
}

// Writes an Azure template from a container of every object and from one of
// `KEY`, writes a request of each with `encode`, which takes the item's key,
// and checks that they are the same.
fn azure_both(
    prepare: impl Fn(&Container<'_>, &mut [u8]) -> Result<azure::Template>,
    encode: impl Fn(&azure::Template, &[u8], &str) -> Vec<u8>,
) {
    let container = Container::new("https://account.blob.core.windows.net", "data").unwrap();
    let (each, each_bytes) = written(|buf| prepare(&container, buf));
    let request = encode(&each, &each_bytes, KEY);
    let one = container.object(KEY);
    let (fixed, fixed_bytes) = written(|buf| prepare(&one, buf));
    assert_eq!(
        encode(&fixed, &fixed_bytes, ""),
        request,
        "a template of one object writes alike"
    );
    assert_eq!(encode(&fixed, &fixed_bytes, KEY), request);
}

// Writes an Azure template of an operation that names no object.
fn azure_one(
    prepare: impl Fn(&Container<'_>, &mut [u8]) -> Result<azure::Template>,
    encode: impl Fn(&azure::Template, &[u8], &mut [u8]),
) {
    let container = Container::new("https://account.blob.core.windows.net", "data").unwrap();
    let (template, bytes) = written(|buf| prepare(&container, buf));
    let mut buf = vec![0; 64 * 1024];
    encode(&template, &bytes, &mut buf);
}

// The URL, the head and the content of an Azure request.
fn azure_request(request: Result<crate::WireRequest<'_>>) -> Vec<u8> {
    let request = request.expect("the request is written");
    let mut text = request.url().as_bytes().to_vec();
    text.extend_from_slice(request.head());
    match request.payload() {
        Payload::Slice(body) => text.extend_from_slice(body),
        Payload::Streamed { len } => text.extend_from_slice(&len.to_le_bytes()),
    }
    text
}

fn token() -> &'static str {
    "eyJ0eXAiOiJKV1QifQ.eyJzdWIiOiJ0ZXN0In0.c2lnbmF0dXJl"
}

const NS: AzureNamespace = AzureNamespace::Unknown;

#[test]
fn every_azure_layout_writes_what_its_table_writes() {
    let now = Timestamps::from_unix(NOW);
    let range = RequestedRange::Bounded {
        start: 10,
        end: 4106,
    };
    let etag = Some(ConditionValue::ETag(b"\"abc\""));
    for kind in [GetKind::Bytes, GetKind::Head] {
        for ranging in [Ranging::Whole, Ranging::Ranged] {
            for (condition, revision) in [
                (ConditionKind::None, RevisionKind::Current),
                (ConditionKind::IfMatch, RevisionKind::Snapshot),
                (ConditionKind::IfNoneMatch, RevisionKind::Version),
            ] {
                if kind == GetKind::Head && ranging == Ranging::Ranged {
                    continue;
                }
                let options = GetOptions {
                    kind,
                    ranging,
                    condition,
                    revision,
                };
                azure_both(
                    |container, buf| azure::prepare_get(container, token(), NS, &options, buf),
                    |template, bytes, key| {
                        let mut buf = vec![0; 4096];
                        let item = GetItem {
                            range: if ranging == Ranging::Ranged {
                                range
                            } else {
                                RequestedRange::Whole
                            },
                            condition_value: etag.filter(|_| condition != ConditionKind::None),
                            revision: (revision != RevisionKind::Current).then_some("2024-01-01"),
                            ..GetItem::new(key)
                        };
                        azure_request(azure::encode_get(template, bytes, &item, &now, &mut buf))
                    },
                );
            }
        }
    }
    for checksum in [None, Some(ChecksumKind::Md5), Some(ChecksumKind::Crc64)] {
        let options = PutOptions {
            condition: ConditionKind::IfNoneMatch,
            metadata: &META,
            checksum,
            options: stored(),
        };
        azure_both(
            |container, buf| azure::prepare_put(container, token(), NS, &options, buf),
            |template, bytes, key| {
                let mut buf = vec![0; 4096];
                let item = PutItem {
                    condition_value: Some(ConditionValue::ETag(b"*")),
                    checksum: checksum.map(|kind| {
                        ChecksumValue::Base64(if kind == ChecksumKind::Md5 {
                            MD5
                        } else {
                            CRC64
                        })
                    }),
                    ..PutItem::new(key, Payload::Streamed { len: 4096 })
                };
                let request = azure::encode_put(template, bytes, &item, &now, &mut buf);
                // A write sends the content it was given.
                assert!(matches!(
                    request.as_ref().map(crate::WireRequest::payload),
                    Ok(Payload::Streamed { len: 4096 })
                ));
                azure_request(request)
            },
        );
    }
    let object = |template: &azure::Template, bytes: &[u8], key: &str, revision: bool| {
        let mut buf = vec![0; 4096];
        let item = ObjectItem {
            revision: revision.then_some("2024-01-01"),
            ..ObjectItem::new(key)
        };
        let encode = match template.operation() {
            azure::Operation::Delete => azure::encode_delete,
            azure::Operation::SetTier => azure::encode_set_tier,
            azure::Operation::SetMetadata => azure::encode_set_metadata,
            azure::Operation::SetProperties => azure::encode_set_properties,
            azure::Operation::SetTags => azure::encode_set_tags,
            azure::Operation::GetTags => azure::encode_get_tags,
            azure::Operation::Snapshot => azure::encode_snapshot,
            azure::Operation::ListBlocks => azure::encode_list_blocks,
            _ => azure::encode_restore,
        };
        azure_request(encode(template, bytes, &item, &now, &mut buf))
    };
    for revision in [RevisionKind::Current, RevisionKind::Version] {
        let named = revision != RevisionKind::Current;
        let delete = DeleteOptions {
            kind: if named {
                DeleteKind::Object
            } else {
                DeleteKind::ObjectAndSnapshots
            },
            revision,
            ..DeleteOptions::default()
        };
        azure_both(
            |c, buf| azure::prepare_delete(c, token(), NS, &delete, buf),
            |t, b, k| object(t, b, k, named),
        );
        azure_both(
            |c, buf| azure::prepare_set_tags(c, token(), NS, &TAGS, revision, buf),
            |t, b, k| object(t, b, k, named),
        );
        azure_both(
            |c, buf| azure::prepare_get_tags(c, token(), NS, revision, buf),
            |t, b, k| object(t, b, k, named),
        );
        azure_both(
            |c, buf| azure::prepare_list_blocks(c, token(), NS, BlockListKind::All, revision, buf),
            |t, b, k| object(t, b, k, named),
        );
        let restore = RestoreOptions {
            priority: RestorePriority::High,
            tier: Some("Hot"),
            revision,
            ..RestoreOptions::default()
        };
        azure_both(
            |c, buf| azure::prepare_restore(c, token(), NS, &restore, buf),
            |t, b, k| object(t, b, k, named),
        );
    }
    azure_both(
        |c, buf| azure::prepare_set_tier(c, token(), NS, "Cool", buf),
        |t, b, k| object(t, b, k, false),
    );
    azure_both(
        |c, buf| {
            let options = azure::SetMetadataOptions {
                metadata: &META,
                condition: ConditionKind::None,
            };
            azure::prepare_set_metadata(c, token(), NS, &options, buf)
        },
        |t, b, k| object(t, b, k, false),
    );
    azure_both(
        |c, buf| {
            let options = azure::SetPropertiesOptions {
                properties: stored().properties,
                condition: ConditionKind::None,
            };
            azure::prepare_set_properties(c, token(), NS, &options, buf)
        },
        |t, b, k| object(t, b, k, false),
    );
    azure_both(
        |c, buf| {
            let options = azure::SnapshotOptions {
                metadata: &META,
                condition: ConditionKind::None,
            };
            azure::prepare_snapshot(c, token(), NS, &options, buf)
        },
        |t, b, k| object(t, b, k, false),
    );
    for checksum in [None, Some(ChecksumKind::Crc64)] {
        azure_both(
            |c, buf| azure::prepare_stage_block(c, token(), NS, &StageOptions { checksum }, buf),
            |template, bytes, key| {
                let mut buf = vec![0; 4096];
                let item = azure::StageBlockItem {
                    checksum: checksum.map(|_| ChecksumValue::Base64(CRC64)),
                    ..azure::StageBlockItem::new(
                        key,
                        "YmxvY2stMDAwMDA=",
                        Payload::Streamed { len: 8 },
                    )
                };
                azure_request(azure::encode_stage_block(
                    template, bytes, &item, &now, &mut buf,
                ))
            },
        );
    }
    let blocks = [
        BlockRef {
            id: "YmxvY2stMDAwMDA=",
            source: BlockSource::Latest,
        },
        BlockRef {
            id: "YmxvY2stMDAwMDE=",
            source: BlockSource::Committed,
        },
    ];
    for content_md5 in [false, true] {
        let options = azure::CommitOptions {
            condition: ConditionKind::IfMatch,
            metadata: &META,
            options: stored(),
            content_md5,
        };
        azure_both(
            |c, buf| azure::prepare_commit_blocks(c, token(), NS, &options, buf),
            |template, bytes, key| {
                let mut buf = vec![0; 4096];
                let item = azure::CommitItem {
                    condition_value: etag,
                    content_md5: content_md5.then_some(MD5),
                    ..azure::CommitItem::new(key, &blocks)
                };
                azure_request(azure::encode_commit_blocks(
                    template, bytes, &item, &now, &mut buf,
                ))
            },
        );
    }
    for kind in 0..4 {
        let staged = kind == 3;
        for (source_revision, source_ranging) in [
            (RevisionKind::Current, Ranging::Whole),
            (RevisionKind::Snapshot, Ranging::Ranged),
        ] {
            if source_ranging == Ranging::Ranged && !staged {
                continue;
            }
            let options = CopyOptions {
                source_revision,
                source_condition: ConditionKind::IfMatch,
                source_ranging,
                // A block is no object, and takes no condition of its own.
                condition: if staged {
                    ConditionKind::None
                } else {
                    ConditionKind::IfNoneMatch
                },
                metadata: (!staged).then_some(&META[..]),
                ..CopyOptions::default()
            };
            azure_both(
                |c, buf| match kind {
                    0 => azure::prepare_copy(c, token(), NS, &options, buf),
                    1 => azure::prepare_copy_from_url(c, token(), NS, &options, buf),
                    2 => azure::prepare_put_from_url(c, token(), NS, &options, buf),
                    _ => azure::prepare_stage_block_from_url(c, token(), NS, &options, buf),
                },
                |template, bytes, key| {
                    let mut buf = vec![0; 4096];
                    let copy = CopyItem {
                        source_revision: (source_revision != RevisionKind::Current)
                            .then_some("2024-01-01"),
                        condition_value: (!staged).then_some(ConditionValue::ETag(b"*")),
                        source_condition_value: etag,
                        source_range: if source_ranging == Ranging::Ranged {
                            range
                        } else {
                            RequestedRange::Whole
                        },
                        ..CopyItem::new(key, SOURCE)
                    };
                    let request = match template.operation() {
                        azure::Operation::Copy => {
                            azure::encode_copy(template, bytes, &copy, &now, &mut buf)
                        }
                        azure::Operation::CopyFromUrl => {
                            azure::encode_copy_from_url(template, bytes, &copy, &now, &mut buf)
                        }
                        azure::Operation::PutFromUrl => {
                            azure::encode_put_from_url(template, bytes, &copy, &now, &mut buf)
                        }
                        _ => {
                            let item = azure::StageBlockFromUrlItem {
                                copy,
                                id: "YmxvY2stMDAwMDA=",
                            };
                            azure::encode_stage_block_from_url(
                                template, bytes, &item, &now, &mut buf,
                            )
                        }
                    };
                    azure_request(request)
                },
            );
        }
    }
    azure_both(
        |c, buf| azure::prepare_abort_copy(c, token(), NS, buf),
        |template, bytes, key| {
            let mut buf = vec![0; 4096];
            let item = azure::AbortCopyItem {
                key,
                copy_id: "copy-1",
            };
            azure_request(azure::encode_abort_copy(
                template, bytes, &item, &now, &mut buf,
            ))
        },
    );
    let targets = [
        DeleteTarget {
            key: KEY,
            revision: None,
        },
        DeleteTarget {
            key: SOURCE,
            revision: None,
        },
    ];
    azure_one(
        |c, buf| azure::prepare_delete_many(c, token(), NS, buf),
        |template, bytes, buf| {
            azure::encode_delete_many(template, bytes, &targets, &now, buf).unwrap();
        },
    );
    for options in [
        ListOptions::default(),
        ListOptions {
            delimiter: Some("/"),
            max_results: Some(100),
            include: ListInclude::METADATA | ListInclude::SNAPSHOTS,
        },
    ] {
        azure_one(
            |c, buf| azure::prepare_list(c, token(), NS, &options, buf),
            |template, bytes, buf| {
                for item in [
                    ListItem::new(""),
                    ListItem {
                        marker: Some(ListMarker::Text("next")),
                        ..ListItem::new("dir/")
                    },
                ] {
                    azure::encode_list(template, bytes, &item, &now, buf).unwrap();
                }
            },
        );
    }
}

// Writes an S3 template from a bucket of every object and from one of
// `KEY`, writes a request of each with `encode`, and checks that they are
// the same.
fn s3_both(
    service: Service,
    prepare: impl Fn(&Bucket<'_>, &Credentials<'_>, &mut [u8]) -> Result<s3::Template>,
    encode: impl Fn(&s3::Template, &[u8], &str, &mut [u8]) -> Result<usize>,
) {
    let (bucket, credentials) = s3_bucket(service);
    let request = |template: &s3::Template, bytes: &[u8], key: &str| {
        let mut buf = vec![0; 8192];
        let len = encode(template, bytes, key, &mut buf).expect("the request is written");
        buf.truncate(len);
        buf
    };
    let (each, each_bytes) = written(|buf| prepare(&bucket, &credentials, buf));
    let expected = request(&each, &each_bytes, KEY);
    let one = bucket.object(KEY);
    let (fixed, fixed_bytes) = written(|buf| prepare(&one, &credentials, buf));
    assert_eq!(
        request(&fixed, &fixed_bytes, ""),
        expected,
        "a template of one object writes alike"
    );
    assert_eq!(request(&fixed, &fixed_bytes, KEY), expected);
}

fn s3_bucket(service: Service) -> (Bucket<'static>, Credentials<'static>) {
    let bucket = match service {
        Service::AwsDirectory => Bucket::new(
            "https://s3express-use1-az4.us-east-1.amazonaws.com",
            "data--use1-az4--x-s3",
            "us-east-1",
            service,
        ),
        _ => Bucket::new(
            "https://s3.eu-west-1.amazonaws.com",
            "data",
            "eu-west-1",
            service,
        ),
    }
    .unwrap();
    let credentials = Credentials::new("AKIDEXAMPLE", "secret/key", wipe_best_effort).unwrap();
    (bucket, credentials)
}

#[test]
fn every_s3_layout_writes_what_its_table_writes() {
    let now = Timestamps::from_unix(NOW);
    let range = RequestedRange::Bounded {
        start: 10,
        end: 4106,
    };
    let etag = Some(ConditionValue::ETag(b"\"abc\""));
    let aws = Service::Aws;
    for ranging in [Ranging::Whole, Ranging::Ranged] {
        for (condition, revision) in [
            (ConditionKind::None, RevisionKind::Current),
            (ConditionKind::IfMatch, RevisionKind::Version),
        ] {
            let options = GetOptions {
                ranging,
                condition,
                revision,
                ..GetOptions::new()
            };
            s3_both(
                aws,
                |b, c, buf| s3::prepare_get(b, c, &options, buf),
                |template, bytes, key, buf| {
                    let item = GetItem {
                        range: if ranging == Ranging::Ranged {
                            range
                        } else {
                            RequestedRange::Whole
                        },
                        condition_value: etag.filter(|_| condition != ConditionKind::None),
                        revision: (revision != RevisionKind::Current).then_some("v1"),
                        ..GetItem::new(key)
                    };
                    len(s3::encode_get(template, bytes, &item, &now, buf))
                },
            );
        }
    }
    for (checksum, signing) in [
        (None, PayloadSigning::Unsigned),
        (Some(ChecksumKind::Crc64), PayloadSigning::Signed),
        (Some(ChecksumKind::Md5), PayloadSigning::Unsigned),
    ] {
        let options = PutOptions {
            condition: ConditionKind::IfNoneMatch,
            metadata: &META,
            checksum,
            options: stored(),
        };
        let digest = [7u8; 32];
        s3_both(
            aws,
            |b, c, buf| s3::prepare_put(b, c, &options, signing, buf),
            |template, bytes, key, buf| {
                let item = PutItem {
                    condition_value: Some(ConditionValue::ETag(b"*")),
                    checksum: checksum.map(|kind| {
                        ChecksumValue::Base64(if kind == ChecksumKind::Md5 {
                            MD5
                        } else {
                            CRC64
                        })
                    }),
                    ..PutItem::new(key, Payload::Streamed { len: 4096 })
                };
                let sha = (signing == PayloadSigning::Signed).then_some(&digest);
                len(s3::encode_put(template, bytes, &item, sha, &now, buf))
            },
        );
    }
    let object =
        |template: &s3::Template, bytes: &[u8], key: &str, buf: &mut [u8], version: bool| {
            let item = ObjectItem {
                revision: version.then_some("v1"),
                ..ObjectItem::new(key)
            };
            let encode = match template.operation() {
                s3::Operation::Delete => s3::encode_delete,
                s3::Operation::GetTagging => s3::encode_get_tagging,
                s3::Operation::PutTagging => s3::encode_put_tagging,
                s3::Operation::Restore => s3::encode_restore,
                _ => s3::encode_create_upload,
            };
            len(encode(template, bytes, &item, &now, buf))
        };
    for revision in [RevisionKind::Current, RevisionKind::Version] {
        let version = revision == RevisionKind::Version;
        let delete = DeleteOptions {
            revision,
            ..DeleteOptions::default()
        };
        s3_both(
            aws,
            |b, c, buf| s3::prepare_delete(b, c, &delete, buf),
            |t, b, k, buf| object(t, b, k, buf, version),
        );
        s3_both(
            aws,
            |b, c, buf| s3::prepare_get_tagging(b, c, revision, buf),
            |t, b, k, buf| object(t, b, k, buf, version),
        );
        for checksum in [None, Some(ChecksumKind::Crc64), Some(ChecksumKind::Md5)] {
            s3_both(
                aws,
                |b, c, buf| s3::prepare_put_tagging(b, c, &TAGS, revision, checksum, buf),
                |t, b, k, buf| object(t, b, k, buf, version),
            );
            let restore = RestoreOptions {
                days: Some(3),
                revision,
                checksum,
                ..RestoreOptions::default()
            };
            s3_both(
                aws,
                |b, c, buf| s3::prepare_restore(b, c, &restore, buf),
                |t, b, k, buf| object(t, b, k, buf, version),
            );
        }
    }
    s3_both(
        aws,
        |b, c, buf| {
            let options = s3::CreateUploadOptions {
                metadata: &META,
                options: stored(),
            };
            s3::prepare_create_upload(b, c, &options, buf)
        },
        |t, b, k, buf| object(t, b, k, buf, false),
    );
    for (checksum, signing) in [
        (None, PayloadSigning::Unsigned),
        (Some(ChecksumKind::Crc64), PayloadSigning::Signed),
    ] {
        let digest = [7u8; 32];
        s3_both(
            aws,
            |b, c, buf| s3::prepare_stage_part(b, c, &StageOptions { checksum }, signing, buf),
            |template, bytes, key, buf| {
                let item = s3::StagePartItem {
                    checksum: checksum.map(|_| ChecksumValue::Base64(CRC64)),
                    ..s3::StagePartItem::new(key, "upload-1", 3, Payload::Streamed { len: 8 })
                };
                let sha = (signing == PayloadSigning::Signed).then_some(&digest);
                len(s3::encode_stage_part(
                    template, bytes, &item, sha, &now, buf,
                ))
            },
        );
    }
    let parts = [
        PartRef {
            number: 1,
            e_tag: b"\"a\"",
        },
        PartRef {
            number: 2,
            e_tag: b"\"b\"",
        },
    ];
    for (checksum, sized) in [(None, false), (Some(ChecksumKind::Crc64), true)] {
        let options = s3::CommitOptions {
            condition: ConditionKind::IfNoneMatch,
            checksum,
            sized,
        };
        s3_both(
            aws,
            |b, c, buf| s3::prepare_commit_parts(b, c, &options, buf),
            |template, bytes, key, buf| {
                let item = s3::CommitItem {
                    condition_value: Some(ConditionValue::ETag(b"*")),
                    size: sized.then_some(1 << 20),
                    ..s3::CommitItem::new(key, "upload-1", &parts)
                };
                len(s3::encode_commit_parts(template, bytes, &item, &now, buf))
            },
        );
    }
    s3_both(
        aws,
        s3::prepare_abort_upload,
        |template, bytes, key, buf| {
            let item = s3::UploadItem {
                key,
                upload_id: "upload-1",
            };
            len(s3::encode_abort_upload(template, bytes, &item, &now, buf))
        },
    );
    for max_parts in [None, Some(50)] {
        s3_both(
            aws,
            |b, c, buf| s3::prepare_list_parts(b, c, max_parts, buf),
            |template, bytes, key, buf| {
                let item = s3::ListPartsItem {
                    key,
                    upload_id: "upload-1",
                    marker: Some("3"),
                };
                len(s3::encode_list_parts(template, bytes, &item, &now, buf))
            },
        );
    }
    for (staged, source_revision, source_ranging) in [
        (false, RevisionKind::Current, Ranging::Whole),
        (false, RevisionKind::Version, Ranging::Whole),
        (true, RevisionKind::Version, Ranging::Ranged),
    ] {
        let options = CopyOptions {
            source_revision,
            source_condition: ConditionKind::IfMatch,
            source_ranging,
            condition: if staged {
                ConditionKind::None
            } else {
                ConditionKind::IfNoneMatch
            },
            metadata: (!staged).then_some(&META[..]),
            tags: (!staged).then_some(&TAGS[..]),
            ..CopyOptions::default()
        };
        s3_both(
            aws,
            |b, c, buf| match staged {
                false => s3::prepare_copy(b, c, &options, buf),
                true => s3::prepare_stage_part_copy(b, c, &options, buf),
            },
            |template, bytes, key, buf| {
                let copy = CopyItem {
                    source_revision: (source_revision != RevisionKind::Current).then_some("v1"),
                    condition_value: (!staged).then_some(ConditionValue::ETag(b"*")),
                    source_condition_value: etag,
                    source_range: if staged { range } else { RequestedRange::Whole },
                    ..CopyItem::new(key, SOURCE)
                };
                match staged {
                    false => len(s3::encode_copy(template, bytes, &copy, &now, buf)),
                    true => {
                        let item = s3::StagePartCopyItem {
                            copy,
                            upload_id: "upload-1",
                            number: 4,
                        };
                        len(s3::encode_stage_part_copy(
                            template, bytes, &item, &now, buf,
                        ))
                    }
                }
            },
        );
    }
    // The operations that name no object.
    let (bucket, credentials) = s3_bucket(aws);
    let targets = [
        DeleteTarget {
            key: KEY,
            revision: None,
        },
        DeleteTarget {
            key: SOURCE,
            revision: None,
        },
    ];
    let (template, bytes) = written(|buf| {
        s3::prepare_delete_many(&bucket, &credentials, Some(ChecksumKind::Crc64), buf)
    });
    let mut buf = vec![0; 64 * 1024];
    s3::encode_delete_many(&template, &bytes, &targets, &now, &mut buf).unwrap();
    for options in [
        ListOptions::default(),
        ListOptions {
            delimiter: Some("/"),
            max_results: Some(100),
            include: ListInclude::OWNER,
        },
        ListOptions {
            delimiter: Some("/"),
            max_results: Some(100),
            include: ListInclude::VERSIONS,
        },
    ] {
        let (template, bytes) =
            written(|buf| s3::prepare_list(&bucket, &credentials, &options, buf));
        let versions = options.include.contains(ListInclude::VERSIONS);
        for item in [
            ListItem::new(""),
            ListItem {
                marker: Some(match versions {
                    true => ListMarker::Version {
                        key: "dir/a",
                        version: Some("v1"),
                    },
                    false => ListMarker::Text("next"),
                }),
                start_after: (!versions).then_some("dir/0"),
                ..ListItem::new("dir/")
            },
        ] {
            s3::encode_list(&template, &bytes, &item, &now, &mut buf).unwrap();
        }
    }
    let (directory, credentials) = s3_bucket(Service::AwsDirectory);
    let (template, bytes) =
        written(|buf| s3::prepare_create_session(&directory, &credentials, buf));
    s3::encode_create_session(&template, &bytes, &now, &mut buf).unwrap();
}

// The length of an S3 request.
fn len(unsigned: Result<s3::Unsigned<'_>>) -> Result<usize> {
    unsigned.map(|unsigned| unsigned.len())
}
