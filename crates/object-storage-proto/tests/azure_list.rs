//! Azure listing encoding, response interpretation and page reading.

use borink_object_storage_proto::{
    Blobs, Container, EntryKind, Error, Failure, FailureClass, Fill, InvalidPlan, ListEntry,
    ListHeadOutcome, ListShape, Listing, Method, PhysicalList, ResponseFault, ResponseHead, Resume,
    ServiceErrorKind, Timestamps, layered,
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
    match blobs().fill_listing(body, into).unwrap() {
        Fill::Page(page) => page,
        Fill::Partial { filled, .. } => panic!("the array holds the page, but only {filled} fit"),
    }
}

// The same, for a call that is expected to run out of room.
fn partial<'b>(body: &'b mut [u8], into: &mut [ListEntry<'b>]) -> (usize, Resume) {
    match blobs().fill_listing(body, into).unwrap() {
        Fill::Partial { filled, resume } => (filled, resume),
        Fill::Page(page) => panic!("the array held the whole page: {page:?}"),
    }
}

fn resume<'b>(body: &'b mut [u8], at: Resume, into: &mut [ListEntry<'b>]) -> Listing<'b> {
    match blobs().resume_listing(body, at, into).unwrap() {
        Fill::Page(page) => page,
        Fill::Partial { filled, .. } => panic!("the array holds the rest, but only {filled} fit"),
    }
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
            marker: Some(b"2!72!MDAwMDI4!a+b%c"),
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
            Some(b"next"),
        )),
        format!("{base}&prefix=directory%2F&delimiter=%2F&marker=next&maxresults=2")
    );
}

#[test]
fn a_shape_and_the_borrowed_bytes_rebuild_the_plan() {
    let list = PhysicalList {
        marker: Some(b"next"),
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
                marker: Some(b""),
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
    assert_eq!(entries[0].e_tag, Some(b"0x8DF0046E8E555AF".as_slice()));
    assert_eq!(
        entries[0].last_modified.and_then(layered::http_date_ms),
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
    assert_eq!(listing.next_marker, Some(b"2!72!MDAwMDI4".as_slice()));

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
            Some(b"2!72!MDAwMDI4".as_slice()),
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
fn an_array_smaller_than_the_page_reads_the_rest_of_it_afterwards() {
    let three = object("a.txt", 1) + &object("b.txt", 2) + &object("c.txt", 3);
    let mut body = page(&three, "next");

    // The array is the budget: two entries fit, and the third is left where it
    // stands rather than counted and dropped.
    let mut entries = [ListEntry::default(); 2];
    let (filled, at) = partial(&mut body, &mut entries);
    assert_eq!(filled, 2);
    assert_eq!(entries.map(|entry| entry.key), ["a.txt", "b.txt"]);

    // The rest of the same body, from where the first call stopped. Nothing
    // was read twice and nothing was lost.
    let mut entries = [ListEntry::default(); 2];
    let listing = resume(&mut body, at, &mut entries);
    assert_eq!(listing.filled, 1);
    assert_eq!(entries[0].key, "c.txt");
    assert_eq!(entries[1], ListEntry::default());
    // Only the call that reaches the end of the page names the next one, so a
    // loop that continues on the marker cannot step over an unread entry.
    assert_eq!(listing.next_marker, Some(b"next".as_slice()));
}

// A caller that cannot hold a Rust value stores the three numbers of a
// position instead, and reading continues from what those numbers rebuild.
#[test]
fn a_position_stored_as_its_numbers_reads_the_same_rest() {
    let mut body = page(&(object("a.txt", 1) + &object("b.txt", 2)), "next");
    let mut entries = [ListEntry::default(); 1];
    let (filled, at) = partial(&mut body, &mut entries);
    assert_eq!(filled, 1);

    let stored = Resume::from_parts(at.at(), at.within(), at.marker());
    assert_eq!(stored, at);

    let mut entries = [ListEntry::default(); 1];
    let listing = resume(&mut body, stored, &mut entries);
    assert_eq!((listing.filled, entries[0].key), (1, "b.txt"));
    assert_eq!(listing.next_marker, Some(b"next".as_slice()));
}

#[test]
fn an_array_that_holds_the_page_exactly_is_not_partial() {
    let mut body = page(&(object("a.txt", 1) + &object("b.txt", 2)), "");
    let listing = fill(&mut body, &mut [ListEntry::default(); 2]);
    assert_eq!((listing.filled, listing.next_marker), (2, None));
}

#[test]
fn an_array_with_no_room_reads_nothing_and_keeps_the_page() {
    let mut body = page(&object("a.txt", 1), "next");
    let (filled, at) = partial(&mut body, &mut []);
    assert_eq!(filled, 0);

    // The page is untouched, so the whole of it is still there to read.
    let mut entries = [ListEntry::default(); 1];
    let listing = resume(&mut body, at, &mut entries);
    assert_eq!((listing.filled, entries[0].key), (1, "a.txt"));
    assert_eq!(listing.next_marker, Some(b"next".as_slice()));
}

#[test]
fn a_page_read_one_entry_at_a_time_reads_every_entry_once() {
    // The whole point of resuming: a caller with room for one entry reads a
    // page of any size without asking the service for it again. The escapes
    // are what a second read of the same bytes would corrupt.
    let mut body = page(
        &(object("a&amp;b", 1) + &object("c&amp;d", 2) + &object("e&amp;f", 3)),
        "next",
    );
    let blobs = blobs();
    let mut keys: Vec<String> = Vec::new();
    let mut at = None;
    let marker = loop {
        let mut entries = [ListEntry::default(); 1];
        let fill = match at {
            None => blobs.fill_listing(&mut body, &mut entries).unwrap(),
            Some(at) => blobs.resume_listing(&mut body, at, &mut entries).unwrap(),
        };
        match fill {
            Fill::Partial { filled, resume } => {
                keys.extend(entries[..filled].iter().map(|entry| entry.key.to_owned()));
                at = Some(resume);
            }
            Fill::Page(page) => {
                keys.extend(
                    entries[..page.filled]
                        .iter()
                        .map(|entry| entry.key.to_owned()),
                );
                break page.next_marker.map(<[u8]>::to_vec);
            }
        }
    };
    assert_eq!(keys, ["a&b", "c&d", "e&f"]);
    assert_eq!(marker.as_deref(), Some(b"next".as_slice()));
}

#[test]
fn an_empty_page_holds_nothing_and_may_still_name_a_next() {
    let mut body = page("", "next");
    let listing = fill(&mut body, &mut []);
    assert_eq!(listing.filled, 0);
    assert_eq!(listing.next_marker, Some(b"next".as_slice()));

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
    assert_eq!(entries[0].e_tag, Some(b"0x8DF0046E8E555AF".as_slice()));
    assert_eq!(
        entries[0].last_modified.and_then(layered::http_date_ms),
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
fn only_the_entries_that_were_read_stop_being_a_document() {
    // The entries borrow the body, so the compiler keeps them from outliving
    // it. What this checks is the other half: an entry that was read was
    // decoded where it stood and is not its own text any more, while an entry
    // that was not read is untouched. That is what makes resuming exact and
    // reading the same bytes twice impossible.
    let two = object("a&amp;b", 1) + &object("c&amp;d", 2);
    let mut body = page(&two, "");

    let mut entries = [ListEntry::default(); 1];
    let (filled, at) = partial(&mut body, &mut entries);
    assert_eq!((filled, entries[0].key), (1, "a&b"));

    let read = String::from_utf8_lossy(&body).into_owned();
    assert!(!read.contains("<Name>a&amp;b</Name>"), "{read}");
    assert!(read.contains("<Name>c&amp;d</Name>"), "{read}");

    let mut entries = [ListEntry::default(); 1];
    assert_eq!(resume(&mut body, at, &mut entries).filled, 1);
    assert_eq!(entries[0].key, "c&d");
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
    assert_eq!(listing.next_marker, Some(b"next".as_slice()));
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
    assert_eq!(entries[0].e_tag, Some(b"0x8DF085C5E09984C".as_slice()));
    assert_eq!(
        entries[0].last_modified.and_then(layered::http_date_ms),
        Some(1_788_289_691_000)
    );
    // The last page of the listing, written the way the service writes it.
    assert_eq!(listing.next_marker, None);
}
