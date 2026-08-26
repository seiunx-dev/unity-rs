//! Compact lookup for FMOD's externally referenced Vorbis setup packets.

static TABLE: &[u8] = include_bytes!("fsb_vorbis_headers.bin");
const MAGIC: &[u8; 8] = b"FSBVORB1";
const HEADER_BYTES: usize = 12;
const RECORD_BYTES: usize = 12;

pub(crate) fn setup_header(crc: u32) -> Option<&'static [u8]> {
    if TABLE.get(..MAGIC.len())? != MAGIC {
        return None;
    }
    let count = usize::try_from(read_u32(TABLE, 8)?).ok()?;
    let records_end = HEADER_BYTES.checked_add(count.checked_mul(RECORD_BYTES)?)?;
    if records_end > TABLE.len() {
        return None;
    }

    let mut left = 0_usize;
    let mut right = count;
    while left < right {
        let middle = left + (right - left) / 2;
        let record = HEADER_BYTES.checked_add(middle.checked_mul(RECORD_BYTES)?)?;
        let candidate = read_u32(TABLE, record)?;
        match candidate.cmp(&crc) {
            std::cmp::Ordering::Less => left = middle + 1,
            std::cmp::Ordering::Greater => right = middle,
            std::cmp::Ordering::Equal => {
                let offset = usize::try_from(read_u32(TABLE, record + 4)?).ok()?;
                let length = usize::try_from(read_u32(TABLE, record + 8)?).ok()?;
                let end = offset.checked_add(length)?;
                if offset < records_end {
                    return None;
                }
                return TABLE.get(offset..end);
            }
        }
    }
    None
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let array: [u8; 4] = bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?;
    Some(u32::from_le_bytes(array))
}

#[cfg(test)]
mod tests {
    use super::{HEADER_BYTES, MAGIC, RECORD_BYTES, TABLE, read_u32, setup_header};

    #[test]
    fn bundled_setup_headers_are_sorted_bounded_and_complete() {
        assert_eq!(&TABLE[..MAGIC.len()], MAGIC);
        let count = usize::try_from(read_u32(TABLE, 8).unwrap()).unwrap();
        assert_eq!(count, 161);
        let records_end = HEADER_BYTES + count * RECORD_BYTES;
        let mut previous = None;
        for index in 0..count {
            let record = HEADER_BYTES + index * RECORD_BYTES;
            let crc = read_u32(TABLE, record).unwrap();
            if let Some(previous) = previous {
                assert!(previous < crc);
            }
            let setup = setup_header(crc).unwrap();
            assert!(setup.starts_with(b"\x05vorbis"));
            assert!(setup.as_ptr() >= TABLE[records_end..].as_ptr());
            previous = Some(crc);
        }
        assert!(setup_header(0).is_none());
    }
}
