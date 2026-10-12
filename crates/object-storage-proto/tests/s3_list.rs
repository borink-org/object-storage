//! S3 listing: the query that the request carries, and page reading.

use borink_object_storage_proto::s3::{self, Addressing, Bucket, Service};
use borink_object_storage_proto::sigv4::{Credentials, wipe_best_effort};
use borink_object_storage_proto::{
    EntryKind, Error, InvalidPlan, ListEntry, ListInclude, ListItem, ListMarker, ListOptions,
    Listing, ResponseFault, Timestamps,
};

// The URL of the page `item` of a listing with `options` of a bucket of
// `service`, or the reason it is refused.
fn encode(
    service: Service,
    addressing: Addressing,
    options: &ListOptions<'_>,
    item: &ListItem<'_>,
) -> Result<String, Error> {
    // A directory bucket is named for its zone.
    let (endpoint, name) = match service {
        Service::AwsDirectory => (
            "https://s3express-euc1-az1.eu-central-1.amazonaws.com",
            "bucket--euc1-az1--x-s3",
        ),
        _ => ("https://s3.example.com", "bucket"),
    };
    let bucket = Bucket::new(endpoint, name, "auto", service)
        .unwrap()
        .with_addressing(addressing);
    let credentials = Credentials::new("AKIAIOSFODNN7EXAMPLE", "secret", wipe_best_effort).unwrap();
    let mut bytes = vec![0; 2048];
    let template = s3::prepare_list(&bucket, &credentials, options, &mut bytes)?;
    let now = Timestamps::from_unix(1_787_400_061);
    let mut buf = vec![0; 4096];
    let unsigned = s3::encode_list(&template, &bytes, item, &now, &mut buf)?;
    let request = s3::finish(&unsigned, &mut buf, &[0; 32]);
    Ok(request.url().to_owned())
}

fn url(addressing: Addressing, options: &ListOptions<'_>, item: &ListItem<'_>) -> String {
    encode(Service::Aws, addressing, options, item).unwrap()
}

fn read(body: &str) -> Result<(Listing<'static>, Vec<ListEntry<'static>>), Error> {
    let body = Vec::leak(body.as_bytes().to_vec());
    let mut entries = vec![ListEntry::default(); 4];
    let page = s3::fill_listing(body, &mut entries)?;
    entries.truncate(page.filled);
    Ok((page, entries))
}

// The URL must carry the query exactly as SigV4 signs it: sorted by name,
// with every byte but the unreserved ones encoded. A parameter that a page
// leaves out takes its separator along.
#[test]
fn the_query_is_written_in_its_canonical_form() {
    let options = ListOptions {
        delimiter: Some("/"),
        max_results: Some(2),
        include: ListInclude::OWNER,
    };
    let item = ListItem {
        start_after: Some("a b/c"),
        ..ListItem::new("a b/é+").after(ListMarker::Text(
            "1ueGcxLPRx1Tr/XYExHnhbYLgveDs2J/wm36Hy4vbOwM=",
        ))
    };
    assert_eq!(
        url(Addressing::Path, &options, &item),
        "https://s3.example.com/bucket\
         ?continuation-token=1ueGcxLPRx1Tr%2FXYExHnhbYLgveDs2J%2Fwm36Hy4vbOwM%3D\
         &delimiter=%2F&encoding-type=url&fetch-owner=true&list-type=2&max-keys=2\
         &prefix=a%20b%2F%C3%A9%2B&start-after=a%20b%2Fc"
    );
    assert_eq!(
        url(Addressing::Path, &options, &ListItem::new("")),
        "https://s3.example.com/bucket\
         ?delimiter=%2F&encoding-type=url&fetch-owner=true&list-type=2&max-keys=2"
    );
    assert_eq!(
        url(
            Addressing::VirtualHosted,
            &ListOptions::default(),
            &ListItem::new("")
        ),
        "https://bucket.s3.example.com/?encoding-type=url&list-type=2"
    );

    let versions = ListOptions {
        include: ListInclude::VERSIONS,
        ..ListOptions::default()
    };
    let after = ListItem::new("p").after(ListMarker::Version {
        key: "k",
        version: Some("v1"),
    });
    assert_eq!(
        url(Addressing::Path, &versions, &after),
        "https://s3.example.com/bucket\
         ?encoding-type=url&key-marker=k&prefix=p&version-id-marker=v1&versions="
    );
    assert_eq!(
        url(Addressing::Path, &versions, &ListItem::new("")),
        "https://s3.example.com/bucket?encoding-type=url&versions="
    );
}

#[test]
fn a_listing_that_s3_cannot_take_is_refused() {
    let refused = |service, options: &ListOptions<'_>, item: &ListItem<'_>| {
        encode(service, Addressing::Path, options, item).unwrap_err()
    };
    let objects = ListOptions::default();
    let versions = ListOptions {
        include: ListInclude::VERSIONS,
        ..objects
    };
    let text = ListItem::new("").after(ListMarker::Text("token"));
    let version = ListItem::new("").after(ListMarker::Version {
        key: "k",
        version: None,
    });
    for (service, options, item, expected) in [
        // A listing continues from the kind of marker its pages hand out.
        (Service::Aws, objects, version, InvalidPlan::Marker),
        (Service::Aws, versions, text, InvalidPlan::Marker),
        (
            Service::Aws,
            objects,
            ListItem::new("").after(ListMarker::Text("")),
            InvalidPlan::Marker,
        ),
        // A listing of versions starts at its markers alone.
        (
            Service::Aws,
            versions,
            ListItem {
                start_after: Some("k"),
                ..ListItem::new("")
            },
            InvalidPlan::Option,
        ),
        (
            Service::Aws,
            ListOptions {
                include: ListInclude::METADATA,
                ..objects
            },
            ListItem::new(""),
            InvalidPlan::Option,
        ),
        // A directory bucket groups keys at `/` alone, lists only at a
        // prefix that ends in it, and keeps no versions.
        (
            Service::AwsDirectory,
            ListOptions {
                delimiter: Some("-"),
                ..objects
            },
            ListItem::new(""),
            InvalidPlan::Delimiter,
        ),
        (
            Service::AwsDirectory,
            objects,
            ListItem::new("dir"),
            InvalidPlan::Prefix,
        ),
        (
            Service::AwsDirectory,
            versions,
            ListItem::new(""),
            InvalidPlan::Option,
        ),
    ] {
        assert_eq!(
            refused(service, &options, &item),
            Error::InvalidPlan(expected),
            "{service:?} {options:?} {item:?}"
        );
    }
}

#[test]
fn keys_are_url_decoded_and_the_rest_is_read_as_aws_writes_it() {
    let (page, entries) = read(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
         <Name>bucket</Name><Prefix>a</Prefix>\
         <NextContinuationToken>token&amp;more</NextContinuationToken>\
         <KeyCount>3</KeyCount><MaxKeys>3</MaxKeys><Delimiter>%2F</Delimiter>\
         <EncodingType>url</EncodingType><IsTruncated>true</IsTruncated>\
         <Contents><Key>a+b%2Bc%3C%2FKey%3E</Key>\
         <LastModified>2026-08-22T12:01:01.000Z</LastModified>\
         <ETag>&quot;fba9dede5f27731c9771645a39863328&quot;</ETag>\
         <Size>434234</Size><StorageClass>STANDARD</StorageClass></Contents>\
         <Contents><Key>caf%C3%A9</Key><Size>0</Size></Contents>\
         <CommonPrefixes><Prefix>a%2Fb%2F</Prefix></CommonPrefixes>\
         </ListBucketResult>",
    )
    .unwrap();
    assert_eq!(page.next_marker, Some(ListMarker::Text("token&more")));
    let keys: Vec<_> = entries
        .iter()
        .map(|entry| (entry.kind, entry.key))
        .collect();
    assert_eq!(
        keys,
        [
            (EntryKind::Object, "a b+c</Key>"),
            (EntryKind::Object, "caf\u{e9}"),
            (EntryKind::Prefix, "a/b/"),
        ]
    );
    assert_eq!(entries[0].size, Some(434_234));
    assert_eq!(
        entries[0].e_tag,
        Some("\"fba9dede5f27731c9771645a39863328\"")
    );
    // The walk over the entry is not misled by the decoded key's `</Key>`.
    assert_eq!(
        entries[0].property("StorageClass"),
        Some(b"STANDARD".as_slice())
    );
    assert_eq!(entries[2].size, None);
}

// Some services write `EncodingType` after the entries, so the reader
// decodes before it knows, and decides at the end of the page.
#[test]
fn a_decoded_key_needs_the_page_to_say_it_was_encoded() {
    let page = |encoding: &str, key: &str| {
        read(&format!(
            "<ListBucketResult><IsTruncated>false</IsTruncated>\
             <Contents><Key>{key}</Key><Size>1</Size></Contents>\
             {encoding}</ListBucketResult>"
        ))
        .map(|(_, entries)| entries[0].key)
    };
    let encoded = "<EncodingType>url</EncodingType>";
    assert_eq!(page(encoded, "a+b"), Ok("a b"));
    assert_eq!(page("", "plain/key"), Ok("plain/key"));
    let fault = Err(Error::Response(ResponseFault::Body));
    assert_eq!(page("", "a+b"), fault);
    assert_eq!(page("", "100%25"), fault);
    assert_eq!(page("<EncodingType>none</EncodingType>", "key"), fault);
}

#[test]
fn a_page_that_contradicts_its_token_is_refused() {
    let page = |truncated: &str, token: &str| {
        read(&format!(
            "<ListBucketResult>{truncated}{token}\
             <EncodingType>url</EncodingType></ListBucketResult>"
        ))
        .map(|(page, _)| page.next_marker)
    };
    let yes = "<IsTruncated>true</IsTruncated>";
    let no = "<IsTruncated>false</IsTruncated>";
    let token = "<NextContinuationToken>t</NextContinuationToken>";
    assert_eq!(page(yes, token), Ok(Some(ListMarker::Text("t"))));
    assert_eq!(page(no, ""), Ok(None));
    assert_eq!(page("", token), Ok(Some(ListMarker::Text("t"))));
    let fault = Err(Error::Response(ResponseFault::Body));
    assert_eq!(page(yes, ""), fault);
    assert_eq!(page(no, token), fault);
}

#[test]
fn wanted_properties_are_read_in_the_same_pass() {
    use borink_object_storage_proto::s3::{ObjectProperty, PropertySet};
    let wanted = PropertySet::of(&[
        ObjectProperty::Owner,
        ObjectProperty::StorageClass,
        ObjectProperty::ChecksumAlgorithm,
    ]);
    let body = Vec::leak(
        "<ListBucketResult><EncodingType>url</EncodingType>\
         <Contents><Key>a%3C%2FKey%3E</Key><Size>1</Size>\
         <ChecksumAlgorithm>CRC32</ChecksumAlgorithm>\
         <ChecksumAlgorithm>SHA256</ChecksumAlgorithm>\
         <Owner><ID>owner-id</ID></Owner><StorageClass>GLACIER</StorageClass></Contents>\
         <CommonPrefixes><Prefix>b/</Prefix></CommonPrefixes></ListBucketResult>"
            .as_bytes()
            .to_vec(),
    );
    let mut entries = [("", None, None, None); 2];
    let page = s3::fill_listing_with(body, &mut entries, wanted, |entry, values| {
        (
            entry.key,
            values.get(ObjectProperty::StorageClass),
            values.get(ObjectProperty::ChecksumAlgorithm),
            values.get(ObjectProperty::Owner),
        )
    })
    .unwrap();
    assert_eq!(page.filled, 2);
    assert_eq!(
        entries[0],
        (
            "a</Key>",
            Some(b"GLACIER".as_slice()),
            Some(b"CRC32".as_slice()),
            Some(b"<ID>owner-id</ID>".as_slice()),
        )
    );
    assert_eq!(entries[1], ("b/", None, None, None));
}
