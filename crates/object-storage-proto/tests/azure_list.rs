//! Azure listing encoding, response interpretation and page reading.

use borink_object_storage_proto::{
    BlobProperty, Blobs, CapacityError, Container, EntryKind, Error, Failure, FailureClass,
    InvalidPlan, ListEntry, ListHeadOutcome, ListShape, Listing, Method, PhysicalList, PropertySet,
    PropertyValues, ResponseFault, ResponseHead, ServiceErrorKind, Timestamps, layered,
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

fn url(list: &PhysicalList<'_>) -> String {
    let blobs = blobs();
    let mut buf = vec![0; layered::list_requirements(&blobs, list, &now()).unwrap()];
    blobs
        .encode_list(&mut buf, list, &now())
        .unwrap()
        .url()
        .to_owned()
}

// One page, with everything a flat account writes into it.
fn page(entries: &str, next_marker: &str) -> Vec<u8> {
    document(entries, &format!("<NextMarker>{next_marker}</NextMarker>"))
}

fn document(entries: &str, next_marker: &str) -> Vec<u8> {
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
         <EnumerationResults ServiceEndpoint=\"https://account.blob.core.windows.net/\" \
         ContainerName=\"container\">\
         <Prefix></Prefix><MaxResults>2</MaxResults>\
         <Blobs>{entries}</Blobs>{next_marker}\
         </EnumerationResults>"
    )
    .into_bytes()
}

fn object(name: &str, size: u64) -> String {
    format!(
        "<Blob><Name>{name}</Name><Properties>\
         <Last-Modified>Sat, 22 Aug 2026 12:00:00 GMT</Last-Modified>\
         <Etag>0x8DF0046E8E555AF</Etag><Content-Length>{size}</Content-Length>\
         <BlobType>BlockBlob</BlobType></Properties></Blob>"
    )
}

fn fill<'b>(body: &'b mut [u8], into: &mut [ListEntry<'b>]) -> Listing<'b> {
    blobs().fill_listing(body, into).unwrap()
}

#[test]
fn a_listing_addresses_the_container_and_carries_no_content() {
    let blobs = blobs();
    let list = PhysicalList::new("");
    let mut buf = vec![0; layered::list_requirements(&blobs, &list, &now()).unwrap()];
    let request = blobs.encode_list(&mut buf, &list, &now()).unwrap();

    assert_eq!(request.method(), Method::Get);
    assert_eq!(
        request.url(),
        "https://account.blob.core.windows.net/container?restype=container&comp=list"
    );
    assert!(request.payload().is_empty());

    let headers: Vec<_> = request.headers().collect();
    assert_eq!(
        headers,
        [
            ("authorization", "Bearer token"),
            ("x-ms-date", "Sat, 22 Aug 2026 12:00:00 GMT"),
            ("x-ms-version", "2026-04-06"),
        ]
    );
}

#[test]
fn every_query_parameter_is_written_in_one_order() {
    let base = "https://account.blob.core.windows.net/container?restype=container&comp=list";

    assert_eq!(
        url(&PhysicalList::new("directory/")),
        format!("{base}&prefix=directory%2F")
    );
    assert_eq!(
        url(&PhysicalList {
            delimited: true,
            ..PhysicalList::new("")
        }),
        format!("{base}&delimiter=%2F")
    );
    assert_eq!(
        url(&PhysicalList {
            max_results: Some(1000),
            ..PhysicalList::new("")
        }),
        format!("{base}&maxresults=1000")
    );

    // A marker is the service's own opaque text, so every byte of it that is
    // not unreserved is encoded, including one that is already a percent.
    assert_eq!(
        url(&PhysicalList {
            marker: Some("2!72!MDAwMDI4!a+b%c"),
            ..PhysicalList::new("")
        }),
        format!("{base}&marker=2%2172%21MDAwMDI4%21a%2Bb%25c")
    );

    assert_eq!(
        url(&PhysicalList::from_shape(
            ListShape {
                delimited: true,
                max_results: Some(2),
            },
            "directory/",
            Some("next"),
        )),
        format!("{base}&prefix=directory%2F&delimiter=%2F&marker=next&maxresults=2")
    );
}

#[test]
fn a_shape_and_the_borrowed_bytes_rebuild_the_plan() {
    let list = PhysicalList {
        marker: Some("next"),
        delimited: true,
        max_results: Some(2),
        ..PhysicalList::new("directory/")
    };
    assert_eq!(
        PhysicalList::from_shape(list.shape(), list.prefix, list.marker),
        list
    );
}

#[test]
fn a_listing_plan_is_validated_before_any_byte_is_written() {
    let blobs = blobs();
    let long = "k".repeat(1025);
    for (list, expected) in [
        (PhysicalList::new(long.as_str()), InvalidPlan::Prefix),
        (
            PhysicalList {
                marker: Some(""),
                ..PhysicalList::new("")
            },
            InvalidPlan::Marker,
        ),
        (
            PhysicalList {
                max_results: Some(0),
                ..PhysicalList::new("")
            },
            InvalidPlan::MaxResults,
        ),
    ] {
        assert_eq!(
            blobs.encode_list(&mut [0; 512], &list, &now()).map(drop),
            Err(Error::InvalidPlan(expected))
        );
        // The plan is refused before the buffer is even looked at.
        assert_eq!(
            blobs.encode_list(&mut [], &list, &now()).map(drop),
            Err(Error::InvalidPlan(expected))
        );
    }

    // A prefix of exactly the longest key, and an empty one, both encode.
    let longest = "k".repeat(1024);
    assert!(
        layered::list_requirements(&blobs, &PhysicalList::new(longest.as_str()), &now()).is_ok()
    );
    assert!(layered::list_requirements(&blobs, &PhysicalList::new(""), &now()).is_ok());
}

#[test]
fn an_undersized_buffer_states_the_exact_requirement() {
    let blobs = blobs();
    let list = PhysicalList::new("directory/");
    let required = layered::list_requirements(&blobs, &list, &now()).unwrap();

    let error = blobs
        .encode_list(&mut vec![0; required - 1], &list, &now())
        .unwrap_err();
    assert_eq!(error.capacity().unwrap().required, required);
    assert!(
        blobs
            .encode_list(&mut vec![0; required], &list, &now())
            .is_ok()
    );
}

#[test]
fn a_success_announces_the_page_and_a_failure_names_the_error() {
    let blobs = blobs();

    assert_eq!(
        blobs.accept_list_head(ResponseHead::from_headers(
            200,
            [("Content-Length", b"512".as_slice())]
        )),
        Ok(ListHeadOutcome::Page {
            expected_len: Some(512)
        })
    );
    assert_eq!(
        blobs.accept_list_head(ResponseHead::new(200)),
        Ok(ListHeadOutcome::Page { expected_len: None })
    );

    // A missing container is the one thing a listing does not find.
    assert_eq!(
        blobs.accept_list_head(ResponseHead::from_headers(
            404,
            [("x-ms-error-code", b"ContainerNotFound".as_slice())]
        )),
        Ok(ListHeadOutcome::NotFound {
            kind: Some(ServiceErrorKind::NoSuchContainer)
        })
    );

    // A status that a listing never answers with is a fault, not an outcome.
    assert_eq!(
        blobs.accept_list_head(ResponseHead::new(206)),
        Err(Error::Response(ResponseFault::Status))
    );
    assert_eq!(
        blobs.accept_list_head(ResponseHead::from_headers(
            200,
            [("Content-Length", b"not a number".as_slice())]
        )),
        Err(Error::Response(ResponseFault::Head))
    );
}

#[test]
fn a_failure_without_a_code_header_is_finished_by_the_body() {
    let blobs = blobs();
    let head = ResponseHead::from_headers(404, [("x-ms-request-id", b"request-1".as_slice())]);

    let ListHeadOutcome::NeedErrorBody(failure) = blobs.accept_list_head(head).unwrap() else {
        panic!("a 404 with no code header needs the body");
    };
    assert_eq!(failure.kind, None);
    assert_eq!(
        blobs.accept_list_error_body(
            failure.status,
            failure.request_id,
            b"<Error><Code>ContainerNotFound</Code></Error>",
        ),
        ListHeadOutcome::NotFound {
            kind: Some(ServiceErrorKind::NoSuchContainer)
        }
    );

    assert_eq!(
        blobs.accept_list_error_body(503, None, b""),
        ListHeadOutcome::ServiceFailure(Failure {
            status: 503,
            class: FailureClass::Server,
            kind: None,
            request_id: None,
        })
    );
}

#[test]
fn a_page_reports_every_object_it_holds() {
    let mut body = page(&(object("a.txt", 8) + &object("b/c.txt", 0)), "");
    let mut entries = [ListEntry::default(); 4];
    let listing = fill(&mut body, &mut entries);

    assert_eq!(listing.filled, 2);
    assert_eq!(listing.next_marker, None);
    assert_eq!(entries[0].kind, EntryKind::Object);
    assert_eq!(entries[0].key, "a.txt");
    assert_eq!(entries[0].size, Some(8));
    assert_eq!(entries[0].e_tag, Some("0x8DF0046E8E555AF"));
    assert_eq!(
        entries[0]
            .last_modified
            .map(str::as_bytes)
            .and_then(layered::http_date_ms),
        Some(1_787_400_000_000)
    );
    assert_eq!(entries[1].key, "b/c.txt");
    assert_eq!(entries[1].size, Some(0));
    // The array past the page is untouched.
    assert_eq!(entries[2], ListEntry::default());
}

#[test]
fn a_delimited_page_interleaves_groups_with_objects() {
    let mut body = page(
        &format!(
            "{}<BlobPrefix><Name>directory/</Name></BlobPrefix>{}",
            object("a.txt", 1),
            object("z.txt", 2)
        ),
        "",
    );
    let mut entries = [ListEntry::default(); 3];
    let listing = fill(&mut body, &mut entries);

    assert_eq!(listing.filled, 3);
    assert_eq!(
        entries.map(|entry| (entry.kind, entry.key)),
        [
            (EntryKind::Object, "a.txt"),
            (EntryKind::Prefix, "directory/"),
            (EntryKind::Object, "z.txt"),
        ]
    );
    // A group of keys is not an object: it has no size and no entity tag.
    assert_eq!(entries[1].size, None);
    assert_eq!(entries[1].e_tag, None);
}

#[test]
fn a_hierarchical_account_reports_its_directories_as_such() {
    let mut body = page(
        "<Blob><Name>directory</Name><Properties>\
         <Etag>0x8DF</Etag><Content-Length>0</Content-Length>\
         <ResourceType>directory</ResourceType></Properties></Blob>\
         <BlobPrefix><Name>other/</Name><Properties>\
         <Etag>0x8E0</Etag></Properties></BlobPrefix>",
        "",
    );
    let mut entries = [ListEntry::default(); 2];
    fill(&mut body, &mut entries);

    assert_eq!(entries[0].kind, EntryKind::Directory);
    assert_eq!(entries[0].key, "directory");
    assert_eq!((entries[0].size, entries[0].e_tag), (None, None));
    // The properties that such an account attaches to a group are skipped.
    assert_eq!(entries[1].kind, EntryKind::Prefix);
    assert_eq!(entries[1].e_tag, None);
}

#[test]
fn a_name_is_decoded_where_it_stands() {
    let mut body = page(
        &(object("a&amp;b/caf&#233;.txt", 1)
            + "<Blob><Name Encoded=\"true\">a%20b%2Fc%C3%A9</Name><Properties>\
               <Content-Length>2</Content-Length></Properties></Blob>"),
        "",
    );
    let mut entries = [ListEntry::default(); 2];
    fill(&mut body, &mut entries);

    assert_eq!(entries[0].key, "a&b/café.txt");
    assert_eq!(entries[1].key, "a b/cé");

    // Written out, `false` says what leaving the attribute off says.
    let mut body = page(
        "<Blob><Name Encoded=\"false\">a%20b</Name><Properties>         <Content-Length>1</Content-Length></Properties></Blob>",
        "",
    );
    let mut entries = [ListEntry::default(); 1];
    fill(&mut body, &mut entries);
    assert_eq!(entries[0].key, "a%20b");
}

/// The keys that lean on a separator, read back out of a page.
///
/// The live suite settles that Azure stores each of these under the name it
/// was given; this settles that a page carrying one is read back the same way.
/// Together they are the round trip.
#[test]
fn a_key_that_leans_on_a_separator_is_read_back_whole() {
    let keys = [
        "directory/trailing/",
        "directory/double//slash",
        "directory/space /x",
        "directory/a.b/c",
        "directory/..leading",
    ];
    let mut body = page(
        &keys.iter().map(|key| object(key, 1)).collect::<String>(),
        "",
    );
    let mut entries = [ListEntry::default(); 5];
    let listing = fill(&mut body, &mut entries);

    assert_eq!(listing.filled, 5);
    assert_eq!(entries.map(|entry| entry.key), keys);
}

/// A page of one key with as many separators as a key can hold. The live suite
/// settles that Azure takes such a name; this settles that reading one back
/// costs the reader nothing it does not have.
#[test]
fn a_key_of_many_segments_is_one_entry_like_any_other() {
    // A separator and a segment cost two UTF-16 code units, and a key holds
    // 1024 of them.
    let key = vec!["s"; 512].join("/");
    assert_eq!(key.encode_utf16().count(), 1023);
    assert_eq!(key.matches('/').count() + 1, 512);

    let mut body = page(&object(&key, 1), "");
    let mut entries = [ListEntry::default(); 1];
    assert_eq!(fill(&mut body, &mut entries).filled, 1);
    assert_eq!(entries[0].key, key);
}

#[test]
fn a_marker_names_the_next_page_and_an_empty_one_names_none() {
    let mut body = page(&object("a.txt", 1), "2!72!MDAwMDI4");
    let mut entries = [ListEntry::default(); 1];
    let listing = fill(&mut body, &mut entries);
    assert_eq!(listing.next_marker, Some("2!72!MDAwMDI4"));

    let mut body = page(&object("a.txt", 1), "");
    assert_eq!(fill(&mut body, &mut entries).next_marker, None);

    // The service writes an absent marker as an empty element, and its own
    // serializer puts a space before the slash. Both name no next page, and
    // both are the last page of every listing that ends.
    for empty in [
        "<NextMarker/>",
        "<NextMarker />",
        "<NextMarker></NextMarker>",
        "<NextMarker>\n  </NextMarker>",
    ] {
        let mut body = document(&object("a.txt", 1), empty);
        let mut entries = [ListEntry::default(); 1];
        assert_eq!(fill(&mut body, &mut entries).next_marker, None, "{empty}");
    }

    // The same spellings, carrying a page to continue from.
    for named in [
        "<NextMarker>2!72!MDAwMDI4</NextMarker>",
        "<NextMarker>\n  2!72!MDAwMDI4\n</NextMarker>",
    ] {
        let mut body = document(&object("a.txt", 1), named);
        let mut entries = [ListEntry::default(); 1];
        assert_eq!(
            fill(&mut body, &mut entries).next_marker,
            Some("2!72!MDAwMDI4"),
            "{named}"
        );
    }

    // A container with nothing in it is the page that carries no entry and no
    // marker at all, written the way the service writes it.
    let mut body = b"<?xml version=\"1.0\" encoding=\"utf-8\"?>\
        <EnumerationResults ContainerName=\"container\">\
        <Blobs />\
        <NextMarker />\
        </EnumerationResults>"
        .to_vec();
    let listing = fill(&mut body, &mut [ListEntry::default(); 1]);
    assert_eq!((listing.filled, listing.next_marker), (0, None));
}

#[test]
fn an_array_smaller_than_the_page_is_refused_with_the_count_the_page_holds() {
    let three = object("a.txt", 1) + &object("b.txt", 2) + &object("c.txt", 3);
    let mut body = page(&three, "next");

    // The array must hold the whole page. One that does not is refused, and
    // the error says how many entries the page holds. The page is walked to
    // its end for that count, so a damaged page is still reported as one.
    let mut entries = [ListEntry::default(); 2];
    assert_eq!(
        blobs().fill_listing(&mut body, &mut entries),
        Err(Error::Capacity(CapacityError {
            required: 3,
            available: 2,
        }))
    );
}

#[test]
fn an_array_that_holds_the_page_exactly_is_read_whole() {
    let mut body = page(&(object("a.txt", 1) + &object("b.txt", 2)), "");
    let listing = fill(&mut body, &mut [ListEntry::default(); 2]);
    assert_eq!((listing.filled, listing.next_marker), (2, None));
}

#[test]
fn an_array_with_no_room_is_refused_unless_the_page_is_empty() {
    let mut body = page(&object("a.txt", 1), "next");
    let mut none: [ListEntry; 0] = [];
    assert_eq!(
        blobs().fill_listing(&mut body, &mut none),
        Err(Error::Capacity(CapacityError {
            required: 1,
            available: 0,
        }))
    );
}

#[test]
fn an_empty_page_holds_nothing_and_may_still_name_a_next() {
    let mut body = page("", "next");
    let listing = fill(&mut body, &mut []);
    assert_eq!(listing.filled, 0);
    assert_eq!(listing.next_marker, Some("next"));

    // A container with nothing in it comes back with the tag written this
    // way instead.
    let mut body = b"<?xml version=\"1.0\" encoding=\"utf-8\"?>\
        <EnumerationResults ContainerName=\"container\">\
        <Blobs/><NextMarker/></EnumerationResults>"
        .to_vec();
    let listing = fill(&mut body, &mut [ListEntry::default(); 2]);
    assert_eq!(listing.filled, 0);
    assert_eq!(listing.next_marker, None);
}

#[test]
fn the_values_the_service_writes_are_read_without_the_space_around_them() {
    // The document may be written with the values on their own lines. The key
    // is the one field that keeps its space, because a key may hold one.
    let mut body = page(
        "<Blob><Name> a.txt </Name>\n  <Properties>\n    \
         <Last-Modified>\n Sat, 22 Aug 2026 12:00:00 GMT \n</Last-Modified>\n    \
         <Etag>\n      0x8DF0046E8E555AF\n    </Etag>\n    \
         <Content-Length>  8  </Content-Length>\n  </Properties>\n</Blob>\
         <Blob><Name>d</Name><Properties><Content-Length>0</Content-Length>\
         <ResourceType>  directory  </ResourceType></Properties></Blob>",
        "",
    );
    let mut entries = [ListEntry::default(); 2];
    fill(&mut body, &mut entries);

    assert_eq!(entries[0].key, " a.txt ");
    assert_eq!(entries[0].size, Some(8));
    assert_eq!(entries[0].e_tag, Some("0x8DF0046E8E555AF"));
    assert_eq!(
        entries[0]
            .last_modified
            .map(str::as_bytes)
            .and_then(layered::http_date_ms),
        Some(1_787_400_000_000)
    );
    assert_eq!(entries[1].kind, EntryKind::Directory);
}

#[test]
fn a_body_that_is_not_a_page_is_a_fault() {
    let blobs = blobs();
    let fault = Err(Error::Response(ResponseFault::Body));

    for mut body in [
        // Not a listing at all: an error document under a success status.
        b"<Error><Code>ContainerNotFound</Code></Error>".to_vec(),
        // Not well formed.
        b"<EnumerationResults><Blobs><Blob><Name>a</Name>".to_vec(),
        // Not UTF-8.
        b"<EnumerationResults><Blobs><Blob><Name>\xff</Name></Blob></Blobs></EnumerationResults>"
            .to_vec(),
        // An object with no length.
        page(
            "<Blob><Name>a.txt</Name><Properties></Properties></Blob>",
            "",
        ),
        // An object whose length is not a number.
        page(
            "<Blob><Name>a.txt</Name><Properties><Content-Length>ten</Content-Length></Properties></Blob>",
            "",
        ),
        // An entry with no name.
        page(
            "<Blob><Properties><Content-Length>1</Content-Length></Properties></Blob>",
            "",
        ),
        // Something else where an entry belongs.
        page("<Snapshot><Name>a.txt</Name></Snapshot>", ""),
        // Tags that cross rather than nest. The tokeniser hands the names
        // over without checking them, so reading only the depth would take
        // this for an object of one byte.
        page(
            "<Blob><Name>a</Name><Properties><Content-Length>1</Properties>\
             </Content-Length></Blob>",
            "",
        ),
        b"<EnumerationResults><Blobs></EnumerationResults></Blobs>".to_vec(),
        // A comment inside a value splits its text in two. Taking either
        // piece would report a key that is not the object's.
        page("<Blob><Name>a<!-- x -->b</Name></Blob>", ""),
        // The same for a value written twice.
        page(
            "<Blob><Name>a</Name><Name>b</Name><Properties>\
             <Content-Length>1</Content-Length></Properties></Blob>",
            "",
        ),
        page(
            "<Blob><Name>a</Name><Properties><Content-Length>1</Content-Length>\
             <Content-Length>2</Content-Length></Properties></Blob>",
            "",
        ),
        // A comment or a character-data section may hold the very tags that
        // the entries are walked by.
        page("<!-- </Blobs> --><Blob><Name>a</Name></Blob>", ""),
        page(
            "<Blob><Name>a</Name><Properties><Content-Length>1</Content-Length>\
             </Properties><![CDATA[</Blob>]]></Blob>",
            "",
        ),
        // A marker in two pieces names a page that the service did not.
        document(
            &object("a.txt", 1),
            "<NextMarker>ab<!-- x -->cd</NextMarker>",
        ),
        // A document cut short leaves elements open.
        b"<EnumerationResults><Blobs><Blob><Name>a</Name></Blob>".to_vec(),
        // A namespace prefix. This crate reads the names Azure writes, and
        // would have to resolve prefixes to know these are the same names.
        b"<a:EnumerationResults><Blobs/><NextMarker/></a:EnumerationResults>".to_vec(),
        b"<EnumerationResults><Blobs/><NextMarker/></b:EnumerationResults>".to_vec(),
        page("<Blob><ns:Name>a.txt</ns:Name></Blob>", ""),
        // A reference to no entity. A listing declares none and may declare
        // none, so this is neither a reference nor text.
        page(&object("a&nbsp;b", 1), ""),
        page(&object("a&b", 1), ""),
        document(&object("a.txt", 1), "<NextMarker>a&nbsp;b</NextMarker>"),
        // A number that names no character a document may hold. Taking it
        // would put a byte in a key that no key can carry.
        page(&object("a&#0;b", 1), ""),
        page(&object("a&#xFFFE;b", 1), ""),
        page(&object("a&#xD800;b", 1), ""),
        // The marker names the next request, so it holds text and nothing
        // else.
        document(
            &object("a.txt", 1),
            "<NextMarker><Unexpected/>next</NextMarker>",
        ),
        document(
            &object("a.txt", 1),
            "<NextMarker>ne<Unexpected/>xt</NextMarker>",
        ),
        // The one attribute this crate reads, written twice or written with
        // something that is not a boolean.
        page(
            "<Blob><Name Encoded=\"true\" Encoded=\"false\">a</Name><Properties>\
             <Content-Length>1</Content-Length></Properties></Blob>",
            "",
        ),
        page(
            "<Blob><Name Encoded=\"yes\">a</Name><Properties>\
             <Content-Length>1</Content-Length></Properties></Blob>",
            "",
        ),
    ] {
        let document = String::from_utf8_lossy(&body).into_owned();
        assert_eq!(
            blobs.fill_listing(&mut body, &mut [ListEntry::default(); 4]),
            fault,
            "{document}"
        );
    }
}

#[test]
fn a_body_that_was_read_stops_being_a_document() {
    // The entries borrow the body, so the compiler keeps them from outliving
    // it. What this checks is the other half: an entry that was read was
    // decoded where it stood and is not its own text any more, while an entry
    // the array had no room for is untouched. That is why a refused read
    // cannot be retried on the same body with a larger array.
    let two = object("a&amp;b", 1) + &object("c&amp;d", 2);
    let mut body = page(&two, "");

    let mut entries = [ListEntry::default(); 1];
    assert!(matches!(
        blobs().fill_listing(&mut body, &mut entries),
        Err(Error::Capacity(_))
    ));
    assert_eq!(entries[0].key, "a&b");

    let read = String::from_utf8_lossy(&body).into_owned();
    assert!(!read.contains("<Name>a&amp;b</Name>"), "{read}");
    assert!(read.contains("<Name>c&amp;d</Name>"), "{read}");

    // The decoding left zero bytes behind, which no document holds.
    let mut entries = [ListEntry::default(); 2];
    assert_eq!(
        blobs().fill_listing(&mut body, &mut entries),
        Err(Error::Response(ResponseFault::Body))
    );
}

#[test]
fn whitespace_between_the_entries_is_not_an_entry() {
    let mut body = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
         <EnumerationResults ContainerName=\"container\">\n  \
         <Blobs>\n    {}\n    {}\n  </Blobs>\n  \
         <NextMarker>next</NextMarker>\n\
         </EnumerationResults>\n",
        object("a.txt", 1),
        object("b.txt", 2)
    )
    .into_bytes();
    let mut entries = [ListEntry::default(); 2];
    let listing = fill(&mut body, &mut entries);

    assert_eq!(listing.filled, 2);
    assert_eq!(entries[1].key, "b.txt");
    assert_eq!(listing.next_marker, Some("next"));
}

/// One page exactly as the service sent it, byte for byte, from a live run.
///
/// The hand-written pages above are this crate's idea of what Azure writes.
/// This is what it actually wrote: a byte-order mark, elements this crate
/// reads nothing from, empty elements written `<X />` with the space, and a
/// name that says it is encoded and is encoded whole, separators included.
#[test]
fn a_page_the_service_actually_sent() {
    let mut body = Vec::from(
        "\u{feff}<?xml version=\"1.0\" encoding=\"utf-8\"?>\
         <EnumerationResults ServiceEndpoint=\"https://borinkstoragetest.blob.core.windows.net/\" \
         ContainerName=\"borink-object-test\">\
         <Prefix>borink-object-storage/azure-list-scratch/</Prefix><Blobs>\
         <Blob><Name Encoded=\"true\">borink-object-storage%2Fazure-list-scratch%2F100%25-%EF%BF%BE-name.txt</Name>\
         <VersionId>2026-09-01T19:08:11.7600332Z</VersionId><IsCurrentVersion>true</IsCurrentVersion>\
         <Properties><Creation-Time>Tue, 01 Sep 2026 19:08:11 GMT</Creation-Time>\
         <Last-Modified>Tue, 01 Sep 2026 19:08:11 GMT</Last-Modified>\
         <Etag>0x8DF085C5E09984C</Etag><Content-Length>1</Content-Length>\
         <Content-Type>application/octet-stream</Content-Type><Content-Encoding />\
         <Content-Language /><Content-CRC64 /><Content-MD5>ndTkYSaMgDT1yFZOFVxnpg==</Content-MD5>\
         <Cache-Control /><Content-Disposition /><BlobType>BlockBlob</BlobType>\
         <AccessTier>Hot</AccessTier><AccessTierInferred>true</AccessTierInferred>\
         <LeaseStatus>unlocked</LeaseStatus><LeaseState>available</LeaseState>\
         <ServerEncrypted>true</ServerEncrypted></Properties><OrMetadata /></Blob>\
         </Blobs><NextMarker /></EnumerationResults>"
            .as_bytes(),
    );
    let mut entries = [ListEntry::default(); 2];
    let listing = fill(&mut body, &mut entries);

    assert_eq!(listing.filled, 1);
    // The whole name was encoded, so the separators come back as separators
    // and the character XML cannot carry comes back as itself.
    assert_eq!(
        entries[0].key,
        "borink-object-storage/azure-list-scratch/100%-\u{fffe}-name.txt"
    );
    assert_eq!(entries[0].kind, EntryKind::Object);
    assert_eq!(entries[0].size, Some(1));
    assert_eq!(entries[0].e_tag, Some("0x8DF085C5E09984C"));
    assert_eq!(
        entries[0]
            .last_modified
            .map(str::as_bytes)
            .and_then(layered::http_date_ms),
        Some(1_788_289_691_000)
    );
    // The last page of the listing, written the way the service writes it.
    assert_eq!(listing.next_marker, None);
}

// An entry as Azure writes it when the account has versioning, a tier and a
// content type: values beside the properties element, values inside it, an
// element that carries nothing, and one that carries other elements.
fn furnished(name: &str) -> String {
    format!(
        "<Blob><Name>{name}</Name>\
         <VersionId>2026-09-01T19:08:11.7600332Z</VersionId>\
         <IsCurrentVersion>true</IsCurrentVersion>\
         <Properties>\
         <Creation-Time>Tue, 01 Sep 2026 19:08:11 GMT</Creation-Time>\
         <Last-Modified>Sat, 22 Aug 2026 12:00:00 GMT</Last-Modified>\
         <Etag>0x8DF0046E8E555AF</Etag><Content-Length>1</Content-Length>\
         <Content-Type>text/plain</Content-Type><Content-Encoding />\
         <BlobType>BlockBlob</BlobType><AccessTier>Hot</AccessTier>\
         </Properties>\
         <Metadata><colour>a&amp;b</colour></Metadata><OrMetadata />\
         </Blob>"
    )
}

/// A property that this crate does not read is read from the entry itself,
/// wherever the service put it.
#[test]
fn a_property_this_crate_does_not_read_is_read_from_the_entry() {
    let mut body = page(&furnished("a.txt"), "");
    let mut entries = [ListEntry::default(); 1];
    fill(&mut body, &mut entries);
    let entry = entries[0];

    // Inside the properties element, and beside it: one call reads both.
    assert_eq!(entry.property("AccessTier"), Some(b"Hot".as_slice()));
    assert_eq!(entry.property("BlobType"), Some(b"BlockBlob".as_slice()));
    assert_eq!(
        entry.property("Content-Type"),
        Some(b"text/plain".as_slice())
    );
    assert_eq!(entry.property("IsCurrentVersion"), Some(b"true".as_slice()));
    assert_eq!(
        entry.property("VersionId"),
        Some(b"2026-09-01T19:08:11.7600332Z".as_slice())
    );

    // An element that carries nothing has an empty value, which is not the
    // same fact as a property the entry never wrote.
    assert_eq!(entry.property("Content-Encoding"), Some(b"".as_slice()));
    assert_eq!(entry.property("OrMetadata"), Some(b"".as_slice()));
    assert_eq!(entry.property("Snapshot"), None);
    // The properties element is what holds properties, not one of them.
    assert_eq!(entry.property("Properties"), None);

    // An element that holds others reports those bytes, which is what a walk
    // over the metadata of a blob starts from.
    assert_eq!(
        entry.property("Metadata"),
        Some(b"<colour>a&amp;b</colour>".as_slice())
    );
}

/// The walk reports every element of an entry once, in the order the service
/// wrote them, and reports the properties element by its contents.
#[test]
fn every_property_is_reported_once_in_the_order_the_service_wrote_them() {
    let mut body = page(&furnished("a.txt"), "");
    let mut entries = [ListEntry::default(); 1];
    fill(&mut body, &mut entries);

    let mut walk = entries[0].properties();
    let names: Vec<String> = walk
        .by_ref()
        .map(|(name, _)| String::from_utf8(name.to_vec()).unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "Name",
            "VersionId",
            "IsCurrentVersion",
            "Creation-Time",
            "Last-Modified",
            "Etag",
            "Content-Length",
            "Content-Type",
            "Content-Encoding",
            "BlobType",
            "AccessTier",
            "Metadata",
            "OrMetadata",
        ]
    );
    // A walk that ended stays ended.
    assert_eq!(walk.next(), None);
    assert_eq!(walk.next(), None);

    // A group of keys carries a name and nothing else.
    let mut body = page("<BlobPrefix><Name>nested/</Name></BlobPrefix>", "");
    let mut entries = [ListEntry::default(); 1];
    fill(&mut body, &mut entries);
    assert_eq!(entries[0].properties().count(), 1);
}

/// Reading a page decodes the values it reports where they stand, so those
/// elements no longer hold what the service wrote. The walk reports the
/// decoded text for them.
#[test]
fn a_value_that_was_decoded_in_place_is_reported_decoded_by_the_walk() {
    let mut body = page(&object("a&amp;b.txt", 4), "");
    let mut entries = [ListEntry::default(); 1];
    fill(&mut body, &mut entries);

    assert_eq!(entries[0].key, "a&b.txt");
    assert_eq!(entries[0].property("Name"), Some(b"a&b.txt".as_slice()));
    // Everything the page did not decode is what the service wrote.
    assert_eq!(
        entries[0].property("BlobType"),
        Some(b"BlockBlob".as_slice())
    );
}

/// A value that carries a reference is decoded into the caller's own buffer,
/// because the page's bytes are lent out and cannot be written again.
#[test]
fn a_listed_value_is_decoded_into_a_buffer_of_the_caller() {
    let mut into = [0; 32];
    assert_eq!(
        layered::decode_into(b"a&amp;b", &mut into),
        Some(b"a&b".as_slice())
    );
    assert_eq!(
        layered::decode_into(b"caf&#233;", &mut into),
        Some("café".as_bytes())
    );
    // Text with nothing to decode is copied as it stands.
    assert_eq!(
        layered::decode_into(b"text/plain", &mut into),
        Some(b"text/plain".as_slice())
    );
    // A buffer shorter than the value, and a reference no listing declares.
    assert_eq!(layered::decode_into(b"a&amp;b", &mut [0; 6]), None);
    assert_eq!(layered::decode_into(b"a&nbsp;b", &mut into), None);
}

/// A prefix that a hierarchical account furnishes, and an entry whose
/// properties element carries nothing: neither reports that element itself.
#[test]
fn the_properties_element_is_never_reported_as_one_of_them() {
    let mut body = page(
        "<Blob><Name>a.txt</Name><Properties><Content-Length>4</Content-Length>\
         </Properties></Blob>\
         <BlobPrefix><Name>nested/</Name><Properties /><ResourceType>directory\
         </ResourceType></BlobPrefix>",
        "",
    );
    let mut entries = [ListEntry::default(); 2];
    fill(&mut body, &mut entries);

    let names: Vec<&[u8]> = entries[1].properties().map(|(name, _)| name).collect();
    assert_eq!(names, [b"Name".as_slice(), b"ResourceType".as_slice()]);
    assert_eq!(entries[1].property("Properties"), None);
    // What follows the empty element is still walked.
    assert_eq!(
        entries[1].property("ResourceType"),
        Some(b"directory".as_slice())
    );
}

// An entry of a caller's own, holding what it asked for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Picked<'b> {
    key: &'b str,
    kind: EntryKind,
    tier: Option<&'b [u8]>,
    created: Option<&'b [u8]>,
    encoding: Option<&'b [u8]>,
    // Never in the set, so never given.
    blob_type: Option<&'b [u8]>,
}

impl<'b> Picked<'b> {
    fn build(entry: ListEntry<'b>, values: PropertyValues<'_, 'b>) -> Self {
        Self {
            key: entry.key,
            kind: entry.kind,
            tier: values.get(BlobProperty::AccessTier),
            created: values.get(BlobProperty::CreationTime),
            encoding: values.get(BlobProperty::ContentEncoding),
            blob_type: values.get(BlobProperty::BlobType),
        }
    }
}

const WANTED: PropertySet = PropertySet::of(&[
    BlobProperty::ContentEncoding,
    BlobProperty::AccessTier,
    BlobProperty::CreationTime,
]);

/// The values of the wanted properties are read in the same pass as the
/// entry, one per member of the set, as the service wrote them.
#[test]
fn the_properties_a_caller_asks_for_are_read_with_the_page() {
    let mut body = page(
        "<Blob><Name>a.txt</Name><Properties>\
         <Creation-Time>Sat, 22 Aug 2026 11:00:00 GMT</Creation-Time>\
         <Last-Modified>Sat, 22 Aug 2026 12:00:00 GMT</Last-Modified>\
         <Etag>0x8DF</Etag><Content-Length>4</Content-Length>\
         <Content-Type>text/plain</Content-Type><Content-Encoding>gzip</Content-Encoding>\
         <BlobType>BlockBlob</BlobType><AccessTier>Cool</AccessTier>\
         </Properties><OrMetadata /></Blob>\
         <Blob><Name>b.txt</Name><Properties>\
         <Last-Modified>Sat, 22 Aug 2026 12:00:00 GMT</Last-Modified>\
         <Etag>0x8E0</Etag><Content-Length>0</Content-Length>\
         <Content-Encoding /><BlobType>BlockBlob</BlobType>\
         </Properties></Blob>\
         <BlobPrefix><Name>c/</Name></BlobPrefix>",
        "",
    );
    let mut entries = [Picked::default(); 3];
    let listing = blobs()
        .fill_listing_with(&mut body, &mut entries, WANTED, Picked::build)
        .unwrap();

    assert_eq!(listing.filled, 3);
    assert_eq!(
        entries[0],
        Picked {
            key: "a.txt",
            kind: EntryKind::Object,
            tier: Some(b"Cool"),
            created: Some(b"Sat, 22 Aug 2026 11:00:00 GMT"),
            encoding: Some(b"gzip"),
            blob_type: None,
        }
    );
    // An element written empty is an empty value. One not written is none.
    assert_eq!(
        entries[1],
        Picked {
            key: "b.txt",
            kind: EntryKind::Object,
            tier: None,
            created: None,
            encoding: Some(b""),
            blob_type: None,
        }
    );
    // A group of keys gives no values.
    assert_eq!(
        entries[2],
        Picked {
            key: "c/",
            kind: EntryKind::Prefix,
            ..Picked::default()
        }
    );
}

/// The whole-tag match reads the spellings the service uses. Any other legal
/// spelling reaches the general path, which still keeps the value.
#[test]
fn a_spelling_the_service_does_not_use_is_still_read() {
    let mut body = page(
        "<Blob><Name>a.txt</Name><Properties>\
         <Last-Modified>Sat, 22 Aug 2026 12:00:00 GMT</Last-Modified>\
         <Etag>0x8DF</Etag><Content-Length>4</Content-Length>\
         <Content-Encoding/>\
         <AccessTier >Hot</AccessTier>\
         <Creation-Time\n>Sat, 22 Aug 2026 11:00:00 GMT</Creation-Time >\
         </Properties></Blob>",
        "",
    );
    let mut entries = [Picked::default(); 1];
    blobs()
        .fill_listing_with(&mut body, &mut entries, WANTED, Picked::build)
        .unwrap();
    assert_eq!(entries[0].encoding, Some(b"".as_slice()));
    assert_eq!(entries[0].tier, Some(b"Hot".as_slice()));
    assert_eq!(
        entries[0].created,
        Some(b"Sat, 22 Aug 2026 11:00:00 GMT".as_slice())
    );
}

/// The elements an account that keeps versions writes beside the properties
/// element are properties too.
#[test]
fn an_element_beside_the_properties_element_is_read_the_same_way() {
    let mut body = page(
        "<Blob><Name>a.txt</Name>\
         <VersionId>2026-09-05T06:22:39.3212012Z</VersionId>\
         <IsCurrentVersion>true</IsCurrentVersion>\
         <Properties><Last-Modified>Sat, 22 Aug 2026 12:00:00 GMT</Last-Modified>\
         <Etag>0x8DF</Etag><Content-Length>4</Content-Length></Properties>\
         <OrMetadata /></Blob>",
        "",
    );
    let set = PropertySet::of(&[BlobProperty::VersionId, BlobProperty::IsCurrentVersion]);
    let mut entries = [[None; 2]; 1];
    blobs()
        .fill_listing_with(&mut body, &mut entries, set, |_, values| {
            [
                values.get(BlobProperty::VersionId),
                values.get(BlobProperty::IsCurrentVersion),
            ]
        })
        .unwrap();
    assert_eq!(
        entries[0],
        [
            Some(b"2026-09-05T06:22:39.3212012Z".as_slice()),
            Some(b"true".as_slice())
        ]
    );
}

/// The values come in the order the enum lists the properties, whatever
/// order the set named them in, and `all` has one per member.
#[test]
fn the_values_of_a_set_stand_in_the_order_the_enum_lists_them() {
    let mut body = page(&object("a.txt", 4), "");
    let set = PropertySet::of(&[BlobProperty::BlobType, BlobProperty::AccessTier]);
    let mut all = [[None; 2]; 1];
    blobs()
        .fill_listing_with(&mut body, &mut all, set, |_, values| {
            <[_; 2]>::try_from(values.all()).unwrap()
        })
        .unwrap();
    // `AccessTier` is numbered before `BlobType`.
    assert_eq!(all[0], [None, Some(b"BlockBlob".as_slice())]);
}

/// A value read with the page is the same bytes the walk reports, so the
/// two ways of reading a property agree.
#[test]
fn a_value_read_with_the_page_is_what_the_walk_reports() {
    let mut body = page(&object("a.txt", 4), "");
    let set = PropertySet::of(&[BlobProperty::BlobType]);
    let mut entries = [(ListEntry::default(), None); 1];
    blobs()
        .fill_listing_with(&mut body, &mut entries, set, |entry, values| {
            (entry, values.get(BlobProperty::BlobType))
        })
        .unwrap();
    let (entry, blob_type) = entries[0];
    assert_eq!(blob_type, entry.property("BlobType"));
    assert_eq!(blob_type, Some(b"BlockBlob".as_slice()));
}
