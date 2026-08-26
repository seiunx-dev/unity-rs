//! Bounded parsing for Unity `Material` property sheets.
//!
//! The managed implementation consumes the shader reference and saved property
//! sheet, but deliberately leaves newer `m_BuildTextureStacks` tail data
//! untouched. This module preserves that boundary instead of guessing a layout
//! for the trailing data.

use crate::endian::{Endian, EndianReader, checked_length};
use crate::serialized::{ObjectReference, SerializedFile};
use crate::source::{Region, RegionCursor};
use crate::{Error, Result};

pub const MATERIAL_CLASS_ID: i32 = 21;

const NO_TARGET_PLATFORM: i32 = -2;

/// Defensive limits for one material object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MaterialReadLimits {
    pub maximum_object_bytes: u64,
    pub maximum_string_bytes: usize,
    pub maximum_total_string_bytes: usize,
    pub maximum_array_elements: usize,
}

impl Default for MaterialReadLimits {
    fn default() -> Self {
        Self {
            maximum_object_bytes: 256 * 1024 * 1024,
            maximum_string_bytes: 16 * 1024 * 1024,
            maximum_total_string_bytes: 64 * 1024 * 1024,
            maximum_array_elements: 1_000_000,
        }
    }
}

/// A named entry in a Unity property sheet. Duplicate names remain distinct.
#[derive(Debug, Clone, PartialEq)]
pub struct NamedMaterialProperty<T> {
    pub name: String,
    pub value: T,
}

/// One texture binding and its UV transform.
#[derive(Debug, Clone, PartialEq)]
pub struct MaterialTextureEnvironment {
    pub texture: ObjectReference,
    pub scale: [f32; 2],
    pub offset: [f32; 2],
}

/// Serialized values retained by Unity's `m_SavedProperties` field.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MaterialPropertySheet {
    pub texture_environments: Vec<NamedMaterialProperty<MaterialTextureEnvironment>>,
    pub integers: Vec<NamedMaterialProperty<i32>>,
    pub floats: Vec<NamedMaterialProperty<f32>>,
    pub colors: Vec<NamedMaterialProperty<[f32; 4]>>,
}

/// The material fields used by `AssetStudio`'s model conversion path.
#[derive(Debug, Clone, PartialEq)]
pub struct Material {
    pub path_id: i64,
    pub name: String,
    pub shader: ObjectReference,
    /// Unity 4.1-4.x array, or the single Unity 5-2021.2.17 keyword string.
    pub legacy_shader_keywords: Vec<String>,
    pub valid_keywords: Vec<String>,
    pub invalid_keywords: Vec<String>,
    pub lightmap_flags: Option<u32>,
    pub enable_instancing_variants: Option<bool>,
    pub custom_render_queue: Option<i32>,
    pub string_tags: Vec<(String, String)>,
    pub disabled_shader_passes: Vec<String>,
    pub saved_properties: MaterialPropertySheet,
    /// Bytes intentionally left for newer fields not consumed by the C# reader.
    pub trailing_bytes: u64,
}

/// Reads the material at `object_index` without resolving any of its `PPtrs`.
pub fn read_material(
    file: &SerializedFile,
    object_index: usize,
    limits: MaterialReadLimits,
) -> Result<Material> {
    if file.unity_version.is_stripped() {
        return Err(Error::unsupported(
            "Material requires a Unity version because its layout is version-dependent",
        ));
    }
    if file.unity_version.components() < (4, 1, 0) {
        return Err(Error::unsupported(
            "Material layouts before Unity 4.1 are not sample-verified",
        ));
    }

    let mut reader = MaterialReader::new(file, object_index, limits)?;
    let name = reader.read_named_object()?;
    let shader = reader.read_pptr()?;
    let version = file.unity_version.components();

    let mut legacy_shader_keywords = Vec::new();
    let mut valid_keywords = Vec::new();
    let mut invalid_keywords = Vec::new();
    if file.unity_version.major == 4 && file.unity_version.minor >= 1 {
        legacy_shader_keywords = reader.read_string_array("Material shader keyword")?;
    } else if version >= (2021, 2, 18) {
        valid_keywords = reader.read_string_array("Material valid keyword")?;
        invalid_keywords = reader.read_string_array("Material invalid keyword")?;
    } else if file.unity_version.major >= 5 {
        legacy_shader_keywords.push(reader.read_aligned_string("Material shader keywords")?);
    }

    let lightmap_flags = (file.unity_version.major >= 5)
        .then(|| reader.reader.read_u32())
        .transpose()?;
    let enable_instancing_variants = if version >= (5, 6, 0) {
        let value = reader.reader.read_bool()?;
        reader.align(4)?;
        Some(value)
    } else {
        None
    };
    let custom_render_queue = (version >= (4, 3, 0))
        .then(|| reader.reader.read_i32())
        .transpose()?;

    let string_tags = if version >= (5, 1, 0) {
        reader.read_string_pairs("Material string tag")?
    } else {
        Vec::new()
    };
    let disabled_shader_passes = if version >= (5, 6, 0) {
        reader.read_string_array("Material disabled shader pass")?
    } else {
        Vec::new()
    };
    let saved_properties = reader.read_property_sheet(file.unity_version.major >= 2021)?;
    let trailing_bytes = reader.reader.remaining()?;

    Ok(Material {
        path_id: reader.path_id,
        name,
        shader,
        legacy_shader_keywords,
        valid_keywords,
        invalid_keywords,
        lightmap_flags,
        enable_instancing_variants,
        custom_render_queue,
        string_tags,
        disabled_shader_passes,
        saved_properties,
        trailing_bytes,
    })
}

struct MaterialReader {
    region: Region,
    reader: EndianReader<RegionCursor>,
    absolute_start: u64,
    path_id: i64,
    target_platform: i32,
    format_version: u32,
    limits: MaterialReadLimits,
    array_elements: usize,
    string_bytes: usize,
}

impl MaterialReader {
    fn new(file: &SerializedFile, object_index: usize, limits: MaterialReadLimits) -> Result<Self> {
        let object = file.objects.get(object_index).ok_or_else(|| {
            Error::invalid_data(format!(
                "serialized object index {object_index} is out of range"
            ))
        })?;
        if object.class_id != MATERIAL_CLASS_ID {
            return Err(Error::unsupported(format!(
                "object {} has class ID {}, expected {MATERIAL_CLASS_ID}",
                object.path_id, object.class_id
            )));
        }
        if object.byte_size > limits.maximum_object_bytes {
            return Err(Error::invalid_data(format!(
                "Material object is {} bytes, exceeding limit {}",
                object.byte_size, limits.maximum_object_bytes
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
            array_elements: 0,
            string_bytes: 0,
        })
    }

    fn read_named_object(&mut self) -> Result<String> {
        if self.target_platform == NO_TARGET_PLATFORM {
            self.skip(4, "Object hide flags")?;
            let _prefab_parent = self.read_pptr()?;
            let _prefab_internal = self.read_pptr()?;
        }
        self.read_aligned_string("Material name")
    }

    fn read_pptr(&mut self) -> Result<ObjectReference> {
        let file_id = self.reader.read_i32()?;
        let path_id = if self.format_version < 14 {
            i64::from(self.reader.read_i32()?)
        } else {
            self.reader.read_i64()?
        };
        Ok(ObjectReference { file_id, path_id })
    }

    fn read_aligned_string(&mut self, field: &str) -> Result<String> {
        let length = checked_length(self.reader.read_i32()?, field)?;
        if length > self.limits.maximum_string_bytes {
            return Err(Error::invalid_data(format!(
                "{field} is {length} bytes, exceeding limit {}",
                self.limits.maximum_string_bytes
            )));
        }
        let worst_case = length
            .checked_mul(3)
            .ok_or_else(|| Error::invalid_data(format!("{field} string budget overflowed")))?;
        self.string_bytes = self
            .string_bytes
            .checked_add(worst_case)
            .ok_or_else(|| Error::invalid_data("Material string budget overflowed"))?;
        if self.string_bytes > self.limits.maximum_total_string_bytes {
            return Err(Error::invalid_data(format!(
                "Material strings need at most {} bytes, exceeding limit {}",
                self.string_bytes, self.limits.maximum_total_string_bytes
            )));
        }
        let value = self.reader.read_utf8(length)?;
        if length != 0 {
            self.align(4)?;
        }
        Ok(value)
    }

    fn read_count(&mut self, field: &str, minimum_record_bytes: usize) -> Result<usize> {
        let count = checked_length(self.reader.read_i32()?, field)?;
        self.array_elements = self
            .array_elements
            .checked_add(count)
            .ok_or_else(|| Error::invalid_data("Material array element budget overflowed"))?;
        if self.array_elements > self.limits.maximum_array_elements {
            return Err(Error::invalid_data(format!(
                "Material arrays contain {} elements, exceeding limit {}",
                self.array_elements, self.limits.maximum_array_elements
            )));
        }
        let minimum_bytes = count
            .checked_mul(minimum_record_bytes)
            .ok_or_else(|| Error::invalid_data(format!("{field} byte size overflowed")))?;
        if u64::try_from(minimum_bytes).unwrap_or(u64::MAX) > self.reader.remaining()? {
            return Err(Error::invalid_data(format!(
                "{field} needs at least {minimum_bytes} bytes beyond the bounded object payload"
            )));
        }
        Ok(count)
    }

    fn read_string_array(&mut self, field: &str) -> Result<Vec<String>> {
        let count = self.read_count(field, 4)?;
        let mut values = try_vec_capacity(count, field)?;
        for _ in 0..count {
            values.push(self.read_aligned_string(field)?);
        }
        Ok(values)
    }

    fn read_string_pairs(&mut self, field: &str) -> Result<Vec<(String, String)>> {
        let count = self.read_count(field, 8)?;
        let mut values = try_vec_capacity(count, field)?;
        for _ in 0..count {
            values.push((
                self.read_aligned_string(&format!("{field} key"))?,
                self.read_aligned_string(&format!("{field} value"))?,
            ));
        }
        Ok(values)
    }

    fn read_property_sheet(
        &mut self,
        has_integer_properties: bool,
    ) -> Result<MaterialPropertySheet> {
        let texture_count = self.read_count(
            "Material texture environment",
            4_usize
                .checked_add(self.pptr_size())
                .and_then(|value| value.checked_add(16))
                .ok_or_else(|| Error::invalid_data("Material texture record size overflowed"))?,
        )?;
        let mut texture_environments =
            try_vec_capacity(texture_count, "Material texture environment")?;
        for _ in 0..texture_count {
            let name = self.read_aligned_string("Material texture property name")?;
            let texture = self.read_pptr()?;
            let scale = [self.reader.read_f32()?, self.reader.read_f32()?];
            let offset = [self.reader.read_f32()?, self.reader.read_f32()?];
            texture_environments.push(NamedMaterialProperty {
                name,
                value: MaterialTextureEnvironment {
                    texture,
                    scale,
                    offset,
                },
            });
        }

        let integers = if has_integer_properties {
            self.read_scalar_properties("Material integer property", |reader| {
                reader.reader.read_i32()
            })?
        } else {
            Vec::new()
        };
        let floats = self
            .read_scalar_properties("Material float property", |reader| reader.reader.read_f32())?;

        let color_count = self.read_count("Material color property", 20)?;
        let mut colors = try_vec_capacity(color_count, "Material color property")?;
        for _ in 0..color_count {
            let name = self.read_aligned_string("Material color property name")?;
            let value = [
                self.reader.read_f32()?,
                self.reader.read_f32()?,
                self.reader.read_f32()?,
                self.reader.read_f32()?,
            ];
            colors.push(NamedMaterialProperty { name, value });
        }

        Ok(MaterialPropertySheet {
            texture_environments,
            integers,
            floats,
            colors,
        })
    }

    fn read_scalar_properties<T>(
        &mut self,
        field: &str,
        mut read_value: impl FnMut(&mut Self) -> Result<T>,
    ) -> Result<Vec<NamedMaterialProperty<T>>> {
        let count = self.read_count(field, 8)?;
        let mut values = try_vec_capacity(count, field)?;
        for _ in 0..count {
            let name = self.read_aligned_string(&format!("{field} name"))?;
            values.push(NamedMaterialProperty {
                name,
                value: read_value(self)?,
            });
        }
        Ok(values)
    }

    const fn pptr_size(&self) -> usize {
        if self.format_version < 14 { 8 } else { 12 }
    }

    fn align(&mut self, alignment: u64) -> Result<()> {
        let position = self.reader.position()?;
        let absolute = self
            .absolute_start
            .checked_add(position)
            .ok_or_else(|| Error::invalid_data("Material alignment position overflowed"))?;
        let remainder = absolute % alignment;
        if remainder == 0 {
            return Ok(());
        }
        self.skip(alignment - remainder, "Material alignment")
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
}

fn try_vec_capacity<T>(count: usize, field: &str) -> Result<Vec<T>> {
    let mut values = Vec::new();
    values.try_reserve_exact(count).map_err(|error| {
        Error::invalid_data(format!("cannot allocate {field} entries: {error}"))
    })?;
    Ok(values)
}

#[cfg(test)]
mod tests {
    use crate::serialized::SerializedFile;
    use crate::source::Region;

    use super::{MATERIAL_CLASS_ID, MaterialReadLimits, read_material};

    #[test]
    fn reads_modern_material_properties_without_collapsing_duplicates() {
        let endian = TestEndian::Little;
        let object = modern_object(endian);
        let file = parse_asset(22, endian, 13, "2022.3.62f1", &object, MATERIAL_CLASS_ID);

        let material = read_material(&file, 0, MaterialReadLimits::default()).unwrap();

        assert_eq!(material.path_id, 7);
        assert_eq!(material.name, "hero material");
        assert_eq!((material.shader.file_id, material.shader.path_id), (1, 42));
        assert!(material.legacy_shader_keywords.is_empty());
        assert_eq!(material.valid_keywords, ["FOO", "BAR"]);
        assert_eq!(material.invalid_keywords, ["OLD"]);
        assert_eq!(material.lightmap_flags, Some(3));
        assert_eq!(material.enable_instancing_variants, Some(true));
        assert_eq!(material.custom_render_queue, Some(2_450));
        assert_eq!(
            material.string_tags,
            [
                ("RenderType".to_owned(), "Opaque".to_owned()),
                ("RenderType".to_owned(), "Cutout".to_owned())
            ]
        );
        assert_eq!(material.disabled_shader_passes, ["ShadowCaster"]);
        let texture = &material.saved_properties.texture_environments[0];
        assert_eq!(texture.name, "_MainTex");
        assert_eq!(texture.value.texture.path_id, 9);
        assert_eq!(
            texture.value.scale.map(f32::to_bits),
            [2.0_f32.to_bits(), 3.0_f32.to_bits()]
        );
        assert_eq!(
            texture.value.offset.map(f32::to_bits),
            [0.25_f32.to_bits(), 0.5_f32.to_bits()]
        );
        assert_eq!(material.saved_properties.integers.len(), 2);
        assert_eq!(material.saved_properties.integers[0].name, "_Mode");
        assert_eq!(material.saved_properties.integers[1].name, "_Mode");
        assert_eq!(material.saved_properties.integers[1].value, 2);
        assert_eq!(
            material.saved_properties.floats[0].value.to_bits(),
            0.75_f32.to_bits()
        );
        assert_eq!(
            material.saved_properties.colors[0].value.map(f32::to_bits),
            [
                1.0_f32.to_bits(),
                0.5_f32.to_bits(),
                0.25_f32.to_bits(),
                1.0_f32.to_bits()
            ]
        );
        assert_eq!(material.trailing_bytes, 4);
    }

    #[test]
    fn reads_big_endian_unity4_material_with_32_bit_pptrs_and_no_target_prefix() {
        let endian = TestEndian::Big;
        let mut object = named_object(endian, "legacy", true, 9);
        push_pptr(&mut object, endian, 2, 0x1020_3040, 9);
        push_string_array(&mut object, endian, &["LIGHTMAP_ON", "FOG_LINEAR"]);
        endian.push_i32(&mut object, 2_000);
        endian.push_i32(&mut object, 1);
        push_aligned_string(&mut object, endian, "_MainTex");
        push_pptr(&mut object, endian, 0, 17, 9);
        push_f32s(&mut object, endian, &[1.0, 1.0, 0.0, 0.0]);
        endian.push_i32(&mut object, 1);
        push_named_f32(&mut object, endian, "_Shininess", 0.25);
        endian.push_i32(&mut object, 0);
        let file = parse_asset(9, endian, -2, "4.7.2f1", &object, MATERIAL_CLASS_ID);

        let material = read_material(&file, 0, MaterialReadLimits::default()).unwrap();

        assert_eq!(material.name, "legacy");
        assert_eq!(
            (material.shader.file_id, material.shader.path_id),
            (2, 0x1020_3040)
        );
        assert_eq!(
            material.legacy_shader_keywords,
            ["LIGHTMAP_ON", "FOG_LINEAR"]
        );
        assert_eq!(material.lightmap_flags, None);
        assert_eq!(material.enable_instancing_variants, None);
        assert_eq!(material.custom_render_queue, Some(2_000));
        assert!(material.string_tags.is_empty());
        assert!(material.disabled_shader_passes.is_empty());
        assert!(material.saved_properties.integers.is_empty());
        assert_eq!(material.saved_properties.floats[0].name, "_Shininess");
        assert_eq!(
            material.saved_properties.floats[0].value.to_bits(),
            0.25_f32.to_bits()
        );
        assert_eq!(material.trailing_bytes, 0);
    }

    #[test]
    fn rejects_wrong_class_stripped_version_truncation_and_budgets() {
        let endian = TestEndian::Little;
        let object = modern_object(endian);
        let file = parse_asset(22, endian, 13, "2022.3.62f1", &object, MATERIAL_CLASS_ID);

        let object_limit = MaterialReadLimits {
            maximum_object_bytes: u64::try_from(object.len() - 1).unwrap(),
            ..MaterialReadLimits::default()
        };
        assert!(read_material(&file, 0, object_limit).is_err());
        let string_limit = MaterialReadLimits {
            maximum_total_string_bytes: 1,
            ..MaterialReadLimits::default()
        };
        assert!(read_material(&file, 0, string_limit).is_err());
        let element_limit = MaterialReadLimits {
            maximum_array_elements: 0,
            ..MaterialReadLimits::default()
        };
        assert!(read_material(&file, 0, element_limit).is_err());

        let truncated = parse_asset(
            22,
            endian,
            13,
            "2022.3.62f1",
            &object[..object.len() - 8],
            MATERIAL_CLASS_ID,
        );
        assert!(read_material(&truncated, 0, MaterialReadLimits::default()).is_err());
        let wrong_class = parse_asset(22, endian, 13, "2022.3.62f1", &object, 49);
        assert!(read_material(&wrong_class, 0, MaterialReadLimits::default()).is_err());
        let stripped = parse_asset(22, endian, 13, "0.0.0", &object, MATERIAL_CLASS_ID);
        assert!(read_material(&stripped, 0, MaterialReadLimits::default()).is_err());
    }

    #[test]
    fn switches_keyword_layout_at_2021_2_18() {
        let endian = TestEndian::Little;
        let before = keyword_gate_object(endian, false);
        let before_file = parse_asset(22, endian, 13, "2021.2.17f1", &before, MATERIAL_CLASS_ID);
        let before_material =
            read_material(&before_file, 0, MaterialReadLimits::default()).unwrap();
        assert_eq!(before_material.legacy_shader_keywords, ["FOO BAR"]);
        assert!(before_material.valid_keywords.is_empty());

        let after = keyword_gate_object(endian, true);
        let after_file = parse_asset(22, endian, 13, "2021.2.18f1", &after, MATERIAL_CLASS_ID);
        let after_material = read_material(&after_file, 0, MaterialReadLimits::default()).unwrap();
        assert!(after_material.legacy_shader_keywords.is_empty());
        assert_eq!(after_material.valid_keywords, ["FOO"]);
        assert_eq!(after_material.invalid_keywords, ["BAR"]);
    }

    fn modern_object(endian: TestEndian) -> Vec<u8> {
        let mut object = named_object(endian, "hero material", false, 22);
        push_pptr(&mut object, endian, 1, 42, 22);
        push_string_array(&mut object, endian, &["FOO", "BAR"]);
        push_string_array(&mut object, endian, &["OLD"]);
        endian.push_u32(&mut object, 3);
        object.push(1);
        align(&mut object, 4);
        endian.push_i32(&mut object, 2_450);
        endian.push_i32(&mut object, 2);
        push_aligned_string(&mut object, endian, "RenderType");
        push_aligned_string(&mut object, endian, "Opaque");
        push_aligned_string(&mut object, endian, "RenderType");
        push_aligned_string(&mut object, endian, "Cutout");
        push_string_array(&mut object, endian, &["ShadowCaster"]);
        endian.push_i32(&mut object, 1);
        push_aligned_string(&mut object, endian, "_MainTex");
        push_pptr(&mut object, endian, 0, 9, 22);
        push_f32s(&mut object, endian, &[2.0, 3.0, 0.25, 0.5]);
        endian.push_i32(&mut object, 2);
        push_named_i32(&mut object, endian, "_Mode", 1);
        push_named_i32(&mut object, endian, "_Mode", 2);
        endian.push_i32(&mut object, 1);
        push_named_f32(&mut object, endian, "_Glossiness", 0.75);
        endian.push_i32(&mut object, 1);
        push_aligned_string(&mut object, endian, "_Color");
        push_f32s(&mut object, endian, &[1.0, 0.5, 0.25, 1.0]);
        // The managed reader deliberately leaves the 2020+ BuildTextureStacks
        // vector untouched. An empty vector still contributes its count.
        endian.push_i32(&mut object, 0);
        object
    }

    fn keyword_gate_object(endian: TestEndian, split_keywords: bool) -> Vec<u8> {
        let mut object = named_object(endian, "keyword gate", false, 22);
        push_pptr(&mut object, endian, 0, 0, 22);
        if split_keywords {
            push_string_array(&mut object, endian, &["FOO"]);
            push_string_array(&mut object, endian, &["BAR"]);
        } else {
            push_aligned_string(&mut object, endian, "FOO BAR");
        }
        endian.push_u32(&mut object, 0);
        object.push(0);
        align(&mut object, 4);
        endian.push_i32(&mut object, 0);
        endian.push_i32(&mut object, 0);
        push_string_array(&mut object, endian, &[]);
        for _ in 0..4 {
            endian.push_i32(&mut object, 0);
        }
        // Empty BuildTextureStacks vector, deliberately left unread.
        endian.push_i32(&mut object, 0);
        object
    }

    fn named_object(
        endian: TestEndian,
        name: &str,
        no_target: bool,
        format_version: u32,
    ) -> Vec<u8> {
        let mut output = Vec::new();
        if no_target {
            endian.push_u32(&mut output, 0);
            push_pptr(&mut output, endian, 0, 0, format_version);
            push_pptr(&mut output, endian, 0, 0, format_version);
        }
        push_aligned_string(&mut output, endian, name);
        output
    }

    fn push_string_array(output: &mut Vec<u8>, endian: TestEndian, values: &[&str]) {
        endian.push_i32(output, i32::try_from(values.len()).unwrap());
        for value in values {
            push_aligned_string(output, endian, value);
        }
    }

    fn push_named_i32(output: &mut Vec<u8>, endian: TestEndian, name: &str, value: i32) {
        push_aligned_string(output, endian, name);
        endian.push_i32(output, value);
    }

    fn push_named_f32(output: &mut Vec<u8>, endian: TestEndian, name: &str, value: f32) {
        push_aligned_string(output, endian, name);
        endian.push_f32(output, value);
    }

    fn push_f32s(output: &mut Vec<u8>, endian: TestEndian, values: &[f32]) {
        for value in values {
            endian.push_f32(output, *value);
        }
    }

    fn push_pptr(
        output: &mut Vec<u8>,
        endian: TestEndian,
        file_id: i32,
        path_id: i64,
        format_version: u32,
    ) {
        endian.push_i32(output, file_id);
        if format_version < 14 {
            endian.push_i32(output, i32::try_from(path_id).unwrap());
        } else {
            endian.push_i64(output, path_id);
        }
    }

    fn push_aligned_string(output: &mut Vec<u8>, endian: TestEndian, value: &str) {
        endian.push_i32(output, i32::try_from(value.len()).unwrap());
        output.extend_from_slice(value.as_bytes());
        if !value.is_empty() {
            align(output, 4);
        }
    }

    fn parse_asset(
        format_version: u32,
        endian: TestEndian,
        target_platform: i32,
        unity_version: &str,
        object: &[u8],
        class_id: i32,
    ) -> SerializedFile {
        let bytes = synthetic_asset(
            format_version,
            endian,
            target_platform,
            unity_version,
            object,
            class_id,
        );
        SerializedFile::open(Region::from_bytes(bytes)).unwrap()
    }

    #[allow(clippy::too_many_lines)]
    fn synthetic_asset(
        format_version: u32,
        endian: TestEndian,
        target_platform: i32,
        unity_version: &str,
        object: &[u8],
        class_id: i32,
    ) -> Vec<u8> {
        assert!(matches!(format_version, 9 | 13 | 22));
        if format_version == 9 {
            return synthetic_v9_asset(endian, target_platform, unity_version, object, class_id);
        }
        let metadata_base = if format_version == 22 { 48 } else { 20 };
        let mut metadata = Vec::new();
        metadata.extend_from_slice(unity_version.as_bytes());
        metadata.push(0);
        endian.push_i32(&mut metadata, target_platform);
        metadata.push(0);
        endian.push_i32(&mut metadata, 1);
        endian.push_i32(&mut metadata, class_id);
        if format_version == 22 {
            metadata.push(0);
            endian.push_i16(&mut metadata, -1);
        }
        metadata.extend_from_slice(&[0_u8; 16]);
        if format_version == 13 {
            endian.push_i32(&mut metadata, 0);
        }
        endian.push_i32(&mut metadata, 1);
        if format_version == 22 {
            align_with_base(&mut metadata, metadata_base, 4);
            endian.push_i64(&mut metadata, 7);
            endian.push_i64(&mut metadata, 0);
        } else {
            endian.push_i32(&mut metadata, 7);
            endian.push_u32(&mut metadata, 0);
        }
        endian.push_u32(&mut metadata, u32::try_from(object.len()).unwrap());
        if format_version == 22 {
            endian.push_i32(&mut metadata, 0);
        } else {
            endian.push_i32(&mut metadata, class_id);
            endian.push_u16(&mut metadata, u16::try_from(class_id).unwrap());
            endian.push_i16(&mut metadata, -1);
        }
        endian.push_i32(&mut metadata, 0);
        endian.push_i32(&mut metadata, 0);
        if format_version == 22 {
            endian.push_i32(&mut metadata, 0);
        }
        metadata.push(0);

        let metadata_size = u32::try_from(metadata.len()).unwrap();
        let data_offset = (metadata_base + metadata.len()).next_multiple_of(16);
        let file_size = data_offset + object.len();
        let mut bytes = vec![0_u8; metadata_base];
        bytes[0..4].copy_from_slice(&metadata_size.to_be_bytes());
        bytes[4..8].copy_from_slice(&u32::try_from(file_size).unwrap().to_be_bytes());
        bytes[8..12].copy_from_slice(&format_version.to_be_bytes());
        bytes[12..16].copy_from_slice(&u32::try_from(data_offset).unwrap().to_be_bytes());
        bytes[16] = endian.marker();
        if format_version == 22 {
            bytes[20..24].copy_from_slice(&metadata_size.to_be_bytes());
            bytes[24..32].copy_from_slice(&i64::try_from(file_size).unwrap().to_be_bytes());
            bytes[32..40].copy_from_slice(&i64::try_from(data_offset).unwrap().to_be_bytes());
        }
        bytes.extend_from_slice(&metadata);
        bytes.resize(data_offset, 0);
        bytes.extend_from_slice(object);
        bytes
    }

    fn synthetic_v9_asset(
        endian: TestEndian,
        target_platform: i32,
        unity_version: &str,
        object: &[u8],
        class_id: i32,
    ) -> Vec<u8> {
        let mut metadata = Vec::new();
        metadata.extend_from_slice(unity_version.as_bytes());
        metadata.push(0);
        endian.push_i32(&mut metadata, target_platform);
        endian.push_i32(&mut metadata, 1);
        endian.push_i32(&mut metadata, class_id);
        push_c_string(&mut metadata, "Material");
        push_c_string(&mut metadata, "Base");
        for value in [-1, 0, 0, 1, 0, 0] {
            endian.push_i32(&mut metadata, value);
        }
        endian.push_i32(&mut metadata, 0);
        endian.push_i32(&mut metadata, 1);
        endian.push_i32(&mut metadata, 7);
        endian.push_u32(&mut metadata, 0);
        endian.push_u32(&mut metadata, u32::try_from(object.len()).unwrap());
        endian.push_i32(&mut metadata, class_id);
        endian.push_u16(&mut metadata, u16::try_from(class_id).unwrap());
        endian.push_u16(&mut metadata, 0);
        endian.push_i32(&mut metadata, 0);
        metadata.push(0);

        let metadata_size = u32::try_from(metadata.len()).unwrap();
        let data_offset = (20 + metadata.len()).next_multiple_of(16);
        let file_size = data_offset + object.len();
        let mut bytes = vec![0_u8; 20];
        bytes[0..4].copy_from_slice(&metadata_size.to_be_bytes());
        bytes[4..8].copy_from_slice(&u32::try_from(file_size).unwrap().to_be_bytes());
        bytes[8..12].copy_from_slice(&9_u32.to_be_bytes());
        bytes[12..16].copy_from_slice(&u32::try_from(data_offset).unwrap().to_be_bytes());
        bytes[16] = endian.marker();
        bytes.extend_from_slice(&metadata);
        bytes.resize(data_offset, 0);
        bytes.extend_from_slice(object);
        bytes
    }

    fn push_c_string(output: &mut Vec<u8>, value: &str) {
        output.extend_from_slice(value.as_bytes());
        output.push(0);
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

    #[derive(Clone, Copy)]
    enum TestEndian {
        Little,
        Big,
    }

    impl TestEndian {
        const fn marker(self) -> u8 {
            match self {
                Self::Little => 0,
                Self::Big => 1,
            }
        }

        fn push_i16(self, output: &mut Vec<u8>, value: i16) {
            match self {
                Self::Little => output.extend_from_slice(&value.to_le_bytes()),
                Self::Big => output.extend_from_slice(&value.to_be_bytes()),
            }
        }

        fn push_u16(self, output: &mut Vec<u8>, value: u16) {
            match self {
                Self::Little => output.extend_from_slice(&value.to_le_bytes()),
                Self::Big => output.extend_from_slice(&value.to_be_bytes()),
            }
        }

        fn push_i32(self, output: &mut Vec<u8>, value: i32) {
            match self {
                Self::Little => output.extend_from_slice(&value.to_le_bytes()),
                Self::Big => output.extend_from_slice(&value.to_be_bytes()),
            }
        }

        fn push_u32(self, output: &mut Vec<u8>, value: u32) {
            match self {
                Self::Little => output.extend_from_slice(&value.to_le_bytes()),
                Self::Big => output.extend_from_slice(&value.to_be_bytes()),
            }
        }

        fn push_i64(self, output: &mut Vec<u8>, value: i64) {
            match self {
                Self::Little => output.extend_from_slice(&value.to_le_bytes()),
                Self::Big => output.extend_from_slice(&value.to_be_bytes()),
            }
        }

        fn push_f32(self, output: &mut Vec<u8>, value: f32) {
            self.push_u32(output, value.to_bits());
        }
    }
}
