//! S3 response heads and error bodies, read without a client.

use borink_object_storage_proto::s3::{self};
use borink_object_storage_proto::{Classification, ResponseHead, ServiceErrorKind};

fn head<'h>(status: u16, headers: &[(&'h str, &'h [u8])]) -> ResponseHead<'h> {
    ResponseHead::from_headers(status, headers.iter().copied())
}

#[test]
fn a_response_head_reads_the_s3_names_of_its_values() {
    let head = head(
        200,
        &[
            ("x-amz-request-id", b"4442587FB7D0A2F9"),
            (
                "x-amz-id-2",
                b"vlR7PnpV2Ce81l0PRw6jlUpck7Jo5ZsQjryTjKlc5aLWGVHPZLj5NeC6qMa0emYBDXOo6QBU0Wo=",
            ),
            ("x-amz-version-id", b"3HL4kqtJlcpXroDTDmJ+rmSpXd3dIbrHY"),
        ],
    );
    assert_eq!(head.request_id, Some(b"4442587FB7D0A2F9".as_slice()));
    assert!(head.extended_request_id.is_some());
    assert_eq!(
        head.version,
        Some(b"3HL4kqtJlcpXroDTDmJ+rmSpXd3dIbrHY".as_slice())
    );
    assert_eq!(s3::metadata_name("X-Amz-Meta-colour"), Some("colour"));
    assert_eq!(s3::metadata_name("x-amz-meta-"), None);
    assert_eq!(s3::metadata_name("x-ms-meta-colour"), None);
}

#[test]
fn an_error_body_is_classified_by_its_code() {
    assert_eq!(
        s3::classify_error(b"<Error><Code>SlowDown</Code></Error>", false),
        Classification::Classified(ServiceErrorKind::Throttled)
    );
    assert_eq!(
        s3::classify_error(b"<Error><Co", true),
        Classification::Incomplete
    );
    assert_eq!(
        s3::classify_error(b"<Error><Code>Unheard</Code></Error>", false),
        Classification::Unknown
    );
    assert_eq!(
        s3::error_code(b"<Error><Code>Unheard</Code></Error>"),
        Some("Unheard")
    );
}
