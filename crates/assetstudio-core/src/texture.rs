use std::io::Write;

use crate::endian::{Endian, EndianReader, checked_length};
use crate::loader::AssetCollection;
use crate::serialized::SerializedFile;
use crate::source::{Region, RegionCursor};
use crate::{Error, Result};

pub const TEXTURE_2D_CLASS_ID: i32 = 28;

const NO_TARGET_PLATFORM: i32 = -2;
const XBOX_360_TARGET_PLATFORM: i32 = 11;
const SWITCH_TARGET_PLATFORM: i32 = 38;
const SWITCH_GOB_WIDTH_IN_CHUNKS: u64 = 4;
const SWITCH_GOB_HEIGHT_IN_CHUNKS: u64 = 8;
const SWITCH_GOB_CHUNK_BYTES: u64 = 16;
const MAXIMUM_SWITCH_BLOCK_HEIGHT_LOG2: u32 = 5;
const CRUNCH_HEADER_PREFIX_BYTES: usize = 70;
const CRUNCH_LEVEL_OFFSET_BYTES: usize = 4;
const CRUNCH_MAXIMUM_LEVELS: u8 = 16;
// At most five persistent, two local, and one nested Huffman model coexist.
// A valid model is bounded by 8,192 code bytes, 8,192 lookup bytes, and 16,384
// sorted-symbol bytes. A malformed model can briefly request 16,383 code bytes
// before the dependency rejects it. Doubling the 256-KiB valid maximum covers
// that path, Vec capacity rounding, and allocator bookkeeping.
const CRUNCH_HUFFMAN_WORKING_BYTES: u64 = 512 * 1024;
const SWITCH_GOB_X_POSITIONS: [u64; 32] = [
    0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1, 2, 2, 3, 3, 2, 2, 3, 3, 2, 2, 3, 3, 2, 2, 3, 3,
];
const SWITCH_GOB_Y_POSITIONS: [u64; 32] = [
    0, 1, 0, 1, 2, 3, 2, 3, 4, 5, 4, 5, 6, 7, 6, 7, 0, 1, 0, 1, 2, 3, 2, 3, 4, 5, 4, 5, 6, 7, 6, 7,
];
const RGBA_IR_MAGIC: &[u8; 16] = b"HARUKI_RGBAIR_V1";
const RGBA_IR_HEADER_SIZE: usize = 36;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextureReadLimits {
    pub maximum_dimension: u32,
    pub maximum_mip_levels: u32,
    pub maximum_image_count: u32,
    pub maximum_string_bytes: usize,
    pub maximum_platform_blob_bytes: usize,
    pub maximum_payload_bytes: u64,
    pub maximum_output_bytes: u64,
    pub maximum_decoder_working_bytes: u64,
}

impl Default for TextureReadLimits {
    fn default() -> Self {
        Self {
            maximum_dimension: 65_536,
            maximum_mip_levels: 32,
            maximum_image_count: 1_024,
            maximum_string_bytes: 16 * 1024 * 1024,
            maximum_platform_blob_bytes: 64 * 1024 * 1024,
            maximum_payload_bytes: 512 * 1024 * 1024,
            maximum_output_bytes: 512 * 1024 * 1024,
            maximum_decoder_working_bytes: 512 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextureFormat(pub i32);

impl TextureFormat {
    pub const ALPHA8: Self = Self(1);
    pub const ARGB4444: Self = Self(2);
    pub const RGB24: Self = Self(3);
    pub const RGBA32: Self = Self(4);
    pub const ARGB32: Self = Self(5);
    pub const RGB565: Self = Self(7);
    pub const BGR24: Self = Self(8);
    pub const R16: Self = Self(9);
    pub const DXT1: Self = Self(10);
    pub const DXT3: Self = Self(11);
    pub const DXT5: Self = Self(12);
    pub const RGBA4444: Self = Self(13);
    pub const BGRA32: Self = Self(14);
    pub const R_HALF: Self = Self(15);
    pub const RG_HALF: Self = Self(16);
    pub const RGBA_HALF: Self = Self(17);
    pub const R_FLOAT: Self = Self(18);
    pub const RG_FLOAT: Self = Self(19);
    pub const RGBA_FLOAT: Self = Self(20);
    pub const YUY2: Self = Self(21);
    pub const RGB9E5_FLOAT: Self = Self(22);
    pub const BC6H: Self = Self(24);
    pub const BC7: Self = Self(25);
    pub const BC4: Self = Self(26);
    pub const BC5: Self = Self(27);
    pub const DXT1_CRUNCHED: Self = Self(28);
    pub const DXT5_CRUNCHED: Self = Self(29);
    pub const PVRTC_RGB2: Self = Self(30);
    pub const PVRTC_RGBA2: Self = Self(31);
    pub const PVRTC_RGB4: Self = Self(32);
    pub const PVRTC_RGBA4: Self = Self(33);
    pub const ETC_RGB4: Self = Self(34);
    pub const ATC_RGB4: Self = Self(35);
    pub const ATC_RGBA8: Self = Self(36);
    pub const EAC_R: Self = Self(41);
    pub const EAC_R_SIGNED: Self = Self(42);
    pub const EAC_RG: Self = Self(43);
    pub const EAC_RG_SIGNED: Self = Self(44);
    pub const ETC2_RGB: Self = Self(45);
    pub const ETC2_RGBA1: Self = Self(46);
    pub const ETC2_RGBA8: Self = Self(47);
    pub const ASTC_RGB_4X4: Self = Self(48);
    pub const ASTC_RGB_5X5: Self = Self(49);
    pub const ASTC_RGB_6X6: Self = Self(50);
    pub const ASTC_RGB_8X8: Self = Self(51);
    pub const ASTC_RGB_10X10: Self = Self(52);
    pub const ASTC_RGB_12X12: Self = Self(53);
    pub const ASTC_RGBA_4X4: Self = Self(54);
    pub const ASTC_RGBA_5X5: Self = Self(55);
    pub const ASTC_RGBA_6X6: Self = Self(56);
    pub const ASTC_RGBA_8X8: Self = Self(57);
    pub const ASTC_RGBA_10X10: Self = Self(58);
    pub const ASTC_RGBA_12X12: Self = Self(59);
    pub const ETC_RGB4_3DS: Self = Self(60);
    pub const ETC_RGBA8_3DS: Self = Self(61);
    pub const RG16: Self = Self(62);
    pub const R8: Self = Self(63);
    pub const ETC_RGB4_CRUNCHED: Self = Self(64);
    pub const ETC2_RGBA8_CRUNCHED: Self = Self(65);
    pub const ASTC_HDR_4X4: Self = Self(66);
    pub const ASTC_HDR_5X5: Self = Self(67);
    pub const ASTC_HDR_6X6: Self = Self(68);
    pub const ASTC_HDR_8X8: Self = Self(69);
    pub const ASTC_HDR_10X10: Self = Self(70);
    pub const ASTC_HDR_12X12: Self = Self(71);
    pub const RG32: Self = Self(72);
    pub const RGB48: Self = Self(73);
    pub const RGBA64: Self = Self(74);

    fn layout(self) -> Result<TextureLayout> {
        if let Some(block_width) = self.astc_block_width() {
            return Ok(TextureLayout::Block {
                block_width,
                block_height: block_width,
                bytes_per_block: 16,
            });
        }
        match self {
            Self::ALPHA8 | Self::R8 => Ok(TextureLayout::Linear { bytes_per_pixel: 1 }),
            Self::ARGB4444
            | Self::RGB565
            | Self::R16
            | Self::RGBA4444
            | Self::R_HALF
            | Self::YUY2
            | Self::RG16 => Ok(TextureLayout::Linear { bytes_per_pixel: 2 }),
            Self::RGB24 | Self::BGR24 => Ok(TextureLayout::Linear { bytes_per_pixel: 3 }),
            Self::RGBA32
            | Self::ARGB32
            | Self::BGRA32
            | Self::RG_HALF
            | Self::R_FLOAT
            | Self::RGB9E5_FLOAT
            | Self::RG32 => Ok(TextureLayout::Linear { bytes_per_pixel: 4 }),
            Self::RGB48 => Ok(TextureLayout::Linear { bytes_per_pixel: 6 }),
            Self::RGBA_HALF | Self::RG_FLOAT | Self::RGBA64 => {
                Ok(TextureLayout::Linear { bytes_per_pixel: 8 })
            }
            Self::RGBA_FLOAT => Ok(TextureLayout::Linear {
                bytes_per_pixel: 16,
            }),
            Self::DXT1
            | Self::BC4
            | Self::PVRTC_RGB4
            | Self::PVRTC_RGBA4
            | Self::ETC_RGB4
            | Self::ATC_RGB4
            | Self::EAC_R
            | Self::EAC_R_SIGNED
            | Self::ETC2_RGB
            | Self::ETC2_RGBA1
            | Self::ETC_RGB4_3DS => Ok(TextureLayout::Block {
                block_width: 4,
                block_height: 4,
                bytes_per_block: 8,
            }),
            Self::PVRTC_RGB2 | Self::PVRTC_RGBA2 => Ok(TextureLayout::Block {
                block_width: 8,
                block_height: 4,
                bytes_per_block: 8,
            }),
            Self::DXT3
            | Self::DXT5
            | Self::BC5
            | Self::BC6H
            | Self::BC7
            | Self::ATC_RGBA8
            | Self::EAC_RG
            | Self::EAC_RG_SIGNED
            | Self::ETC2_RGBA8
            | Self::ETC_RGBA8_3DS => Ok(TextureLayout::Block {
                block_width: 4,
                block_height: 4,
                bytes_per_block: 16,
            }),
            _ => Err(Error::unsupported(format!(
                "Texture2D format {}{} has no native Rust decoder",
                self.0,
                self.name()
                    .map_or_else(String::new, |name| format!(" ({name})"))
            ))),
        }
    }

    fn astc_block_width(self) -> Option<u64> {
        match self {
            Self::ASTC_RGB_4X4 | Self::ASTC_RGBA_4X4 | Self::ASTC_HDR_4X4 => Some(4),
            Self::ASTC_RGB_5X5 | Self::ASTC_RGBA_5X5 | Self::ASTC_HDR_5X5 => Some(5),
            Self::ASTC_RGB_6X6 | Self::ASTC_RGBA_6X6 | Self::ASTC_HDR_6X6 => Some(6),
            Self::ASTC_RGB_8X8 | Self::ASTC_RGBA_8X8 | Self::ASTC_HDR_8X8 => Some(8),
            Self::ASTC_RGB_10X10 | Self::ASTC_RGBA_10X10 | Self::ASTC_HDR_10X10 => Some(10),
            Self::ASTC_RGB_12X12 | Self::ASTC_RGBA_12X12 | Self::ASTC_HDR_12X12 => Some(12),
            _ => None,
        }
    }

    fn pvrtc_block_width(self) -> Option<u64> {
        match self {
            Self::PVRTC_RGB2 | Self::PVRTC_RGBA2 => Some(8),
            Self::PVRTC_RGB4 | Self::PVRTC_RGBA4 => Some(4),
            _ => None,
        }
    }

    fn is_crunched(self) -> bool {
        matches!(
            self,
            Self::DXT1_CRUNCHED
                | Self::DXT5_CRUNCHED
                | Self::ETC_RGB4_CRUNCHED
                | Self::ETC2_RGBA8_CRUNCHED
        )
    }

    fn is_etc_crunched(self) -> bool {
        matches!(self, Self::ETC_RGB4_CRUNCHED | Self::ETC2_RGBA8_CRUNCHED)
    }

    fn name(self) -> Option<&'static str> {
        Some(match self.0 {
            1 => "Alpha8",
            2 => "ARGB4444",
            3 => "RGB24",
            4 => "RGBA32",
            5 => "ARGB32",
            7 => "RGB565",
            8 => "BGR24",
            9 => "R16",
            10 => "DXT1",
            11 => "DXT3",
            12 => "DXT5",
            13 => "RGBA4444",
            14 => "BGRA32",
            15 => "RHalf",
            16 => "RGHalf",
            17 => "RGBAHalf",
            18 => "RFloat",
            19 => "RGFloat",
            20 => "RGBAFloat",
            21 => "YUY2",
            22 => "RGB9e5Float",
            24 => "BC6H",
            25 => "BC7",
            26 => "BC4",
            27 => "BC5",
            28 => "DXT1Crunched",
            29 => "DXT5Crunched",
            30 => "PVRTC_RGB2",
            31 => "PVRTC_RGBA2",
            32 => "PVRTC_RGB4",
            33 => "PVRTC_RGBA4",
            34 => "ETC_RGB4",
            35 => "ATC_RGB4",
            36 => "ATC_RGBA8",
            41 => "EAC_R",
            42 => "EAC_R_SIGNED",
            43 => "EAC_RG",
            44 => "EAC_RG_SIGNED",
            45 => "ETC2_RGB",
            46 => "ETC2_RGBA1",
            47 => "ETC2_RGBA8",
            48 => "ASTC_RGB_4x4",
            49 => "ASTC_RGB_5x5",
            50 => "ASTC_RGB_6x6",
            51 => "ASTC_RGB_8x8",
            52 => "ASTC_RGB_10x10",
            53 => "ASTC_RGB_12x12",
            54 => "ASTC_RGBA_4x4",
            55 => "ASTC_RGBA_5x5",
            56 => "ASTC_RGBA_6x6",
            57 => "ASTC_RGBA_8x8",
            58 => "ASTC_RGBA_10x10",
            59 => "ASTC_RGBA_12x12",
            60 => "ETC_RGB4_3DS",
            61 => "ETC_RGBA8_3DS",
            62 => "RG16",
            63 => "R8",
            64 => "ETC_RGB4Crunched",
            65 => "ETC2_RGBA8Crunched",
            66 => "ASTC_HDR_4x4",
            67 => "ASTC_HDR_5x5",
            68 => "ASTC_HDR_6x6",
            69 => "ASTC_HDR_8x8",
            70 => "ASTC_HDR_10x10",
            71 => "ASTC_HDR_12x12",
            72 => "RG32",
            73 => "RGB48",
            74 => "RGBA64",
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextureLayout {
    Linear {
        bytes_per_pixel: u64,
    },
    Block {
        block_width: u64,
        block_height: u64,
        bytes_per_block: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CrunchCodec {
    Classic,
    Unity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CrunchPalette {
    offset: u32,
    size: u32,
    entries: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CrunchHeader {
    width: u16,
    height: u16,
    levels: u8,
    faces: u8,
    format: u8,
    palettes: [CrunchPalette; 4],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Texture2D {
    pub path_id: i64,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub complete_image_size: u32,
    pub mips_stripped: u32,
    pub mip_count: u32,
    pub format: TextureFormat,
    pub image_count: u32,
    pub dimension: i32,
    pub platform_blob_size: u64,
    /// Unity's source-bound platform payload. A non-empty blob on target 38
    /// carries the Nintendo Switch block-height information used at decode time.
    pub platform_blob: Option<Region>,
    pub target_platform: i32,
    /// Mirrors `AssetStudio`'s exact Crunch split: DXT crunched textures use
    /// `UnityCrunch` from Unity 2017.3 onward, while ETC crunched textures
    /// always use `UnityCrunch`.
    pub uses_unity_crunch: bool,
    pub data: Region,
}

impl Texture2D {
    pub fn mip_dimensions(&self, mip_level: u32) -> Result<(u32, u32)> {
        if mip_level >= self.mip_count {
            return Err(Error::invalid_data(format!(
                "Texture2D mip level {mip_level} exceeds {} levels",
                self.mip_count
            )));
        }
        if mip_level < self.mips_stripped {
            return Err(Error::unsupported(format!(
                "Texture2D mip level {mip_level} was stripped; first resident level is {}",
                self.mips_stripped
            )));
        }
        Ok((
            self.width.checked_shr(mip_level).unwrap_or(0).max(1),
            self.height.checked_shr(mip_level).unwrap_or(0).max(1),
        ))
    }

    pub fn mip_region(&self, mip_level: u32) -> Result<Region> {
        let (width, height) = self.mip_dimensions(mip_level)?;
        if self.format.is_crunched() {
            if mip_level != 0 || self.mips_stripped != 0 {
                return Err(Error::unsupported(format!(
                    "crunched Texture2D mip level {mip_level} is unavailable; only an unstripped level zero is implemented"
                )));
            }
            return Ok(self.data.clone());
        }
        let mut offset = 0_u64;
        for level in self.mips_stripped..mip_level {
            let (level_width, level_height) = self.mip_dimensions(level)?;
            offset = offset
                .checked_add(mip_byte_length(level_width, level_height, self.format)?)
                .ok_or_else(|| Error::invalid_data("Texture2D mip offset overflowed"))?;
        }
        self.data
            .subregion(offset, mip_byte_length(width, height, self.format)?)
    }

    pub fn decode_mip_rgba8(&self, mip_level: u32, limits: TextureReadLimits) -> Result<RgbaImage> {
        if self.image_count != 1 {
            return Err(Error::unsupported(format!(
                "Texture2D image count {} (only a single 2D image is implemented)",
                self.image_count
            )));
        }
        if self.dimension != 2 {
            return Err(Error::unsupported(format!(
                "Texture2D dimension {} (only Tex2D dimension 2 is implemented)",
                self.dimension
            )));
        }
        if self.uses_switch_swizzle() {
            return self.decode_switch_mip0_rgba8(mip_level, limits);
        }
        if self.format.is_crunched() {
            return self.decode_crunched_mip0_rgba8(mip_level, limits);
        }

        let (width, height) = self.mip_dimensions(mip_level)?;
        validate_dimensions(width, height, &limits)?;
        validate_pvrtc_decode_dimensions(self.format, width, height)?;
        let input = self.mip_region(mip_level)?;
        let pixel_count = checked_product(u64::from(width), u64::from(height), "pixel count")?;
        let output_length = checked_product(pixel_count, 4, "RGBA output byte size")?;
        if output_length > limits.maximum_output_bytes {
            return Err(Error::invalid_data(format!(
                "Texture2D RGBA output is {output_length} bytes, exceeding limit {}",
                limits.maximum_output_bytes
            )));
        }
        if matches!(self.format.layout()?, TextureLayout::Block { .. })
            && !uses_internal_block_decoder(self.format)
        {
            let working_length =
                external_decoder_working_byte_length(self.format, width, height, output_length)?;
            if working_length > limits.maximum_decoder_working_bytes {
                return Err(Error::invalid_data(format!(
                    "Texture2D decoder working buffers are {working_length} bytes, exceeding limit {}",
                    limits.maximum_decoder_working_bytes
                )));
            }
        }
        let mut input_bytes = input.read_to_vec(limits.maximum_payload_bytes)?;
        if self.target_platform == XBOX_360_TARGET_PLATFORM
            && matches!(
                self.format,
                TextureFormat::ARGB4444
                    | TextureFormat::RGB565
                    | TextureFormat::DXT1
                    | TextureFormat::DXT5
            )
        {
            swap_adjacent_bytes(&mut input_bytes)?;
        }
        let output_length = usize::try_from(output_length)
            .map_err(|_| Error::invalid_data("Texture2D output is too large for this platform"))?;
        let mut pixels = Vec::new();
        pixels.try_reserve_exact(output_length).map_err(|error| {
            Error::invalid_data(format!("cannot allocate Texture2D RGBA output: {error}"))
        })?;
        pixels.resize(output_length, 0);
        decode_pixels(self.format, &input_bytes, &mut pixels, width, height)?;

        Ok(RgbaImage {
            width,
            height,
            pixels,
        })
    }

    fn decode_crunched_mip0_rgba8(
        &self,
        mip_level: u32,
        limits: TextureReadLimits,
    ) -> Result<RgbaImage> {
        if mip_level != 0 || self.mips_stripped != 0 {
            return Err(Error::unsupported(format!(
                "crunched Texture2D mip level {mip_level} is unavailable; only an unstripped level zero is implemented"
            )));
        }
        let (width, height) = self.mip_dimensions(mip_level)?;
        validate_dimensions(width, height, &limits)?;
        let output_length = rgba_output_byte_length(width, height)?;
        if output_length > limits.maximum_output_bytes {
            return Err(Error::invalid_data(format!(
                "Texture2D RGBA output is {output_length} bytes, exceeding limit {}",
                limits.maximum_output_bytes
            )));
        }

        let input = self.data.read_to_vec(limits.maximum_payload_bytes)?;
        let codec = if self.uses_unity_crunch {
            CrunchCodec::Unity
        } else {
            CrunchCodec::Classic
        };
        let header = parse_crunch_header(&input)?;
        validate_crunch_texture(&header, self.format, width, height, codec)?;
        let working_length = crunch_decoder_working_byte_length(&header, codec, output_length)?;
        if working_length > limits.maximum_decoder_working_bytes {
            return Err(Error::invalid_data(format!(
                "Texture2D Crunch decoder working buffers are {working_length} bytes, exceeding limit {}",
                limits.maximum_decoder_working_bytes
            )));
        }

        let output_length = usize::try_from(output_length)
            .map_err(|_| Error::invalid_data("Texture2D output is too large for this platform"))?;
        let mut pixels = try_zeroed_vec(output_length, "crunched Texture2D RGBA output")?;
        decode_crunched_pixels(self.format, codec, &input, &mut pixels, width, height)?;
        Ok(RgbaImage {
            width,
            height,
            pixels,
        })
    }

    fn uses_switch_swizzle(&self) -> bool {
        self.target_platform == SWITCH_TARGET_PLATFORM
            && self
                .platform_blob
                .as_ref()
                .is_some_and(|blob| !blob.is_empty())
    }

    fn decode_switch_mip0_rgba8(
        &self,
        mip_level: u32,
        limits: TextureReadLimits,
    ) -> Result<RgbaImage> {
        if mip_level != 0 {
            return Err(Error::unsupported(format!(
                "Nintendo Switch Texture2D mip level {mip_level} is not implemented; only mip zero has a sample-verified block-linear layout"
            )));
        }
        if self.mips_stripped != 0 {
            return Err(Error::unsupported(format!(
                "Nintendo Switch Texture2D has {} stripped mip level(s); reconstructing mip zero is not implemented",
                self.mips_stripped
            )));
        }
        let plan = self.plan_switch_mip0_decode(&limits)?;

        // AssetStudio's managed Switch converter consumes the block-linear
        // base image at the start of `image_data` and ignores lower mips. Keep
        // the read source-bound so a large chain does not get materialized just
        // to export mip zero.
        let swizzled = self
            .data
            .subregion(0, plan.encoded_length)?
            .read_to_vec(limits.maximum_payload_bytes)?;
        let encoded_length = usize::try_from(plan.encoded_length).map_err(|_| {
            Error::invalid_data("Nintendo Switch Texture2D input is too large for this platform")
        })?;
        let mut unswizzled = try_zeroed_vec(encoded_length, "Nintendo Switch Texture2D unswizzle")?;
        unswizzle_switch_texture(
            &swizzled,
            &mut unswizzled,
            plan.padded_width,
            plan.padded_height,
            plan.layout,
            plan.gobs_per_block,
        )?;
        drop(swizzled);

        let padded_output_length = usize::try_from(plan.padded_output_length).map_err(|_| {
            Error::invalid_data(
                "Nintendo Switch Texture2D padded output is too large for this platform",
            )
        })?;
        let mut pixels = try_zeroed_vec(
            padded_output_length,
            "Nintendo Switch Texture2D padded RGBA output",
        )?;
        decode_pixels(
            plan.layout.decode_format,
            &unswizzled,
            &mut pixels,
            plan.padded_width,
            plan.padded_height,
        )?;
        drop(unswizzled);
        crop_switch_rgba_top_left(
            &mut pixels,
            plan.padded_width,
            self.width,
            self.height,
            plan.output_length,
        )?;

        Ok(RgbaImage {
            width: self.width,
            height: self.height,
            pixels,
        })
    }

    fn plan_switch_mip0_decode(&self, limits: &TextureReadLimits) -> Result<SwitchDecodePlan> {
        validate_dimensions(self.width, self.height, limits)?;
        let layout = switch_texture_layout(self.format)?;
        let platform_blob = self
            .platform_blob
            .as_ref()
            .expect("a Switch-swizzled texture has a non-empty platform blob");
        let gobs_per_block = switch_gobs_per_block(platform_blob)?;
        let (padded_width, padded_height) = switch_padded_dimensions(
            self.width,
            self.height,
            layout.texel_width,
            layout.texel_height,
            gobs_per_block,
        )?;
        validate_dimensions(padded_width, padded_height, limits)?;
        let encoded_length = mip_byte_length(padded_width, padded_height, layout.decode_format)?;
        validate_switch_input_length(
            self.data.len(),
            encoded_length,
            padded_width,
            padded_height,
            self.mip_count > 1,
            limits,
        )?;
        let output_length = rgba_byte_length(self.width, self.height, "cropped")?;
        if output_length > limits.maximum_output_bytes {
            return Err(Error::invalid_data(format!(
                "Texture2D RGBA output is {output_length} bytes, exceeding limit {}",
                limits.maximum_output_bytes
            )));
        }
        let padded_output_length = rgba_byte_length(padded_width, padded_height, "padded")?;
        validate_switch_working_length(
            layout.decode_format,
            padded_width,
            padded_height,
            encoded_length,
            padded_output_length,
            limits,
        )?;
        Ok(SwitchDecodePlan {
            layout,
            gobs_per_block,
            padded_width,
            padded_height,
            encoded_length,
            padded_output_length,
            output_length,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SwitchTextureLayout {
    decode_format: TextureFormat,
    /// Number of source pixels represented by one 16-byte GOB chunk.
    texel_width: u32,
    texel_height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SwitchDecodePlan {
    layout: SwitchTextureLayout,
    gobs_per_block: u32,
    padded_width: u32,
    padded_height: u32,
    encoded_length: u64,
    padded_output_length: u64,
    output_length: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbaImage {
    pub width: u32,
    pub height: u32,
    /// Tightly packed RGBA8 rows in Unity's decoded row order.
    pub pixels: Vec<u8>,
}

/// Writes the same `HARUKI_RGBAIR_V1` payload used by the existing managed API:
/// a 36-byte little-endian header followed by vertically flipped RGBA8 rows.
pub fn write_rgba_ir<W: Write>(image: &RgbaImage, output: &mut W) -> Result<u64> {
    write_rgba_ir_rows(image, output, true)
}

/// Writes RGBA IR from an image whose rows are already in display order.
///
/// Sprite rendering performs its vertical flip while cropping, matching the
/// existing `ImageSharp` path. Unlike [`write_rgba_ir`], this preserves that
/// row order instead of flipping it a second time.
pub fn write_rgba_ir_display_order<W: Write>(image: &RgbaImage, output: &mut W) -> Result<u64> {
    write_rgba_ir_rows(image, output, false)
}

fn write_rgba_ir_rows<W: Write>(
    image: &RgbaImage,
    output: &mut W,
    flip_vertical: bool,
) -> Result<u64> {
    let stride = checked_product(u64::from(image.width), 4, "RGBA row stride")?;
    let pixel_length = checked_product(stride, u64::from(image.height), "RGBA byte size")?;
    let actual_length = u64::try_from(image.pixels.len())
        .map_err(|_| Error::invalid_data("RGBA pixel buffer length does not fit in u64"))?;
    if actual_length != pixel_length {
        return Err(Error::invalid_data(format!(
            "RGBA pixel buffer is {actual_length} bytes, expected {pixel_length}"
        )));
    }
    let width = i32::try_from(image.width)
        .map_err(|_| Error::invalid_data("RGBA width does not fit in its IR header"))?;
    let height = i32::try_from(image.height)
        .map_err(|_| Error::invalid_data("RGBA height does not fit in its IR header"))?;
    let stride_i32 = i32::try_from(stride)
        .map_err(|_| Error::invalid_data("RGBA stride does not fit in its IR header"))?;

    let mut header = [0_u8; RGBA_IR_HEADER_SIZE];
    header[..16].copy_from_slice(RGBA_IR_MAGIC);
    header[16..20].copy_from_slice(&width.to_le_bytes());
    header[20..24].copy_from_slice(&height.to_le_bytes());
    header[24..28].copy_from_slice(&stride_i32.to_le_bytes());
    header[28..32].copy_from_slice(&1_i32.to_le_bytes());
    header[32..36].copy_from_slice(&0_i32.to_le_bytes());
    output.write_all(&header)?;

    let stride = usize::try_from(stride)
        .map_err(|_| Error::invalid_data("RGBA row stride is too large for this platform"))?;
    let height = usize::try_from(image.height)
        .map_err(|_| Error::invalid_data("RGBA height is too large for this platform"))?;
    for output_row in 0..height {
        let row = if flip_vertical {
            height - 1 - output_row
        } else {
            output_row
        };
        let start = row
            .checked_mul(stride)
            .ok_or_else(|| Error::invalid_data("RGBA row offset overflowed"))?;
        let end = start
            .checked_add(stride)
            .ok_or_else(|| Error::invalid_data("RGBA row end overflowed"))?;
        output.write_all(&image.pixels[start..end])?;
    }
    u64::try_from(RGBA_IR_HEADER_SIZE)
        .expect("RGBA IR header size fits in u64")
        .checked_add(pixel_length)
        .ok_or_else(|| Error::invalid_data("RGBA IR length overflowed"))
}

pub fn read_texture2d(
    collection: &AssetCollection,
    file: &SerializedFile,
    object_index: usize,
    limits: TextureReadLimits,
) -> Result<Texture2D> {
    if file.unity_version.is_stripped() {
        return Err(Error::unsupported(
            "Texture2D requires a Unity version because its layout is version-dependent",
        ));
    }
    let mut reader = TextureObjectReader::new(file, object_index, limits)?;
    let name = reader.read_named_texture()?;
    let shape = read_texture_shape(&mut reader, file, &limits)?;
    skip_texture_import_settings(&mut reader, file)?;
    let tail = read_texture_tail(&mut reader, file, &limits)?;
    let data = read_texture_data(&mut reader, collection, file, &limits)?;

    Ok(Texture2D {
        path_id: reader.path_id,
        name,
        width: shape.width,
        height: shape.height,
        complete_image_size: shape.complete_image_size,
        mips_stripped: shape.mips_stripped,
        mip_count: shape.mip_count,
        format: shape.format,
        image_count: tail.image_count,
        dimension: tail.dimension,
        platform_blob_size: tail.platform_blob_size,
        platform_blob: tail.platform_blob,
        target_platform: file.target_platform,
        uses_unity_crunch: texture_uses_unity_crunch(shape.format, file.unity_version.components()),
        data,
    })
}

fn texture_uses_unity_crunch(format: TextureFormat, unity_components: (u32, u32, u32)) -> bool {
    format.is_etc_crunched() || unity_components >= (2017, 3, 0)
}

struct TextureShape {
    width: u32,
    height: u32,
    complete_image_size: u32,
    mips_stripped: u32,
    mip_count: u32,
    format: TextureFormat,
}

fn read_texture_shape(
    reader: &mut TextureObjectReader,
    file: &SerializedFile,
    limits: &TextureReadLimits,
) -> Result<TextureShape> {
    let width = reader.read_positive_dimension("Texture2D width")?;
    let height = reader.read_positive_dimension("Texture2D height")?;
    validate_dimensions(width, height, limits)?;
    let complete_image_size = reader.reader.read_u32()?;
    let mips_stripped = if file.unity_version.major >= 2020 {
        reader.read_non_negative_i32("Texture2D stripped mip count")?
    } else {
        0
    };

    if file.unity_version.is_tuanjie()
        && (file.unity_version.components() > (2022, 3, 2) || file.unity_version.build >= 8)
    {
        reader.skip(1, "Texture2D web-streaming flag")?;
        reader.align(4)?;
        reader.skip(8, "Texture2D Tuanjie priority settings")?;
        reader.skip(4, "Texture2D Tuanjie data-stream size")?;
        reader.read_aligned_string("Texture2D Tuanjie data-stream path")?;
    }

    let format = TextureFormat(reader.reader.read_i32()?);
    if file.unity_version.is_tuanjie() && file.unity_version.components() >= (2022, 3, 62) {
        let length = reader.read_bounded_blob_length(
            "Texture2D multi-format setting",
            limits.maximum_platform_blob_bytes,
        )?;
        reader.skip(
            u64::try_from(length).expect("usize length fits in u64"),
            "Texture2D multi-format setting",
        )?;
        reader.align(4)?;
    }

    let mip_count = if file.unity_version.components() < (5, 2, 0) {
        if reader.reader.read_bool()? {
            full_mip_count(width, height)
        } else {
            1
        }
    } else {
        reader.read_positive_count("Texture2D mip count", limits.maximum_mip_levels)?
    };
    if mip_count > limits.maximum_mip_levels {
        return Err(Error::invalid_data(format!(
            "Texture2D has {mip_count} mip levels, exceeding limit {}",
            limits.maximum_mip_levels
        )));
    }
    if mips_stripped > mip_count {
        return Err(Error::invalid_data(format!(
            "Texture2D strips {mips_stripped} mip levels but declares only {mip_count}"
        )));
    }

    Ok(TextureShape {
        width,
        height,
        complete_image_size,
        mips_stripped,
        mip_count,
        format,
    })
}

fn skip_texture_import_settings(
    reader: &mut TextureObjectReader,
    file: &SerializedFile,
) -> Result<()> {
    if file.unity_version.components() >= (2, 6, 0) {
        reader.skip(1, "Texture2D readable flag")?;
    }
    if file.unity_version.major >= 2020 {
        reader.skip(1, "Texture2D preprocessing flag")?;
    }
    if file.unity_version.components() >= (2019, 3, 0) {
        reader.skip(1, "Texture2D mip-limit flag")?;
        if file.unity_version.components() >= (2022, 2, 0) {
            reader.align(4)?;
        }
    }
    if file.unity_version.components() >= (3, 0, 0) && file.unity_version.components() < (5, 5, 0) {
        reader.skip(1, "Texture2D read-allowed flag")?;
    }
    if file.unity_version.components() >= (2022, 2, 0) {
        reader.read_aligned_string("Texture2D mip-limit group")?;
    }
    if file.unity_version.components() >= (2018, 2, 0) {
        reader.skip(1, "Texture2D streaming-mipmaps flag")?;
    }
    reader.align(4)?;
    if file.unity_version.components() >= (2018, 2, 0) {
        reader.skip(4, "Texture2D streaming-mipmaps priority")?;
    }
    Ok(())
}

struct TextureTail {
    image_count: u32,
    dimension: i32,
    platform_blob_size: u64,
    platform_blob: Option<Region>,
}

fn read_texture_tail(
    reader: &mut TextureObjectReader,
    file: &SerializedFile,
    limits: &TextureReadLimits,
) -> Result<TextureTail> {
    let image_count =
        reader.read_positive_count("Texture2D image count", limits.maximum_image_count)?;
    let dimension = reader.reader.read_i32()?;
    if file.unity_version.major >= 2017 {
        reader.skip(24, "Texture2D GL texture settings")?;
    } else {
        reader.skip(16, "Texture2D GL texture settings")?;
    }
    if file.unity_version.major >= 3 {
        reader.skip(4, "Texture2D lightmap format")?;
    }
    if file.unity_version.components() >= (3, 5, 0) {
        reader.skip(4, "Texture2D color space")?;
    }

    let platform_blob = if file.unity_version.components() >= (2020, 2, 0) {
        let length = reader.read_bounded_blob_length(
            "Texture2D platform blob",
            limits.maximum_platform_blob_bytes,
        )?;
        let blob = if length == 0 {
            None
        } else {
            Some(reader.inline_region(
                u64::try_from(length).expect("usize length fits in u64"),
                u64::try_from(limits.maximum_platform_blob_bytes).unwrap_or(u64::MAX),
                "Texture2D platform blob",
            )?)
        };
        reader.skip(
            u64::try_from(length).expect("usize length fits in u64"),
            "Texture2D platform blob",
        )?;
        reader.align(4)?;
        blob
    } else {
        None
    };
    let platform_blob_size = platform_blob.as_ref().map_or(0, Region::len);

    Ok(TextureTail {
        image_count,
        dimension,
        platform_blob_size,
        platform_blob,
    })
}

fn read_texture_data(
    reader: &mut TextureObjectReader,
    collection: &AssetCollection,
    file: &SerializedFile,
    limits: &TextureReadLimits,
) -> Result<Region> {
    let image_data_size = reader.read_non_negative_i32("Texture2D image data")?;
    let data = if image_data_size == 0 && file.unity_version.components() >= (5, 3, 0) {
        let stream_offset = if file.unity_version.major >= 2020 {
            reader.read_non_negative_i64("Texture2D stream offset")?
        } else {
            u64::from(reader.reader.read_u32()?)
        };
        let stream_size = u64::from(reader.reader.read_u32()?);
        let stream_path = reader.read_aligned_string("Texture2D stream path")?;
        if stream_path.is_empty() {
            reader.inline_region(0, limits.maximum_payload_bytes, "Texture2D image data")?
        } else {
            resolve_external_region(
                collection,
                &stream_path,
                stream_offset,
                stream_size,
                limits.maximum_payload_bytes,
            )?
        }
    } else {
        reader.inline_region(
            u64::from(image_data_size),
            limits.maximum_payload_bytes,
            "Texture2D image data",
        )?
    };
    Ok(data)
}

struct TextureObjectReader {
    region: Region,
    reader: EndianReader<RegionCursor>,
    absolute_start: u64,
    path_id: i64,
    target_platform: i32,
    format_version: u32,
    unity_components: (u32, u32, u32),
    limits: TextureReadLimits,
}

impl TextureObjectReader {
    fn new(file: &SerializedFile, object_index: usize, limits: TextureReadLimits) -> Result<Self> {
        let object = file.objects.get(object_index).ok_or_else(|| {
            Error::invalid_data(format!(
                "serialized object index {object_index} is out of range"
            ))
        })?;
        if object.class_id != TEXTURE_2D_CLASS_ID {
            return Err(Error::unsupported(format!(
                "object {} has class ID {}, not Texture2D ({TEXTURE_2D_CLASS_ID})",
                object.path_id, object.class_id
            )));
        }
        let region = file.object_region(object_index)?;
        let endian = if file.header.endianness == 0 {
            Endian::Little
        } else {
            Endian::Big
        };
        Ok(Self {
            reader: EndianReader::new(region.cursor(), endian),
            region,
            absolute_start: object.byte_start,
            path_id: object.path_id,
            target_platform: file.target_platform,
            format_version: file.header.version.0,
            unity_components: file.unity_version.components(),
            limits,
        })
    }

    fn read_named_texture(&mut self) -> Result<String> {
        if self.target_platform == NO_TARGET_PLATFORM {
            self.skip(4, "Object hide flags")?;
            self.read_pptr()?;
            self.read_pptr()?;
        }
        let name = self.read_aligned_string("Texture2D name")?;
        if self.unity_components >= (2017, 3, 0) {
            if self.unity_components < (2023, 2, 0) {
                self.skip(5, "Texture fallback settings")?;
            }
            if self.unity_components >= (2020, 2, 0) {
                self.skip(1, "Texture alpha-channel setting")?;
            }
            self.align(4)?;
        }
        Ok(name)
    }

    fn read_pptr(&mut self) -> Result<()> {
        self.skip(4, "PPtr file ID")?;
        if self.format_version < 14 {
            self.skip(4, "PPtr path ID")
        } else {
            self.skip(8, "PPtr path ID")
        }
    }

    fn read_aligned_string(&mut self, field: &str) -> Result<String> {
        let length = checked_length(self.reader.read_i32()?, field)?;
        if length > self.limits.maximum_string_bytes {
            return Err(Error::invalid_data(format!(
                "{field} is {length} bytes, exceeding limit {}",
                self.limits.maximum_string_bytes
            )));
        }
        let value = self.reader.read_utf8(length)?;
        if length != 0 {
            self.align(4)?;
        }
        Ok(value)
    }

    fn read_non_negative_i32(&mut self, field: &str) -> Result<u32> {
        let value = self.reader.read_i32()?;
        u32::try_from(value)
            .map_err(|_| Error::invalid_data(format!("{field} cannot be negative: {value}")))
    }

    fn read_non_negative_i64(&mut self, field: &str) -> Result<u64> {
        let value = self.reader.read_i64()?;
        u64::try_from(value)
            .map_err(|_| Error::invalid_data(format!("{field} cannot be negative: {value}")))
    }

    fn read_positive_dimension(&mut self, field: &str) -> Result<u32> {
        let value = self.read_non_negative_i32(field)?;
        if value == 0 {
            return Err(Error::invalid_data(format!("{field} must be positive")));
        }
        Ok(value)
    }

    fn read_positive_count(&mut self, field: &str, maximum: u32) -> Result<u32> {
        let value = self.read_non_negative_i32(field)?;
        if value == 0 {
            return Err(Error::invalid_data(format!("{field} must be positive")));
        }
        if value > maximum {
            return Err(Error::invalid_data(format!(
                "{field} {value} exceeds limit {maximum}"
            )));
        }
        Ok(value)
    }

    fn read_bounded_blob_length(&mut self, field: &str, maximum: usize) -> Result<usize> {
        let length = checked_length(self.reader.read_i32()?, field)?;
        if length > maximum {
            return Err(Error::invalid_data(format!(
                "{field} is {length} bytes, exceeding limit {maximum}"
            )));
        }
        Ok(length)
    }

    fn skip(&mut self, length: u64, field: &str) -> Result<()> {
        let position = self.reader.position()?;
        let target = position
            .checked_add(length)
            .ok_or_else(|| Error::invalid_data(format!("{field} position overflowed")))?;
        if target > self.region.len() {
            return Err(Error::invalid_data(format!(
                "{field} ends at {target}, beyond object size {}",
                self.region.len()
            )));
        }
        self.reader.set_position(target)
    }

    fn align(&mut self, alignment: u64) -> Result<()> {
        let position = self.reader.position()?;
        let absolute = self
            .absolute_start
            .checked_add(position)
            .ok_or_else(|| Error::invalid_data("object alignment position overflowed"))?;
        let remainder = absolute % alignment;
        if remainder == 0 {
            return Ok(());
        }
        self.skip(alignment - remainder, "object alignment")
    }

    fn inline_region(&mut self, length: u64, maximum: u64, field: &str) -> Result<Region> {
        if length > maximum {
            return Err(Error::invalid_data(format!(
                "{field} is {length} bytes, exceeding limit {maximum}"
            )));
        }
        self.region.subregion(self.reader.position()?, length)
    }
}

fn resolve_external_region(
    collection: &AssetCollection,
    path: &str,
    offset: u64,
    size: u64,
    maximum: u64,
) -> Result<Region> {
    if size > maximum {
        return Err(Error::invalid_data(format!(
            "Texture2D external data is {size} bytes, exceeding limit {maximum}"
        )));
    }
    let resource = collection
        .resource(path)
        .ok_or_else(|| Error::invalid_data(format!("external resource was not found: {path}")))?;
    resource.region.subregion(offset, size)
}

fn validate_dimensions(width: u32, height: u32, limits: &TextureReadLimits) -> Result<()> {
    if width > limits.maximum_dimension || height > limits.maximum_dimension {
        return Err(Error::invalid_data(format!(
            "Texture2D dimensions {width}x{height} exceed limit {}",
            limits.maximum_dimension
        )));
    }
    checked_product(u64::from(width), u64::from(height), "Texture2D pixel count")?;
    Ok(())
}

fn checked_product(left: u64, right: u64, field: &str) -> Result<u64> {
    left.checked_mul(right)
        .ok_or_else(|| Error::invalid_data(format!("{field} overflowed")))
}

fn rgba_output_byte_length(width: u32, height: u32) -> Result<u64> {
    let pixels = checked_product(
        u64::from(width),
        u64::from(height),
        "Texture2D RGBA pixel count",
    )?;
    checked_product(pixels, 4, "Texture2D RGBA output byte size")
}

fn try_zeroed_vec(length: usize, field: &str) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    output
        .try_reserve_exact(length)
        .map_err(|error| Error::invalid_data(format!("cannot allocate {field} buffer: {error}")))?;
    output.resize(length, 0);
    Ok(output)
}

fn parse_crunch_header(input: &[u8]) -> Result<CrunchHeader> {
    let (declared_length, header_size, levels) = validate_crunch_prefix(input)?;
    let palettes = parse_crunch_palettes(input, header_size, declared_length)?;
    validate_crunch_tables(input, header_size, declared_length)?;
    validate_crunch_level_offsets(input, levels, header_size, declared_length)?;

    Ok(CrunchHeader {
        width: read_crunch_be_u16(input, 12, "width")?,
        height: read_crunch_be_u16(input, 14, "height")?,
        levels,
        faces: input[17],
        format: input[18],
        palettes,
    })
}

fn validate_crunch_prefix(input: &[u8]) -> Result<(u32, u16, u8)> {
    if input.len() < CRUNCH_HEADER_PREFIX_BYTES + CRUNCH_LEVEL_OFFSET_BYTES {
        return Err(Error::invalid_data(format!(
            "Crunch payload is {} bytes; at least 74 bytes are required",
            input.len()
        )));
    }
    if input.get(..2) != Some(b"Hx") {
        return Err(Error::invalid_data(
            "Crunch payload is missing the Hx signature",
        ));
    }
    let input_length = u32::try_from(input.len())
        .map_err(|_| Error::invalid_data("Crunch payload exceeds the u32 decoder ABI"))?;
    let declared_length = read_crunch_be_u32(input, 6, "data size")?;
    if declared_length != input_length {
        return Err(Error::invalid_data(format!(
            "Crunch header declares {declared_length} bytes, but the payload contains {input_length}"
        )));
    }

    let levels = input[16];
    if levels == 0 || levels > CRUNCH_MAXIMUM_LEVELS {
        return Err(Error::invalid_data(format!(
            "Crunch level count {levels} is outside the validated range 1..={CRUNCH_MAXIMUM_LEVELS}"
        )));
    }
    let required_header_size = CRUNCH_HEADER_PREFIX_BYTES
        .checked_add(usize::from(levels) * CRUNCH_LEVEL_OFFSET_BYTES)
        .ok_or_else(|| Error::invalid_data("Crunch header size overflowed"))?;
    let header_size = read_crunch_be_u16(input, 2, "header size")?;
    let header_size_usize = usize::from(header_size);
    if header_size_usize < required_header_size || header_size_usize > input.len() {
        return Err(Error::invalid_data(format!(
            "Crunch header is {header_size} bytes, but {required_header_size}..={} bytes are required",
            input.len()
        )));
    }
    validate_crunch_checksums(input, header_size_usize)?;
    Ok((declared_length, header_size, levels))
}

fn parse_crunch_palettes(
    input: &[u8],
    header_size: u16,
    declared_length: u32,
) -> Result<[CrunchPalette; 4]> {
    let palettes = [33_usize, 41, 49, 57].map(|offset| {
        Ok(CrunchPalette {
            offset: read_crunch_be_u24(input, offset, "palette offset")?,
            size: read_crunch_be_u24(input, offset + 3, "palette size")?,
            entries: read_crunch_be_u16(input, offset + 6, "palette entry count")?,
        })
    });
    let palettes: [CrunchPalette; 4] = palettes
        .into_iter()
        .collect::<Result<Vec<_>>>()?
        .try_into()
        .expect("four parsed Crunch palettes retain their array length");
    for (index, palette) in palettes.iter().enumerate() {
        validate_crunch_region(
            palette.offset,
            palette.size,
            header_size,
            declared_length,
            &format!("palette {index}"),
        )?;
        if palette.entries != 0 && palette.size == 0 {
            return Err(Error::invalid_data(format!(
                "Crunch palette {index} declares {} entries in an empty region",
                palette.entries
            )));
        }
    }
    Ok(palettes)
}

fn validate_crunch_tables(input: &[u8], header_size: u16, declared_length: u32) -> Result<()> {
    let tables_size = read_crunch_be_u16(input, 65, "tables size")?;
    let tables_offset = read_crunch_be_u24(input, 67, "tables offset")?;
    if tables_size == 0 {
        return Err(Error::invalid_data("Crunch Huffman table region is empty"));
    }
    validate_crunch_region(
        tables_offset,
        u32::from(tables_size),
        header_size,
        declared_length,
        "Huffman tables",
    )
}

fn validate_crunch_level_offsets(
    input: &[u8],
    levels: u8,
    header_size: u16,
    declared_length: u32,
) -> Result<()> {
    let mut level_offsets = Vec::with_capacity(usize::from(levels));
    for level in 0..usize::from(levels) {
        let offset = read_crunch_be_u32(
            input,
            CRUNCH_HEADER_PREFIX_BYTES + level * CRUNCH_LEVEL_OFFSET_BYTES,
            "level offset",
        )?;
        if offset < u32::from(header_size) || offset >= declared_length {
            return Err(Error::invalid_data(format!(
                "Crunch level {level} offset {offset} is outside {header_size}..{declared_length}"
            )));
        }
        if level_offsets
            .last()
            .is_some_and(|previous| *previous >= offset)
        {
            return Err(Error::invalid_data(format!(
                "Crunch level {level} offset {offset} is not strictly increasing"
            )));
        }
        level_offsets.push(offset);
    }
    Ok(())
}

fn validate_crunch_checksums(input: &[u8], header_size: usize) -> Result<()> {
    let expected_header = read_crunch_be_u16(input, 4, "header checksum")?;
    let actual_header = crunch_crc16(&input[6..header_size]);
    if expected_header != actual_header {
        return Err(Error::invalid_data(format!(
            "Crunch header checksum is {expected_header:#06x}, expected {actual_header:#06x}"
        )));
    }
    let expected_data = read_crunch_be_u16(input, 10, "data checksum")?;
    let actual_data = crunch_crc16(&input[header_size..]);
    if expected_data != actual_data {
        return Err(Error::invalid_data(format!(
            "Crunch data checksum is {expected_data:#06x}, expected {actual_data:#06x}"
        )));
    }
    Ok(())
}

fn validate_crunch_region(
    offset: u32,
    size: u32,
    header_size: u16,
    data_size: u32,
    field: &str,
) -> Result<()> {
    if size == 0 {
        return Ok(());
    }
    let end = offset
        .checked_add(size)
        .ok_or_else(|| Error::invalid_data(format!("Crunch {field} end overflowed")))?;
    if offset < u32::from(header_size) || end > data_size {
        return Err(Error::invalid_data(format!(
            "Crunch {field} range {offset}..{end} is outside {header_size}..{data_size}"
        )));
    }
    Ok(())
}

fn validate_crunch_texture(
    header: &CrunchHeader,
    format: TextureFormat,
    width: u32,
    height: u32,
    codec: CrunchCodec,
) -> Result<()> {
    if header.faces != 1 {
        return Err(Error::unsupported(format!(
            "Crunch Texture2D contains {} faces; only one face is supported",
            header.faces
        )));
    }
    if u32::from(header.width) != width || u32::from(header.height) != height {
        return Err(Error::invalid_data(format!(
            "Crunch header dimensions {}x{} do not match Texture2D {width}x{height}",
            header.width, header.height
        )));
    }
    if u32::from(header.levels) > full_mip_count(width, height) {
        return Err(Error::invalid_data(format!(
            "Crunch declares {} mip levels for {width}x{height}",
            header.levels
        )));
    }
    let format_matches = match format {
        TextureFormat::DXT1_CRUNCHED => header.format == 0,
        TextureFormat::DXT5_CRUNCHED => matches!(header.format, 2..=6),
        TextureFormat::ETC_RGB4_CRUNCHED => {
            codec == CrunchCodec::Unity && matches!(header.format, 10 | 13)
        }
        TextureFormat::ETC2_RGBA8_CRUNCHED => {
            codec == CrunchCodec::Unity && matches!(header.format, 12 | 14)
        }
        _ => false,
    };
    if !format_matches {
        return Err(Error::invalid_data(format!(
            "Texture2D {} does not match {} Crunch header format {}",
            format.name().unwrap_or("format"),
            match codec {
                CrunchCodec::Classic => "classic",
                CrunchCodec::Unity => "Unity",
            },
            header.format
        )));
    }
    Ok(())
}

fn crunch_decoder_working_byte_length(
    header: &CrunchHeader,
    codec: CrunchCodec,
    rgba_output_length: u64,
) -> Result<u64> {
    let block_columns = u64::from(header.width).div_ceil(4);
    let block_rows = u64::from(header.height).div_ceil(4);
    let block_count = checked_product(block_columns, block_rows, "Crunch block count")?;
    let block_bytes = if matches!(header.format, 0 | 10 | 11 | 13) {
        8
    } else {
        16
    };
    let unpacked_blocks = checked_product(block_count, block_bytes, "Crunch unpacked blocks")?;

    let color_endpoints = checked_product(
        u64::from(header.palettes[0].entries),
        4,
        "Crunch color endpoint palette",
    )?;
    let color_selector_size = if codec == CrunchCodec::Unity && matches!(header.format, 10..=12) {
        8
    } else {
        4
    };
    let color_selectors = checked_product(
        u64::from(header.palettes[1].entries),
        color_selector_size,
        "Crunch color selector palette",
    )?;
    let alpha_endpoints = checked_product(
        u64::from(header.palettes[2].entries),
        2,
        "Crunch alpha endpoint palette",
    )?;
    let alpha_selector_words = match (codec, header.format) {
        (CrunchCodec::Unity, 12) => u64::from(header.palettes[3].entries)
            .checked_mul(6)
            .and_then(|size| size.checked_add(1)),
        (CrunchCodec::Unity, 14) => u64::from(header.palettes[3].entries)
            .checked_mul(3)
            .and_then(|size| size.checked_add(1)),
        _ => u64::from(header.palettes[3].entries).checked_mul(3),
    }
    .ok_or_else(|| Error::invalid_data("Crunch alpha selector palette overflowed"))?;
    let alpha_selectors = checked_product(
        alpha_selector_words,
        2,
        "Crunch alpha selector palette bytes",
    )?;

    let block_buffer = if codec == CrunchCodec::Unity {
        let rounded_columns = block_columns.next_multiple_of(2);
        let records = if matches!(header.format, 10 | 12) {
            checked_product(rounded_columns, 2, "UnityCrunch ETC block-buffer records")?
        } else {
            rounded_columns
        };
        checked_product(records, 8, "UnityCrunch block-buffer bytes")?
    } else {
        0
    };
    [
        rgba_output_length,
        unpacked_blocks,
        color_endpoints,
        color_selectors,
        alpha_endpoints,
        alpha_selectors,
        block_buffer,
        CRUNCH_HUFFMAN_WORKING_BYTES,
    ]
    .into_iter()
    .try_fold(0_u64, |total, length| {
        total
            .checked_add(length)
            .ok_or_else(|| Error::invalid_data("Crunch decoder working byte size overflowed"))
    })
}

fn read_crunch_be_u16(input: &[u8], offset: usize, field: &str) -> Result<u16> {
    let bytes = input
        .get(offset..offset + 2)
        .ok_or_else(|| Error::invalid_data(format!("Crunch {field} is truncated")))?;
    Ok(u16::from_be_bytes(
        bytes.try_into().expect("bounded Crunch u16 has two bytes"),
    ))
}

fn read_crunch_be_u24(input: &[u8], offset: usize, field: &str) -> Result<u32> {
    let bytes = input
        .get(offset..offset + 3)
        .ok_or_else(|| Error::invalid_data(format!("Crunch {field} is truncated")))?;
    Ok((u32::from(bytes[0]) << 16) | (u32::from(bytes[1]) << 8) | u32::from(bytes[2]))
}

fn read_crunch_be_u32(input: &[u8], offset: usize, field: &str) -> Result<u32> {
    let bytes = input
        .get(offset..offset + 4)
        .ok_or_else(|| Error::invalid_data(format!("Crunch {field} is truncated")))?;
    Ok(u32::from_be_bytes(
        bytes.try_into().expect("bounded Crunch u32 has four bytes"),
    ))
}

fn crunch_crc16(input: &[u8]) -> u16 {
    let mut crc = !0_u16;
    for byte in input {
        let quotient = u16::from(*byte) ^ (crc >> 8);
        crc <<= 8;
        let mut remainder = (quotient >> 4) ^ quotient;
        crc ^= remainder;
        remainder <<= 5;
        crc ^= remainder;
        remainder <<= 7;
        crc ^= remainder;
    }
    !crc
}

fn switch_texture_layout(format: TextureFormat) -> Result<SwitchTextureLayout> {
    let layout = match format {
        TextureFormat::ALPHA8 | TextureFormat::R8 => SwitchTextureLayout {
            decode_format: format,
            texel_width: 16,
            texel_height: 1,
        },
        TextureFormat::ARGB4444
        | TextureFormat::RGB565
        | TextureFormat::R16
        | TextureFormat::RGBA4444
        | TextureFormat::RG16 => SwitchTextureLayout {
            decode_format: format,
            texel_width: 8,
            texel_height: 1,
        },
        // AssetStudio's managed Switch converter treats these legacy payloads
        // as four-channel data before calculating the GOB layout.
        TextureFormat::RGB24 => SwitchTextureLayout {
            decode_format: TextureFormat::RGBA32,
            texel_width: 4,
            texel_height: 1,
        },
        TextureFormat::BGR24 => SwitchTextureLayout {
            decode_format: TextureFormat::BGRA32,
            texel_width: 4,
            texel_height: 1,
        },
        TextureFormat::RGBA32 | TextureFormat::ARGB32 | TextureFormat::BGRA32 => {
            SwitchTextureLayout {
                decode_format: format,
                texel_width: 4,
                texel_height: 1,
            }
        }
        TextureFormat::DXT1 | TextureFormat::BC4 => SwitchTextureLayout {
            decode_format: format,
            texel_width: 8,
            texel_height: 4,
        },
        TextureFormat::DXT5 | TextureFormat::BC5 | TextureFormat::BC6H | TextureFormat::BC7 => {
            SwitchTextureLayout {
                decode_format: format,
                texel_width: 4,
                texel_height: 4,
            }
        }
        TextureFormat::ASTC_RGB_4X4 | TextureFormat::ASTC_RGBA_4X4 => SwitchTextureLayout {
            decode_format: format,
            texel_width: 4,
            texel_height: 4,
        },
        TextureFormat::ASTC_RGB_5X5 | TextureFormat::ASTC_RGBA_5X5 => SwitchTextureLayout {
            decode_format: format,
            texel_width: 5,
            texel_height: 5,
        },
        TextureFormat::ASTC_RGB_6X6 | TextureFormat::ASTC_RGBA_6X6 => SwitchTextureLayout {
            decode_format: format,
            texel_width: 6,
            texel_height: 6,
        },
        TextureFormat::ASTC_RGB_8X8 | TextureFormat::ASTC_RGBA_8X8 => SwitchTextureLayout {
            decode_format: format,
            texel_width: 8,
            texel_height: 8,
        },
        TextureFormat::ASTC_RGB_10X10 | TextureFormat::ASTC_RGBA_10X10 => SwitchTextureLayout {
            decode_format: format,
            texel_width: 10,
            texel_height: 10,
        },
        TextureFormat::ASTC_RGB_12X12 | TextureFormat::ASTC_RGBA_12X12 => SwitchTextureLayout {
            decode_format: format,
            texel_width: 12,
            texel_height: 12,
        },
        _ => {
            return Err(Error::unsupported(format!(
                "Nintendo Switch Texture2D format {}{} has no verified 16-byte GOB layout in the managed converter",
                format.0,
                format
                    .name()
                    .map_or_else(String::new, |name| format!(" ({name})"))
            )));
        }
    };
    Ok(layout)
}

fn switch_gobs_per_block(platform_blob: &Region) -> Result<u32> {
    if platform_blob.len() < 12 {
        return Err(Error::invalid_data(format!(
            "Nintendo Switch Texture2D platform blob is {} bytes; at least 12 bytes are required for block-height log2 at offset 8",
            platform_blob.len()
        )));
    }
    let mut encoded = [0_u8; 4];
    platform_blob.read_exact_at(8, &mut encoded)?;
    let encoded = i32::from_le_bytes(encoded);
    let block_height_log2 = u32::try_from(encoded).map_err(|_| {
        Error::invalid_data(format!(
            "Nintendo Switch Texture2D block-height log2 cannot be negative: {encoded}"
        ))
    })?;
    if block_height_log2 > MAXIMUM_SWITCH_BLOCK_HEIGHT_LOG2 {
        return Err(Error::invalid_data(format!(
            "Nintendo Switch Texture2D block-height log2 {block_height_log2} exceeds the validated Tegra range 0..={MAXIMUM_SWITCH_BLOCK_HEIGHT_LOG2}"
        )));
    }
    1_u32
        .checked_shl(block_height_log2)
        .ok_or_else(|| Error::invalid_data("Nintendo Switch GOB height overflowed"))
}

fn switch_padded_dimensions(
    width: u32,
    height: u32,
    texel_width: u32,
    texel_height: u32,
    gobs_per_block: u32,
) -> Result<(u32, u32)> {
    if texel_width == 0 || texel_height == 0 || gobs_per_block == 0 {
        return Err(Error::invalid_data(
            "Nintendo Switch Texture2D GOB dimensions must be positive",
        ));
    }
    let width_multiple = checked_product(
        u64::from(texel_width),
        SWITCH_GOB_WIDTH_IN_CHUNKS,
        "Nintendo Switch Texture2D padded width multiple",
    )?;
    let height_multiple = checked_product(
        checked_product(
            u64::from(texel_height),
            SWITCH_GOB_HEIGHT_IN_CHUNKS,
            "Nintendo Switch Texture2D GOB height",
        )?,
        u64::from(gobs_per_block),
        "Nintendo Switch Texture2D padded height multiple",
    )?;
    let padded_width = u64::from(width)
        .div_ceil(width_multiple)
        .checked_mul(width_multiple)
        .ok_or_else(|| Error::invalid_data("Nintendo Switch padded width overflowed"))?;
    let padded_height = u64::from(height)
        .div_ceil(height_multiple)
        .checked_mul(height_multiple)
        .ok_or_else(|| Error::invalid_data("Nintendo Switch padded height overflowed"))?;
    Ok((
        u32::try_from(padded_width)
            .map_err(|_| Error::invalid_data("Nintendo Switch padded width exceeds u32"))?,
        u32::try_from(padded_height)
            .map_err(|_| Error::invalid_data("Nintendo Switch padded height exceeds u32"))?,
    ))
}

fn rgba_byte_length(width: u32, height: u32, description: &str) -> Result<u64> {
    let pixels = checked_product(
        u64::from(width),
        u64::from(height),
        &format!("Nintendo Switch Texture2D {description} pixel count"),
    )?;
    checked_product(
        pixels,
        4,
        &format!("Nintendo Switch Texture2D {description} RGBA byte size"),
    )
}

fn validate_switch_input_length(
    actual_length: u64,
    encoded_length: u64,
    padded_width: u32,
    padded_height: u32,
    allows_trailing_mips: bool,
    limits: &TextureReadLimits,
) -> Result<()> {
    if actual_length < encoded_length {
        return Err(Error::invalid_data(format!(
            "Nintendo Switch Texture2D payload is {actual_length} bytes, shorter than the {encoded_length} bytes required for the padded {padded_width}x{padded_height} base mip"
        )));
    }
    if !allows_trailing_mips && actual_length != encoded_length {
        return Err(Error::invalid_data(format!(
            "Nintendo Switch Texture2D single-mip payload is {actual_length} bytes, expected exactly {encoded_length} for the padded {padded_width}x{padded_height} mip"
        )));
    }
    if encoded_length > limits.maximum_payload_bytes {
        return Err(Error::invalid_data(format!(
            "Nintendo Switch Texture2D padded input is {encoded_length} bytes, exceeding limit {}",
            limits.maximum_payload_bytes
        )));
    }
    Ok(())
}

fn validate_switch_working_length(
    format: TextureFormat,
    padded_width: u32,
    padded_height: u32,
    encoded_length: u64,
    padded_output_length: u64,
    limits: &TextureReadLimits,
) -> Result<()> {
    let decoder_scratch = if matches!(format.layout()?, TextureLayout::Block { .. })
        && !uses_internal_block_decoder(format)
    {
        external_decoder_working_byte_length(
            format,
            padded_width,
            padded_height,
            padded_output_length,
        )?
    } else {
        0
    };
    let unswizzle_peak = checked_product(
        encoded_length,
        2,
        "Nintendo Switch Texture2D swizzle working byte size",
    )?;
    let decode_peak = encoded_length
        .checked_add(padded_output_length)
        .and_then(|length| length.checked_add(decoder_scratch))
        .ok_or_else(|| {
            Error::invalid_data("Nintendo Switch Texture2D decode working byte size overflowed")
        })?;
    let working_length = unswizzle_peak.max(decode_peak);
    if working_length > limits.maximum_decoder_working_bytes {
        return Err(Error::invalid_data(format!(
            "Nintendo Switch Texture2D working buffers are {working_length} bytes, exceeding limit {}",
            limits.maximum_decoder_working_bytes
        )));
    }
    Ok(())
}

fn unswizzle_switch_texture(
    input: &[u8],
    output: &mut [u8],
    padded_width: u32,
    padded_height: u32,
    layout: SwitchTextureLayout,
    gobs_per_block: u32,
) -> Result<()> {
    if !padded_width.is_multiple_of(layout.texel_width)
        || !padded_height.is_multiple_of(layout.texel_height)
    {
        return Err(Error::invalid_data(
            "Nintendo Switch padded dimensions are not divisible by the 16-byte texel shape",
        ));
    }
    let chunk_columns = u64::from(padded_width / layout.texel_width);
    let chunk_rows = u64::from(padded_height / layout.texel_height);
    if !chunk_columns.is_multiple_of(SWITCH_GOB_WIDTH_IN_CHUNKS)
        || !chunk_rows.is_multiple_of(SWITCH_GOB_HEIGHT_IN_CHUNKS)
    {
        return Err(Error::invalid_data(
            "Nintendo Switch padded dimensions do not contain complete GOBs",
        ));
    }
    let gob_columns = chunk_columns / SWITCH_GOB_WIDTH_IN_CHUNKS;
    let gob_rows = chunk_rows / SWITCH_GOB_HEIGHT_IN_CHUNKS;
    let gobs_per_block = u64::from(gobs_per_block);
    if !gob_rows.is_multiple_of(gobs_per_block) {
        return Err(Error::invalid_data(
            "Nintendo Switch padded height does not contain complete GOB blocks",
        ));
    }
    let expected_length = checked_product(
        checked_product(
            chunk_columns,
            chunk_rows,
            "Nintendo Switch Texture2D GOB chunk count",
        )?,
        SWITCH_GOB_CHUNK_BYTES,
        "Nintendo Switch Texture2D GOB byte size",
    )?;
    let expected_length = usize::try_from(expected_length).map_err(|_| {
        Error::invalid_data("Nintendo Switch Texture2D GOB bytes exceed this platform")
    })?;
    if input.len() != expected_length || output.len() != expected_length {
        return Err(Error::invalid_data(format!(
            "Nintendo Switch Texture2D GOB buffers are {} and {} bytes, expected {expected_length}",
            input.len(),
            output.len()
        )));
    }
    unswizzle_switch_gob_rows(
        input,
        output,
        chunk_columns,
        gob_columns,
        gob_rows / gobs_per_block,
        gobs_per_block,
    )
}

fn unswizzle_switch_gob_rows(
    input: &[u8],
    output: &mut [u8],
    chunk_columns: u64,
    gob_columns: u64,
    gob_block_rows: u64,
    gobs_per_block: u64,
) -> Result<()> {
    let mut source_position = 0_u64;
    for block_row in 0..gob_block_rows {
        for gob_column in 0..gob_columns {
            for gob_in_block in 0..gobs_per_block {
                for chunk_in_gob in 0..SWITCH_GOB_X_POSITIONS.len() {
                    let destination_x = gob_column
                        .checked_mul(SWITCH_GOB_WIDTH_IN_CHUNKS)
                        .and_then(|value| value.checked_add(SWITCH_GOB_X_POSITIONS[chunk_in_gob]))
                        .ok_or_else(|| Error::invalid_data("Switch GOB x offset overflowed"))?;
                    let destination_y = block_row
                        .checked_mul(gobs_per_block)
                        .and_then(|value| value.checked_add(gob_in_block))
                        .and_then(|value| value.checked_mul(SWITCH_GOB_HEIGHT_IN_CHUNKS))
                        .and_then(|value| value.checked_add(SWITCH_GOB_Y_POSITIONS[chunk_in_gob]))
                        .ok_or_else(|| Error::invalid_data("Switch GOB y offset overflowed"))?;
                    let destination_position = destination_y
                        .checked_mul(chunk_columns)
                        .and_then(|value| value.checked_add(destination_x))
                        .and_then(|value| value.checked_mul(SWITCH_GOB_CHUNK_BYTES))
                        .ok_or_else(|| {
                            Error::invalid_data("Switch GOB destination offset overflowed")
                        })?;
                    let source_end = source_position
                        .checked_add(SWITCH_GOB_CHUNK_BYTES)
                        .ok_or_else(|| Error::invalid_data("Switch GOB source end overflowed"))?;
                    let destination_end = destination_position
                        .checked_add(SWITCH_GOB_CHUNK_BYTES)
                        .ok_or_else(|| {
                            Error::invalid_data("Switch GOB destination end overflowed")
                        })?;
                    let source_range = usize::try_from(source_position)
                        .map_err(|_| Error::invalid_data("Switch GOB source exceeds usize"))?
                        ..usize::try_from(source_end)
                            .map_err(|_| Error::invalid_data("Switch GOB source exceeds usize"))?;
                    let destination_range = usize::try_from(destination_position)
                        .map_err(|_| Error::invalid_data("Switch GOB output exceeds usize"))?
                        ..usize::try_from(destination_end)
                            .map_err(|_| Error::invalid_data("Switch GOB output exceeds usize"))?;
                    let source = input.get(source_range).ok_or_else(|| {
                        Error::invalid_data("Nintendo Switch Texture2D GOB source was truncated")
                    })?;
                    let destination = output.get_mut(destination_range).ok_or_else(|| {
                        Error::invalid_data(
                            "Nintendo Switch Texture2D GOB destination exceeded its buffer",
                        )
                    })?;
                    destination.copy_from_slice(source);
                    source_position = source_end;
                }
            }
        }
    }
    if usize::try_from(source_position).ok() != Some(input.len()) {
        return Err(Error::invalid_data(format!(
            "Nintendo Switch Texture2D consumed {source_position} GOB bytes, expected {}",
            input.len()
        )));
    }
    Ok(())
}

fn crop_switch_rgba_top_left(
    pixels: &mut Vec<u8>,
    padded_width: u32,
    width: u32,
    height: u32,
    output_length: u64,
) -> Result<()> {
    if width > padded_width {
        return Err(Error::invalid_data(
            "Nintendo Switch crop width exceeds the padded image",
        ));
    }
    let padded_stride = checked_product(
        u64::from(padded_width),
        4,
        "Nintendo Switch Texture2D padded row stride",
    )?;
    let output_stride = checked_product(
        u64::from(width),
        4,
        "Nintendo Switch Texture2D cropped row stride",
    )?;
    let padded_stride = usize::try_from(padded_stride)
        .map_err(|_| Error::invalid_data("Nintendo Switch padded stride exceeds usize"))?;
    let output_stride = usize::try_from(output_stride)
        .map_err(|_| Error::invalid_data("Nintendo Switch crop stride exceeds usize"))?;
    let height = usize::try_from(height)
        .map_err(|_| Error::invalid_data("Nintendo Switch crop height exceeds usize"))?;
    for row in 0..height {
        let source_start = row
            .checked_mul(padded_stride)
            .ok_or_else(|| Error::invalid_data("Nintendo Switch crop source overflowed"))?;
        let source_end = source_start
            .checked_add(output_stride)
            .ok_or_else(|| Error::invalid_data("Nintendo Switch crop source end overflowed"))?;
        let destination_start = row
            .checked_mul(output_stride)
            .ok_or_else(|| Error::invalid_data("Nintendo Switch crop output overflowed"))?;
        if source_end > pixels.len() {
            return Err(Error::invalid_data(
                "Nintendo Switch crop source exceeded its padded buffer",
            ));
        }
        pixels.copy_within(source_start..source_end, destination_start);
    }
    let output_length = usize::try_from(output_length)
        .map_err(|_| Error::invalid_data("Nintendo Switch crop output exceeds usize"))?;
    pixels.truncate(output_length);
    Ok(())
}

fn mip_byte_length(width: u32, height: u32, format: TextureFormat) -> Result<u64> {
    if format.pvrtc_block_width().is_some()
        && (!width.is_power_of_two() || !height.is_power_of_two())
    {
        return Err(Error::unsupported(format!(
            "PVRTC Texture2D dimensions {width}x{height} are unsupported; both sides must be powers of two"
        )));
    }
    match format.layout()? {
        TextureLayout::Linear { bytes_per_pixel } => {
            let pixels = checked_product(u64::from(width), u64::from(height), "mip pixel count")?;
            checked_product(pixels, bytes_per_pixel, "mip byte size")
        }
        TextureLayout::Block {
            block_width,
            block_height,
            bytes_per_block,
        } => {
            let mut block_columns = u64::from(width).div_ceil(block_width);
            let mut block_rows = u64::from(height).div_ceil(block_height);
            if format.pvrtc_block_width().is_some() {
                // PVRTC1 always stores at least a 2x2-block footprint: 16x8
                // texels at 2bpp or 8x8 texels at 4bpp, hence at least 32 bytes.
                block_columns = block_columns.max(2);
                block_rows = block_rows.max(2);
            }
            let block_count = checked_product(block_columns, block_rows, "mip block count")?;
            checked_product(block_count, bytes_per_block, "mip block byte size")
        }
    }
}

fn validate_pvrtc_decode_dimensions(format: TextureFormat, width: u32, height: u32) -> Result<()> {
    const MINIMUM_HEIGHT: u32 = 8;
    let Some(block_width) = format.pvrtc_block_width() else {
        return Ok(());
    };
    if !width.is_power_of_two() || !height.is_power_of_two() {
        return Err(Error::unsupported(format!(
            "PVRTC Texture2D dimensions {width}x{height} are unsupported; both sides must be powers of two"
        )));
    }
    let minimum_width = u32::try_from(block_width * 2).expect("PVRTC minimum width fits in u32");
    if width < minimum_width || height < MINIMUM_HEIGHT {
        let bits_per_pixel = if block_width == 8 { 2 } else { 4 };
        return Err(Error::unsupported(format!(
            "PVRTC {bits_per_pixel}bpp Texture2D dimensions {width}x{height} are unsupported; the validated decoder minimum is {minimum_width}x{MINIMUM_HEIGHT}"
        )));
    }
    Ok(())
}

fn external_decoder_working_byte_length(
    format: TextureFormat,
    width: u32,
    height: u32,
    rgba_output_length: u64,
) -> Result<u64> {
    let Some(block_width) = format.pvrtc_block_width() else {
        return Ok(rgba_output_length);
    };
    let block_columns = u64::from(width).div_ceil(block_width);
    let block_rows = u64::from(height).div_ceil(4);
    let block_count = checked_product(
        block_columns,
        block_rows,
        "PVRTC decoder scratch block count",
    )?;
    // texture2ddecoder 0.1.2 retains one 44-byte texel-info record per
    // compressed block. Charge 64 bytes per block so the public limit remains
    // conservative across allocator alignment and compatible dependency updates.
    let pvrtc_scratch = checked_product(block_count, 64, "PVRTC decoder scratch byte size")?;
    rgba_output_length
        .checked_add(pvrtc_scratch)
        .ok_or_else(|| Error::invalid_data("Texture2D decoder working byte size overflowed"))
}

fn full_mip_count(width: u32, height: u32) -> u32 {
    u32::BITS - width.max(height).leading_zeros()
}

fn decode_pixels(
    format: TextureFormat,
    input: &[u8],
    output: &mut [u8],
    width: u32,
    height: u32,
) -> Result<()> {
    if width == 0 || height == 0 {
        return Err(Error::invalid_data(
            "Texture2D decode dimensions must be positive",
        ));
    }
    validate_pvrtc_decode_dimensions(format, width, height)?;
    let output_pixels = checked_product(u64::from(width), u64::from(height), "pixel count")?;
    let expected_output = checked_product(output_pixels, 4, "RGBA output byte size")?;
    let expected_output = usize::try_from(expected_output)
        .map_err(|_| Error::invalid_data("Texture2D output is too large for this platform"))?;
    if output.len() != expected_output {
        return Err(Error::invalid_data(format!(
            "Texture2D RGBA output is {} bytes, expected {expected_output}",
            output.len()
        )));
    }

    let expected_input = mip_byte_length(width, height, format)?;
    let expected_input = usize::try_from(expected_input)
        .map_err(|_| Error::invalid_data("Texture2D input is too large for this platform"))?;
    if input.len() != expected_input {
        return Err(Error::invalid_data(format!(
            "Texture2D mip is {} bytes, expected {expected_input}",
            input.len()
        )));
    }

    if format == TextureFormat::YUY2 {
        return decode_yuy2(input, output, width, height);
    }

    match format.layout()? {
        TextureLayout::Linear { bytes_per_pixel } => {
            let bytes_per_pixel =
                usize::try_from(bytes_per_pixel).expect("supported bytes per pixel fits in usize");
            decode_linear_pixels(format, input, output, bytes_per_pixel);
            Ok(())
        }
        TextureLayout::Block {
            bytes_per_block, ..
        } if uses_internal_block_decoder(format) => decode_block_pixels(
            format,
            input,
            output,
            width,
            height,
            usize::try_from(bytes_per_block).expect("supported block size fits in usize"),
        ),
        TextureLayout::Block { .. } => {
            decode_external_compressed_pixels(format, input, output, width, height)
        }
    }
}

fn uses_internal_block_decoder(format: TextureFormat) -> bool {
    matches!(
        format,
        TextureFormat::DXT1
            | TextureFormat::DXT3
            | TextureFormat::DXT5
            | TextureFormat::BC4
            | TextureFormat::BC5
    )
}

/// Which EAC channels a block carries, and whether its base is signed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EacLayout {
    R,
    RSigned,
    Rg,
    RgSigned,
}

impl EacLayout {
    const fn block_bytes(self) -> usize {
        match self {
            Self::R | Self::RSigned => 8,
            Self::Rg | Self::RgSigned => 16,
        }
    }

    const fn is_signed(self) -> bool {
        matches!(self, Self::RSigned | Self::RgSigned)
    }
}

/// Decodes EAC single- and dual-channel textures.
///
/// Implemented here rather than through `texture2ddecoder` because that crate's
/// standalone EAC entry points read the block's 48-bit index field as a
/// little-endian integer, while the format packs it most significant bit first.
/// Its own ETC2 alpha decoder -- the same codec, reached through
/// `decode_etc2_rgba8` -- reads it big-endian and agrees with the managed
/// decoder; the standalone path does not, so every `EAC_R` and `EAC_RG` texture
/// came out with a scrambled index stream. The differential caught it by
/// feeding one block through both paths and finding the crate disagreeing with
/// itself.
///
/// The arithmetic follows the same crate's ETC2 alpha routine, which is the
/// half that was already verified against the managed decoder: eight-bit base
/// plus multiplier times the modifier table entry, clamped.
fn decode_eac(
    input: &[u8],
    width: usize,
    height: usize,
    output: &mut [u32],
    layout: EacLayout,
) -> std::result::Result<(), &'static str> {
    /// Maps a block's index position to its pixel slot, as the format's
    /// column-major order requires.
    const WRITE_ORDER: [usize; 16] = [15, 11, 7, 3, 14, 10, 6, 2, 13, 9, 5, 1, 12, 8, 4, 0];
    const MODIFIERS: [[i8; 8]; 16] = [
        [-3, -6, -9, -15, 2, 5, 8, 14],
        [-3, -7, -10, -13, 2, 6, 9, 12],
        [-2, -5, -8, -13, 1, 4, 7, 12],
        [-2, -4, -6, -13, 1, 3, 5, 12],
        [-3, -6, -8, -12, 2, 5, 7, 11],
        [-3, -7, -9, -11, 2, 6, 8, 10],
        [-4, -7, -8, -11, 3, 6, 7, 10],
        [-3, -5, -8, -11, 2, 4, 7, 10],
        [-2, -6, -8, -10, 1, 5, 7, 9],
        [-2, -5, -8, -10, 1, 4, 7, 9],
        [-2, -4, -8, -10, 1, 3, 7, 9],
        [-2, -5, -7, -10, 1, 4, 6, 9],
        [-3, -4, -7, -10, 2, 3, 6, 9],
        [-1, -2, -3, -10, 0, 1, 2, 9],
        [-4, -6, -8, -9, 3, 5, 7, 8],
        [-3, -5, -7, -9, 2, 4, 6, 8],
    ];

    let block_bytes = layout.block_bytes();
    let blocks_wide = width.div_ceil(4);
    let blocks_high = height.div_ceil(4);
    let required = blocks_wide
        .checked_mul(blocks_high)
        .and_then(|blocks| blocks.checked_mul(block_bytes))
        .ok_or("EAC block count overflowed")?;
    if input.len() < required {
        return Err("EAC payload is shorter than its block grid");
    }
    if output.len() < width.checked_mul(height).ok_or("EAC size overflowed")? {
        return Err("EAC output buffer is too small");
    }

    let mut block = [0_u32; 16];
    for block_y in 0..blocks_high {
        for block_x in 0..blocks_wide {
            let offset = (block_y * blocks_wide + block_x) * block_bytes;
            // Opaque black, so a channel the layout does not carry stays zero.
            block.fill(0xFF00_0000);
            decode_eac_channel(
                &input[offset..offset + 8],
                &mut block,
                16,
                layout.is_signed(),
                &WRITE_ORDER,
                &MODIFIERS,
            );
            if block_bytes == 16 {
                decode_eac_channel(
                    &input[offset + 8..offset + 16],
                    &mut block,
                    8,
                    layout.is_signed(),
                    &WRITE_ORDER,
                    &MODIFIERS,
                );
            }
            copy_block_into_image(&block, output, width, height, block_x, block_y);
        }
    }
    Ok(())
}

/// Decodes one 8-byte EAC channel block into `shift`-positioned bytes.
///
/// The arithmetic works in the format's eleven-bit space: the base codeword and
/// the modifier are scaled by eight, a rounding term is added, and the result is
/// shifted back down. That matters for the zero-multiplier case, where the
/// substituted multiplier of one is an eleven-bit step -- an eighth of a level
/// -- rather than a whole eight-bit one. Collapsing this to eight-bit
/// arithmetic looks equivalent for every other multiplier and silently widens
/// that one block's modifier range by eight.
fn decode_eac_channel(
    data: &[u8],
    block: &mut [u32; 16],
    shift: u32,
    signed: bool,
    write_order: &[usize; 16],
    modifiers: &[[i8; 8]; 16],
) {
    let (base, rounding) = if signed {
        // The signed base is a two's-complement codeword, and the larger
        // rounding term re-centres the eleven-bit range around zero.
        #[allow(clippy::cast_possible_wrap)]
        (i32::from(data[0] as i8), 1023)
    } else {
        (i32::from(data[0]), 4)
    };
    let multiplier = match i32::from((data[1] >> 1) & 0x78) {
        0 => 1,
        value => value,
    };
    let table = modifiers[usize::from(data[1] & 0xf)];
    // Most significant bit first: the index stream is a big-endian bit field,
    // and reading it little-endian is exactly the upstream defect.
    let mut indices = u64::from_be_bytes(data[..8].try_into().expect("an eight byte EAC block"));

    for position in write_order {
        let value = base * 8 + multiplier * i32::from(table[(indices & 7) as usize]) + rounding;
        #[allow(clippy::cast_sign_loss)]
        let value = if value < 0 {
            0
        } else if value >= 2048 {
            255
        } else {
            (value >> 3) as u32
        };
        block[*position] = (block[*position] & !(0xFF << shift)) | (value << shift);
        indices >>= 3;
    }
}

/// Copies a decoded 4x4 block into the image, cropping at the edges.
fn copy_block_into_image(
    block: &[u32; 16],
    output: &mut [u32],
    width: usize,
    height: usize,
    block_x: usize,
    block_y: usize,
) {
    for row in 0..4 {
        let y = block_y * 4 + row;
        if y >= height {
            break;
        }
        for column in 0..4 {
            let x = block_x * 4 + column;
            if x >= width {
                break;
            }
            output[y * width + x] = block[row * 4 + column];
        }
    }
}

fn decode_external_compressed_pixels(
    format: TextureFormat,
    input: &[u8],
    output: &mut [u8],
    width: u32,
    height: u32,
) -> Result<()> {
    let width = usize::try_from(width)
        .map_err(|_| Error::invalid_data("Texture2D width is too large for this platform"))?;
    let height = usize::try_from(height)
        .map_err(|_| Error::invalid_data("Texture2D height is too large for this platform"))?;
    let pixel_count = width
        .checked_mul(height)
        .ok_or_else(|| Error::invalid_data("Texture2D decoder pixel count overflowed"))?;
    let mut decoded = Vec::new();
    decoded.try_reserve_exact(pixel_count).map_err(|error| {
        Error::invalid_data(format!(
            "cannot allocate compressed Texture2D decoder buffer: {error}"
        ))
    })?;
    decoded.resize(pixel_count, 0_u32);

    let decode_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match format {
        TextureFormat::BC6H => {
            texture2ddecoder::decode_bc6_unsigned(input, width, height, &mut decoded)
        }
        TextureFormat::BC7 => texture2ddecoder::decode_bc7(input, width, height, &mut decoded),
        TextureFormat::PVRTC_RGB2 | TextureFormat::PVRTC_RGBA2 => {
            texture2ddecoder::decode_pvrtc_2bpp(input, width, height, &mut decoded)
        }
        TextureFormat::PVRTC_RGB4 | TextureFormat::PVRTC_RGBA4 => {
            texture2ddecoder::decode_pvrtc_4bpp(input, width, height, &mut decoded)
        }
        TextureFormat::ETC_RGB4 | TextureFormat::ETC_RGB4_3DS => {
            texture2ddecoder::decode_etc1(input, width, height, &mut decoded)
        }
        TextureFormat::ATC_RGB4 => {
            texture2ddecoder::decode_atc_rgb4(input, width, height, &mut decoded)
        }
        TextureFormat::ATC_RGBA8 => {
            texture2ddecoder::decode_atc_rgba8(input, width, height, &mut decoded)
        }
        TextureFormat::EAC_R => decode_eac(input, width, height, &mut decoded, EacLayout::R),
        TextureFormat::EAC_R_SIGNED => {
            decode_eac(input, width, height, &mut decoded, EacLayout::RSigned)
        }
        TextureFormat::EAC_RG => decode_eac(input, width, height, &mut decoded, EacLayout::Rg),
        TextureFormat::EAC_RG_SIGNED => {
            decode_eac(input, width, height, &mut decoded, EacLayout::RgSigned)
        }
        TextureFormat::ETC2_RGB => {
            texture2ddecoder::decode_etc2_rgb(input, width, height, &mut decoded)
        }
        TextureFormat::ETC2_RGBA1 => {
            texture2ddecoder::decode_etc2_rgba1(input, width, height, &mut decoded)
        }
        TextureFormat::ETC2_RGBA8 | TextureFormat::ETC_RGBA8_3DS => {
            texture2ddecoder::decode_etc2_rgba8(input, width, height, &mut decoded)
        }
        TextureFormat::ASTC_RGB_4X4
        | TextureFormat::ASTC_RGBA_4X4
        | TextureFormat::ASTC_HDR_4X4 => {
            texture2ddecoder::decode_astc(input, width, height, 4, 4, &mut decoded)
        }
        TextureFormat::ASTC_RGB_5X5
        | TextureFormat::ASTC_RGBA_5X5
        | TextureFormat::ASTC_HDR_5X5 => {
            texture2ddecoder::decode_astc(input, width, height, 5, 5, &mut decoded)
        }
        TextureFormat::ASTC_RGB_6X6
        | TextureFormat::ASTC_RGBA_6X6
        | TextureFormat::ASTC_HDR_6X6 => {
            texture2ddecoder::decode_astc(input, width, height, 6, 6, &mut decoded)
        }
        TextureFormat::ASTC_RGB_8X8
        | TextureFormat::ASTC_RGBA_8X8
        | TextureFormat::ASTC_HDR_8X8 => {
            texture2ddecoder::decode_astc(input, width, height, 8, 8, &mut decoded)
        }
        TextureFormat::ASTC_RGB_10X10
        | TextureFormat::ASTC_RGBA_10X10
        | TextureFormat::ASTC_HDR_10X10 => {
            texture2ddecoder::decode_astc(input, width, height, 10, 10, &mut decoded)
        }
        TextureFormat::ASTC_RGB_12X12
        | TextureFormat::ASTC_RGBA_12X12
        | TextureFormat::ASTC_HDR_12X12 => {
            texture2ddecoder::decode_astc(input, width, height, 12, 12, &mut decoded)
        }
        _ => Err("unsupported external texture decoder format"),
    }))
    .map_err(|_| {
        Error::invalid_data(format!(
            "Texture2D {} decoder rejected a malformed compressed block",
            format.name().unwrap_or("compressed format")
        ))
    })?;
    decode_result.map_err(|message| {
        Error::invalid_data(format!(
            "Texture2D {} decode failed: {message}",
            format.name().unwrap_or("compressed format")
        ))
    })?;

    for (source, destination) in decoded.iter().zip(output.chunks_exact_mut(4)) {
        let [blue, green, red, alpha] = source.to_le_bytes();
        destination.copy_from_slice(&[red, green, blue, alpha]);
    }
    Ok(())
}

fn decode_crunched_pixels(
    format: TextureFormat,
    codec: CrunchCodec,
    input: &[u8],
    output: &mut [u8],
    width: u32,
    height: u32,
) -> Result<()> {
    let width = usize::try_from(width)
        .map_err(|_| Error::invalid_data("Crunch width is too large for this platform"))?;
    let height = usize::try_from(height)
        .map_err(|_| Error::invalid_data("Crunch height is too large for this platform"))?;
    let pixel_count = width
        .checked_mul(height)
        .ok_or_else(|| Error::invalid_data("Crunch decoder pixel count overflowed"))?;
    let expected_output = pixel_count
        .checked_mul(4)
        .ok_or_else(|| Error::invalid_data("Crunch RGBA output byte size overflowed"))?;
    if output.len() != expected_output {
        return Err(Error::invalid_data(format!(
            "Crunch RGBA output is {} bytes, expected {expected_output}",
            output.len()
        )));
    }

    let mut decoded = Vec::new();
    decoded.try_reserve_exact(pixel_count).map_err(|error| {
        Error::invalid_data(format!("cannot allocate Crunch decoder buffer: {error}"))
    })?;
    decoded.resize(pixel_count, 0_u32);
    let decode_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match codec {
        CrunchCodec::Classic => texture2ddecoder::decode_crunch(input, width, height, &mut decoded),
        CrunchCodec::Unity => {
            texture2ddecoder::decode_unity_crunch(input, width, height, &mut decoded)
        }
    }))
    .map_err(|_| {
        Error::invalid_data(format!(
            "Texture2D {} decoder panicked on malformed Crunch data",
            format.name().unwrap_or("crunched format")
        ))
    })?;
    decode_result.map_err(|message| {
        Error::invalid_data(format!(
            "Texture2D {} decode failed: {message}",
            format.name().unwrap_or("crunched format")
        ))
    })?;

    for (source, destination) in decoded.iter().zip(output.chunks_exact_mut(4)) {
        let [blue, green, red, alpha] = source.to_le_bytes();
        destination.copy_from_slice(&[red, green, blue, alpha]);
    }
    Ok(())
}

fn decode_yuy2(input: &[u8], output: &mut [u8], width: u32, height: u32) -> Result<()> {
    if !width.is_multiple_of(2) {
        return Err(Error::unsupported(format!(
            "YUY2 Texture2D width {width} is odd; chroma pairs require an even width"
        )));
    }
    let width = usize::try_from(width)
        .map_err(|_| Error::invalid_data("YUY2 width is too large for this platform"))?;
    let height = usize::try_from(height)
        .map_err(|_| Error::invalid_data("YUY2 height is too large for this platform"))?;
    let source_stride = width
        .checked_mul(2)
        .ok_or_else(|| Error::invalid_data("YUY2 source stride overflowed"))?;
    let destination_stride = width
        .checked_mul(4)
        .ok_or_else(|| Error::invalid_data("YUY2 output stride overflowed"))?;
    for row in 0..height {
        let source_start = row
            .checked_mul(source_stride)
            .ok_or_else(|| Error::invalid_data("YUY2 source row offset overflowed"))?;
        let destination_start = row
            .checked_mul(destination_stride)
            .ok_or_else(|| Error::invalid_data("YUY2 output row offset overflowed"))?;
        let source_row = &input[source_start..source_start + source_stride];
        let destination_row =
            &mut output[destination_start..destination_start + destination_stride];
        for (source, destination) in source_row
            .chunks_exact(4)
            .zip(destination_row.chunks_exact_mut(8))
        {
            let u = i32::from(source[1]) - 128;
            let v = i32::from(source[3]) - 128;
            destination[..4].copy_from_slice(&decode_yuv_pixel(source[0], u, v));
            destination[4..].copy_from_slice(&decode_yuv_pixel(source[2], u, v));
        }
    }
    Ok(())
}

fn decode_yuv_pixel(y: u8, u: i32, v: i32) -> Rgba8 {
    let luminance = i32::from(y) - 16;
    let red = (298 * luminance + 409 * v + 128) >> 8;
    let green = (298 * luminance - 100 * u - 208 * v + 128) >> 8;
    let blue = (298 * luminance + 516 * u + 128) >> 8;
    [clamp_u8(red), clamp_u8(green), clamp_u8(blue), 255]
}

fn clamp_u8(value: i32) -> u8 {
    u8::try_from(value.clamp(0, 255)).expect("clamped color channel fits in u8")
}

fn decode_linear_pixels(
    format: TextureFormat,
    input: &[u8],
    output: &mut [u8],
    bytes_per_pixel: usize,
) {
    for (source, destination) in input
        .chunks_exact(bytes_per_pixel)
        .zip(output.chunks_exact_mut(4))
    {
        match format {
            TextureFormat::ALPHA8 => destination.copy_from_slice(&[255, 255, 255, source[0]]),
            TextureFormat::RGB24 => {
                destination.copy_from_slice(&[source[0], source[1], source[2], 255]);
            }
            TextureFormat::BGR24 => {
                destination.copy_from_slice(&[source[2], source[1], source[0], 255]);
            }
            TextureFormat::RGBA32 => destination.copy_from_slice(source),
            TextureFormat::ARGB32 => {
                destination.copy_from_slice(&[source[1], source[2], source[3], source[0]]);
            }
            TextureFormat::ARGB4444 => {
                let packed = u16::from_le_bytes([source[0], source[1]]);
                destination.copy_from_slice(&[
                    expand_nibble(packed >> 8),
                    expand_nibble(packed >> 4),
                    expand_nibble(packed),
                    expand_nibble(packed >> 12),
                ]);
            }
            TextureFormat::RGB565 => {
                let [red, green, blue] = rgb565(u16::from_le_bytes([source[0], source[1]]));
                destination.copy_from_slice(&[red, green, blue, 255]);
            }
            TextureFormat::R16 => {
                destination.copy_from_slice(&[downscale_u16(source[0], source[1]), 0, 0, 255]);
            }
            TextureFormat::RGBA4444 => {
                let packed = u16::from_le_bytes([source[0], source[1]]);
                destination.copy_from_slice(&[
                    expand_nibble(packed >> 12),
                    expand_nibble(packed >> 8),
                    expand_nibble(packed >> 4),
                    expand_nibble(packed),
                ]);
            }
            TextureFormat::BGRA32 => {
                destination.copy_from_slice(&[source[2], source[1], source[0], source[3]]);
            }
            TextureFormat::R_HALF
            | TextureFormat::RG_HALF
            | TextureFormat::RGBA_HALF
            | TextureFormat::R_FLOAT
            | TextureFormat::RG_FLOAT
            | TextureFormat::RGBA_FLOAT
            | TextureFormat::RGB9E5_FLOAT => {
                decode_floating_point_pixel(format, source, destination);
            }
            TextureFormat::YUY2 => unreachable!("YUY2 is decoded in shared chroma pairs"),
            TextureFormat::R8 => destination.copy_from_slice(&[source[0], 0, 0, 255]),
            TextureFormat::RG16 => destination.copy_from_slice(&[source[0], source[1], 0, 255]),
            TextureFormat::RG32 => destination.copy_from_slice(&[
                downscale_u16(source[0], source[1]),
                downscale_u16(source[2], source[3]),
                0,
                255,
            ]),
            TextureFormat::RGB48 => destination.copy_from_slice(&[
                downscale_u16(source[0], source[1]),
                downscale_u16(source[2], source[3]),
                downscale_u16(source[4], source[5]),
                255,
            ]),
            TextureFormat::RGBA64 => {
                destination.copy_from_slice(&[
                    downscale_u16(source[0], source[1]),
                    downscale_u16(source[2], source[3]),
                    downscale_u16(source[4], source[5]),
                    downscale_u16(source[6], source[7]),
                ]);
            }
            _ => unreachable!("unsupported formats are rejected before decoding"),
        }
    }
}

fn decode_floating_point_pixel(format: TextureFormat, source: &[u8], destination: &mut [u8]) {
    let rgba = match format {
        TextureFormat::R_HALF => [
            normalized_float_to_u8(half_to_f32(source[0], source[1])),
            0,
            0,
            255,
        ],
        TextureFormat::RG_HALF => [
            normalized_float_to_u8(half_to_f32(source[0], source[1])),
            normalized_float_to_u8(half_to_f32(source[2], source[3])),
            0,
            255,
        ],
        TextureFormat::RGBA_HALF => [
            normalized_float_to_u8(half_to_f32(source[0], source[1])),
            normalized_float_to_u8(half_to_f32(source[2], source[3])),
            normalized_float_to_u8(half_to_f32(source[4], source[5])),
            normalized_float_to_u8(half_to_f32(source[6], source[7])),
        ],
        TextureFormat::R_FLOAT => [normalized_float_to_u8(read_f32(source, 0)), 0, 0, 255],
        TextureFormat::RG_FLOAT => [
            normalized_float_to_u8(read_f32(source, 0)),
            normalized_float_to_u8(read_f32(source, 4)),
            0,
            255,
        ],
        TextureFormat::RGBA_FLOAT => [
            normalized_float_to_u8(read_f32(source, 0)),
            normalized_float_to_u8(read_f32(source, 4)),
            normalized_float_to_u8(read_f32(source, 8)),
            normalized_float_to_u8(read_f32(source, 12)),
        ],
        TextureFormat::RGB9E5_FLOAT => decode_rgb9e5(source),
        _ => unreachable!("only floating-point formats use this decoder"),
    };
    destination.copy_from_slice(&rgba);
}

fn decode_rgb9e5(source: &[u8]) -> Rgba8 {
    let packed = u32::from_le_bytes(source.try_into().expect("RGB9e5 pixel is 4 bytes"));
    let scale = 2.0_f32
        .powi(i32::try_from((packed >> 27) & 0x1f).expect("five-bit exponent fits in i32") - 24);
    [
        normalized_float_to_u8(packed_9_bit_channel(packed, 0) * scale),
        normalized_float_to_u8(packed_9_bit_channel(packed, 9) * scale),
        normalized_float_to_u8(packed_9_bit_channel(packed, 18) * scale),
        255,
    ]
}

type Rgba8 = [u8; 4];

fn decode_block_pixels(
    format: TextureFormat,
    input: &[u8],
    output: &mut [u8],
    width: u32,
    height: u32,
    bytes_per_block: usize,
) -> Result<()> {
    let width = usize::try_from(width)
        .map_err(|_| Error::invalid_data("Texture2D width is too large for this platform"))?;
    let height = usize::try_from(height)
        .map_err(|_| Error::invalid_data("Texture2D height is too large for this platform"))?;
    let block_columns = width.div_ceil(4);

    for (block_index, block) in input.chunks_exact(bytes_per_block).enumerate() {
        let block_x = block_index % block_columns;
        let block_y = block_index / block_columns;
        let decoded = decode_compressed_block(format, block)?;
        copy_cropped_block(&decoded, output, width, height, block_x, block_y)?;
    }
    Ok(())
}

fn copy_cropped_block(
    block: &[Rgba8; 16],
    output: &mut [u8],
    width: usize,
    height: usize,
    block_x: usize,
    block_y: usize,
) -> Result<()> {
    let origin_x = block_x
        .checked_mul(4)
        .ok_or_else(|| Error::invalid_data("Texture2D block x offset overflowed"))?;
    let origin_y = block_y
        .checked_mul(4)
        .ok_or_else(|| Error::invalid_data("Texture2D block y offset overflowed"))?;

    for local_y in 0..4 {
        let y = origin_y
            .checked_add(local_y)
            .ok_or_else(|| Error::invalid_data("Texture2D pixel y offset overflowed"))?;
        if y >= height {
            break;
        }
        for local_x in 0..4 {
            let x = origin_x
                .checked_add(local_x)
                .ok_or_else(|| Error::invalid_data("Texture2D pixel x offset overflowed"))?;
            if x >= width {
                break;
            }
            let source_index = local_y * 4 + local_x;
            let pixel_index = y
                .checked_mul(width)
                .and_then(|row| row.checked_add(x))
                .ok_or_else(|| Error::invalid_data("Texture2D pixel offset overflowed"))?;
            let destination_start = pixel_index
                .checked_mul(4)
                .ok_or_else(|| Error::invalid_data("Texture2D byte offset overflowed"))?;
            let destination_end = destination_start
                .checked_add(4)
                .ok_or_else(|| Error::invalid_data("Texture2D pixel end overflowed"))?;
            let destination = output
                .get_mut(destination_start..destination_end)
                .ok_or_else(|| {
                    Error::invalid_data("Texture2D block write exceeds the RGBA output")
                })?;
            destination.copy_from_slice(&block[source_index]);
        }
    }
    Ok(())
}

fn decode_compressed_block(format: TextureFormat, block: &[u8]) -> Result<[Rgba8; 16]> {
    match format {
        TextureFormat::DXT1 => {
            let block: &[u8; 8] = block
                .try_into()
                .map_err(|_| Error::invalid_data("DXT1 block must contain exactly 8 bytes"))?;
            Ok(decode_bc1_color_block(*block, true))
        }
        TextureFormat::DXT5 => {
            let block: &[u8; 16] = block
                .try_into()
                .map_err(|_| Error::invalid_data("DXT5 block must contain exactly 16 bytes"))?;
            let color_block: &[u8; 8] = block[8..]
                .try_into()
                .expect("DXT5 color block is exactly 8 bytes");
            let mut pixels = decode_bc1_color_block(*color_block, false);
            let alpha_block: &[u8; 8] = block[..8]
                .try_into()
                .expect("DXT5 alpha block is exactly 8 bytes");
            let alpha = decode_bc_alpha_block(*alpha_block);
            for (pixel, alpha) in pixels.iter_mut().zip(alpha) {
                pixel[3] = alpha;
            }
            Ok(pixels)
        }
        TextureFormat::DXT3 => {
            let block: &[u8; 16] = block
                .try_into()
                .map_err(|_| Error::invalid_data("DXT3 block must contain exactly 16 bytes"))?;
            let color_block: &[u8; 8] = block[8..]
                .try_into()
                .expect("DXT3 color block is exactly 8 bytes");
            let mut pixels = decode_bc1_color_block(*color_block, false);
            let alpha = u64::from_le_bytes(
                block[..8]
                    .try_into()
                    .expect("DXT3 alpha block is exactly 8 bytes"),
            );
            for (pixel_index, pixel) in pixels.iter_mut().enumerate() {
                let shift = pixel_index * 4;
                pixel[3] = expand_nibble(
                    u16::try_from((alpha >> shift) & 0xf).expect("DXT3 alpha fits in u16"),
                );
            }
            Ok(pixels)
        }
        TextureFormat::BC4 => {
            let block: &[u8; 8] = block
                .try_into()
                .map_err(|_| Error::invalid_data("BC4 block must contain exactly 8 bytes"))?;
            let red = decode_bc_alpha_block(*block);
            Ok(std::array::from_fn(|index| [red[index], 0, 0, 255]))
        }
        TextureFormat::BC5 => {
            let block: &[u8; 16] = block
                .try_into()
                .map_err(|_| Error::invalid_data("BC5 block must contain exactly 16 bytes"))?;
            let red_block: &[u8; 8] = block[..8]
                .try_into()
                .expect("BC5 red block is exactly 8 bytes");
            let green_block: &[u8; 8] = block[8..]
                .try_into()
                .expect("BC5 green block is exactly 8 bytes");
            let red = decode_bc_alpha_block(*red_block);
            let green = decode_bc_alpha_block(*green_block);
            Ok(std::array::from_fn(|index| {
                [red[index], green[index], 0, 255]
            }))
        }
        _ => Err(Error::unsupported(format!(
            "Texture2D format {} is not block-compressed",
            format.0
        ))),
    }
}

fn decode_bc1_color_block(block: [u8; 8], one_bit_alpha: bool) -> [Rgba8; 16] {
    let endpoint_0 = u16::from_le_bytes([block[0], block[1]]);
    let endpoint_1 = u16::from_le_bytes([block[2], block[3]]);
    let color_0 = rgb565(endpoint_0);
    let color_1 = rgb565(endpoint_1);
    let mut palette = [
        [color_0[0], color_0[1], color_0[2], 255],
        [color_1[0], color_1[1], color_1[2], 255],
        [0; 4],
        [0; 4],
    ];

    if !one_bit_alpha || endpoint_0 > endpoint_1 {
        palette[2] = interpolate_rgba(palette[0], 2, palette[1], 1, 3);
        palette[3] = interpolate_rgba(palette[0], 1, palette[1], 2, 3);
    } else {
        // Punch-through mode. EXT_texture_compression_s3tc specifies index 3 as
        // fully transparent black here, which is what a cut-out texture's alpha
        // was authored to mean. This is a deliberate divergence from the managed
        // oracle: AssetStudio's native decoder emits opaque black instead,
        // reproducing NV4x-era hardware behaviour, so a masked region comes out
        // as a black block there rather than transparent. The compatibility
        // matrix records the divergence.
        palette[2] = interpolate_rgba(palette[0], 1, palette[1], 1, 2);
        palette[3] = [0, 0, 0, 0];
    }

    let selectors = u32::from_le_bytes([block[4], block[5], block[6], block[7]]);
    std::array::from_fn(|pixel_index| {
        let shift = pixel_index * 2;
        let palette_index =
            usize::try_from((selectors >> shift) & 0x3).expect("BC1 palette index fits in usize");
        palette[palette_index]
    })
}

fn decode_bc_alpha_block(block: [u8; 8]) -> [u8; 16] {
    let endpoint_0 = block[0];
    let endpoint_1 = block[1];
    let mut palette = [endpoint_0, endpoint_1, 0, 0, 0, 0, 0, 0];
    if endpoint_0 > endpoint_1 {
        for (offset, slot) in palette[2..].iter_mut().enumerate() {
            let endpoint_0_weight =
                u16::try_from(6 - offset).expect("BC alpha interpolation weight fits in u16");
            let endpoint_1_weight =
                u16::try_from(offset + 1).expect("BC alpha interpolation weight fits in u16");
            *slot = interpolate_u8(
                endpoint_0,
                endpoint_0_weight,
                endpoint_1,
                endpoint_1_weight,
                7,
            );
        }
    } else {
        for (offset, slot) in palette[2..6].iter_mut().enumerate() {
            let endpoint_0_weight =
                u16::try_from(4 - offset).expect("BC alpha interpolation weight fits in u16");
            let endpoint_1_weight =
                u16::try_from(offset + 1).expect("BC alpha interpolation weight fits in u16");
            *slot = interpolate_u8(
                endpoint_0,
                endpoint_0_weight,
                endpoint_1,
                endpoint_1_weight,
                5,
            );
        }
        palette[6] = 0;
        palette[7] = 255;
    }

    let selector_bits = u64::from_le_bytes(block) >> 16;
    std::array::from_fn(|pixel_index| {
        let shift = pixel_index * 3;
        let palette_index = usize::try_from((selector_bits >> shift) & 0x7)
            .expect("BC alpha palette index fits in usize");
        palette[palette_index]
    })
}

fn rgb565(value: u16) -> [u8; 3] {
    let red = u8::try_from((value >> 11) & 0x1f).expect("RGB565 red fits in u8");
    let green = u8::try_from((value >> 5) & 0x3f).expect("RGB565 green fits in u8");
    let blue = u8::try_from(value & 0x1f).expect("RGB565 blue fits in u8");
    [
        (red << 3) | (red >> 2),
        (green << 2) | (green >> 4),
        (blue << 3) | (blue >> 2),
    ]
}

fn expand_nibble(value: u16) -> u8 {
    let value = u8::try_from(value & 0x0f).expect("four-bit component fits in u8");
    (value << 4) | value
}

fn half_to_f32(low: u8, high: u8) -> f32 {
    let bits = u16::from_le_bytes([low, high]);
    let sign = u32::from(bits & 0x8000) << 16;
    let exponent = (bits >> 10) & 0x1f;
    let fraction = bits & 0x03ff;
    match exponent {
        0 if fraction == 0 => f32::from_bits(sign),
        0 => {
            let magnitude = f32::from(fraction) * 2.0_f32.powi(-24);
            if sign == 0 { magnitude } else { -magnitude }
        }
        0x1f => f32::from_bits(sign | 0x7f80_0000 | (u32::from(fraction) << 13)),
        _ => {
            let adjusted = u32::from(exponent) + 112;
            f32::from_bits(sign | (adjusted << 23) | (u32::from(fraction) << 13))
        }
    }
}

fn read_f32(source: &[u8], offset: usize) -> f32 {
    f32::from_le_bytes(
        source[offset..offset + 4]
            .try_into()
            .expect("float channel is exactly 4 bytes"),
    )
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn normalized_float_to_u8(value: f32) -> u8 {
    // Rust's float-to-integer conversion intentionally saturates finite values
    // and maps NaN to zero after the managed-compatible ties-to-even rounding.
    (value * 255.0).round_ties_even() as u8
}

fn packed_9_bit_channel(value: u32, shift: u32) -> f32 {
    let component = u16::try_from((value >> shift) & 0x1ff)
        .expect("nine-bit shared-exponent channel fits in u16");
    f32::from(component)
}

fn swap_adjacent_bytes(bytes: &mut [u8]) -> Result<()> {
    if !bytes.len().is_multiple_of(2) {
        return Err(Error::invalid_data(
            "Xbox 360 Texture2D payload has an odd byte length",
        ));
    }
    for pair in bytes.chunks_exact_mut(2) {
        pair.swap(0, 1);
    }
    Ok(())
}

fn interpolate_rgba(
    left: Rgba8,
    left_weight: u16,
    right: Rgba8,
    right_weight: u16,
    divisor: u16,
) -> Rgba8 {
    std::array::from_fn(|channel| {
        interpolate_u8(
            left[channel],
            left_weight,
            right[channel],
            right_weight,
            divisor,
        )
    })
}

fn interpolate_u8(left: u8, left_weight: u16, right: u8, right_weight: u16, divisor: u16) -> u8 {
    let numerator = u16::from(left) * left_weight + u16::from(right) * right_weight;
    u8::try_from(numerator / divisor).expect("interpolated byte fits in u8")
}

fn downscale_u16(low: u8, high: u8) -> u8 {
    let component = u32::from(u16::from_le_bytes([low, high]));
    u8::try_from(((component * 255) + 32_895) >> 16).expect("16-bit downscale fits in u8")
}

#[cfg(test)]
mod tests {
    use super::{EacLayout, decode_eac};
    use crate::loader::{AssetCollection, LoadedResource, LoadedSerializedFile};
    use crate::serialized::SerializedFile;
    use crate::source::Region;

    use super::{
        CrunchCodec, NO_TARGET_PLATFORM, RgbaImage, SWITCH_GOB_CHUNK_BYTES, SwitchTextureLayout,
        TEXTURE_2D_CLASS_ID, Texture2D, TextureFormat, TextureReadLimits,
        crunch_decoder_working_byte_length, decode_pixels, mip_byte_length, parse_crunch_header,
        read_texture2d, switch_texture_layout, texture_uses_unity_crunch, unswizzle_switch_texture,
        write_rgba_ir, write_rgba_ir_display_order,
    };

    const CLASSIC_DXT1_CRN: &[u8] = include_bytes!("../tests/fixtures/crunch/classic_dxt1.crn");
    const CLASSIC_DXT5_CRN: &[u8] = include_bytes!("../tests/fixtures/crunch/classic_dxt5.crn");
    const UNITY_DXT1_CRN: &[u8] = include_bytes!("../tests/fixtures/crunch/unity_dxt1.crn");
    const UNITY_DXT5_CRN: &[u8] = include_bytes!("../tests/fixtures/crunch/unity_dxt5.crn");
    const UNITY_ETC1_CRN: &[u8] = include_bytes!("../tests/fixtures/crunch/unity_etc1.crn");
    const UNITY_ETC2A_CRN: &[u8] = include_bytes!("../tests/fixtures/crunch/unity_etc2a.crn");

    #[test]
    fn crunch_codec_gate_matches_the_managed_converter() {
        assert!(!texture_uses_unity_crunch(
            TextureFormat::DXT1_CRUNCHED,
            (2017, 2, 99)
        ));
        assert!(texture_uses_unity_crunch(
            TextureFormat::DXT1_CRUNCHED,
            (2017, 3, 0)
        ));
        assert!(!texture_uses_unity_crunch(
            TextureFormat::DXT5_CRUNCHED,
            (5, 6, 9)
        ));
        assert!(texture_uses_unity_crunch(
            TextureFormat::ETC_RGB4_CRUNCHED,
            (5, 0, 0)
        ));
        assert!(texture_uses_unity_crunch(
            TextureFormat::ETC2_RGBA8_CRUNCHED,
            (2017, 2, 0)
        ));
    }

    #[test]
    fn decodes_classic_and_unity_crunch_like_the_native_cpp_oracle() {
        let cases = [
            (
                TextureFormat::DXT1_CRUNCHED,
                false,
                CLASSIC_DXT1_CRN,
                0x141e_cbae_e8fd_eb6b,
            ),
            (
                TextureFormat::DXT5_CRUNCHED,
                false,
                CLASSIC_DXT5_CRN,
                0xf6bb_d719_6ee7_b293,
            ),
            (
                TextureFormat::DXT1_CRUNCHED,
                true,
                UNITY_DXT1_CRN,
                0x2325_b896_a579_1851,
            ),
            (
                TextureFormat::DXT5_CRUNCHED,
                true,
                UNITY_DXT5_CRN,
                0xb69e_1abe_cd80_0da1,
            ),
            (
                TextureFormat::ETC_RGB4_CRUNCHED,
                true,
                UNITY_ETC1_CRN,
                0x794f_5132_6238_5ed4,
            ),
            (
                TextureFormat::ETC2_RGBA8_CRUNCHED,
                true,
                UNITY_ETC2A_CRN,
                0xb00c_f611_3233_e6db,
            ),
        ];
        for (format, uses_unity_crunch, payload, expected_hash) in cases {
            let texture = crunched_texture(format, uses_unity_crunch, payload);
            let image = texture
                .decode_mip_rgba8(0, TextureReadLimits::default())
                .unwrap();
            assert_eq!((image.width, image.height), (512, 512));
            assert_eq!(fnv1a64(&image.pixels), expected_hash);
        }
    }

    #[test]
    fn crunch_validates_headers_formats_and_all_memory_budgets_before_decode() {
        let texture = crunched_texture(TextureFormat::DXT1_CRUNCHED, false, CLASSIC_DXT1_CRN);
        let header = parse_crunch_header(CLASSIC_DXT1_CRN).unwrap();
        let output_length = 512_u64 * 512 * 4;
        let working_length =
            crunch_decoder_working_byte_length(&header, CrunchCodec::Classic, output_length)
                .unwrap();

        for limits in [
            TextureReadLimits {
                maximum_payload_bytes: u64::try_from(CLASSIC_DXT1_CRN.len()).unwrap() - 1,
                ..TextureReadLimits::default()
            },
            TextureReadLimits {
                maximum_output_bytes: output_length - 1,
                ..TextureReadLimits::default()
            },
            TextureReadLimits {
                maximum_decoder_working_bytes: working_length - 1,
                ..TextureReadLimits::default()
            },
        ] {
            assert!(texture.decode_mip_rgba8(0, limits).is_err());
        }

        assert!(
            texture
                .decode_mip_rgba8(1, TextureReadLimits::default())
                .is_err()
        );
        let wrong_format = crunched_texture(TextureFormat::DXT5_CRUNCHED, false, CLASSIC_DXT1_CRN);
        assert!(
            wrong_format
                .decode_mip_rgba8(0, TextureReadLimits::default())
                .is_err()
        );
        let wrong_codec = crunched_texture(TextureFormat::ETC_RGB4_CRUNCHED, false, UNITY_ETC1_CRN);
        assert!(
            wrong_codec
                .decode_mip_rgba8(0, TextureReadLimits::default())
                .is_err()
        );

        let mut corrupt = CLASSIC_DXT1_CRN.to_vec();
        *corrupt.last_mut().unwrap() ^= 1;
        assert!(parse_crunch_header(&corrupt).is_err());
        let mut bad_offset = CLASSIC_DXT1_CRN.to_vec();
        bad_offset[70..74].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(parse_crunch_header(&bad_offset).is_err());
    }

    #[test]
    fn switch_unswizzle_matches_the_managed_one_gob_chunk_order() {
        let mut swizzled = vec![0_u8; 32 * 16];
        for (chunk_index, chunk) in swizzled.chunks_exact_mut(16).enumerate() {
            chunk.fill(u8::try_from(chunk_index).unwrap());
        }
        let mut linear = vec![0_u8; swizzled.len()];

        unswizzle_switch_texture(
            &swizzled,
            &mut linear,
            16,
            8,
            SwitchTextureLayout {
                decode_format: TextureFormat::RGBA32,
                texel_width: 4,
                texel_height: 1,
            },
            1,
        )
        .unwrap();

        let expected_chunk_order = [
            0, 2, 16, 18, 1, 3, 17, 19, 4, 6, 20, 22, 5, 7, 21, 23, 8, 10, 24, 26, 9, 11, 25, 27,
            12, 14, 28, 30, 13, 15, 29, 31,
        ];
        assert_eq!(linear.len(), expected_chunk_order.len() * 16);
        for (chunk, expected) in linear.chunks_exact(16).zip(expected_chunk_order) {
            assert!(chunk.iter().all(|value| *value == expected));
        }
    }

    #[test]
    fn switch_rgba32_decodes_two_gobs_then_crops_the_top_left() {
        let width = 5;
        let height = 9;
        let padded_width = 16;
        let padded_height = 16;
        let layout = switch_texture_layout(TextureFormat::RGBA32).unwrap();
        let mut padded_rgba = Vec::with_capacity(padded_width * padded_height * 4);
        for y in 0..padded_height {
            for x in 0..padded_width {
                padded_rgba.extend_from_slice(&[
                    u8::try_from(x).unwrap(),
                    u8::try_from(y).unwrap(),
                    u8::try_from(x ^ y).unwrap(),
                    255,
                ]);
            }
        }
        let swizzled = managed_switch_swizzle_oracle(
            &padded_rgba,
            u32::try_from(padded_width).unwrap(),
            u32::try_from(padded_height).unwrap(),
            layout,
            2,
        );
        let texture = switch_texture(
            u32::try_from(width).unwrap(),
            u32::try_from(height).unwrap(),
            TextureFormat::RGBA32,
            1,
            &swizzled,
        );

        let image = texture
            .decode_mip_rgba8(0, TextureReadLimits::default())
            .unwrap();

        let mut expected = Vec::new();
        for y in 0..height {
            for x in 0..width {
                expected.extend_from_slice(&[
                    u8::try_from(x).unwrap(),
                    u8::try_from(y).unwrap(),
                    u8::try_from(x ^ y).unwrap(),
                    255,
                ]);
            }
        }
        assert_eq!((image.width, image.height), (5, 9));
        assert_eq!(image.pixels, expected);
    }

    #[test]
    fn switch_mip_chain_decodes_only_the_bounded_base_prefix() {
        let layout = switch_texture_layout(TextureFormat::RGBA32).unwrap();
        let mut padded_rgba = vec![0_u8; 16 * 8 * 4];
        padded_rgba[..4].copy_from_slice(&[9, 8, 7, 6]);
        let base = managed_switch_swizzle_oracle(&padded_rgba, 16, 8, layout, 1);
        let base_length = u64::try_from(base.len()).unwrap();
        let mut chain = base;
        chain.extend_from_slice(&[0xa5; 128]);

        let mut texture = switch_texture(1, 1, TextureFormat::RGBA32, 0, &chain);
        texture.mip_count = 2;
        let image = texture
            .decode_mip_rgba8(
                0,
                TextureReadLimits {
                    maximum_payload_bytes: base_length,
                    ..TextureReadLimits::default()
                },
            )
            .unwrap();
        assert_eq!(image.pixels, [9, 8, 7, 6]);
        assert!(matches!(
            texture.decode_mip_rgba8(1, TextureReadLimits::default()),
            Err(crate::Error::Unsupported(message)) if message.contains("only mip zero")
        ));

        texture.mip_count = 1;
        assert!(matches!(
            texture.decode_mip_rgba8(0, TextureReadLimits::default()),
            Err(crate::Error::InvalidData(message)) if message.contains("single-mip payload")
        ));
    }

    #[test]
    fn switch_swizzle_is_blob_gated_and_rejects_unverified_metadata() {
        let linear = Texture2D {
            path_id: 1,
            name: "linear-switch".to_owned(),
            width: 1,
            height: 1,
            complete_image_size: 4,
            mips_stripped: 0,
            mip_count: 1,
            format: TextureFormat::RGBA32,
            image_count: 1,
            dimension: 2,
            platform_blob_size: 0,
            platform_blob: None,
            target_platform: 38,
            uses_unity_crunch: false,
            data: Region::from_bytes([1, 2, 3, 4]),
        };
        assert_eq!(
            linear
                .decode_mip_rgba8(0, TextureReadLimits::default())
                .unwrap()
                .pixels,
            [1, 2, 3, 4]
        );

        let source = vec![0_u8; 16 * 8 * 4];
        let mut texture = switch_texture(1, 1, TextureFormat::RGBA32, 0, &source);
        texture.platform_blob = Some(Region::from_bytes(vec![0_u8; 11]));
        texture.platform_blob_size = 11;
        assert!(matches!(
            texture.decode_mip_rgba8(0, TextureReadLimits::default()),
            Err(crate::Error::InvalidData(message)) if message.contains("at least 12")
        ));

        texture.platform_blob = Some(switch_platform_blob(-1));
        texture.platform_blob_size = 12;
        assert!(matches!(
            texture.decode_mip_rgba8(0, TextureReadLimits::default()),
            Err(crate::Error::InvalidData(message)) if message.contains("cannot be negative")
        ));

        texture.platform_blob = Some(switch_platform_blob(6));
        assert!(matches!(
            texture.decode_mip_rgba8(0, TextureReadLimits::default()),
            Err(crate::Error::InvalidData(message)) if message.contains("validated Tegra range")
        ));

        texture.platform_blob = Some(switch_platform_blob(0));
        texture.mip_count = 2;
        assert_eq!(
            texture
                .decode_mip_rgba8(0, TextureReadLimits::default())
                .unwrap()
                .pixels,
            [0, 0, 0, 0]
        );
        texture.mip_count = 1;
        texture.mips_stripped = 1;
        assert!(matches!(
            texture.decode_mip_rgba8(0, TextureReadLimits::default()),
            Err(crate::Error::Unsupported(message)) if message.contains("stripped mip")
        ));
        texture.mips_stripped = 0;
        texture.format = TextureFormat::DXT3;
        assert!(matches!(
            texture.decode_mip_rgba8(0, TextureReadLimits::default()),
            Err(crate::Error::Unsupported(message)) if message.contains("no verified 16-byte GOB layout")
        ));
    }

    #[test]
    fn switch_swizzle_enforces_payload_output_and_working_limits_before_materializing() {
        let source = vec![0_u8; 16 * 8 * 4];
        let texture = switch_texture(1, 1, TextureFormat::RGBA32, 0, &source);

        for (limits, expected) in [
            (
                TextureReadLimits {
                    maximum_payload_bytes: 511,
                    ..TextureReadLimits::default()
                },
                "padded input",
            ),
            (
                TextureReadLimits {
                    maximum_output_bytes: 3,
                    ..TextureReadLimits::default()
                },
                "RGBA output",
            ),
            (
                TextureReadLimits {
                    maximum_decoder_working_bytes: 1_023,
                    ..TextureReadLimits::default()
                },
                "working buffers",
            ),
        ] {
            assert!(matches!(
                texture.decode_mip_rgba8(0, limits),
                Err(crate::Error::InvalidData(message)) if message.contains(expected)
            ));
        }

        let truncated = switch_texture(1, 1, TextureFormat::RGBA32, 0, &source[..511]);
        assert!(matches!(
            truncated.decode_mip_rgba8(0, TextureReadLimits::default()),
            Err(crate::Error::InvalidData(message)) if message.contains("512 bytes required")
        ));
    }

    #[test]
    fn switch_layout_matches_every_managed_format_that_the_rust_decoder_supports() {
        assert_switch_layouts(&[
            (TextureFormat::ALPHA8, TextureFormat::ALPHA8, 16, 1),
            (TextureFormat::ARGB4444, TextureFormat::ARGB4444, 8, 1),
            (TextureFormat::RGB24, TextureFormat::RGBA32, 4, 1),
            (TextureFormat::BGR24, TextureFormat::BGRA32, 4, 1),
            (TextureFormat::RGBA32, TextureFormat::RGBA32, 4, 1),
            (TextureFormat::ARGB32, TextureFormat::ARGB32, 4, 1),
            (TextureFormat::RGB565, TextureFormat::RGB565, 8, 1),
            (TextureFormat::R16, TextureFormat::R16, 8, 1),
            (TextureFormat::DXT1, TextureFormat::DXT1, 8, 4),
            (TextureFormat::DXT5, TextureFormat::DXT5, 4, 4),
            (TextureFormat::RGBA4444, TextureFormat::RGBA4444, 8, 1),
            (TextureFormat::BGRA32, TextureFormat::BGRA32, 4, 1),
            (TextureFormat::BC6H, TextureFormat::BC6H, 4, 4),
            (TextureFormat::BC7, TextureFormat::BC7, 4, 4),
            (TextureFormat::BC4, TextureFormat::BC4, 8, 4),
            (TextureFormat::BC5, TextureFormat::BC5, 4, 4),
        ]);
        assert_switch_layouts(&[
            (
                TextureFormat::ASTC_RGB_4X4,
                TextureFormat::ASTC_RGB_4X4,
                4,
                4,
            ),
            (
                TextureFormat::ASTC_RGB_5X5,
                TextureFormat::ASTC_RGB_5X5,
                5,
                5,
            ),
            (
                TextureFormat::ASTC_RGB_6X6,
                TextureFormat::ASTC_RGB_6X6,
                6,
                6,
            ),
            (
                TextureFormat::ASTC_RGB_8X8,
                TextureFormat::ASTC_RGB_8X8,
                8,
                8,
            ),
            (
                TextureFormat::ASTC_RGB_10X10,
                TextureFormat::ASTC_RGB_10X10,
                10,
                10,
            ),
            (
                TextureFormat::ASTC_RGB_12X12,
                TextureFormat::ASTC_RGB_12X12,
                12,
                12,
            ),
            (
                TextureFormat::ASTC_RGBA_4X4,
                TextureFormat::ASTC_RGBA_4X4,
                4,
                4,
            ),
            (
                TextureFormat::ASTC_RGBA_5X5,
                TextureFormat::ASTC_RGBA_5X5,
                5,
                5,
            ),
            (
                TextureFormat::ASTC_RGBA_6X6,
                TextureFormat::ASTC_RGBA_6X6,
                6,
                6,
            ),
            (
                TextureFormat::ASTC_RGBA_8X8,
                TextureFormat::ASTC_RGBA_8X8,
                8,
                8,
            ),
            (
                TextureFormat::ASTC_RGBA_10X10,
                TextureFormat::ASTC_RGBA_10X10,
                10,
                10,
            ),
            (
                TextureFormat::ASTC_RGBA_12X12,
                TextureFormat::ASTC_RGBA_12X12,
                12,
                12,
            ),
            (TextureFormat::RG16, TextureFormat::RG16, 8, 1),
            (TextureFormat::R8, TextureFormat::R8, 16, 1),
        ]);
    }

    fn assert_switch_layouts(cases: &[(TextureFormat, TextureFormat, u32, u32)]) {
        for &(format, decode_format, texel_width, texel_height) in cases {
            assert_eq!(
                switch_texture_layout(format).unwrap(),
                SwitchTextureLayout {
                    decode_format,
                    texel_width,
                    texel_height,
                }
            );
        }
    }

    #[test]
    fn parses_the_platform_blob_as_a_bounded_source_region() {
        let mut blob = (0_u8..20).collect::<Vec<_>>();
        blob[8..12].copy_from_slice(&1_i32.to_le_bytes());
        let object = texture_object_with_platform_blob(
            1,
            1,
            TextureFormat::RGBA32,
            1,
            &[1, 2, 3, 4],
            None,
            &blob,
        );
        let file = parse_asset(&object);
        let collection = collection_with(file.clone(), "unused", b"");

        let texture = read_texture2d(&collection, &file, 0, TextureReadLimits::default()).unwrap();

        assert_eq!(texture.platform_blob_size, 20);
        assert_eq!(
            texture
                .platform_blob
                .as_ref()
                .unwrap()
                .read_to_vec(20)
                .unwrap(),
            blob
        );

        let limits = TextureReadLimits {
            maximum_platform_blob_bytes: 19,
            ..TextureReadLimits::default()
        };
        assert!(read_texture2d(&collection, &file, 0, limits).is_err());
    }

    #[test]
    fn parses_inline_texture_and_decodes_all_supported_formats() {
        let cases: &[(TextureFormat, &[u8], &[u8])] = &[
            (TextureFormat::ALPHA8, &[128], &[255, 255, 255, 128]),
            (
                TextureFormat::ARGB4444,
                &0xa123_u16.to_le_bytes(),
                &[17, 34, 51, 170],
            ),
            (TextureFormat::RGB24, &[1, 2, 3], &[1, 2, 3, 255]),
            (TextureFormat::BGR24, &[3, 2, 1], &[1, 2, 3, 255]),
            (TextureFormat::RGBA32, &[1, 2, 3, 4], &[1, 2, 3, 4]),
            (TextureFormat::ARGB32, &[4, 1, 2, 3], &[1, 2, 3, 4]),
            (TextureFormat::BGRA32, &[3, 2, 1, 4], &[1, 2, 3, 4]),
            (
                TextureFormat::RGB565,
                &0xfc08_u16.to_le_bytes(),
                &[255, 130, 66, 255],
            ),
            (TextureFormat::R16, &[0x80, 0x80], &[128, 0, 0, 255]),
            (
                TextureFormat::RGBA4444,
                &0x123a_u16.to_le_bytes(),
                &[17, 34, 51, 170],
            ),
            (TextureFormat::R_HALF, &[0x00, 0x38], &[128, 0, 0, 255]),
            (
                TextureFormat::RG_HALF,
                &[0x00, 0x3c, 0x00, 0x38],
                &[255, 128, 0, 255],
            ),
            (
                TextureFormat::RGBA_HALF,
                &[0x00, 0x00, 0x00, 0x38, 0x00, 0x3c, 0x00, 0x34],
                &[0, 128, 255, 64],
            ),
            (
                TextureFormat::R_FLOAT,
                &[0x00, 0x00, 0x00, 0x3f],
                &[128, 0, 0, 255],
            ),
            (
                TextureFormat::RG_FLOAT,
                &[0x00, 0x00, 0x80, 0x3f, 0x00, 0x00, 0x00, 0x3f],
                &[255, 128, 0, 255],
            ),
            (
                TextureFormat::RGBA_FLOAT,
                &[
                    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x3f, 0x00, 0x00, 0x80, 0x3f, 0x00,
                    0x00, 0x80, 0x3e,
                ],
                &[0, 128, 255, 64],
            ),
            (
                TextureFormat::RGB9E5_FLOAT,
                &[0x00, 0x01, 0x01, 0x80],
                &[255, 128, 0, 255],
            ),
            (TextureFormat::R8, &[7], &[7, 0, 0, 255]),
            (TextureFormat::RG16, &[7, 8], &[7, 8, 0, 255]),
            (
                TextureFormat::RG32,
                &[0xff, 0xff, 0x80, 0x80],
                &[255, 128, 0, 255],
            ),
            (
                TextureFormat::RGB48,
                &[0xff, 0xff, 0x80, 0x80, 0x00, 0x00],
                &[255, 128, 0, 255],
            ),
            (
                TextureFormat::RGBA64,
                &[255, 255, 0, 0, 128, 128, 1, 1],
                &[255, 0, 128, 1],
            ),
        ];

        for (format, source, expected) in cases {
            let object = texture_object(1, 1, *format, 1, source, None);
            let file = parse_asset(&object);
            let collection = collection_with(file.clone(), "unused.resS", b"");
            let texture =
                read_texture2d(&collection, &file, 0, TextureReadLimits::default()).unwrap();
            let image = texture
                .decode_mip_rgba8(0, TextureReadLimits::default())
                .unwrap();

            assert_eq!(texture.name, "image");
            assert_eq!(image.pixels, *expected, "format {}", format.0);
        }
    }

    #[test]
    fn swaps_xbox_360_word_order_before_decoding() {
        let object = texture_object(1, 1, TextureFormat::ARGB4444, 1, &[0xa1, 0x23], None);
        let file = parse_asset(&object);
        let collection = collection_with(file.clone(), "unused", b"");
        let mut texture =
            read_texture2d(&collection, &file, 0, TextureReadLimits::default()).unwrap();
        texture.target_platform = 11;

        assert_eq!(
            texture
                .decode_mip_rgba8(0, TextureReadLimits::default())
                .unwrap()
                .pixels,
            [17, 34, 51, 170]
        );
    }

    #[test]
    fn decodes_yuy2_chroma_pairs_and_rejects_odd_widths() {
        let object = texture_object(2, 1, TextureFormat::YUY2, 1, &[16, 128, 235, 128], None);
        let file = parse_asset(&object);
        let collection = collection_with(file.clone(), "unused", b"");
        let texture = read_texture2d(&collection, &file, 0, TextureReadLimits::default()).unwrap();
        assert_eq!(
            texture
                .decode_mip_rgba8(0, TextureReadLimits::default())
                .unwrap()
                .pixels,
            [0, 0, 0, 255, 255, 255, 255, 255]
        );

        let odd = texture_object(1, 1, TextureFormat::YUY2, 1, &[16, 128], None);
        let file = parse_asset(&odd);
        let collection = collection_with(file.clone(), "unused", b"");
        let texture = read_texture2d(&collection, &file, 0, TextureReadLimits::default()).unwrap();
        assert!(matches!(
            texture.decode_mip_rgba8(0, TextureReadLimits::default()),
            Err(crate::Error::Unsupported(message)) if message.contains("odd")
        ));
    }

    #[test]
    fn decodes_dxt1_four_color_and_one_bit_alpha_palettes() {
        let mut source = Vec::new();
        source.extend_from_slice(&bc1_block(0xf800, 0x07e0, 0xe4e4_e4e4));
        source.extend_from_slice(&bc1_block(0x0000, 0xffff, 0xe4e4_e4e4));
        let object = texture_object(8, 4, TextureFormat::DXT1, 1, &source, None);
        let file = parse_asset(&object);
        let collection = collection_with(file.clone(), "unused", b"");

        let texture = read_texture2d(&collection, &file, 0, TextureReadLimits::default()).unwrap();
        let image = texture
            .decode_mip_rgba8(0, TextureReadLimits::default())
            .unwrap();

        // The second block has endpoint_0 < endpoint_1, so it decodes in
        // punch-through mode and its index-3 texel is transparent black per the
        // s3tc specification. AssetStudio's native decoder would give opaque
        // black there; the divergence is deliberate and documented.
        let expected_row = [
            255, 0, 0, 255, 0, 255, 0, 255, 170, 85, 0, 255, 85, 170, 0, 255, 0, 0, 0, 255, 255,
            255, 255, 255, 127, 127, 127, 255, 0, 0, 0, 0,
        ];
        assert_eq!(image.pixels, expected_row.repeat(4));
    }

    #[test]
    fn decodes_dxt5_alpha_modes_and_uses_opaque_color_interpolation() {
        let selectors = [0, 1, 2, 3, 4, 5, 6, 7, 0, 1, 2, 3, 4, 5, 6, 7];
        let mut source = Vec::new();
        source.extend_from_slice(&dxt5_block(
            bc_alpha_block(255, 0, selectors),
            0xf800,
            0x07e0,
            0,
        ));
        source.extend_from_slice(&dxt5_block(
            bc_alpha_block(10, 20, selectors),
            0x0000,
            0xffff,
            u32::MAX,
        ));
        let object = texture_object(8, 4, TextureFormat::DXT5, 1, &source, None);
        let file = parse_asset(&object);
        let collection = collection_with(file.clone(), "unused", b"");

        let texture = read_texture2d(&collection, &file, 0, TextureReadLimits::default()).unwrap();
        let image = texture
            .decode_mip_rgba8(0, TextureReadLimits::default())
            .unwrap();

        let eight_alpha = [
            255, 0, 218, 182, 145, 109, 72, 36, 255, 0, 218, 182, 145, 109, 72, 36,
        ];
        let six_alpha = [
            10, 20, 12, 14, 16, 18, 0, 255, 10, 20, 12, 14, 16, 18, 0, 255,
        ];
        for y in 0..4 {
            for x in 0..8 {
                let pixel_offset = (y * 8 + x) * 4;
                let pixel = &image.pixels[pixel_offset..pixel_offset + 4];
                if x < 4 {
                    assert_eq!(pixel, &[255, 0, 0, eight_alpha[y * 4 + x]]);
                } else {
                    assert_eq!(pixel, &[170, 170, 170, six_alpha[y * 4 + x - 4]]);
                }
            }
        }
    }

    #[test]
    fn decodes_dxt3_explicit_alpha_nibbles() {
        let mut source = 0xfedc_ba98_7654_3210_u64.to_le_bytes().to_vec();
        source.extend_from_slice(&bc1_block(0xf800, 0x0000, 0));
        let object = texture_object(4, 4, TextureFormat::DXT3, 1, &source, None);
        let file = parse_asset(&object);
        let collection = collection_with(file.clone(), "unused", b"");

        let texture = read_texture2d(&collection, &file, 0, TextureReadLimits::default()).unwrap();
        let image = texture
            .decode_mip_rgba8(0, TextureReadLimits::default())
            .unwrap();

        for (index, pixel) in image.pixels.chunks_exact(4).enumerate() {
            assert_eq!(pixel[..3], [255, 0, 0]);
            assert_eq!(pixel[3], u8::try_from(index).unwrap() * 17);
        }
    }

    #[test]
    fn decodes_etc1_through_the_bounded_pure_rust_adapter() {
        let source = [0xf0, 0x00, 0x00, 0x00, 0, 0, 0, 0];
        let object = texture_object(4, 4, TextureFormat::ETC_RGB4, 1, &source, None);
        let file = parse_asset(&object);
        let collection = collection_with(file.clone(), "unused", b"");
        let texture = read_texture2d(&collection, &file, 0, TextureReadLimits::default()).unwrap();
        let image = texture
            .decode_mip_rgba8(0, TextureReadLimits::default())
            .unwrap();

        assert_eq!(image.pixels.len(), 64);
        assert_eq!(
            image
                .pixels
                .chunks_exact(4)
                .filter(|pixel| *pixel == [255, 2, 2, 255])
                .count(),
            8
        );
        assert_eq!(
            image
                .pixels
                .chunks_exact(4)
                .filter(|pixel| *pixel == [2, 2, 2, 255])
                .count(),
            8
        );
    }

    #[test]
    fn eac_reads_its_index_stream_most_significant_bit_first() {
        // Values verified against the managed decoder. The upstream crate reads
        // this 48-bit field as a little-endian integer, which selects a
        // different modifier per pixel and scrambles the whole block; its own
        // ETC2 alpha decoder reads it big-endian and agrees with these.
        let block = [0x80_u8, 0x10, 0x01, 0x23, 0x45, 0x67, 0x89, 0xAB];
        let mut decoded = vec![0_u32; 16];
        decode_eac(&block, 4, 4, &mut decoded, EacLayout::R).unwrap();

        let red: Vec<u8> = decoded
            .iter()
            .map(|pixel| ((pixel >> 16) & 0xFF) as u8)
            .collect();
        assert_eq!(
            red,
            [
                0x7D, 0x7A, 0x71, 0x82, 0x7D, 0x85, 0x7A, 0x88, 0x77, 0x7D, 0x8E, 0x85, 0x77, 0x85,
                0x7D, 0x71
            ]
        );
        // Single channel: green and blue stay clear, alpha opaque.
        assert!(decoded.iter().all(|pixel| pixel.trailing_zeros() >= 16));
        assert!(decoded.iter().all(|pixel| pixel >> 24 == 0xFF));
    }

    #[test]
    fn a_zero_eac_multiplier_steps_in_eleven_bit_units() {
        // The substituted multiplier of one is an eleven-bit step, an eighth of
        // a level, not a whole eight-bit one. Doing this in eight-bit
        // arithmetic looks identical for every other multiplier and widens this
        // block's range eightfold, which is what the differential caught.
        let mut block = [0_u8; 8];
        block[0] = 185; // base
        block[1] = 0x01; // multiplier nibble 0, modifier table 1
        block[2..].copy_from_slice(&[0x11, 0x22, 0x33, 0x44, 0x55, 0x66]);
        let mut decoded = vec![0_u32; 16];
        decode_eac(&block, 4, 4, &mut decoded, EacLayout::R).unwrap();

        let red: Vec<u8> = decoded
            .iter()
            .map(|pixel| ((pixel >> 16) & 0xFF) as u8)
            .collect();
        let low = *red.iter().min().unwrap();
        let high = *red.iter().max().unwrap();
        assert!(
            (183..=187).contains(&low) && (183..=187).contains(&high),
            "a zero multiplier should stay within a few levels of the base, got {low}..={high}"
        );
    }

    #[test]
    fn eac_rejects_a_payload_shorter_than_its_block_grid() {
        let mut decoded = vec![0_u32; 64];
        assert!(decode_eac(&[0_u8; 8], 8, 8, &mut decoded, EacLayout::R).is_err());
        // Two channels need sixteen bytes per block, not eight.
        assert!(decode_eac(&[0_u8; 8], 4, 4, &mut decoded, EacLayout::Rg).is_err());
    }

    #[test]
    fn decodes_astc_void_extent_blocks_for_every_unity_block_size() {
        let formats = [
            TextureFormat::ASTC_RGB_4X4,
            TextureFormat::ASTC_RGBA_5X5,
            TextureFormat::ASTC_HDR_6X6,
            TextureFormat::ASTC_RGB_8X8,
            TextureFormat::ASTC_RGBA_10X10,
            TextureFormat::ASTC_HDR_12X12,
        ];
        let mut block = [0_u8; 16];
        block[0] = 0xfc;
        block[1] = 1;
        block[9] = 11;
        block[11] = 22;
        block[13] = 33;
        block[15] = 44;

        for format in formats {
            let object = texture_object(3, 2, format, 1, &block, None);
            let file = parse_asset(&object);
            let collection = collection_with(file.clone(), "unused", b"");
            let texture =
                read_texture2d(&collection, &file, 0, TextureReadLimits::default()).unwrap();
            assert_eq!(
                texture
                    .decode_mip_rgba8(0, TextureReadLimits::default())
                    .unwrap()
                    .pixels,
                [11, 22, 33, 44].repeat(6),
                "format {}",
                format.0
            );
        }
    }

    #[test]
    fn decodes_pvrtc_2bpp_and_4bpp_like_the_native_cpp_oracle() {
        // Deterministic compressed words were decoded with
        // Texture2DDecoderNative/pvrtc.cpp. The hashes cover every RGBA byte;
        // the prefixes additionally pin the native BGRA-u32 to RGBA contract.
        let source: Vec<u8> = (0_u8..32)
            .map(|index| index.wrapping_mul(73).wrapping_add(19) ^ index.wrapping_mul(11))
            .collect();
        let cases = [
            (
                [TextureFormat::PVRTC_RGB4, TextureFormat::PVRTC_RGBA4],
                8,
                8,
                0xa95e_fecb_f7b9_90d5,
                [
                    0xde, 0xc8, 0x8c, 0xd4, 0x7f, 0xc9, 0xca, 0xcc, 0xb7, 0xcd, 0xab, 0xcb, 0x7f,
                    0xc9, 0xca, 0xcc, 0xde, 0xc8, 0x8c, 0xd4, 0xa0, 0x95, 0xca, 0xdd, 0xb7, 0xa8,
                    0xab, 0xdc, 0xa0, 0x95, 0xca, 0xdd,
                ],
            ),
            (
                [TextureFormat::PVRTC_RGB2, TextureFormat::PVRTC_RGBA2],
                16,
                8,
                0x6a7d_7937_cb3c_cb66,
                [
                    0xde, 0xc8, 0x8c, 0xd4, 0xe7, 0xc3, 0x8c, 0xd4, 0x7f, 0xc9, 0xca, 0xcc, 0xd7,
                    0xc2, 0x9b, 0xd0, 0xa5, 0xd2, 0xb2, 0xc9, 0x97, 0xce, 0xba, 0xca, 0x7f, 0xc9,
                    0xca, 0xcc, 0xab, 0xbe, 0xb2, 0xd1,
                ],
            ),
        ];

        for (formats, width, height, expected_hash, expected_prefix) in cases {
            for format in formats {
                let object = texture_object(width, height, format, 1, &source, None);
                let file = parse_asset(&object);
                let collection = collection_with(file.clone(), "unused", b"");
                let texture =
                    read_texture2d(&collection, &file, 0, TextureReadLimits::default()).unwrap();
                let image = texture
                    .decode_mip_rgba8(0, TextureReadLimits::default())
                    .unwrap();

                assert_eq!(fnv1a64(&image.pixels), expected_hash, "format {}", format.0);
                assert_eq!(&image.pixels[..expected_prefix.len()], &expected_prefix);
            }
        }
    }

    #[test]
    fn pvrtc_uses_minimum_footprints_and_rejects_unverified_dimensions() {
        assert_eq!(
            mip_byte_length(1, 1, TextureFormat::PVRTC_RGB4).unwrap(),
            32
        );
        assert_eq!(
            mip_byte_length(8, 8, TextureFormat::PVRTC_RGBA4).unwrap(),
            32
        );
        assert_eq!(
            mip_byte_length(1, 1, TextureFormat::PVRTC_RGB2).unwrap(),
            32
        );
        assert_eq!(
            mip_byte_length(16, 8, TextureFormat::PVRTC_RGBA2).unwrap(),
            32
        );
        assert!(matches!(
            mip_byte_length(12, 8, TextureFormat::PVRTC_RGB4),
            Err(crate::Error::Unsupported(message)) if message.contains("powers of two")
        ));

        let mip_chain = vec![0_u8; 32 * 5];
        let object = texture_object(16, 8, TextureFormat::PVRTC_RGBA2, 5, &mip_chain, None);
        let file = parse_asset(&object);
        let collection = collection_with(file.clone(), "unused", b"");
        let texture = read_texture2d(&collection, &file, 0, TextureReadLimits::default()).unwrap();
        assert_eq!(texture.mip_region(1).unwrap().len(), 32);
        assert!(matches!(
            texture.decode_mip_rgba8(1, TextureReadLimits::default()),
            Err(crate::Error::Unsupported(message)) if message.contains("minimum")
        ));

        let non_power_of_two = texture_object(12, 8, TextureFormat::PVRTC_RGBA4, 1, &[0; 48], None);
        let file = parse_asset(&non_power_of_two);
        let collection = collection_with(file.clone(), "unused", b"");
        let texture = read_texture2d(&collection, &file, 0, TextureReadLimits::default()).unwrap();
        assert!(matches!(
            texture.decode_mip_rgba8(0, TextureReadLimits::default()),
            Err(crate::Error::Unsupported(message)) if message.contains("powers of two")
        ));
    }

    #[test]
    fn pvrtc_enforces_exact_input_output_and_decoder_working_limits() {
        let source = [0_u8; 32];
        let mut output = [0_u8; 8 * 8 * 4];
        let mut oversized = source.to_vec();
        oversized.push(0);
        for malformed in [&source[..31], oversized.as_slice()] {
            assert!(matches!(
                decode_pixels(
                    TextureFormat::PVRTC_RGBA4,
                    malformed,
                    &mut output,
                    8,
                    8
                ),
                Err(crate::Error::InvalidData(message)) if message.contains("expected 32")
            ));
        }
        assert!(matches!(
            decode_pixels(
                TextureFormat::PVRTC_RGBA4,
                &source,
                &mut output[..255],
                8,
                8
            ),
            Err(crate::Error::InvalidData(message)) if message.contains("expected 256")
        ));

        let object = texture_object(8, 8, TextureFormat::PVRTC_RGBA4, 1, &source, None);
        let file = parse_asset(&object);
        let collection = collection_with(file.clone(), "unused", b"");
        let working_limits = TextureReadLimits {
            maximum_decoder_working_bytes: 511,
            ..TextureReadLimits::default()
        };
        let texture = read_texture2d(&collection, &file, 0, working_limits).unwrap();
        assert!(matches!(
            texture.decode_mip_rgba8(0, working_limits),
            Err(crate::Error::InvalidData(message)) if message.contains("working buffers")
        ));

        let output_limits = TextureReadLimits {
            maximum_output_bytes: 255,
            ..TextureReadLimits::default()
        };
        assert!(matches!(
            texture.decode_mip_rgba8(0, output_limits),
            Err(crate::Error::InvalidData(message)) if message.contains("RGBA output")
        ));
    }

    #[test]
    fn crops_partial_dxt1_blocks_and_selects_small_compressed_mips() {
        let mut source = Vec::new();
        source.extend_from_slice(&bc1_block(0xf800, 0x0000, 0));
        source.extend_from_slice(&bc1_block(0x001f, 0x0000, 0));
        source.extend_from_slice(&bc1_block(0x07e0, 0x0000, 0));
        let object = texture_object(5, 3, TextureFormat::DXT1, 2, &source, None);
        let file = parse_asset(&object);
        let collection = collection_with(file.clone(), "unused", b"");
        let texture = read_texture2d(&collection, &file, 0, TextureReadLimits::default()).unwrap();

        let top = texture
            .decode_mip_rgba8(0, TextureReadLimits::default())
            .unwrap();
        let expected_row = [
            255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 0, 0, 255, 255,
        ];
        assert_eq!((top.width, top.height), (5, 3));
        assert_eq!(top.pixels, expected_row.repeat(3));

        let mip = texture
            .decode_mip_rgba8(1, TextureReadLimits::default())
            .unwrap();
        assert_eq!((mip.width, mip.height), (2, 1));
        assert_eq!(mip.pixels, [0, 255, 0, 255, 0, 255, 0, 255]);
    }

    #[test]
    fn decodes_bc4_and_bc5_channels() {
        let selectors = [1; 16];
        let bc4_source = bc_alpha_block(0, 255, selectors);
        let object = texture_object(1, 1, TextureFormat::BC4, 1, &bc4_source, None);
        let file = parse_asset(&object);
        let collection = collection_with(file.clone(), "unused", b"");
        let texture = read_texture2d(&collection, &file, 0, TextureReadLimits::default()).unwrap();
        assert_eq!(
            texture
                .decode_mip_rgba8(0, TextureReadLimits::default())
                .unwrap()
                .pixels,
            [255, 0, 0, 255]
        );

        let mut bc5_source = Vec::new();
        bc5_source.extend_from_slice(&bc_alpha_block(0, 255, selectors));
        bc5_source.extend_from_slice(&bc_alpha_block(255, 0, [0; 16]));
        let object = texture_object(1, 1, TextureFormat::BC5, 1, &bc5_source, None);
        let file = parse_asset(&object);
        let collection = collection_with(file.clone(), "unused", b"");
        let texture = read_texture2d(&collection, &file, 0, TextureReadLimits::default()).unwrap();
        assert_eq!(
            texture
                .decode_mip_rgba8(0, TextureReadLimits::default())
                .unwrap()
                .pixels,
            [255, 255, 0, 255]
        );
    }

    #[test]
    fn resolves_external_data_and_selects_a_mip_region() {
        let top_level = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
        let mip = [21, 22, 23];
        let mut payload = top_level.to_vec();
        payload.extend_from_slice(&mip);
        let object = texture_object(
            2,
            2,
            TextureFormat::RGB24,
            2,
            &[],
            Some((
                2,
                u32::try_from(payload.len()).unwrap(),
                "archive:/image.resS",
            )),
        );
        let file = parse_asset(&object);
        let mut resource = b"xx".to_vec();
        resource.extend_from_slice(&payload);
        resource.extend_from_slice(b"zz");
        let collection = collection_with(file.clone(), "image.resS", &resource);

        let texture = read_texture2d(&collection, &file, 0, TextureReadLimits::default()).unwrap();
        let image = texture
            .decode_mip_rgba8(1, TextureReadLimits::default())
            .unwrap();

        assert_eq!(texture.data.len(), 15);
        assert_eq!((image.width, image.height), (1, 1));
        assert_eq!(image.pixels, [21, 22, 23, 255]);
    }

    #[test]
    fn writes_the_managed_rgba_ir_contract_with_vertical_flip() {
        let image = RgbaImage {
            width: 1,
            height: 2,
            pixels: vec![255, 0, 0, 255, 0, 0, 255, 128],
        };
        let mut output = Vec::new();

        let length = write_rgba_ir(&image, &mut output).unwrap();

        assert_eq!(length, 44);
        assert_eq!(&output[..16], b"HARUKI_RGBAIR_V1");
        assert_eq!(&output[16..20], &1_i32.to_le_bytes());
        assert_eq!(&output[20..24], &2_i32.to_le_bytes());
        assert_eq!(&output[24..28], &4_i32.to_le_bytes());
        assert_eq!(&output[28..32], &1_i32.to_le_bytes());
        assert_eq!(&output[32..36], &0_i32.to_le_bytes());
        assert_eq!(&output[36..], &[0, 0, 255, 128, 255, 0, 0, 255]);

        let mut display_order = Vec::new();
        assert_eq!(
            write_rgba_ir_display_order(&image, &mut display_order).unwrap(),
            44
        );
        assert_eq!(&display_order[36..], &[255, 0, 0, 255, 0, 0, 255, 128]);
    }

    #[test]
    fn rejects_truncated_unsupported_and_oversized_textures() {
        let truncated_dxt1 = texture_object(5, 3, TextureFormat::DXT1, 1, &[0; 15], None);
        let file = parse_asset(&truncated_dxt1);
        let collection = collection_with(file.clone(), "unused", b"");
        let texture = read_texture2d(&collection, &file, 0, TextureReadLimits::default()).unwrap();
        assert!(
            texture
                .decode_mip_rgba8(0, TextureReadLimits::default())
                .is_err()
        );

        let unsupported = texture_object(4, 4, TextureFormat(23), 1, &[0; 16], None);
        let file = parse_asset(&unsupported);
        let collection = collection_with(file.clone(), "unused", b"");
        let texture = read_texture2d(&collection, &file, 0, TextureReadLimits::default()).unwrap();
        assert!(matches!(
            texture.decode_mip_rgba8(0, TextureReadLimits::default()),
            Err(crate::Error::Unsupported(_))
        ));

        let truncated = texture_object(2, 2, TextureFormat::RGBA32, 1, &[0; 15], None);
        let file = parse_asset(&truncated);
        let collection = collection_with(file.clone(), "unused", b"");
        let texture = read_texture2d(&collection, &file, 0, TextureReadLimits::default()).unwrap();
        assert!(
            texture
                .decode_mip_rgba8(0, TextureReadLimits::default())
                .is_err()
        );

        let oversized = texture_object(2, 2, TextureFormat::RGBA32, 1, &[0; 16], None);
        let file = parse_asset(&oversized);
        let collection = collection_with(file.clone(), "unused", b"");
        let limits = TextureReadLimits {
            maximum_output_bytes: 15,
            ..TextureReadLimits::default()
        };
        let texture = read_texture2d(&collection, &file, 0, limits).unwrap();
        assert!(texture.decode_mip_rgba8(0, limits).is_err());

        let mut switch_texture = texture;
        switch_texture.target_platform = 38;
        assert_eq!(
            switch_texture
                .decode_mip_rgba8(0, TextureReadLimits::default())
                .unwrap()
                .pixels,
            [0; 16]
        );

        let bc7 = texture_object(4, 4, TextureFormat::BC7, 1, &[0; 16], None);
        let file = parse_asset(&bc7);
        let collection = collection_with(file.clone(), "unused", b"");
        let limits = TextureReadLimits {
            maximum_decoder_working_bytes: 63,
            ..TextureReadLimits::default()
        };
        let texture = read_texture2d(&collection, &file, 0, limits).unwrap();
        assert!(matches!(
            texture.decode_mip_rgba8(0, limits),
            Err(crate::Error::InvalidData(message)) if message.contains("working buffer")
        ));
    }

    #[test]
    fn rejects_bad_dimensions_and_missing_external_ranges() {
        let object = texture_object(
            2,
            2,
            TextureFormat::RGBA32,
            1,
            &[],
            Some((20, 16, "x.resS")),
        );
        let file = parse_asset(&object);
        let collection = collection_with(file.clone(), "x.resS", &[0; 8]);
        assert!(read_texture2d(&collection, &file, 0, TextureReadLimits::default()).is_err());

        let object = texture_object(2, 2, TextureFormat::RGBA32, 1, &[0; 16], None);
        let file = parse_asset(&object);
        let collection = collection_with(file.clone(), "unused", b"");
        let limits = TextureReadLimits {
            maximum_dimension: 1,
            ..TextureReadLimits::default()
        };
        assert!(read_texture2d(&collection, &file, 0, limits).is_err());

        let object = texture_object(-1, 2, TextureFormat::RGBA32, 1, &[], None);
        let file = parse_asset(&object);
        let collection = collection_with(file.clone(), "unused", b"");
        assert!(read_texture2d(&collection, &file, 0, TextureReadLimits::default()).is_err());

        let object = texture_object(2, 2, TextureFormat::RGBA32, 1, &[0; 16], None);
        let file = parse_asset(&object);
        let collection = collection_with(file.clone(), "unused", b"");
        let limits = TextureReadLimits {
            maximum_payload_bytes: 15,
            ..TextureReadLimits::default()
        };
        assert!(read_texture2d(&collection, &file, 0, limits).is_err());
    }

    fn collection_with(
        file: SerializedFile,
        resource_path: &str,
        resource: &[u8],
    ) -> AssetCollection {
        AssetCollection::from_loaded_parts(
            vec![LoadedSerializedFile {
                path: "fixture.assets".to_owned(),
                file,
            }],
            vec![LoadedResource {
                path: resource_path.to_owned(),
                region: Region::from_bytes(resource.to_vec()),
            }],
        )
    }

    fn bc1_block(endpoint_0: u16, endpoint_1: u16, selectors: u32) -> [u8; 8] {
        let mut block = [0; 8];
        block[..2].copy_from_slice(&endpoint_0.to_le_bytes());
        block[2..4].copy_from_slice(&endpoint_1.to_le_bytes());
        block[4..].copy_from_slice(&selectors.to_le_bytes());
        block
    }

    fn bc_alpha_block(endpoint_0: u8, endpoint_1: u8, selectors: [u8; 16]) -> [u8; 8] {
        let mut selector_bits = 0_u64;
        for (index, selector) in selectors.into_iter().enumerate() {
            assert!(selector < 8);
            selector_bits |= u64::from(selector) << (index * 3);
        }
        let encoded_selectors = selector_bits.to_le_bytes();
        let mut block = [0; 8];
        block[0] = endpoint_0;
        block[1] = endpoint_1;
        block[2..].copy_from_slice(&encoded_selectors[..6]);
        block
    }

    fn dxt5_block(
        alpha: [u8; 8],
        color_endpoint_0: u16,
        color_endpoint_1: u16,
        color_selectors: u32,
    ) -> [u8; 16] {
        let mut block = [0; 16];
        block[..8].copy_from_slice(&alpha);
        block[8..].copy_from_slice(&bc1_block(
            color_endpoint_0,
            color_endpoint_1,
            color_selectors,
        ));
        block
    }

    fn fnv1a64(bytes: &[u8]) -> u64 {
        bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
        })
    }

    /// Inverse of the managed `Texture2DSwitchDeswizzler.Unswizzle` loop,
    /// retained independently in the test so multi-GOB crop fixtures exercise
    /// the observable C# byte order rather than calling the implementation
    /// under test twice.
    fn managed_switch_swizzle_oracle(
        linear: &[u8],
        padded_width: u32,
        padded_height: u32,
        layout: SwitchTextureLayout,
        gobs_per_block: u32,
    ) -> Vec<u8> {
        const X: [usize; 32] = [
            0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1, 2, 2, 3, 3, 2, 2, 3, 3, 2, 2, 3, 3, 2,
            2, 3, 3,
        ];
        const Y: [usize; 32] = [
            0, 1, 0, 1, 2, 3, 2, 3, 4, 5, 4, 5, 6, 7, 6, 7, 0, 1, 0, 1, 2, 3, 2, 3, 4, 5, 4, 5, 6,
            7, 6, 7,
        ];
        let chunk_columns = usize::try_from(padded_width / layout.texel_width).unwrap();
        let chunk_rows = usize::try_from(padded_height / layout.texel_height).unwrap();
        let gob_columns = chunk_columns / 4;
        let gob_rows = chunk_rows / 8;
        let gobs_per_block = usize::try_from(gobs_per_block).unwrap();
        let chunk_bytes = usize::try_from(SWITCH_GOB_CHUNK_BYTES).unwrap();
        let mut swizzled = vec![0_u8; linear.len()];
        let mut source_position = 0;
        for block_row in 0..gob_rows / gobs_per_block {
            for gob_column in 0..gob_columns {
                for gob_in_block in 0..gobs_per_block {
                    for chunk_in_gob in 0..32 {
                        let destination_x = gob_column * 4 + X[chunk_in_gob];
                        let destination_y =
                            (block_row * gobs_per_block + gob_in_block) * 8 + Y[chunk_in_gob];
                        let destination_position =
                            (destination_y * chunk_columns + destination_x) * chunk_bytes;
                        swizzled[source_position..source_position + chunk_bytes].copy_from_slice(
                            &linear[destination_position..destination_position + chunk_bytes],
                        );
                        source_position += chunk_bytes;
                    }
                }
            }
        }
        assert_eq!(source_position, linear.len());
        swizzled
    }

    fn switch_platform_blob(block_height_log2: i32) -> Region {
        let mut blob = vec![0_u8; 12];
        blob[8..12].copy_from_slice(&block_height_log2.to_le_bytes());
        Region::from_bytes(blob)
    }

    fn crunched_texture(format: TextureFormat, uses_unity_crunch: bool, data: &[u8]) -> Texture2D {
        Texture2D {
            path_id: 1,
            name: "crunched".to_owned(),
            width: 512,
            height: 512,
            complete_image_size: u32::try_from(data.len()).unwrap(),
            mips_stripped: 0,
            mip_count: 10,
            format,
            image_count: 1,
            dimension: 2,
            platform_blob_size: 0,
            platform_blob: None,
            target_platform: NO_TARGET_PLATFORM,
            uses_unity_crunch,
            data: Region::from_bytes(data.to_vec()),
        }
    }

    fn switch_texture(
        width: u32,
        height: u32,
        format: TextureFormat,
        block_height_log2: i32,
        data: &[u8],
    ) -> Texture2D {
        Texture2D {
            path_id: 1,
            name: "switch".to_owned(),
            width,
            height,
            complete_image_size: u32::try_from(data.len()).unwrap(),
            mips_stripped: 0,
            mip_count: 1,
            format,
            image_count: 1,
            dimension: 2,
            platform_blob_size: 12,
            platform_blob: Some(switch_platform_blob(block_height_log2)),
            target_platform: 38,
            uses_unity_crunch: false,
            data: Region::from_bytes(data.to_vec()),
        }
    }

    fn texture_object(
        width: i32,
        height: i32,
        format: TextureFormat,
        mip_count: i32,
        inline_data: &[u8],
        stream: Option<(u64, u32, &str)>,
    ) -> Vec<u8> {
        texture_object_with_platform_blob(
            width,
            height,
            format,
            mip_count,
            inline_data,
            stream,
            &[],
        )
    }

    fn texture_object_with_platform_blob(
        width: i32,
        height: i32,
        format: TextureFormat,
        mip_count: i32,
        inline_data: &[u8],
        stream: Option<(u64, u32, &str)>,
        platform_blob: &[u8],
    ) -> Vec<u8> {
        let mut output = Vec::new();
        push_aligned_string(&mut output, "image");
        push_i32(&mut output, 0);
        output.push(0);
        output.push(0);
        align(&mut output, 4);
        push_i32(&mut output, width);
        push_i32(&mut output, height);
        let complete_size = stream.map_or_else(
            || u32::try_from(inline_data.len()).unwrap(),
            |(_, size, _)| size,
        );
        output.extend_from_slice(&complete_size.to_le_bytes());
        push_i32(&mut output, 0);
        push_i32(&mut output, format.0);
        push_i32(&mut output, mip_count);
        output.extend_from_slice(&[0, 0, 0]);
        align(&mut output, 4);
        push_aligned_string(&mut output, "");
        output.push(0);
        align(&mut output, 4);
        push_i32(&mut output, 0);
        push_i32(&mut output, 1);
        push_i32(&mut output, 2);
        output.extend_from_slice(&[0_u8; 24]);
        push_i32(&mut output, 0);
        push_i32(&mut output, 0);
        push_i32(&mut output, i32::try_from(platform_blob.len()).unwrap());
        output.extend_from_slice(platform_blob);
        align(&mut output, 4);
        if let Some((offset, size, path)) = stream {
            push_i32(&mut output, 0);
            output.extend_from_slice(&i64::try_from(offset).unwrap().to_le_bytes());
            output.extend_from_slice(&size.to_le_bytes());
            push_aligned_string(&mut output, path);
        } else {
            push_i32(&mut output, i32::try_from(inline_data.len()).unwrap());
            output.extend_from_slice(inline_data);
        }
        output
    }

    fn parse_asset(object: &[u8]) -> SerializedFile {
        SerializedFile::open(Region::from_bytes(synthetic_v22_asset(object))).unwrap()
    }

    fn synthetic_v22_asset(object: &[u8]) -> Vec<u8> {
        let mut metadata = Vec::new();
        metadata.extend_from_slice(b"2022.3.62f1\0");
        push_i32(&mut metadata, 13);
        metadata.push(0);
        push_i32(&mut metadata, 1);
        push_i32(&mut metadata, TEXTURE_2D_CLASS_ID);
        metadata.push(0);
        metadata.extend_from_slice(&(-1_i16).to_le_bytes());
        metadata.extend_from_slice(&[0_u8; 16]);
        push_i32(&mut metadata, 1);
        align_with_base(&mut metadata, 48, 4);
        metadata.extend_from_slice(&7_i64.to_le_bytes());
        metadata.extend_from_slice(&0_i64.to_le_bytes());
        metadata.extend_from_slice(&u32::try_from(object.len()).unwrap().to_le_bytes());
        push_i32(&mut metadata, 0);
        for _ in 0..3 {
            push_i32(&mut metadata, 0);
        }
        metadata.push(0);

        let metadata_size = u32::try_from(metadata.len()).unwrap();
        let metadata_end = 48_u64 + u64::from(metadata_size);
        let data_offset = metadata_end.div_ceil(16) * 16;
        let file_size = data_offset + u64::try_from(object.len()).unwrap();
        let mut bytes = vec![0_u8; 48];
        bytes[8..12].copy_from_slice(&22_u32.to_be_bytes());
        bytes[20..24].copy_from_slice(&metadata_size.to_be_bytes());
        bytes[24..32].copy_from_slice(&i64::try_from(file_size).unwrap().to_be_bytes());
        bytes[32..40].copy_from_slice(&i64::try_from(data_offset).unwrap().to_be_bytes());
        bytes.extend_from_slice(&metadata);
        bytes.resize(usize::try_from(data_offset).unwrap(), 0);
        bytes.extend_from_slice(object);
        bytes
    }

    fn push_aligned_string(output: &mut Vec<u8>, value: &str) {
        push_i32(output, i32::try_from(value.len()).unwrap());
        output.extend_from_slice(value.as_bytes());
        if !value.is_empty() {
            align(output, 4);
        }
    }

    fn push_i32(output: &mut Vec<u8>, value: i32) {
        output.extend_from_slice(&value.to_le_bytes());
    }

    fn align(output: &mut Vec<u8>, alignment: usize) {
        while !output.len().is_multiple_of(alignment) {
            output.push(0);
        }
    }

    fn align_with_base(output: &mut Vec<u8>, base: usize, alignment: usize) {
        while !(base + output.len()).is_multiple_of(alignment) {
            output.push(0);
        }
    }
}
