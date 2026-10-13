// The Azure Get Block List body, read whole into a caller's array.
//
// This is the reading half only: `azure/blocks.rs` writes the Put Block List body
// that a commit sends, and nothing here is shared with it beyond the names
// of the tags. As for a listing, the scanner in `scan.rs` walks the
// structure, IDs are decoded in place with `decode.rs`, and each entry
// borrows the body.
//
// The reader requires every `<Block>` to carry a non-empty `<Name>` and a
// decimal `<Size>`, and refuses a repeated field or a section it does not
// know. It does not check that a name is base64: the service decided what it
// stored, and a listed ID is passed back to it unchanged. That is what fixes
// the 43-byte minimum element that `azure::max_blocks_in` divides by.

use crate::azure::{Block, BlockState};
use crate::encoding::parse_decimal;
use crate::xml::decode::decode_scanned;
use crate::xml::page::check_body;
use crate::xml::scan::{ChildTag, Scan, body_fault, trim_span};
use crate::{CapacityError, Error, Listing, Result};

pub(crate) fn fill_blocks<'b, E: From<Block<'b>>>(
    body: &'b mut [u8],
    into: &mut [E],
) -> Result<Listing<'b>> {
    let mut filled = 0;
    read_block_list(body, into.len(), &mut |block| {
        if let Some(slot) = into.get_mut(filled) {
            *slot = block.into();
        }
        // Each call consumes a distinct Block element, so filled <= body.len().
        filled += 1;
        true
    })
}

pub(crate) fn read_block_list<'b>(
    body: &'b mut [u8],
    room: usize,
    sink: &mut dyn FnMut(Block<'b>) -> bool,
) -> Result<Listing<'b>> {
    check_body(body)?;
    let mut scan = Scan::new(body);
    scan.lit(b"\xef\xbb\xbf");
    loop {
        scan.skip_space();
        if scan.cur() != b'<' {
            return body_fault();
        }
        if !scan.skip_misc()? {
            break;
        }
    }
    let root = scan.open()?;
    if scan.text(root.name) != b"BlockList" || !scan.text(root.attributes).trim_ascii().is_empty() {
        return body_fault();
    }
    let mut count = 0;
    let mut seen = [false; 2];
    if !root.empty {
        loop {
            scan.take();
            let ChildTag::Open(section) = scan.child(b"BlockList")? else {
                break;
            };
            let (name, state, index): (&[u8], _, _) = match scan.text(section.name) {
                b"CommittedBlocks" => (b"CommittedBlocks", BlockState::Committed, 0),
                b"UncommittedBlocks" => (b"UncommittedBlocks", BlockState::Staged, 1),
                _ => return body_fault(),
            };
            if seen[index] || !scan.text(section.attributes).trim_ascii().is_empty() {
                return body_fault();
            }
            seen[index] = true;
            if section.empty {
                continue;
            }
            loop {
                scan.take();
                let ChildTag::Open(block) = scan.child(name)? else {
                    break;
                };
                if scan.text(block.name) != b"Block"
                    || block.empty
                    || !scan.text(block.attributes).trim_ascii().is_empty()
                {
                    return body_fault();
                }
                let (mut id, mut size) = (None, None);
                while let ChildTag::Open(field) = scan.child(b"Block")? {
                    if !scan.text(field.attributes).trim_ascii().is_empty() {
                        return body_fault();
                    }
                    match scan.text(field.name) {
                        b"Name" if id.is_none() => id = Some(scan.value(field)?),
                        b"Size" if size.is_none() => size = Some(scan.value(field)?.0),
                        _ => return body_fault(),
                    }
                }
                let (Some((id, flags)), Some(size)) = (id, size) else {
                    return body_fault();
                };
                let chunk = scan.take();
                let (start, end) = trim_span(chunk, size);
                let Some(size) = parse_decimal(&chunk[start..end]) else {
                    return body_fault();
                };
                let len = decode_scanned(&mut chunk[id.0..id.1], flags, false)?;
                if len == 0 {
                    return body_fault();
                }
                // Scan bounds id by chunk; decode cannot grow id.0..id.1.
                let id =
                    core::str::from_utf8(&chunk[id.0..id.0 + len]).or_else(|_| body_fault())?;
                if sink(Block { id, size, state }) {
                    // Every iteration consumes a Block, bounding count by body.len().
                    count += 1;
                }
            }
        }
    }
    loop {
        scan.skip_space();
        if scan.cur() == 0 {
            break;
        }
        if scan.cur() != b'<' || !scan.skip_misc()? {
            return body_fault();
        }
    }
    if count > room {
        return Err(Error::Capacity(CapacityError { required: count }));
    }
    Ok(Listing {
        filled: count,
        next_marker: None,
    })
}
