// Reads the answers of an S3 copy: the `CopyObjectResult` of a CopyObject,
// and the `CopyPartResult` of an UploadPartCopy. Each carries the entity tag
// of what the copy wrote, decoded in place.

use crate::Result;
use crate::xml::s3::read_values;
use crate::xml::scan::fault;

// Reads a `CopyObjectResult` and returns the entity tag of the copy.
pub(crate) fn read_copied(body: &mut [u8]) -> Result<&str> {
    let [e_tag] = read_values(body, b"CopyObjectResult", [b"ETag"])?;
    e_tag.map_or_else(fault, Ok)
}

// Reads a `CopyPartResult` and returns the entity tag of the part.
pub(crate) fn read_part_copied(body: &mut [u8]) -> Result<&str> {
    let [e_tag] = read_values(body, b"CopyPartResult", [b"ETag"])?;
    e_tag.map_or_else(fault, Ok)
}
