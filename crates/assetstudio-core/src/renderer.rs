//! Bounded readers for the renderer fields used by model conversion.
//!
//! The verified range covers standard Unity 2017.3 through 6000.2 and Tuanjie
//! 2022.3.x. Tuanjie's virtual-geometry, shading-rate, and GRD flags are read
//! from their managed-reader version gates without interpreting mesh clusters.

use crate::endian::{Endian, EndianReader, checked_length};
use crate::scene::{ComponentHeader, EditorExtensionHeader};
use crate::serialized::{ObjectReference, SerializedFile};
use crate::source::{Region, RegionCursor};
use crate::{Error, Result};

pub const MESH_RENDERER_CLASS_ID: i32 = 23;
pub const SKINNED_MESH_RENDERER_CLASS_ID: i32 = 137;

const NO_TARGET_PLATFORM: i32 = -2;
const MINIMUM_RENDERER_VERSION: (u32, u32, u32) = (2017, 3, 0);
const MAXIMUM_RENDERER_VERSION: (u32, u32, u32) = (6000, 2, u32::MAX);

/// Defensive limits for one renderer object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RendererReadLimits {
    pub maximum_object_bytes: u64,
    pub maximum_materials: usize,
    pub maximum_bones: usize,
    pub maximum_blend_shape_weights: usize,
    pub maximum_reference_bytes: u64,
}

impl Default for RendererReadLimits {
    fn default() -> Self {
        Self {
            maximum_object_bytes: 512 * 1024 * 1024,
            maximum_materials: 1_000_000,
            maximum_bones: 1_000_000,
            maximum_blend_shape_weights: 16_000_000,
            maximum_reference_bytes: 256 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StaticBatchInfo {
    pub first_sub_mesh: u16,
    pub sub_mesh_count: u16,
}

/// Common renderer data needed to bind a Mesh to materials and scene nodes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Renderer {
    pub component: ComponentHeader,
    pub enabled: bool,
    pub cast_shadows: u8,
    pub receive_shadows: bool,
    pub virtual_geometry: u8,
    pub virtual_geometry_shadow: u8,
    pub shading_rate: Option<u8>,
    pub force_disable_grd: Option<u8>,
    pub materials: Vec<ObjectReference>,
    pub static_batch_info: StaticBatchInfo,
    pub static_batch_root: ObjectReference,
    pub probe_anchor: ObjectReference,
    pub light_probe_volume_override: ObjectReference,
    pub sorting_layer_id: i32,
    pub sorting_layer: i16,
    pub sorting_order: i16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeshRenderer {
    pub path_id: i64,
    pub renderer: Renderer,
    pub trailing_bytes: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SkinnedMeshRenderer {
    pub path_id: i64,
    pub renderer: Renderer,
    pub quality: i32,
    pub update_when_offscreen: bool,
    pub skin_normals: bool,
    pub mesh: ObjectReference,
    pub bones: Vec<ObjectReference>,
    pub blend_shape_weights: Vec<f32>,
    pub trailing_bytes: u64,
}

pub fn read_mesh_renderer(
    file: &SerializedFile,
    object_index: usize,
    limits: RendererReadLimits,
) -> Result<MeshRenderer> {
    validate_renderer_version(file)?;
    let mut reader = RendererObjectReader::new(
        file,
        object_index,
        MESH_RENDERER_CLASS_ID,
        "MeshRenderer",
        limits,
    )?;
    let renderer = reader.read_renderer()?;
    let trailing_bytes = reader.reader.remaining()?;
    Ok(MeshRenderer {
        path_id: reader.path_id,
        renderer,
        trailing_bytes,
    })
}

pub fn read_skinned_mesh_renderer(
    file: &SerializedFile,
    object_index: usize,
    limits: RendererReadLimits,
) -> Result<SkinnedMeshRenderer> {
    validate_renderer_version(file)?;
    let mut reader = RendererObjectReader::new(
        file,
        object_index,
        SKINNED_MESH_RENDERER_CLASS_ID,
        "SkinnedMeshRenderer",
        limits,
    )?;
    let renderer = reader.read_renderer()?;
    let quality = reader.reader.read_i32()?;
    let update_when_offscreen = reader.reader.read_bool()?;
    let skin_normals = reader.reader.read_bool()?;
    reader.align(4)?;
    let mesh = reader.read_pptr("SkinnedMeshRenderer mesh")?;
    let bone_count = reader.read_count(
        "SkinnedMeshRenderer bone",
        limits.maximum_bones,
        reader.pptr_size(),
    )?;
    reader.check_reference_budget(bone_count, "SkinnedMeshRenderer bone")?;
    let mut bones = reserve_vec(bone_count, "SkinnedMeshRenderer bones")?;
    for _ in 0..bone_count {
        bones.push(reader.read_pptr("SkinnedMeshRenderer bone")?);
    }
    let weight_count = reader.read_count(
        "SkinnedMeshRenderer blend-shape weight",
        limits.maximum_blend_shape_weights,
        4,
    )?;
    let mut blend_shape_weights =
        reserve_vec(weight_count, "SkinnedMeshRenderer blend-shape weights")?;
    for _ in 0..weight_count {
        blend_shape_weights.push(reader.reader.read_f32()?);
    }
    let trailing_bytes = reader.reader.remaining()?;
    Ok(SkinnedMeshRenderer {
        path_id: reader.path_id,
        renderer,
        quality,
        update_when_offscreen,
        skin_normals,
        mesh,
        bones,
        blend_shape_weights,
        trailing_bytes,
    })
}

fn validate_renderer_version(file: &SerializedFile) -> Result<()> {
    if file.unity_version.is_stripped() {
        return Err(Error::unsupported(
            "Renderer requires a Unity version because its layout is version-dependent",
        ));
    }
    let version = file.unity_version.components();
    let is_tuanjie = file.unity_version.build_type.as_deref() == Some("t");
    let supported = if is_tuanjie {
        version.0 == 2022 && version.1 == 3 && version.2 >= 2
    } else {
        (MINIMUM_RENDERER_VERSION..=MAXIMUM_RENDERER_VERSION).contains(&version)
    };
    if !supported {
        return Err(Error::unsupported(format!(
            "Renderer is verified for standard Unity 2017.3 through 6000.2 and Tuanjie 2022.3.x, got {}",
            file.unity_version
        )));
    }
    Ok(())
}

struct RendererObjectReader {
    region: Region,
    reader: EndianReader<RegionCursor>,
    absolute_start: u64,
    path_id: i64,
    target_platform: i32,
    format_version: u32,
    version: (u32, u32, u32),
    version_build: u32,
    is_tuanjie: bool,
    limits: RendererReadLimits,
    reference_bytes: u64,
}

impl RendererObjectReader {
    fn new(
        file: &SerializedFile,
        object_index: usize,
        expected_class_id: i32,
        name: &str,
        limits: RendererReadLimits,
    ) -> Result<Self> {
        let object = file.objects.get(object_index).ok_or_else(|| {
            Error::invalid_data(format!(
                "serialized object index {object_index} is out of range"
            ))
        })?;
        if object.class_id != expected_class_id {
            return Err(Error::unsupported(format!(
                "object {} has class ID {}, expected {expected_class_id} ({name})",
                object.path_id, object.class_id
            )));
        }
        if object.byte_size > limits.maximum_object_bytes {
            return Err(Error::invalid_data(format!(
                "{name} object is {} bytes, exceeding limit {}",
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
            version: file.unity_version.components(),
            version_build: file.unity_version.build,
            is_tuanjie: file.unity_version.build_type.as_deref() == Some("t"),
            limits,
            reference_bytes: 0,
        })
    }

    fn read_renderer(&mut self) -> Result<Renderer> {
        let component = self.read_component()?;
        let enabled = self.reader.read_bool()?;
        let cast_shadows = self.reader.read_u8()?;
        let receive_shadows = self.reader.read_bool()?;
        let _dynamic_occludee = self.reader.read_u8()?;
        if self.version >= (2021, 1, 0) {
            let _static_shadow_caster = self.reader.read_u8()?;
        }
        let _motion_vectors = self.reader.read_u8()?;
        let _light_probe_usage = self.reader.read_u8()?;
        let _reflection_probe_usage = self.reader.read_u8()?;
        if self.version >= (2019, 3, 0) {
            let _ray_tracing_mode = self.reader.read_u8()?;
        }
        if self.version >= (2020, 1, 0) {
            let _ray_trace_procedural = self.reader.read_u8()?;
        }
        let (virtual_geometry, virtual_geometry_shadow, shading_rate, force_disable_grd) =
            self.read_tuanjie_renderer_flags()?;
        if self.version >= (2023, 2, 0) {
            let _ray_tracing_acceleration_structure_build_flags_override = self.reader.read_u8()?;
            let _ray_tracing_acceleration_structure_build_flags = self.reader.read_u8()?;
        }
        if self.version >= (2023, 3, 0) {
            let _small_mesh_culling = self.reader.read_u8()?;
        }
        self.align(4)?;
        if self.version >= (6000, 2, 0) {
            let _force_mesh_lod = self.reader.read_i16()?;
            self.align(4)?;
            let _mesh_lod_selection_bias = self.reader.read_f32()?;
        }
        if self.version >= (2018, 1, 0) {
            let _rendering_layer_mask = self.reader.read_u32()?;
        }
        if self.version >= (2018, 3, 0) {
            let _renderer_priority = self.reader.read_i32()?;
        }
        let _lightmap_index = self.reader.read_u16()?;
        let _lightmap_index_dynamic = self.reader.read_u16()?;
        self.skip(16, "Renderer lightmap tiling offset")?;
        self.skip(16, "Renderer dynamic lightmap tiling offset")?;
        let material_count = self.read_count(
            "Renderer material",
            self.limits.maximum_materials,
            self.pptr_size(),
        )?;
        self.check_reference_budget(material_count, "Renderer material")?;
        let mut materials = reserve_vec(material_count, "Renderer materials")?;
        for _ in 0..material_count {
            materials.push(self.read_pptr("Renderer material")?);
        }
        let static_batch_info = StaticBatchInfo {
            first_sub_mesh: self.reader.read_u16()?,
            sub_mesh_count: self.reader.read_u16()?,
        };
        let static_batch_root = self.read_pptr("Renderer static batch root")?;
        let probe_anchor = self.read_pptr("Renderer probe anchor")?;
        let light_probe_volume_override = self.read_pptr("Renderer light-probe volume override")?;
        let sorting_layer_id = self.reader.read_i32()?;
        let sorting_layer = self.reader.read_i16()?;
        let sorting_order = self.reader.read_i16()?;
        self.align(4)?;
        Ok(Renderer {
            component,
            enabled,
            cast_shadows,
            receive_shadows,
            virtual_geometry,
            virtual_geometry_shadow,
            shading_rate,
            force_disable_grd,
            materials,
            static_batch_info,
            static_batch_root,
            probe_anchor,
            light_probe_volume_override,
            sorting_layer_id,
            sorting_layer,
            sorting_order,
        })
    }

    fn read_tuanjie_renderer_flags(&mut self) -> Result<(u8, u8, Option<u8>, Option<u8>)> {
        if !self.is_tuanjie {
            return Ok((0, 0, None, None));
        }
        let virtual_geometry = self.reader.read_u8()?;
        let virtual_geometry_shadow = self.reader.read_u8()?;
        let has_shading_rate = self.version > (2022, 3, 48)
            || (self.version == (2022, 3, 48) && self.version_build >= 3);
        if !has_shading_rate {
            return Ok((virtual_geometry, virtual_geometry_shadow, None, None));
        }
        self.align(4)?;
        let shading_rate = Some(self.reader.read_u8()?);
        let force_disable_grd = (self.version >= (2022, 3, 61))
            .then(|| self.reader.read_u8())
            .transpose()?;
        Ok((
            virtual_geometry,
            virtual_geometry_shadow,
            shading_rate,
            force_disable_grd,
        ))
    }

    fn read_component(&mut self) -> Result<ComponentHeader> {
        let editor_extension = if self.target_platform == NO_TARGET_PLATFORM {
            let object_hide_flags = self.reader.read_u32()?;
            let prefab_parent_object = self.read_pptr("Renderer prefab parent")?;
            let prefab_internal = self.read_pptr("Renderer prefab internal")?;
            EditorExtensionHeader {
                object_hide_flags: Some(object_hide_flags),
                prefab_parent_object: Some(prefab_parent_object),
                prefab_internal: Some(prefab_internal),
            }
        } else {
            EditorExtensionHeader::default()
        };
        let game_object = self.read_pptr("Renderer game object")?;
        Ok(ComponentHeader {
            editor_extension,
            game_object,
        })
    }

    fn read_pptr(&mut self, field: &str) -> Result<ObjectReference> {
        let pptr_size = u64::try_from(self.pptr_size())
            .map_err(|_| Error::invalid_data("Renderer PPtr size does not fit in u64"))?;
        self.reference_bytes = self
            .reference_bytes
            .checked_add(pptr_size)
            .ok_or_else(|| Error::invalid_data("Renderer reference byte budget overflowed"))?;
        if self.reference_bytes > self.limits.maximum_reference_bytes {
            return Err(Error::invalid_data(format!(
                "Renderer references total {} bytes while reading {field}, exceeding limit {}",
                self.reference_bytes, self.limits.maximum_reference_bytes
            )));
        }
        self.ensure_remaining(self.pptr_size(), field)?;
        let file_id = self.reader.read_i32()?;
        let path_id = if self.format_version < 14 {
            i64::from(self.reader.read_i32()?)
        } else {
            self.reader.read_i64()?
        };
        Ok(ObjectReference { file_id, path_id })
    }

    fn read_count(&mut self, field: &str, maximum: usize, element_size: usize) -> Result<usize> {
        let count = checked_length(self.reader.read_i32()?, field)?;
        if count > maximum {
            return Err(Error::invalid_data(format!(
                "{field} count {count} exceeds limit {maximum}"
            )));
        }
        let minimum_bytes = count
            .checked_mul(element_size)
            .ok_or_else(|| Error::invalid_data(format!("{field} byte size overflowed")))?;
        self.ensure_remaining(minimum_bytes, field)?;
        Ok(count)
    }

    fn check_reference_budget(&self, count: usize, field: &str) -> Result<()> {
        let bytes = count
            .checked_mul(self.pptr_size())
            .and_then(|value| u64::try_from(value).ok())
            .ok_or_else(|| Error::invalid_data(format!("{field} reference bytes overflowed")))?;
        let total = self
            .reference_bytes
            .checked_add(bytes)
            .ok_or_else(|| Error::invalid_data("Renderer reference byte budget overflowed"))?;
        if total > self.limits.maximum_reference_bytes {
            return Err(Error::invalid_data(format!(
                "Renderer references would total {total} bytes while reading {field}, exceeding limit {}",
                self.limits.maximum_reference_bytes
            )));
        }
        Ok(())
    }

    const fn pptr_size(&self) -> usize {
        if self.format_version < 14 { 8 } else { 12 }
    }

    fn ensure_remaining(&mut self, length: usize, field: &str) -> Result<()> {
        if u64::try_from(length).unwrap_or(u64::MAX) > self.reader.remaining()? {
            return Err(Error::invalid_data(format!(
                "{field} needs {length} bytes beyond the bounded object payload"
            )));
        }
        Ok(())
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
            .ok_or_else(|| Error::invalid_data("Renderer alignment position overflowed"))?;
        let remainder = absolute % alignment;
        if remainder == 0 {
            return Ok(());
        }
        self.skip(alignment - remainder, "Renderer alignment")
    }
}

fn reserve_vec<T>(count: usize, field: &str) -> Result<Vec<T>> {
    let mut values = Vec::new();
    values.try_reserve_exact(count).map_err(|error| {
        Error::invalid_data(format!("cannot allocate {field} entries: {error}"))
    })?;
    Ok(values)
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use crate::serialized::SerializedFile;
    use crate::source::Region;
    use crate::unity_version::UnityVersion;

    use super::{
        MESH_RENDERER_CLASS_ID, RendererReadLimits, SKINNED_MESH_RENDERER_CLASS_ID,
        read_mesh_renderer, read_skinned_mesh_renderer,
    };

    #[test]
    fn reads_2017_mesh_renderer_with_material_and_scene_references() {
        let object = renderer_prefix("2017.4.39f1");
        let file = parse_asset(MESH_RENDERER_CLASS_ID, "2017.4.39f1", &object);

        let renderer = read_mesh_renderer(&file, 0, RendererReadLimits::default()).unwrap();

        assert_eq!(renderer.path_id, 7);
        assert!(renderer.renderer.enabled);
        assert_eq!(renderer.renderer.cast_shadows, 2);
        assert!(renderer.renderer.receive_shadows);
        assert_eq!(renderer.renderer.component.game_object.path_id, 11);
        assert_eq!(renderer.renderer.materials[0].path_id, 21);
        assert_eq!(renderer.renderer.static_batch_info.first_sub_mesh, 3);
        assert_eq!(renderer.renderer.static_batch_info.sub_mesh_count, 4);
        assert_eq!(renderer.renderer.static_batch_root.path_id, 31);
        assert_eq!(renderer.renderer.probe_anchor.path_id, 32);
        assert_eq!(renderer.renderer.light_probe_volume_override.path_id, 33);
        assert_eq!(renderer.renderer.sorting_layer_id, 41);
        assert_eq!(renderer.renderer.sorting_layer, 42);
        assert_eq!(renderer.renderer.sorting_order, 43);
        assert_eq!(renderer.trailing_bytes, 0);
    }

    #[test]
    fn reads_2022_skinned_renderer_bones_and_weights() {
        let mut object = renderer_prefix("2022.3.62f1");
        object.extend_from_slice(&5_i32.to_le_bytes());
        object.push(1);
        object.push(0);
        align(&mut object, 4);
        push_pptr(&mut object, 0, 50);
        object.extend_from_slice(&2_i32.to_le_bytes());
        push_pptr(&mut object, 0, 61);
        push_pptr(&mut object, 1, 62);
        object.extend_from_slice(&2_i32.to_le_bytes());
        object.extend_from_slice(&0.25_f32.to_le_bytes());
        object.extend_from_slice(&0.75_f32.to_le_bytes());
        let file = parse_asset(SKINNED_MESH_RENDERER_CLASS_ID, "2022.3.62f1", &object);

        let renderer = read_skinned_mesh_renderer(&file, 0, RendererReadLimits::default()).unwrap();

        assert_eq!(renderer.quality, 5);
        assert!(renderer.update_when_offscreen);
        assert!(!renderer.skin_normals);
        assert_eq!(renderer.mesh.path_id, 50);
        assert_eq!(renderer.bones[0].path_id, 61);
        assert_eq!(renderer.bones[1].file_id, 1);
        assert_eq!(
            renderer
                .blend_shape_weights
                .iter()
                .map(|value| value.to_bits())
                .collect::<Vec<_>>(),
            [0.25_f32.to_bits(), 0.75_f32.to_bits()]
        );
        assert_eq!(renderer.trailing_bytes, 0);
    }

    #[test]
    fn enforces_version_class_object_count_and_cumulative_reference_limits() {
        let object = renderer_prefix("2022.3.62f1");
        let file = parse_asset(MESH_RENDERER_CLASS_ID, "2022.3.62f1", &object);
        let object_limit = RendererReadLimits {
            maximum_object_bytes: u64::try_from(object.len() - 1).unwrap(),
            ..RendererReadLimits::default()
        };
        assert!(read_mesh_renderer(&file, 0, object_limit).is_err());
        let material_limit = RendererReadLimits {
            maximum_materials: 0,
            ..RendererReadLimits::default()
        };
        assert!(read_mesh_renderer(&file, 0, material_limit).is_err());
        let reference_limit = RendererReadLimits {
            maximum_reference_bytes: 12,
            ..RendererReadLimits::default()
        };
        assert!(read_mesh_renderer(&file, 0, reference_limit).is_err());

        let wrong_class = parse_asset(49, "2022.3.62f1", &object);
        assert!(read_mesh_renderer(&wrong_class, 0, RendererReadLimits::default()).is_err());
        let stripped = parse_asset(MESH_RENDERER_CLASS_ID, "0.0.0", &object);
        assert!(read_mesh_renderer(&stripped, 0, RendererReadLimits::default()).is_err());
        let unsupported_tuanjie = parse_asset(MESH_RENDERER_CLASS_ID, "2023.1.0t1", &object);
        assert!(
            read_mesh_renderer(&unsupported_tuanjie, 0, RendererReadLimits::default()).is_err()
        );
        let early_tuanjie = parse_asset(MESH_RENDERER_CLASS_ID, "2022.3.1t1", &object);
        assert!(read_mesh_renderer(&early_tuanjie, 0, RendererReadLimits::default()).is_err());
        let future = parse_asset(MESH_RENDERER_CLASS_ID, "6000.3.0f1", &object);
        assert!(read_mesh_renderer(&future, 0, RendererReadLimits::default()).is_err());
    }

    #[test]
    fn locks_tuanjie_virtual_geometry_shading_and_grd_gates() {
        for (version, shading_rate, force_disable_grd) in [
            ("2022.3.2t1", None, None),
            ("2022.3.48t2", None, None),
            ("2022.3.48t3", Some(9), None),
            ("2022.3.61t1", Some(9), Some(10)),
            ("2022.3.61t2", Some(9), Some(10)),
        ] {
            let object = renderer_prefix(version);
            let file = parse_asset(MESH_RENDERER_CLASS_ID, version, &object);
            let renderer = read_mesh_renderer(&file, 0, RendererReadLimits::default()).unwrap();

            assert_eq!(renderer.renderer.virtual_geometry, 7, "{version}");
            assert_eq!(renderer.renderer.virtual_geometry_shadow, 8, "{version}");
            assert_eq!(renderer.renderer.shading_rate, shading_rate, "{version}");
            assert_eq!(
                renderer.renderer.force_disable_grd, force_disable_grd,
                "{version}"
            );
            assert_eq!(renderer.renderer.materials[0].path_id, 21, "{version}");
            assert_eq!(renderer.trailing_bytes, 0, "{version}");
        }
    }

    #[test]
    fn locks_renderer_flag_gates_from_2018_through_unity_6000_2() {
        for version in [
            "2018.1.0f1",
            "2018.3.0f1",
            "2019.3.0f1",
            "2020.1.0f1",
            "2021.1.0f1",
            "2023.1.0f1",
            "2023.2.0f1",
            "2023.3.0f1",
            "6000.0.0f1",
            "6000.2.0f1",
        ] {
            let object = renderer_prefix(version);
            let file = parse_asset(MESH_RENDERER_CLASS_ID, version, &object);
            let renderer = read_mesh_renderer(&file, 0, RendererReadLimits::default()).unwrap();
            assert_eq!(renderer.renderer.materials[0].path_id, 21, "{version}");
            assert_eq!(renderer.trailing_bytes, 0, "{version}");
        }
    }

    fn renderer_prefix(version: &str) -> Vec<u8> {
        let parsed = UnityVersion::from_str(version).unwrap();
        let (major, minor, patch) = parsed.components();
        let has_rendering_layer = major >= 2018;
        let has_renderer_priority = major > 2018 || (major == 2018 && minor >= 3);
        let has_ray_tracing_mode = major > 2019 || (major == 2019 && minor >= 3);
        let has_ray_trace_procedural = major >= 2020;
        let has_static_shadow_caster = major >= 2021;
        let has_acceleration_structure_flags = major > 2023 || (major == 2023 && minor >= 2);
        let has_small_mesh_culling = major > 2023 || (major == 2023 && minor >= 3);
        let has_mesh_lod_selection = major > 6000 || (major == 6000 && minor >= 2);
        let mut object = Vec::new();
        push_pptr(&mut object, 0, 11);
        object.push(1);
        object.push(2);
        object.push(1);
        object.push(0);
        if has_static_shadow_caster {
            object.push(0);
        }
        object.extend_from_slice(&[0, 0, 0]);
        if has_ray_tracing_mode {
            object.push(0);
        }
        if has_ray_trace_procedural {
            object.push(0);
        }
        if parsed.is_tuanjie() {
            object.extend_from_slice(&[7, 8]);
            let has_shading_rate = (major, minor, patch) > (2022, 3, 48)
                || ((major, minor, patch) == (2022, 3, 48) && parsed.build >= 3);
            if has_shading_rate {
                align(&mut object, 4);
                object.push(9);
                if (major, minor, patch) >= (2022, 3, 61) {
                    object.push(10);
                }
            }
        }
        if has_acceleration_structure_flags {
            object.extend_from_slice(&[0, 0]);
        }
        if has_small_mesh_culling {
            object.push(0);
        }
        align(&mut object, 4);
        if has_mesh_lod_selection {
            object.extend_from_slice(&(-1_i16).to_le_bytes());
            align(&mut object, 4);
            object.extend_from_slice(&1.0_f32.to_le_bytes());
        }
        if has_rendering_layer {
            object.extend_from_slice(&u32::MAX.to_le_bytes());
        }
        if has_renderer_priority {
            object.extend_from_slice(&0_i32.to_le_bytes());
        }
        object.extend_from_slice(&0_u16.to_le_bytes());
        object.extend_from_slice(&0_u16.to_le_bytes());
        object.extend_from_slice(&[0_u8; 32]);
        object.extend_from_slice(&1_i32.to_le_bytes());
        push_pptr(&mut object, 0, 21);
        object.extend_from_slice(&3_u16.to_le_bytes());
        object.extend_from_slice(&4_u16.to_le_bytes());
        push_pptr(&mut object, 0, 31);
        push_pptr(&mut object, 0, 32);
        push_pptr(&mut object, 0, 33);
        object.extend_from_slice(&41_i32.to_le_bytes());
        object.extend_from_slice(&42_i16.to_le_bytes());
        object.extend_from_slice(&43_i16.to_le_bytes());
        align(&mut object, 4);
        object
    }

    fn push_pptr(output: &mut Vec<u8>, file_id: i32, path_id: i64) {
        output.extend_from_slice(&file_id.to_le_bytes());
        output.extend_from_slice(&path_id.to_le_bytes());
    }

    fn parse_asset(class_id: i32, version: &str, object: &[u8]) -> SerializedFile {
        let major = version
            .split('.')
            .next()
            .unwrap()
            .parse::<u32>()
            .unwrap_or(2022);
        let bytes = if (2017..=2018).contains(&major) {
            synthetic_pre_v22_asset(17, class_id, version, object)
        } else if major == 2019 {
            synthetic_pre_v22_asset(21, class_id, version, object)
        } else {
            synthetic_v22_asset(class_id, version, object)
        };
        SerializedFile::open(Region::from_bytes(bytes)).unwrap()
    }

    fn synthetic_pre_v22_asset(
        format_version: u32,
        class_id: i32,
        version: &str,
        object: &[u8],
    ) -> Vec<u8> {
        assert!(matches!(format_version, 17 | 21));
        let mut metadata = Vec::new();
        metadata.extend_from_slice(version.as_bytes());
        metadata.push(0);
        metadata.extend_from_slice(&13_i32.to_le_bytes());
        metadata.push(0);
        metadata.extend_from_slice(&1_i32.to_le_bytes());
        metadata.extend_from_slice(&class_id.to_le_bytes());
        metadata.push(0);
        metadata.extend_from_slice(&(-1_i16).to_le_bytes());
        metadata.extend_from_slice(&[0_u8; 16]);
        metadata.extend_from_slice(&1_i32.to_le_bytes());
        align_with_base(&mut metadata, 20, 4);
        metadata.extend_from_slice(&7_i64.to_le_bytes());
        metadata.extend_from_slice(&0_u32.to_le_bytes());
        metadata.extend_from_slice(&u32::try_from(object.len()).unwrap().to_le_bytes());
        metadata.extend_from_slice(&0_i32.to_le_bytes());
        metadata.extend_from_slice(&0_i32.to_le_bytes());
        metadata.extend_from_slice(&0_i32.to_le_bytes());
        if format_version >= 20 {
            metadata.extend_from_slice(&0_i32.to_le_bytes());
        }
        metadata.push(0);

        let metadata_size = u32::try_from(metadata.len()).unwrap();
        let data_offset = (20 + metadata.len()).next_multiple_of(16);
        let file_size = data_offset + object.len();
        let mut bytes = vec![0_u8; 20];
        bytes[0..4].copy_from_slice(&metadata_size.to_be_bytes());
        bytes[4..8].copy_from_slice(&u32::try_from(file_size).unwrap().to_be_bytes());
        bytes[8..12].copy_from_slice(&format_version.to_be_bytes());
        bytes[12..16].copy_from_slice(&u32::try_from(data_offset).unwrap().to_be_bytes());
        bytes.extend_from_slice(&metadata);
        bytes.resize(data_offset, 0);
        bytes.extend_from_slice(object);
        bytes
    }

    fn synthetic_v22_asset(class_id: i32, version: &str, object: &[u8]) -> Vec<u8> {
        let mut metadata = Vec::new();
        metadata.extend_from_slice(version.as_bytes());
        metadata.push(0);
        metadata.extend_from_slice(&13_i32.to_le_bytes());
        metadata.push(0);
        metadata.extend_from_slice(&1_i32.to_le_bytes());
        metadata.extend_from_slice(&class_id.to_le_bytes());
        metadata.push(0);
        metadata.extend_from_slice(&(-1_i16).to_le_bytes());
        metadata.extend_from_slice(&[0_u8; 16]);
        metadata.extend_from_slice(&1_i32.to_le_bytes());
        align_with_base(&mut metadata, 48, 4);
        metadata.extend_from_slice(&7_i64.to_le_bytes());
        metadata.extend_from_slice(&0_i64.to_le_bytes());
        metadata.extend_from_slice(&u32::try_from(object.len()).unwrap().to_le_bytes());
        metadata.extend_from_slice(&0_i32.to_le_bytes());
        metadata.extend_from_slice(&0_i32.to_le_bytes());
        metadata.extend_from_slice(&0_i32.to_le_bytes());
        metadata.extend_from_slice(&0_i32.to_le_bytes());
        metadata.push(0);
        let metadata_size = u32::try_from(metadata.len()).unwrap();
        let data_offset = (48 + metadata.len()).next_multiple_of(16);
        let file_size = data_offset + object.len();
        let mut bytes = vec![0_u8; 48];
        bytes[8..12].copy_from_slice(&22_u32.to_be_bytes());
        bytes[20..24].copy_from_slice(&metadata_size.to_be_bytes());
        bytes[24..32].copy_from_slice(&i64::try_from(file_size).unwrap().to_be_bytes());
        bytes[32..40].copy_from_slice(&i64::try_from(data_offset).unwrap().to_be_bytes());
        bytes.extend_from_slice(&metadata);
        bytes.resize(data_offset, 0);
        bytes.extend_from_slice(object);
        bytes
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
