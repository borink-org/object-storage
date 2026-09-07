//! Azure block operations: staging, committing and listing blocks.

use borink_object_storage_proto::azure::{
    Block, BlockListKind, BlockRef, BlockResponseHead, BlockSource, BlockState,
    PhysicalCommitBlocks, PhysicalListBlocks, PhysicalStageBlock,
};
use borink_object_storage_proto::{
    Blobs, CommitBlocksHeadOutcome, ConditionKind, Container, Error, HeaderSpan, InvalidPlan,
    ListBlocksHeadOutcome, Method, Payload, ResponseHead, StageBlockHeadOutcome, Timestamps,
    layered,
};

fn blobs() -> Blobs<'static> {
    Blobs::new(
        Container::new("https://account.blob.core.windows.net", "container").unwrap(),
        "token",
    )
    .unwrap()
}

fn now() -> Timestamps {
    Timestamps::from_unix(1_787_400_000)
}

fn stage_id(id: &str) -> Result<(), Error> {
    blobs()
        .encode_stage_block(
            &mut [0; 1024],
            &mut [HeaderSpan::default(); 8],
            &PhysicalStageBlock { key: "object", id },
            Payload::Slice(b"bytes"),
            &now(),
        )
        .map(drop)
}

#[test]
fn a_stage_names_the_block_in_the_query_and_states_the_length() {
    let mut buf = [0; 1024];
    let mut headers = [HeaderSpan::default(); 8];
    let request = blobs()
        .encode_stage_block(
            &mut buf,
            &mut headers,
            &PhysicalStageBlock {
                key: "object",
                id: "+/8=",
            },
            Payload::Slice(b"bytes"),
            &now(),
        )
        .unwrap();
    assert_eq!(request.method(), Method::Put);
    assert_eq!(
        request.url(),
        "https://account.blob.core.windows.net/container/object?comp=block&blockid=%2B%2F8%3D"
    );
    assert!(
        request
            .headers()
            .any(|header| header == ("content-length", "5"))
    );
    assert_eq!(request.payload().bytes(), Some(b"bytes".as_slice()));
    assert_eq!(request.body_span(), None);
}

#[test]
fn the_block_id_is_checked_locally_before_any_byte_is_written() {
    let refused = |id: &str| {
        assert!(
            matches!(stage_id(id), Err(Error::InvalidPlan(InvalidPlan::BlockId))),
            "{id:?}"
        );
    };
    assert!(stage_id("YQ==").is_ok());
    assert!(stage_id("YWJj").is_ok());
    // 88 characters with two bytes of padding decode to exactly 64 bytes.
    assert!(stage_id(&format!("{}==", "A".repeat(86))).is_ok());
    refused("");
    refused("YQ=");
    refused("YQ===");
    refused("Y Q==");
    refused("YQ!=");
    // 88 characters without padding decode to 66 bytes.
    refused(&"A".repeat(88));
    refused(&"A".repeat(92));
    // The plan is refused before the buffer is measured.
    assert!(matches!(
        blobs().encode_stage_block(
            &mut [],
            &mut [],
            &PhysicalStageBlock {
                key: "object",
                id: "?"
            },
            Payload::Slice(b""),
            &now(),
        ),
        Err(Error::InvalidPlan(InvalidPlan::BlockId))
    ));
}

#[test]
fn a_commit_writes_the_selectors_in_order_after_the_head() {
    let blocks = [
        BlockRef {
            id: "YQ==",
            source: BlockSource::Committed,
        },
        BlockRef {
            id: "Yg==",
            source: BlockSource::Uncommitted,
        },
        BlockRef {
            id: "YQ==",
            source: BlockSource::Latest,
        },
    ];
    let plan = PhysicalCommitBlocks::new("object");
    let size = layered::commit_blocks_requirements(&blobs(), &plan, &blocks, &now()).unwrap();
    let mut buf = vec![0; size.bytes];
    let mut headers = vec![HeaderSpan::default(); size.headers];
    let request = blobs()
        .encode_commit_blocks(&mut buf, &mut headers, &plan, &blocks, &now())
        .unwrap();
    let body = "<?xml version=\"1.0\" encoding=\"utf-8\"?><BlockList>\
                <Committed>YQ==</Committed><Uncommitted>Yg==</Uncommitted><Latest>YQ==</Latest>\
                </BlockList>";
    assert_eq!(request.method(), Method::Put);
    assert_eq!(
        request.url(),
        "https://account.blob.core.windows.net/container/object?comp=blocklist"
    );
    assert_eq!(request.payload().bytes(), Some(body.as_bytes()));
    assert!(
        request
            .headers()
            .any(|header| header == ("content-length", &body.len().to_string()))
    );
    let span = request.body_span().unwrap();
    assert_eq!(&buf[span.start..span.start + span.len], body.as_bytes());

    // The requirement is exact: one byte or one slot less is refused with it.
    let short = blobs().encode_commit_blocks(
        &mut buf[..size.bytes - 1],
        &mut headers,
        &plan,
        &blocks,
        &now(),
    );
    match short {
        Err(Error::Capacity(error)) => assert_eq!(error.required, size.bytes),
        other => panic!("{other:?}"),
    }
    let mut buf = vec![0; size.bytes];
    assert!(matches!(
        blobs().encode_commit_blocks(
            &mut buf,
            &mut headers[..size.headers - 1],
            &plan,
            &blocks,
            &now(),
        ),
        Err(Error::Capacity(_))
    ));
}

#[test]
fn a_commit_from_an_iterator_writes_the_same_request_as_a_slice() {
    let ids = ["YQ==", "Yg=="];
    let blocks = ids.map(|id| BlockRef {
        id,
        source: BlockSource::Uncommitted,
    });
    let plan = PhysicalCommitBlocks::new("object");
    let mut from_slice = [0; 1024];
    let mut from_iter = [0; 1024];
    let mut headers = [HeaderSpan::default(); 8];
    let slice = blobs()
        .encode_commit_blocks(&mut from_slice, &mut headers, &plan, &blocks, &now())
        .unwrap();
    let slice_body = slice.payload().bytes().unwrap().to_vec();
    let iter = blobs()
        .encode_commit_blocks_from_iter(
            &mut from_iter,
            &mut headers,
            &plan,
            ids.iter().map(|id| (id, BlockSource::Uncommitted)),
            &now(),
        )
        .unwrap();
    assert_eq!(iter.payload().bytes(), Some(slice_body.as_slice()));
}

#[test]
fn an_empty_commit_writes_an_empty_list() {
    let mut buf = [0; 1024];
    let mut headers = [HeaderSpan::default(); 8];
    let request = blobs()
        .encode_commit_blocks(
            &mut buf,
            &mut headers,
            &PhysicalCommitBlocks::new("object"),
            &[],
            &now(),
        )
        .unwrap();
    assert_eq!(
        request.payload().bytes(),
        Some(b"<?xml version=\"1.0\" encoding=\"utf-8\"?><BlockList></BlockList>".as_slice())
    );
}

#[test]
fn a_commit_refuses_more_blocks_than_the_service_takes() {
    let one = ("YQ==", BlockSource::Latest);
    let plan = PhysicalCommitBlocks::new("object");
    assert!(matches!(
        blobs().encode_commit_blocks_from_iter(
            &mut [],
            &mut [],
            &plan,
            core::iter::repeat_n(one, 50_001),
            &now(),
        ),
        Err(Error::InvalidPlan(InvalidPlan::Blocks))
    ));
    assert!(matches!(
        blobs().encode_commit_blocks_from_iter(
            &mut [],
            &mut [],
            &plan,
            core::iter::repeat_n(one, 50_000),
            &now(),
        ),
        Err(Error::Capacity(_))
    ));
}

#[test]
fn a_conditional_commit_sends_the_condition_and_keeps_it_in_the_shape() {
    let plan = PhysicalCommitBlocks {
        key: "object",
        condition: ConditionKind::IfNoneMatch,
        condition_value: Some(b"*"),
    };
    let mut buf = [0; 1024];
    let mut headers = [HeaderSpan::default(); 8];
    let request = blobs()
        .encode_commit_blocks(&mut buf, &mut headers, &plan, &[], &now())
        .unwrap();
    assert!(
        request
            .headers()
            .any(|header| header == ("if-none-match", "*"))
    );
    assert_eq!(plan.shape().condition, ConditionKind::IfNoneMatch);
}

#[test]
fn a_staged_block_has_no_entity_tag_but_keeps_its_checksum() {
    let head = BlockResponseHead::from_headers(
        201,
        [
            ("x-ms-content-crc64", b"AAAAAAAAAAA=".as_slice()),
            ("x-ms-request-id", b"request".as_slice()),
        ],
    );
    assert_eq!(head.content_crc64, Some(b"AAAAAAAAAAA=".as_slice()));
    assert_eq!(
        blobs().accept_stage_block_head(head.common).unwrap(),
        StageBlockHeadOutcome::Staged
    );
}

#[test]
fn a_lease_refusal_is_not_a_failed_condition() {
    let lease =
        ResponseHead::from_headers(412, [("x-ms-error-code", b"LeaseIdMissing".as_slice())]);
    match blobs().accept_stage_block_head(lease).unwrap() {
        StageBlockHeadOutcome::ServiceFailure(failure) => assert_eq!(failure.status, 412),
        other => panic!("{other:?}"),
    }
    let unconditional = PhysicalCommitBlocks::new("object").shape();
    match blobs()
        .accept_commit_blocks_head(unconditional, lease)
        .unwrap()
    {
        CommitBlocksHeadOutcome::ServiceFailure(failure) => assert_eq!(failure.status, 412),
        other => panic!("{other:?}"),
    }
    let conditional = PhysicalCommitBlocks {
        key: "object",
        condition: ConditionKind::IfMatch,
        condition_value: Some(b"\"tag\""),
    }
    .shape();
    match blobs()
        .accept_commit_blocks_head(conditional, lease)
        .unwrap()
    {
        CommitBlocksHeadOutcome::ServiceFailure(failure) => assert_eq!(failure.status, 412),
        other => panic!("{other:?}"),
    }
    let stale =
        ResponseHead::from_headers(412, [("x-ms-error-code", b"ConditionNotMet".as_slice())]);
    assert_eq!(
        blobs()
            .accept_commit_blocks_head(conditional, stale)
            .unwrap(),
        CommitBlocksHeadOutcome::PreconditionFailed
    );
    // The same code without a condition in the plan is not a failed condition
    // of ours either.
    assert!(matches!(
        blobs()
            .accept_commit_blocks_head(unconditional, stale)
            .unwrap(),
        CommitBlocksHeadOutcome::ServiceFailure(_)
    ));
}

#[test]
fn a_block_list_read_sizes_its_body() {
    let plan = PhysicalListBlocks::new("object", BlockListKind::All);
    let size = layered::list_blocks_requirements(&blobs(), &plan, &now()).unwrap();
    let mut buf = vec![0; size.bytes];
    let mut headers = vec![HeaderSpan::default(); size.headers];
    let request = blobs()
        .encode_list_blocks(&mut buf, &mut headers, &plan, &now())
        .unwrap();
    assert_eq!(request.method(), Method::Get);
    assert_eq!(
        request.url(),
        "https://account.blob.core.windows.net/container/object?comp=blocklist&blocklisttype=all"
    );
    assert!(matches!(
        blobs().encode_list_blocks(&mut buf[..size.bytes - 1], &mut headers, &plan, &now()),
        Err(Error::Capacity(_))
    ));
    let head = ResponseHead::from_headers(200, [("Content-Length", b"86".as_slice())]);
    match blobs().accept_list_blocks_head(head).unwrap() {
        ListBlocksHeadOutcome::Blocks { expected_len, .. } => assert_eq!(expected_len, Some(86)),
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_block_bound_is_the_smallest_element_the_reader_accepts() {
    const ELEMENT: &str = "<Block><Name>x</Name><Size>0</Size></Block>";
    assert_eq!(ELEMENT.len(), 43);
    let count = 7;
    let body = format!(
        "<BlockList><UncommittedBlocks>{}</UncommittedBlocks></BlockList>",
        ELEMENT.repeat(count)
    );
    let bound = layered::max_blocks_in(body.len());
    assert!(bound >= count);
    let mut entries = vec![Block::default(); bound];
    let mut bytes = body.clone().into_bytes();
    let listing = blobs().fill_blocks(&mut bytes, &mut entries).unwrap();
    assert_eq!(listing.filled, count);
    assert!(
        entries[..count].iter().all(|block| {
            block.id == "x" && block.size == 0 && block.state == BlockState::Staged
        })
    );

    // Too small an array reports how many blocks the body holds.
    let mut bytes = body.clone().into_bytes();
    match blobs().fill_blocks(&mut bytes, &mut entries[..count - 1]) {
        Err(Error::Capacity(error)) => assert_eq!(error.required, count),
        other => panic!("{other:?}"),
    }

    // A block without a name is not accepted, so no element is shorter.
    let mut nameless =
        b"<BlockList><UncommittedBlocks><Block><Name></Name><Size>0</Size></Block></UncommittedBlocks></BlockList>"
            .to_vec();
    assert!(matches!(
        blobs().fill_blocks(&mut nameless, &mut entries),
        Err(Error::Response(_))
    ));
}

#[test]
fn a_listed_id_is_passed_back_unchanged() {
    let mut body = b"<BlockList><CommittedBlocks><Block><Name>+/8=</Name><Size>5</Size></Block>\
                     </CommittedBlocks><UncommittedBlocks /></BlockList>"
        .to_vec();
    let mut entries = [Block::default(); 2];
    let listing = blobs().fill_blocks(&mut body, &mut entries).unwrap();
    assert_eq!(listing.filled, 1);
    assert_eq!(entries[0].state, BlockState::Committed);
    let block = BlockRef {
        id: entries[0].id,
        source: BlockSource::Committed,
    };
    let mut buf = [0; 1024];
    let mut headers = [HeaderSpan::default(); 8];
    let request = blobs()
        .encode_commit_blocks(
            &mut buf,
            &mut headers,
            &PhysicalCommitBlocks::new("object"),
            &[block],
            &now(),
        )
        .unwrap();
    assert!(
        std::str::from_utf8(request.payload().bytes().unwrap())
            .unwrap()
            .contains("<Committed>+/8=</Committed>")
    );
}

#[test]
fn the_stage_requirement_follows_the_stated_length_not_the_bytes() {
    let plan = PhysicalStageBlock {
        key: "object",
        id: "YQ==",
    };
    let held =
        layered::stage_block_requirements(&blobs(), &plan, Payload::Slice(&[0; 300]), &now())
            .unwrap();
    let streamed =
        layered::stage_block_requirements(&blobs(), &plan, Payload::Streamed { len: 300 }, &now())
            .unwrap();
    assert_eq!(held, streamed);
    let mut buf = vec![0; held.bytes];
    let mut headers = vec![HeaderSpan::default(); held.headers];
    assert!(
        blobs()
            .encode_stage_block(
                &mut buf,
                &mut headers,
                &plan,
                Payload::Streamed { len: 300 },
                &now()
            )
            .is_ok()
    );
}
