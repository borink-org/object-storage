//! Azure block operations: the answers to a stage, a commit and a block
//! listing.

use borink_object_storage_proto::azure::{self, Block, BlockState};
use borink_object_storage_proto::{
    CommitHeadOutcome, CommitShape, ConditionKind, Error, ResponseHead, StageHeadOutcome,
};

#[test]
fn a_lease_refusal_is_not_a_failed_condition() {
    let lease =
        ResponseHead::from_headers(412, [("x-ms-error-code", b"LeaseIdMissing".as_slice())]);
    match azure::accept_stage_block_head(lease).unwrap() {
        StageHeadOutcome::ServiceFailure(failure) => assert_eq!(failure.status, 412),
        other => panic!("{other:?}"),
    }
    let unconditional = CommitShape::default();
    match azure::accept_commit_blocks_head(unconditional, lease).unwrap() {
        CommitHeadOutcome::ServiceFailure(failure) => assert_eq!(failure.status, 412),
        other => panic!("{other:?}"),
    }
    let conditional = CommitShape {
        condition: ConditionKind::IfMatch,
    };
    match azure::accept_commit_blocks_head(conditional, lease).unwrap() {
        CommitHeadOutcome::ServiceFailure(failure) => assert_eq!(failure.status, 412),
        other => panic!("{other:?}"),
    }
    let stale =
        ResponseHead::from_headers(412, [("x-ms-error-code", b"ConditionNotMet".as_slice())]);
    assert_eq!(
        azure::accept_commit_blocks_head(conditional, stale).unwrap(),
        CommitHeadOutcome::PreconditionFailed
    );
    // The same code without a condition in the template is not a failed
    // condition of ours either.
    assert!(matches!(
        azure::accept_commit_blocks_head(unconditional, stale).unwrap(),
        CommitHeadOutcome::ServiceFailure(_)
    ));
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
    let bound = azure::max_blocks_in(body.len());
    assert!(bound >= count);
    let mut entries = vec![Block::default(); bound];
    let mut bytes = body.clone().into_bytes();
    let listing = azure::fill_blocks(&mut bytes, &mut entries).unwrap();
    assert_eq!(listing.filled, count);
    assert!(
        entries[..count].iter().all(|block| {
            block.id == "x" && block.size == 0 && block.state == BlockState::Staged
        })
    );

    // Too small an array reports how many blocks the body holds.
    let mut bytes = body.clone().into_bytes();
    match azure::fill_blocks(&mut bytes, &mut entries[..count - 1]) {
        Err(Error::Capacity(error)) => assert_eq!(error.required, count),
        other => panic!("{other:?}"),
    }

    // A block without a name is not accepted, so no element is shorter.
    let mut nameless =
        b"<BlockList><UncommittedBlocks><Block><Name></Name><Size>0</Size></Block></UncommittedBlocks></BlockList>"
            .to_vec();
    assert!(matches!(
        azure::fill_blocks(&mut nameless, &mut entries),
        Err(Error::Response(_))
    ));
}
