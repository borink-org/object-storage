//! One realistic Azure listing page.

/// Builds a page of `entries` objects and 64 groups of keys, with the
/// properties Azure writes for a block blob. Every 17th key holds a space and
/// an `&`, so it is written percent-encoded with `Encoded="true"` the way
/// Azure writes a name that XML cannot hold.
pub fn azure(entries: usize) -> Vec<u8> {
    let mut s = String::with_capacity(entries * 700);
    s.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
    s.push_str("<EnumerationResults ServiceEndpoint=\"https://acct.blob.core.windows.net/\" ContainerName=\"data\">");
    s.push_str("<Prefix>db/wal/</Prefix><MaxResults>5000</MaxResults><Delimiter>/</Delimiter>");
    s.push_str("<Blobs>");
    for i in 0..entries {
        if i % 17 == 0 {
            s.push_str(&format!(
                "<Blob><Name Encoded=\"true\">db/wal/segment%20{i:06}&amp;log.sst</Name>"
            ));
        } else {
            s.push_str(&format!("<Blob><Name>db/wal/{i:06}.sst</Name>"));
        }
        s.push_str("<Properties>");
        s.push_str("<Creation-Time>Mon, 01 Sep 2026 10:00:00 GMT</Creation-Time>");
        s.push_str("<Last-Modified>Mon, 01 Sep 2026 10:12:31 GMT</Last-Modified>");
        s.push_str("<Etag>0x8DC7A1B2C3D4E5F</Etag>");
        s.push_str("<Content-Length>1048576</Content-Length>");
        s.push_str("<Content-Type>application/octet-stream</Content-Type>");
        s.push_str("<Content-Encoding /><Content-Language /><Content-CRC64 />");
        s.push_str("<Content-MD5>rL0Y20zC+Fzt72VPzMSk2A==</Content-MD5>");
        s.push_str("<Cache-Control /><Content-Disposition />");
        s.push_str("<BlobType>BlockBlob</BlobType><AccessTier>Hot</AccessTier>");
        s.push_str("<AccessTierInferred>true</AccessTierInferred>");
        s.push_str("<LeaseStatus>unlocked</LeaseStatus><LeaseState>available</LeaseState>");
        s.push_str("<ServerEncrypted>true</ServerEncrypted>");
        s.push_str("</Properties><OrMetadata /></Blob>");
    }
    for i in 0..64 {
        s.push_str(&format!(
            "<BlobPrefix><Name>db/wal/part{i:03}/</Name></BlobPrefix>"
        ));
    }
    s.push_str("</Blobs><NextMarker>2!108!MDAwMDI5IWRiL3dhbC8wMDUwMDAuc3N0ITAwMDAyOCE5OTk5LTEyLTMxVDIzOjU5OjU5Ljk5OTk5OTlaIQ--</NextMarker>");
    s.push_str("</EnumerationResults>");
    s.into_bytes()
}
