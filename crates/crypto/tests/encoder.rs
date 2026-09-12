//! What a client does with a registered provider.

#![cfg(all(feature = "crc64", feature = "md5"))]

use borink_object_storage_proto::azure::{
    BlockRef, BlockSource, PhysicalCommitBlocks, PhysicalStageBlock,
};
use borink_object_storage_proto::{
    Blobs, ChecksumKind, Container, Error, HeaderSpan, InvalidPlan, Payload, PhysicalPut,
    Timestamps, TransactionalChecksum, WriteOptions, layered,
};

fn blobs() -> Blobs<'static> {
    Blobs::new(
        Container::new("https://account.blob.core.windows.net", "container").unwrap(),
        "token",
    )
    .unwrap()
    .with_checksum(borink_crypto::CRC64)
    .with_checksum(borink_crypto::MD5)
}

fn now() -> Timestamps {
    Timestamps::from_unix(1_787_400_000)
}

#[test]
fn the_encoder_computes_a_checksum_of_content_it_holds() {
    let blobs = blobs();
    for (kind, name, expected) in [
        (ChecksumKind::Md5, "content-md5", "eB5eJF1ptWaXm4bijSPyxw=="),
        (ChecksumKind::Crc64, "x-ms-content-crc64", "HZz9TO6x+RU="),
    ] {
        let put = PhysicalPut {
            options: WriteOptions {
                checksum: Some(TransactionalChecksum::Compute(kind)),
                ..Default::default()
            },
            ..PhysicalPut::new("object.bin")
        };
        let content = Payload::Slice(b"0123456789");
        let size = layered::put_requirements(&blobs, &put, content, &now()).unwrap();
        let mut buf = vec![0; size.bytes];
        let mut request_headers = vec![HeaderSpan::default(); size.headers];
        let request = blobs
            .encode_put(&mut buf, &mut request_headers, &put, content, &now())
            .unwrap();
        let headers: Vec<_> = request.headers().collect();
        assert!(headers.contains(&(name, expected)), "{kind:?}: {headers:?}");
        // The bytes are not here to sum, so a streamed payload is refused.
        assert_eq!(
            blobs
                .encode_put(
                    &mut buf,
                    &mut request_headers,
                    &put,
                    Payload::Streamed { len: 10 },
                    &now()
                )
                .map(drop),
            Err(Error::InvalidPlan(InvalidPlan::Option))
        );
    }
}

#[test]
fn a_client_registers_the_kinds_it_computes() {
    let put = |blobs: &Blobs<'_>, kind| {
        let options = WriteOptions {
            checksum: Some(TransactionalChecksum::Compute(kind)),
            ..Default::default()
        };
        blobs
            .encode_put(
                &mut [0; 1024],
                &mut [HeaderSpan::default(); 8],
                &PhysicalPut {
                    options,
                    ..PhysicalPut::new("object.bin")
                },
                Payload::Slice(b"0123456789"),
                &now(),
            )
            .map(|request| {
                request
                    .headers()
                    .find(|(name, _)| *name == kind_header(kind))
                    .map(|(_, value)| value.to_owned())
                    .unwrap()
            })
    };
    let one = Blobs::new(
        Container::new("https://account.blob.core.windows.net", "container").unwrap(),
        "token",
    )
    .unwrap()
    .with_checksum(borink_crypto::CRC64);
    // A client is the value that carries the providers, so registering one
    // kind leaves the other refused.
    assert_eq!(put(&one, ChecksumKind::Crc64).unwrap(), "HZz9TO6x+RU=");
    assert_eq!(
        put(&one, ChecksumKind::Md5),
        Err(Error::InvalidPlan(InvalidPlan::Option))
    );
}

fn kind_header(kind: ChecksumKind) -> &'static str {
    match kind {
        ChecksumKind::Md5 => "content-md5",
        _ => "x-ms-content-crc64",
    }
}

#[test]
fn a_stage_and_a_commit_compute_the_checksum_of_what_they_send() {
    let options = WriteOptions {
        checksum: Some(TransactionalChecksum::Compute(ChecksumKind::Crc64)),
        ..Default::default()
    };
    let stage = PhysicalStageBlock {
        options,
        ..PhysicalStageBlock::new("object", "AAAAAA==")
    };
    let content = Payload::Slice(b"0123456789");
    let size = layered::stage_block_requirements(&blobs(), &stage, content, &now()).unwrap();
    let mut buf = vec![0; size.bytes];
    let mut headers = vec![HeaderSpan::default(); size.headers];
    let request = blobs()
        .encode_stage_block(&mut buf, &mut headers, &stage, content, &now())
        .unwrap();
    assert!(
        request
            .headers()
            .any(|header| header == ("x-ms-content-crc64", "HZz9TO6x+RU="))
    );

    // A commit's content is the block list, so that is what it sums, and the
    // encoder never holds that text whole.
    let options = WriteOptions {
        checksum: Some(TransactionalChecksum::Compute(ChecksumKind::Md5)),
        ..Default::default()
    };
    let commit = PhysicalCommitBlocks {
        options,
        ..PhysicalCommitBlocks::new("object")
    };
    let blocks = [BlockRef {
        id: "AAAAAA==",
        source: BlockSource::Latest,
    }];
    let size = layered::commit_blocks_requirements(&blobs(), &commit, &blocks, &now()).unwrap();
    let mut buf = vec![0; size.bytes];
    let mut headers = vec![HeaderSpan::default(); size.headers];
    let request = blobs()
        .encode_commit_blocks(&mut buf, &mut headers, &commit, &blocks, &now())
        .unwrap();
    assert_eq!(
        request.payload().bytes(),
        Some(b"<?xml version=\"1.0\" encoding=\"utf-8\"?><BlockList><Latest>AAAAAA==</Latest></BlockList>".as_slice())
    );
    assert!(
        request
            .headers()
            .any(|header| header == ("content-md5", "YzOsE0fk1HdRsGkEw5j/sg=="))
    );
}
