//! Converts fsbex's licensed Rust setup-header constants into a compact table.
//!
//! Usage:
//! `rustc --edition 2021 tools/build_fsb_vorbis_table.rs -o /tmp/build-fsb-vorbis-table`
//! `/tmp/build-fsb-vorbis-table <vorbis_lookup.rs> <output.bin>`

use std::{
    env, fs,
    io::{self, Write},
    path::Path,
};

const MAGIC: &[u8; 8] = b"FSBVORB1";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = env::args_os().skip(1);
    let input = arguments.next().ok_or("missing input path")?;
    let output = arguments.next().ok_or("missing output path")?;
    if arguments.next().is_some() {
        return Err("expected exactly two paths".into());
    }

    let source = fs::read_to_string(input)?;
    let mut entries = parse_constants(&source)?;
    entries.sort_unstable_by_key(|entry| entry.0);
    if entries.len() != 161 {
        return Err(format!("expected 161 setup headers, found {}", entries.len()).into());
    }
    for pair in entries.windows(2) {
        if pair[0].0 == pair[1].0 {
            return Err(format!("duplicate setup CRC {:08x}", pair[0].0).into());
        }
    }

    let record_bytes = entries
        .len()
        .checked_mul(12)
        .ok_or("record table size overflowed")?;
    let payload_start = MAGIC
        .len()
        .checked_add(4)
        .and_then(|size| size.checked_add(record_bytes))
        .ok_or("table header size overflowed")?;
    let mut payload_offset = u32::try_from(payload_start)?;
    let mut encoded = Vec::new();
    encoded.try_reserve_exact(
        payload_start + entries.iter().map(|entry| entry.1.len()).sum::<usize>(),
    )?;
    encoded.extend_from_slice(MAGIC);
    encoded.extend_from_slice(&u32::try_from(entries.len())?.to_le_bytes());
    for (crc, bytes) in &entries {
        encoded.extend_from_slice(&crc.to_le_bytes());
        encoded.extend_from_slice(&payload_offset.to_le_bytes());
        encoded.extend_from_slice(&u32::try_from(bytes.len())?.to_le_bytes());
        payload_offset = payload_offset
            .checked_add(u32::try_from(bytes.len())?)
            .ok_or("setup payload offset overflowed")?;
    }
    for (_, bytes) in &entries {
        encoded.extend_from_slice(bytes);
    }

    write_atomic(Path::new(&output), &encoded)?;
    eprintln!(
        "wrote {} setup headers in {} bytes",
        entries.len(),
        encoded.len()
    );
    Ok(())
}

fn parse_constants(source: &str) -> Result<Vec<(u32, Vec<u8>)>, Box<dyn std::error::Error>> {
    let mut entries = Vec::new();
    let mut remainder = source;
    while let Some(start) = remainder.find("const FVS_") {
        remainder = &remainder[start + "const FVS_".len()..];
        let crc_text = remainder.get(..8).ok_or("truncated constant name")?;
        let crc = u32::from_str_radix(crc_text, 16)?;
        let array_start = remainder.find("[").ok_or("missing array type")?;
        let value_start = remainder[array_start..]
            .find("= [")
            .map(|offset| array_start + offset + 3)
            .ok_or("missing array initializer")?;
        let value_end = remainder[value_start..]
            .find("];")
            .map(|offset| value_start + offset)
            .ok_or("unterminated array initializer")?;
        let mut bytes = Vec::new();
        for token in remainder[value_start..value_end].split(',') {
            let token = token.trim();
            if token.is_empty() {
                continue;
            }
            let hexadecimal = token.strip_prefix("0x").ok_or("non-hex byte token")?;
            bytes.push(u8::from_str_radix(hexadecimal, 16)?);
        }
        if bytes.is_empty() {
            return Err(format!("setup CRC {crc:08x} is empty").into());
        }
        entries.push((crc, bytes));
        remainder = &remainder[value_end + 2..];
    }
    Ok(entries)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut temporary = path.to_path_buf();
    temporary.set_extension("bin.tmp");
    let mut file = fs::File::create(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(temporary, path)
}
