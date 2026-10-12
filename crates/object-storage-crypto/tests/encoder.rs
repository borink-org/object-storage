//! What a client does with a registered provider.

#![cfg(all(feature = "crc64", feature = "md5-rustcrypto"))]

use borink_object_storage_proto::azure::{BlockRef, BlockSource, PhysicalStageBlock};
use borink_object_storage_proto::{
    Blobs, ChecksumKind, Container, HeaderSpan, Payload, PhysicalCommit, Timestamps,
    TransactionalChecksum, WriteOptions, layered,
};

fn blobs() -> Blobs<'static> {
    Blobs::new(
        Container::new("https://account.blob.core.windows.net", "container").unwrap(),
        "token",
    )
    .unwrap()
    .with_checksum(borink_object_storage_crypto::CRC64)
    .with_checksum(borink_object_storage_crypto::MD5_RUSTCRYPTO)
}

fn now() -> Timestamps {
    Timestamps::from_unix(1_787_400_000)
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
    let commit = PhysicalCommit {
        options,
        ..PhysicalCommit::new("object")
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
