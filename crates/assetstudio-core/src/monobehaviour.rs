use std::io::{self, Write};

use crate::endian::{Endian, EndianReader, checked_length};
use crate::json::write_type_value_json;
use crate::serialized::{ObjectInfo, SerializedFile};
use crate::source::{Region, RegionCursor};
use crate::type_tree::TypeTreeReadLimits;
use crate::{Error, Result};

pub const MONO_BEHAVIOUR_CLASS_ID: i32 = 114;
pub const MONO_SCRIPT_CLASS_ID: i32 = 115;

const NO_TARGET_PLATFORM: i32 = -2;

/// Limits applied while reading the fixed `MonoBehaviour` header, `MonoScript`
/// metadata, and an embedded-TypeTree JSON representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MonoBehaviourReadLimits {
    pub maximum_string_bytes: usize,
    pub maximum_total_string_bytes: usize,
    pub maximum_json_bytes: usize,
    pub type_tree: TypeTreeReadLimits,
}

impl Default for MonoBehaviourReadLimits {
    fn default() -> Self {
        Self {
            maximum_string_bytes: 16 * 1024 * 1024,
            maximum_total_string_bytes: 64 * 1024 * 1024,
            maximum_json_bytes: 256 * 1024 * 1024,
            type_tree: TypeTreeReadLimits::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ObjectReference {
    pub file_id: i32,
    pub path_id: i64,
}

impl ObjectReference {
    #[must_use]
    pub const fn is_null(self) -> bool {
        self.path_id == 0 || self.file_id < 0
    }
}

/// The engine-owned prefix common to every serialized `MonoBehaviour`.
/// Script-defined fields follow these values in the object payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonoBehaviour {
    pub path_id: i64,
    pub game_object: ObjectReference,
    pub enabled: u8,
    pub script: ObjectReference,
    pub name: String,
}

/// Metadata stored in a `MonoScript` object and used to locate the managed
/// type that supplies a stripped `MonoBehaviour` schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonoScript {
    pub path_id: i64,
    pub name: String,
    pub execution_order: Option<i32>,
    pub class_name: String,
    pub namespace: String,
    pub assembly_name: String,
    pub is_editor_script: Option<bool>,
}

/// Reads only the stable engine-owned prefix. This deliberately does not try
/// to infer the layout of any script-defined fields that follow `m_Name`.
pub fn read_mono_behaviour(
    file: &SerializedFile,
    object_index: usize,
    limits: MonoBehaviourReadLimits,
) -> Result<MonoBehaviour> {
    read_mono_behaviour_with_script_data(file, object_index, limits)
        .map(|(behaviour, _script_data)| behaviour)
}

/// Reads the stable engine-owned prefix and returns the source-bound bytes
/// occupied by the script-defined fields that follow `m_Name`.
///
/// The returned region is intentionally untyped. Callers must already know
/// the matching managed schema (for example, the verified `CubismMoc` byte
/// array layout) before interpreting it.
pub fn read_mono_behaviour_with_script_data(
    file: &SerializedFile,
    object_index: usize,
    limits: MonoBehaviourReadLimits,
) -> Result<(MonoBehaviour, Region)> {
    let mut reader = ObjectPayloadReader::new(
        file,
        object_index,
        MONO_BEHAVIOUR_CLASS_ID,
        "MonoBehaviour",
        limits,
    )?;
    reader.read_editor_extension_prefix()?;
    let game_object = reader.read_pptr()?;
    let enabled = reader.reader.read_u8()?;
    reader.align(4)?;
    let script = reader.read_pptr()?;
    let name = reader.read_aligned_string("MonoBehaviour name")?;
    let script_data_offset = reader.reader.position()?;
    let script_data_length = reader
        .region
        .len()
        .checked_sub(script_data_offset)
        .ok_or_else(|| Error::invalid_data("MonoBehaviour script-data offset is out of range"))?;
    let script_data = reader
        .region
        .subregion(script_data_offset, script_data_length)?;

    Ok((
        MonoBehaviour {
            path_id: reader.path_id,
            game_object,
            enabled,
            script,
            name,
        },
        script_data,
    ))
}

/// Reads a complete `MonoBehaviour` through its file-embedded `TypeTree` and
/// writes bounded JSON. If Unity stripped the tree, a managed assembly (or a
/// dummy DLL generated from matching metadata) is required to reconstruct the
/// script schema; guessing from the remaining bytes is unsafe.
pub fn read_mono_behaviour_json(
    file: &SerializedFile,
    object_index: usize,
    pretty: bool,
    limits: MonoBehaviourReadLimits,
) -> Result<String> {
    let object =
        require_object_class(file, object_index, MONO_BEHAVIOUR_CLASS_ID, "MonoBehaviour")?;
    let has_type_tree = object
        .serialized_type_index
        .and_then(|index| file.types.get(index))
        .and_then(|kind| kind.type_tree.as_ref())
        .is_some();
    if !has_type_tree {
        return Err(Error::unsupported(format!(
            "MonoBehaviour {} has no serialized TypeTree; its script fields require a managed assembly or matching dummy-DLL schema",
            object.path_id
        )));
    }

    let value = file.read_type_tree_value_with_limits(object_index, limits.type_tree)?;
    let mut output = BoundedJsonOutput::new(limits.maximum_json_bytes);
    let write_result = write_type_value_json(&value, &mut output, pretty);
    if output.limit_exceeded {
        return Err(Error::invalid_data(format!(
            "MonoBehaviour JSON exceeds the {} byte limit",
            limits.maximum_json_bytes
        )));
    }
    write_result?;
    String::from_utf8(output.bytes)
        .map_err(|error| Error::invalid_data(format!("MonoBehaviour JSON is not UTF-8: {error}")))
}

/// Reads the source-bound metadata needed to identify the managed class behind
/// a `MonoBehaviour`. This parses only Unity's documented, version-gated
/// `MonoScript` fields and never opens or executes the named assembly.
pub fn read_mono_script(
    file: &SerializedFile,
    object_index: usize,
    limits: MonoBehaviourReadLimits,
) -> Result<MonoScript> {
    if file.unity_version.is_stripped() {
        return Err(Error::unsupported(
            "MonoScript metadata requires a Unity version because its layout is version-dependent",
        ));
    }
    let version = file.unity_version.components();
    let mut reader = ObjectPayloadReader::new(
        file,
        object_index,
        MONO_SCRIPT_CLASS_ID,
        "MonoScript",
        limits,
    )?;
    reader.read_editor_extension_prefix()?;
    let name = reader.read_aligned_string("MonoScript name")?;

    let execution_order = if version >= (3, 4, 0) {
        let value = reader.reader.read_i32()?;
        if version < (5, 0, 0) {
            reader.skip(4, "MonoScript properties hash")?;
        } else {
            reader.skip(16, "MonoScript properties hash")?;
        }
        Some(value)
    } else {
        None
    };
    if version < (3, 0, 0) {
        let _path_name = reader.read_aligned_string("MonoScript legacy path name")?;
    }
    let class_name = reader.read_aligned_string("MonoScript class name")?;
    let namespace = if version >= (3, 0, 0) {
        reader.read_aligned_string("MonoScript namespace")?
    } else {
        String::new()
    };
    let assembly_name = reader.read_aligned_string("MonoScript assembly name")?;
    let is_editor_script = if version < (2018, 2, 0) {
        Some(reader.reader.read_bool()?)
    } else {
        None
    };

    Ok(MonoScript {
        path_id: reader.path_id,
        name,
        execution_order,
        class_name,
        namespace,
        assembly_name,
        is_editor_script,
    })
}

fn require_object_class<'a>(
    file: &'a SerializedFile,
    object_index: usize,
    expected_class_id: i32,
    class_name: &str,
) -> Result<&'a ObjectInfo> {
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
    Ok(object)
}

struct ObjectPayloadReader {
    reader: EndianReader<RegionCursor>,
    region: Region,
    absolute_start: u64,
    path_id: i64,
    target_platform: i32,
    format_version: u32,
    limits: MonoBehaviourReadLimits,
    string_bytes_read: usize,
}

impl ObjectPayloadReader {
    fn new(
        file: &SerializedFile,
        object_index: usize,
        expected_class_id: i32,
        class_name: &str,
        limits: MonoBehaviourReadLimits,
    ) -> Result<Self> {
        let object = require_object_class(file, object_index, expected_class_id, class_name)?;
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
            string_bytes_read: 0,
        })
    }

    fn read_editor_extension_prefix(&mut self) -> Result<()> {
        if self.target_platform == NO_TARGET_PLATFORM {
            self.skip(4, "Object hide flags")?;
            let _prefab_parent = self.read_pptr()?;
            let _prefab_internal = self.read_pptr()?;
        }
        Ok(())
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
        self.string_bytes_read = self
            .string_bytes_read
            .checked_add(length)
            .ok_or_else(|| Error::invalid_data("MonoBehaviour string byte budget overflowed"))?;
        if self.string_bytes_read > self.limits.maximum_total_string_bytes {
            return Err(Error::invalid_data(format!(
                "MonoBehaviour strings exceed the {} byte total limit",
                self.limits.maximum_total_string_bytes
            )));
        }
        let value = self.reader.read_utf8(length)?;
        if length != 0 {
            self.align(4)?;
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
}

struct BoundedJsonOutput {
    bytes: Vec<u8>,
    maximum: usize,
    limit_exceeded: bool,
}

impl BoundedJsonOutput {
    fn new(maximum: usize) -> Self {
        Self {
            bytes: Vec::new(),
            maximum,
            limit_exceeded: false,
        }
    }
}

impl Write for BoundedJsonOutput {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let new_length = self
            .bytes
            .len()
            .checked_add(buffer.len())
            .ok_or_else(|| io::Error::other("MonoBehaviour JSON length overflowed"))?;
        if new_length > self.maximum {
            self.limit_exceeded = true;
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "MonoBehaviour JSON byte limit exceeded",
            ));
        }
        self.bytes
            .try_reserve(buffer.len())
            .map_err(|error| io::Error::other(format!("cannot allocate JSON output: {error}")))?;
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::source::Region;

    use super::{
        MONO_BEHAVIOUR_CLASS_ID, MONO_SCRIPT_CLASS_ID, MonoBehaviourReadLimits, ObjectReference,
        read_mono_behaviour, read_mono_behaviour_json, read_mono_behaviour_with_script_data,
        read_mono_script,
    };

    #[test]
    fn reads_v22_base_fields_and_prefers_embedded_type_tree_json() {
        let mut object = Vec::new();
        push_pptr(&mut object, 0, 41);
        object.push(1);
        align(&mut object, 4);
        push_pptr(&mut object, 2, 99);
        push_aligned_string(&mut object, "Hero");
        push_i32(&mut object, 123);
        let tree = mono_behaviour_tree();
        let file = parse_v22(MONO_BEHAVIOUR_CLASS_ID, "2022.3.62f1", &object, Some(&tree));

        let behaviour = read_mono_behaviour(&file, 0, MonoBehaviourReadLimits::default()).unwrap();
        assert_eq!(behaviour.path_id, 7);
        assert_eq!(
            behaviour.game_object,
            ObjectReference {
                file_id: 0,
                path_id: 41
            }
        );
        assert_eq!(behaviour.enabled, 1);
        assert_eq!(
            behaviour.script,
            ObjectReference {
                file_id: 2,
                path_id: 99
            }
        );
        assert_eq!(behaviour.name, "Hero");
        let (_behaviour, script_data) =
            read_mono_behaviour_with_script_data(&file, 0, MonoBehaviourReadLimits::default())
                .unwrap();
        assert_eq!(script_data.read_to_vec(4).unwrap(), 123_i32.to_le_bytes());

        let json =
            read_mono_behaviour_json(&file, 0, false, MonoBehaviourReadLimits::default()).unwrap();
        let json: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(json["m_GameObject"]["m_PathID"], 41);
        assert_eq!(json["m_Enabled"], 1);
        assert_eq!(json["m_Script"]["m_FileID"], 2);
        assert_eq!(json["m_Name"], "Hero");
        assert_eq!(json["score"], 123);
    }

    #[test]
    fn no_tree_requires_a_managed_or_dummy_dll_schema_without_guessing() {
        let mut object = Vec::new();
        push_pptr(&mut object, 0, 1);
        object.push(1);
        align(&mut object, 4);
        push_pptr(&mut object, 0, 2);
        push_aligned_string(&mut object, "NoTree");
        object.extend_from_slice(b"unknown script bytes");
        let file = parse_v22(MONO_BEHAVIOUR_CLASS_ID, "2022.3.62f1", &object, None);

        let base = read_mono_behaviour(&file, 0, MonoBehaviourReadLimits::default()).unwrap();
        assert_eq!(base.name, "NoTree");
        let error = read_mono_behaviour_json(&file, 0, false, MonoBehaviourReadLimits::default())
            .unwrap_err();
        let message = error.to_string();
        assert!(message.contains("managed assembly"));
        assert!(message.contains("dummy-DLL schema"));
    }

    #[test]
    fn parses_source_bound_mono_script_metadata() {
        let mut object = Vec::new();
        push_aligned_string(&mut object, "Mover script");
        push_i32(&mut object, -10);
        object.extend_from_slice(&[0xa5; 16]);
        push_aligned_string(&mut object, "Mover");
        push_aligned_string(&mut object, "Game.Logic");
        push_aligned_string(&mut object, "Assembly-CSharp.dll");
        let file = parse_v22(MONO_SCRIPT_CLASS_ID, "2022.3.62f1", &object, None);

        let script = read_mono_script(&file, 0, MonoBehaviourReadLimits::default()).unwrap();

        assert_eq!(script.path_id, 7);
        assert_eq!(script.name, "Mover script");
        assert_eq!(script.execution_order, Some(-10));
        assert_eq!(script.class_name, "Mover");
        assert_eq!(script.namespace, "Game.Logic");
        assert_eq!(script.assembly_name, "Assembly-CSharp.dll");
        assert_eq!(script.is_editor_script, None);
    }

    #[test]
    fn enforces_class_string_and_json_limits() {
        let mut object = Vec::new();
        push_pptr(&mut object, 0, 1);
        object.push(1);
        align(&mut object, 4);
        push_pptr(&mut object, 0, 2);
        push_aligned_string(&mut object, "Hero");
        push_i32(&mut object, 123);
        let tree = mono_behaviour_tree();
        let file = parse_v22(MONO_BEHAVIOUR_CLASS_ID, "2022.3.62f1", &object, Some(&tree));

        let short_strings = MonoBehaviourReadLimits {
            maximum_string_bytes: 3,
            ..MonoBehaviourReadLimits::default()
        };
        assert!(read_mono_behaviour(&file, 0, short_strings).is_err());

        let short_json = MonoBehaviourReadLimits {
            maximum_json_bytes: 8,
            ..MonoBehaviourReadLimits::default()
        };
        let error = read_mono_behaviour_json(&file, 0, false, short_json).unwrap_err();
        assert!(error.to_string().contains("JSON exceeds"));

        let script_file = parse_v22(MONO_SCRIPT_CLASS_ID, "2022.3.62f1", &[], None);
        let error =
            read_mono_behaviour(&script_file, 0, MonoBehaviourReadLimits::default()).unwrap_err();
        assert!(error.to_string().contains("not MonoBehaviour (114)"));
    }

    #[derive(Clone, Copy)]
    struct TestNode {
        type_name: &'static str,
        field_name: &'static str,
        level: u8,
        align: bool,
    }

    fn mono_behaviour_tree() -> Vec<TestNode> {
        vec![
            node("MonoBehaviour", "Base", 0, false),
            node("PPtr<GameObject>", "m_GameObject", 1, false),
            node("int", "m_FileID", 2, false),
            node("SInt64", "m_PathID", 2, false),
            node("UInt8", "m_Enabled", 1, true),
            node("PPtr<MonoScript>", "m_Script", 1, false),
            node("int", "m_FileID", 2, false),
            node("SInt64", "m_PathID", 2, false),
            node("string", "m_Name", 1, false),
            node("Array", "Array", 2, true),
            node("int", "size", 3, false),
            node("char", "data", 3, false),
            node("int", "score", 1, false),
        ]
    }

    const fn node(
        type_name: &'static str,
        field_name: &'static str,
        level: u8,
        align: bool,
    ) -> TestNode {
        TestNode {
            type_name,
            field_name,
            level,
            align,
        }
    }

    fn parse_v22(
        class_id: i32,
        unity_version: &str,
        object: &[u8],
        tree: Option<&[TestNode]>,
    ) -> crate::serialized::SerializedFile {
        crate::serialized::SerializedFile::open(Region::from_bytes(synthetic_v22(
            class_id,
            unity_version,
            object,
            tree,
        )))
        .unwrap()
    }

    fn synthetic_v22(
        class_id: i32,
        unity_version: &str,
        object: &[u8],
        tree: Option<&[TestNode]>,
    ) -> Vec<u8> {
        let mut metadata = Vec::new();
        metadata.extend_from_slice(unity_version.as_bytes());
        metadata.push(0);
        push_i32(&mut metadata, 13);
        metadata.push(u8::from(tree.is_some()));
        push_i32(&mut metadata, 1);
        push_i32(&mut metadata, class_id);
        metadata.push(0);
        metadata.extend_from_slice(&(-1_i16).to_le_bytes());
        if class_id == MONO_BEHAVIOUR_CLASS_ID {
            metadata.extend_from_slice(&[0x11; 16]);
        }
        metadata.extend_from_slice(&[0x22; 16]);
        if let Some(nodes) = tree {
            push_blob_tree(&mut metadata, nodes);
            push_i32(&mut metadata, 0);
        }

        push_i32(&mut metadata, 1);
        align_with_base(&mut metadata, 48, 4);
        metadata.extend_from_slice(&7_i64.to_le_bytes());
        metadata.extend_from_slice(&0_i64.to_le_bytes());
        metadata.extend_from_slice(&u32::try_from(object.len()).unwrap().to_le_bytes());
        push_i32(&mut metadata, 0);
        push_i32(&mut metadata, 0);
        push_i32(&mut metadata, 0);
        push_i32(&mut metadata, 0);
        metadata.push(0);

        let metadata_size = u32::try_from(metadata.len()).unwrap();
        let metadata_end = 48_u64 + u64::from(metadata_size);
        let data_offset = metadata_end.div_ceil(16) * 16;
        let file_size = data_offset + u64::try_from(object.len()).unwrap();
        let mut bytes = vec![0_u8; 48];
        bytes[8..12].copy_from_slice(&22_u32.to_be_bytes());
        bytes[16] = 0;
        bytes[20..24].copy_from_slice(&metadata_size.to_be_bytes());
        bytes[24..32].copy_from_slice(&i64::try_from(file_size).unwrap().to_be_bytes());
        bytes[32..40].copy_from_slice(&i64::try_from(data_offset).unwrap().to_be_bytes());
        bytes.extend_from_slice(&metadata);
        bytes.resize(usize::try_from(data_offset).unwrap(), 0);
        bytes.extend_from_slice(object);
        bytes
    }

    fn push_blob_tree(output: &mut Vec<u8>, nodes: &[TestNode]) {
        let mut strings = Vec::new();
        let mut offsets = Vec::new();
        for node in nodes {
            let type_offset = u32::try_from(strings.len()).unwrap();
            strings.extend_from_slice(node.type_name.as_bytes());
            strings.push(0);
            let name_offset = u32::try_from(strings.len()).unwrap();
            strings.extend_from_slice(node.field_name.as_bytes());
            strings.push(0);
            offsets.push((type_offset, name_offset));
        }

        push_i32(output, i32::try_from(nodes.len()).unwrap());
        push_i32(output, i32::try_from(strings.len()).unwrap());
        for (index, (node, (type_offset, name_offset))) in nodes.iter().zip(offsets).enumerate() {
            output.extend_from_slice(&1_u16.to_le_bytes());
            output.push(node.level);
            output.push(0);
            output.extend_from_slice(&type_offset.to_le_bytes());
            output.extend_from_slice(&name_offset.to_le_bytes());
            output.extend_from_slice(&(-1_i32).to_le_bytes());
            output.extend_from_slice(&i32::try_from(index).unwrap().to_le_bytes());
            output.extend_from_slice(&(if node.align { 0x4000_i32 } else { 0 }).to_le_bytes());
            output.extend_from_slice(&0_u64.to_le_bytes());
        }
        output.extend_from_slice(&strings);
    }

    fn push_pptr(output: &mut Vec<u8>, file_id: i32, path_id: i64) {
        output.extend_from_slice(&file_id.to_le_bytes());
        output.extend_from_slice(&path_id.to_le_bytes());
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
