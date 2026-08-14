//! Source-bound readers for Unity's basic GameObject/component graph.
//!
//! This module intentionally stops at the stable engine-owned fields needed to
//! connect `GameObject`, `Transform`, `MeshFilter`, and `Animator` objects. It
//! does not infer animation, renderer, or FBX state from trailing payload bytes.

use crate::endian::{Endian, EndianReader, checked_length};
use crate::loader::AssetCollection;
pub use crate::object_name::{ANIMATOR_CLASS_ID, GAME_OBJECT_CLASS_ID};
pub use crate::serialized::ObjectReference;
use crate::serialized::{ObjectInfo, SerializedFile};
use crate::source::{Region, RegionCursor};
use crate::{Error, Result};

pub const TRANSFORM_CLASS_ID: i32 = 4;
pub const MESH_FILTER_CLASS_ID: i32 = 33;
pub const RECT_TRANSFORM_CLASS_ID: i32 = 224;

const NO_TARGET_PLATFORM: i32 = -2;

/// Defensive limits for source-bound scene-graph prefix readers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SceneReadLimits {
    pub maximum_string_bytes: usize,
    pub maximum_game_object_components: usize,
    pub maximum_transform_children: usize,
    pub maximum_reference_bytes: u64,
}

impl Default for SceneReadLimits {
    fn default() -> Self {
        Self {
            maximum_string_bytes: 16 * 1024 * 1024,
            maximum_game_object_components: 1_000_000,
            maximum_transform_children: 1_000_000,
            maximum_reference_bytes: 256 * 1024 * 1024,
        }
    }
}

/// The editor-only prefix serialized by `Object` and `EditorExtension` when
/// Unity's target platform is `NoTarget`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EditorExtensionHeader {
    pub object_hide_flags: Option<u32>,
    pub prefab_parent_object: Option<ObjectReference>,
    pub prefab_internal: Option<ObjectReference>,
}

/// Fields common to every parsed `Component`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComponentHeader {
    pub editor_extension: EditorExtensionHeader,
    pub game_object: ObjectReference,
}

/// Fields common to every parsed `Behaviour`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BehaviourHeader {
    pub component: ComponentHeader,
    pub enabled: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GameObjectComponent {
    /// Unity versions before 5.5 stored an unused integer before each `PPtr`.
    pub legacy_index: Option<i32>,
    pub component: ObjectReference,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameObject {
    pub path_id: i64,
    pub editor_extension: EditorExtensionHeader,
    pub components: Vec<GameObjectComponent>,
    pub layer: i32,
    /// Present only in Tuanjie 2022.3.2t11 and newer builds.
    pub has_editor_info: Option<bool>,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vector3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quaternion {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub w: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Transform {
    pub path_id: i64,
    pub component: ComponentHeader,
    pub local_rotation: Quaternion,
    pub local_position: Vector3,
    pub local_scale: Vector3,
    pub children: Vec<ObjectReference>,
    pub father: ObjectReference,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeshFilter {
    pub path_id: i64,
    pub component: ComponentHeader,
    pub mesh: ObjectReference,
}

/// The stable prefix of an `Animator` through `m_Controller`.
///
/// Version-dependent culling/update flags follow this prefix and are
/// deliberately not consumed here. Animation-state migration can extend the
/// reader once those fields are needed without guessing from trailing bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Animator {
    pub path_id: i64,
    pub behaviour: BehaviourHeader,
    pub avatar: ObjectReference,
    pub controller: ObjectReference,
}

/// A `PPtr` target resolved against an already-loaded asset collection.
#[derive(Debug)]
pub struct ResolvedObject<'a> {
    pub file_index: usize,
    pub file_path: &'a str,
    pub file: &'a SerializedFile,
    pub object_index: usize,
    pub object: &'a ObjectInfo,
}

/// Resolves a local or external `PPtr` using one-based external-file indexing and
/// the portable, ASCII-case-insensitive file-name lookup used by the native
/// asset loader.
/// Null references return `Ok(None)`; malformed or missing non-null references
/// are errors rather than being silently converted to null.
pub fn resolve_object_reference(
    collection: &AssetCollection,
    source_file_index: usize,
    reference: ObjectReference,
) -> Result<Option<ResolvedObject<'_>>> {
    let source = collection
        .serialized_files
        .get(source_file_index)
        .ok_or_else(|| {
            Error::invalid_data(format!(
                "source serialized-file index {source_file_index} is out of range"
            ))
        })?;
    if reference.is_null() {
        return Ok(None);
    }

    let target_file_index = if reference.file_id == 0 {
        source_file_index
    } else {
        let external_index = reference
            .file_id
            .checked_sub(1)
            .and_then(|index| usize::try_from(index).ok())
            .ok_or_else(|| {
                Error::invalid_data(format!(
                    "PPtr file ID {} cannot be converted to an external index",
                    reference.file_id
                ))
            })?;
        let external = source.file.externals.get(external_index).ok_or_else(|| {
            Error::invalid_data(format!(
                "PPtr file ID {} refers to external index {external_index}, but source file {:?} has {} externals",
                reference.file_id,
                source.path,
                source.file.externals.len()
            ))
        })?;
        let target_name = portable_file_name(&external.path);
        collection
            .serialized_file_index_by_portable_name(target_name)
            .ok_or_else(|| {
                Error::invalid_data(format!(
                    "PPtr external file {:?} (portable name {:?}) was not found in the asset collection",
                    external.path, target_name
                ))
            })?
    };

    let target = collection
        .serialized_files
        .get(target_file_index)
        .ok_or_else(|| {
            Error::invalid_data(format!(
                "resolved serialized-file index {target_file_index} is out of range"
            ))
        })?;
    let object_index = collection
        .object_index_by_path_id(target_file_index, reference.path_id)
        .ok_or_else(|| {
            Error::invalid_data(format!(
                "PPtr path ID {} was not found in resolved file {:?}",
                reference.path_id, target.path
            ))
        })?;
    let object = target
        .file
        .objects
        .get(object_index)
        .filter(|object| object.path_id == reference.path_id)
        .ok_or_else(|| {
            Error::invalid_data(format!(
                "PPtr path ID {} was not found in resolved file {:?}",
                reference.path_id, target.path
            ))
        })?;
    Ok(Some(ResolvedObject {
        file_index: target_file_index,
        file_path: &target.path,
        file: &target.file,
        object_index,
        object,
    }))
}

/// Resolves a `PPtr` and verifies the target class before a typed reader is used.
pub fn resolve_typed_object_reference(
    collection: &AssetCollection,
    source_file_index: usize,
    reference: ObjectReference,
    expected_class_id: i32,
) -> Result<Option<ResolvedObject<'_>>> {
    let resolved = resolve_object_reference(collection, source_file_index, reference)?;
    if let Some(target) = &resolved
        && target.object.class_id != expected_class_id
    {
        return Err(Error::invalid_data(format!(
            "PPtr path ID {} has class ID {}, expected {expected_class_id}",
            reference.path_id, target.object.class_id
        )));
    }
    Ok(resolved)
}

pub fn read_game_object(
    file: &SerializedFile,
    object_index: usize,
    limits: SceneReadLimits,
) -> Result<GameObject> {
    if file.unity_version.is_stripped() {
        return Err(Error::unsupported(
            "GameObject requires a Unity version because component entries changed in Unity 5.5",
        ));
    }
    let mut reader = SceneObjectReader::new(
        file,
        object_index,
        GAME_OBJECT_CLASS_ID,
        "GameObject",
        limits,
    )?;
    let editor_extension = reader.read_editor_extension()?;
    let legacy_components = reader.version < (5, 5, 0);
    let element_size = reader
        .pptr_size()
        .checked_add(usize::from(legacy_components) * 4)
        .ok_or_else(|| Error::invalid_data("GameObject component size overflowed"))?;
    let component_count = reader.read_count(
        "GameObject component",
        limits.maximum_game_object_components,
        element_size,
    )?;
    reader.check_reference_array_budget(component_count, "GameObject component")?;
    let mut components = reserve_vec(component_count, "GameObject components")?;
    for _ in 0..component_count {
        let legacy_index = if legacy_components {
            Some(reader.reader.read_i32()?)
        } else {
            None
        };
        components.push(GameObjectComponent {
            legacy_index,
            component: reader.read_pptr("GameObject component")?,
        });
    }
    let layer = reader.reader.read_i32()?;
    let has_editor_info = if has_tuanjie_game_object_editor_info(file) {
        let value = reader.reader.read_bool()?;
        reader.align(4)?;
        Some(value)
    } else {
        None
    };
    let name = reader.read_aligned_string("GameObject name")?;

    Ok(GameObject {
        path_id: reader.path_id,
        editor_extension,
        components,
        layer,
        has_editor_info,
        name,
    })
}

pub fn read_transform(
    file: &SerializedFile,
    object_index: usize,
    limits: SceneReadLimits,
) -> Result<Transform> {
    let object = file.objects.get(object_index).ok_or_else(|| {
        Error::invalid_data(format!(
            "serialized object index {object_index} is out of range"
        ))
    })?;
    if !matches!(
        object.class_id,
        TRANSFORM_CLASS_ID | RECT_TRANSFORM_CLASS_ID
    ) {
        return Err(Error::unsupported(format!(
            "object {} has class ID {}, not Transform ({TRANSFORM_CLASS_ID}) or RectTransform ({RECT_TRANSFORM_CLASS_ID})",
            object.path_id, object.class_id
        )));
    }
    let mut reader = SceneObjectReader::new(
        file,
        object_index,
        object.class_id,
        "Transform/RectTransform",
        limits,
    )?;
    let component = reader.read_component()?;
    let local_rotation = reader.read_quaternion()?;
    let local_position = reader.read_vector3()?;
    let local_scale = reader.read_vector3()?;
    let child_count = reader.read_count(
        "Transform child",
        limits.maximum_transform_children,
        reader.pptr_size(),
    )?;
    reader.check_reference_array_budget(child_count, "Transform child")?;
    let mut children = reserve_vec(child_count, "Transform children")?;
    for _ in 0..child_count {
        children.push(reader.read_pptr("Transform child")?);
    }
    let father = reader.read_pptr("Transform father")?;
    Ok(Transform {
        path_id: reader.path_id,
        component,
        local_rotation,
        local_position,
        local_scale,
        children,
        father,
    })
}

pub fn read_mesh_filter(
    file: &SerializedFile,
    object_index: usize,
    limits: SceneReadLimits,
) -> Result<MeshFilter> {
    let mut reader = SceneObjectReader::new(
        file,
        object_index,
        MESH_FILTER_CLASS_ID,
        "MeshFilter",
        limits,
    )?;
    let component = reader.read_component()?;
    let mesh = reader.read_pptr("MeshFilter mesh")?;
    Ok(MeshFilter {
        path_id: reader.path_id,
        component,
        mesh,
    })
}

pub fn read_animator(
    file: &SerializedFile,
    object_index: usize,
    limits: SceneReadLimits,
) -> Result<Animator> {
    let mut reader =
        SceneObjectReader::new(file, object_index, ANIMATOR_CLASS_ID, "Animator", limits)?;
    let behaviour = reader.read_behaviour()?;
    let avatar = reader.read_pptr("Animator avatar")?;
    let controller = reader.read_pptr("Animator controller")?;
    Ok(Animator {
        path_id: reader.path_id,
        behaviour,
        avatar,
        controller,
    })
}

fn has_tuanjie_game_object_editor_info(file: &SerializedFile) -> bool {
    let version = &file.unity_version;
    version.is_tuanjie()
        && (version.components() > (2022, 3, 2)
            || (version.components() == (2022, 3, 2) && version.build >= 11))
}

fn portable_file_name(path: &str) -> &str {
    let component = path.rsplit_once("::").map_or(path, |(_, name)| name);
    component.rsplit(['/', '\\']).next().unwrap_or(component)
}

fn reserve_vec<T>(length: usize, field: &str) -> Result<Vec<T>> {
    let mut values = Vec::new();
    values.try_reserve_exact(length).map_err(|error| {
        Error::invalid_data(format!("cannot allocate {length} {field}: {error}"))
    })?;
    Ok(values)
}

struct SceneObjectReader {
    reader: EndianReader<RegionCursor>,
    region: Region,
    absolute_start: u64,
    path_id: i64,
    target_platform: i32,
    format_version: u32,
    version: (u32, u32, u32),
    limits: SceneReadLimits,
    reference_bytes_read: u64,
}

impl SceneObjectReader {
    fn new(
        file: &SerializedFile,
        object_index: usize,
        expected_class_id: i32,
        class_name: &str,
        limits: SceneReadLimits,
    ) -> Result<Self> {
        let object = file.objects.get(object_index).ok_or_else(|| {
            Error::invalid_data(format!(
                "serialized object index {object_index} is out of range"
            ))
        })?;
        if object.class_id != expected_class_id {
            return Err(Error::unsupported(format!(
                "object {} has class ID {}, not {class_name} ({expected_class_id})",
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
            version: file.unity_version.components(),
            limits,
            reference_bytes_read: 0,
        })
    }

    fn pptr_size(&self) -> usize {
        if self.format_version < 14 { 8 } else { 12 }
    }

    fn read_editor_extension(&mut self) -> Result<EditorExtensionHeader> {
        if self.target_platform != NO_TARGET_PLATFORM {
            return Ok(EditorExtensionHeader::default());
        }
        Ok(EditorExtensionHeader {
            object_hide_flags: Some(self.reader.read_u32()?),
            prefab_parent_object: Some(self.read_pptr("prefab parent object")?),
            prefab_internal: Some(self.read_pptr("prefab internal")?),
        })
    }

    fn read_component(&mut self) -> Result<ComponentHeader> {
        Ok(ComponentHeader {
            editor_extension: self.read_editor_extension()?,
            game_object: self.read_pptr("Component game object")?,
        })
    }

    fn read_behaviour(&mut self) -> Result<BehaviourHeader> {
        let component = self.read_component()?;
        let enabled = self.reader.read_u8()?;
        self.align(4)?;
        Ok(BehaviourHeader { component, enabled })
    }

    fn read_pptr(&mut self, field: &str) -> Result<ObjectReference> {
        let pptr_size = u64::try_from(self.pptr_size())
            .map_err(|_| Error::invalid_data("PPtr size does not fit in u64"))?;
        self.reference_bytes_read = self
            .reference_bytes_read
            .checked_add(pptr_size)
            .ok_or_else(|| Error::invalid_data("scene reference byte budget overflowed"))?;
        if self.reference_bytes_read > self.limits.maximum_reference_bytes {
            return Err(Error::invalid_data(format!(
                "scene references total {} bytes while reading {field}, exceeding limit {}",
                self.reference_bytes_read, self.limits.maximum_reference_bytes
            )));
        }
        let file_id = self.reader.read_i32()?;
        let path_id = if self.format_version < 14 {
            i64::from(self.reader.read_i32()?)
        } else {
            self.reader.read_i64()?
        };
        Ok(ObjectReference { file_id, path_id })
    }

    fn check_reference_array_budget(&self, count: usize, field: &str) -> Result<()> {
        let count = u64::try_from(count)
            .map_err(|_| Error::invalid_data(format!("{field} count does not fit in u64")))?;
        let pptr_size = u64::try_from(self.pptr_size())
            .map_err(|_| Error::invalid_data("PPtr size does not fit in u64"))?;
        let array_bytes = count
            .checked_mul(pptr_size)
            .ok_or_else(|| Error::invalid_data(format!("{field} reference bytes overflowed")))?;
        let total = self
            .reference_bytes_read
            .checked_add(array_bytes)
            .ok_or_else(|| Error::invalid_data("scene reference byte budget overflowed"))?;
        if total > self.limits.maximum_reference_bytes {
            return Err(Error::invalid_data(format!(
                "scene references would total {total} bytes while reading {field}, exceeding limit {}",
                self.limits.maximum_reference_bytes
            )));
        }
        Ok(())
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
            .ok_or_else(|| Error::invalid_data(format!("{field} byte length overflowed")))?;
        if u64::try_from(minimum_bytes).unwrap_or(u64::MAX) > self.reader.remaining()? {
            return Err(Error::invalid_data(format!(
                "{field} needs at least {minimum_bytes} bytes beyond the bounded object payload"
            )));
        }
        Ok(count)
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

    fn read_vector3(&mut self) -> Result<Vector3> {
        Ok(Vector3 {
            x: self.reader.read_f32()?,
            y: self.reader.read_f32()?,
            z: self.reader.read_f32()?,
        })
    }

    fn read_quaternion(&mut self) -> Result<Quaternion> {
        Ok(Quaternion {
            x: self.reader.read_f32()?,
            y: self.reader.read_f32()?,
            z: self.reader.read_f32()?,
            w: self.reader.read_f32()?,
        })
    }

    fn skip(&mut self, length: u64, field: &str) -> Result<()> {
        let target = self
            .reader
            .position()?
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
}

#[cfg(test)]
mod tests {
    use crate::endian::Endian;
    use crate::loader::{AssetCollection, LoadedSerializedFile};
    use crate::serialized::SerializedFile;
    use crate::source::Region;

    use super::{
        ANIMATOR_CLASS_ID, GAME_OBJECT_CLASS_ID, MESH_FILTER_CLASS_ID, ObjectReference,
        RECT_TRANSFORM_CLASS_ID, SceneReadLimits, TRANSFORM_CLASS_ID, read_animator,
        read_game_object, read_mesh_filter, read_transform, resolve_object_reference,
        resolve_typed_object_reference,
    };

    #[test]
    fn reads_no_target_game_object_and_tuanjie_editor_info() {
        let mut object = Vec::new();
        push_u32(&mut object, 0x1234_5678, Endian::Little);
        push_pptr(&mut object, 1, 11, 22, Endian::Little);
        push_pptr(&mut object, 2, 12, 22, Endian::Little);
        push_i32(&mut object, 2, Endian::Little);
        push_pptr(&mut object, 0, 21, 22, Endian::Little);
        push_pptr(&mut object, 1, 22, 22, Endian::Little);
        push_i32(&mut object, 7, Endian::Little);
        push_aligned_string(&mut object, "Editor Player", Endian::Little);
        let file = parse_v22(
            "2022.3.62f1",
            -2,
            Endian::Little,
            &[(GAME_OBJECT_CLASS_ID, 7, object)],
            &[],
        );

        let game_object = read_game_object(&file, 0, SceneReadLimits::default()).unwrap();
        assert_eq!(game_object.path_id, 7);
        assert_eq!(game_object.name, "Editor Player");
        assert_eq!(game_object.layer, 7);
        assert_eq!(game_object.has_editor_info, None);
        assert_eq!(game_object.components.len(), 2);
        assert_eq!(game_object.components[1].component.file_id, 1);
        assert_eq!(game_object.components[1].component.path_id, 22);
        assert_eq!(
            game_object.editor_extension.object_hide_flags,
            Some(0x1234_5678)
        );
        assert_eq!(
            game_object
                .editor_extension
                .prefab_parent_object
                .unwrap()
                .path_id,
            11
        );

        let mut tuanjie = Vec::new();
        push_i32(&mut tuanjie, 0, Endian::Little);
        push_i32(&mut tuanjie, 3, Endian::Little);
        tuanjie.push(1);
        align(&mut tuanjie, 4);
        push_aligned_string(&mut tuanjie, "Tuanjie Player", Endian::Little);
        let file = parse_v22(
            "2022.3.2t11",
            13,
            Endian::Little,
            &[(GAME_OBJECT_CLASS_ID, 8, tuanjie)],
            &[],
        );
        let game_object = read_game_object(&file, 0, SceneReadLimits::default()).unwrap();
        assert_eq!(game_object.has_editor_info, Some(true));
        assert_eq!(game_object.name, "Tuanjie Player");

        let mut before_gate = Vec::new();
        push_i32(&mut before_gate, 0, Endian::Little);
        push_i32(&mut before_gate, 3, Endian::Little);
        push_aligned_string(&mut before_gate, "Before Gate", Endian::Little);
        let file = parse_v22(
            "2022.3.2t10",
            13,
            Endian::Little,
            &[(GAME_OBJECT_CLASS_ID, 9, before_gate)],
            &[],
        );
        let game_object = read_game_object(&file, 0, SceneReadLimits::default()).unwrap();
        assert_eq!(game_object.has_editor_info, None);
        assert_eq!(game_object.name, "Before Gate");

        let mut mesh_filter = Vec::new();
        push_u32(&mut mesh_filter, 9, Endian::Little);
        push_pptr(&mut mesh_filter, 0, 51, 22, Endian::Little);
        push_pptr(&mut mesh_filter, 0, 52, 22, Endian::Little);
        push_pptr(&mut mesh_filter, 0, 53, 22, Endian::Little);
        push_pptr(&mut mesh_filter, 0, 54, 22, Endian::Little);
        let file = parse_v22(
            "2022.3.62f1",
            -2,
            Endian::Little,
            &[(MESH_FILTER_CLASS_ID, 50, mesh_filter)],
            &[],
        );
        let mesh_filter = read_mesh_filter(&file, 0, SceneReadLimits::default()).unwrap();
        assert_eq!(
            mesh_filter
                .component
                .editor_extension
                .prefab_internal
                .unwrap()
                .path_id,
            52
        );
        assert_eq!(mesh_filter.component.game_object.path_id, 53);
        assert_eq!(mesh_filter.mesh.path_id, 54);
    }

    #[test]
    fn reads_legacy_component_entries_and_32_bit_pptrs() {
        let mut object = Vec::new();
        push_i32(&mut object, 1, Endian::Little);
        push_i32(&mut object, 99, Endian::Little);
        push_pptr(&mut object, 3, 0x1234_5678, 13, Endian::Little);
        push_i32(&mut object, 5, Endian::Little);
        push_aligned_string(&mut object, "Legacy", Endian::Little);
        let file = parse_v13(GAME_OBJECT_CLASS_ID, &object);

        let game_object = read_game_object(&file, 0, SceneReadLimits::default()).unwrap();
        assert_eq!(game_object.components[0].legacy_index, Some(99));
        assert_eq!(game_object.components[0].component.file_id, 3);
        assert_eq!(game_object.components[0].component.path_id, 0x1234_5678);
        assert_eq!(game_object.name, "Legacy");
    }

    #[test]
    fn reads_big_endian_transform_trs_and_hierarchy() {
        let mut object = Vec::new();
        push_pptr(&mut object, 0, 10, 22, Endian::Big);
        push_f32s(&mut object, &[1.0, 2.0, 3.0, 4.0], Endian::Big);
        push_f32s(&mut object, &[5.0, 6.0, 7.0], Endian::Big);
        push_f32s(&mut object, &[8.0, 9.0, 10.0], Endian::Big);
        push_i32(&mut object, 2, Endian::Big);
        push_pptr(&mut object, 0, 31, 22, Endian::Big);
        push_pptr(&mut object, 1, 32, 22, Endian::Big);
        push_pptr(&mut object, 0, 40, 22, Endian::Big);
        let file = parse_v22(
            "2022.3.62f1",
            13,
            Endian::Big,
            &[(TRANSFORM_CLASS_ID, 20, object.clone())],
            &[],
        );

        let transform = read_transform(&file, 0, SceneReadLimits::default()).unwrap();
        assert_eq!(transform.path_id, 20);
        assert_eq!(transform.component.game_object.path_id, 10);
        assert_eq!(transform.local_rotation.w.to_bits(), 4.0_f32.to_bits());
        assert_eq!(transform.local_position.y.to_bits(), 6.0_f32.to_bits());
        assert_eq!(transform.local_scale.z.to_bits(), 10.0_f32.to_bits());
        assert_eq!(transform.children[0].path_id, 31);
        assert_eq!(transform.children[1].file_id, 1);
        assert_eq!(transform.father.path_id, 40);

        let file = parse_v22(
            "2022.3.62f1",
            13,
            Endian::Big,
            &[(RECT_TRANSFORM_CLASS_ID, 21, object)],
            &[],
        );
        let rect_transform = read_transform(&file, 0, SceneReadLimits::default()).unwrap();
        assert_eq!(rect_transform.path_id, 21);
        assert_eq!(rect_transform.local_position.y.to_bits(), 6.0_f32.to_bits());
    }

    #[test]
    fn reads_mesh_filter_and_stable_animator_reference_prefix() {
        let mut mesh_filter = Vec::new();
        push_pptr(&mut mesh_filter, 0, 1, 22, Endian::Little);
        push_pptr(&mut mesh_filter, 2, 90, 22, Endian::Little);

        let mut animator = Vec::new();
        push_pptr(&mut animator, 0, 1, 22, Endian::Little);
        animator.push(1);
        align(&mut animator, 4);
        push_pptr(&mut animator, 1, 91, 22, Endian::Little);
        push_pptr(&mut animator, 2, 92, 22, Endian::Little);
        // Version-dependent Animator flags deliberately remain unread.
        animator.extend_from_slice(&[0xa5; 32]);

        let file = parse_v22(
            "2023.1.0f1",
            13,
            Endian::Little,
            &[
                (MESH_FILTER_CLASS_ID, 2, mesh_filter),
                (ANIMATOR_CLASS_ID, 3, animator),
            ],
            &[],
        );
        let mesh_filter = read_mesh_filter(&file, 0, SceneReadLimits::default()).unwrap();
        assert_eq!(mesh_filter.component.game_object.path_id, 1);
        assert_eq!(mesh_filter.mesh.file_id, 2);
        assert_eq!(mesh_filter.mesh.path_id, 90);

        let animator = read_animator(&file, 1, SceneReadLimits::default()).unwrap();
        assert_eq!(animator.behaviour.component.game_object.path_id, 1);
        assert_eq!(animator.behaviour.enabled, 1);
        assert_eq!(animator.avatar.path_id, 91);
        assert_eq!(animator.controller.file_id, 2);
        assert_eq!(animator.controller.path_id, 92);
    }

    #[test]
    fn parsers_enforce_counts_strings_and_reference_bytes() {
        let mut game_object = Vec::new();
        push_i32(&mut game_object, 2, Endian::Little);
        push_pptr(&mut game_object, 0, 1, 22, Endian::Little);
        push_pptr(&mut game_object, 0, 2, 22, Endian::Little);
        push_i32(&mut game_object, 0, Endian::Little);
        push_aligned_string(&mut game_object, "Long Name", Endian::Little);
        let file = parse_v22(
            "2022.3.62f1",
            13,
            Endian::Little,
            &[(GAME_OBJECT_CLASS_ID, 1, game_object)],
            &[],
        );
        let count_limit = SceneReadLimits {
            maximum_game_object_components: 1,
            ..SceneReadLimits::default()
        };
        assert!(read_game_object(&file, 0, count_limit).is_err());
        let string_limit = SceneReadLimits {
            maximum_string_bytes: 4,
            ..SceneReadLimits::default()
        };
        assert!(read_game_object(&file, 0, string_limit).is_err());

        let mut mesh_filter = Vec::new();
        push_pptr(&mut mesh_filter, 0, 1, 22, Endian::Little);
        push_pptr(&mut mesh_filter, 0, 2, 22, Endian::Little);
        let file = parse_v22(
            "2022.3.62f1",
            13,
            Endian::Little,
            &[(MESH_FILTER_CLASS_ID, 2, mesh_filter)],
            &[],
        );
        let reference_limit = SceneReadLimits {
            maximum_reference_bytes: 12,
            ..SceneReadLimits::default()
        };
        let error = read_mesh_filter(&file, 0, reference_limit).unwrap_err();
        assert!(error.to_string().contains("reference"));

        let mut transform = Vec::new();
        push_pptr(&mut transform, 0, 1, 22, Endian::Little);
        push_f32s(&mut transform, &[0.0; 10], Endian::Little);
        push_i32(&mut transform, 2, Endian::Little);
        push_pptr(&mut transform, 0, 2, 22, Endian::Little);
        push_pptr(&mut transform, 0, 3, 22, Endian::Little);
        push_pptr(&mut transform, 0, 0, 22, Endian::Little);
        let file = parse_v22(
            "2022.3.62f1",
            13,
            Endian::Little,
            &[(TRANSFORM_CLASS_ID, 6, transform)],
            &[],
        );
        let child_limit = SceneReadLimits {
            maximum_transform_children: 1,
            ..SceneReadLimits::default()
        };
        assert!(read_transform(&file, 0, child_limit).is_err());

        let stripped = parse_v22(
            "0.0.0",
            13,
            Endian::Little,
            &[(GAME_OBJECT_CLASS_ID, 7, Vec::new())],
            &[],
        );
        let error = read_game_object(&stripped, 0, SceneReadLimits::default()).unwrap_err();
        assert!(error.to_string().contains("Unity version"));
    }

    #[test]
    fn parsers_are_source_bound_and_reject_impossible_child_bytes() {
        let mut truncated_transform = Vec::new();
        push_pptr(&mut truncated_transform, 0, 1, 22, Endian::Little);
        push_f32s(&mut truncated_transform, &[0.0; 10], Endian::Little);
        push_i32(&mut truncated_transform, 0, Endian::Little);
        let file = parse_v22(
            "2022.3.62f1",
            13,
            Endian::Little,
            &[
                (TRANSFORM_CLASS_ID, 3, truncated_transform),
                (GAME_OBJECT_CLASS_ID, 4, vec![0; 32]),
            ],
            &[],
        );
        // The following object's bytes must not satisfy the missing father PPtr.
        assert!(read_transform(&file, 0, SceneReadLimits::default()).is_err());

        let mut impossible_children = Vec::new();
        push_pptr(&mut impossible_children, 0, 1, 22, Endian::Little);
        push_f32s(&mut impossible_children, &[0.0; 10], Endian::Little);
        push_i32(&mut impossible_children, 2, Endian::Little);
        let file = parse_v22(
            "2022.3.62f1",
            13,
            Endian::Little,
            &[(TRANSFORM_CLASS_ID, 5, impossible_children)],
            &[],
        );
        let error = read_transform(&file, 0, SceneReadLimits::default()).unwrap_err();
        assert!(error.to_string().contains("needs at least"));
    }

    #[test]
    fn resolves_local_external_and_null_pptrs() {
        let collection = reference_collection();
        let local = resolve_object_reference(
            &collection,
            0,
            ObjectReference {
                file_id: 0,
                path_id: 7,
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(local.file_index, 0);
        assert_eq!(local.object.class_id, GAME_OBJECT_CLASS_ID);

        let external_reference = ObjectReference {
            file_id: 1,
            path_id: 77,
        };
        let external =
            resolve_typed_object_reference(&collection, 0, external_reference, TRANSFORM_CLASS_ID)
                .unwrap()
                .unwrap();
        assert_eq!(external.file_index, 1);
        assert_eq!(external.file_path, "bundle::folder/TARGET.ASSETS");
        assert_eq!(external.object_index, 0);

        for null in [
            ObjectReference {
                file_id: 99,
                path_id: 0,
            },
            ObjectReference {
                file_id: -1,
                path_id: 77,
            },
        ] {
            assert!(
                resolve_object_reference(&collection, 0, null)
                    .unwrap()
                    .is_none()
            );
        }
    }

    #[test]
    fn rejects_bad_local_external_and_typed_references() {
        let collection = reference_collection();
        let external_reference = ObjectReference {
            file_id: 1,
            path_id: 77,
        };
        assert!(
            resolve_object_reference(
                &collection,
                0,
                ObjectReference {
                    file_id: 2,
                    path_id: 77
                }
            )
            .is_err()
        );
        assert!(
            resolve_object_reference(
                &collection,
                0,
                ObjectReference {
                    file_id: 1,
                    path_id: 999
                }
            )
            .is_err()
        );
        assert!(
            resolve_typed_object_reference(
                &collection,
                0,
                ObjectReference {
                    file_id: 0,
                    path_id: 7
                },
                TRANSFORM_CLASS_ID
            )
            .is_err()
        );
        assert!(resolve_object_reference(&collection, 99, external_reference).is_err());

        let source_only = AssetCollection::from_loaded_parts(
            vec![collection.serialized_files[0].clone()],
            Vec::new(),
        );
        assert!(resolve_object_reference(&source_only, 0, external_reference).is_err());
    }

    #[test]
    fn indexed_resolution_preserves_first_duplicate_file_and_object() {
        let source = parse_v22(
            "2022.3.62f1",
            13,
            Endian::Little,
            &[(GAME_OBJECT_CLASS_ID, 7, Vec::new())],
            &["archive:/shared/target.assets"],
        );
        let mut first_target = parse_v22(
            "2022.3.62f1",
            13,
            Endian::Little,
            &[
                (TRANSFORM_CLASS_ID, 77, Vec::new()),
                (GAME_OBJECT_CLASS_ID, 78, Vec::new()),
            ],
            &[],
        );
        // `SerializedFile::open` rejects duplicate IDs, but the low-level collection fields are
        // public and may be assembled or edited by callers. Index semantics still retain the
        // first serialized object in that state.
        first_target.objects[1].path_id = 77;
        let second_target = parse_v22(
            "2022.3.62f1",
            13,
            Endian::Little,
            &[(GAME_OBJECT_CLASS_ID, 77, Vec::new())],
            &[],
        );
        let mut collection = AssetCollection::from_loaded_parts(
            vec![
                LoadedSerializedFile {
                    path: "source.assets".to_owned(),
                    file: source,
                },
                LoadedSerializedFile {
                    path: "first/TARGET.ASSETS".to_owned(),
                    file: first_target,
                },
                LoadedSerializedFile {
                    path: "second/target.assets".to_owned(),
                    file: second_target,
                },
            ],
            Vec::new(),
        );
        collection
            .rebuild_reference_index(crate::loader::AssetLoadLimits::default())
            .unwrap();

        let resolved = resolve_object_reference(
            &collection,
            0,
            ObjectReference {
                file_id: 1,
                path_id: 77,
            },
        )
        .unwrap()
        .unwrap();

        assert_eq!(resolved.file_index, 1);
        assert_eq!(resolved.object_index, 0);
        assert_eq!(resolved.object.class_id, TRANSFORM_CLASS_ID);
    }

    #[test]
    fn indexed_resolution_falls_back_after_public_object_table_mutation() {
        let mut collection = reference_collection();
        collection
            .rebuild_reference_index(crate::loader::AssetLoadLimits::default())
            .unwrap();

        collection.serialized_files[0].file.objects[0].path_id = 8;
        let moved = resolve_object_reference(
            &collection,
            0,
            ObjectReference {
                file_id: 0,
                path_id: 8,
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(moved.object_index, 0);
        assert_eq!(moved.object.path_id, 8);
        assert!(
            resolve_object_reference(
                &collection,
                0,
                ObjectReference {
                    file_id: 0,
                    path_id: 7,
                },
            )
            .is_err()
        );

        collection.serialized_files[0].file.objects.clear();
        let error = resolve_object_reference(
            &collection,
            0,
            ObjectReference {
                file_id: 0,
                path_id: 7,
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("PPtr path ID 7 was not found"));
    }

    #[test]
    fn indexed_resolution_falls_back_after_public_object_reordering() {
        let file = parse_v22(
            "2022.3.62f1",
            13,
            Endian::Little,
            &[
                (TRANSFORM_CLASS_ID, 7, Vec::new()),
                (GAME_OBJECT_CLASS_ID, 8, Vec::new()),
            ],
            &[],
        );
        let mut collection = AssetCollection::from_loaded_parts(
            vec![LoadedSerializedFile {
                path: "objects.assets".to_owned(),
                file,
            }],
            Vec::new(),
        );
        collection
            .rebuild_reference_index(crate::loader::AssetLoadLimits::default())
            .unwrap();
        collection.serialized_files[0].file.objects.swap(0, 1);

        let resolved = resolve_typed_object_reference(
            &collection,
            0,
            ObjectReference {
                file_id: 0,
                path_id: 7,
            },
            TRANSFORM_CLASS_ID,
        )
        .unwrap()
        .unwrap();
        assert_eq!(resolved.object_index, 1);
        assert_eq!(resolved.object.path_id, 7);
    }

    #[test]
    fn indexed_resolution_falls_back_after_public_file_removal() {
        let mut popped = reference_collection();
        popped
            .rebuild_reference_index(crate::loader::AssetLoadLimits::default())
            .unwrap();
        popped.serialized_files.pop();
        let error = resolve_object_reference(
            &popped,
            0,
            ObjectReference {
                file_id: 1,
                path_id: 77,
            },
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("was not found in the asset collection")
        );
    }

    #[test]
    fn indexed_resolution_falls_back_after_public_file_path_rename() {
        let mut renamed = reference_collection();
        renamed
            .rebuild_reference_index(crate::loader::AssetLoadLimits::default())
            .unwrap();
        renamed.serialized_files[0].file.externals[0].path =
            "archive:/shared/aaa.assets".to_owned();
        renamed.serialized_files[1].path = "bundle::folder/AAA.ASSETS".to_owned();
        let resolved = resolve_object_reference(
            &renamed,
            0,
            ObjectReference {
                file_id: 1,
                path_id: 77,
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(resolved.file_index, 1);
    }

    #[test]
    fn indexed_resolution_falls_back_after_public_file_reordering() {
        let source = parse_v22(
            "2022.3.62f1",
            13,
            Endian::Little,
            &[(GAME_OBJECT_CLASS_ID, 7, Vec::new())],
            &["archive:/shared/a.assets"],
        );
        let mut reordered = AssetCollection::from_loaded_parts(
            vec![
                LoadedSerializedFile {
                    path: "source.assets".to_owned(),
                    file: source,
                },
                LoadedSerializedFile {
                    path: "a.assets".to_owned(),
                    file: parse_v22(
                        "2022.3.62f1",
                        13,
                        Endian::Little,
                        &[(TRANSFORM_CLASS_ID, 77, Vec::new())],
                        &[],
                    ),
                },
                LoadedSerializedFile {
                    path: "b.assets".to_owned(),
                    file: parse_v22(
                        "2022.3.62f1",
                        13,
                        Endian::Little,
                        &[(GAME_OBJECT_CLASS_ID, 88, Vec::new())],
                        &[],
                    ),
                },
                LoadedSerializedFile {
                    path: "c.assets".to_owned(),
                    file: parse_v22(
                        "2022.3.62f1",
                        13,
                        Endian::Little,
                        &[(GAME_OBJECT_CLASS_ID, 99, Vec::new())],
                        &[],
                    ),
                },
            ],
            Vec::new(),
        );
        reordered
            .rebuild_reference_index(crate::loader::AssetLoadLimits::default())
            .unwrap();
        reordered.serialized_files.reverse();

        let resolved = resolve_typed_object_reference(
            &reordered,
            3,
            ObjectReference {
                file_id: 1,
                path_id: 77,
            },
            TRANSFORM_CLASS_ID,
        )
        .unwrap()
        .unwrap();
        assert_eq!(resolved.file_index, 2);
        assert_eq!(resolved.file_path, "a.assets");
        assert_eq!(resolved.object_index, 0);
    }

    fn reference_collection() -> AssetCollection {
        let source = parse_v22(
            "2022.3.62f1",
            13,
            Endian::Little,
            &[(GAME_OBJECT_CLASS_ID, 7, Vec::new())],
            &["archive:/shared/target.assets"],
        );
        let target = parse_v22(
            "2022.3.62f1",
            13,
            Endian::Little,
            &[(TRANSFORM_CLASS_ID, 77, Vec::new())],
            &[],
        );
        AssetCollection::from_loaded_parts(
            vec![
                LoadedSerializedFile {
                    path: "root/source.assets".to_owned(),
                    file: source,
                },
                LoadedSerializedFile {
                    path: "bundle::folder/TARGET.ASSETS".to_owned(),
                    file: target,
                },
            ],
            Vec::new(),
        )
    }

    fn parse_v22(
        unity_version: &str,
        target_platform: i32,
        endian: Endian,
        objects: &[(i32, i64, Vec<u8>)],
        externals: &[&str],
    ) -> SerializedFile {
        SerializedFile::open(Region::from_bytes(synthetic_v22(
            unity_version,
            target_platform,
            endian,
            objects,
            externals,
        )))
        .unwrap()
    }

    fn synthetic_v22(
        unity_version: &str,
        target_platform: i32,
        endian: Endian,
        objects: &[(i32, i64, Vec<u8>)],
        externals: &[&str],
    ) -> Vec<u8> {
        let mut classes = Vec::new();
        for (class_id, _, _) in objects {
            if !classes.contains(class_id) {
                classes.push(*class_id);
            }
        }

        let mut metadata = Vec::new();
        metadata.extend_from_slice(unity_version.as_bytes());
        metadata.push(0);
        push_i32(&mut metadata, target_platform, endian);
        metadata.push(0);
        push_i32(&mut metadata, i32::try_from(classes.len()).unwrap(), endian);
        for class_id in &classes {
            push_i32(&mut metadata, *class_id, endian);
            metadata.push(0);
            push_i16(&mut metadata, -1, endian);
            metadata.extend_from_slice(&[0; 16]);
        }

        let mut data = Vec::new();
        let mut records = Vec::new();
        for (class_id, path_id, payload) in objects {
            align(&mut data, 4);
            let offset = i64::try_from(data.len()).unwrap();
            let type_index = classes.iter().position(|value| value == class_id).unwrap();
            records.push((
                *path_id,
                offset,
                u32::try_from(payload.len()).unwrap(),
                i32::try_from(type_index).unwrap(),
            ));
            data.extend_from_slice(payload);
        }

        push_i32(&mut metadata, i32::try_from(records.len()).unwrap(), endian);
        for (path_id, offset, size, type_index) in records {
            align_with_base(&mut metadata, 48, 4);
            push_i64(&mut metadata, path_id, endian);
            push_i64(&mut metadata, offset, endian);
            push_u32(&mut metadata, size, endian);
            push_i32(&mut metadata, type_index, endian);
        }
        push_i32(&mut metadata, 0, endian);
        push_i32(
            &mut metadata,
            i32::try_from(externals.len()).unwrap(),
            endian,
        );
        for external in externals {
            metadata.push(0);
            metadata.extend_from_slice(&[0; 16]);
            push_i32(&mut metadata, 0, endian);
            metadata.extend_from_slice(external.as_bytes());
            metadata.push(0);
        }
        push_i32(&mut metadata, 0, endian);
        metadata.push(0);

        let metadata_size = u32::try_from(metadata.len()).unwrap();
        let metadata_end = 48_u64 + u64::from(metadata_size);
        let data_offset = metadata_end.div_ceil(16) * 16;
        let file_size = data_offset + u64::try_from(data.len()).unwrap();
        let mut bytes = vec![0; 48];
        bytes[8..12].copy_from_slice(&22_u32.to_be_bytes());
        bytes[16] = u8::from(endian == Endian::Big);
        bytes[20..24].copy_from_slice(&metadata_size.to_be_bytes());
        bytes[24..32].copy_from_slice(&i64::try_from(file_size).unwrap().to_be_bytes());
        bytes[32..40].copy_from_slice(&i64::try_from(data_offset).unwrap().to_be_bytes());
        bytes.extend_from_slice(&metadata);
        bytes.resize(usize::try_from(data_offset).unwrap(), 0);
        bytes.extend_from_slice(&data);
        bytes
    }

    fn parse_v13(class_id: i32, object: &[u8]) -> SerializedFile {
        let mut metadata = Vec::new();
        metadata.extend_from_slice(b"5.4.0f1\0");
        push_i32(&mut metadata, 13, Endian::Little);
        metadata.push(0);
        push_i32(&mut metadata, 1, Endian::Little);
        push_i32(&mut metadata, class_id, Endian::Little);
        metadata.extend_from_slice(&[0; 16]);
        push_i32(&mut metadata, 0, Endian::Little);
        push_i32(&mut metadata, 1, Endian::Little);
        push_i32(&mut metadata, 7, Endian::Little);
        push_u32(&mut metadata, 0, Endian::Little);
        push_u32(
            &mut metadata,
            u32::try_from(object.len()).unwrap(),
            Endian::Little,
        );
        push_i32(&mut metadata, class_id, Endian::Little);
        push_u16(
            &mut metadata,
            u16::try_from(class_id).unwrap(),
            Endian::Little,
        );
        push_i16(&mut metadata, -1, Endian::Little);
        push_i32(&mut metadata, 0, Endian::Little);
        push_i32(&mut metadata, 0, Endian::Little);
        metadata.push(0);

        let data_offset = (20 + metadata.len()).div_ceil(16) * 16;
        let file_size = data_offset + object.len();
        let mut bytes = vec![0; 20];
        bytes[0..4].copy_from_slice(&u32::try_from(metadata.len()).unwrap().to_be_bytes());
        bytes[4..8].copy_from_slice(&u32::try_from(file_size).unwrap().to_be_bytes());
        bytes[8..12].copy_from_slice(&13_u32.to_be_bytes());
        bytes[12..16].copy_from_slice(&u32::try_from(data_offset).unwrap().to_be_bytes());
        bytes[16] = 0;
        bytes.extend_from_slice(&metadata);
        bytes.resize(data_offset, 0);
        bytes.extend_from_slice(object);
        SerializedFile::open(Region::from_bytes(bytes)).unwrap()
    }

    fn push_pptr(
        output: &mut Vec<u8>,
        file_id: i32,
        path_id: i64,
        format_version: u32,
        endian: Endian,
    ) {
        push_i32(output, file_id, endian);
        if format_version < 14 {
            push_i32(output, i32::try_from(path_id).unwrap(), endian);
        } else {
            push_i64(output, path_id, endian);
        }
    }

    fn push_aligned_string(output: &mut Vec<u8>, value: &str, endian: Endian) {
        push_i32(output, i32::try_from(value.len()).unwrap(), endian);
        output.extend_from_slice(value.as_bytes());
        if !value.is_empty() {
            align(output, 4);
        }
    }

    fn push_f32s(output: &mut Vec<u8>, values: &[f32], endian: Endian) {
        for value in values {
            match endian {
                Endian::Little => output.extend_from_slice(&value.to_le_bytes()),
                Endian::Big => output.extend_from_slice(&value.to_be_bytes()),
            }
        }
    }

    fn push_i16(output: &mut Vec<u8>, value: i16, endian: Endian) {
        match endian {
            Endian::Little => output.extend_from_slice(&value.to_le_bytes()),
            Endian::Big => output.extend_from_slice(&value.to_be_bytes()),
        }
    }

    fn push_u16(output: &mut Vec<u8>, value: u16, endian: Endian) {
        match endian {
            Endian::Little => output.extend_from_slice(&value.to_le_bytes()),
            Endian::Big => output.extend_from_slice(&value.to_be_bytes()),
        }
    }

    fn push_i32(output: &mut Vec<u8>, value: i32, endian: Endian) {
        match endian {
            Endian::Little => output.extend_from_slice(&value.to_le_bytes()),
            Endian::Big => output.extend_from_slice(&value.to_be_bytes()),
        }
    }

    fn push_u32(output: &mut Vec<u8>, value: u32, endian: Endian) {
        match endian {
            Endian::Little => output.extend_from_slice(&value.to_le_bytes()),
            Endian::Big => output.extend_from_slice(&value.to_be_bytes()),
        }
    }

    fn push_i64(output: &mut Vec<u8>, value: i64, endian: Endian) {
        match endian {
            Endian::Little => output.extend_from_slice(&value.to_le_bytes()),
            Endian::Big => output.extend_from_slice(&value.to_be_bytes()),
        }
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
