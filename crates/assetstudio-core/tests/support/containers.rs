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

/// Builds an uncompressed `UnityFS` bundle.
///
/// Compression is deliberately left out: `lz4_flex` is built with decode-only
/// features here, so a compressed fixture could not be produced without adding
/// a dependency purely for tests. The block table still describes a real block,
/// so the reader's block mapping is exercised either way.
#[must_use]
pub fn unity_fs(
    version: u32,
    revision: &str,
    entries: &[BundleEntry<'_>],
    info: BlocksInfo,
) -> Vec<u8> {
    bundle("UnityFS", version, revision, entries, info)
}

/// Builds a legacy `UnityRaw` v6 bundle, which shares the `UnityFS` body but
/// carries one extra byte after the archive flags.
#[must_use]
pub fn unity_raw_v6(revision: &str, entries: &[BundleEntry<'_>]) -> Vec<u8> {
    bundle("UnityRaw", 6, revision, entries, BlocksInfo::Inline)
}

fn bundle(
    signature: &str,
    version: u32,
    revision: &str,
    entries: &[BundleEntry<'_>],
    info: BlocksInfo,
) -> Vec<u8> {
    assert!(matches!(version, 6 | 7), "bundle fixtures cover v6 and v7");
    let legacy = signature != "UnityFS";

    let mut data = Vec::new();
    let mut directory = Vec::new();
    for entry in entries {
        push_i64(&mut directory, i64::try_from(data.len()).unwrap());
        push_i64(&mut directory, i64::try_from(entry.bytes.len()).unwrap());
        push_u32(&mut directory, SERIALIZED_FILE_ENTRY);
        directory.extend_from_slice(entry.path.as_bytes());
        directory.push(0);
        data.extend_from_slice(entry.bytes);
    }

    let mut blocks_info = vec![0_u8; 16];
    push_i32(&mut blocks_info, 1);
    push_u32(&mut blocks_info, u32::try_from(data.len()).unwrap());
    push_u32(&mut blocks_info, u32::try_from(data.len()).unwrap());
    blocks_info.extend_from_slice(&0_u16.to_be_bytes());
    push_i32(&mut blocks_info, i32::try_from(entries.len()).unwrap());
    blocks_info.extend_from_slice(&directory);

    let flags = match info {
        BlocksInfo::Inline => COMBINED,
        BlocksInfo::InlineAligned => 0,
        BlocksInfo::AtEnd => COMBINED | AT_END,
    };
    let blocks_info_size = u32::try_from(blocks_info.len()).unwrap();

    let mut output = Vec::new();
    output.extend_from_slice(signature.as_bytes());
    output.push(0);
    push_u32(&mut output, version);
    output.extend_from_slice(b"5.x.x\0");
    output.extend_from_slice(revision.as_bytes());
    output.push(0);
    let size_offset = output.len();
    push_i64(&mut output, 0);
    push_u32(&mut output, blocks_info_size);
    push_u32(&mut output, blocks_info_size);
    push_u32(&mut output, flags);
    if legacy {
        output.push(0);
    }

    // The reader aligns to 16 for v7 always, and for v6 only when the revision
    // is 2019.4 or newer and the flags are not exactly the combined marker.
    if version >= 7 || (info != BlocksInfo::Inline && revision_is_2019_4_or_newer(revision)) {
        align_to_16(&mut output);
    }
    match info {
        BlocksInfo::Inline | BlocksInfo::InlineAligned => {
            output.extend_from_slice(&blocks_info);
            output.extend_from_slice(&data);
        }
        BlocksInfo::AtEnd => {
            output.extend_from_slice(&data);
            output.extend_from_slice(&blocks_info);
        }
    }
    let size = i64::try_from(output.len()).unwrap();
    output[size_offset..size_offset + 8].copy_from_slice(&size.to_be_bytes());
    output
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
