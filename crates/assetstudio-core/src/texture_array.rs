use std::io::Write;

use crate::endian::{Endian, EndianReader, checked_length};
use crate::loader::AssetCollection;
use crate::serialized::SerializedFile;
use crate::source::{Region, RegionCursor};
use crate::texture::{RgbaImage, Texture2D, TextureFormat, TextureReadLimits, write_rgba_ir};
use crate::{Error, Result};

pub const TEXTURE_2D_ARRAY_CLASS_ID: i32 = 187;

const NO_TARGET_PLATFORM: i32 = -2;
const PAYLOAD_BUNDLE_MAGIC: &[u8; 30] = b"HARUKI_ASSET_PAYLOAD_BUNDLE_V1";
const RGBA_IR_HEADER_SIZE: u64 = 36;

/// Defensive limits for a versioned `Texture2DArray` and its generated bundle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextureArrayReadLimits {
    pub maximum_dimension: u32,
    pub maximum_depth: u32,
    pub maximum_mip_levels: u32,
    pub maximum_string_bytes: usize,
    pub maximum_payload_bytes: u64,
    pub maximum_output_bytes: u64,
    pub maximum_decoder_working_bytes: u64,
    pub maximum_bundle_bytes: u64,
}

impl Default for TextureArrayReadLimits {
    fn default() -> Self {
        Self {
            maximum_dimension: 65_536,
            maximum_depth: 4_096,
            maximum_mip_levels: 32,
            maximum_string_bytes: 16 * 1024 * 1024,
            maximum_payload_bytes: 1024 * 1024 * 1024,
            maximum_output_bytes: 1024 * 1024 * 1024,
            maximum_decoder_working_bytes: 512 * 1024 * 1024,
            maximum_bundle_bytes: 1024 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextureArrayStreamingInfo {
    pub offset: u64,
    pub size: u64,
    pub path: String,
}

/// A source-bound Unity `Texture2DArray`.
#[derive(Debug, Clone)]
pub struct Texture2DArray {
    pub path_id: i64,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub graphics_format: i32,
    pub texture_format: TextureFormat,
    pub mip_count: u32,
    pub mips_stripped: u32,
    pub data_size: u64,
    pub color_space: i32,
    pub usage_mode: Option<i32>,
    pub target_platform: i32,
    pub streaming_info: Option<TextureArrayStreamingInfo>,
    pub data: Region,
}

impl Texture2DArray {
    #[must_use]
    pub fn layer_count(&self) -> u32 {
        self.depth
    }

    pub fn layer_region(&self, layer: u32) -> Result<Region> {
        if layer >= self.depth {
            return Err(Error::invalid_data(format!(
                "Texture2DArray layer {layer} exceeds depth {}",
                self.depth
            )));
        }
        let stride = self.layer_stride()?;
        let offset = u64::from(layer)
            .checked_mul(stride)
            .ok_or_else(|| Error::invalid_data("Texture2DArray layer offset overflowed"))?;
        self.data.subregion(offset, stride)
    }

    /// Decodes one resident mip-zero layer through the existing pure-Rust
    /// `Texture2D` format decoders.
    pub fn decode_layer_mip0_rgba8(
        &self,
        layer: u32,
        limits: TextureArrayReadLimits,
    ) -> Result<RgbaImage> {
        if self.mips_stripped != 0 {
            return Err(Error::unsupported(format!(
                "Texture2DArray has {} stripped mip level(s); reconstructing mip 0 is not implemented",
                self.mips_stripped
            )));
        }
        let layer_stride = self.layer_stride()?;
        let complete_image_size = u32::try_from(layer_stride).map_err(|_| {
            Error::invalid_data("Texture2DArray layer payload does not fit in a Texture2D size")
        })?;
        let texture = Texture2D {
            path_id: self.path_id,
            name: format!("{}_{}", self.name, layer + 1),
            width: self.width,
            height: self.height,
            complete_image_size,
            mips_stripped: 0,
            mip_count: self.mip_count,
            format: self.texture_format,
            image_count: 1,
            dimension: 2,
            platform_blob_size: 0,
            platform_blob: None,
            target_platform: self.target_platform,
            uses_unity_crunch: false,
            data: self.layer_region(layer)?,
        };
        texture.decode_mip_rgba8(0, texture_limits(limits))
    }

    /// Decodes every layer in numeric order while enforcing a cumulative RGBA
    /// output budget.
    pub fn decode_mip0_layers_rgba8(
        &self,
        limits: TextureArrayReadLimits,
    ) -> Result<Vec<RgbaImage>> {
        let depth = usize::try_from(self.depth).map_err(|_| {
            Error::invalid_data("Texture2DArray depth is too large for this platform")
        })?;
        let mut images = Vec::new();
        images.try_reserve_exact(depth).map_err(|error| {
            Error::invalid_data(format!("cannot allocate Texture2DArray layers: {error}"))
        })?;
        let mut total_output = 0_u64;
        for layer in 0..self.depth {
            let image = self.decode_layer_mip0_rgba8(layer, limits)?;
            let image_length = u64::try_from(image.pixels.len()).map_err(|_| {
                Error::invalid_data("Texture2DArray RGBA output length does not fit in u64")
            })?;
            total_output = total_output
                .checked_add(image_length)
                .ok_or_else(|| Error::invalid_data("Texture2DArray RGBA output overflowed"))?;
            if total_output > limits.maximum_output_bytes {
                return Err(Error::invalid_data(format!(
                    "Texture2DArray RGBA output is {total_output} bytes, exceeding limit {}",
                    limits.maximum_output_bytes
                )));
            }
            images.push(image);
        }
        Ok(images)
    }

    fn layer_stride(&self) -> Result<u64> {
        let depth = u64::from(self.depth);
        if depth == 0 {
            return Err(Error::invalid_data("Texture2DArray depth must be positive"));
        }
        if !self.data_size.is_multiple_of(depth) {
            return Err(Error::invalid_data(format!(
                "Texture2DArray data size {} is not divisible by depth {}",
                self.data_size, self.depth
            )));
        }
        let stride = self.data_size / depth;
        if stride == 0 {
            return Err(Error::invalid_data(
                "Texture2DArray layer payload must not be empty",
            ));
        }
        Ok(stride)
    }
}

/// Reads the hand-written `Texture2DArray` layout used by `AssetStudio` for
/// Unity 2019 and newer, retaining inline or streamed bytes as a bounded
/// [`Region`].
#[allow(clippy::too_many_lines)]
pub fn read_texture2d_array(
    collection: &AssetCollection,
    file: &SerializedFile,
    object_index: usize,
    limits: TextureArrayReadLimits,
) -> Result<Texture2DArray> {
    if file.unity_version.is_stripped() {
        return Err(Error::unsupported(
            "Texture2DArray requires a Unity version because its layout is version-dependent",
        ));
    }
    if file.unity_version.major < 2019 {
        return Err(Error::unsupported(format!(
            "Texture2DArray layout before Unity 2019 is not implemented (found {})",
            file.unity_version
        )));
    }

    let mut reader = TextureArrayObjectReader::new(file, object_index, limits)?;
    let name = reader.read_named_texture()?;
    let color_space = reader.reader.read_i32()?;
    let graphics_format = reader.reader.read_i32()?;
    let texture_format = graphics_format_to_texture_format(graphics_format)?;
    let width = reader.read_positive_dimension("Texture2DArray width")?;
    let height = reader.read_positive_dimension("Texture2DArray height")?;
    validate_dimensions(width, height, &limits)?;
    let depth = reader.read_positive_count("Texture2DArray depth", limits.maximum_depth)?;
    let mip_count =
        reader.read_positive_count("Texture2DArray mip count", limits.maximum_mip_levels)?;
    let mips_stripped = if version_at_least_major_minor(file, 2023, 2) {
        reader.read_non_negative_i32("Texture2DArray stripped mip count")?
    } else {
        0
    };
    if mips_stripped > mip_count {
        return Err(Error::invalid_data(format!(
            "Texture2DArray strips {mips_stripped} mip levels but declares only {mip_count}"
        )));
    }
    let data_size = u64::from(reader.reader.read_u32()?);
    if data_size == 0 {
        return Err(Error::invalid_data(
            "Texture2DArray declared data size must be positive",
        ));
    }
    if data_size > limits.maximum_payload_bytes {
        return Err(Error::invalid_data(format!(
            "Texture2DArray declared data is {data_size} bytes, exceeding limit {}",
            limits.maximum_payload_bytes
        )));
    }
    reader.skip(24, "Texture2DArray GL texture settings")?;
    let usage_mode = if version_at_least_major_minor(file, 2020, 2) {
        Some(reader.reader.read_i32()?)
    } else {
        None
    };
    reader.skip(1, "Texture2DArray readable flag")?;
    if version_after_major_minor(file, 2023, 2) {
        reader.skip(1, "Texture2DArray mip-limit flag")?;
        reader.align(4)?;
        reader.read_aligned_string("Texture2DArray mip-limit group")?;
    } else {
        reader.align(4)?;
    }

    let image_data_size = reader.read_non_negative_i32("Texture2DArray image data")?;
    let (streaming_info, raw_data) = if image_data_size == 0 {
        let offset = if file.unity_version.major >= 2020 {
            reader.read_non_negative_i64("Texture2DArray stream offset")?
        } else {
            u64::from(reader.reader.read_u32()?)
        };
        let size = u64::from(reader.reader.read_u32()?);
        let path = reader.read_aligned_string("Texture2DArray stream path")?;
        let info = TextureArrayStreamingInfo { offset, size, path };
        let data = if info.path.is_empty() {
            reader.inline_region(0, limits.maximum_payload_bytes, "Texture2DArray image data")?
        } else {
            resolve_external_region(collection, &info, limits.maximum_payload_bytes)?
        };
        (Some(info), data)
    } else {
        (
            None,
            reader.inline_region(
                u64::from(image_data_size),
                limits.maximum_payload_bytes,
                "Texture2DArray image data",
            )?,
        )
    };
    if raw_data.len() < data_size {
        return Err(Error::invalid_data(format!(
            "Texture2DArray source contains {} bytes, fewer than declared data size {data_size}",
            raw_data.len()
        )));
    }
    if !data_size.is_multiple_of(u64::from(depth)) {
        return Err(Error::invalid_data(format!(
            "Texture2DArray data size {data_size} is not divisible by depth {depth}"
        )));
    }
    let data = raw_data.subregion(0, data_size)?;

    Ok(Texture2DArray {
        path_id: reader.path_id,
        name,
        width,
        height,
        depth,
        graphics_format,
        texture_format,
        mip_count,
        mips_stripped,
        data_size,
        color_space,
        usage_mode,
        target_platform: file.target_platform,
        streaming_info,
        data,
    })
}

/// Writes the managed API's exact `HARUKI_ASSET_PAYLOAD_BUNDLE_V1` contract.
/// Directory entries and payloads are both ordered by ascending layer number.
pub fn write_texture2d_array_rgba_bundle<W: Write>(
    texture: &Texture2DArray,
    limits: TextureArrayReadLimits,
    output: &mut W,
) -> Result<u64> {
    let images = texture.decode_mip0_layers_rgba8(limits)?;
    let mut entries = Vec::new();
    entries.try_reserve_exact(images.len()).map_err(|error| {
        Error::invalid_data(format!(
            "cannot allocate Texture2DArray bundle entries: {error}"
        ))
    })?;
    let mut header_length = u64::try_from(PAYLOAD_BUNDLE_MAGIC.len() + 4)
        .expect("payload bundle fixed header fits in u64");
    let mut payload_length = 0_u64;
    for (layer, image) in images.iter().enumerate() {
        let name = format!("layer_{layer:04}.rgba");
        let name_length = u64::try_from(name.len())
            .map_err(|_| Error::invalid_data("Texture2DArray entry name is too long"))?;
        let pixels = checked_product(
            u64::from(image.width),
            u64::from(image.height),
            "Texture2DArray layer pixel count",
        )?;
        let rgba_length = checked_product(pixels, 4, "Texture2DArray layer RGBA byte size")?;
        let entry_payload_length = RGBA_IR_HEADER_SIZE
            .checked_add(rgba_length)
            .ok_or_else(|| Error::invalid_data("Texture2DArray layer IR length overflowed"))?;
        header_length = header_length
            .checked_add(12)
            .and_then(|length| length.checked_add(name_length))
            .ok_or_else(|| Error::invalid_data("Texture2DArray bundle header overflowed"))?;
        payload_length = payload_length
            .checked_add(entry_payload_length)
            .ok_or_else(|| Error::invalid_data("Texture2DArray bundle payload overflowed"))?;
        entries.push((name, entry_payload_length));
    }
    let total_length = header_length
        .checked_add(payload_length)
        .ok_or_else(|| Error::invalid_data("Texture2DArray bundle length overflowed"))?;
    if total_length > limits.maximum_bundle_bytes {
        return Err(Error::invalid_data(format!(
            "Texture2DArray bundle is {total_length} bytes, exceeding limit {}",
            limits.maximum_bundle_bytes
        )));
    }

    output.write_all(PAYLOAD_BUNDLE_MAGIC)?;
    let entry_count = i32::try_from(entries.len())
        .map_err(|_| Error::invalid_data("Texture2DArray entry count does not fit in i32"))?;
    output.write_all(&entry_count.to_le_bytes())?;
    for (name, entry_payload_length) in &entries {
        let name_length = i32::try_from(name.len())
            .map_err(|_| Error::invalid_data("Texture2DArray entry name is too long"))?;
        let entry_payload_length = i64::try_from(*entry_payload_length).map_err(|_| {
            Error::invalid_data("Texture2DArray entry payload length does not fit in i64")
        })?;
        output.write_all(&name_length.to_le_bytes())?;
        output.write_all(&entry_payload_length.to_le_bytes())?;
        output.write_all(name.as_bytes())?;
    }
    let mut written_payload = 0_u64;
    for image in &images {
        written_payload = written_payload
            .checked_add(write_rgba_ir(image, output)?)
            .ok_or_else(|| Error::invalid_data("Texture2DArray written payload overflowed"))?;
    }
    if written_payload != payload_length {
        return Err(Error::invalid_data(format!(
            "Texture2DArray wrote {written_payload} payload bytes, expected {payload_length}"
        )));
    }
    Ok(total_length)
}

fn graphics_format_to_texture_format(graphics_format: i32) -> Result<TextureFormat> {
    Ok(match graphics_format {
        1 | 5 | 13 => TextureFormat::R8,
        2 | 6 | 14 => TextureFormat::RG16,
        3 | 7 | 15 => TextureFormat::RGB24,
        4 | 8 | 16 => TextureFormat::RGBA32,
        21 | 29 => TextureFormat::R16,
        22 | 30 => TextureFormat::RG32,
        23 | 31 => TextureFormat::RGB48,
        24 | 32 => TextureFormat::RGBA64,
        45 => TextureFormat::R_HALF,
        46 => TextureFormat::RG_HALF,
        47 | 48 => TextureFormat::RGBA_HALF,
        49 => TextureFormat::R_FLOAT,
        50 => TextureFormat::RG_FLOAT,
        51 | 52 => TextureFormat::RGBA_FLOAT,
        57 | 59 | 63 => TextureFormat::BGRA32,
        73 => TextureFormat::RGB9E5_FLOAT,
        96 | 97 => TextureFormat::DXT1,
        98 | 99 => TextureFormat::DXT3,
        100 | 101 => TextureFormat::DXT5,
        102 => TextureFormat::BC4,
        104 => TextureFormat::BC5,
        106 | 107 => TextureFormat::BC6H,
        108 | 109 => TextureFormat::BC7,
        110 | 111 => TextureFormat::PVRTC_RGB2,
        112 | 113 => TextureFormat::PVRTC_RGB4,
        114 | 115 => TextureFormat::PVRTC_RGBA2,
        116 | 117 => TextureFormat::PVRTC_RGBA4,
        118 => TextureFormat::ETC_RGB4,
        119 | 120 => TextureFormat::ETC2_RGB,
        121 | 122 => TextureFormat::ETC2_RGBA1,
        123 | 124 => TextureFormat::ETC2_RGBA8,
        125 => TextureFormat::EAC_R,
        126 => TextureFormat::EAC_R_SIGNED,
        127 => TextureFormat::EAC_RG,
        128 => TextureFormat::EAC_RG_SIGNED,
        129 | 130 => TextureFormat::ASTC_RGBA_4X4,
        131 | 132 => TextureFormat::ASTC_RGBA_5X5,
        133 | 134 => TextureFormat::ASTC_RGBA_6X6,
        135 | 136 => TextureFormat::ASTC_RGBA_8X8,
        137 | 138 => TextureFormat::ASTC_RGBA_10X10,
        139 | 140 => TextureFormat::ASTC_RGBA_12X12,
        141 | 144 => TextureFormat::YUY2,
        145 => TextureFormat::ASTC_HDR_4X4,
        146 => TextureFormat::ASTC_HDR_5X5,
        147 => TextureFormat::ASTC_HDR_6X6,
        148 => TextureFormat::ASTC_HDR_8X8,
        149 => TextureFormat::ASTC_HDR_10X10,
        150 => TextureFormat::ASTC_HDR_12X12,
        _ => {
            return Err(Error::unsupported(format!(
                "Texture2DArray GraphicsFormat {graphics_format} has no native Rust decoder"
            )));
        }
    })
}

fn texture_limits(limits: TextureArrayReadLimits) -> TextureReadLimits {
    TextureReadLimits {
        maximum_dimension: limits.maximum_dimension,
        maximum_mip_levels: limits.maximum_mip_levels,
        maximum_payload_bytes: limits.maximum_payload_bytes,
        maximum_output_bytes: limits.maximum_output_bytes,
        maximum_decoder_working_bytes: limits.maximum_decoder_working_bytes,
        ..TextureReadLimits::default()
    }
}

fn validate_dimensions(width: u32, height: u32, limits: &TextureArrayReadLimits) -> Result<()> {
    if width > limits.maximum_dimension || height > limits.maximum_dimension {
        return Err(Error::invalid_data(format!(
            "Texture2DArray dimensions {width}x{height} exceed limit {}",
            limits.maximum_dimension
        )));
    }
    checked_product(
        u64::from(width),
        u64::from(height),
        "Texture2DArray pixel count",
    )?;
    Ok(())
}

fn checked_product(left: u64, right: u64, field: &str) -> Result<u64> {
    left.checked_mul(right)
        .ok_or_else(|| Error::invalid_data(format!("{field} overflowed")))
}

fn resolve_external_region(
    collection: &AssetCollection,
    info: &TextureArrayStreamingInfo,
    maximum: u64,
) -> Result<Region> {
    if info.size > maximum {
        return Err(Error::invalid_data(format!(
            "Texture2DArray external data is {} bytes, exceeding limit {maximum}",
            info.size
        )));
    }
    let resource = collection.resource(&info.path).ok_or_else(|| {
        Error::invalid_data(format!("external resource was not found: {}", info.path))
    })?;
    resource.region.subregion(info.offset, info.size)
}

fn version_at_least_major_minor(file: &SerializedFile, major: u32, minor: u32) -> bool {
    (file.unity_version.major, file.unity_version.minor) >= (major, minor)
}

fn version_after_major_minor(file: &SerializedFile, major: u32, minor: u32) -> bool {
    (file.unity_version.major, file.unity_version.minor) > (major, minor)
}

struct TextureArrayObjectReader {
    region: Region,
    reader: EndianReader<RegionCursor>,
    absolute_start: u64,
    path_id: i64,
    target_platform: i32,
    format_version: u32,
    unity_major: u32,
    unity_minor: u32,
    limits: TextureArrayReadLimits,
}

impl TextureArrayObjectReader {
    fn new(
        file: &SerializedFile,
        object_index: usize,
        limits: TextureArrayReadLimits,
    ) -> Result<Self> {
        let object = file.objects.get(object_index).ok_or_else(|| {
            Error::invalid_data(format!(
                "serialized object index {object_index} is out of range"
            ))
        })?;
        if object.class_id != TEXTURE_2D_ARRAY_CLASS_ID {
            return Err(Error::unsupported(format!(
                "object {} has class ID {}, not Texture2DArray ({TEXTURE_2D_ARRAY_CLASS_ID})",
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
            unity_major: file.unity_version.major,
            unity_minor: file.unity_version.minor,
            limits,
        })
    }

    fn read_named_texture(&mut self) -> Result<String> {
        if self.target_platform == NO_TARGET_PLATFORM {
            self.skip(4, "Object hide flags")?;
            self.read_pptr()?;
            self.read_pptr()?;
        }
        let name = self.read_aligned_string("Texture2DArray name")?;
        if (self.unity_major, self.unity_minor) >= (2017, 3) {
            if (self.unity_major, self.unity_minor) < (2023, 2) {
                self.skip(5, "Texture fallback settings")?;
            }
            if (self.unity_major, self.unity_minor) >= (2020, 2) {
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

#[cfg(test)]
mod tests {
    use crate::Error;
    use crate::loader::{AssetCollection, LoadedResource, LoadedSerializedFile};
    use crate::serialized::SerializedFile;
    use crate::source::Region;
    use crate::studio::Studio;
    use crate::texture::TextureFormat;

    use super::{
        PAYLOAD_BUNDLE_MAGIC, TEXTURE_2D_ARRAY_CLASS_ID, TextureArrayReadLimits,
        graphics_format_to_texture_format, read_texture2d_array, write_texture2d_array_rgba_bundle,
    };

    #[test]
    fn maps_every_graphics_format_backed_by_the_current_native_decoders() {
        let cases = [
            (&[1, 5, 13][..], TextureFormat::R8),
            (&[2, 6, 14], TextureFormat::RG16),
            (&[3, 7, 15], TextureFormat::RGB24),
            (&[4, 8, 16], TextureFormat::RGBA32),
            (&[21, 29], TextureFormat::R16),
            (&[22, 30], TextureFormat::RG32),
            (&[23, 31], TextureFormat::RGB48),
            (&[24, 32], TextureFormat::RGBA64),
            (&[45], TextureFormat::R_HALF),
            (&[46], TextureFormat::RG_HALF),
            (&[47, 48], TextureFormat::RGBA_HALF),
            (&[49], TextureFormat::R_FLOAT),
            (&[50], TextureFormat::RG_FLOAT),
            (&[51, 52], TextureFormat::RGBA_FLOAT),
            (&[57, 59, 63], TextureFormat::BGRA32),
            (&[73], TextureFormat::RGB9E5_FLOAT),
            (&[96, 97], TextureFormat::DXT1),
            (&[98, 99], TextureFormat::DXT3),
            (&[100, 101], TextureFormat::DXT5),
            (&[102], TextureFormat::BC4),
            (&[104], TextureFormat::BC5),
            (&[106, 107], TextureFormat::BC6H),
            (&[108, 109], TextureFormat::BC7),
            (&[110, 111], TextureFormat::PVRTC_RGB2),
            (&[112, 113], TextureFormat::PVRTC_RGB4),
            (&[114, 115], TextureFormat::PVRTC_RGBA2),
            (&[116, 117], TextureFormat::PVRTC_RGBA4),
            (&[118], TextureFormat::ETC_RGB4),
            (&[119, 120], TextureFormat::ETC2_RGB),
            (&[121, 122], TextureFormat::ETC2_RGBA1),
            (&[123, 124], TextureFormat::ETC2_RGBA8),
            (&[125], TextureFormat::EAC_R),
            (&[126], TextureFormat::EAC_R_SIGNED),
            (&[127], TextureFormat::EAC_RG),
            (&[128], TextureFormat::EAC_RG_SIGNED),
            (&[129, 130], TextureFormat::ASTC_RGBA_4X4),
            (&[131, 132], TextureFormat::ASTC_RGBA_5X5),
            (&[133, 134], TextureFormat::ASTC_RGBA_6X6),
            (&[135, 136], TextureFormat::ASTC_RGBA_8X8),
            (&[137, 138], TextureFormat::ASTC_RGBA_10X10),
            (&[139, 140], TextureFormat::ASTC_RGBA_12X12),
            (&[141, 144], TextureFormat::YUY2),
            (&[145], TextureFormat::ASTC_HDR_4X4),
            (&[146], TextureFormat::ASTC_HDR_5X5),
            (&[147], TextureFormat::ASTC_HDR_6X6),
            (&[148], TextureFormat::ASTC_HDR_8X8),
            (&[149], TextureFormat::ASTC_HDR_10X10),
            (&[150], TextureFormat::ASTC_HDR_12X12),
        ];
        for (graphics_formats, expected) in cases {
            for graphics_format in graphics_formats {
                assert_eq!(
                    graphics_format_to_texture_format(*graphics_format).unwrap(),
                    expected,
                    "GraphicsFormat {graphics_format}"
                );
            }
        }
    }

    #[test]
    fn decodes_pvrtc_array_layers_with_the_texture_decoder_limits() {
        let source: Vec<u8> = (0_u8..32)
            .map(|index| index.wrapping_mul(73).wrapping_add(19) ^ index.wrapping_mul(11))
            .collect();
        let object = texture_array_object(ArrayObject {
            version: "2022.3.62f1",
            width: 8,
            height: 8,
            depth: 1,
            graphics_format: 116,
            mip_count: 1,
            mips_stripped: 0,
            data_size: u32::try_from(source.len()).unwrap(),
            inline_data: &source,
            stream: None,
        });
        let file = parse_asset("2022.3.62f1", 13, &object);
        let collection = collection_with(file.clone(), "unused", b"");
        let texture =
            read_texture2d_array(&collection, &file, 0, TextureArrayReadLimits::default()).unwrap();

        assert_eq!(texture.texture_format, TextureFormat::PVRTC_RGBA4);
        let image = texture
            .decode_layer_mip0_rgba8(0, TextureArrayReadLimits::default())
            .unwrap();
        assert_eq!(fnv1a64(&image.pixels), 0xa95e_fecb_f7b9_90d5);
        assert_eq!(&image.pixels[..4], &[0xde, 0xc8, 0x8c, 0xd4]);

        let limits = TextureArrayReadLimits {
            maximum_decoder_working_bytes: 511,
            ..TextureArrayReadLimits::default()
        };
        assert!(matches!(
            texture.decode_layer_mip0_rgba8(0, limits),
            Err(Error::InvalidData(message)) if message.contains("working buffers")
        ));
    }

    #[test]
    fn parses_2019_through_2023_inline_arrays_and_slices_layers_before_mips() {
        let layer_0 = [
            1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 21, 22, 23, 24,
        ];
        let layer_1 = [
            31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 51, 52, 53, 54,
        ];
        let mut payload = layer_0.to_vec();
        payload.extend_from_slice(&layer_1);

        for version in ["2019.4.40f1", "2020.2.7f1", "2023.2.20f1"] {
            let object = texture_array_object(ArrayObject {
                version,
                width: 2,
                height: 2,
                depth: 2,
                graphics_format: 4,
                mip_count: 2,
                mips_stripped: 0,
                data_size: u32::try_from(payload.len()).unwrap(),
                inline_data: &payload,
                stream: None,
            });
            let file = parse_asset(version, 13, &object);
            let collection = collection_with(file.clone(), "unused.resS", b"");
            let texture =
                read_texture2d_array(&collection, &file, 0, TextureArrayReadLimits::default())
                    .unwrap();

            assert_eq!(texture.name, "array");
            assert_eq!((texture.width, texture.height, texture.depth), (2, 2, 2));
            assert_eq!(texture.texture_format, TextureFormat::RGBA32);
            assert_eq!(texture.mip_count, 2);
            assert_eq!(texture.mips_stripped, 0);
            assert_eq!(texture.usage_mode, (version >= "2020").then_some(7));
            assert_eq!(texture.layer_region(0).unwrap().len(), 20);
            assert_eq!(texture.layer_region(1).unwrap().len(), 20);
            assert_eq!(
                texture
                    .decode_layer_mip0_rgba8(0, TextureArrayReadLimits::default())
                    .unwrap()
                    .pixels,
                layer_0[..16]
            );
            assert_eq!(
                texture
                    .decode_layer_mip0_rgba8(1, TextureArrayReadLimits::default())
                    .unwrap()
                    .pixels,
                layer_1[..16]
            );
        }
    }

    #[test]
    fn resolves_2019_and_2020_streaming_info_as_bounded_regions() {
        for version in ["2019.4.40f1", "2020.2.7f1"] {
            let object = texture_array_object(ArrayObject {
                version,
                width: 1,
                height: 1,
                depth: 2,
                graphics_format: 4,
                mip_count: 1,
                mips_stripped: 0,
                data_size: 8,
                inline_data: b"",
                stream: Some((3, 8, "array.resS")),
            });
            let file = parse_asset(version, 13, &object);
            let collection = collection_with(
                file.clone(),
                "bundle/root/array.resS",
                &[90, 91, 92, 1, 2, 3, 4, 5, 6, 7, 8, 99],
            );
            let texture =
                read_texture2d_array(&collection, &file, 0, TextureArrayReadLimits::default())
                    .unwrap();

            let stream = texture.streaming_info.as_ref().unwrap();
            assert_eq!((stream.offset, stream.size), (3, 8));
            assert_eq!(stream.path, "array.resS");
            assert_eq!(
                texture.data.read_to_vec(8).unwrap(),
                [1, 2, 3, 4, 5, 6, 7, 8]
            );
            assert_eq!(
                texture
                    .decode_layer_mip0_rgba8(1, TextureArrayReadLimits::default())
                    .unwrap()
                    .pixels,
                [5, 6, 7, 8]
            );
        }
    }

    #[test]
    fn writes_exact_managed_payload_bundle_bytes_in_layer_order() {
        let source = [1, 2, 3, 4, 5, 6, 7, 8, 11, 12, 13, 14, 15, 16, 17, 18];
        let object = texture_array_object(ArrayObject {
            version: "2022.3.62f1",
            width: 1,
            height: 2,
            depth: 2,
            graphics_format: 4,
            mip_count: 1,
            mips_stripped: 0,
            data_size: u32::try_from(source.len()).unwrap(),
            inline_data: &source,
            stream: None,
        });
        let file = parse_asset("2022.3.62f1", 13, &object);
        let collection = collection_with(file.clone(), "unused", b"");
        let texture =
            read_texture2d_array(&collection, &file, 0, TextureArrayReadLimits::default()).unwrap();

        let mut actual = Vec::new();
        let written = write_texture2d_array_rgba_bundle(
            &texture,
            TextureArrayReadLimits::default(),
            &mut actual,
        )
        .unwrap();

        let mut expected = Vec::new();
        expected.extend_from_slice(PAYLOAD_BUNDLE_MAGIC);
        expected.extend_from_slice(&2_i32.to_le_bytes());
        for name in ["layer_0000.rgba", "layer_0001.rgba"] {
            expected.extend_from_slice(&15_i32.to_le_bytes());
            expected.extend_from_slice(&44_i64.to_le_bytes());
            expected.extend_from_slice(name.as_bytes());
        }
        expected.extend_from_slice(&rgba_ir(1, 2, &[5, 6, 7, 8, 1, 2, 3, 4]));
        expected.extend_from_slice(&rgba_ir(1, 2, &[15, 16, 17, 18, 11, 12, 13, 14]));

        assert_eq!(usize::try_from(written).unwrap(), actual.len());
        assert_eq!(actual, expected);
    }

    #[test]
    fn high_level_studio_decodes_layers_in_order_with_a_cumulative_limit() {
        let source = [1, 2, 3, 4, 5, 6, 7, 8, 11, 12, 13, 14, 15, 16, 17, 18];
        let object = texture_array_object(ArrayObject {
            version: "2022.3.62f1",
            width: 1,
            height: 2,
            depth: 2,
            graphics_format: 4,
            mip_count: 1,
            mips_stripped: 0,
            data_size: u32::try_from(source.len()).unwrap(),
            inline_data: &source,
            stream: None,
        });
        let file = parse_asset("2022.3.62f1", 13, &object);
        let studio = Studio::from_collection(collection_with(file, "unused", b""));
        let studio_object = studio.object(0, 7).unwrap();

        let images = studio_object
            .decode_texture_array_mip0(TextureArrayReadLimits::default())
            .unwrap();
        assert_eq!(images.len(), 2);
        assert_eq!((images[0].width, images[0].height), (1, 2));
        assert_eq!(images[0].pixels, source[..8]);
        assert_eq!(images[1].pixels, source[8..]);

        let limits = TextureArrayReadLimits {
            maximum_output_bytes: 15,
            ..TextureArrayReadLimits::default()
        };
        assert!(matches!(
            studio_object.decode_texture_array_mip0(limits),
            Err(Error::InvalidData(message)) if message.contains("output")
        ));
    }

    #[test]
    fn rejects_unknown_formats_and_stripped_mip_zero_but_decodes_switch_arrays_linearly() {
        let unknown = texture_array_object(ArrayObject {
            version: "2022.3.62f1",
            width: 1,
            height: 1,
            depth: 1,
            graphics_format: 999,
            mip_count: 1,
            mips_stripped: 0,
            data_size: 4,
            inline_data: &[0; 4],
            stream: None,
        });
        let file = parse_asset("2022.3.62f1", 13, &unknown);
        let collection = collection_with(file.clone(), "unused", b"");
        assert!(matches!(
            read_texture2d_array(
                &collection,
                &file,
                0,
                TextureArrayReadLimits::default()
            ),
            Err(Error::Unsupported(message)) if message.contains("GraphicsFormat 999")
        ));

        let supported = texture_array_object(ArrayObject {
            version: "2022.3.62f1",
            width: 1,
            height: 1,
            depth: 1,
            graphics_format: 4,
            mip_count: 1,
            mips_stripped: 0,
            data_size: 4,
            inline_data: &[0; 4],
            stream: None,
        });
        let file = parse_asset("2022.3.62f1", 38, &supported);
        let collection = collection_with(file.clone(), "unused", b"");
        let texture =
            read_texture2d_array(&collection, &file, 0, TextureArrayReadLimits::default()).unwrap();
        assert_eq!(
            texture
                .decode_layer_mip0_rgba8(0, TextureArrayReadLimits::default())
                .unwrap()
                .pixels,
            [0, 0, 0, 0]
        );

        let stripped = texture_array_object(ArrayObject {
            version: "2023.2.20f1",
            width: 1,
            height: 1,
            depth: 1,
            graphics_format: 4,
            mip_count: 2,
            mips_stripped: 1,
            data_size: 4,
            inline_data: &[0; 4],
            stream: None,
        });
        let file = parse_asset("2023.2.20f1", 13, &stripped);
        let collection = collection_with(file.clone(), "unused", b"");
        let texture =
            read_texture2d_array(&collection, &file, 0, TextureArrayReadLimits::default()).unwrap();
        assert!(matches!(
            texture.decode_layer_mip0_rgba8(0, TextureArrayReadLimits::default()),
            Err(Error::Unsupported(message)) if message.contains("stripped mip")
        ));
    }

    #[test]
    fn enforces_depth_payload_output_bundle_and_layer_bounds() {
        let source = [1, 2, 3, 4, 5, 6, 7, 8];
        let object = texture_array_object(ArrayObject {
            version: "2022.3.62f1",
            width: 1,
            height: 1,
            depth: 2,
            graphics_format: 4,
            mip_count: 1,
            mips_stripped: 0,
            data_size: 8,
            inline_data: &source,
            stream: None,
        });
        let file = parse_asset("2022.3.62f1", 13, &object);
        let collection = collection_with(file.clone(), "unused", b"");

        for limits in [
            TextureArrayReadLimits {
                maximum_depth: 1,
                ..TextureArrayReadLimits::default()
            },
            TextureArrayReadLimits {
                maximum_payload_bytes: 7,
                ..TextureArrayReadLimits::default()
            },
        ] {
            assert!(read_texture2d_array(&collection, &file, 0, limits).is_err());
        }

        let texture =
            read_texture2d_array(&collection, &file, 0, TextureArrayReadLimits::default()).unwrap();
        assert!(texture.layer_region(2).is_err());
        let output_limits = TextureArrayReadLimits {
            maximum_output_bytes: 7,
            ..TextureArrayReadLimits::default()
        };
        assert!(texture.decode_mip0_layers_rgba8(output_limits).is_err());

        let mut unmodified = Vec::new();
        let bundle_limits = TextureArrayReadLimits {
            maximum_bundle_bytes: 1,
            ..TextureArrayReadLimits::default()
        };
        assert!(
            write_texture2d_array_rgba_bundle(&texture, bundle_limits, &mut unmodified).is_err()
        );
        assert!(unmodified.is_empty());

        let indivisible = texture_array_object(ArrayObject {
            data_size: 7,
            inline_data: &source[..7],
            ..ArrayObject::rgba32("2022.3.62f1", 1, 1, 2)
        });
        let file = parse_asset("2022.3.62f1", 13, &indivisible);
        let collection = collection_with(file.clone(), "unused", b"");
        assert!(
            read_texture2d_array(&collection, &file, 0, TextureArrayReadLimits::default()).is_err()
        );
    }

    #[test]
    fn rejects_pre_2019_layouts_explicitly() {
        let file = parse_asset("2018.4.36f1", 13, &[]);
        let collection = collection_with(file.clone(), "unused", b"");
        assert!(matches!(
            read_texture2d_array(
                &collection,
                &file,
                0,
                TextureArrayReadLimits::default()
            ),
            Err(Error::Unsupported(message)) if message.contains("before Unity 2019")
        ));
    }

    #[derive(Clone, Copy)]
    struct ArrayObject<'a> {
        version: &'a str,
        width: i32,
        height: i32,
        depth: i32,
        graphics_format: i32,
        mip_count: i32,
        mips_stripped: i32,
        data_size: u32,
        inline_data: &'a [u8],
        stream: Option<(u64, u32, &'a str)>,
    }

    impl<'a> ArrayObject<'a> {
        fn rgba32(version: &'a str, width: i32, height: i32, depth: i32) -> Self {
            Self {
                version,
                width,
                height,
                depth,
                graphics_format: 4,
                mip_count: 1,
                mips_stripped: 0,
                data_size: 0,
                inline_data: b"",
                stream: None,
            }
        }
    }

    fn texture_array_object(fixture: ArrayObject<'_>) -> Vec<u8> {
        let (major, minor) = version_major_minor(fixture.version);
        let mut output = Vec::new();
        push_aligned_string(&mut output, "array");
        if (major, minor) >= (2017, 3) {
            if (major, minor) < (2023, 2) {
                push_i32(&mut output, 0);
                output.push(0);
            }
            if (major, minor) >= (2020, 2) {
                output.push(0);
            }
            align(&mut output, 4);
        }
        push_i32(&mut output, 0);
        push_i32(&mut output, fixture.graphics_format);
        push_i32(&mut output, fixture.width);
        push_i32(&mut output, fixture.height);
        push_i32(&mut output, fixture.depth);
        push_i32(&mut output, fixture.mip_count);
        if (major, minor) >= (2023, 2) {
            push_i32(&mut output, fixture.mips_stripped);
        }
        output.extend_from_slice(&fixture.data_size.to_le_bytes());
        output.extend_from_slice(&[0; 24]);
        if (major, minor) >= (2020, 2) {
            push_i32(&mut output, 7);
        }
        output.push(1);
        if (major, minor) > (2023, 2) {
            output.push(0);
            align(&mut output, 4);
            push_aligned_string(&mut output, "group");
        } else {
            align(&mut output, 4);
        }
        if let Some((offset, size, path)) = fixture.stream {
            push_i32(&mut output, 0);
            if major >= 2020 {
                output.extend_from_slice(&i64::try_from(offset).unwrap().to_le_bytes());
            } else {
                output.extend_from_slice(&u32::try_from(offset).unwrap().to_le_bytes());
            }
            output.extend_from_slice(&size.to_le_bytes());
            push_aligned_string(&mut output, path);
        } else {
            push_i32(
                &mut output,
                i32::try_from(fixture.inline_data.len()).unwrap(),
            );
            output.extend_from_slice(fixture.inline_data);
        }
        output
    }

    fn rgba_ir(width: i32, height: i32, display_pixels: &[u8]) -> Vec<u8> {
        let mut output = Vec::new();
        output.extend_from_slice(b"HARUKI_RGBAIR_V1");
        output.extend_from_slice(&width.to_le_bytes());
        output.extend_from_slice(&height.to_le_bytes());
        output.extend_from_slice(&(width * 4).to_le_bytes());
        output.extend_from_slice(&1_i32.to_le_bytes());
        output.extend_from_slice(&0_i32.to_le_bytes());
        output.extend_from_slice(display_pixels);
        output
    }

    fn fnv1a64(bytes: &[u8]) -> u64 {
        bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
        })
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

    fn parse_asset(version: &str, target_platform: i32, object: &[u8]) -> SerializedFile {
        SerializedFile::open(Region::from_bytes(synthetic_v22_asset(
            version,
            target_platform,
            object,
        )))
        .unwrap()
    }

    fn synthetic_v22_asset(version: &str, target_platform: i32, object: &[u8]) -> Vec<u8> {
        let mut metadata = Vec::new();
        metadata.extend_from_slice(version.as_bytes());
        metadata.push(0);
        push_i32(&mut metadata, target_platform);
        metadata.push(0);
        push_i32(&mut metadata, 1);
        push_i32(&mut metadata, TEXTURE_2D_ARRAY_CLASS_ID);
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

    fn version_major_minor(version: &str) -> (u32, u32) {
        let mut numbers = version
            .split(|character: char| !character.is_ascii_digit())
            .filter(|part| !part.is_empty());
        (
            numbers.next().unwrap().parse().unwrap(),
            numbers.next().unwrap().parse().unwrap(),
        )
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
