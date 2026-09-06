use super::azure::check_body;
use super::decode::decode;
use super::scan::{Child, Scan, fault, trim};
use crate::azure::{Block, BlockState};
use crate::{CapacityError, Error, Listing, Result};

pub(crate) fn fill_blocks<'b, E: From<Block<'b>>>(
    body: &'b mut [u8],
    into: &mut [E],
) -> Result<Listing<'b>> {
    let mut filled = 0;
    read(body, into.len(), &mut |part| {
        if let Some(slot) = into.get_mut(filled) {
            *slot = part.into();
        }
        // Each call consumes a distinct Block element, so filled <= body.len().
        filled += 1;
        true
    })
}

pub(crate) fn read<'b>(
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
            return fault();
        }
        if !scan.skip_misc()? {
            break;
        }
    }
    let root = scan.open()?;
    if scan.text(root.name) != b"BlockList" || !scan.text(root.attributes).trim_ascii().is_empty() {
        return fault();
    }
    let mut count = 0;
    let mut seen = [false; 2];
    if !root.empty {
        loop {
            scan.take();
            let Child::Open(section) = scan.child(b"BlockList")? else {
                break;
            };
            let (name, state, index): (&[u8], _, _) = match scan.text(section.name) {
                b"CommittedBlocks" => (b"CommittedBlocks", BlockState::Committed, 0),
                b"UncommittedBlocks" => (b"UncommittedBlocks", BlockState::Staged, 1),
                _ => return fault(),
            };
            if seen[index] || !scan.text(section.attributes).trim_ascii().is_empty() {
                return fault();
            }
            seen[index] = true;
            if section.empty {
                continue;
            }
            loop {
                scan.take();
                let Child::Open(block) = scan.child(name)? else {
                    break;
                };
                if scan.text(block.name) != b"Block"
                    || block.empty
                    || !scan.text(block.attributes).trim_ascii().is_empty()
                {
                    return fault();
                }
                let (mut id, mut size) = (None, None);
                while let Child::Open(field) = scan.child(b"Block")? {
                    if !scan.text(field.attributes).trim_ascii().is_empty() {
                        return fault();
                    }
                    match scan.text(field.name) {
                        b"Name" if id.is_none() => id = Some(scan.value(field)?),
                        b"Size" if size.is_none() => size = Some(scan.value(field)?.0),
                        _ => return fault(),
                    }
                }
                let (Some((id, flags)), Some(size)) = (id, size) else {
                    return fault();
                };
                let chunk = scan.take();
                let (start, end) = trim(chunk, size);
                let Some(size) = crate::azure::decimal(&chunk[start..end]) else {
                    return fault();
                };
                let len = decode(&mut chunk[id.0..id.1], flags, false)?;
                if len == 0 {
                    return fault();
                }
                // Scan bounds id by chunk; decode cannot grow id.0..id.1.
                let id = core::str::from_utf8(&chunk[id.0..id.0 + len]).or_else(|_| fault())?;
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
            return fault();
        }
    }
    if count > room {
        return Err(Error::Capacity(CapacityError {
            required: count,
            ..CapacityError::default()
        }));
    }
    Ok(Listing {
        filled: count,
        next_marker: None,
    })
}
