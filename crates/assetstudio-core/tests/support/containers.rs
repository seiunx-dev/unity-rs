//! Synthetic containers for the managed differential gate.
//!
//! The gate's fixtures were bare `.assets` files, so every container row in the
//! compatibility matrix rested on Rust-internal round trips. These builders wrap
//! an existing serialized file in the containers a real game ships, which lets
//! the same manifest comparison run through the container stack without needing
//! any proprietary bytes.

#![allow(dead_code)]

use std::io::Write;

/// One file inside a bundle.
pub struct BundleEntry<'a> {
    pub path: &'a str,
    pub bytes: &'a [u8],
}

/// Where a `UnityFS` bundle keeps its blocks-info table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlocksInfo {
    /// Immediately after the header, with the combined-info flag set.
    Inline,
    /// After the header and 16-byte alignment, without the combined flag.
    InlineAligned,
    /// At the very end of the bundle, after the block data.
    AtEnd,
}

const COMBINED: u32 = 0x40;
const AT_END: u32 = 0x80;
/// `kArchiveNodeFlagsSerializedFile`.
const SERIALIZED_FILE_ENTRY: u32 = 4;

/// How a bundle's block data or blocks-info table is stored.
///
/// LZ4 and LZ4HC share one block format, so the same safe block encoder can
/// exercise both of Unity's compression codes. `lzma-rust2` exposes no bulk
/// encoder, so LZMA remains outside this synthetic fixture builder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    None,
    Lzma,
    Lz4,
    Lz4Hc,
    Zstd,
}

impl Compression {
    const fn code(self) -> u32 {
        match self {
            Self::None => 0,
            Self::Lzma => 1,
            Self::Lz4 => 2,
            Self::Lz4Hc => 3,
            Self::Zstd => 5,
        }
    }

    fn apply(self, bytes: &[u8]) -> Vec<u8> {
        match self {
            Self::None => bytes.to_vec(),
            Self::Lzma => lzma_block(bytes),
            Self::Lz4 | Self::Lz4Hc => lz4_flex::block::compress(bytes),
            Self::Zstd => zstd::bulk::compress(bytes, 3).expect("zstd fixture compression"),
        }
    }
}

/// Encodes `bytes` the way Unity frames an LZMA bundle block.
///
/// A block carries the five-byte LZMA property header -- the packed properties
/// byte and the little-endian dictionary size -- followed immediately by the
/// raw range-coded stream. The `.lzma` container's eight-byte uncompressed size
/// is absent, because the block's own table already records it, so the encoder
/// is asked for a header and those eight bytes are dropped.
fn lzma_block(bytes: &[u8]) -> Vec<u8> {
    use lzma_rust2::{LzmaOptions, LzmaWriter};

    let options = LzmaOptions::with_preset(6);
    let mut encoded = Vec::new();
    let mut writer = LzmaWriter::new_use_header(&mut encoded, &options, Some(bytes.len() as u64))
        .expect("the LZMA fixture encoder starts");
    writer.write_all(bytes).expect("the LZMA fixture encodes");
    writer.finish().expect("the LZMA fixture finishes");

    let mut block = Vec::with_capacity(encoded.len() - 8);
    block.extend_from_slice(&encoded[..5]);
    block.extend_from_slice(&encoded[13..]);
    block
}

/// How one bundle fixture is laid out.
pub struct BundleLayout<'a> {
    pub version: u32,
    pub revision: &'a str,
    pub info: BlocksInfo,
    /// Compression of the block holding the entry payloads.
    pub blocks: Compression,
    /// Compression of the blocks-info and directory table.
    pub directory: Compression,
}

impl<'a> BundleLayout<'a> {
    /// An uncompressed v6 bundle with a combined inline blocks-info table.
    #[must_use]
    pub const fn v6(revision: &'a str) -> Self {
        Self {
            version: 6,
            revision,
            info: BlocksInfo::Inline,
            blocks: Compression::None,
            directory: Compression::None,
        }
    }
}

/// Builds a `UnityFS` bundle.
#[must_use]
pub fn unity_fs(layout: &BundleLayout<'_>, entries: &[BundleEntry<'_>]) -> Vec<u8> {
    bundle("UnityFS", layout, entries)
}

/// Builds a legacy `UnityRaw` v6 bundle, which shares the `UnityFS` body but
/// carries one extra byte after the archive flags.
#[must_use]
pub fn unity_raw_v6(revision: &str, entries: &[BundleEntry<'_>]) -> Vec<u8> {
    bundle("UnityRaw", &BundleLayout::v6(revision), entries)
}

fn bundle(signature: &str, layout: &BundleLayout<'_>, entries: &[BundleEntry<'_>]) -> Vec<u8> {
    let BundleLayout {
        version,
        revision,
        info,
        blocks,
        directory,
    } = *layout;
    // 8 shares v7's header and blocks info; the writer below already keys the
    // alignment off `>= 7`, so it needs nothing else to emit one.
    assert!(
        matches!(version, 6..=8),
        "bundle fixtures cover v6, v7 and v8"
    );
    let legacy = signature != "UnityFS";

    let mut data = Vec::new();
    let mut entry_table = Vec::new();
    for entry in entries {
        push_i64(&mut entry_table, i64::try_from(data.len()).unwrap());
        push_i64(&mut entry_table, i64::try_from(entry.bytes.len()).unwrap());
        push_u32(&mut entry_table, SERIALIZED_FILE_ENTRY);
        entry_table.extend_from_slice(entry.path.as_bytes());
        entry_table.push(0);
        data.extend_from_slice(entry.bytes);
    }
    let stored_data = blocks.apply(&data);

    let mut blocks_info = vec![0_u8; 16];
    push_i32(&mut blocks_info, 1);
    push_u32(&mut blocks_info, u32::try_from(data.len()).unwrap());
    push_u32(&mut blocks_info, u32::try_from(stored_data.len()).unwrap());
    blocks_info.extend_from_slice(&u16::try_from(blocks.code()).unwrap().to_be_bytes());
    push_i32(&mut blocks_info, i32::try_from(entries.len()).unwrap());
    blocks_info.extend_from_slice(&entry_table);
    let stored_blocks_info = directory.apply(&blocks_info);

    let flags = directory.code()
        | match info {
            BlocksInfo::Inline => COMBINED,
            BlocksInfo::InlineAligned => 0,
            BlocksInfo::AtEnd => COMBINED | AT_END,
        };

    let mut output = Vec::new();
    output.extend_from_slice(signature.as_bytes());
    output.push(0);
    push_u32(&mut output, version);
    output.extend_from_slice(b"5.x.x\0");
    output.extend_from_slice(revision.as_bytes());
    output.push(0);
    let size_offset = output.len();
    push_i64(&mut output, 0);
    push_u32(
        &mut output,
        u32::try_from(stored_blocks_info.len()).unwrap(),
    );
    push_u32(&mut output, u32::try_from(blocks_info.len()).unwrap());
    push_u32(&mut output, flags);
    if legacy {
        output.push(0);
    }

    // The reader aligns to 16 for v7 always, and for v6 only when the revision
    // is 2019.4 or newer and the flags are not exactly the combined marker.
    if version >= 7 || (flags != COMBINED && revision_is_2019_4_or_newer(revision)) {
        align_to_16(&mut output);
    }
    match info {
        BlocksInfo::Inline | BlocksInfo::InlineAligned => {
            output.extend_from_slice(&stored_blocks_info);
            output.extend_from_slice(&stored_data);
        }
        BlocksInfo::AtEnd => {
            output.extend_from_slice(&stored_data);
            output.extend_from_slice(&stored_blocks_info);
        }
    }
    let size = i64::try_from(output.len()).unwrap();
    output[size_offset..size_offset + 8].copy_from_slice(&size.to_be_bytes());
    output
}

/// Builds a `UnityWebData1.0` archive, the WebGL player's asset container.
///
/// The header is a null-terminated signature, the byte length of the header
/// itself, then one record per entry: data offset, data length, path length and
/// the path. Entry payloads follow at the offsets the records name.
#[must_use]
pub fn unity_web_data(signature: &str, entries: &[BundleEntry<'_>]) -> Vec<u8> {
    let mut header_length = signature.len() + 1 + 4;
    for entry in entries {
        header_length += 12 + entry.path.len();
    }

    let mut output = Vec::new();
    output.extend_from_slice(signature.as_bytes());
    output.push(0);
    output.extend_from_slice(&u32::try_from(header_length).unwrap().to_le_bytes());
    let mut offset = header_length;
    for entry in entries {
        output.extend_from_slice(&u32::try_from(offset).unwrap().to_le_bytes());
        output.extend_from_slice(&u32::try_from(entry.bytes.len()).unwrap().to_le_bytes());
        output.extend_from_slice(&u32::try_from(entry.path.len()).unwrap().to_le_bytes());
        output.extend_from_slice(entry.path.as_bytes());
        offset += entry.bytes.len();
    }
    assert_eq!(
        output.len(),
        header_length,
        "the header length must be exact"
    );
    for entry in entries {
        output.extend_from_slice(entry.bytes);
    }
    output
}

/// Builds a stored (uncompressed) ZIP archive holding `entries`.
///
/// Stored rather than deflated so the fixture stays byte-predictable; the
/// container dispatch and directory walk are what this exercises, not the
/// inflate path, which the block compressions already cover.
#[must_use]
pub fn zip_archive(entries: &[BundleEntry<'_>]) -> Vec<u8> {
    use std::io::Cursor;
    use zip::write::SimpleFileOptions;

    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for entry in entries {
        writer.start_file(entry.path, options).unwrap();
        writer.write_all(entry.bytes).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

/// Wraps `bytes` in a gzip stream, which the loader unwraps before dispatch.
#[must_use]
pub fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

fn revision_is_2019_4_or_newer(revision: &str) -> bool {
    let mut parts = revision.split('.');
    let major: u32 = parts.next().unwrap_or("0").parse().unwrap_or(0);
    let minor: u32 = parts
        .next()
        .unwrap_or("0")
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .unwrap_or(0);
    (major, minor) >= (2019, 4)
}

fn align_to_16(output: &mut Vec<u8>) {
    while output.len() % 16 != 0 {
        output.push(0);
    }
}

fn push_i32(output: &mut Vec<u8>, value: i32) {
    output.extend_from_slice(&value.to_be_bytes());
}

fn push_u32(output: &mut Vec<u8>, value: u32) {
    output.extend_from_slice(&value.to_be_bytes());
}

fn push_i64(output: &mut Vec<u8>, value: i64) {
    output.extend_from_slice(&value.to_be_bytes());
}
