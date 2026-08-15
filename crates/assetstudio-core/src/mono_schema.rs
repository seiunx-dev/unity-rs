//! External, non-executing schemas for stripped `MonoBehaviour` objects.
//!
//! Providers return a complete Unity `TypeTree` for the serialized object.
//! The Core never opens or executes the named managed assembly.

use std::io::{self, Write};

use crate::endian::{Endian, EndianReader};
use crate::json::write_type_value_json;
use crate::loader::AssetCollection;
use crate::monobehaviour::{
    MONO_BEHAVIOUR_CLASS_ID, MONO_SCRIPT_CLASS_ID, MonoBehaviourReadLimits, MonoScript,
    read_mono_behaviour, read_mono_script,
};
use crate::scene::resolve_object_reference;
use crate::serialized::{ObjectReference, TypeTree};
use crate::type_tree::{TypeValue, read_type_tree_from_reader};
use crate::{Error, Result};

#[derive(Debug, Clone, Copy)]
pub struct MonoBehaviourSchemaIdentity<'a> {
    pub unity_version: &'a str,
    pub assembly_name: &'a str,
    pub namespace: &'a str,
    pub class_name: &'a str,
}

/// Supplies complete object schemas reconstructed by a trusted external tool.
/// Implementations must not execute asset-controlled code during lookup.
pub trait MonoBehaviourSchemaProvider: Send + Sync {
    fn schema(&self, identity: MonoBehaviourSchemaIdentity<'_>) -> Result<Option<&TypeTree>>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonoBehaviourSchemaEntry {
    pub assembly_name: String,
    pub namespace: String,
    pub class_name: String,
    /// Exact Unity generator version. `None` is a fallback for every version.
    pub unity_version: Option<String>,
    pub tree: TypeTree,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MonoBehaviourSchemaRegistry {
    entries: Vec<MonoBehaviourSchemaEntry>,
}

impl MonoBehaviourSchemaRegistry {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    pub fn push(&mut self, entry: MonoBehaviourSchemaEntry) -> Result<()> {
        self.entries.try_reserve(1).map_err(|error| {
            Error::invalid_data(format!(
                "cannot grow MonoBehaviour schema registry: {error}"
            ))
        })?;
        self.entries.push(entry);
        Ok(())
    }

    #[must_use]
    pub fn entries(&self) -> &[MonoBehaviourSchemaEntry] {
        &self.entries
    }
}

impl MonoBehaviourSchemaProvider for MonoBehaviourSchemaRegistry {
    fn schema(&self, identity: MonoBehaviourSchemaIdentity<'_>) -> Result<Option<&TypeTree>> {
        let mut fallback = None;
        for entry in &self.entries {
            if !assembly_names_equal(&entry.assembly_name, identity.assembly_name)
                || entry.namespace != identity.namespace
                || entry.class_name != identity.class_name
            {
                continue;
            }
            match entry.unity_version.as_deref() {
                Some(version) if version == identity.unity_version => return Ok(Some(&entry.tree)),
                None if fallback.is_none() => fallback = Some(&entry.tree),
                _ => {}
            }
        }
        Ok(fallback)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MonoBehaviourSchemaSource {
    Embedded,
    External,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedMonoBehaviourValue {
    pub value: TypeValue,
    pub script: Option<MonoScript>,
    pub source: MonoBehaviourSchemaSource,
}

/// Reads an embedded tree when present, otherwise resolves `m_Script` and asks
/// the provider for a complete matching object tree.
pub fn read_mono_behaviour_value_with_provider(
    collection: &AssetCollection,
    file_index: usize,
    object_index: usize,
    provider: &dyn MonoBehaviourSchemaProvider,
    limits: MonoBehaviourReadLimits,
) -> Result<ResolvedMonoBehaviourValue> {
    let loaded = collection.serialized_files.get(file_index).ok_or_else(|| {
        Error::invalid_data(format!(
            "serialized file index {file_index} is out of range"
        ))
    })?;
    let object = loaded.file.objects.get(object_index).ok_or_else(|| {
        Error::invalid_data(format!(
            "serialized object index {object_index} is out of range"
        ))
    })?;
    if object.class_id != MONO_BEHAVIOUR_CLASS_ID {
        return Err(Error::invalid_data(format!(
            "object {} has class {}, expected MonoBehaviour ({MONO_BEHAVIOUR_CLASS_ID})",
            object.path_id, object.class_id
        )));
    }
    let embedded = object
        .serialized_type_index
        .and_then(|index| loaded.file.types.get(index))
        .and_then(|kind| kind.type_tree.as_ref())
        .filter(|tree| !tree.nodes.is_empty());
    if embedded.is_some() {
        return Ok(ResolvedMonoBehaviourValue {
            value: loaded
                .file
                .read_type_tree_value_with_limits(object_index, limits.type_tree)?,
            script: None,
            source: MonoBehaviourSchemaSource::Embedded,
        });
    }

    let behaviour = read_mono_behaviour(&loaded.file, object_index, limits)?;
    if behaviour.script.is_null() {
        return Err(Error::unsupported(format!(
            "MonoBehaviour {} has no embedded TypeTree and a null MonoScript reference",
            object.path_id
        )));
    }
    let reference = ObjectReference {
        file_id: behaviour.script.file_id,
        path_id: behaviour.script.path_id,
    };
    let target = resolve_object_reference(collection, file_index, reference)?.ok_or_else(|| {
        Error::unsupported(format!(
            "MonoBehaviour {} cannot resolve its MonoScript",
            object.path_id
        ))
    })?;
    if target.object.class_id != MONO_SCRIPT_CLASS_ID {
        return Err(Error::invalid_data(format!(
            "MonoBehaviour {} m_Script resolves to class {}, not MonoScript ({MONO_SCRIPT_CLASS_ID})",
            object.path_id, target.object.class_id
        )));
    }
    let script = read_mono_script(target.file, target.object_index, limits)?;
    let identity = MonoBehaviourSchemaIdentity {
        unity_version: &loaded.file.unity_version_string,
        assembly_name: &script.assembly_name,
        namespace: &script.namespace,
        class_name: &script.class_name,
    };
    let tree = provider.schema(identity)?.ok_or_else(|| {
        Error::unsupported(format!(
            "no external MonoBehaviour schema matches {}::{} in {} for Unity {}",
            script.namespace,
            script.class_name,
            script.assembly_name,
            loaded.file.unity_version_string
        ))
    })?;
    let payload = loaded.file.object_region(object_index)?;
    let endian = if loaded.file.header.endianness == 0 {
        Endian::Little
    } else {
        Endian::Big
    };
    let value = read_type_tree_from_reader(
        tree,
        EndianReader::new(payload.cursor(), endian),
        object.byte_start,
        limits.type_tree,
    )?;
    Ok(ResolvedMonoBehaviourValue {
        value,
        script: Some(script),
        source: MonoBehaviourSchemaSource::External,
    })
}

pub fn read_mono_behaviour_json_with_provider(
    collection: &AssetCollection,
    file_index: usize,
    object_index: usize,
    provider: &dyn MonoBehaviourSchemaProvider,
    pretty: bool,
    limits: MonoBehaviourReadLimits,
) -> Result<String> {
    let resolved = read_mono_behaviour_value_with_provider(
        collection,
        file_index,
        object_index,
        provider,
        limits,
    )?;
    let mut output = BoundedJsonOutput::new(limits.maximum_json_bytes);
    let write_result = write_type_value_json(&resolved.value, &mut output, pretty);
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

/// Compares assembly names the way the two sides actually spell them.
///
/// A `MonoScript` names its assembly without a suffix -- `Fwk`, `App.Runtime`
/// -- while a generator walking a directory names the file, `Fwk.dll`. Both
/// spellings mean the same assembly, so the suffix is trimmed from either side
/// before comparing; without this every schema misses and the whole document
/// silently does nothing.
fn assembly_names_equal(left: &str, right: &str) -> bool {
    trim_assembly_extension(portable_file_name(left))
        .eq_ignore_ascii_case(trim_assembly_extension(portable_file_name(right)))
}

fn trim_assembly_extension(value: &str) -> &str {
    let Some(cut) = value.len().checked_sub(4) else {
        return value;
    };
    // `get` rather than indexing: a name is caller data, and slicing four
    // bytes back can land inside a multi-byte character.
    match value.get(cut..) {
        Some(suffix) if suffix.eq_ignore_ascii_case(".dll") => &value[..cut],
        _ => value,
    }
}


fn portable_file_name(value: &str) -> &str {
    value.rsplit(['/', '\\']).next().unwrap_or(value)
}

struct BoundedJsonOutput {
    bytes: Vec<u8>,
    maximum: usize,
    limit_exceeded: bool,
}

impl BoundedJsonOutput {
    const fn new(maximum: usize) -> Self {
        Self {
            bytes: Vec::new(),
            maximum,
            limit_exceeded: false,
        }
    }
}

impl Write for BoundedJsonOutput {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let Some(end) = self.bytes.len().checked_add(buffer.len()) else {
            self.limit_exceeded = true;
            return Err(io::Error::other("MonoBehaviour JSON size overflowed"));
        };
        if end > self.maximum {
            self.limit_exceeded = true;
            return Err(io::Error::other("MonoBehaviour JSON limit exceeded"));
        }
        self.bytes.try_reserve(buffer.len()).map_err(|error| {
            io::Error::other(format!("cannot allocate MonoBehaviour JSON: {error}"))
        })?;
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::loader::{AssetCollection, LoadedSerializedFile};
    use crate::serialized::{SerializedFile, TypeTree, TypeTreeNode};
    use crate::source::Region;

    use super::{
        MonoBehaviourSchemaEntry, MonoBehaviourSchemaRegistry, MonoBehaviourSchemaSource,
        read_mono_behaviour_json_with_provider, read_mono_behaviour_value_with_provider,
    };
    use crate::monobehaviour::{
        MONO_BEHAVIOUR_CLASS_ID, MONO_SCRIPT_CLASS_ID, MonoBehaviourReadLimits,
    };

    #[test]
    fn external_full_tree_resolves_monoscript_and_reads_bounded_json() {
        let collection = fixture();
        let mut registry = MonoBehaviourSchemaRegistry::new();
        registry
            .push(MonoBehaviourSchemaEntry {
                assembly_name: "folder/assembly-csharp.DLL".to_owned(),
                namespace: "Game".to_owned(),
                class_name: "Stats".to_owned(),
                unity_version: None,
                tree: full_tree(),
            })
            .unwrap();

        let resolved = read_mono_behaviour_value_with_provider(
            &collection,
            0,
            0,
            &registry,
            MonoBehaviourReadLimits::default(),
        )
        .unwrap();
        assert_eq!(resolved.source, MonoBehaviourSchemaSource::External);
        assert_eq!(resolved.script.unwrap().class_name, "Stats");
        let json = read_mono_behaviour_json_with_provider(
            &collection,
            0,
            0,
            &registry,
            false,
            MonoBehaviourReadLimits::default(),
        )
        .unwrap();
        let json: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(json["m_Name"], "Hero");
        assert_eq!(json["score"], 123);
    }

    #[test]
    fn exact_version_wins_and_missing_or_tiny_output_is_rejected() {
        let collection = fixture();
        let mut missing = MonoBehaviourSchemaRegistry::new();
        missing
            .push(MonoBehaviourSchemaEntry {
                assembly_name: "Other.dll".to_owned(),
                namespace: "Game".to_owned(),
                class_name: "Stats".to_owned(),
                unity_version: None,
                tree: full_tree(),
            })
            .unwrap();
        assert!(
            read_mono_behaviour_value_with_provider(
                &collection,
                0,
                0,
                &missing,
                MonoBehaviourReadLimits::default(),
            )
            .unwrap_err()
            .to_string()
            .contains("no external")
        );

        let mut exact = MonoBehaviourSchemaRegistry::new();
        exact
            .push(MonoBehaviourSchemaEntry {
                assembly_name: "Assembly-CSharp.dll".to_owned(),
                namespace: "Game".to_owned(),
                class_name: "Stats".to_owned(),
                unity_version: Some("2022.3.62f1".to_owned()),
                tree: full_tree(),
            })
            .unwrap();
        let limits = MonoBehaviourReadLimits {
            maximum_json_bytes: 1,
            ..MonoBehaviourReadLimits::default()
        };
        assert!(
            read_mono_behaviour_json_with_provider(&collection, 0, 0, &exact, false, limits)
                .is_err()
        );
    }

    fn fixture() -> AssetCollection {
        let behaviour = mono_behaviour();
        let script = mono_script();
        let file = SerializedFile::open(Region::from_bytes(synthetic_v22(&[
            (MONO_BEHAVIOUR_CLASS_ID, 7, behaviour),
            (MONO_SCRIPT_CLASS_ID, 8, script),
        ])))
        .unwrap();
        AssetCollection::from_loaded_parts(
            vec![LoadedSerializedFile {
                path: "schema.assets".to_owned(),
                file,
            }],
            Vec::new(),
        )
    }

    fn mono_behaviour() -> Vec<u8> {
        let mut output = Vec::new();
        push_pptr(&mut output, 0, 1);
        output.push(1);
        align(&mut output, 4);
        push_pptr(&mut output, 0, 8);
        push_string(&mut output, "Hero");
        push_i32(&mut output, 123);
        output
    }

    fn mono_script() -> Vec<u8> {
        let mut output = Vec::new();
        push_string(&mut output, "Stats script");
        push_i32(&mut output, 0);
        output.extend_from_slice(&[0; 16]);
        push_string(&mut output, "Stats");
        push_string(&mut output, "Game");
        push_string(&mut output, "Assembly-CSharp.dll");
        output
    }

    fn full_tree() -> TypeTree {
        TypeTree {
            nodes: vec![
                node("MonoBehaviour", "Base", 0, 0),
                node("PPtr<GameObject>", "m_GameObject", 1, 0),
                node("int", "m_FileID", 2, 0),
                node("SInt64", "m_PathID", 2, 0),
                node("UInt8", "m_Enabled", 1, 0x4000),
                node("PPtr<MonoScript>", "m_Script", 1, 0),
                node("int", "m_FileID", 2, 0),
                node("SInt64", "m_PathID", 2, 0),
                node("string", "m_Name", 1, 0),
                node("Array", "Array", 2, 0),
                node("int", "size", 3, 0),
                node("char", "data", 3, 0),
                node("SInt32", "score", 1, 0),
            ],
            string_buffer: Vec::new(),
        }
    }

    fn node(type_name: &str, field_name: &str, level: u32, meta_flags: i32) -> TypeTreeNode {
        TypeTreeNode {
            type_name: type_name.to_owned(),
            field_name: field_name.to_owned(),
            byte_size: -1,
            index: 0,
            type_flags: 0,
            version: 1,
            meta_flags,
            level,
            type_string_offset: None,
            name_string_offset: None,
            reference_type_hash: 0,
        }
    }

    fn synthetic_v22(objects: &[(i32, i64, Vec<u8>)]) -> Vec<u8> {
        let mut classes = objects.iter().map(|value| value.0).collect::<Vec<_>>();
        classes.sort_unstable();
        classes.dedup();
        let mut metadata = Vec::new();
        metadata.extend_from_slice(b"2022.3.62f1\0");
        push_i32(&mut metadata, 13);
        metadata.push(0);
        push_i32(&mut metadata, i32::try_from(classes.len()).unwrap());
        for class_id in &classes {
            push_i32(&mut metadata, *class_id);
            metadata.push(0);
            metadata.extend_from_slice(&(-1_i16).to_le_bytes());
            if *class_id == MONO_BEHAVIOUR_CLASS_ID {
                metadata.extend_from_slice(&[0; 16]);
            }
            metadata.extend_from_slice(&[0; 16]);
        }
        let mut data = Vec::new();
        let mut records = Vec::new();
        for (class_id, path_id, payload) in objects {
            align(&mut data, 4);
            records.push((
                *path_id,
                i64::try_from(data.len()).unwrap(),
                u32::try_from(payload.len()).unwrap(),
                i32::try_from(classes.iter().position(|value| value == class_id).unwrap()).unwrap(),
            ));
            data.extend_from_slice(payload);
        }
        push_i32(&mut metadata, i32::try_from(records.len()).unwrap());
        for (path_id, offset, size, type_index) in records {
            align_with_base(&mut metadata, 48, 4);
            metadata.extend_from_slice(&path_id.to_le_bytes());
            metadata.extend_from_slice(&offset.to_le_bytes());
            metadata.extend_from_slice(&size.to_le_bytes());
            push_i32(&mut metadata, type_index);
        }
        for _ in 0..3 {
            push_i32(&mut metadata, 0);
        }
        metadata.push(0);
        let data_offset = (48 + metadata.len()).next_multiple_of(16);
        let file_size = data_offset + data.len();
        let mut output = vec![0; 48];
        output[8..12].copy_from_slice(&22_u32.to_be_bytes());
        output[20..24].copy_from_slice(&u32::try_from(metadata.len()).unwrap().to_be_bytes());
        output[24..32].copy_from_slice(&i64::try_from(file_size).unwrap().to_be_bytes());
        output[32..40].copy_from_slice(&i64::try_from(data_offset).unwrap().to_be_bytes());
        output.extend_from_slice(&metadata);
        output.resize(data_offset, 0);
        output.extend_from_slice(&data);
        output
    }

    fn push_pptr(output: &mut Vec<u8>, file_id: i32, path_id: i64) {
        push_i32(output, file_id);
        output.extend_from_slice(&path_id.to_le_bytes());
    }

    fn push_string(output: &mut Vec<u8>, value: &str) {
        push_i32(output, i32::try_from(value.len()).unwrap());
        output.extend_from_slice(value.as_bytes());
        align(output, 4);
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
