//! Azure Delete Blob: the removals the client refuses.

use borink_object_storage_proto::azure;
use borink_object_storage_proto::{
    AzureNamespace, ConditionKind, ConditionValue, Container, DeleteKind, DeleteOptions, Error,
    InvalidRequest, ObjectItem, RevisionKind, Timestamps,
};

fn container() -> Container<'static> {
    Container::new("https://account.blob.core.windows.net", "container").unwrap()
}

fn refusal(options: &DeleteOptions, item: &ObjectItem<'_>) -> Option<Error> {
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
            ObjectItem::new(""),
            InvalidRequest::EmptyKey,
        ),
        // The template carries a condition, so each removal names its value.
        (if_match, ObjectItem::new("k"), InvalidRequest::Condition),
        (
            DeleteOptions::new(),
            ObjectItem {
                condition_value: Some(ConditionValue::ETag(b"\"etag\"")),
                ..ObjectItem::new("k")
            },
            InvalidRequest::Condition,
        ),
        // The template names a version, so each removal names one.
        (version, ObjectItem::new("k"), InvalidRequest::Revision),
        (
            version,
            ObjectItem {
                revision: Some(""),
                ..ObjectItem::new("k")
            },
            InvalidRequest::Revision,
        ),
        // A version has no snapshots of its own, and Azure refuses
        // `x-ms-delete-snapshots` beside one.
        (
            DeleteOptions {
                kind: DeleteKind::ObjectAndSnapshots,
                ..version
            },
            ObjectItem {
                revision: Some("v1"),
                ..ObjectItem::new("k")
            },
            InvalidRequest::Revision,
        ),
    ] {
        assert_eq!(
            refusal(&options, &item),
            Some(Error::InvalidRequest(expected)),
            "{options:?} {item:?}"
        );
    }
}
