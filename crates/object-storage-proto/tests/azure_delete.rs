//! Azure Delete Blob: the removals the client refuses.

use borink_object_storage_proto::azure;
use borink_object_storage_proto::{
    AzureNamespace, ConditionKind, ConditionValue, Container, DeleteItem, DeleteKind,
    DeleteOptions, Error, InvalidPlan, RevisionKind, Timestamps,
};

fn container() -> Container<'static> {
    Container::new("https://account.blob.core.windows.net", "container").unwrap()
}

fn refusal(options: &DeleteOptions, item: &DeleteItem<'_>) -> Option<Error> {
    let mut bytes = [0; 512];
    let template = match azure::prepare_delete(
        &container(),
        "token",
        AzureNamespace::Unknown,
        options,
        &mut bytes,
    ) {
        Ok(template) => template,
        Err(error) => return Some(error),
    };
    let mut untouched = [0xAA; 512];
    let now = Timestamps::from_unix(1_787_400_000);
    let error = azure::encode_delete(&template, &bytes, item, &now, &mut untouched).err();
    if error.is_some() {
        assert!(untouched.iter().all(|byte| *byte == 0xAA));
    }
    error
}

#[test]
fn a_removal_is_refused_before_any_byte_is_written() {
    let if_match = DeleteOptions {
        condition: ConditionKind::IfMatch,
        ..DeleteOptions::new()
    };
    let version = DeleteOptions {
        revision: RevisionKind::Version,
        ..DeleteOptions::new()
    };
    for (options, item, expected) in [
        (
            DeleteOptions::new(),
            DeleteItem::new(""),
            InvalidPlan::EmptyKey,
        ),
        // The template carries a condition, so each removal names its value.
        (if_match, DeleteItem::new("k"), InvalidPlan::Condition),
        (
            DeleteOptions::new(),
            DeleteItem {
                condition_value: Some(ConditionValue::ETag(b"\"etag\"")),
                ..DeleteItem::new("k")
            },
            InvalidPlan::Condition,
        ),
        // The template names a version, so each removal names one.
        (version, DeleteItem::new("k"), InvalidPlan::Revision),
        (
            version,
            DeleteItem {
                revision: Some(""),
                ..DeleteItem::new("k")
            },
            InvalidPlan::Revision,
        ),
        // A version has no snapshots of its own, and Azure refuses
        // `x-ms-delete-snapshots` beside one.
        (
            DeleteOptions {
                kind: DeleteKind::ObjectAndSnapshots,
                ..version
            },
            DeleteItem {
                revision: Some("v1"),
                ..DeleteItem::new("k")
            },
            InvalidPlan::Revision,
        ),
    ] {
        assert_eq!(
            refusal(&options, &item),
            Some(Error::InvalidPlan(expected)),
            "{options:?} {item:?}"
        );
    }
}
