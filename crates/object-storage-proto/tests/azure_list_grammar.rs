//! The document shapes the listing reader accepts, and the ones it refuses.
//!
//! `azure_list.rs` tests what a page means to a caller. This file tests how
//! the reader handles the shapes a document can take. It also has two sweeps
//! that check a damaged body is refused rather than misread. One tries every
//! truncation of a page, the other every single-byte change to one.

use borink_object_storage_proto::{
    Blobs, Container, EntryKind, Error, ListEntry, Listing, ResponseFault,
};

fn blobs() -> Blobs<'static> {
    Blobs::new(
        Container::new("https://account.blob.core.windows.net", "container").unwrap(),
        "token",
    )
    .unwrap()
}

const HEAD: &str = "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
     <EnumerationResults ServiceEndpoint=\"https://account.blob.core.windows.net/\" \
     ContainerName=\"container\"><Prefix>p/</Prefix><Delimiter>/</Delimiter>";

// One page, read into an array with room for everything it can hold.
fn read(document: &str) -> (Vec<ListEntry<'static>>, Option<String>) {
    let body: &'static mut [u8] = Vec::leak(document.as_bytes().to_vec());
    let mut into = vec![ListEntry::default(); 32];
    let listing: Listing<'static> = blobs().fill_listing(body, &mut into).unwrap();
    into.truncate(listing.filled);
    (into, listing.next_marker.map(str::to_owned))
}

fn refused(document: &str) -> bool {
    let mut body = document.as_bytes().to_vec();
    let fault = Err(Error::Response(ResponseFault::Body));
    blobs().fill_listing(&mut body, &mut [ListEntry::default(); 16]) == fault
}

#[test]
fn every_field_of_an_entry_is_read_wherever_the_document_puts_it() {
    let (entries, marker) = read(&format!(
        "{HEAD}<Blobs>\
         <Blob><Name Encoded=\"true\">a%20b%2F%26c</Name>\
         <Properties><Creation-Time>x</Creation-Time>\
         <Last-Modified>Mon, 01 Sep 2026 10:12:31 GMT</Last-Modified>\
         <Etag>0x8DC</Etag><Content-Length>12</Content-Length><Content-Type/>\
         <ResourceType>file</ResourceType></Properties>\
         <Metadata><k>v</k></Metadata></Blob>\
         <Blob><Name>dir</Name><Properties><Content-Length>0</Content-Length>\
         <ResourceType>directory</ResourceType></Properties></Blob>\
         <BlobPrefix><Name>p/q/</Name></BlobPrefix>\
         </Blobs><NextMarker>m1</NextMarker></EnumerationResults>"
    ));

    assert_eq!(marker.as_deref(), Some("m1"));
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].key, "a b/&c");
    assert_eq!(entries[0].size, Some(12));
    assert_eq!(entries[0].e_tag, Some("0x8DC"));
    assert_eq!(
        entries[0].last_modified,
        Some("Mon, 01 Sep 2026 10:12:31 GMT")
    );
    assert_eq!(entries[1].kind, EntryKind::Directory);
    assert_eq!(entries[1].key, "dir");
    assert_eq!(entries[2].kind, EntryKind::Prefix);
    assert_eq!(entries[2].key, "p/q/");
}

#[test]
fn a_document_written_for_a_person_to_read_is_still_a_page() {
    // Whitespace, a comment and a processing instruction, everywhere outside
    // the entries. There none of them can hold a tag the entries are read by.
    let (entries, marker) = read(&format!(
        "{HEAD}\n  <!-- a comment -->\n  <?instruction here?>\n  \
         <Blobs>\n    <Blob>\n      <Name>k</Name>\n      \
         <Properties>\n        <Content-Length>7</Content-Length>\n      \
         </Properties>\n    </Blob>\n  </Blobs>\n  \
         <NextMarker />\n</EnumerationResults>\n"
    ));
    assert_eq!(entries.len(), 1);
    assert_eq!((entries[0].key, entries[0].size), ("k", Some(7)));
    assert_eq!(marker, None);
}

#[test]
fn an_unusual_but_legal_spelling_takes_the_general_path() {
    let (entries, _) = read(&format!(
        "{HEAD}<Blobs><Blob ><Name >k</Name ><Properties >\
         <Content-Length >7</Content-Length ><Empty/><Also /></Properties >\
         </Blob ></Blobs ><NextMarker /></EnumerationResults >"
    ));
    assert_eq!((entries[0].key, entries[0].size), ("k", Some(7)));
}

#[test]
fn a_reference_that_spells_a_percent_sign_is_then_percent_decoded() {
    let (entries, _) = read(&format!(
        "{HEAD}<Blobs><Blob><Name Encoded=\"true\">a&#37;20b</Name>\
         <Properties><Content-Length>1</Content-Length></Properties></Blob>\
         <Blob><Name>&#65;&#x42;&#x1F600;</Name><Properties>\
         <Content-Length>2</Content-Length></Properties></Blob>\
         </Blobs><NextMarker /></EnumerationResults>"
    ));
    assert_eq!(entries[0].key, "a b");
    assert_eq!(entries[1].key, "AB\u{1f600}");
}

#[test]
fn the_shapes_this_reader_does_not_model_are_refused() {
    for document in [
        // A page that nests deeper than a listing goes.
        format!(
            "{HEAD}<Blobs><Blob><Name>a</Name><Properties><Content-Length>1</Content-Length>\
             <X>{}{}</X></Properties></Blob></Blobs></EnumerationResults>",
            "<d>".repeat(17),
            "</d>".repeat(17)
        ),
        // A document type declaration, which this crate expands nothing from.
        format!("<!DOCTYPE x>{HEAD}<Blobs /></EnumerationResults>"),
        // Text where the entries belong.
        format!("{HEAD}<Blobs>text<Blob><Name>a</Name></Blob></Blobs></EnumerationResults>"),
        // A page with no entries element at all.
        format!("{HEAD}<NextMarker>m</NextMarker></EnumerationResults>"),
        // The shapes below are refused for consistency with the rules above.
        // Azure writes none of them, so none was ever seen.
        // An entry with an empty name, written both ways.
        format!(
            "{HEAD}<Blobs><Blob><Name/><Properties><Content-Length>1</Content-Length></Properties></Blob></Blobs></EnumerationResults>"
        ),
        format!("{HEAD}<Blobs><BlobPrefix><Name></Name></BlobPrefix></Blobs></EnumerationResults>"),
        // A value that may appear once, appearing twice.
        format!(
            "{HEAD}<Blobs /><NextMarker>a</NextMarker><NextMarker>b</NextMarker></EnumerationResults>"
        ),
        format!("{HEAD}<Blobs /><Blobs /></EnumerationResults>"),
        format!(
            "{HEAD}<Blobs><Blob><Name>a</Name><Name>b</Name><Properties><Content-Length>1</Content-Length></Properties></Blob></Blobs></EnumerationResults>"
        ),
        // A zero byte, which XML forbids and which the reader writes over the
        // bytes a decoded value no longer needs: in the document, and named by
        // a percent escape.
        format!(
            "{HEAD}<Blobs><Blob><Name>a\0</Name><Properties><Content-Length>1</Content-Length></Properties></Blob></Blobs></EnumerationResults>"
        ),
        format!(
            "{HEAD}<Blobs><Blob><Name Encoded=\"true\">a%00</Name><Properties><Content-Length>1</Content-Length></Properties></Blob></Blobs></EnumerationResults>"
        ),
        // A root element that holds nothing.
        HEAD.replace("\">", "\" />"),
    ] {
        assert!(refused(&document), "{document}");
    }
}

/// A key that holds XML syntax is not read back as properties.
///
/// The key is decoded in place, so `&lt;` becomes `<` inside the bytes of the
/// entry, and Azure allows `<` and `>` in a blob name. The properties walk
/// must not read a `</Name>` or an element inside the key text as the
/// service's. It finds the end of the key by the zero bytes the decoding left
/// behind instead.
#[test]
fn a_key_that_holds_xml_syntax_is_not_read_back_as_properties() {
    let (entries, _) = read(&format!(
        "{HEAD}<Blobs><Blob>\
         <Name>k&lt;/Name&gt;&lt;AccessTier&gt;Hot&lt;/AccessTier&gt;</Name>\
         <Properties><Content-Length>1</Content-Length></Properties></Blob>\
         </Blobs><NextMarker /></EnumerationResults>"
    ));
    let key = "k</Name><AccessTier>Hot</AccessTier>";
    assert_eq!(entries[0].key, key);
    assert_eq!(entries[0].property("Name"), Some(key.as_bytes()));
    assert_eq!(entries[0].property("AccessTier"), None);
    assert_eq!(entries[0].property("Content-Length"), Some(b"1".as_slice()));
    assert_eq!(entries[0].properties().count(), 2);
}

// The page the two sweeps below damage. It holds entries with escapes in
// their names, an encoded group of keys and a marker, so every path of the
// reader is exercised.
fn sweep_page() -> Vec<u8> {
    let mut document = String::from(HEAD);
    document.push_str("<Blobs>");
    for index in 0..8 {
        document.push_str(&format!(
            "<Blob><Name>k&amp;{index}</Name><Properties>\
             <Last-Modified>t</Last-Modified><Etag>0x8DF</Etag>\
             <Content-Length>{index}</Content-Length></Properties></Blob>"
        ));
    }
    document.push_str(
        "<BlobPrefix><Name Encoded=\"true\">p%2F</Name></BlobPrefix></Blobs>\
         <NextMarker>2!72!MDAwMDI4</NextMarker></EnumerationResults>",
    );
    document.into_bytes()
}

/// A page that stops early is never a page. The reader never reaches the end
/// of the document, and reports a fault rather than the entries it did read.
#[test]
fn a_page_cut_short_at_any_byte_is_refused() {
    let page = sweep_page();
    for cut in 0..page.len() {
        let mut body = page[..cut].to_vec();
        assert_eq!(
            blobs().fill_listing(&mut body, &mut [ListEntry::default(); 16]),
            Err(Error::Response(ResponseFault::Body)),
            "a page cut at {cut} was read as one"
        );
    }
}

/// Every single-byte change to the page, to each of ten values. A change
/// inside a value, or inside an element the reader skips, may still be a
/// valid page. This checks that no change panics, and that none reports more
/// entries than the document can hold.
#[test]
fn a_page_with_one_byte_changed_is_refused_or_read_but_never_misread() {
    let page = sweep_page();
    let mut read = 0;
    for at in 0..page.len() {
        for byte in [b'<', b'>', b'&', b'/', b'%', b'"', b'=', 0, 0xFF, b' '] {
            let mut body = page.clone();
            body[at] = byte;
            let mut entries = [ListEntry::default(); 16];
            if let Ok(page) = blobs().fill_listing(&mut body, &mut entries) {
                read += 1;
                assert!(page.filled <= 9, "{} entries at {at}", page.filled);
            }
        }
    }
    assert!(read > 0);
}
