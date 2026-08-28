use std::cmp::{max, min};
use std::fmt;
use std::io::{Cursor, Read, Seek, Write};
use std::str::FromStr;
use std::sync::Arc;

use crate::endian::{Endian, EndianReader, checked_length};
use crate::source::Region;
use crate::unity_cn::{
    ARCHIVE_ENCRYPTED_FLAG as UNITY_CN_ENCRYPTED_FLAG, ArchiveDecryptor,
    HEADER_BYTES as UNITY_CN_HEADER_BYTES, UnityCnHeader, UnityCnKey,
};
use crate::unity_version::UnityVersion;
use crate::{Error, Result};

const ARCHIVE_COMPRESSION_MASK: u32 = 0x3f;
const BLOCKS_AND_DIRECTORY_INFO_COMBINED: u32 = 0x40;
const BLOCKS_INFO_AT_END: u32 = 0x80;
const BLOCK_INFO_NEEDS_PADDING_AT_START: u32 = 0x200;
const UNITY_CN_V1_FLAG: u32 = 0x200;
const UNITY_CN_V2_V3_FLAGS: u32 = 0x1400;
const STORAGE_BLOCK_COMPRESSION_MASK: u16 = 0x3f;

/// Common prefix shared by `UnityWeb`, `UnityRaw`, `UnityArchive`, and `UnityFS`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleHeader {
    pub signature: String,
    pub version: u32,
    pub unity_version: String,
    pub unity_revision: UnityVersion,
}

impl BundleHeader {
    pub fn read<R: Read + Seek>(reader: &mut EndianReader<R>) -> Result<Self> {
        reader.set_endian(Endian::Big);
        let signature = reader.read_c_string_required(32_767, "bundle signature")?;
        if !matches!(
            signature.as_str(),
            "UnityWeb" | "UnityRaw" | "UnityArchive" | "UnityFS"
        ) {
            return Err(Error::invalid_data(format!(
                "unsupported Unity bundle signature: {signature:?}"
            )));
        }
        let version = reader.read_u32()?;
        let unity_version = reader.read_c_string_required(32_767, "bundle generator version")?;
        let revision = reader.read_c_string_required(32_767, "bundle Unity revision")?;
        let unity_revision = if revision.is_empty() {
            UnityVersion::default()
        } else {
            UnityVersion::from_str(&revision).map_err(|error| {
                Error::invalid_data(format!("invalid bundle Unity revision: {error}"))
            })?
        };

        Ok(Self {
            signature,
            version,
            unity_version,
            unity_revision,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompressionType {
    None,
    Lzma,
    Lz4,
    Lz4Hc,
    Lzham,
    Zstd,
    Oodle,
    Unknown(u8),
}

impl CompressionType {
    #[must_use]
    pub const fn from_code(code: u8) -> Self {
        match code {
            0 => Self::None,
            1 => Self::Lzma,
            2 => Self::Lz4,
            3 => Self::Lz4Hc,
            4 => Self::Lzham,
            5 => Self::Zstd,
            6 => Self::Oodle,
            value => Self::Unknown(value),
        }
    }

    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Lzma => 1,
            Self::Lz4 => 2,
            Self::Lz4Hc => 3,
            Self::Lzham => 4,
            Self::Zstd => 5,
            Self::Oodle => 6,
            Self::Unknown(value) => value,
        }
    }
}

/// Safe boundary for an externally supplied Oodle implementation.
///
/// The core allocates an initialized destination of the exact declared size
/// and verifies the returned byte count. Implementations should translate the
/// native decoder's signed result into either a byte count or an error.
pub trait OodleDecoder: Send + Sync {
    fn decompress(&self, input: &[u8], output: &mut [u8]) -> Result<usize>;
}

#[derive(Clone, Default)]
struct OodleDecoderSlot(Option<Arc<dyn OodleDecoder>>);

impl OodleDecoderSlot {
    fn as_deref(&self) -> Option<&dyn OodleDecoder> {
        self.0.as_deref()
    }
}

impl fmt::Debug for OodleDecoderSlot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("OodleDecoderSlot")
            .field(&self.0.as_ref().map(|_| "<configured>"))
            .finish()
    }
}

impl PartialEq for OodleDecoderSlot {
    fn eq(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (Some(left), Some(right)) => Arc::ptr_eq(left, right),
            (None, None) => true,
            (Some(_), None) | (None, Some(_)) => false,
        }
    }
}

impl Eq for OodleDecoderSlot {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArchiveFlags(pub u32);

impl ArchiveFlags {
    #[must_use]
    pub const fn compression(self) -> CompressionType {
        CompressionType::from_code((self.0 & ARCHIVE_COMPRESSION_MASK) as u8)
    }

    #[must_use]
    pub const fn blocks_info_at_end(self) -> bool {
        self.0 & BLOCKS_INFO_AT_END != 0
    }

    #[must_use]
    pub const fn block_info_needs_padding(self) -> bool {
        self.0 & BLOCK_INFO_NEEDS_PADDING_AT_START != 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnityFsHeader {
    pub common: BundleHeader,
    pub size: u64,
    pub compressed_blocks_info_size: u32,
    pub uncompressed_blocks_info_size: u32,
    pub flags: ArchiveFlags,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageBlock {
    pub uncompressed_size: u32,
    pub compressed_size: u32,
    pub flags: u16,
    pub compression: CompressionType,
    /// Offset of this block's compressed bytes within the bound bundle region.
    pub compressed_offset: u64,
    /// Logical offset of this block in the concatenated uncompressed stream.
    pub uncompressed_offset: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleEntry {
    pub offset: u64,
    pub size: u64,
    pub flags: u32,
    pub path: String,
    pub file_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnityFsBundle {
    pub header: UnityFsHeader,
    pub blocks_info_hash: [u8; 16],
    pub blocks: Vec<StorageBlock>,
    pub entries: Vec<BundleEntry>,
    /// Position within the bundle region at which block bytes begin.
    pub data_offset: u64,
    /// Position immediately after this bundle, relative to its bound region.
    pub bundle_end: u64,
    region: Region,
    entry_read_limit: u64,
    max_lzma_memory_kib: u32,
    max_zstd_window_log: u32,
    oodle_decoder: OodleDecoderSlot,
    decryptor: Option<ArchiveDecryptor>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BundleParseLimits {
    pub max_blocks_info_size: u64,
    pub max_blocks: usize,
    pub max_entries: usize,
    pub max_path_length: usize,
    pub max_entry_read_size: u64,
    pub max_compressed_block_size: u64,
    pub max_uncompressed_block_size: u64,
    pub max_lzma_memory_kib: u32,
    pub max_zstd_window_log: u32,
}

impl Default for BundleParseLimits {
    fn default() -> Self {
        Self {
            max_blocks_info_size: 256 * 1024 * 1024,
            max_blocks: 1_000_000,
            max_entries: 1_000_000,
            max_path_length: 32_767,
            max_entry_read_size: 256 * 1024 * 1024,
            max_compressed_block_size: 512 * 1024 * 1024,
            max_uncompressed_block_size: 512 * 1024 * 1024,
            max_lzma_memory_kib: 512 * 1024,
            max_zstd_window_log: 29,
        }
    }
}

/// Options used both while opening a `UnityFS` bundle and while lazily reading
/// its entries.
#[derive(Clone, Default)]
pub struct BundleOpenOptions {
    pub limits: BundleParseLimits,
    pub oodle_decoder: Option<Arc<dyn OodleDecoder>>,
    /// Key for UnityCN-encrypted bundles. Without one they stay refused.
    pub unity_cn_key: Option<UnityCnKey>,
}

impl fmt::Debug for BundleOpenOptions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BundleOpenOptions")
            .field("limits", &self.limits)
            .field(
                "oodle_decoder",
                &self.oodle_decoder.as_ref().map(|_| "<configured>"),
            )
            .field("unity_cn_key", &self.unity_cn_key)
            .finish()
    }
}

impl UnityFsBundle {
    pub fn open(region: &Region) -> Result<Self> {
        Self::open_with_options(region, BundleOpenOptions::default())
    }

    pub fn open_with_limits(region: &Region, limits: BundleParseLimits) -> Result<Self> {
        Self::open_with_options(
            region,
            BundleOpenOptions {
                limits,
                ..BundleOpenOptions::default()
            },
        )
    }

    // Keeping the header, blocks-info location, and logical block mapping in one
    // linear routine makes the format's position transitions auditable.
    #[allow(clippy::too_many_lines)]
    pub fn open_with_options(region: &Region, options: BundleOpenOptions) -> Result<Self> {
        let BundleOpenOptions {
            limits,
            oodle_decoder,
            unity_cn_key,
        } = options;
        let mut reader = EndianReader::new(region.cursor(), Endian::Big);
        let bundle_start = reader.position()?;
        let common = BundleHeader::read(&mut reader)?;
        // 8 is the version modern Unity writes, and its header and blocks
        // info are v7's -- the managed reader does not distinguish them at
        // all, branching only on `>= 7`. Refusing it meant refusing every
        // bundle a current game ships; four real ones read identically to
        // UnityPy once it is accepted. Later versions stay refused rather
        // than assumed compatible, because that is the difference between
        // reading a file and guessing at it.
        let is_legacy_v6 = validate_unity_fs_signature(&common)?;

        let size = non_negative_i64(reader.read_i64()?, "UnityFS bundle size")?;
        let compressed_blocks_info_size = reader.read_u32()?;
        let uncompressed_blocks_info_size = reader.read_u32()?;
        let flags = ArchiveFlags(reader.read_u32()?);
        if is_legacy_v6 {
            reader.read_u8()?;
        }
        let bundle_end = bundle_start
            .checked_add(size)
            .ok_or_else(|| Error::invalid_data("UnityFS bundle end overflowed"))?;
        let stream_length = reader.len()?;
        if bundle_end > stream_length {
            return Err(Error::invalid_data(format!(
                "UnityFS bundle ends at {bundle_end}, beyond stream length {stream_length}"
            )));
        }
        if bundle_end < reader.position()? {
            return Err(Error::invalid_data(
                "UnityFS declared size is smaller than its header",
            ));
        }

        let compressed_size = u64::from(compressed_blocks_info_size);
        let uncompressed_size = u64::from(uncompressed_blocks_info_size);
        validate_blocks_info_sizes(compressed_size, uncompressed_size, limits)?;
        // UnityCN puts its encryption header immediately after the archive
        // flags, before the alignment the ordinary layout applies.
        let decryptor = if has_unity_cn_flag(&common, flags) {
            let Some(key) = unity_cn_key else {
                return Err(Error::unsupported(
                    "UnityCN-encrypted UnityFS bundles need a caller-supplied key",
                ));
            };
            let length = usize::try_from(UNITY_CN_HEADER_BYTES).map_err(|_| {
                Error::invalid_data("UnityCN header size does not fit this platform")
            })?;
            let header = UnityCnHeader::parse(reader.read_bytes(length)?.as_slice())?;
            Some(ArchiveDecryptor::new(&header, key)?)
        } else {
            None
        };

        align_blocks_info(&mut reader, bundle_start, &common, flags, bundle_end)?;
        let blocks_info_return_position = reader.position()?;

        let blocks_info_offset = if flags.blocks_info_at_end() {
            bundle_end
                .checked_sub(compressed_size)
                .ok_or_else(|| Error::invalid_data("UnityFS blocks info exceeds bundle size"))?
        } else {
            blocks_info_return_position
        };
        let blocks_info_end = blocks_info_offset
            .checked_add(compressed_size)
            .ok_or_else(|| Error::invalid_data("UnityFS blocks info range overflowed"))?;
        if blocks_info_offset < bundle_start || blocks_info_end > bundle_end {
            return Err(Error::invalid_data(
                "UnityFS blocks info is outside the declared bundle range",
            ));
        }
        if flags.blocks_info_at_end() && blocks_info_offset < blocks_info_return_position {
            return Err(Error::invalid_data(
                "tail UnityFS blocks info overlaps the bundle header",
            ));
        }

        reader.set_position(blocks_info_offset)?;
        let compressed_length = usize::try_from(compressed_size).map_err(|_| {
            Error::invalid_data("UnityFS blocks info is too large for this platform")
        })?;
        let blocks_info_bytes = reader.read_bytes(compressed_length)?;
        if flags.blocks_info_at_end() {
            reader.set_position(blocks_info_return_position)?;
        }

        let mut blocks_info_bytes = blocks_info_bytes;
        if let Some(decryptor) = &decryptor
            && flags.0 & UNITY_CN_ENCRYPTED_FLAG != 0
            && flags.compression() != CompressionType::None
        {
            decryptor.decrypt_block(&mut blocks_info_bytes, 0)?;
        }
        let blocks_info = decompress_blocks_info(
            flags.compression(),
            blocks_info_bytes,
            uncompressed_size,
            limits,
            oodle_decoder.as_deref(),
        )?;
        let (blocks_info_hash, mut blocks, entries) = parse_blocks_info(&blocks_info, limits)?;

        if block_info_needs_padding(&common, flags) {
            align_relative(&mut reader, bundle_start, 16, bundle_end)?;
        }
        let data_offset = reader.position()?;
        let block_data_limit = if flags.blocks_info_at_end() {
            blocks_info_offset
        } else {
            bundle_end
        };

        let (compressed_offset, uncompressed_offset) =
            layout_storage_blocks(&mut blocks, data_offset, limits)?;
        if compressed_offset > block_data_limit {
            return Err(Error::invalid_data(format!(
                "UnityFS block data ends at {compressed_offset}, beyond its limit {block_data_limit}"
            )));
        }
        validate_bundle_entries(&entries, uncompressed_offset)?;

        let bundle_region = region.subregion(bundle_start, size)?;

        Ok(Self {
            header: UnityFsHeader {
                common,
                size,
                compressed_blocks_info_size,
                uncompressed_blocks_info_size,
                flags,
            },
            blocks_info_hash,
            blocks,
            entries,
            data_offset,
            bundle_end,
            region: bundle_region,
            entry_read_limit: limits.max_entry_read_size,
            max_lzma_memory_kib: limits.max_lzma_memory_kib,
            max_zstd_window_log: limits.max_zstd_window_log,
            oodle_decoder: OodleDecoderSlot(oodle_decoder),
            decryptor,
        })
    }

    #[must_use]
    pub fn entry_by_path(&self, path: &str) -> Option<&BundleEntry> {
        self.entries.iter().find(|entry| entry.path == path)
    }

    /// Streams one bundle entry to `output` without materializing the complete
    /// uncompressed bundle.
    /// Reads one block's stored bytes, decrypting them when the block is marked
    /// encrypted.
    ///
    /// `UnityCN` obfuscates only compressed blocks; an uncompressed block is
    /// stored in the clear and never reaches here.
    fn block_bytes(&self, block: &StorageBlock, block_index: usize) -> Result<Vec<u8>> {
        let mut bytes = self
            .region
            .subregion(block.compressed_offset, u64::from(block.compressed_size))?
            .read_to_vec(u64::from(block.compressed_size))?;
        if let Some(decryptor) = &self.decryptor
            && u32::from(block.flags) & UNITY_CN_ENCRYPTED_FLAG != 0
        {
            decryptor.decrypt_block(&mut bytes, block_index)?;
        }
        Ok(bytes)
    }

    pub fn copy_entry<W: Write>(&self, index: usize, output: &mut W) -> Result<u64> {
        self.copy_entry_with_cache(index, output, &mut BlockDecodeCache::new())
    }

    /// [`Self::copy_entry`] reusing the caller's decoded-block cache across
    /// calls.
    pub fn copy_entry_with_cache<W: Write>(
        &self,
        index: usize,
        output: &mut W,
        cache: &mut BlockDecodeCache,
    ) -> Result<u64> {
        self.copy_entry_range(index, u64::MAX, output, cache)
    }

    /// Copies at most the first `limit` bytes of one entry.
    ///
    /// The walk stops as soon as the prefix is covered, so a header probe of
    /// a many-block entry touches one block instead of all of them. Unlike
    /// the full copy, the caller is responsible for knowing that a short
    /// result is intentional; the returned count always equals
    /// `min(limit, entry.size)` or the copy failed.
    pub fn copy_entry_prefix<W: Write>(
        &self,
        index: usize,
        limit: u64,
        output: &mut W,
        cache: &mut BlockDecodeCache,
    ) -> Result<u64> {
        self.copy_entry_range(index, limit, output, cache)
    }

    fn copy_entry_range<W: Write>(
        &self,
        index: usize,
        limit: u64,
        output: &mut W,
        cache: &mut BlockDecodeCache,
    ) -> Result<u64> {
        let entry = self.entries.get(index).ok_or_else(|| {
            Error::invalid_data(format!("UnityFS entry index {index} is out of range"))
        })?;
        let wanted = entry.size.min(limit);
        let entry_end = entry
            .offset
            .checked_add(wanted)
            .ok_or_else(|| Error::invalid_data("UnityFS entry range overflowed"))?;
        let mut written = 0_u64;

        for (block_index, block) in self.blocks.iter().enumerate() {
            let block_end = block
                .uncompressed_offset
                .checked_add(u64::from(block.uncompressed_size))
                .ok_or_else(|| Error::invalid_data("UnityFS block range overflowed"))?;
            let overlap_start = max(entry.offset, block.uncompressed_offset);
            let overlap_end = min(entry_end, block_end);
            if overlap_start >= overlap_end {
                continue;
            }
            let length = overlap_end - overlap_start;
            match block.compression {
                CompressionType::None => {
                    let source_offset = block
                        .compressed_offset
                        .checked_add(overlap_start - block.uncompressed_offset)
                        .ok_or_else(|| Error::invalid_data("UnityFS source offset overflowed"))?;
                    self.region.copy_range(source_offset, length, output)?;
                }
                CompressionType::Lz4 | CompressionType::Lz4Hc => {
                    let decoded = self.decoded_block(block, block_index, cache)?;
                    let relative_start = usize::try_from(overlap_start - block.uncompressed_offset)
                        .map_err(|_| Error::invalid_data("UnityFS block overlap is too large"))?;
                    let overlap_length = usize::try_from(length)
                        .map_err(|_| Error::invalid_data("UnityFS block overlap is too large"))?;
                    let relative_end = relative_start
                        .checked_add(overlap_length)
                        .ok_or_else(|| Error::invalid_data("UnityFS overlap range overflowed"))?;
                    output.write_all(decoded.get(relative_start..relative_end).ok_or_else(
                        || Error::invalid_data("UnityFS overlap exceeds decoded block"),
                    )?)?;
                }
                CompressionType::Lzma | CompressionType::Zstd | CompressionType::Oodle => {
                    let decoded = self.decoded_block(block, block_index, cache)?;
                    let relative_start = usize::try_from(overlap_start - block.uncompressed_offset)
                        .map_err(|_| Error::invalid_data("UnityFS block overlap is too large"))?;
                    let overlap_length = usize::try_from(length)
                        .map_err(|_| Error::invalid_data("UnityFS block overlap is too large"))?;
                    let relative_end = relative_start
                        .checked_add(overlap_length)
                        .ok_or_else(|| Error::invalid_data("UnityFS overlap range overflowed"))?;
                    output.write_all(decoded.get(relative_start..relative_end).ok_or_else(
                        || Error::invalid_data("UnityFS overlap exceeds decoded block"),
                    )?)?;
                }
                compression => {
                    return Err(Error::unsupported(format!(
                        "UnityFS {compression:?} data block decompression"
                    )));
                }
            }
            written += length;
            if written >= wanted {
                break;
            }
        }

        if written != wanted {
            return Err(Error::invalid_data(format!(
                "UnityFS entry {:?} mapped {written} bytes but declares {} bytes",
                entry.path, entry.size
            )));
        }
        Ok(written)
    }

    /// Decodes one compressed storage block, reusing the cache slot when the
    /// caller's walk asks for the same block again.
    fn decoded_block<'cache>(
        &self,
        block: &StorageBlock,
        block_index: usize,
        cache: &'cache mut BlockDecodeCache,
    ) -> Result<&'cache [u8]> {
        if cache.slot.as_ref().map(|(cached, _)| *cached) != Some(block_index) {
            let compressed = self.block_bytes(block, block_index)?;
            let decoded = match block.compression {
                CompressionType::Lz4 | CompressionType::Lz4Hc => decompress_lz4(
                    &compressed,
                    u64::from(block.uncompressed_size),
                    "UnityFS data block",
                )?,
                _ => decompress_unity_block(
                    block.compression,
                    &compressed,
                    u64::from(block.uncompressed_size),
                    BundleParseLimits {
                        max_lzma_memory_kib: self.max_lzma_memory_kib,
                        max_zstd_window_log: self.max_zstd_window_log,
                        ..BundleParseLimits::default()
                    },
                    "UnityFS data block",
                    self.oodle_decoder.as_deref(),
                )?,
            };
            cache.slot = Some((block_index, decoded));
        }
        Ok(&cache.slot.as_ref().expect("slot was just filled").1)
    }

    pub fn read_entry(&self, index: usize) -> Result<Vec<u8>> {
        let entry = self.entries.get(index).ok_or_else(|| {
            Error::invalid_data(format!("UnityFS entry index {index} is out of range"))
        })?;
        if entry.size > self.entry_read_limit {
            return Err(Error::invalid_data(format!(
                "UnityFS entry is {} bytes, exceeding the configured read limit {}",
                entry.size, self.entry_read_limit
            )));
        }
        let length = usize::try_from(entry.size)
            .map_err(|_| Error::invalid_data("UnityFS entry is too large for this platform"))?;
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(length).map_err(|error| {
            Error::invalid_data(format!("cannot allocate UnityFS entry: {error}"))
        })?;
        self.copy_entry(index, &mut bytes)?;
        Ok(bytes)
    }

    /// [`Self::read_entry`] reusing the caller's decoded-block cache.
    pub fn read_entry_with_cache(
        &self,
        index: usize,
        cache: &mut BlockDecodeCache,
    ) -> Result<Vec<u8>> {
        let entry = self.entries.get(index).ok_or_else(|| {
            Error::invalid_data(format!("UnityFS entry index {index} is out of range"))
        })?;
        if entry.size > self.entry_read_limit {
            return Err(Error::invalid_data(format!(
                "UnityFS entry is {} bytes, exceeding the configured read limit {}",
                entry.size, self.entry_read_limit
            )));
        }
        let length = usize::try_from(entry.size)
            .map_err(|_| Error::invalid_data("UnityFS entry is too large for this platform"))?;
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(length).map_err(|error| {
            Error::invalid_data(format!("cannot allocate UnityFS entry: {error}"))
        })?;
        self.copy_entry_with_cache(index, &mut bytes, cache)?;
        Ok(bytes)
    }
}

fn validate_unity_fs_signature(common: &BundleHeader) -> Result<bool> {
    let is_unity_fs = common.signature == "UnityFS" && matches!(common.version, 6..=8);
    let is_legacy_v6 =
        matches!(common.signature.as_str(), "UnityWeb" | "UnityRaw") && common.version == 6;
    if !is_unity_fs && !is_legacy_v6 {
        return Err(Error::unsupported(format!(
            "bundle signature {:?} format version {}; expected UnityFS v6/v7/v8 or UnityWeb/UnityRaw v6",
            common.signature, common.version
        )));
    }
    Ok(is_legacy_v6)
}

fn validate_blocks_info_sizes(
    compressed: u64,
    uncompressed: u64,
    limits: BundleParseLimits,
) -> Result<()> {
    if compressed > limits.max_blocks_info_size || uncompressed > limits.max_blocks_info_size {
        return Err(Error::invalid_data(format!(
            "UnityFS blocks info exceeds the configured {} byte limit",
            limits.max_blocks_info_size
        )));
    }
    Ok(())
}

fn layout_storage_blocks(
    blocks: &mut [StorageBlock],
    data_offset: u64,
    limits: BundleParseLimits,
) -> Result<(u64, u64)> {
    let mut compressed_offset = data_offset;
    let mut uncompressed_offset = 0_u64;
    for block in blocks {
        if u64::from(block.compressed_size) > limits.max_compressed_block_size {
            return Err(Error::invalid_data(format!(
                "UnityFS compressed block size {} exceeds configured limit {}",
                block.compressed_size, limits.max_compressed_block_size
            )));
        }
        if u64::from(block.uncompressed_size) > limits.max_uncompressed_block_size {
            return Err(Error::invalid_data(format!(
                "UnityFS uncompressed block size {} exceeds configured limit {}",
                block.uncompressed_size, limits.max_uncompressed_block_size
            )));
        }
        block.compressed_offset = compressed_offset;
        block.uncompressed_offset = uncompressed_offset;
        compressed_offset = compressed_offset
            .checked_add(u64::from(block.compressed_size))
            .ok_or_else(|| Error::invalid_data("UnityFS compressed block range overflowed"))?;
        uncompressed_offset = uncompressed_offset
            .checked_add(u64::from(block.uncompressed_size))
            .ok_or_else(|| Error::invalid_data("UnityFS uncompressed size overflowed"))?;
        if block.compression == CompressionType::None
            && block.compressed_size != block.uncompressed_size
        {
            return Err(Error::invalid_data(
                "uncompressed UnityFS block has different compressed and uncompressed sizes",
            ));
        }
    }
    Ok((compressed_offset, uncompressed_offset))
}

fn validate_bundle_entries(entries: &[BundleEntry], uncompressed_length: u64) -> Result<()> {
    for entry in entries {
        let entry_end = entry
            .offset
            .checked_add(entry.size)
            .ok_or_else(|| Error::invalid_data("UnityFS entry range overflowed"))?;
        if entry_end > uncompressed_length {
            return Err(Error::invalid_data(format!(
                "UnityFS entry {:?} ends at {entry_end}, beyond uncompressed data length {uncompressed_length}",
                entry.path
            )));
        }
    }
    Ok(())
}

/// One-slot cache of the most recently decoded storage block, shared across
/// the entry reads of a single bundle walk.
///
/// Real LZMA bundles hold every entry in one block, so without sharing, each
/// entry read decodes the whole stream again — a two-entry bundle paid the
/// full LZMA decode twice. The cache never holds more than one decoded
/// block, which the per-entry read path already materialized transiently, so
/// peak memory is unchanged; only the decode count drops. A fresh cache
/// holds nothing.
#[derive(Debug, Default)]
pub struct BlockDecodeCache {
    slot: Option<(usize, Vec<u8>)>,
}

impl BlockDecodeCache {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

fn uses_legacy_flag_vocabulary(common: &BundleHeader) -> bool {
    if common.unity_revision.is_stripped() {
        return false;
    }
    let revision = &common.unity_revision;
    revision.components().0 < 2020
        || revision.is_in_range((2020, 0, 0), (2020, 3, 34))
        || revision.is_in_range((2021, 0, 0), (2021, 3, 2))
        || revision.is_in_range((2022, 0, 0), (2022, 1, 1))
}

/// Reports whether the block data is preceded by 16-byte alignment padding.
///
/// Only meaningful under the newer flag vocabulary; where 0x200 marks `UnityCN`
/// encryption it says nothing about padding.
fn block_info_needs_padding(common: &BundleHeader, flags: ArchiveFlags) -> bool {
    !uses_legacy_flag_vocabulary(common) && flags.block_info_needs_padding()
}

fn has_unity_cn_flag(common: &BundleHeader, flags: ArchiveFlags) -> bool {
    if common.unity_revision.is_stripped() {
        return false;
    }
    // The managed reader spells these as IsInRange(major, upper), which is
    // `major >= lower && version < upper`: the upper bound is exclusive. Using
    // an inclusive bound here misread the three boundary releases themselves,
    // where 0x200 is an ordinary BlockInfoNeedPaddingAtStart rather than the
    // UnityCN V1 marker, so a perfectly good bundle could be probed as
    // encrypted. is_in_range already implements the half-open comparison.
    let uses_v1_flag = uses_legacy_flag_vocabulary(common);
    let mask = if uses_v1_flag {
        UNITY_CN_V1_FLAG
    } else {
        UNITY_CN_V2_V3_FLAGS
    };
    flags.0 & mask != 0
}

fn align_blocks_info<R: Read + Seek>(
    reader: &mut EndianReader<R>,
    bundle_start: u64,
    common: &BundleHeader,
    flags: ArchiveFlags,
    bundle_end: u64,
) -> Result<()> {
    if common.version >= 7 {
        return align_relative(reader, bundle_start, 16, bundle_end);
    }
    if common.unity_revision.components() < (2019, 4, 0)
        || flags.0 == BLOCKS_AND_DIRECTORY_INFO_COMBINED
    {
        return Ok(());
    }

    let position = reader.position()?;
    let padding = relative_padding(position, bundle_start, 16)?;
    if padding == 0 {
        return Ok(());
    }
    let target = position
        .checked_add(padding)
        .ok_or_else(|| Error::invalid_data("UnityFS optional alignment overflowed"))?;
    if target > bundle_end {
        return Err(Error::invalid_data(
            "UnityFS optional header alignment exceeds bundle size",
        ));
    }
    let bytes = reader.read_bytes(usize::try_from(padding).expect("padding is less than 16"))?;
    if bytes.iter().any(|byte| *byte != 0) {
        reader.set_position(position)?;
    }
    Ok(())
}

fn align_relative<R: Read + Seek>(
    reader: &mut EndianReader<R>,
    origin: u64,
    alignment: u64,
    limit: u64,
) -> Result<()> {
    let position = reader.position()?;
    let padding = relative_padding(position, origin, alignment)?;
    let target = position
        .checked_add(padding)
        .ok_or_else(|| Error::invalid_data("UnityFS aligned position overflowed"))?;
    if target > limit {
        return Err(Error::invalid_data(
            "UnityFS alignment exceeds the bundle boundary",
        ));
    }
    reader.set_position(target)
}

fn relative_padding(position: u64, origin: u64, alignment: u64) -> Result<u64> {
    let relative = position
        .checked_sub(origin)
        .ok_or_else(|| Error::invalid_data("reader position precedes UnityFS origin"))?;
    let remainder = relative % alignment;
    Ok(if remainder == 0 {
        0
    } else {
        alignment - remainder
    })
}

fn decompress_blocks_info(
    compression: CompressionType,
    bytes: Vec<u8>,
    expected_size: u64,
    limits: BundleParseLimits,
    oodle_decoder: Option<&dyn OodleDecoder>,
) -> Result<Vec<u8>> {
    match compression {
        CompressionType::None => {
            if u64::try_from(bytes.len()).ok() != Some(expected_size) {
                return Err(Error::invalid_data(format!(
                    "uncompressed UnityFS blocks info has {} bytes; expected {expected_size}",
                    bytes.len()
                )));
            }
            Ok(bytes)
        }
        CompressionType::Lz4 | CompressionType::Lz4Hc => {
            decompress_lz4(&bytes, expected_size, "UnityFS blocks info")
        }
        CompressionType::Lzma | CompressionType::Zstd | CompressionType::Oodle => {
            decompress_unity_block(
                compression,
                &bytes,
                expected_size,
                limits,
                "UnityFS blocks info",
                oodle_decoder,
            )
        }
        compression => Err(Error::unsupported(format!(
            "UnityFS {compression:?} blocks-info decompression"
        ))),
    }
}

fn decompress_unity_block(
    compression: CompressionType,
    input: &[u8],
    expected_size: u64,
    limits: BundleParseLimits,
    context: &str,
    oodle_decoder: Option<&dyn OodleDecoder>,
) -> Result<Vec<u8>> {
    match compression {
        CompressionType::Lzma => {
            decompress_lzma(input, expected_size, limits.max_lzma_memory_kib, context)
        }
        CompressionType::Zstd => {
            decompress_zstd(input, expected_size, limits.max_zstd_window_log, context)
        }
        CompressionType::Oodle => decompress_oodle(input, expected_size, oodle_decoder, context),
        _ => Err(Error::invalid_data(format!(
            "{context} received non-stream compression {compression:?}"
        ))),
    }
}

fn decompress_oodle(
    input: &[u8],
    expected_size: u64,
    decoder: Option<&dyn OodleDecoder>,
    context: &str,
) -> Result<Vec<u8>> {
    let decoder = decoder.ok_or_else(|| {
        Error::unsupported(format!(
            "{context} Oodle decompression requires a configured decoder"
        ))
    })?;
    let output_length = usize::try_from(expected_size)
        .map_err(|_| Error::invalid_data(format!("{context} is too large for this platform")))?;
    let mut output = allocate_zeroed(output_length, context)?;
    let written = decoder.decompress(input, &mut output)?;
    if written != output_length {
        return Err(Error::invalid_data(format!(
            "{context} Oodle decoder wrote {written} bytes; expected {output_length}"
        )));
    }
    Ok(output)
}

fn decompress_lzma(
    input: &[u8],
    expected_size: u64,
    maximum_memory_kib: u32,
    context: &str,
) -> Result<Vec<u8>> {
    let header = input.get(..5).ok_or_else(|| {
        Error::invalid_data(format!(
            "{context} LZMA data is shorter than its 5-byte header"
        ))
    })?;
    let properties = header[0];
    let dictionary_size = u32::from_le_bytes(
        header[1..5]
            .try_into()
            .expect("the LZMA header slice is exactly four bytes"),
    );
    let required_memory = lzma_rust2::lzma_get_memory_usage_by_props(dictionary_size, properties)
        .map_err(|error| {
        Error::invalid_data(format!("invalid {context} LZMA properties: {error}"))
    })?;
    if required_memory > maximum_memory_kib {
        return Err(Error::invalid_data(format!(
            "{context} LZMA decoder needs {required_memory} KiB, exceeding limit {maximum_memory_kib} KiB"
        )));
    }

    let output_length = usize::try_from(expected_size)
        .map_err(|_| Error::invalid_data(format!("{context} is too large for this platform")))?;
    let mut output = allocate_zeroed(output_length, context)?;
    let mut decoder = lzma_rust2::LzmaReader::new_with_props(
        std::io::Cursor::new(&input[5..]),
        expected_size,
        properties,
        dictionary_size,
        None,
    )
    .map_err(|error| Error::invalid_data(format!("invalid {context} LZMA stream: {error}")))?;
    decoder.read_exact(&mut output).map_err(|error| {
        Error::invalid_data(format!("truncated {context} LZMA stream: {error}"))
    })?;
    let mut trailing_output = [0_u8; 1];
    if decoder
        .read(&mut trailing_output)
        .map_err(|error| Error::invalid_data(format!("invalid {context} LZMA ending: {error}")))?
        != 0
    {
        return Err(Error::invalid_data(format!(
            "{context} LZMA decoder produced more than {expected_size} bytes"
        )));
    }
    Ok(output)
}

fn decompress_zstd(
    input: &[u8],
    expected_size: u64,
    maximum_window_log: u32,
    context: &str,
) -> Result<Vec<u8>> {
    let output_length = usize::try_from(expected_size)
        .map_err(|_| Error::invalid_data(format!("{context} is too large for this platform")))?;
    let mut output = allocate_zeroed(output_length, context)?;
    let mut decoder = zstd::bulk::Decompressor::new().map_err(|error| {
        Error::invalid_data(format!("cannot initialize {context} Zstd decoder: {error}"))
    })?;
    decoder
        .window_log_max(maximum_window_log)
        .map_err(|error| {
            Error::invalid_data(format!("cannot limit {context} Zstd window: {error}"))
        })?;
    let written = decoder
        .decompress_to_buffer(input, &mut output[..])
        .map_err(|error| Error::invalid_data(format!("invalid {context} Zstd data: {error}")))?;
    if written != output_length {
        return Err(Error::invalid_data(format!(
            "{context} Zstd decoder wrote {written} bytes; expected {output_length}"
        )));
    }
    Ok(output)
}

fn allocate_zeroed(length: usize, context: &str) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    output.try_reserve_exact(length).map_err(|error| {
        Error::invalid_data(format!("cannot allocate {context} output: {error}"))
    })?;
    output.resize(length, 0);
    Ok(output)
}

fn decompress_lz4(input: &[u8], expected_size: u64, context: &str) -> Result<Vec<u8>> {
    let output_length = usize::try_from(expected_size)
        .map_err(|_| Error::invalid_data(format!("{context} is too large for this platform")))?;
    let mut output = Vec::new();
    output.try_reserve_exact(output_length).map_err(|error| {
        Error::invalid_data(format!("cannot allocate {context} output: {error}"))
    })?;
    // Keep the destination initialized and zeroed even though the selected
    // lz4_flex release contains the offset-validation fix from RUSTSEC-2026-0041.
    output.resize(output_length, 0);
    let written = lz4_flex::block::decompress_into(input, &mut output)
        .map_err(|error| Error::invalid_data(format!("invalid {context} LZ4 data: {error}")))?;
    if written != output_length {
        return Err(Error::invalid_data(format!(
            "{context} LZ4 decoder wrote {written} bytes; expected {output_length}"
        )));
    }
    Ok(output)
}

fn parse_blocks_info(
    bytes: &[u8],
    limits: BundleParseLimits,
) -> Result<([u8; 16], Vec<StorageBlock>, Vec<BundleEntry>)> {
    let mut reader = EndianReader::new(Cursor::new(bytes), Endian::Big);
    let hash_bytes = reader.read_bytes(16)?;
    let blocks_info_hash = hash_bytes
        .as_slice()
        .try_into()
        .map_err(|_| Error::invalid_data("failed to read UnityFS blocks-info hash"))?;

    let blocks_count = checked_count(reader.read_i32()?, "UnityFS block", limits.max_blocks)?;
    let maximum_blocks_in_input = usize::try_from(reader.remaining()? / 10).unwrap_or(usize::MAX);
    if blocks_count > maximum_blocks_in_input {
        return Err(Error::invalid_data(format!(
            "UnityFS block count {blocks_count} exceeds metadata capacity {maximum_blocks_in_input}"
        )));
    }
    let mut blocks = Vec::new();
    blocks
        .try_reserve_exact(blocks_count)
        .map_err(|error| Error::invalid_data(format!("cannot allocate block table: {error}")))?;
    for _ in 0..blocks_count {
        let uncompressed_size = reader.read_u32()?;
        let compressed_size = reader.read_u32()?;
        let flags = reader.read_u16()?;
        let code = u8::try_from(flags & STORAGE_BLOCK_COMPRESSION_MASK)
            .expect("storage compression mask fits in u8");
        blocks.push(StorageBlock {
            uncompressed_size,
            compressed_size,
            flags,
            compression: CompressionType::from_code(code),
            compressed_offset: 0,
            uncompressed_offset: 0,
        });
    }

    let entries_count = checked_count(reader.read_i32()?, "UnityFS entry", limits.max_entries)?;
    let maximum_entries_in_input = usize::try_from(reader.remaining()? / 21).unwrap_or(usize::MAX);
    if entries_count > maximum_entries_in_input {
        return Err(Error::invalid_data(format!(
            "UnityFS entry count {entries_count} exceeds metadata capacity {maximum_entries_in_input}"
        )));
    }
    let mut entries = Vec::new();
    entries
        .try_reserve_exact(entries_count)
        .map_err(|error| Error::invalid_data(format!("cannot allocate entry table: {error}")))?;
    for _ in 0..entries_count {
        let offset = non_negative_i64(reader.read_i64()?, "UnityFS entry offset")?;
        let size = non_negative_i64(reader.read_i64()?, "UnityFS entry size")?;
        let flags = reader.read_u32()?;
        let path = reader.read_c_string_required(limits.max_path_length, "UnityFS path")?;
        let file_name = copy_portable_file_name(&path, "UnityFS file name")?;
        entries.push(BundleEntry {
            offset,
            size,
            flags,
            path,
            file_name,
        });
    }

    Ok((blocks_info_hash, blocks, entries))
}

fn checked_count(value: i32, field: &str, maximum: usize) -> Result<usize> {
    let count = checked_length(value, field)?;
    if count > maximum {
        return Err(Error::invalid_data(format!(
            "{field} count {count} exceeds configured limit {maximum}"
        )));
    }
    Ok(count)
}

fn non_negative_i64(value: i64, field: &str) -> Result<u64> {
    u64::try_from(value).map_err(|_| Error::invalid_data(format!("{field} cannot be negative")))
}

fn copy_portable_file_name(path: &str, field: &str) -> Result<String> {
    let file_name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let mut copied = String::new();
    copied
        .try_reserve_exact(file_name.len())
        .map_err(|error| Error::invalid_data(format!("cannot allocate {field}: {error}")))?;
    copied.push_str(file_name);
    Ok(copied)
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::io::Cursor;
    use std::sync::{Arc, Mutex};

    use lz4_flex::block::compress;

    use crate::Error;
    use crate::endian::{Endian, EndianReader};
    use crate::source::Region;

    use super::{
        ArchiveFlags, BLOCK_INFO_NEEDS_PADDING_AT_START, BLOCKS_AND_DIRECTORY_INFO_COMBINED,
        BLOCKS_INFO_AT_END, BundleHeader, BundleOpenOptions, BundleParseLimits, CompressionType,
        OodleDecoder, UNITY_CN_HEADER_BYTES, UNITY_CN_V1_FLAG, UNITY_CN_V2_V3_FLAGS, UnityFsBundle,
        has_unity_cn_flag, parse_blocks_info,
    };

    const PAYLOAD: &[u8] = b"native-rust-payload";
    const OODLE_INFO_INPUT: &[u8] = b"fake-oodle-blocks-info";
    const OODLE_DATA_INPUT: &[u8] = b"fake-oodle-data";

    struct FakeOodleResponse {
        output: Vec<u8>,
        reported_size: usize,
    }

    #[derive(Default)]
    struct FakeOodleState {
        responses: VecDeque<FakeOodleResponse>,
        calls: Vec<(Vec<u8>, usize)>,
    }

    #[derive(Default)]
    struct FakeOodleDecoder {
        state: Mutex<FakeOodleState>,
    }

    impl FakeOodleDecoder {
        fn new(responses: Vec<FakeOodleResponse>) -> Self {
            Self {
                state: Mutex::new(FakeOodleState {
                    responses: responses.into(),
                    calls: Vec::new(),
                }),
            }
        }

        fn calls(&self) -> Vec<(Vec<u8>, usize)> {
            self.state.lock().unwrap().calls.clone()
        }
    }

    impl OodleDecoder for FakeOodleDecoder {
        fn decompress(&self, input: &[u8], output: &mut [u8]) -> crate::Result<usize> {
            let mut state = self.state.lock().unwrap();
            state.calls.push((input.to_vec(), output.len()));
            let response = state
                .responses
                .pop_front()
                .ok_or_else(|| Error::invalid_data("fake Oodle decoder has no queued response"))?;
            if response.output.len() != output.len() {
                return Err(Error::invalid_data(format!(
                    "fake Oodle response has {} bytes for a {} byte output",
                    response.output.len(),
                    output.len()
                )));
            }
            output.copy_from_slice(&response.output);
            Ok(response.reported_size)
        }
    }

    #[test]
    fn reads_common_unity_fs_header() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"UnityFS\0");
        bytes.extend_from_slice(&7_u32.to_be_bytes());
        bytes.extend_from_slice(b"5.x.x\0");
        bytes.extend_from_slice(b"2022.3.62f1\0");

        let mut reader = EndianReader::new(Cursor::new(bytes), Endian::Little);
        let header = BundleHeader::read(&mut reader).unwrap();

        assert_eq!(header.signature, "UnityFS");
        assert_eq!(header.version, 7);
        assert_eq!(header.unity_version, "5.x.x");
        assert_eq!(header.unity_revision.components(), (2022, 3, 62));
    }

    #[test]
    fn preserves_stripped_revision_as_default() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"UnityFS\0");
        bytes.extend_from_slice(&6_u32.to_be_bytes());
        bytes.extend_from_slice(b"5.x.x\0\0");

        let mut reader = EndianReader::new(Cursor::new(bytes), Endian::Big);
        let header = BundleHeader::read(&mut reader).unwrap();
        assert!(header.unity_revision.is_stripped());
    }

    #[test]
    fn rejects_unterminated_or_invalid_common_header() {
        let mut unterminated = Vec::new();
        unterminated.extend_from_slice(b"UnityFS\0");
        unterminated.extend_from_slice(&6_u32.to_be_bytes());
        unterminated.extend_from_slice(b"5.x.x\0revision-without-nul");
        let mut reader = EndianReader::new(Cursor::new(unterminated), Endian::Big);
        assert!(BundleHeader::read(&mut reader).is_err());

        let mut invalid = Vec::new();
        invalid.extend_from_slice(b"NotUnity\0");
        invalid.extend_from_slice(&6_u32.to_be_bytes());
        invalid.extend_from_slice(b"5.x.x\0\0");
        let mut reader = EndianReader::new(Cursor::new(invalid), Endian::Big);
        assert!(BundleHeader::read(&mut reader).is_err());
    }

    #[test]
    fn reads_inline_uncompressed_bundle_and_entry() {
        let bytes = make_bundle(
            6,
            BLOCKS_AND_DIRECTORY_INFO_COMBINED,
            u64::try_from(PAYLOAD.len()).unwrap(),
        );
        let bundle = UnityFsBundle::open(&Region::from_bytes(bytes)).unwrap();

        assert_eq!(bundle.header.common.version, 6);
        assert_eq!(bundle.blocks.len(), 1);
        assert_eq!(bundle.blocks[0].compression, CompressionType::None);
        assert_eq!(bundle.entries.len(), 1);
        assert_eq!(bundle.entries[0].path, "folder/data.bin");
        assert_eq!(bundle.entries[0].file_name, "data.bin");
        assert_eq!(bundle.read_entry(0).unwrap(), PAYLOAD);
    }

    #[test]
    fn copies_a_unicode_file_name_after_a_windows_separator() {
        let path = "folder\\目录\\表.bin";
        let blocks_info = make_blocks_info_record_with_path(
            u64::try_from(PAYLOAD.len()).unwrap(),
            u32::try_from(PAYLOAD.len()).unwrap(),
            u32::try_from(PAYLOAD.len()).unwrap(),
            CompressionType::None,
            path.as_bytes(),
        );
        let (_, _, entries) =
            parse_blocks_info(&blocks_info, BundleParseLimits::default()).unwrap();

        assert_eq!(entries[0].path, path);
        assert_eq!(entries[0].file_name, "表.bin");
        assert!(
            parse_blocks_info(
                &blocks_info,
                BundleParseLimits {
                    max_path_length: 4,
                    ..BundleParseLimits::default()
                }
            )
            .is_err()
        );
    }

    #[test]
    fn reads_unity_web_version_six_fs_layout() {
        let bytes = make_bundle_with_signature(
            b"UnityWeb\0",
            6,
            BLOCKS_AND_DIRECTORY_INFO_COMBINED,
            u64::try_from(PAYLOAD.len()).unwrap(),
        );
        let bundle = UnityFsBundle::open(&Region::from_bytes(bytes)).unwrap();

        assert_eq!(bundle.header.common.signature, "UnityWeb");
        assert_eq!(bundle.header.common.version, 6);
        assert_eq!(bundle.read_entry(0).unwrap(), PAYLOAD);
    }

    #[test]
    fn reads_blocks_info_at_end() {
        let bytes = make_bundle(7, BLOCKS_INFO_AT_END, u64::try_from(PAYLOAD.len()).unwrap());
        let bundle = UnityFsBundle::open(&Region::from_bytes(bytes)).unwrap();

        assert!(bundle.header.flags.blocks_info_at_end());
        assert_eq!(bundle.data_offset % 16, 0);
        assert_eq!(bundle.read_entry(0).unwrap(), PAYLOAD);
    }

    #[test]
    fn honors_block_data_padding_flag() {
        // 0x200 only means padding under the newer flag vocabulary; on a
        // pre-2020 revision it is the UnityCN marker instead.
        let flags = BLOCKS_AND_DIRECTORY_INFO_COMBINED | BLOCK_INFO_NEEDS_PADDING_AT_START;
        let bytes = make_bundle_with_revision(
            b"UnityFS\0",
            7,
            flags,
            u64::try_from(PAYLOAD.len()).unwrap(),
            "2022.3.62f1",
        );
        let bundle = UnityFsBundle::open(&Region::from_bytes(bytes)).unwrap();

        assert_eq!(bundle.data_offset % 16, 0);
        assert_eq!(bundle.read_entry(0).unwrap(), PAYLOAD);
    }

    #[test]
    fn unity_cn_flag_ranges_use_the_managed_half_open_bounds() {
        // AssetStudio spells these as IsInRange(major, upper), which is
        // `major >= lower && version < upper`. At exactly the upper bounds the
        // newer flag vocabulary already applies, so 0x200 there is an ordinary
        // BlockInfoNeedPaddingAtStart and must not be read as the UnityCN V1
        // marker -- an inclusive bound made those three releases probe a good
        // bundle as encrypted.
        let header = |revision: &str| BundleHeader {
            signature: "UnityFS".to_owned(),
            version: 7,
            unity_version: "5.x.x".to_owned(),
            unity_revision: revision.parse().unwrap(),
        };
        for (revision, expects_v1) in [
            ("2019.4.40f1", true),
            ("2020.3.33f1", true),
            ("2020.3.34f1", false),
            ("2021.3.1f1", true),
            ("2021.3.2f1", false),
            ("2022.1.0f1", true),
            ("2022.1.1f1", false),
            ("2023.1.0f1", false),
        ] {
            assert_eq!(
                has_unity_cn_flag(&header(revision), ArchiveFlags(UNITY_CN_V1_FLAG)),
                expects_v1,
                "{revision} against the V1 flag"
            );
            assert_eq!(
                has_unity_cn_flag(&header(revision), ArchiveFlags(UNITY_CN_V2_V3_FLAGS)),
                !expects_v1,
                "{revision} against the V2/V3 flags"
            );
        }
    }

    #[test]
    fn unity_cn_bundles_report_the_missing_key_rather_than_a_parse_error() {
        // The flag decides this now, not a speculative parse at header+70. That
        // matters because the common case has an encrypted blocks-info table,
        // which the old probe could not read, so it fell through and reported a
        // misleading "invalid LZ4 data" instead of naming UnityCN.
        let bytes = make_unity_cn_bundle();
        let error = UnityFsBundle::open(&Region::from_bytes(bytes)).unwrap_err();
        assert!(
            matches!(error, Error::Unsupported(message) if message.contains("UnityCN") && message.contains("caller-supplied key"))
        );
    }

    #[test]
    fn alignment_is_relative_to_embedded_bundle_start() {
        let prefix = b"thirteen-byte";
        let bundle_bytes =
            make_bundle(7, BLOCKS_INFO_AT_END, u64::try_from(PAYLOAD.len()).unwrap());
        let mut bytes = prefix.to_vec();
        bytes.extend_from_slice(&bundle_bytes);
        let root = Region::from_bytes(bytes);
        let region = root
            .subregion(
                u64::try_from(prefix.len()).unwrap(),
                u64::try_from(bundle_bytes.len()).unwrap(),
            )
            .unwrap();
        let bundle = UnityFsBundle::open(&region).unwrap();

        assert_eq!(bundle.data_offset % 16, 0);
        assert_eq!(bundle.read_entry(0).unwrap(), PAYLOAD);
    }

    #[test]
    fn rejects_entry_outside_uncompressed_blocks() {
        let bytes = make_bundle(6, BLOCKS_AND_DIRECTORY_INFO_COMBINED, 10_000);
        assert!(UnityFsBundle::open(&Region::from_bytes(bytes)).is_err());
    }

    #[test]
    fn rejects_invalid_lzma_blocks_info() {
        let flags = BLOCKS_AND_DIRECTORY_INFO_COMBINED | u32::from(CompressionType::Lzma.code());
        let bytes = make_bundle(6, flags, u64::try_from(PAYLOAD.len()).unwrap());
        let error = UnityFsBundle::open(&Region::from_bytes(bytes)).unwrap_err();
        assert!(matches!(error, Error::InvalidData(_)));
    }

    #[test]
    fn decodes_lz4_blocks_info() {
        let bytes = make_lz4_bundle(true, false);
        let bundle = UnityFsBundle::open(&Region::from_bytes(bytes)).unwrap();

        assert_eq!(bundle.header.flags.compression(), CompressionType::Lz4);
        assert_eq!(bundle.read_entry(0).unwrap(), PAYLOAD);
    }

    #[test]
    fn decodes_lz4_data_block() {
        let bytes = make_lz4_bundle(false, true);
        let bundle = UnityFsBundle::open(&Region::from_bytes(bytes)).unwrap();

        assert_eq!(bundle.blocks[0].compression, CompressionType::Lz4);
        assert_eq!(bundle.read_entry(0).unwrap(), PAYLOAD);
    }

    #[test]
    fn decodes_zstd_data_block() {
        let data = zstd::bulk::compress(PAYLOAD, 1).unwrap();
        let bytes = make_encoded_data_bundle(&data, CompressionType::Zstd);
        let bundle = UnityFsBundle::open(&Region::from_bytes(bytes)).unwrap();

        assert_eq!(bundle.blocks[0].compression, CompressionType::Zstd);
        assert_eq!(bundle.read_entry(0).unwrap(), PAYLOAD);
    }

    #[test]
    fn decodes_oodle_blocks_info_and_data_through_injected_decoder() {
        let (bytes, blocks_info) = make_oodle_bundle();
        let decoder = Arc::new(FakeOodleDecoder::new(vec![
            FakeOodleResponse {
                reported_size: blocks_info.len(),
                output: blocks_info.clone(),
            },
            FakeOodleResponse {
                reported_size: PAYLOAD.len(),
                output: PAYLOAD.to_vec(),
            },
        ]));
        let options = BundleOpenOptions {
            oodle_decoder: Some(decoder.clone()),
            ..BundleOpenOptions::default()
        };
        assert!(format!("{options:?}").contains("<configured>"));

        let bundle =
            UnityFsBundle::open_with_options(&Region::from_bytes(bytes), options.clone()).unwrap();
        assert_eq!(bundle.read_entry(0).unwrap(), PAYLOAD);
        assert_eq!(
            decoder.calls(),
            vec![
                (OODLE_INFO_INPUT.to_vec(), blocks_info.len()),
                (OODLE_DATA_INPUT.to_vec(), PAYLOAD.len()),
            ]
        );
    }

    #[test]
    fn rejects_oodle_without_a_configured_decoder() {
        let (bytes, _) = make_oodle_bundle();
        let error = UnityFsBundle::open(&Region::from_bytes(bytes)).unwrap_err();
        assert!(
            matches!(error, Error::Unsupported(message) if message.contains("UnityFS blocks info Oodle"))
        );

        let bytes = make_encoded_data_bundle(OODLE_DATA_INPUT, CompressionType::Oodle);
        let bundle = UnityFsBundle::open(&Region::from_bytes(bytes)).unwrap();
        let error = bundle.read_entry(0).unwrap_err();
        assert!(
            matches!(error, Error::Unsupported(message) if message.contains("UnityFS data block Oodle"))
        );
    }

    #[test]
    fn rejects_inexact_oodle_output_sizes_for_info_and_data() {
        let (bytes, blocks_info) = make_oodle_bundle();
        let decoder = Arc::new(FakeOodleDecoder::new(vec![FakeOodleResponse {
            reported_size: blocks_info.len() - 1,
            output: blocks_info,
        }]));
        let error = UnityFsBundle::open_with_options(
            &Region::from_bytes(bytes),
            BundleOpenOptions {
                oodle_decoder: Some(decoder),
                ..BundleOpenOptions::default()
            },
        )
        .unwrap_err();
        assert!(
            matches!(error, Error::InvalidData(message) if message.contains("UnityFS blocks info Oodle decoder wrote"))
        );

        let bytes = make_encoded_data_bundle(OODLE_DATA_INPUT, CompressionType::Oodle);
        let decoder = Arc::new(FakeOodleDecoder::new(vec![FakeOodleResponse {
            reported_size: PAYLOAD.len() - 1,
            output: PAYLOAD.to_vec(),
        }]));
        let bundle = UnityFsBundle::open_with_options(
            &Region::from_bytes(bytes),
            BundleOpenOptions {
                oodle_decoder: Some(decoder),
                ..BundleOpenOptions::default()
            },
        )
        .unwrap();
        let error = bundle.read_entry(0).unwrap_err();
        assert!(
            matches!(error, Error::InvalidData(message) if message.contains("UnityFS data block Oodle decoder wrote"))
        );
    }

    #[test]
    fn enforces_oodle_input_and_output_limits_before_calling_decoder() {
        let (bytes, _) = make_oodle_bundle();
        let decoder = Arc::new(FakeOodleDecoder::default());
        let error = UnityFsBundle::open_with_options(
            &Region::from_bytes(bytes),
            BundleOpenOptions {
                limits: BundleParseLimits {
                    max_blocks_info_size: 1,
                    ..BundleParseLimits::default()
                },
                oodle_decoder: Some(decoder.clone()),
                unity_cn_key: None,
            },
        )
        .unwrap_err();
        assert!(
            matches!(error, Error::InvalidData(message) if message.contains("blocks info exceeds"))
        );
        assert!(decoder.calls().is_empty());

        let bytes = make_encoded_data_bundle(OODLE_DATA_INPUT, CompressionType::Oodle);
        let error = UnityFsBundle::open_with_options(
            &Region::from_bytes(bytes.clone()),
            BundleOpenOptions {
                limits: BundleParseLimits {
                    max_compressed_block_size: 1,
                    ..BundleParseLimits::default()
                },
                oodle_decoder: Some(decoder.clone()),
                unity_cn_key: None,
            },
        )
        .unwrap_err();
        assert!(
            matches!(error, Error::InvalidData(message) if message.contains("compressed block size"))
        );
        assert!(decoder.calls().is_empty());

        let error = UnityFsBundle::open_with_options(
            &Region::from_bytes(bytes),
            BundleOpenOptions {
                limits: BundleParseLimits {
                    max_uncompressed_block_size: 1,
                    ..BundleParseLimits::default()
                },
                oodle_decoder: Some(decoder.clone()),
                unity_cn_key: None,
            },
        )
        .unwrap_err();
        assert!(
            matches!(error, Error::InvalidData(message) if message.contains("uncompressed block size"))
        );
        assert!(decoder.calls().is_empty());
    }

    #[test]
    fn decodes_unity_raw_lzma_block_and_enforces_memory_limit() {
        let mut compressed = vec![0x5d, 0x00, 0x00, 0x10, 0x00];
        compressed.extend_from_slice(&[
            0x00, 0x37, 0x18, 0x4a, 0xef, 0x3f, 0x62, 0x69, 0xc4, 0xb6, 0xbd, 0x10, 0x76, 0x83,
            0xb7, 0x38, 0xcc, 0xc7, 0xfb, 0xdb, 0x81, 0xd2, 0x24, 0x01, 0xff, 0xff, 0xb1, 0xc8,
            0x00, 0x00,
        ]);
        let decoded = super::decompress_lzma(
            &compressed,
            u64::try_from(PAYLOAD.len()).unwrap(),
            64 * 1024,
            "test payload",
        )
        .unwrap();
        assert_eq!(decoded, PAYLOAD);
        assert!(
            super::decompress_lzma(
                &compressed,
                u64::try_from(PAYLOAD.len()).unwrap(),
                1,
                "test payload",
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_truncated_bundle() {
        let mut bytes = make_bundle(
            6,
            BLOCKS_AND_DIRECTORY_INFO_COMBINED,
            u64::try_from(PAYLOAD.len()).unwrap(),
        );
        bytes.pop();
        assert!(UnityFsBundle::open(&Region::from_bytes(bytes)).is_err());
    }

    #[test]
    fn rejects_record_count_larger_than_metadata_can_hold() {
        let mut blocks_info = vec![0_u8; 16];
        blocks_info.extend_from_slice(&1_000_000_i32.to_be_bytes());
        assert!(parse_blocks_info(&blocks_info, BundleParseLimits::default()).is_err());
    }

    fn make_bundle(version: u32, flags: u32, node_size: u64) -> Vec<u8> {
        make_bundle_with_signature(b"UnityFS\0", version, flags, node_size)
    }

    fn make_bundle_with_signature(
        signature: &[u8],
        version: u32,
        flags: u32,
        node_size: u64,
    ) -> Vec<u8> {
        make_bundle_with_revision(signature, version, flags, node_size, "2018.4.0f1")
    }

    fn make_bundle_with_revision(
        signature: &[u8],
        version: u32,
        flags: u32,
        node_size: u64,
        revision: &str,
    ) -> Vec<u8> {
        let blocks_info = make_blocks_info(node_size);
        let blocks_info_size = u32::try_from(blocks_info.len()).unwrap();

        let mut bytes = Vec::new();
        bytes.extend_from_slice(signature);
        bytes.extend_from_slice(&version.to_be_bytes());
        bytes.extend_from_slice(b"5.x.x\0");
        bytes.extend_from_slice(revision.as_bytes());
        bytes.push(0);
        let size_position = bytes.len();
        bytes.extend_from_slice(&0_i64.to_be_bytes());
        bytes.extend_from_slice(&blocks_info_size.to_be_bytes());
        bytes.extend_from_slice(&blocks_info_size.to_be_bytes());
        bytes.extend_from_slice(&flags.to_be_bytes());
        if signature != b"UnityFS\0" {
            bytes.push(0);
        }

        if version >= 7 {
            pad_to(&mut bytes, 16);
        }
        if flags & BLOCKS_INFO_AT_END != 0 {
            if flags & BLOCK_INFO_NEEDS_PADDING_AT_START != 0 {
                pad_to(&mut bytes, 16);
            }
            bytes.extend_from_slice(PAYLOAD);
            bytes.extend_from_slice(&blocks_info);
        } else {
            bytes.extend_from_slice(&blocks_info);
            if flags & BLOCK_INFO_NEEDS_PADDING_AT_START != 0 {
                pad_to(&mut bytes, 16);
            }
            bytes.extend_from_slice(PAYLOAD);
        }

        let bundle_size = i64::try_from(bytes.len()).unwrap();
        bytes[size_position..size_position + 8].copy_from_slice(&bundle_size.to_be_bytes());
        bytes
    }

    fn make_blocks_info(node_size: u64) -> Vec<u8> {
        let payload_size = u32::try_from(PAYLOAD.len()).unwrap();
        make_blocks_info_record(node_size, payload_size, payload_size, CompressionType::None)
    }

    fn make_unity_cn_bundle() -> Vec<u8> {
        let blocks_info = make_blocks_info(u64::try_from(PAYLOAD.len()).unwrap());
        let blocks_info_size = u32::try_from(blocks_info.len()).unwrap();
        let flags = BLOCKS_AND_DIRECTORY_INFO_COMBINED | BLOCK_INFO_NEEDS_PADDING_AT_START;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"UnityFS\0");
        bytes.extend_from_slice(&6_u32.to_be_bytes());
        bytes.extend_from_slice(b"5.x.x\0");
        bytes.extend_from_slice(b"2018.4.0f1\0");
        let size_position = bytes.len();
        bytes.extend_from_slice(&0_i64.to_be_bytes());
        bytes.extend_from_slice(&blocks_info_size.to_be_bytes());
        bytes.extend_from_slice(&blocks_info_size.to_be_bytes());
        bytes.extend_from_slice(&flags.to_be_bytes());
        bytes.resize(
            bytes.len() + usize::try_from(UNITY_CN_HEADER_BYTES).unwrap(),
            0xa5,
        );
        bytes.extend_from_slice(&blocks_info);
        bytes.extend_from_slice(PAYLOAD);
        let bundle_size = i64::try_from(bytes.len()).unwrap();
        bytes[size_position..size_position + 8].copy_from_slice(&bundle_size.to_be_bytes());
        bytes
    }

    fn make_blocks_info_record(
        node_size: u64,
        uncompressed_size: u32,
        compressed_size: u32,
        compression: CompressionType,
    ) -> Vec<u8> {
        make_blocks_info_record_with_path(
            node_size,
            uncompressed_size,
            compressed_size,
            compression,
            b"folder/data.bin",
        )
    }

    fn make_blocks_info_record_with_path(
        node_size: u64,
        uncompressed_size: u32,
        compressed_size: u32,
        compression: CompressionType,
        path: &[u8],
    ) -> Vec<u8> {
        let mut bytes = vec![0_u8; 16];
        bytes.extend_from_slice(&1_i32.to_be_bytes());
        bytes.extend_from_slice(&uncompressed_size.to_be_bytes());
        bytes.extend_from_slice(&compressed_size.to_be_bytes());
        bytes.extend_from_slice(&u16::from(compression.code()).to_be_bytes());
        bytes.extend_from_slice(&1_i32.to_be_bytes());
        bytes.extend_from_slice(&0_i64.to_be_bytes());
        bytes.extend_from_slice(&i64::try_from(node_size).unwrap().to_be_bytes());
        bytes.extend_from_slice(&0_u32.to_be_bytes());
        bytes.extend_from_slice(path);
        bytes.push(0);
        bytes
    }

    fn make_lz4_bundle(compress_info: bool, compress_data: bool) -> Vec<u8> {
        let data = if compress_data {
            compress(PAYLOAD)
        } else {
            PAYLOAD.to_vec()
        };
        let data_compression = if compress_data {
            CompressionType::Lz4
        } else {
            CompressionType::None
        };
        let raw_blocks_info = make_blocks_info_record(
            u64::try_from(PAYLOAD.len()).unwrap(),
            u32::try_from(PAYLOAD.len()).unwrap(),
            u32::try_from(data.len()).unwrap(),
            data_compression,
        );
        let stored_blocks_info = if compress_info {
            compress(&raw_blocks_info)
        } else {
            raw_blocks_info.clone()
        };
        let info_compression = if compress_info {
            CompressionType::Lz4
        } else {
            CompressionType::None
        };
        let flags = BLOCKS_AND_DIRECTORY_INFO_COMBINED | u32::from(info_compression.code());

        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"UnityFS\0");
        bytes.extend_from_slice(&6_u32.to_be_bytes());
        bytes.extend_from_slice(b"5.x.x\0");
        bytes.extend_from_slice(b"2018.4.0f1\0");
        let size_position = bytes.len();
        bytes.extend_from_slice(&0_i64.to_be_bytes());
        bytes.extend_from_slice(
            &u32::try_from(stored_blocks_info.len())
                .unwrap()
                .to_be_bytes(),
        );
        bytes.extend_from_slice(&u32::try_from(raw_blocks_info.len()).unwrap().to_be_bytes());
        bytes.extend_from_slice(&flags.to_be_bytes());
        bytes.extend_from_slice(&stored_blocks_info);
        bytes.extend_from_slice(&data);

        let bundle_size = i64::try_from(bytes.len()).unwrap();
        bytes[size_position..size_position + 8].copy_from_slice(&bundle_size.to_be_bytes());
        bytes
    }

    fn make_oodle_bundle() -> (Vec<u8>, Vec<u8>) {
        let blocks_info = make_blocks_info_record(
            u64::try_from(PAYLOAD.len()).unwrap(),
            u32::try_from(PAYLOAD.len()).unwrap(),
            u32::try_from(OODLE_DATA_INPUT.len()).unwrap(),
            CompressionType::Oodle,
        );
        let flags = BLOCKS_AND_DIRECTORY_INFO_COMBINED | u32::from(CompressionType::Oodle.code());

        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"UnityFS\0");
        bytes.extend_from_slice(&6_u32.to_be_bytes());
        bytes.extend_from_slice(b"5.x.x\0");
        bytes.extend_from_slice(b"2018.4.0f1\0");
        let size_position = bytes.len();
        bytes.extend_from_slice(&0_i64.to_be_bytes());
        bytes.extend_from_slice(&u32::try_from(OODLE_INFO_INPUT.len()).unwrap().to_be_bytes());
        bytes.extend_from_slice(&u32::try_from(blocks_info.len()).unwrap().to_be_bytes());
        bytes.extend_from_slice(&flags.to_be_bytes());
        bytes.extend_from_slice(OODLE_INFO_INPUT);
        bytes.extend_from_slice(OODLE_DATA_INPUT);

        let bundle_size = i64::try_from(bytes.len()).unwrap();
        bytes[size_position..size_position + 8].copy_from_slice(&bundle_size.to_be_bytes());
        (bytes, blocks_info)
    }

    /// Two entries packed into one compressed block: reads through a shared
    /// decode cache must equal independent full reads, and a prefix copy
    /// stops at the requested count instead of demanding the whole entry.
    #[test]
    fn shared_block_cache_and_prefix_reads_match_full_entry_reads() {
        let payload: Vec<u8> = (0_u32..512).flat_map(u32::to_le_bytes).collect();
        let split = 800_usize;
        let compressed = compress(&payload);

        let mut info = vec![0_u8; 16];
        info.extend_from_slice(&1_i32.to_be_bytes());
        info.extend_from_slice(&u32::try_from(payload.len()).unwrap().to_be_bytes());
        info.extend_from_slice(&u32::try_from(compressed.len()).unwrap().to_be_bytes());
        info.extend_from_slice(&u16::from(CompressionType::Lz4.code()).to_be_bytes());
        info.extend_from_slice(&2_i32.to_be_bytes());
        for (offset, size, path) in [
            (0_usize, split, b"a.bin".as_slice()),
            (split, payload.len() - split, b"b.bin".as_slice()),
        ] {
            info.extend_from_slice(&i64::try_from(offset).unwrap().to_be_bytes());
            info.extend_from_slice(&i64::try_from(size).unwrap().to_be_bytes());
            info.extend_from_slice(&0_u32.to_be_bytes());
            info.extend_from_slice(path);
            info.push(0);
        }

        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"UnityFS\0");
        bytes.extend_from_slice(&6_u32.to_be_bytes());
        bytes.extend_from_slice(b"5.x.x\0");
        bytes.extend_from_slice(b"2018.4.0f1\0");
        let size_position = bytes.len();
        bytes.extend_from_slice(&0_i64.to_be_bytes());
        bytes.extend_from_slice(&u32::try_from(info.len()).unwrap().to_be_bytes());
        bytes.extend_from_slice(&u32::try_from(info.len()).unwrap().to_be_bytes());
        bytes.extend_from_slice(&BLOCKS_AND_DIRECTORY_INFO_COMBINED.to_be_bytes());
        bytes.extend_from_slice(&info);
        bytes.extend_from_slice(&compressed);
        let bundle_size = i64::try_from(bytes.len()).unwrap();
        bytes[size_position..size_position + 8].copy_from_slice(&bundle_size.to_be_bytes());

        let bundle = UnityFsBundle::open(&Region::from_bytes(bytes)).unwrap();
        assert_eq!(bundle.entries.len(), 2);

        let mut cache = super::BlockDecodeCache::new();
        let first = bundle.read_entry_with_cache(0, &mut cache).unwrap();
        let second = bundle.read_entry_with_cache(1, &mut cache).unwrap();
        assert_eq!(first, &payload[..split]);
        assert_eq!(second, &payload[split..]);
        assert_eq!(first, bundle.read_entry(0).unwrap());
        assert_eq!(second, bundle.read_entry(1).unwrap());

        // Prefix copies stop at the requested count, both under and over the
        // entry size, and reuse the same cache.
        let mut prefix = Vec::new();
        let copied = bundle
            .copy_entry_prefix(1, 16, &mut prefix, &mut cache)
            .unwrap();
        assert_eq!(copied, 16);
        assert_eq!(prefix, &payload[split..split + 16]);
        let mut whole = Vec::new();
        let copied = bundle
            .copy_entry_prefix(1, u64::MAX, &mut whole, &mut cache)
            .unwrap();
        assert_eq!(copied, second.len() as u64);
        assert_eq!(whole, second);
        assert!(
            bundle
                .copy_entry_prefix(9, 16, &mut Vec::new(), &mut cache)
                .is_err()
        );
    }

    fn make_encoded_data_bundle(data: &[u8], compression: CompressionType) -> Vec<u8> {
        let raw_blocks_info = make_blocks_info_record(
            u64::try_from(PAYLOAD.len()).unwrap(),
            u32::try_from(PAYLOAD.len()).unwrap(),
            u32::try_from(data.len()).unwrap(),
            compression,
        );
        let flags = BLOCKS_AND_DIRECTORY_INFO_COMBINED;

        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"UnityFS\0");
        bytes.extend_from_slice(&6_u32.to_be_bytes());
        bytes.extend_from_slice(b"5.x.x\0");
        bytes.extend_from_slice(b"2018.4.0f1\0");
        let size_position = bytes.len();
        bytes.extend_from_slice(&0_i64.to_be_bytes());
        bytes.extend_from_slice(&u32::try_from(raw_blocks_info.len()).unwrap().to_be_bytes());
        bytes.extend_from_slice(&u32::try_from(raw_blocks_info.len()).unwrap().to_be_bytes());
        bytes.extend_from_slice(&flags.to_be_bytes());
        bytes.extend_from_slice(&raw_blocks_info);
        bytes.extend_from_slice(data);

        let bundle_size = i64::try_from(bytes.len()).unwrap();
        bytes[size_position..size_position + 8].copy_from_slice(&bundle_size.to_be_bytes());
        bytes
    }

    fn pad_to(bytes: &mut Vec<u8>, alignment: usize) {
        let padding = (alignment - bytes.len() % alignment) % alignment;
        bytes.resize(bytes.len() + padding, 0);
    }
}
