//! S3 listing: the query that the request carries, and page reading.

use borink_object_storage_proto::s3::{Addressing, Bucket, Objects, Service};
use borink_object_storage_proto::sigv4::{
    Credentials, Sha256Provider, Sha256State, wipe_best_effort,
};
use borink_object_storage_proto::{
    EntryKind, Error, HeaderSpan, ListEntry, ListInclude, ListMarker, Listing, PhysicalList,
    ResponseFault, Timestamps, layered,
};

const ZEROS: Sha256Provider =
    Sha256Provider::new(Sha256State::uninit, |_, _| {}, |_| [0; 32], |_, _| [0; 32]);

fn objects(addressing: Addressing) -> Objects<'static> {
    let bucket = Bucket::new("https://s3.example.com", "bucket", "auto", Service::Aws)
        .unwrap()
        .with_addressing(addressing);
    let credentials = Credentials::new("AKIAIOSFODNN7EXAMPLE", "secret", wipe_best_effort).unwrap();
    Objects::new(bucket, credentials, ZEROS)
}

fn url(objects: &Objects<'_>, list: &PhysicalList<'_>) -> String {
    let now = Timestamps::from_unix(1_787_400_061);
    let size = layered::s3::list_requirements(objects, list, &now).unwrap();
    let mut buf = vec![0; size.bytes];
    let mut slots = vec![HeaderSpan::default(); size.headers];
    let request = objects
        .encode_list(&mut buf, &mut slots, list, &now)
        .unwrap();
    request.url().to_owned()
}

fn read(body: &str) -> Result<(Listing<'static>, Vec<ListEntry<'static>>), Error> {
    let body = Vec::leak(body.as_bytes().to_vec());
    let mut entries = vec![ListEntry::default(); 4];
    let page = objects(Addressing::Path).fill_listing(body, &mut entries)?;
    entries.truncate(page.filled);
    Ok((page, entries))
}

// The URL must carry the query exactly as SigV4 signs it: sorted by name,
// with every byte but the unreserved ones encoded.
#[test]
fn the_query_is_written_in_its_canonical_form() {
    let list = PhysicalList {
        marker: Some(ListMarker::Text(
            "1ueGcxLPRx1Tr/XYExHnhbYLgveDs2J/wm36Hy4vbOwM=",
        )),
        start_after: Some("a b/c"),
        delimiter: Some("/"),
        max_results: Some(2),
        include: ListInclude::OWNER,
        ..PhysicalList::new("a b/é+")
    };
    assert_eq!(
        url(&objects(Addressing::Path), &list),
        "https://s3.example.com/bucket\
         ?continuation-token=1ueGcxLPRx1Tr%2FXYExHnhbYLgveDs2J%2Fwm36Hy4vbOwM%3D\
         &delimiter=%2F&encoding-type=url&fetch-owner=true&list-type=2&max-keys=2\
         &prefix=a%20b%2F%C3%A9%2B&start-after=a%20b%2Fc"
    );
    assert_eq!(
        url(&objects(Addressing::VirtualHosted), &PhysicalList::new("")),
        "https://bucket.s3.example.com/?encoding-type=url&list-type=2"
    );
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
    let page = objects(Addressing::Path)
        .fill_listing_with(body, &mut entries, wanted, |entry, values| {
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
