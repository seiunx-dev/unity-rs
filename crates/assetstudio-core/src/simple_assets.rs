use crate::audio::detect_direct_wav;
pub use crate::audio::{DirectWavKind, direct_wav_output_size, write_direct_wav};
use crate::endian::{Endian, EndianReader, checked_length};
use crate::loader::AssetCollection;
use crate::serialized::SerializedFile;
use crate::source::{Region, RegionCursor};
use crate::{Error, Result};

pub const AUDIO_CLIP_CLASS_ID: i32 = 83;
pub const FONT_CLASS_ID: i32 = 128;
pub const MOVIE_TEXTURE_CLASS_ID: i32 = 152;
pub const VIDEO_CLIP_CLASS_ID: i32 = 329;

const NO_TARGET_PLATFORM: i32 = -2;

/// Defensive limits for the small, byte-oriented asset readers in this module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SimpleAssetReadLimits {
    pub maximum_string_bytes: usize,
    pub maximum_array_elements: usize,
    pub maximum_payload_bytes: u64,
}

impl Default for SimpleAssetReadLimits {
    fn default() -> Self {
        Self {
            maximum_string_bytes: 16 * 1024 * 1024,
            maximum_array_elements: 1_000_000,
            maximum_payload_bytes: 512 * 1024 * 1024,
        }
    }
}

/// An export-ready binary asset whose bytes remain bound to their source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimpleBinaryAsset {
    pub path_id: i64,
    pub name: String,
    pub payload: Region,
    pub payload_kind: &'static str,
    pub suggested_extension: String,
}

/// A parsed `AudioClip` retaining enough metadata for verified direct WAV output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioClipAsset {
    pub path_id: i64,
    pub name: String,
    pub payload: Region,
    pub raw_extension: String,
    pub direct_wav: Option<DirectWavKind>,
}

impl AudioClipAsset {
    fn into_raw(self) -> SimpleBinaryAsset {
        Self::into_binary_parts(self, "audio_raw")
    }

    fn into_binary_parts(self, payload_kind: &'static str) -> SimpleBinaryAsset {
        SimpleBinaryAsset {
            path_id: self.path_id,
            name: self.name,
            payload: self.payload,
            payload_kind,
            suggested_extension: self.raw_extension,
        }
    }
}

/// Reads a Unity `Font` and returns its embedded font program without copying it.
pub fn read_font(
    file: &SerializedFile,
    object_index: usize,
    limits: SimpleAssetReadLimits,
) -> Result<SimpleBinaryAsset> {
    require_known_unity_version(file, "Font")?;
    let mut reader = ObjectPayloadReader::new(file, object_index, FONT_CLASS_ID, limits)?;
    let name = reader.read_named_object()?;
    let version = file.unity_version.components();

    if version >= (5, 5, 0) {
        reader.skip(4, "Font line spacing")?;
        reader.read_pptr()?;
        reader.skip(4, "Font size")?;
        reader.read_pptr()?;
        reader.skip(20, "Font scalar fields")?;
        reader.skip_counted_records(44, "Font character rectangles")?;
        reader.skip_counted_records(8, "Font kerning values")?;
        reader.skip(4, "Font pixel scale")?;
    } else {
        reader.skip(4, "Font ASCII start offset")?;
        if file.unity_version.major <= 3 {
            reader.skip(8, "Font grid dimensions")?;
        }
        reader.skip(8, "Font kerning and line spacing")?;
        if file.unity_version.major <= 3 {
            reader.skip_counted_records(8, "Font per-character kerning")?;
        } else {
            reader.skip(8, "Font character spacing and padding")?;
        }
        reader.skip(4, "Font case conversion")?;
        reader.read_pptr()?;

        let character_count = reader.read_count("Font character rectangle")?;
        for _ in 0..character_count {
            reader.skip(40, "Font character rectangle")?;
            if file.unity_version.major >= 4 {
                reader.skip(1, "Font flipped flag")?;
                reader.align(4)?;
            }
        }

        reader.read_pptr()?;
        reader.skip_counted_records(8, "Font kerning values")?;
        if file.unity_version.major <= 3 {
            reader.skip(1, "Font grid flag")?;
            reader.align(4)?;
        } else {
            reader.skip(4, "Font pixel scale")?;
        }
    }

    let payload = reader.read_counted_payload("Font data")?;
    let extension = if payload.len() >= 4 {
        let mut signature = [0_u8; 4];
        payload.read_exact_at(0, &mut signature)?;
        if signature == *b"OTTO" {
            ".otf"
        } else {
            ".ttf"
        }
    } else {
        ".ttf"
    };

    Ok(SimpleBinaryAsset {
        path_id: reader.path_id,
        name,
        payload,
        payload_kind: "font",
        suggested_extension: copy_simple_string(extension, "Font extension")?,
    })
}

/// Reads the legacy, resident Ogg payload used by `MovieTexture`.
pub fn read_movie_texture(
    file: &SerializedFile,
    object_index: usize,
    limits: SimpleAssetReadLimits,
) -> Result<SimpleBinaryAsset> {
    require_known_unity_version(file, "MovieTexture")?;
    if file.unity_version.components() >= (2019, 3, 0) {
        return Err(Error::unsupported(
            "MovieTexture data at Unity 2019.3 or newer (the serialized class no longer carries m_MovieData)",
        ));
    }

    let mut reader = ObjectPayloadReader::new(file, object_index, MOVIE_TEXTURE_CLASS_ID, limits)?;
    let name = reader.read_named_object()?;
    if file.unity_version.components() >= (2017, 3, 0) {
        if file.unity_version.components() < (2023, 2, 0) {
            reader.skip(5, "Texture fallback settings")?;
        }
        if file.unity_version.components() >= (2020, 2, 0) {
            reader.skip(1, "Texture alpha-channel setting")?;
        }
        reader.align(4)?;
    }
    reader.skip(1, "MovieTexture loop flag")?;
    reader.align(4)?;
    reader.read_pptr()?;
    let payload = reader.read_counted_payload("MovieTexture data")?;

    Ok(SimpleBinaryAsset {
        path_id: reader.path_id,
        name,
        payload,
        payload_kind: "movie_ogv",
        suggested_extension: copy_simple_string(".ogv", "MovieTexture extension")?,
    })
}

/// Reads an `AudioClip`, resolving legacy and Unity 5+ streamed resources.
pub fn read_audio_clip(
    collection: &AssetCollection,
    file: &SerializedFile,
    object_index: usize,
    limits: SimpleAssetReadLimits,
) -> Result<SimpleBinaryAsset> {
    Ok(read_audio_clip_asset(collection, file, object_index, limits)?.into_raw())
}

/// Reads an `AudioClip` and retains metadata for pure-Rust WAV export.
pub fn read_audio_clip_asset(
    collection: &AssetCollection,
    file: &SerializedFile,
    object_index: usize,
    limits: SimpleAssetReadLimits,
) -> Result<AudioClipAsset> {
    require_known_unity_version(file, "AudioClip")?;
    let mut reader = ObjectPayloadReader::new(file, object_index, AUDIO_CLIP_CLASS_ID, limits)?;
    let name = reader.read_named_object()?;
    if file.unity_version.major < 5 {
        return read_legacy_audio_clip(collection, file, reader, name, limits);
    }
    reader.skip(12, "AudioClip load, channel, and frequency settings")?;
    let decoded_bits = reader.reader.read_i32()?;
    reader.skip(4, "AudioClip duration")?;
    reader.skip(1, "AudioClip tracker flag")?;
    reader.align(4)?;
    reader.skip(4, "AudioClip subsound index")?;
    reader.skip(3, "AudioClip preload flags")?;
    reader.align(4)?;
    let mut resource = reader.read_streamed_resource("AudioClip resource")?;
    let compression_format = reader.reader.read_i32()?;
    resource.inline_offset = reader.reader.position()?;
    let payload = resource.resolve(collection, limits.maximum_payload_bytes)?;

    build_audio_clip_asset(
        reader.path_id,
        name,
        payload,
        audio_extension(compression_format),
        None,
        u16::try_from(decoded_bits).ok(),
    )
}

fn read_legacy_audio_clip(
    collection: &AssetCollection,
    file: &SerializedFile,
    mut reader: ObjectPayloadReader,
    name: String,
    limits: SimpleAssetReadLimits,
) -> Result<AudioClipAsset> {
    let format = reader.reader.read_i32()?;
    let (sound_type, legacy_pcm) = if file.unity_version.components() >= (2, 6, 0) {
        let sound_type = reader.reader.read_i32()?;
        reader.skip(2, "legacy AudioClip 3D and hardware flags")?;
        reader.align(4)?;
        (sound_type, None)
    } else {
        reader.skip(4, "legacy AudioClip duration")?;
        let frequency = reader.reader.read_i32()?;
        let channels = if format == 0x05 { 0 } else { format >> 1 };
        (0, Some((channels, frequency)))
    };

    let resource = if file.unity_version.components() >= (3, 2, 0) {
        reader.skip(4, "legacy AudioClip stream mode")?;
        let size = non_negative_i32(reader.reader.read_i32()?, "legacy AudioClip size")?;
        let padded_size = size
            .checked_add(3)
            .map(|value| value / 4 * 4)
            .ok_or_else(|| Error::invalid_data("legacy AudioClip padded size overflowed"))?;
        if reader.reader.remaining()? == padded_size {
            inline_streamed_resource(&mut reader, size)?
        } else {
            let offset = u64::from(reader.reader.read_u32()?);
            StreamedResource {
                source: legacy_audio_resource_name(collection, file, limits.maximum_string_bytes)?,
                offset,
                size,
                inline_region: reader.region.clone(),
                inline_offset: reader.reader.position()?,
            }
        }
    } else {
        let size = non_negative_i32(reader.reader.read_i32()?, "legacy AudioClip size")?;
        inline_streamed_resource(&mut reader, size)?
    };
    let payload = resource.resolve(collection, limits.maximum_payload_bytes)?;
    let direct_wav = legacy_pcm
        .map(|(channels, frequency)| legacy_pcm_kind(channels, frequency))
        .transpose()?;
    build_audio_clip_asset(
        reader.path_id,
        name,
        payload,
        legacy_audio_extension(sound_type),
        direct_wav,
        None,
    )
}

fn build_audio_clip_asset(
    path_id: i64,
    name: String,
    payload: Region,
    raw_extension: &str,
    direct_wav: Option<DirectWavKind>,
    decoded_bits: Option<u16>,
) -> Result<AudioClipAsset> {
    let direct_wav = detect_direct_wav(&payload, decoded_bits)?.or(direct_wav);
    Ok(AudioClipAsset {
        path_id,
        name,
        payload,
        raw_extension: copy_simple_string(raw_extension, "AudioClip extension")?,
        direct_wav,
    })
}

fn legacy_pcm_kind(channels: i32, frequency: i32) -> Result<DirectWavKind> {
    let channels = u16::try_from(channels).map_err(|_| {
        Error::invalid_data(format!(
            "legacy raw AudioClip channel count is invalid: {channels}"
        ))
    })?;
    if channels == 0 {
        return Err(Error::invalid_data(
            "legacy raw AudioClip channel count cannot be zero",
        ));
    }
    let sample_rate = u32::try_from(frequency).map_err(|_| {
        Error::invalid_data(format!(
            "legacy raw AudioClip sample rate is invalid: {frequency}"
        ))
    })?;
    if sample_rate == 0 {
        return Err(Error::invalid_data(
            "legacy raw AudioClip sample rate cannot be zero",
        ));
    }
    Ok(DirectWavKind::LegacyPcm16 {
        channels,
        sample_rate,
    })
}

fn inline_streamed_resource(
    reader: &mut ObjectPayloadReader,
    size: u64,
) -> Result<StreamedResource> {
    Ok(StreamedResource {
        source: String::new(),
        offset: 0,
        size,
        inline_region: reader.region.clone(),
        inline_offset: reader.reader.position()?,
    })
}

fn legacy_audio_resource_name(
    collection: &AssetCollection,
    file: &SerializedFile,
    maximum_string_bytes: usize,
) -> Result<String> {
    let path = collection
        .serialized_files
        .iter()
        .find(|loaded| std::ptr::eq(std::ptr::from_ref(&loaded.file), std::ptr::from_ref(file)))
        .map(|loaded| loaded.path.as_str())
        .ok_or_else(|| {
            Error::invalid_data(
                "legacy streamed AudioClip source file is not part of the asset collection",
            )
        })?;
    append_simple_suffix(
        path,
        ".resS",
        maximum_string_bytes,
        "legacy AudioClip resource name",
    )
}

/// Reads a `VideoClip`, resolving its `StreamedResource` when needed.
pub fn read_video_clip(
    collection: &AssetCollection,
    file: &SerializedFile,
    object_index: usize,
    limits: SimpleAssetReadLimits,
) -> Result<SimpleBinaryAsset> {
    require_known_unity_version(file, "VideoClip")?;
    let mut reader = ObjectPayloadReader::new(file, object_index, VIDEO_CLIP_CLASS_ID, limits)?;
    let name = reader.read_named_object()?;
    let original_path = reader.read_aligned_string("VideoClip original path")?;
    reader.skip(16, "VideoClip dimensions")?;
    if file.unity_version.components() >= (2017, 2, 0) {
        reader.skip(8, "VideoClip pixel aspect ratio")?;
    }
    reader.skip(20, "VideoClip frame and format fields")?;
    reader.skip_counted_elements(2, "VideoClip audio channel counts")?;
    reader.align(4)?;
    reader.skip_counted_elements(4, "VideoClip audio sample rates")?;
    let language_count = reader.read_count("VideoClip audio language")?;
    for _ in 0..language_count {
        reader.read_aligned_string("VideoClip audio language")?;
    }
    if file.unity_version.major >= 2020 {
        let shader_count = reader.read_count("VideoClip shader")?;
        for _ in 0..shader_count {
            reader.read_pptr()?;
        }
    }
    let mut resource = reader.read_streamed_resource("VideoClip resource")?;
    reader.skip(1, "VideoClip split-alpha flag")?;
    if file.unity_version.major >= 2020 {
        reader.skip(1, "VideoClip sRGB flag")?;
    }
    resource.inline_offset = reader.reader.position()?;
    let payload = resource.resolve(collection, limits.maximum_payload_bytes)?;

    Ok(SimpleBinaryAsset {
        path_id: reader.path_id,
        name,
        payload,
        payload_kind: "video_raw",
        suggested_extension: portable_extension(&original_path)?,
    })
}

#[derive(Debug, Clone)]
struct StreamedResource {
    source: String,
    offset: u64,
    size: u64,
    inline_region: Region,
    inline_offset: u64,
}

impl StreamedResource {
    fn resolve(&self, collection: &AssetCollection, maximum_payload_bytes: u64) -> Result<Region> {
        if self.size > maximum_payload_bytes {
            return Err(Error::invalid_data(format!(
                "streamed resource is {} bytes, exceeding limit {maximum_payload_bytes}",
                self.size
            )));
        }
        if self.source.is_empty() {
            return self.inline_region.subregion(self.inline_offset, self.size);
        }
        let resource = collection.resource(&self.source).ok_or_else(|| {
            Error::invalid_data(format!("external resource was not found: {}", self.source))
        })?;
        resource.region.subregion(self.offset, self.size)
    }
}

struct ObjectPayloadReader {
    region: Region,
    reader: EndianReader<RegionCursor>,
    absolute_start: u64,
    path_id: i64,
    target_platform: i32,
    format_version: u32,
    limits: SimpleAssetReadLimits,
}

impl ObjectPayloadReader {
    fn new(
        file: &SerializedFile,
        object_index: usize,
        expected_class_id: i32,
        limits: SimpleAssetReadLimits,
    ) -> Result<Self> {
        let object = file.objects.get(object_index).ok_or_else(|| {
            Error::invalid_data(format!(
                "serialized object index {object_index} is out of range"
            ))
        })?;
        if object.class_id != expected_class_id {
            return Err(Error::unsupported(format!(
                "object {} has class ID {}, expected {expected_class_id}",
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
            limits,
        })
    }

    fn read_named_object(&mut self) -> Result<String> {
        if self.target_platform == NO_TARGET_PLATFORM {
            self.skip(4, "Object hide flags")?;
            self.read_pptr()?;
            self.read_pptr()?;
        }
        self.read_aligned_string("asset name")
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

    fn read_count(&mut self, field: &str) -> Result<usize> {
        let count = checked_length(self.reader.read_i32()?, field)?;
        if count > self.limits.maximum_array_elements {
            return Err(Error::invalid_data(format!(
                "{field} count {count} exceeds limit {}",
                self.limits.maximum_array_elements
            )));
        }
        Ok(count)
    }

    fn skip_counted_records(&mut self, record_size: u64, field: &str) -> Result<()> {
        let count = self.read_count(field)?;
        self.skip_elements(count, record_size, field)
    }

    fn skip_counted_elements(&mut self, element_size: u64, field: &str) -> Result<()> {
        let count = self.read_count(field)?;
        self.skip_elements(count, element_size, field)
    }

    fn skip_elements(&mut self, count: usize, element_size: u64, field: &str) -> Result<()> {
        let count = u64::try_from(count)
            .map_err(|_| Error::invalid_data(format!("{field} count does not fit in u64")))?;
        let length = count
            .checked_mul(element_size)
            .ok_or_else(|| Error::invalid_data(format!("{field} byte size overflowed")))?;
        self.skip(length, field)
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

    fn read_counted_payload(&mut self, field: &str) -> Result<Region> {
        let length = checked_length(self.reader.read_i32()?, field)?;
        let length = u64::try_from(length)
            .map_err(|_| Error::invalid_data(format!("{field} length does not fit in u64")))?;
        self.payload_region(length, field)
    }

    fn payload_region(&mut self, length: u64, field: &str) -> Result<Region> {
        if length > self.limits.maximum_payload_bytes {
            return Err(Error::invalid_data(format!(
                "{field} is {length} bytes, exceeding limit {}",
                self.limits.maximum_payload_bytes
            )));
        }
        self.region.subregion(self.reader.position()?, length)
    }

    fn read_streamed_resource(&mut self, field: &str) -> Result<StreamedResource> {
        let source = self.read_aligned_string(&format!("{field} source"))?;
        let offset = non_negative_i64(self.reader.read_i64()?, &format!("{field} offset"))?;
        let size = non_negative_i64(self.reader.read_i64()?, &format!("{field} size"))?;
        Ok(StreamedResource {
            source,
            offset,
            size,
            inline_region: self.region.clone(),
            inline_offset: self.reader.position()?,
        })
    }
}

fn non_negative_i64(value: i64, field: &str) -> Result<u64> {
    u64::try_from(value)
        .map_err(|_| Error::invalid_data(format!("{field} cannot be negative: {value}")))
}

fn non_negative_i32(value: i32, field: &str) -> Result<u64> {
    u64::try_from(value)
        .map_err(|_| Error::invalid_data(format!("{field} cannot be negative: {value}")))
}

fn require_known_unity_version(file: &SerializedFile, asset: &str) -> Result<()> {
    if file.unity_version.is_stripped() {
        return Err(Error::unsupported(format!(
            "{asset} requires a Unity version because its layout is version-dependent"
        )));
    }
    Ok(())
}

fn audio_extension(compression_format: i32) -> &'static str {
    match compression_format {
        7 => ".m4a",
        0..=6 | 8 | 9 => ".fsb",
        _ => ".AudioClip",
    }
}

fn legacy_audio_extension(sound_type: i32) -> &'static str {
    match sound_type {
        1 => ".m4a",
        2 => ".aif",
        10 => ".it",
        12 => ".mod",
        13 => ".mp3",
        14 => ".ogg",
        17 => ".s3m",
        20 | 22 => ".wav",
        21 => ".xm",
        23 => ".vag",
        24 => ".fsb",
        _ => ".AudioClip",
    }
}

fn portable_extension(path: &str) -> Result<String> {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let extension = name
        .rfind('.')
        .filter(|index| *index + 1 < name.len())
        .map_or(".video", |index| &name[index..]);
    copy_simple_string(extension, "VideoClip extension")
}

fn copy_simple_string(value: &str, field: &str) -> Result<String> {
    let mut copied = String::new();
    copied
        .try_reserve_exact(value.len())
        .map_err(|error| Error::invalid_data(format!("cannot allocate {field}: {error}")))?;
    copied.push_str(value);
    Ok(copied)
}

fn append_simple_suffix(
    value: &str,
    suffix: &str,
    maximum_bytes: usize,
    field: &str,
) -> Result<String> {
    let length = value
        .len()
        .checked_add(suffix.len())
        .ok_or_else(|| Error::invalid_data(format!("{field} length overflowed")))?;
    if length > maximum_bytes {
        return Err(Error::invalid_data(format!(
            "{field} is {length} bytes, exceeding limit {maximum_bytes}"
        )));
    }
    let mut output = String::new();
    output
        .try_reserve_exact(length)
        .map_err(|error| Error::invalid_data(format!("cannot allocate {field}: {error}")))?;
    output.push_str(value);
    output.push_str(suffix);
    Ok(output)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::audio::PcmSampleFormat;
    use crate::export::{AudioExportFormat, ExportOptions, export_collection};
    use crate::loader::{AssetCollection, LoadedResource, LoadedSerializedFile};
    use crate::serialized::SerializedFile;
    use crate::source::Region;

    use super::{
        AUDIO_CLIP_CLASS_ID, DirectWavKind, FONT_CLASS_ID, MOVIE_TEXTURE_CLASS_ID,
        SimpleAssetReadLimits, VIDEO_CLIP_CLASS_ID, read_audio_clip, read_audio_clip_asset,
        read_font, read_movie_texture, read_video_clip, write_direct_wav,
    };

    const FSB5_VORBIS_FIXTURE: &[u8] =
        include_bytes!("../tests/fixtures/audio/fsb5-vorbis-stereo.fsb");

    #[test]
    fn reads_font_payload_and_detects_opentype() {
        let mut object = named_object("heading");
        object.extend_from_slice(&0_f32.to_le_bytes());
        push_pptr(&mut object);
        object.extend_from_slice(&12_f32.to_le_bytes());
        push_pptr(&mut object);
        object.extend_from_slice(&[0_u8; 20]);
        push_i32(&mut object, 0);
        push_i32(&mut object, 0);
        object.extend_from_slice(&1_f32.to_le_bytes());
        push_i32(&mut object, 7);
        object.extend_from_slice(b"OTTOabc");
        let file = parse_asset(FONT_CLASS_ID, "2022.3.62f1", &object);

        let font = read_font(&file, 0, SimpleAssetReadLimits::default()).unwrap();

        assert_eq!(font.name, "heading");
        assert_eq!(font.suggested_extension, ".otf");
        assert_eq!(font.payload.read_to_vec(32).unwrap(), b"OTTOabc");
    }

    #[test]
    fn reads_legacy_movie_texture_payload() {
        let mut object = named_object("intro");
        object.extend_from_slice(&0_i32.to_le_bytes());
        object.push(0);
        align(&mut object, 4);
        object.push(1);
        align(&mut object, 4);
        push_pptr(&mut object);
        push_i32(&mut object, 4);
        object.extend_from_slice(b"OggS");
        let file = parse_asset(MOVIE_TEXTURE_CLASS_ID, "2018.4.36f1", &object);

        let movie = read_movie_texture(&file, 0, SimpleAssetReadLimits::default()).unwrap();

        assert_eq!(movie.name, "intro");
        assert_eq!(movie.suggested_extension, ".ogv");
        assert_eq!(movie.payload.read_to_vec(32).unwrap(), b"OggS");
    }

    #[test]
    fn resolves_external_audio_and_enforces_payload_limit() {
        let mut object = named_object("theme");
        object.extend_from_slice(&[0_u8; 20]);
        object.push(0);
        align(&mut object, 4);
        object.extend_from_slice(&0_i32.to_le_bytes());
        object.extend_from_slice(&[0_u8; 3]);
        align(&mut object, 4);
        push_aligned_string(&mut object, "archive:/sound.resS");
        object.extend_from_slice(&2_i64.to_le_bytes());
        object.extend_from_slice(&3_i64.to_le_bytes());
        object.extend_from_slice(&1_i32.to_le_bytes());
        let file = parse_asset(AUDIO_CLIP_CLASS_ID, "2022.3.62f1", &object);
        let collection = collection_with(file.clone(), "sound.resS", b"xxabczz");

        let audio =
            read_audio_clip(&collection, &file, 0, SimpleAssetReadLimits::default()).unwrap();

        assert_eq!(audio.name, "theme");
        assert_eq!(audio.suggested_extension, ".fsb");
        assert_eq!(audio.payload.read_to_vec(32).unwrap(), b"abc");

        let limits = SimpleAssetReadLimits {
            maximum_payload_bytes: 2,
            ..SimpleAssetReadLimits::default()
        };
        assert!(read_audio_clip(&collection, &file, 0, limits).is_err());
    }

    #[test]
    fn reads_legacy_inline_and_external_audio_without_fmod_conversion() {
        let mut inline = named_object("legacy-inline");
        inline.extend_from_slice(&0_i32.to_le_bytes());
        inline.extend_from_slice(&14_i32.to_le_bytes());
        inline.extend_from_slice(&[1, 0]);
        align(&mut inline, 4);
        inline.extend_from_slice(&0_i32.to_le_bytes());
        inline.extend_from_slice(&4_i32.to_le_bytes());
        inline.extend_from_slice(b"OggS");
        let inline_file = parse_asset(AUDIO_CLIP_CLASS_ID, "4.7.2f1", &inline);
        let inline_collection = collection_with(inline_file.clone(), "unused.resS", b"");

        let audio = read_audio_clip(
            &inline_collection,
            &inline_file,
            0,
            SimpleAssetReadLimits::default(),
        )
        .unwrap();
        assert_eq!(audio.suggested_extension, ".ogg");
        assert_eq!(audio.payload.read_to_vec(16).unwrap(), b"OggS");

        let mut external = named_object("legacy-stream");
        external.extend_from_slice(&0_i32.to_le_bytes());
        external.extend_from_slice(&13_i32.to_le_bytes());
        external.extend_from_slice(&[0, 0]);
        align(&mut external, 4);
        external.extend_from_slice(&1_i32.to_le_bytes());
        external.extend_from_slice(&5_i32.to_le_bytes());
        external.extend_from_slice(&2_u32.to_le_bytes());
        let external_file = parse_asset(AUDIO_CLIP_CLASS_ID, "4.7.2f1", &external);
        let collection = AssetCollection::from_loaded_parts(
            vec![LoadedSerializedFile {
                path: "legacy.assets".to_owned(),
                file: external_file,
            }],
            vec![LoadedResource {
                path: "legacy.assets.resS".to_owned(),
                region: Region::from_bytes(b"xxmp3!!zz".to_vec()),
            }],
        );
        let file = &collection.serialized_files[0].file;

        let audio =
            read_audio_clip(&collection, file, 0, SimpleAssetReadLimits::default()).unwrap();
        assert_eq!(audio.suggested_extension, ".mp3");
        assert_eq!(audio.payload.read_to_vec(16).unwrap(), b"mp3!!");

        let error = read_audio_clip(
            &collection,
            file,
            0,
            SimpleAssetReadLimits {
                // `legacy-stream` itself fits exactly. The collection path
                // plus `.resS` does not, so the derived lookup string must be
                // rejected before it allocates rather than bypassing this
                // caller budget.
                maximum_string_bytes: "legacy-stream".len(),
                ..SimpleAssetReadLimits::default()
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("legacy AudioClip resource name"));
    }

    #[test]
    fn writes_legacy_raw_pcm16_with_the_managed_wav_header() {
        let mut object = named_object("legacy-pcm");
        object.extend_from_slice(&4_i32.to_le_bytes());
        object.extend_from_slice(&0_f32.to_le_bytes());
        object.extend_from_slice(&44_100_i32.to_le_bytes());
        object.extend_from_slice(&8_i32.to_le_bytes());
        object.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        let file = parse_asset(AUDIO_CLIP_CLASS_ID, "2.5.0f1", &object);
        let collection = collection_with(file.clone(), "unused.resS", b"");

        let audio =
            read_audio_clip_asset(&collection, &file, 0, SimpleAssetReadLimits::default()).unwrap();
        assert_eq!(
            audio.direct_wav,
            Some(DirectWavKind::LegacyPcm16 {
                channels: 2,
                sample_rate: 44_100,
            })
        );

        let mut wav = Vec::new();
        let written =
            write_direct_wav(&audio.payload, audio.direct_wav.unwrap(), 52, &mut wav).unwrap();
        assert_eq!(written, 52);
        assert_eq!(&wav[..12], b"RIFF,\0\0\0WAVE");
        assert_eq!(&wav[12..20], b"fmt \x10\0\0\0");
        assert_eq!(&wav[20..24], &[1, 0, 2, 0]);
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 44_100);
        assert_eq!(u32::from_le_bytes(wav[28..32].try_into().unwrap()), 176_400);
        assert_eq!(&wav[32..36], &[4, 0, 16, 0]);
        assert_eq!(&wav[36..44], b"data\x08\0\0\0");
        assert_eq!(&wav[44..], &[1, 2, 3, 4, 5, 6, 7, 8]);

        assert!(
            write_direct_wav(
                &audio.payload,
                audio.direct_wav.unwrap(),
                51,
                &mut Vec::new(),
            )
            .is_err()
        );
    }

    #[test]
    fn streams_existing_wave_and_rejects_false_or_partial_direct_wav_claims() {
        let mut object = named_object("wave");
        object.extend_from_slice(&0_i32.to_le_bytes());
        object.extend_from_slice(&20_i32.to_le_bytes());
        object.extend_from_slice(&[0, 0]);
        align(&mut object, 4);
        object.extend_from_slice(&0_i32.to_le_bytes());
        object.extend_from_slice(&12_i32.to_le_bytes());
        object.extend_from_slice(b"RIFF\x04\0\0\0WAVE");
        let file = parse_asset(AUDIO_CLIP_CLASS_ID, "4.7.2f1", &object);
        let collection = collection_with(file.clone(), "unused.resS", b"");
        let audio =
            read_audio_clip_asset(&collection, &file, 0, SimpleAssetReadLimits::default()).unwrap();

        assert_eq!(audio.direct_wav, Some(DirectWavKind::ExistingWave));
        let mut output = Vec::new();
        assert_eq!(
            write_direct_wav(&audio.payload, DirectWavKind::ExistingWave, 12, &mut output,)
                .unwrap(),
            12
        );
        assert_eq!(output, b"RIFF\x04\0\0\0WAVE");
        assert!(
            write_direct_wav(
                &Region::from_bytes(b"not a wave".to_vec()),
                DirectWavKind::ExistingWave,
                100,
                &mut Vec::new(),
            )
            .is_err()
        );
        assert!(
            write_direct_wav(
                &Region::from_bytes(vec![0; 3]),
                DirectWavKind::LegacyPcm16 {
                    channels: 2,
                    sample_rate: 44_100,
                },
                100,
                &mut Vec::new(),
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_negative_and_over_limit_legacy_audio_sizes() {
        let mut negative = named_object("bad");
        negative.extend_from_slice(&0_i32.to_le_bytes());
        negative.extend_from_slice(&14_i32.to_le_bytes());
        negative.extend_from_slice(&[0, 0]);
        align(&mut negative, 4);
        negative.extend_from_slice(&0_i32.to_le_bytes());
        negative.extend_from_slice(&(-1_i32).to_le_bytes());
        let file = parse_asset(AUDIO_CLIP_CLASS_ID, "4.7.2f1", &negative);
        let collection = collection_with(file.clone(), "unused.resS", b"");
        assert!(read_audio_clip(&collection, &file, 0, SimpleAssetReadLimits::default()).is_err());

        let mut oversized = named_object("large");
        oversized.extend_from_slice(&0_i32.to_le_bytes());
        oversized.extend_from_slice(&20_i32.to_le_bytes());
        oversized.extend_from_slice(&[0, 0]);
        align(&mut oversized, 4);
        oversized.extend_from_slice(&0_i32.to_le_bytes());
        oversized.extend_from_slice(&4_i32.to_le_bytes());
        oversized.extend_from_slice(b"RIFF");
        let file = parse_asset(AUDIO_CLIP_CLASS_ID, "4.7.2f1", &oversized);
        let collection = collection_with(file.clone(), "unused.resS", b"");
        let limits = SimpleAssetReadLimits {
            maximum_payload_bytes: 3,
            ..SimpleAssetReadLimits::default()
        };
        assert!(read_audio_clip(&collection, &file, 0, limits).is_err());
    }

    #[test]
    fn reads_inline_video_without_materializing_its_payload() {
        let mut object = named_object("trailer");
        push_aligned_string(&mut object, "movies\\目录\\trailer.mp4");
        object.extend_from_slice(&[0_u8; 16]);
        object.extend_from_slice(&[0_u8; 8]);
        object.extend_from_slice(&[0_u8; 20]);
        push_i32(&mut object, 0);
        align(&mut object, 4);
        push_i32(&mut object, 0);
        push_i32(&mut object, 0);
        push_i32(&mut object, 0);
        push_aligned_string(&mut object, "");
        object.extend_from_slice(&0_i64.to_le_bytes());
        object.extend_from_slice(&4_i64.to_le_bytes());
        object.push(0);
        object.push(1);
        object.extend_from_slice(b"video");
        let file = parse_asset(VIDEO_CLIP_CLASS_ID, "2022.3.62f1", &object);
        let collection = collection_with(file.clone(), "unused.resS", b"");

        let video =
            read_video_clip(&collection, &file, 0, SimpleAssetReadLimits::default()).unwrap();

        assert_eq!(video.name, "trailer");
        assert_eq!(video.suggested_extension, ".mp4");
        assert_eq!(video.payload.read_to_vec(32).unwrap(), b"vide");
    }

    #[test]
    fn gives_a_video_without_a_portable_extension_a_fallback() {
        assert_eq!(
            super::portable_extension("movies\\目录\\trailer").unwrap(),
            ".video"
        );
    }

    #[test]
    fn auto_export_streams_font_and_external_audio_payloads() {
        let mut font_object = named_object("heading");
        font_object.extend_from_slice(&0_f32.to_le_bytes());
        push_pptr(&mut font_object);
        font_object.extend_from_slice(&12_f32.to_le_bytes());
        push_pptr(&mut font_object);
        font_object.extend_from_slice(&[0_u8; 20]);
        push_i32(&mut font_object, 0);
        push_i32(&mut font_object, 0);
        font_object.extend_from_slice(&1_f32.to_le_bytes());
        push_i32(&mut font_object, 7);
        font_object.extend_from_slice(b"OTTOabc");
        let font = parse_asset(FONT_CLASS_ID, "2022.3.62f1", &font_object);

        let mut audio_object = named_object("theme");
        audio_object.extend_from_slice(&[0_u8; 20]);
        audio_object.push(0);
        align(&mut audio_object, 4);
        audio_object.extend_from_slice(&0_i32.to_le_bytes());
        audio_object.extend_from_slice(&[0_u8; 3]);
        align(&mut audio_object, 4);
        push_aligned_string(&mut audio_object, "sound.resS");
        audio_object.extend_from_slice(&2_i64.to_le_bytes());
        audio_object.extend_from_slice(&3_i64.to_le_bytes());
        audio_object.extend_from_slice(&1_i32.to_le_bytes());
        let audio = parse_asset(AUDIO_CLIP_CLASS_ID, "2022.3.62f1", &audio_object);
        let collection = AssetCollection::from_loaded_parts(
            vec![
                LoadedSerializedFile {
                    path: "font.assets".to_owned(),
                    file: font,
                },
                LoadedSerializedFile {
                    path: "audio.assets".to_owned(),
                    file: audio,
                },
            ],
            vec![LoadedResource {
                path: "sound.resS".to_owned(),
                region: Region::from_bytes(b"xxabczz".to_vec()),
            }],
        );
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let output = std::env::temp_dir().join(format!("assetstudio-simple-export-{unique}"));

        let report = export_collection(&collection, &output, ExportOptions::default()).unwrap();

        assert!(report.failures.is_empty());
        assert_eq!(report.exported.len(), 2);
        let font_record = report
            .exported
            .iter()
            .find(|record| record.class_id == FONT_CLASS_ID)
            .unwrap();
        assert_eq!(font_record.payload_kind, "font");
        assert_eq!(font_record.output_path.extension().unwrap(), "otf");
        assert_eq!(fs::read(&font_record.output_path).unwrap(), b"OTTOabc");
        let audio_record = report
            .exported
            .iter()
            .find(|record| record.class_id == AUDIO_CLIP_CLASS_ID)
            .unwrap();
        assert_eq!(audio_record.payload_kind, "audio_raw");
        assert_eq!(audio_record.output_path.extension().unwrap(), "fsb");
        assert_eq!(fs::read(&audio_record.output_path).unwrap(), b"abc");

        fs::remove_dir_all(output).unwrap();
    }

    #[test]
    fn auto_export_writes_legacy_pcm_as_wav_and_raw_or_limits_remain_explicit() {
        let mut object = named_object("legacy-pcm");
        object.extend_from_slice(&2_i32.to_le_bytes());
        object.extend_from_slice(&0_f32.to_le_bytes());
        object.extend_from_slice(&22_050_i32.to_le_bytes());
        object.extend_from_slice(&4_i32.to_le_bytes());
        object.extend_from_slice(&[1, 2, 3, 4]);
        let file = parse_asset(AUDIO_CLIP_CLASS_ID, "2.5.0f1", &object);
        let collection = collection_with(file, "unused.resS", b"");

        let output = unique_temp_directory("assetstudio-legacy-pcm-auto");
        let report = export_collection(&collection, &output, ExportOptions::default()).unwrap();
        assert!(report.failures.is_empty());
        assert_eq!(report.exported[0].payload_kind, "audio_wav");
        assert_eq!(report.exported[0].output_path.extension().unwrap(), "wav");
        let wav = fs::read(&report.exported[0].output_path).unwrap();
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[44..], &[1, 2, 3, 4]);
        fs::remove_dir_all(output).unwrap();

        let output = unique_temp_directory("assetstudio-legacy-pcm-raw");
        let report = export_collection(
            &collection,
            &output,
            ExportOptions {
                audio_format: AudioExportFormat::Raw,
                ..ExportOptions::default()
            },
        )
        .unwrap();
        assert!(report.failures.is_empty());
        assert_eq!(report.exported[0].payload_kind, "audio_raw");
        assert_eq!(
            fs::read(&report.exported[0].output_path).unwrap(),
            [1, 2, 3, 4]
        );
        fs::remove_dir_all(output).unwrap();

        let output = unique_temp_directory("assetstudio-legacy-pcm-limit");
        let report = export_collection(
            &collection,
            &output,
            ExportOptions {
                audio_format: AudioExportFormat::Wav,
                maximum_audio_output_bytes: 47,
                ..ExportOptions::default()
            },
        )
        .unwrap();
        assert!(report.exported.is_empty());
        assert_eq!(report.failures.len(), 1);
        assert!(report.failures[0].error.contains("exceeding limit"));
        fs::remove_dir_all(output).unwrap();
    }

    #[test]
    fn auto_export_writes_modern_fsb5_pcm_as_wav() {
        let fsb = fsb5_pcm16(&[1, 2, 3, 4], 1, 2, 44_100);
        let mut object = named_object("modern-pcm");
        object.extend_from_slice(&0_i32.to_le_bytes());
        object.extend_from_slice(&2_i32.to_le_bytes());
        object.extend_from_slice(&44_100_i32.to_le_bytes());
        object.extend_from_slice(&16_i32.to_le_bytes());
        object.extend_from_slice(&0_f32.to_le_bytes());
        object.push(0);
        align(&mut object, 4);
        object.extend_from_slice(&0_i32.to_le_bytes());
        object.extend_from_slice(&[0_u8; 3]);
        align(&mut object, 4);
        push_aligned_string(&mut object, "");
        object.extend_from_slice(&0_i64.to_le_bytes());
        object.extend_from_slice(&i64::try_from(fsb.len()).unwrap().to_le_bytes());
        object.extend_from_slice(&0_i32.to_le_bytes());
        object.extend_from_slice(&fsb);
        let file = parse_asset(AUDIO_CLIP_CLASS_ID, "2022.3.62f1", &object);
        let collection = collection_with(file.clone(), "unused.resS", b"");

        let audio =
            read_audio_clip_asset(&collection, &file, 0, SimpleAssetReadLimits::default()).unwrap();
        let DirectWavKind::Fsb5Pcm(stream) = audio.direct_wav.unwrap() else {
            panic!("modern PCM FSB5 should be directly writable");
        };
        assert_eq!(stream.sample_format, PcmSampleFormat::Signed16);
        assert_eq!((stream.channels, stream.sample_rate), (2, 44_100));

        let output = unique_temp_directory("assetstudio-modern-fsb5-pcm");
        let report = export_collection(&collection, &output, ExportOptions::default()).unwrap();
        assert!(report.failures.is_empty());
        assert_eq!(report.exported[0].payload_kind, "audio_wav");
        let wav = fs::read(&report.exported[0].output_path).unwrap();
        assert_eq!(&wav[..12], b"RIFF(\0\0\0WAVE");
        assert_eq!(&wav[44..], &[1, 2, 3, 4]);
        fs::remove_dir_all(output).unwrap();
    }

    #[test]
    fn auto_export_decodes_modern_fsb5_ima_as_wav() {
        let fsb = fsb5_ima_mono();
        let mut object = named_object("modern-ima");
        object.extend_from_slice(&0_i32.to_le_bytes());
        object.extend_from_slice(&1_i32.to_le_bytes());
        object.extend_from_slice(&44_100_i32.to_le_bytes());
        object.extend_from_slice(&16_i32.to_le_bytes());
        object.extend_from_slice(&0_f32.to_le_bytes());
        object.push(0);
        align(&mut object, 4);
        object.extend_from_slice(&0_i32.to_le_bytes());
        object.extend_from_slice(&[0_u8; 3]);
        align(&mut object, 4);
        push_aligned_string(&mut object, "");
        object.extend_from_slice(&0_i64.to_le_bytes());
        object.extend_from_slice(&i64::try_from(fsb.len()).unwrap().to_le_bytes());
        object.extend_from_slice(&0_i32.to_le_bytes());
        object.extend_from_slice(&fsb);
        let file = parse_asset(AUDIO_CLIP_CLASS_ID, "2022.3.62f1", &object);
        let collection = collection_with(file.clone(), "unused.resS", b"");

        let audio =
            read_audio_clip_asset(&collection, &file, 0, SimpleAssetReadLimits::default()).unwrap();
        let DirectWavKind::Fsb5Ima(stream) = audio.direct_wav.unwrap() else {
            panic!("modern IMA FSB5 should be directly writable");
        };
        assert_eq!((stream.channels, stream.sample_rate), (1, 44_100));
        assert_eq!((stream.frame_count, stream.compressed_length), (64, 36));

        let output = unique_temp_directory("assetstudio-modern-fsb5-ima");
        let report = export_collection(&collection, &output, ExportOptions::default()).unwrap();
        assert!(report.failures.is_empty());
        assert_eq!(report.exported[0].payload_kind, "audio_wav");
        let wav = fs::read(&report.exported[0].output_path).unwrap();
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(&wav[20..24], &[1, 0, 1, 0]);
        assert_eq!(wav.len(), 44 + 64 * 2);
        assert_eq!(i16::from_le_bytes(wav[44..46].try_into().unwrap()), 1000);
        assert_eq!(i16::from_le_bytes(wav[46..48].try_into().unwrap()), 1002);
        fs::remove_dir_all(output).unwrap();
    }

    #[test]
    fn auto_export_decodes_modern_fsb5_dsp_as_wav() {
        let fsb = fsb5_dsp_mono();
        let mut object = named_object("modern-dsp");
        object.extend_from_slice(&0_i32.to_le_bytes());
        object.extend_from_slice(&1_i32.to_le_bytes());
        object.extend_from_slice(&44_100_i32.to_le_bytes());
        object.extend_from_slice(&16_i32.to_le_bytes());
        object.extend_from_slice(&0_f32.to_le_bytes());
        object.push(0);
        align(&mut object, 4);
        object.extend_from_slice(&0_i32.to_le_bytes());
        object.extend_from_slice(&[0_u8; 3]);
        align(&mut object, 4);
        push_aligned_string(&mut object, "");
        object.extend_from_slice(&0_i64.to_le_bytes());
        object.extend_from_slice(&i64::try_from(fsb.len()).unwrap().to_le_bytes());
        object.extend_from_slice(&0_i32.to_le_bytes());
        object.extend_from_slice(&fsb);
        let file = parse_asset(AUDIO_CLIP_CLASS_ID, "2022.3.62f1", &object);
        let collection = collection_with(file.clone(), "unused.resS", b"");

        let audio =
            read_audio_clip_asset(&collection, &file, 0, SimpleAssetReadLimits::default()).unwrap();
        let DirectWavKind::Fsb5Dsp(stream) = audio.direct_wav.unwrap() else {
            panic!("modern DSP FSB5 should be directly writable");
        };
        assert_eq!((stream.channels, stream.sample_rate), (1, 44_100));
        assert_eq!((stream.frame_count, stream.compressed_length), (14, 8));

        let output = unique_temp_directory("assetstudio-modern-fsb5-dsp");
        let report = export_collection(&collection, &output, ExportOptions::default()).unwrap();
        assert!(report.failures.is_empty());
        assert_eq!(report.exported[0].payload_kind, "audio_wav");
        let wav = fs::read(&report.exported[0].output_path).unwrap();
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(&wav[20..24], &[1, 0, 1, 0]);
        assert_eq!(wav.len(), 44 + 14 * 2);
        assert_eq!(i16::from_le_bytes(wav[44..46].try_into().unwrap()), 1);
        assert_eq!(i16::from_le_bytes(wav[46..48].try_into().unwrap()), 3);
        fs::remove_dir_all(output).unwrap();
    }

    #[test]
    fn auto_export_decodes_modern_fsb5_vag_as_wav() {
        let fsb = fsb5_vag_mono();
        let mut object = named_object("modern-vag");
        object.extend_from_slice(&0_i32.to_le_bytes());
        object.extend_from_slice(&1_i32.to_le_bytes());
        object.extend_from_slice(&44_100_i32.to_le_bytes());
        object.extend_from_slice(&16_i32.to_le_bytes());
        object.extend_from_slice(&0_f32.to_le_bytes());
        object.push(0);
        align(&mut object, 4);
        object.extend_from_slice(&0_i32.to_le_bytes());
        object.extend_from_slice(&[0_u8; 3]);
        align(&mut object, 4);
        push_aligned_string(&mut object, "");
        object.extend_from_slice(&0_i64.to_le_bytes());
        object.extend_from_slice(&i64::try_from(fsb.len()).unwrap().to_le_bytes());
        object.extend_from_slice(&0_i32.to_le_bytes());
        object.extend_from_slice(&fsb);
        let file = parse_asset(AUDIO_CLIP_CLASS_ID, "2022.3.62f1", &object);
        let collection = collection_with(file.clone(), "unused.resS", b"");

        let audio =
            read_audio_clip_asset(&collection, &file, 0, SimpleAssetReadLimits::default()).unwrap();
        let DirectWavKind::Fsb5Vag(stream) = audio.direct_wav.unwrap() else {
            panic!("modern VAG FSB5 should be directly writable");
        };
        assert_eq!((stream.channels, stream.sample_rate), (1, 44_100));
        assert_eq!((stream.frame_count, stream.compressed_length), (56, 32));

        let output = unique_temp_directory("assetstudio-modern-fsb5-vag");
        let report = export_collection(&collection, &output, ExportOptions::default()).unwrap();
        assert!(report.failures.is_empty());
        assert_eq!(report.exported[0].payload_kind, "audio_wav");
        let wav = fs::read(&report.exported[0].output_path).unwrap();
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(&wav[20..24], &[1, 0, 1, 0]);
        assert_eq!(wav.len(), 44 + 56 * 2);
        assert_eq!(i16::from_le_bytes(wav[44..46].try_into().unwrap()), 1);
        assert_eq!(i16::from_le_bytes(wav[46..48].try_into().unwrap()), 2);
        assert_eq!(i16::from_le_bytes(wav[100..102].try_into().unwrap()), 2);
        fs::remove_dir_all(output).unwrap();
    }

    #[test]
    fn auto_export_decodes_modern_fsb5_hevag_as_wav() {
        let fsb = fsb5_hevag_mono();
        let mut object = named_object("modern-hevag");
        object.extend_from_slice(&0_i32.to_le_bytes());
        object.extend_from_slice(&1_i32.to_le_bytes());
        object.extend_from_slice(&44_100_i32.to_le_bytes());
        object.extend_from_slice(&16_i32.to_le_bytes());
        object.extend_from_slice(&0_f32.to_le_bytes());
        object.push(0);
        align(&mut object, 4);
        object.extend_from_slice(&0_i32.to_le_bytes());
        object.extend_from_slice(&[0_u8; 3]);
        align(&mut object, 4);
        push_aligned_string(&mut object, "");
        object.extend_from_slice(&0_i64.to_le_bytes());
        object.extend_from_slice(&i64::try_from(fsb.len()).unwrap().to_le_bytes());
        object.extend_from_slice(&0_i32.to_le_bytes());
        object.extend_from_slice(&fsb);
        let file = parse_asset(AUDIO_CLIP_CLASS_ID, "2022.3.62f1", &object);
        let collection = collection_with(file.clone(), "unused.resS", b"");

        let audio =
            read_audio_clip_asset(&collection, &file, 0, SimpleAssetReadLimits::default()).unwrap();
        let DirectWavKind::Fsb5Hevag(stream) = audio.direct_wav.unwrap() else {
            panic!("modern HEVAG FSB5 should be directly writable");
        };
        assert_eq!((stream.channels, stream.sample_rate), (1, 44_100));
        assert_eq!((stream.frame_count, stream.compressed_length), (56, 32));

        let output = unique_temp_directory("assetstudio-modern-fsb5-hevag");
        let report = export_collection(&collection, &output, ExportOptions::default()).unwrap();
        assert!(report.failures.is_empty());
        assert_eq!(report.exported[0].payload_kind, "audio_wav");
        let wav = fs::read(&report.exported[0].output_path).unwrap();
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(&wav[20..24], &[1, 0, 1, 0]);
        assert_eq!(wav.len(), 44 + 56 * 2);
        assert_eq!(i16::from_le_bytes(wav[44..46].try_into().unwrap()), 1);
        assert_eq!(i16::from_le_bytes(wav[46..48].try_into().unwrap()), 2);
        assert_eq!(i16::from_le_bytes(wav[100..102].try_into().unwrap()), 2);
        fs::remove_dir_all(output).unwrap();
    }

    #[test]
    fn auto_export_decodes_modern_fsb5_fadpcm_as_wav() {
        let fsb = fsb5_fadpcm_mono();
        let mut object = named_object("modern-fadpcm");
        object.extend_from_slice(&0_i32.to_le_bytes());
        object.extend_from_slice(&1_i32.to_le_bytes());
        object.extend_from_slice(&44_100_i32.to_le_bytes());
        object.extend_from_slice(&16_i32.to_le_bytes());
        object.extend_from_slice(&0_f32.to_le_bytes());
        object.push(0);
        align(&mut object, 4);
        object.extend_from_slice(&0_i32.to_le_bytes());
        object.extend_from_slice(&[0_u8; 3]);
        align(&mut object, 4);
        push_aligned_string(&mut object, "");
        object.extend_from_slice(&0_i64.to_le_bytes());
        object.extend_from_slice(&i64::try_from(fsb.len()).unwrap().to_le_bytes());
        object.extend_from_slice(&0_i32.to_le_bytes());
        object.extend_from_slice(&fsb);
        let file = parse_asset(AUDIO_CLIP_CLASS_ID, "2022.3.62f1", &object);
        let collection = collection_with(file.clone(), "unused.resS", b"");

        let audio =
            read_audio_clip_asset(&collection, &file, 0, SimpleAssetReadLimits::default()).unwrap();
        let DirectWavKind::Fsb5Fadpcm(stream) = audio.direct_wav.unwrap() else {
            panic!("modern FADPCM FSB5 should be directly writable");
        };
        assert_eq!((stream.channels, stream.sample_rate), (1, 44_100));
        assert_eq!((stream.frame_count, stream.compressed_length), (512, 0x118));

        let output = unique_temp_directory("assetstudio-modern-fsb5-fadpcm");
        let report = export_collection(&collection, &output, ExportOptions::default()).unwrap();
        assert!(report.failures.is_empty());
        assert_eq!(report.exported[0].payload_kind, "audio_wav");
        let wav = fs::read(&report.exported[0].output_path).unwrap();
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(&wav[20..24], &[1, 0, 1, 0]);
        assert_eq!(wav.len(), 44 + 512 * 2);
        assert_eq!(i16::from_le_bytes(wav[44..46].try_into().unwrap()), 1);
        assert_eq!(i16::from_le_bytes(wav[46..48].try_into().unwrap()), 2);
        assert_eq!(i16::from_le_bytes(wav[556..558].try_into().unwrap()), 2);
        fs::remove_dir_all(output).unwrap();
    }

    #[test]
    fn auto_export_decodes_modern_fsb5_mpeg_as_wav() {
        let fsb = fsb5_mpeg_mono();
        let mut object = named_object("modern-mpeg");
        object.extend_from_slice(&0_i32.to_le_bytes());
        object.extend_from_slice(&1_i32.to_le_bytes());
        object.extend_from_slice(&44_100_i32.to_le_bytes());
        object.extend_from_slice(&16_i32.to_le_bytes());
        object.extend_from_slice(&0_f32.to_le_bytes());
        object.push(0);
        align(&mut object, 4);
        object.extend_from_slice(&0_i32.to_le_bytes());
        object.extend_from_slice(&[0_u8; 3]);
        align(&mut object, 4);
        push_aligned_string(&mut object, "");
        object.extend_from_slice(&0_i64.to_le_bytes());
        object.extend_from_slice(&i64::try_from(fsb.len()).unwrap().to_le_bytes());
        object.extend_from_slice(&0_i32.to_le_bytes());
        object.extend_from_slice(&fsb);
        let file = parse_asset(AUDIO_CLIP_CLASS_ID, "2022.3.62f1", &object);
        let collection = collection_with(file.clone(), "unused.resS", b"");

        let audio =
            read_audio_clip_asset(&collection, &file, 0, SimpleAssetReadLimits::default()).unwrap();
        let DirectWavKind::Fsb5Mpeg(stream) = audio.direct_wav.unwrap() else {
            panic!("modern MPEG FSB5 should be directly writable");
        };
        assert_eq!((stream.channels, stream.sample_rate), (1, 44_100));
        assert_eq!((stream.frame_count, stream.compressed_length), (2304, 208));

        let output = unique_temp_directory("assetstudio-modern-fsb5-mpeg");
        let report = export_collection(&collection, &output, ExportOptions::default()).unwrap();
        assert!(report.failures.is_empty());
        assert_eq!(report.exported[0].payload_kind, "audio_wav");
        let wav = fs::read(&report.exported[0].output_path).unwrap();
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(&wav[20..24], &[1, 0, 1, 0]);
        assert_eq!(wav.len(), 44 + 2304 * 2);
        assert!(wav[44..].iter().all(|byte| *byte == 0));
        fs::remove_dir_all(output).unwrap();
    }

    #[test]
    fn auto_export_decodes_modern_fsb5_opus_as_wav() {
        let fsb = fsb5_opus_mono();
        let mut object = named_object("modern-opus");
        object.extend_from_slice(&0_i32.to_le_bytes());
        object.extend_from_slice(&1_i32.to_le_bytes());
        object.extend_from_slice(&48_000_i32.to_le_bytes());
        object.extend_from_slice(&16_i32.to_le_bytes());
        object.extend_from_slice(&0_f32.to_le_bytes());
        object.push(0);
        align(&mut object, 4);
        object.extend_from_slice(&0_i32.to_le_bytes());
        object.extend_from_slice(&[0_u8; 3]);
        align(&mut object, 4);
        push_aligned_string(&mut object, "");
        object.extend_from_slice(&0_i64.to_le_bytes());
        object.extend_from_slice(&i64::try_from(fsb.len()).unwrap().to_le_bytes());
        object.extend_from_slice(&0_i32.to_le_bytes());
        object.extend_from_slice(&fsb);
        let file = parse_asset(AUDIO_CLIP_CLASS_ID, "2022.3.62f1", &object);
        let collection = collection_with(file.clone(), "unused.resS", b"");

        let audio =
            read_audio_clip_asset(&collection, &file, 0, SimpleAssetReadLimits::default()).unwrap();
        let DirectWavKind::Fsb5Opus(stream) = audio.direct_wav.unwrap() else {
            panic!("modern Opus FSB5 should be directly writable");
        };
        assert_eq!((stream.channels, stream.sample_rate), (1, 48_000));
        assert_eq!((stream.frame_count, stream.compressed_length), (648, 68));

        let output = unique_temp_directory("assetstudio-modern-fsb5-opus");
        let report = export_collection(&collection, &output, ExportOptions::default()).unwrap();
        assert!(report.failures.is_empty());
        assert_eq!(report.exported[0].payload_kind, "audio_wav");
        let wav = fs::read(&report.exported[0].output_path).unwrap();
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(&wav[20..24], &[1, 0, 1, 0]);
        assert_eq!(wav.len(), 44 + 648 * 2);
        assert!(wav[44..].iter().all(|byte| *byte == 0));
        fs::remove_dir_all(output).unwrap();
    }

    #[test]
    fn auto_export_decodes_modern_fsb5_vorbis_as_wav() {
        let mut object = named_object("modern-vorbis");
        object.extend_from_slice(&0_i32.to_le_bytes());
        object.extend_from_slice(&2_i32.to_le_bytes());
        object.extend_from_slice(&48_000_i32.to_le_bytes());
        object.extend_from_slice(&16_i32.to_le_bytes());
        object.extend_from_slice(&0_f32.to_le_bytes());
        object.push(0);
        align(&mut object, 4);
        object.extend_from_slice(&0_i32.to_le_bytes());
        object.extend_from_slice(&[0_u8; 3]);
        align(&mut object, 4);
        push_aligned_string(&mut object, "");
        object.extend_from_slice(&0_i64.to_le_bytes());
        object.extend_from_slice(
            &i64::try_from(FSB5_VORBIS_FIXTURE.len())
                .unwrap()
                .to_le_bytes(),
        );
        object.extend_from_slice(&0_i32.to_le_bytes());
        object.extend_from_slice(FSB5_VORBIS_FIXTURE);
        let file = parse_asset(AUDIO_CLIP_CLASS_ID, "2022.3.62f1", &object);
        let collection = collection_with(file.clone(), "unused.resS", b"");

        let audio =
            read_audio_clip_asset(&collection, &file, 0, SimpleAssetReadLimits::default()).unwrap();
        let DirectWavKind::Fsb5Vorbis(stream) = audio.direct_wav.unwrap() else {
            panic!("modern Vorbis FSB5 should be directly writable");
        };
        assert_eq!((stream.channels, stream.sample_rate), (2, 48_000));
        assert_eq!((stream.frame_count, stream.setup_crc), (4800, 0x87c1_21d5));

        let output = unique_temp_directory("assetstudio-modern-fsb5-vorbis");
        let report = export_collection(&collection, &output, ExportOptions::default()).unwrap();
        assert!(report.failures.is_empty());
        assert_eq!(report.exported[0].payload_kind, "audio_wav");
        let wav = fs::read(&report.exported[0].output_path).unwrap();
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(&wav[20..24], &[1, 0, 2, 0]);
        assert_eq!(wav.len(), 44 + 4800 * 2 * 2);
        assert!(wav[44..].iter().any(|byte| *byte != 0));
        fs::remove_dir_all(output).unwrap();
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

    fn unique_temp_directory(label: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("{label}-{unique}"))
    }

    fn parse_asset(class_id: i32, version: &str, object: &[u8]) -> SerializedFile {
        SerializedFile::open(Region::from_bytes(synthetic_v22_asset(
            class_id, version, object,
        )))
        .unwrap()
    }

    fn named_object(name: &str) -> Vec<u8> {
        let mut output = Vec::new();
        push_aligned_string(&mut output, name);
        output
    }

    fn push_pptr(output: &mut Vec<u8>) {
        output.extend_from_slice(&0_i32.to_le_bytes());
        output.extend_from_slice(&0_i64.to_le_bytes());
    }

    fn push_aligned_string(output: &mut Vec<u8>, value: &str) {
        push_i32(output, i32::try_from(value.len()).unwrap());
        output.extend_from_slice(value.as_bytes());
        if !value.is_empty() {
            align(output, 4);
        }
    }

    fn align(output: &mut Vec<u8>, alignment: usize) {
        while !output.len().is_multiple_of(alignment) {
            output.push(0);
        }
    }

    fn synthetic_v22_asset(class_id: i32, version: &str, object: &[u8]) -> Vec<u8> {
        let mut metadata = Vec::new();
        metadata.extend_from_slice(version.as_bytes());
        metadata.push(0);
        push_i32(&mut metadata, 13);
        metadata.push(0);
        push_i32(&mut metadata, 1);
        push_i32(&mut metadata, class_id);
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

    fn push_i32(output: &mut Vec<u8>, value: i32) {
        output.extend_from_slice(&value.to_le_bytes());
    }

    fn align_with_base(output: &mut Vec<u8>, base: usize, alignment: usize) {
        while !(base + output.len()).is_multiple_of(alignment) {
            output.push(0);
        }
    }

    fn fsb5_pcm16(data: &[u8], frames: u64, channels: u16, sample_rate: u32) -> Vec<u8> {
        let channel_code = match channels {
            1 => 0_u64,
            2 => 1,
            6 => 2,
            8 => 3,
            _ => panic!("test fixture channel count must use compact FSB5 metadata"),
        };
        let rate_code = match sample_rate {
            8_000 => 1_u64,
            44_100 => 8,
            48_000 => 9,
            _ => panic!("test fixture sample rate must use compact FSB5 metadata"),
        };
        let sample_mode = (frames << 34) | (channel_code << 5) | (rate_code << 1);
        let mut fsb = vec![0_u8; 0x3c];
        fsb[..4].copy_from_slice(b"FSB5");
        fsb[4..8].copy_from_slice(&1_u32.to_le_bytes());
        fsb[8..12].copy_from_slice(&1_u32.to_le_bytes());
        fsb[12..16].copy_from_slice(&8_u32.to_le_bytes());
        fsb[20..24].copy_from_slice(&u32::try_from(data.len()).unwrap().to_le_bytes());
        fsb[24..28].copy_from_slice(&2_u32.to_le_bytes());
        fsb.extend_from_slice(&sample_mode.to_le_bytes());
        fsb.extend_from_slice(data);
        fsb
    }

    fn fsb5_ima_mono() -> Vec<u8> {
        let mut block = vec![0x10_u8; 36];
        block[..2].copy_from_slice(&1000_i16.to_le_bytes());
        block[2] = 10;
        block[3] = 0;
        let sample_mode = (64_u64 << 34) | (8 << 1);
        let mut fsb = vec![0_u8; 0x3c];
        fsb[..4].copy_from_slice(b"FSB5");
        fsb[4..8].copy_from_slice(&1_u32.to_le_bytes());
        fsb[8..12].copy_from_slice(&1_u32.to_le_bytes());
        fsb[12..16].copy_from_slice(&8_u32.to_le_bytes());
        fsb[20..24].copy_from_slice(&36_u32.to_le_bytes());
        fsb[24..28].copy_from_slice(&7_u32.to_le_bytes());
        fsb.extend_from_slice(&sample_mode.to_le_bytes());
        fsb.extend_from_slice(&block);
        fsb
    }

    fn fsb5_dsp_mono() -> Vec<u8> {
        let mut coefficients = vec![0_u8; 0x2e];
        coefficients[..2].copy_from_slice(&2048_i16.to_be_bytes());
        let sample_mode = (14_u64 << 34) | (8 << 1) | 1;
        let chunk_header = (7_u32 << 25) | (u32::try_from(coefficients.len()).unwrap() << 1);
        let mut headers = Vec::new();
        headers.extend_from_slice(&sample_mode.to_le_bytes());
        headers.extend_from_slice(&chunk_header.to_le_bytes());
        headers.extend_from_slice(&coefficients);
        let data = [0_u8, 0x12, 0x12, 0x12, 0x12, 0x12, 0x12, 0x12];

        let mut fsb = vec![0_u8; 0x3c];
        fsb[..4].copy_from_slice(b"FSB5");
        fsb[4..8].copy_from_slice(&1_u32.to_le_bytes());
        fsb[8..12].copy_from_slice(&1_u32.to_le_bytes());
        fsb[12..16].copy_from_slice(&u32::try_from(headers.len()).unwrap().to_le_bytes());
        fsb[20..24].copy_from_slice(&u32::try_from(data.len()).unwrap().to_le_bytes());
        fsb[24..28].copy_from_slice(&6_u32.to_le_bytes());
        fsb.extend_from_slice(&headers);
        fsb.extend_from_slice(&data);
        fsb
    }

    fn fsb5_vag_mono() -> Vec<u8> {
        let mut first = [0x21_u8; 16];
        first[0] = 0x0c;
        first[1] = 0;
        let mut second = [0x32_u8; 16];
        second[0] = 0x0c;
        second[1] = 0;
        let sample_mode = (56_u64 << 34) | (8 << 1);
        let mut fsb = vec![0_u8; 0x3c];
        fsb[..4].copy_from_slice(b"FSB5");
        fsb[4..8].copy_from_slice(&1_u32.to_le_bytes());
        fsb[8..12].copy_from_slice(&1_u32.to_le_bytes());
        fsb[12..16].copy_from_slice(&8_u32.to_le_bytes());
        fsb[20..24].copy_from_slice(&32_u32.to_le_bytes());
        fsb[24..28].copy_from_slice(&8_u32.to_le_bytes());
        fsb.extend_from_slice(&sample_mode.to_le_bytes());
        fsb.extend_from_slice(&first);
        fsb.extend_from_slice(&second);
        fsb
    }

    fn fsb5_hevag_mono() -> Vec<u8> {
        let mut first = [0x21_u8; 16];
        first[0] = 0x0c;
        first[1] = 0;
        let mut second = [0x32_u8; 16];
        second[0] = 0x0c;
        second[1] = 0;
        let sample_mode = (56_u64 << 34) | (8 << 1);
        let mut fsb = vec![0_u8; 0x3c];
        fsb[..4].copy_from_slice(b"FSB5");
        fsb[4..8].copy_from_slice(&1_u32.to_le_bytes());
        fsb[8..12].copy_from_slice(&1_u32.to_le_bytes());
        fsb[12..16].copy_from_slice(&8_u32.to_le_bytes());
        fsb[20..24].copy_from_slice(&32_u32.to_le_bytes());
        fsb[24..28].copy_from_slice(&9_u32.to_le_bytes());
        fsb.extend_from_slice(&sample_mode.to_le_bytes());
        fsb.extend_from_slice(&first);
        fsb.extend_from_slice(&second);
        fsb
    }

    fn fsb5_fadpcm_mono() -> Vec<u8> {
        let mut first = vec![0x21_u8; 0x8c];
        first[..12].fill(0);
        let mut second = vec![0x32_u8; 0x8c];
        second[..12].fill(0);
        let sample_mode = (512_u64 << 34) | (8 << 1);
        let mut fsb = vec![0_u8; 0x3c];
        fsb[..4].copy_from_slice(b"FSB5");
        fsb[4..8].copy_from_slice(&1_u32.to_le_bytes());
        fsb[8..12].copy_from_slice(&1_u32.to_le_bytes());
        fsb[12..16].copy_from_slice(&8_u32.to_le_bytes());
        fsb[20..24].copy_from_slice(&0x118_u32.to_le_bytes());
        fsb[24..28].copy_from_slice(&16_u32.to_le_bytes());
        fsb.extend_from_slice(&sample_mode.to_le_bytes());
        fsb.extend_from_slice(&first);
        fsb.extend_from_slice(&second);
        fsb
    }

    fn fsb5_mpeg_mono() -> Vec<u8> {
        let mut frames = vec![0_u8; 208];
        frames[..4].copy_from_slice(&[0xff, 0xfb, 0x10, 0xc0]);
        frames[104..108].copy_from_slice(&[0xff, 0xfb, 0x10, 0xc0]);
        let sample_mode = (2304_u64 << 34) | (8 << 1);
        let mut fsb = vec![0_u8; 0x3c];
        fsb[..4].copy_from_slice(b"FSB5");
        fsb[4..8].copy_from_slice(&1_u32.to_le_bytes());
        fsb[8..12].copy_from_slice(&1_u32.to_le_bytes());
        fsb[12..16].copy_from_slice(&8_u32.to_le_bytes());
        fsb[20..24].copy_from_slice(&208_u32.to_le_bytes());
        fsb[24..28].copy_from_slice(&11_u32.to_le_bytes());
        fsb.extend_from_slice(&sample_mode.to_le_bytes());
        fsb.extend_from_slice(&frames);
        fsb
    }

    fn fsb5_opus_mono() -> Vec<u8> {
        let packet = [
            0xf8, 0x6f, 0xed, 0x8a, 0x58, 0xc6, 0x40, 0x44, 0x64, 0xd8, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0xad, 0x43, 0xa8, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        let mut data = Vec::new();
        data.extend_from_slice(&64_u16.to_le_bytes());
        data.extend_from_slice(&packet);
        data.extend_from_slice(&0_u16.to_le_bytes());
        let sample_mode = (648_u64 << 34) | (9 << 1);
        let mut fsb = vec![0_u8; 0x3c];
        fsb[..4].copy_from_slice(b"FSB5");
        fsb[4..8].copy_from_slice(&1_u32.to_le_bytes());
        fsb[8..12].copy_from_slice(&1_u32.to_le_bytes());
        fsb[12..16].copy_from_slice(&8_u32.to_le_bytes());
        fsb[20..24].copy_from_slice(&u32::try_from(data.len()).unwrap().to_le_bytes());
        fsb[24..28].copy_from_slice(&17_u32.to_le_bytes());
        fsb.extend_from_slice(&sample_mode.to_le_bytes());
        fsb.extend_from_slice(&data);
        fsb
    }
}
