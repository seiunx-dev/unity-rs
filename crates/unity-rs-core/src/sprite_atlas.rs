//! Source-bound parsing and composite-key lookup for Unity `SpriteAtlas`.
//!
//! `Sprite.m_RenderDataKey` and the atlas map serialize the same GUID bytes and
//! signed 64-bit value. The managed render path uses atlas crop metadata only
//! after that exact key matches. Atlas selection and texture decoding remain in
//! the caller: notably, the managed `SpriteHelper` does not fall back to the
//! Sprite's resident render data after it has resolved a non-null atlas.

use std::io::Read;

use crate::endian::{Endian, EndianReader, checked_length};
use crate::serialized::{ObjectReference, SerializedFile};
use crate::source::RegionCursor;
use crate::{Error, Result};

pub const SPRITE_ATLAS_CLASS_ID: i32 = 687_078_895;

const NO_TARGET_PLATFORM: i32 = -2;
const FIRST_SPRITE_ATLAS_VERSION: (u32, u32, u32) = (2017, 1, 0);
const LAST_VERIFIED_UNITY_MAJOR: u32 = 2023;
const UNITY_6_MAJOR: u32 = 6000;
// 6000.3 checked rather than assumed: a shipping 6000.3.12f1 build's own type
// tree for `SpriteAtlas` lists exactly the fields below, in this order, with
// nothing added after 2020.2's secondary textures.
const LAST_VERIFIED_UNITY_6_MINOR: u32 = 3;

/// Allocation and input-size limits for one `SpriteAtlas` object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpriteAtlasReadLimits {
    pub maximum_string_bytes: usize,
    pub maximum_total_string_bytes: usize,
    pub maximum_packed_sprites: usize,
    pub maximum_packed_sprite_names: usize,
    pub maximum_render_data_entries: usize,
    pub maximum_secondary_textures: usize,
}

impl Default for SpriteAtlasReadLimits {
    fn default() -> Self {
        Self {
            maximum_string_bytes: 16 * 1024 * 1024,
            maximum_total_string_bytes: 32 * 1024 * 1024,
            maximum_packed_sprites: 1_000_000,
            maximum_packed_sprite_names: 1_000_000,
            maximum_render_data_entries: 1_000_000,
            maximum_secondary_textures: 1_000_000,
        }
    }
}

/// A render-data map key as it is serialized by Unity.
///
/// The GUID stays in its original 16-byte representation. This avoids the
/// display-byte reordering performed by `.NET Guid` while preserving exactly
/// the equality semantics used between `Sprite.m_RenderDataKey` and the atlas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SpriteRenderDataKey {
    pub guid_bytes: [u8; 16],
    pub value: i64,
}

impl From<([u8; 16], i64)> for SpriteRenderDataKey {
    fn from((guid_bytes, value): ([u8; 16], i64)) -> Self {
        Self { guid_bytes, value }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpriteAtlasVector2 {
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpriteAtlasVector4 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub w: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpriteAtlasRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// Raw `SpriteSettings` bit field used by the existing Sprite crop contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpriteAtlasSettings {
    pub raw: u32,
}

impl SpriteAtlasSettings {
    #[must_use]
    pub const fn packed(self) -> bool {
        self.raw & 1 != 0
    }

    #[must_use]
    pub const fn packing_mode(self) -> u8 {
        ((self.raw >> 1) & 1) as u8
    }

    #[must_use]
    pub const fn packing_rotation(self) -> u8 {
        ((self.raw >> 2) & 0xf) as u8
    }

    #[must_use]
    pub const fn mesh_type(self) -> u8 {
        ((self.raw >> 6) & 1) as u8
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpriteAtlasSecondaryTexture {
    pub texture: ObjectReference,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SpriteAtlasData {
    pub texture: ObjectReference,
    pub alpha_texture: ObjectReference,
    pub texture_rect: SpriteAtlasRect,
    pub texture_rect_offset: SpriteAtlasVector2,
    /// Zero before Unity 2017.2, matching the default C# value.
    pub atlas_rect_offset: SpriteAtlasVector2,
    pub uv_transform: SpriteAtlasVector4,
    pub downscale_multiplier: f32,
    pub settings: SpriteAtlasSettings,
    /// `None` before Unity 2020.2; present (possibly empty) from 2020.2.
    pub secondary_textures: Option<Vec<SpriteAtlasSecondaryTexture>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SpriteAtlasRenderDataEntry {
    pub key: SpriteRenderDataKey,
    pub data: SpriteAtlasData,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SpriteAtlas {
    pub path_id: i64,
    pub name: String,
    pub packed_sprites: Vec<ObjectReference>,
    pub packed_sprite_names: Vec<String>,
    /// Entries sorted by key so lookup is bounded and does not require a
    /// second, allocation-heavy index.
    pub render_data_entries: Vec<SpriteAtlasRenderDataEntry>,
    pub tag: String,
    pub is_variant: bool,
}

impl SpriteAtlas {
    /// Looks up atlas render data with the same composite-key equality as the
    /// C# `Dictionary<KeyValuePair<Guid, long>, SpriteAtlasData>`.
    #[must_use]
    pub fn render_data(&self, key: &SpriteRenderDataKey) -> Option<&SpriteAtlasData> {
        self.render_data_entries
            .binary_search_by_key(key, |entry| entry.key)
            .ok()
            .map(|index| &self.render_data_entries[index].data)
    }

    /// Convenience lookup for the tuple currently exposed by `Sprite`.
    #[must_use]
    pub fn render_data_for_sprite_key(
        &self,
        guid_bytes: &[u8; 16],
        value: i64,
    ) -> Option<&SpriteAtlasData> {
        self.render_data(&SpriteRenderDataKey {
            guid_bytes: *guid_bytes,
            value,
        })
    }
}

/// Parses class `687078895` from its declared object region.
///
/// All reads remain inside `SerializedFile::object_region`; bytes belonging to
/// an adjacent object can never satisfy a truncated atlas payload.
pub fn read_sprite_atlas(
    file: &SerializedFile,
    object_index: usize,
    limits: SpriteAtlasReadLimits,
) -> Result<SpriteAtlas> {
    if file.unity_version.is_stripped() {
        return Err(Error::unsupported(
            "SpriteAtlas requires a Unity version because its layout is version-dependent",
        ));
    }
    let version = file.unity_version.components();
    if !is_verified_sprite_atlas_version(version) {
        return Err(Error::unsupported(format!(
            "SpriteAtlas layout is verified for Unity 2017.1 through 2023.x and Unity 6000.0 through 6000.3, got {}",
            file.unity_version
        )));
    }
    let object = file.objects.get(object_index).ok_or_else(|| {
        Error::invalid_data(format!(
            "serialized object index {object_index} is out of range"
        ))
    })?;
    if object.class_id != SPRITE_ATLAS_CLASS_ID {
        return Err(Error::unsupported(format!(
            "object {} has class ID {}, not SpriteAtlas ({SPRITE_ATLAS_CLASS_ID})",
            object.path_id, object.class_id
        )));
    }

    let region = file.object_region(object_index)?;
    let endian = if file.header.endianness == 0 {
        Endian::Little
    } else {
        Endian::Big
    };
    let mut reader = SpriteAtlasObjectReader {
        reader: EndianReader::new(region.cursor(), endian),
        absolute_start: object.byte_start,
        target_platform: file.target_platform,
        format_version: file.header.version.0,
        version,
        limits,
        total_string_bytes: 0,
        total_secondary_textures: 0,
    };
    let atlas = reader.read_atlas(object.path_id)?;
    reader.finish_known_layout()?;
    Ok(atlas)
}

const fn is_verified_sprite_atlas_version(version: (u32, u32, u32)) -> bool {
    let (major, minor, _) = version;
    ((major > FIRST_SPRITE_ATLAS_VERSION.0
        || (major == FIRST_SPRITE_ATLAS_VERSION.0 && minor >= FIRST_SPRITE_ATLAS_VERSION.1))
        && major <= LAST_VERIFIED_UNITY_MAJOR)
        || (major == UNITY_6_MAJOR && minor <= LAST_VERIFIED_UNITY_6_MINOR)
}

struct SpriteAtlasObjectReader {
    reader: EndianReader<RegionCursor>,
    absolute_start: u64,
    target_platform: i32,
    format_version: u32,
    version: (u32, u32, u32),
    limits: SpriteAtlasReadLimits,
    total_string_bytes: usize,
    total_secondary_textures: usize,
}

impl SpriteAtlasObjectReader {
    fn read_atlas(&mut self, path_id: i64) -> Result<SpriteAtlas> {
        let name = self.read_named_object()?;
        let packed_sprites = self.read_packed_sprites()?;
        let packed_sprite_names = self.read_string_array(
            "SpriteAtlas packed sprite name",
            self.limits.maximum_packed_sprite_names,
        )?;
        let mut render_data_entries = self.read_render_data_entries()?;
        render_data_entries.sort_unstable_by_key(|entry| entry.key);
        if let Some(pair) = render_data_entries
            .windows(2)
            .find(|pair| pair[0].key == pair[1].key)
        {
            return Err(Error::invalid_data(format!(
                "SpriteAtlas render-data map contains duplicate key {:?}",
                pair[0].key
            )));
        }
        let tag = self.read_aligned_string("SpriteAtlas tag")?;
        let is_variant = self.reader.read_bool()?;
        self.align(4)?;

        Ok(SpriteAtlas {
            path_id,
            name,
            packed_sprites,
            packed_sprite_names,
            render_data_entries,
            tag,
            is_variant,
        })
    }

    fn read_named_object(&mut self) -> Result<String> {
        if self.target_platform == NO_TARGET_PLATFORM {
            self.skip(4, "SpriteAtlas Object hide flags")?;
            self.read_pptr()?;
            self.read_pptr()?;
        }
        self.read_aligned_string("SpriteAtlas name")
    }

    fn read_packed_sprites(&mut self) -> Result<Vec<ObjectReference>> {
        let reference_size = self.pptr_size();
        let count = self.read_count(
            "SpriteAtlas packed sprite",
            self.limits.maximum_packed_sprites,
            reference_size,
        )?;
        let mut sprites = reserve_vec(count, "SpriteAtlas packed sprites")?;
        for _ in 0..count {
            sprites.push(self.read_pptr()?);
        }
        Ok(sprites)
    }

    fn read_string_array(&mut self, field: &str, maximum: usize) -> Result<Vec<String>> {
        let count = self.read_count(field, maximum, 4)?;
        let mut strings = reserve_vec(count, field)?;
        for _ in 0..count {
            strings.push(self.read_aligned_string(field)?);
        }
        Ok(strings)
    }

    fn read_render_data_entries(&mut self) -> Result<Vec<SpriteAtlasRenderDataEntry>> {
        let count = self.read_count(
            "SpriteAtlas render-data map",
            self.limits.maximum_render_data_entries,
            self.minimum_render_entry_size(),
        )?;
        let mut entries = reserve_vec(count, "SpriteAtlas render-data map")?;
        for _ in 0..count {
            entries.push(SpriteAtlasRenderDataEntry {
                key: self.read_render_data_key()?,
                data: self.read_render_data()?,
            });
        }
        Ok(entries)
    }

    fn read_render_data_key(&mut self) -> Result<SpriteRenderDataKey> {
        self.ensure_remaining(24, "SpriteAtlas render-data key")?;
        let mut guid_bytes = [0_u8; 16];
        self.reader.get_mut().read_exact(&mut guid_bytes)?;
        let value = self.reader.read_i64()?;
        Ok(SpriteRenderDataKey { guid_bytes, value })
    }

    fn read_render_data(&mut self) -> Result<SpriteAtlasData> {
        let texture = self.read_pptr()?;
        let alpha_texture = self.read_pptr()?;
        let texture_rect = self.read_rect()?;
        let texture_rect_offset = self.read_vector2()?;
        let atlas_rect_offset = if self.version >= (2017, 2, 0) {
            self.read_vector2()?
        } else {
            zero_vector2()
        };
        let uv_transform = self.read_vector4()?;
        let downscale_multiplier = self.reader.read_f32()?;
        let settings = SpriteAtlasSettings {
            raw: self.reader.read_u32()?,
        };
        let secondary_textures = if self.version >= (2020, 2, 0) {
            Some(self.read_secondary_textures()?)
        } else {
            None
        };

        Ok(SpriteAtlasData {
            texture,
            alpha_texture,
            texture_rect,
            texture_rect_offset,
            atlas_rect_offset,
            uv_transform,
            downscale_multiplier,
            settings,
            secondary_textures,
        })
    }

    fn read_secondary_textures(&mut self) -> Result<Vec<SpriteAtlasSecondaryTexture>> {
        let remaining_budget = self
            .limits
            .maximum_secondary_textures
            .saturating_sub(self.total_secondary_textures);
        let count = self.read_count(
            "SpriteAtlas secondary texture",
            remaining_budget,
            self.pptr_size() + 4,
        )?;
        self.total_secondary_textures = self
            .total_secondary_textures
            .checked_add(count)
            .ok_or_else(|| Error::invalid_data("SpriteAtlas secondary texture count overflowed"))?;
        let mut textures = reserve_vec(count, "SpriteAtlas secondary textures")?;
        for _ in 0..count {
            textures.push(SpriteAtlasSecondaryTexture {
                texture: self.read_pptr()?,
                name: self.read_aligned_string("SpriteAtlas secondary texture name")?,
            });
        }
        self.align(4)?;
        Ok(textures)
    }

    fn read_pptr(&mut self) -> Result<ObjectReference> {
        self.ensure_remaining(self.pptr_size(), "SpriteAtlas PPtr")?;
        let file_id = self.reader.read_i32()?;
        let path_id = if self.format_version < 14 {
            i64::from(self.reader.read_i32()?)
        } else {
            self.reader.read_i64()?
        };
        Ok(ObjectReference { file_id, path_id })
    }

    fn read_rect(&mut self) -> Result<SpriteAtlasRect> {
        self.ensure_remaining(16, "SpriteAtlas texture rectangle")?;
        Ok(SpriteAtlasRect {
            x: self.reader.read_f32()?,
            y: self.reader.read_f32()?,
            width: self.reader.read_f32()?,
            height: self.reader.read_f32()?,
        })
    }

    fn read_vector2(&mut self) -> Result<SpriteAtlasVector2> {
        self.ensure_remaining(8, "SpriteAtlas Vector2")?;
        Ok(SpriteAtlasVector2 {
            x: self.reader.read_f32()?,
            y: self.reader.read_f32()?,
        })
    }

    fn read_vector4(&mut self) -> Result<SpriteAtlasVector4> {
        self.ensure_remaining(16, "SpriteAtlas Vector4")?;
        Ok(SpriteAtlasVector4 {
            x: self.reader.read_f32()?,
            y: self.reader.read_f32()?,
            z: self.reader.read_f32()?,
            w: self.reader.read_f32()?,
        })
    }

    fn read_aligned_string(&mut self, field: &str) -> Result<String> {
        self.ensure_remaining(4, field)?;
        let length = checked_length(self.reader.read_i32()?, field)?;
        if length > self.limits.maximum_string_bytes {
            return Err(Error::invalid_data(format!(
                "{field} is {length} bytes, exceeding limit {}",
                self.limits.maximum_string_bytes
            )));
        }
        self.total_string_bytes = self
            .total_string_bytes
            .checked_add(length)
            .ok_or_else(|| Error::invalid_data("SpriteAtlas string byte budget overflowed"))?;
        if self.total_string_bytes > self.limits.maximum_total_string_bytes {
            return Err(Error::invalid_data(format!(
                "SpriteAtlas strings total {} bytes, exceeding limit {}",
                self.total_string_bytes, self.limits.maximum_total_string_bytes
            )));
        }
        self.ensure_remaining(
            u64::try_from(length)
                .map_err(|_| Error::invalid_data(format!("{field} length does not fit in u64")))?,
            field,
        )?;
        let value = self.reader.read_utf8(length)?;
        if length != 0 {
            self.align(4)?;
        }
        Ok(value)
    }

    fn read_count(&mut self, field: &str, maximum: usize, minimum_size: u64) -> Result<usize> {
        self.ensure_remaining(4, field)?;
        let count = checked_length(self.reader.read_i32()?, field)?;
        if count > maximum {
            return Err(Error::invalid_data(format!(
                "{field} count {count} exceeds limit {maximum}"
            )));
        }
        let count_u64 = u64::try_from(count)
            .map_err(|_| Error::invalid_data(format!("{field} count does not fit in u64")))?;
        let minimum_bytes = count_u64
            .checked_mul(minimum_size)
            .ok_or_else(|| Error::invalid_data(format!("{field} minimum byte size overflowed")))?;
        self.ensure_remaining(minimum_bytes, field)?;
        Ok(count)
    }

    const fn pptr_size(&self) -> u64 {
        if self.format_version < 14 { 8 } else { 12 }
    }

    fn minimum_render_entry_size(&self) -> u64 {
        let atlas_rect_size = if self.version >= (2017, 2, 0) { 8 } else { 0 };
        let secondary_count_size = if self.version >= (2020, 2, 0) { 4 } else { 0 };
        24 + 2 * self.pptr_size() + 48 + atlas_rect_size + secondary_count_size
    }

    fn ensure_remaining(&mut self, length: u64, field: &str) -> Result<()> {
        let remaining = self.reader.remaining()?;
        if length > remaining {
            return Err(Error::invalid_data(format!(
                "{field} needs {length} bytes but only {remaining} remain in the SpriteAtlas object"
            )));
        }
        Ok(())
    }

    fn skip(&mut self, length: u64, field: &str) -> Result<()> {
        self.ensure_remaining(length, field)?;
        let position = self.reader.position()?;
        let target = position
            .checked_add(length)
            .ok_or_else(|| Error::invalid_data(format!("{field} position overflowed")))?;
        self.reader.set_position(target)
    }

    fn align(&mut self, alignment: u64) -> Result<()> {
        let position = self.reader.position()?;
        let absolute = self
            .absolute_start
            .checked_add(position)
            .ok_or_else(|| Error::invalid_data("SpriteAtlas alignment position overflowed"))?;
        let remainder = absolute % alignment;
        if remainder == 0 {
            return Ok(());
        }
        self.skip(alignment - remainder, "SpriteAtlas alignment")
    }

    fn finish_known_layout(&mut self) -> Result<()> {
        let remaining = self.reader.remaining()?;
        if remaining != 0 {
            return Err(Error::unsupported(format!(
                "SpriteAtlas object has {remaining} trailing bytes not described by the verified layout"
            )));
        }
        Ok(())
    }
}

const fn zero_vector2() -> SpriteAtlasVector2 {
    SpriteAtlasVector2 { x: 0.0, y: 0.0 }
}

fn reserve_vec<T>(count: usize, field: &str) -> Result<Vec<T>> {
    let mut output = Vec::new();
    output.try_reserve_exact(count).map_err(|error| {
        Error::invalid_data(format!("cannot allocate {count} {field} entries: {error}"))
    })?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use crate::endian::Endian;
    use crate::error::Error;
    use crate::serialized::SerializedFile;
    use crate::source::Region;

    use super::{
        NO_TARGET_PLATFORM, SPRITE_ATLAS_CLASS_ID, SpriteAtlasObjectReader, SpriteAtlasReadLimits,
        SpriteRenderDataKey, read_sprite_atlas,
    };

    const FIRST_KEY: SpriteRenderDataKey = SpriteRenderDataKey {
        guid_bytes: [
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
            0xee, 0xff,
        ],
        value: 0x0102_0304_0506_0708,
    };
    const SECOND_KEY: SpriteRenderDataKey = SpriteRenderDataKey {
        guid_bytes: [0x44; 16],
        value: -71,
    };
    type TestObjectRecord = (i64, i64, u32, i32);

    #[test]
    fn parses_modern_atlas_and_resolves_composite_keys() {
        let object = atlas_object((2022, 3, 62), Endian::Little, 13, &[SECOND_KEY, FIRST_KEY]);
        let file = parse_asset(
            "2022.3.62f1",
            Endian::Little,
            13,
            &[(SPRITE_ATLAS_CLASS_ID, 41, object)],
        );
        let atlas = read_sprite_atlas(&file, 0, SpriteAtlasReadLimits::default()).unwrap();

        assert_eq!(atlas.path_id, 41);
        assert_eq!(atlas.name, "atlas");
        assert_eq!(atlas.packed_sprites.len(), 2);
        assert_eq!(atlas.packed_sprites[0].file_id, 0);
        assert_eq!(atlas.packed_sprites[0].path_id, 101);
        assert_eq!(atlas.packed_sprites[1].file_id, 2);
        assert_eq!(atlas.packed_sprites[1].path_id, -202);
        assert_eq!(atlas.packed_sprite_names, ["hero", "enemy"]);
        assert_eq!(atlas.tag, "ui");
        assert!(atlas.is_variant);
        assert_eq!(atlas.render_data_entries[0].key, FIRST_KEY);
        assert_eq!(atlas.render_data_entries[1].key, SECOND_KEY);

        let data = atlas.render_data(&FIRST_KEY).unwrap();
        assert_eq!(data.texture.file_id, 3);
        assert_eq!(data.texture.path_id, 300);
        assert_eq!(data.alpha_texture.path_id, 400);
        assert_f32(data.texture_rect.x, 1.0);
        assert_f32(data.texture_rect.height, 64.0);
        assert_f32(data.texture_rect_offset.y, 4.0);
        assert_f32(data.atlas_rect_offset.x, 5.0);
        assert_f32(data.uv_transform.w, 10.0);
        assert_f32(data.downscale_multiplier, 0.5);
        assert_eq!(data.settings.raw, 0b0001_0111);
        assert!(data.settings.packed());
        assert_eq!(data.settings.packing_mode(), 1);
        assert_eq!(data.settings.packing_rotation(), 5);
        assert_eq!(data.settings.mesh_type(), 0);
        let secondary = data.secondary_textures.as_ref().unwrap();
        assert_eq!(secondary.len(), 1);
        assert_eq!(secondary[0].texture.path_id, 500);
        assert_eq!(secondary[0].name, "normal");

        assert!(
            atlas
                .render_data_for_sprite_key(&FIRST_KEY.guid_bytes, FIRST_KEY.value)
                .is_some()
        );
        assert!(
            atlas
                .render_data_for_sprite_key(&FIRST_KEY.guid_bytes, -1)
                .is_none()
        );
    }

    #[test]
    fn observes_version_gates_and_big_endian_layout() {
        for (version_string, version, endian, has_offset, has_secondary) in [
            ("2017.1.0f1", (2017, 1, 0), Endian::Little, false, false),
            ("2017.2.0f1", (2017, 2, 0), Endian::Big, true, false),
            ("2020.1.17f1", (2020, 1, 17), Endian::Little, true, false),
            ("2020.2.0f1", (2020, 2, 0), Endian::Big, true, true),
            ("6000.2.0f1", (6000, 2, 0), Endian::Little, true, true),
        ] {
            let object = atlas_object(version, endian, 13, &[FIRST_KEY]);
            let file = parse_asset(
                version_string,
                endian,
                13,
                &[(SPRITE_ATLAS_CLASS_ID, 1, object)],
            );
            let atlas = read_sprite_atlas(&file, 0, SpriteAtlasReadLimits::default()).unwrap();
            let data = atlas.render_data(&FIRST_KEY).unwrap();
            assert_f32(data.atlas_rect_offset.x, if has_offset { 5.0 } else { 0.0 });
            assert_eq!(data.secondary_textures.is_some(), has_secondary);
            assert_eq!(atlas.tag, "ui");
        }
    }

    #[test]
    fn reads_no_target_named_object_prefix() {
        let object = atlas_object(
            (2022, 3, 62),
            Endian::Little,
            NO_TARGET_PLATFORM,
            &[FIRST_KEY],
        );
        let file = parse_asset(
            "2022.3.62f1",
            Endian::Little,
            NO_TARGET_PLATFORM,
            &[(SPRITE_ATLAS_CLASS_ID, 1, object)],
        );

        let atlas = read_sprite_atlas(&file, 0, SpriteAtlasReadLimits::default()).unwrap();
        assert_eq!(atlas.name, "atlas");
        assert!(atlas.render_data(&FIRST_KEY).is_some());
    }

    #[test]
    fn preserves_empty_names_and_empty_collections() {
        let mut object = Vec::new();
        push_aligned_string(&mut object, Endian::Little, "");
        push_i32(&mut object, Endian::Little, 0);
        push_i32(&mut object, Endian::Little, 0);
        push_i32(&mut object, Endian::Little, 0);
        push_aligned_string(&mut object, Endian::Little, "");
        object.push(0);
        align(&mut object, 4);
        let file = parse_asset(
            "2022.3.62f1",
            Endian::Little,
            13,
            &[(SPRITE_ATLAS_CLASS_ID, 1, object)],
        );

        let atlas = read_sprite_atlas(&file, 0, SpriteAtlasReadLimits::default()).unwrap();
        assert!(atlas.name.is_empty());
        assert!(atlas.packed_sprites.is_empty());
        assert!(atlas.packed_sprite_names.is_empty());
        assert!(atlas.render_data_entries.is_empty());
        assert!(atlas.tag.is_empty());
        assert!(!atlas.is_variant);
    }

    #[test]
    fn switches_pptr_path_width_at_serialized_format_14() {
        let mut legacy_bytes = Vec::new();
        push_i32(&mut legacy_bytes, Endian::Little, 7);
        push_i32(&mut legacy_bytes, Endian::Little, -123);
        let mut legacy = object_reader_for_pptr(legacy_bytes, 13, Endian::Little);
        let reference = legacy.read_pptr().unwrap();
        assert_eq!(reference.file_id, 7);
        assert_eq!(reference.path_id, -123);
        assert_eq!(legacy.reader.remaining().unwrap(), 0);

        let path_id = i64::from(i32::MAX) + 99;
        let mut modern_bytes = Vec::new();
        push_i32(&mut modern_bytes, Endian::Big, 8);
        push_i64(&mut modern_bytes, Endian::Big, path_id);
        let mut modern = object_reader_for_pptr(modern_bytes, 14, Endian::Big);
        let reference = modern.read_pptr().unwrap();
        assert_eq!(reference.file_id, 8);
        assert_eq!(reference.path_id, path_id);
        assert_eq!(modern.reader.remaining().unwrap(), 0);
    }

    #[test]
    fn enforces_independent_count_and_string_budgets() {
        let object = atlas_object((2022, 3, 62), Endian::Little, 13, &[FIRST_KEY, SECOND_KEY]);
        let file = parse_asset(
            "2022.3.62f1",
            Endian::Little,
            13,
            &[(SPRITE_ATLAS_CLASS_ID, 1, object)],
        );

        for (limits, expected) in [
            (
                SpriteAtlasReadLimits {
                    maximum_packed_sprites: 1,
                    ..SpriteAtlasReadLimits::default()
                },
                "packed sprite",
            ),
            (
                SpriteAtlasReadLimits {
                    maximum_packed_sprite_names: 1,
                    ..SpriteAtlasReadLimits::default()
                },
                "packed sprite name",
            ),
            (
                SpriteAtlasReadLimits {
                    maximum_render_data_entries: 1,
                    ..SpriteAtlasReadLimits::default()
                },
                "render-data map",
            ),
            (
                SpriteAtlasReadLimits {
                    maximum_secondary_textures: 1,
                    ..SpriteAtlasReadLimits::default()
                },
                "secondary texture",
            ),
            (
                SpriteAtlasReadLimits {
                    maximum_string_bytes: 4,
                    ..SpriteAtlasReadLimits::default()
                },
                "SpriteAtlas name",
            ),
            (
                SpriteAtlasReadLimits {
                    maximum_total_string_bytes: 7,
                    ..SpriteAtlasReadLimits::default()
                },
                "strings total",
            ),
        ] {
            let error = read_sprite_atlas(&file, 0, limits).unwrap_err();
            assert!(
                matches!(error, Error::InvalidData(message) if message.contains(expected)),
                "expected invalid-data error containing {expected:?}"
            );
        }
    }

    #[test]
    fn rejects_negative_or_duplicate_counts_and_unknown_layout_tails() {
        let mut negative = atlas_object((2022, 3, 62), Endian::Little, 13, &[FIRST_KEY]);
        negative[12..16].copy_from_slice(&(-1_i32).to_le_bytes());
        let file = parse_asset(
            "2022.3.62f1",
            Endian::Little,
            13,
            &[(SPRITE_ATLAS_CLASS_ID, 1, negative)],
        );
        let error = read_sprite_atlas(&file, 0, SpriteAtlasReadLimits::default()).unwrap_err();
        assert!(matches!(error, Error::InvalidData(message) if message.contains("negative")));

        let duplicate = atlas_object((2022, 3, 62), Endian::Little, 13, &[FIRST_KEY, FIRST_KEY]);
        let file = parse_asset(
            "2022.3.62f1",
            Endian::Little,
            13,
            &[(SPRITE_ATLAS_CLASS_ID, 1, duplicate)],
        );
        let error = read_sprite_atlas(&file, 0, SpriteAtlasReadLimits::default()).unwrap_err();
        assert!(matches!(error, Error::InvalidData(message) if message.contains("duplicate key")));

        let mut tailed = atlas_object((2022, 3, 62), Endian::Little, 13, &[FIRST_KEY]);
        tailed.extend_from_slice(&[1, 2, 3, 4]);
        let file = parse_asset(
            "2022.3.62f1",
            Endian::Little,
            13,
            &[(SPRITE_ATLAS_CLASS_ID, 1, tailed)],
        );
        let error = read_sprite_atlas(&file, 0, SpriteAtlasReadLimits::default()).unwrap_err();
        assert!(matches!(error, Error::Unsupported(message) if message.contains("trailing bytes")));
    }

    #[test]
    fn truncated_object_cannot_consume_the_following_object() {
        let mut truncated = atlas_object((2022, 3, 62), Endian::Little, 13, &[FIRST_KEY]);
        truncated.truncate(truncated.len() - 2);
        let file = parse_asset(
            "2022.3.62f1",
            Endian::Little,
            13,
            &[
                (SPRITE_ATLAS_CLASS_ID, 1, truncated),
                (1, 2, vec![0_u8; 64]),
            ],
        );

        let error = read_sprite_atlas(&file, 0, SpriteAtlasReadLimits::default()).unwrap_err();
        assert!(
            matches!(error, Error::InvalidData(message) if message.contains("remain in the SpriteAtlas object"))
        );
    }

    #[test]
    fn rejects_unverified_versions_and_wrong_class() {
        // The upper bound moved to 6000.3 on the evidence in the constant's
        // comment, so the refusal case moves with it.
        for version in ["2017.0.4f1", "2024.1.0f1", "6000.4.0f1", ""] {
            let object = atlas_object((2022, 3, 62), Endian::Little, 13, &[FIRST_KEY]);
            let file = parse_asset(
                version,
                Endian::Little,
                13,
                &[(SPRITE_ATLAS_CLASS_ID, 1, object)],
            );
            let error = read_sprite_atlas(&file, 0, SpriteAtlasReadLimits::default()).unwrap_err();
            assert!(
                matches!(error, Error::Unsupported(_)),
                "version {version:?}"
            );
        }

        let object = atlas_object((2022, 3, 62), Endian::Little, 13, &[FIRST_KEY]);
        let file = parse_asset("2022.3.62f1", Endian::Little, 13, &[(1, 1, object)]);
        let error = read_sprite_atlas(&file, 0, SpriteAtlasReadLimits::default()).unwrap_err();
        assert!(
            matches!(error, Error::Unsupported(message) if message.contains("not SpriteAtlas"))
        );
    }

    fn object_reader_for_pptr(
        bytes: Vec<u8>,
        format_version: u32,
        endian: Endian,
    ) -> SpriteAtlasObjectReader {
        let region = Region::from_bytes(bytes);
        SpriteAtlasObjectReader {
            reader: crate::endian::EndianReader::new(region.cursor(), endian),
            absolute_start: 0,
            target_platform: 13,
            format_version,
            version: (2017, 1, 0),
            limits: SpriteAtlasReadLimits::default(),
            total_string_bytes: 0,
            total_secondary_textures: 0,
        }
    }

    fn atlas_object(
        version: (u32, u32, u32),
        endian: Endian,
        target_platform: i32,
        keys: &[SpriteRenderDataKey],
    ) -> Vec<u8> {
        let mut output = Vec::new();
        if target_platform == NO_TARGET_PLATFORM {
            push_u32(&mut output, endian, 0x1122_3344);
            push_pptr(&mut output, endian, 0, 11);
            push_pptr(&mut output, endian, 0, 12);
        }
        push_aligned_string(&mut output, endian, "atlas");
        push_i32(&mut output, endian, 2);
        push_pptr(&mut output, endian, 0, 101);
        push_pptr(&mut output, endian, 2, -202);
        push_i32(&mut output, endian, 2);
        push_aligned_string(&mut output, endian, "hero");
        push_aligned_string(&mut output, endian, "enemy");
        push_i32(&mut output, endian, i32::try_from(keys.len()).unwrap());
        for key in keys {
            output.extend_from_slice(&key.guid_bytes);
            push_i64(&mut output, endian, key.value);
            push_atlas_data(&mut output, version, endian);
        }
        push_aligned_string(&mut output, endian, "ui");
        output.push(1);
        align(&mut output, 4);
        output
    }

    fn push_atlas_data(output: &mut Vec<u8>, version: (u32, u32, u32), endian: Endian) {
        push_pptr(output, endian, 3, 300);
        push_pptr(output, endian, 0, 400);
        push_floats(output, endian, &[1.0, 2.0, 32.0, 64.0]);
        push_floats(output, endian, &[3.0, 4.0]);
        if version >= (2017, 2, 0) {
            push_floats(output, endian, &[5.0, 6.0]);
        }
        push_floats(output, endian, &[7.0, 8.0, 9.0, 10.0]);
        push_floats(output, endian, &[0.5]);
        push_u32(output, endian, 0b0001_0111);
        if version >= (2020, 2, 0) {
            push_i32(output, endian, 1);
            push_pptr(output, endian, 0, 500);
            push_aligned_string(output, endian, "normal");
            align(output, 4);
        }
    }

    fn parse_asset(
        version: &str,
        endian: Endian,
        target_platform: i32,
        objects: &[(i32, i64, Vec<u8>)],
    ) -> SerializedFile {
        SerializedFile::open(Region::from_bytes(synthetic_v22_asset(
            version,
            endian,
            target_platform,
            objects,
        )))
        .unwrap()
    }

    fn synthetic_v22_asset(
        version: &str,
        endian: Endian,
        target_platform: i32,
        objects: &[(i32, i64, Vec<u8>)],
    ) -> Vec<u8> {
        let classes = unique_classes(objects);
        let (data, object_records) = object_data(objects, &classes);
        let metadata = v22_metadata(version, endian, target_platform, &classes, &object_records);
        assemble_v22_file(endian, &metadata, &data)
    }

    fn unique_classes(objects: &[(i32, i64, Vec<u8>)]) -> Vec<i32> {
        let mut classes = Vec::new();
        for (class_id, _, _) in objects {
            if !classes.contains(class_id) {
                classes.push(*class_id);
            }
        }
        classes
    }

    fn object_data(
        objects: &[(i32, i64, Vec<u8>)],
        classes: &[i32],
    ) -> (Vec<u8>, Vec<TestObjectRecord>) {
        let mut data = Vec::new();
        let mut records = Vec::new();
        for (class_id, path_id, object) in objects {
            align(&mut data, 4);
            let offset = i64::try_from(data.len()).unwrap();
            let type_index = classes.iter().position(|value| value == class_id).unwrap();
            records.push((
                *path_id,
                offset,
                u32::try_from(object.len()).unwrap(),
                i32::try_from(type_index).unwrap(),
            ));
            data.extend_from_slice(object);
        }
        (data, records)
    }

    fn v22_metadata(
        version: &str,
        endian: Endian,
        target_platform: i32,
        classes: &[i32],
        object_records: &[TestObjectRecord],
    ) -> Vec<u8> {
        let mut metadata = Vec::new();
        metadata.extend_from_slice(version.as_bytes());
        metadata.push(0);
        push_i32(&mut metadata, endian, target_platform);
        metadata.push(0);
        push_i32(&mut metadata, endian, i32::try_from(classes.len()).unwrap());
        for class_id in classes {
            push_i32(&mut metadata, endian, *class_id);
            metadata.push(0);
            push_i16(&mut metadata, endian, -1);
            metadata.extend_from_slice(&[0_u8; 16]);
        }
        push_object_records(&mut metadata, endian, object_records);
        push_i32(&mut metadata, endian, 0);
        push_i32(&mut metadata, endian, 0);
        push_i32(&mut metadata, endian, 0);
        metadata.push(0);
        metadata
    }

    fn push_object_records(metadata: &mut Vec<u8>, endian: Endian, records: &[TestObjectRecord]) {
        push_i32(metadata, endian, i32::try_from(records.len()).unwrap());
        for (path_id, offset, size, type_index) in records {
            align_with_base(metadata, 48, 4);
            push_i64(metadata, endian, *path_id);
            push_i64(metadata, endian, *offset);
            push_u32(metadata, endian, *size);
            push_i32(metadata, endian, *type_index);
        }
    }

    fn assemble_v22_file(endian: Endian, metadata: &[u8], data: &[u8]) -> Vec<u8> {
        let metadata_size = u32::try_from(metadata.len()).unwrap();
        let metadata_end = 48_u64 + u64::from(metadata_size);
        let data_offset = metadata_end.div_ceil(16) * 16;
        let file_size = data_offset + u64::try_from(data.len()).unwrap();
        let mut bytes = vec![0_u8; 48];
        bytes[8..12].copy_from_slice(&22_u32.to_be_bytes());
        bytes[16] = u8::from(endian == Endian::Big);
        bytes[20..24].copy_from_slice(&metadata_size.to_be_bytes());
        bytes[24..32].copy_from_slice(&i64::try_from(file_size).unwrap().to_be_bytes());
        bytes[32..40].copy_from_slice(&i64::try_from(data_offset).unwrap().to_be_bytes());
        bytes.extend_from_slice(metadata);
        bytes.resize(usize::try_from(data_offset).unwrap(), 0);
        bytes.extend_from_slice(data);
        bytes
    }

    fn push_pptr(output: &mut Vec<u8>, endian: Endian, file_id: i32, path_id: i64) {
        push_i32(output, endian, file_id);
        push_i64(output, endian, path_id);
    }

    fn push_aligned_string(output: &mut Vec<u8>, endian: Endian, value: &str) {
        push_i32(output, endian, i32::try_from(value.len()).unwrap());
        output.extend_from_slice(value.as_bytes());
        if !value.is_empty() {
            align(output, 4);
        }
    }

    fn push_floats(output: &mut Vec<u8>, endian: Endian, values: &[f32]) {
        for value in values {
            push_u32(output, endian, value.to_bits());
        }
    }

    fn push_i16(output: &mut Vec<u8>, endian: Endian, value: i16) {
        let bytes = match endian {
            Endian::Little => value.to_le_bytes(),
            Endian::Big => value.to_be_bytes(),
        };
        output.extend_from_slice(&bytes);
    }

    fn push_i32(output: &mut Vec<u8>, endian: Endian, value: i32) {
        let bytes = match endian {
            Endian::Little => value.to_le_bytes(),
            Endian::Big => value.to_be_bytes(),
        };
        output.extend_from_slice(&bytes);
    }

    fn push_u32(output: &mut Vec<u8>, endian: Endian, value: u32) {
        let bytes = match endian {
            Endian::Little => value.to_le_bytes(),
            Endian::Big => value.to_be_bytes(),
        };
        output.extend_from_slice(&bytes);
    }

    fn push_i64(output: &mut Vec<u8>, endian: Endian, value: i64) {
        let bytes = match endian {
            Endian::Little => value.to_le_bytes(),
            Endian::Big => value.to_be_bytes(),
        };
        output.extend_from_slice(&bytes);
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

    fn assert_f32(actual: f32, expected: f32) {
        assert_eq!(actual.to_bits(), expected.to_bits());
    }
}
