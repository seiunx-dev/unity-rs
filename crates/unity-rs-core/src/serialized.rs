use std::collections::HashMap;
use std::io::{Read, Seek};
use std::str::FromStr;

use crate::endian::{Endian, EndianReader, checked_length};
use crate::serialized_file::{SerializedFileFormatVersion, SerializedFileHeader};
use crate::source::{Region, RegionCursor};
use crate::unity_version::UnityVersion;
use crate::{Error, Result};

const UNKNOWN_PLATFORM: i32 = 9_999;
const NO_TARGET_PLATFORM: i32 = -2;
const TEXT_ASSET_CLASS_ID: i32 = 49;
pub const ASSET_BUNDLE_CLASS_ID: i32 = 142;
pub const RESOURCE_MANAGER_CLASS_ID: i32 = 147;
pub const PRELOAD_DATA_CLASS_ID: i32 = 150;

/// Defensive limits for the object payload metadata that supplies `AssetStudio`'s container index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContainerMetadataReadLimits {
    pub maximum_preload_references: usize,
    pub maximum_container_entries: usize,
    pub maximum_dependencies: usize,
    pub maximum_class_version_entries: usize,
    pub maximum_string_bytes: usize,
    pub maximum_total_string_bytes: usize,
}

impl Default for ContainerMetadataReadLimits {
    fn default() -> Self {
        Self {
            maximum_preload_references: 1_000_000,
            maximum_container_entries: 1_000_000,
            maximum_dependencies: 1_000_000,
            maximum_class_version_entries: 1_000_000,
            maximum_string_bytes: 16 * 1024 * 1024,
            maximum_total_string_bytes: 64 * 1024 * 1024,
        }
    }
}

/// Defensive limits for untrusted serialized-file metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SerializedParseLimits {
    pub maximum_metadata_bytes: u64,
    pub maximum_types: usize,
    pub maximum_objects: usize,
    pub maximum_script_types: usize,
    pub maximum_externals: usize,
    pub maximum_reference_types: usize,
    pub maximum_type_tree_nodes: usize,
    pub maximum_total_type_tree_nodes: usize,
    pub maximum_type_tree_depth: usize,
    pub maximum_string_buffer_bytes: usize,
    pub maximum_total_string_buffer_bytes: usize,
    pub maximum_string_bytes: usize,
    pub maximum_materialized_metadata_string_bytes: usize,
    pub maximum_resolved_type_tree_string_bytes: usize,
    pub maximum_type_dependencies: usize,
}

#[derive(Debug, Clone, Default)]
pub struct SerializedOpenOptions {
    pub limits: SerializedParseLimits,
    /// A caller-supplied version, equivalent to the managed reader's
    /// `CustomUnityVersion`. It outranks everything the file itself declares.
    pub unity_version_override: Option<UnityVersion>,
    /// The revision recorded by the enclosing bundle, if this file came from
    /// one. Unlike the override, this is only consulted when the file cannot
    /// speak for itself: see [`SerializedFile::open_with_options`].
    pub bundle_version_hint: Option<UnityVersion>,
    /// Reject classes whose standard-Unity version is above the verified
    /// ceiling instead of attempting the newest known layout. The default is
    /// lenient; see `version_gate` for the exact error contract.
    pub strict_unity_versions: bool,
}

impl Default for SerializedParseLimits {
    fn default() -> Self {
        Self {
            maximum_metadata_bytes: 512 * 1024 * 1024,
            maximum_types: 1_000_000,
            maximum_objects: 10_000_000,
            maximum_script_types: 1_000_000,
            maximum_externals: 1_000_000,
            maximum_reference_types: 1_000_000,
            maximum_type_tree_nodes: 2_000_000,
            maximum_total_type_tree_nodes: 2_000_000,
            maximum_type_tree_depth: 256,
            maximum_string_buffer_bytes: 256 * 1024 * 1024,
            maximum_total_string_buffer_bytes: 256 * 1024 * 1024,
            maximum_string_bytes: 16 * 1024 * 1024,
            maximum_materialized_metadata_string_bytes: 64 * 1024 * 1024,
            maximum_resolved_type_tree_string_bytes: 64 * 1024 * 1024,
            maximum_type_dependencies: 2_000_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SerializedType {
    pub class_id: i32,
    pub is_stripped_type: bool,
    pub script_type_index: i16,
    pub script_id: Option<[u8; 16]>,
    pub old_type_hash: Option<[u8; 16]>,
    pub type_tree: Option<TypeTree>,
    pub type_dependencies: Vec<i32>,
    pub class_name: Option<String>,
    pub namespace: Option<String>,
    pub assembly_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeTree {
    pub nodes: Vec<TypeTreeNode>,
    pub string_buffer: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeTreeNode {
    pub type_name: String,
    pub field_name: String,
    pub byte_size: i32,
    pub index: i32,
    pub type_flags: i32,
    pub version: i32,
    pub meta_flags: i32,
    pub level: u32,
    pub type_string_offset: Option<u32>,
    pub name_string_offset: Option<u32>,
    pub reference_type_hash: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectInfo {
    pub path_id: i64,
    pub byte_start: u64,
    pub byte_size: u64,
    pub type_id: i32,
    pub class_id: i32,
    pub serialized_type_index: Option<usize>,
    pub destroyed: u16,
    pub stripped: u8,
    pub script_type_index: Option<i16>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptIdentifier {
    pub local_serialized_file_index: i32,
    pub local_identifier_in_file: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalFile {
    pub prefix: Option<String>,
    pub guid: Option<[u8; 16]>,
    pub kind: Option<i32>,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextAsset {
    pub path_id: i64,
    pub name: String,
    pub script: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetBundleContainerEntry {
    pub key: String,
    pub preload_index: usize,
    pub preload_size: usize,
    pub asset: ObjectReference,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetBundleMetadata {
    /// Effective bundle name used by the loader: `asset_bundle_name` when it
    /// is present and non-empty, otherwise the inherited `NamedObject` name.
    pub name: String,
    pub object_name: String,
    pub asset_bundle_name: Option<String>,
    pub preload_table: Vec<ObjectReference>,
    pub container: Vec<AssetBundleContainerEntry>,
    pub dependencies: Vec<String>,
    pub is_streamed_scene_asset_bundle: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceManagerContainerEntry {
    pub key: String,
    pub asset: ObjectReference,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceManagerMetadata {
    pub container: Vec<ResourceManagerContainerEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreloadDataMetadata {
    pub name: String,
    pub assets: Vec<ObjectReference>,
}

/// A parsed Unity serialized file whose lazy object payloads remain bound to
/// the exact source region from which the metadata was read.
#[derive(Debug, Clone)]
pub struct SerializedFile {
    region: Region,
    pub header: SerializedFileHeader,
    pub unity_version_string: String,
    pub unity_version: UnityVersion,
    pub target_platform: i32,
    pub type_tree_enabled: bool,
    pub types: Vec<SerializedType>,
    pub big_id_enabled: i32,
    pub objects: Vec<ObjectInfo>,
    pub script_types: Vec<ScriptIdentifier>,
    pub externals: Vec<ExternalFile>,
    pub reference_types: Vec<SerializedType>,
    pub user_information: String,
    /// Carried from [`SerializedOpenOptions::strict_unity_versions`] so every
    /// per-class version gate sees the caller's choice.
    pub strict_unity_versions: bool,
}

#[derive(Debug, Default)]
struct SerializedParseBudget {
    type_tree_nodes: usize,
    type_tree_string_buffer_bytes: usize,
    resolved_type_tree_string_bytes: usize,
    materialized_metadata_string_bytes: usize,
}

impl SerializedFile {
    pub fn open(region: Region) -> Result<Self> {
        Self::open_with_options(region, SerializedOpenOptions::default())
    }

    pub fn open_with_limits(region: Region, limits: SerializedParseLimits) -> Result<Self> {
        Self::open_with_options(
            region,
            SerializedOpenOptions {
                limits,
                ..SerializedOpenOptions::default()
            },
        )
    }

    /// Opens one serialized file.
    ///
    /// The effective Unity version follows the managed reader's precedence: an
    /// explicit [`SerializedOpenOptions::unity_version_override`] always wins,
    /// then a [`SerializedOpenOptions::bundle_version_hint`] but only where the
    /// managed reader would apply one, and otherwise the version the file
    /// declares. `AssetsManager.LoadAssetsFromMemory` overrides with the bundle
    /// revision only for format versions below 7, which carry no version string
    /// of their own.
    ///
    /// One deliberate deviation: where the file's own version is stripped and no
    /// override was supplied, the managed reader raises `NotSupportedException`
    /// and refuses to load. This reader falls back to the bundle revision, which
    /// is the value the managed error message itself suggests, and only reports
    /// a missing version when no source has one.
    // Metadata is deliberately kept as one linear transaction: every version
    // gate changes the byte position consumed by all fields that follow.
    #[allow(clippy::too_many_lines)]
    pub fn open_with_options(region: Region, options: SerializedOpenOptions) -> Result<Self> {
        let SerializedOpenOptions {
            limits,
            unity_version_override,
            bundle_version_hint,
            strict_unity_versions,
        } = options;
        let mut root_reader = EndianReader::new(region.cursor(), Endian::Big);
        let header = SerializedFileHeader::read(&mut root_reader)?;
        if header.version.0 < 5 {
            return Err(Error::unsupported(format!(
                "serialized format version {} metadata",
                header.version
            )));
        }
        if header.version > SerializedFileFormatVersion::LARGE_FILES_SUPPORT {
            return Err(Error::unsupported(format!(
                "serialized format version {} metadata; newest implemented version is {}",
                header.version,
                SerializedFileFormatVersion::LARGE_FILES_SUPPORT
            )));
        }

        let declared_metadata = header.metadata_range()?;
        let metadata_start = root_reader.position()?;
        let metadata_end = declared_metadata.end;
        let metadata_length = metadata_end
            .checked_sub(metadata_start)
            .ok_or_else(|| Error::invalid_data("serialized metadata ends before it starts"))?;
        if metadata_length > limits.maximum_metadata_bytes {
            return Err(Error::invalid_data(format!(
                "serialized metadata is {metadata_length} bytes, exceeding limit {}",
                limits.maximum_metadata_bytes
            )));
        }
        let metadata = region.subregion(metadata_start, metadata_length)?;
        let endian = if header.endianness == 0 {
            Endian::Little
        } else {
            Endian::Big
        };
        let mut reader = EndianReader::new(metadata.cursor(), endian);
        let mut budget = SerializedParseBudget::default();

        let unity_version_string = if header.version.0 >= 7 {
            read_metadata_string(
                &mut reader,
                "serialized Unity version",
                &limits,
                &mut budget,
            )?
        } else {
            "2.5.0f1".to_owned()
        };
        let detected_unity_version =
            if unity_version_string.is_empty() || unity_version_string == "0.0.0" {
                None
            } else {
                UnityVersion::from_str(&unity_version_string).ok()
            };
        let unity_version = unity_version_override
            .or_else(|| {
                if header.version.0 < 7 || detected_unity_version.is_none() {
                    bundle_version_hint
                } else {
                    None
                }
            })
            .or(detected_unity_version)
            .unwrap_or_default();
        let target_platform = if header.version.0 >= 8 {
            reader.read_i32()?
        } else {
            UNKNOWN_PLATFORM
        };
        let type_tree_enabled =
            if header.version >= SerializedFileFormatVersion::HAS_TYPE_TREE_HASHES {
                reader.read_bool()?
            } else {
                true
            };

        let type_count = read_record_count(
            &mut reader,
            limits.maximum_types,
            "serialized type",
            minimum_serialized_type_size(header.version, type_tree_enabled, false),
        )?;
        let mut types = reserve_vec(type_count, "serialized types")?;
        for _ in 0..type_count {
            types.push(read_serialized_type(
                &mut reader,
                header.version,
                type_tree_enabled,
                false,
                &limits,
                &mut budget,
            )?);
        }
        let mut type_index_by_class_id = HashMap::new();
        type_index_by_class_id
            .try_reserve(types.len())
            .map_err(|error| Error::invalid_data(format!("cannot allocate type index: {error}")))?;
        for (index, kind) in types.iter().enumerate() {
            type_index_by_class_id.entry(kind.class_id).or_insert(index);
        }

        let big_id_enabled = if (7..14).contains(&header.version.0) {
            reader.read_i32()?
        } else {
            0
        };

        let object_count = read_record_count(
            &mut reader,
            limits.maximum_objects,
            "serialized object",
            minimum_object_info_size(header.version, big_id_enabled),
        )?;
        let mut objects = reserve_vec(object_count, "serialized objects")?;
        for _ in 0..object_count {
            let path_id = if big_id_enabled != 0 {
                reader.read_i64()?
            } else if header.version.0 < 14 {
                i64::from(reader.read_i32()?)
            } else {
                align_absolute(&mut reader, metadata_start, 4)?;
                reader.read_i64()?
            };

            let relative_start =
                if header.version >= SerializedFileFormatVersion::LARGE_FILES_SUPPORT {
                    positive_i64(reader.read_i64()?, "serialized object byte start")?
                } else {
                    u64::from(reader.read_u32()?)
                };
            let byte_start = header
                .data_offset
                .checked_add(relative_start)
                .ok_or_else(|| Error::invalid_data("serialized object byte start overflowed"))?;
            let byte_size = u64::from(reader.read_u32()?);
            validate_object_range(
                byte_start,
                byte_size,
                header.data_offset,
                header.object_data_end()?,
            )?;

            let type_id = reader.read_i32()?;
            let (class_id, serialized_type_index) = if header.version.0 < 16 {
                let class_id = i32::from(reader.read_u16()?);
                let index = type_index_by_class_id.get(&type_id).copied();
                (class_id, index)
            } else {
                let index = usize::try_from(type_id).map_err(|_| {
                    Error::invalid_data(format!(
                        "serialized object type index cannot be negative: {type_id}"
                    ))
                })?;
                let kind = types.get(index).ok_or_else(|| {
                    Error::invalid_data(format!(
                        "serialized object type index {index} exceeds {} types",
                        types.len()
                    ))
                })?;
                (kind.class_id, Some(index))
            };

            let destroyed = if header.version < SerializedFileFormatVersion::HAS_SCRIPT_TYPE_INDEX {
                reader.read_u16()?
            } else {
                0
            };
            let script_type_index = if header.version
                >= SerializedFileFormatVersion::HAS_SCRIPT_TYPE_INDEX
                && header.version.0 < 17
            {
                let value = reader.read_i16()?;
                Some(value)
            } else {
                None
            };
            let stripped = if matches!(header.version.0, 15 | 16) {
                reader.read_u8()?
            } else {
                0
            };

            objects.push(ObjectInfo {
                path_id,
                byte_start,
                byte_size,
                type_id,
                class_id,
                serialized_type_index,
                destroyed,
                stripped,
                script_type_index,
            });
        }
        reject_duplicate_path_ids(&objects)?;

        let script_types = if header.version >= SerializedFileFormatVersion::HAS_SCRIPT_TYPE_INDEX {
            let count = read_record_count(
                &mut reader,
                limits.maximum_script_types,
                "serialized script type",
                if header.version.0 < 14 { 8 } else { 12 },
            )?;
            let mut values = reserve_vec(count, "serialized script types")?;
            for _ in 0..count {
                let local_serialized_file_index = reader.read_i32()?;
                let local_identifier_in_file = if header.version.0 < 14 {
                    i64::from(reader.read_i32()?)
                } else {
                    align_absolute(&mut reader, metadata_start, 4)?;
                    reader.read_i64()?
                };
                values.push(ScriptIdentifier {
                    local_serialized_file_index,
                    local_identifier_in_file,
                });
            }
            values
        } else {
            Vec::new()
        };

        let external_count = read_record_count(
            &mut reader,
            limits.maximum_externals,
            "serialized external",
            if header.version.0 >= 6 { 22 } else { 21 },
        )?;
        let mut externals = reserve_vec(external_count, "serialized externals")?;
        for _ in 0..external_count {
            let prefix = if header.version.0 >= 6 {
                Some(read_metadata_string(
                    &mut reader,
                    "serialized external prefix",
                    &limits,
                    &mut budget,
                )?)
            } else {
                None
            };
            let (guid, kind) = if header.version.0 >= 5 {
                (Some(read_hash(&mut reader)?), Some(reader.read_i32()?))
            } else {
                (None, None)
            };
            let path = read_metadata_string(
                &mut reader,
                "serialized external path",
                &limits,
                &mut budget,
            )?;
            externals.push(ExternalFile {
                prefix,
                guid,
                kind,
                path,
            });
        }

        let reference_types = if header.version >= SerializedFileFormatVersion::SUPPORTS_REF_OBJECT
        {
            let count = read_record_count(
                &mut reader,
                limits.maximum_reference_types,
                "serialized reference type",
                minimum_serialized_type_size(header.version, type_tree_enabled, true),
            )?;
            let mut values = reserve_vec(count, "serialized reference types")?;
            for _ in 0..count {
                values.push(read_serialized_type(
                    &mut reader,
                    header.version,
                    type_tree_enabled,
                    true,
                    &limits,
                    &mut budget,
                )?);
            }
            values
        } else {
            Vec::new()
        };

        let user_information = if header.version.0 >= 5 {
            read_metadata_string(
                &mut reader,
                "serialized user information",
                &limits,
                &mut budget,
            )?
        } else {
            String::new()
        };
        consume_metadata_tail(&mut reader)?;

        Ok(Self {
            region,
            header,
            unity_version_string,
            unity_version,
            target_platform,
            type_tree_enabled,
            types,
            big_id_enabled,
            objects,
            script_types,
            externals,
            reference_types,
            user_information,
            strict_unity_versions,
        })
    }

    #[must_use]
    pub const fn region(&self) -> &Region {
        &self.region
    }

    pub fn object_region(&self, index: usize) -> Result<Region> {
        let object = self.objects.get(index).ok_or_else(|| {
            Error::invalid_data(format!("serialized object index {index} is out of range"))
        })?;
        self.region.subregion(object.byte_start, object.byte_size)
    }

    /// Returns the non-empty embedded `TypeTree` selected by one object's
    /// serialized type metadata.
    pub fn object_type_tree(&self, index: usize) -> Result<&TypeTree> {
        let object = self.objects.get(index).ok_or_else(|| {
            Error::invalid_data(format!("serialized object index {index} is out of range"))
        })?;
        let type_index = object.serialized_type_index.ok_or_else(|| {
            Error::unsupported(format!(
                "serialized object {} has no matching type metadata",
                object.path_id
            ))
        })?;
        self.types
            .get(type_index)
            .and_then(|kind| kind.type_tree.as_ref())
            .filter(|tree| !tree.nodes.is_empty())
            .ok_or_else(|| {
                Error::unsupported(format!(
                    "serialized object {} has no embedded TypeTree",
                    object.path_id
                ))
            })
    }

    pub fn object_region_by_path_id(&self, path_id: i64) -> Result<Region> {
        let index = self
            .objects
            .iter()
            .position(|object| object.path_id == path_id)
            .ok_or_else(|| {
                Error::invalid_data(format!("serialized object path ID {path_id} was not found"))
            })?;
        self.object_region(index)
    }

    pub fn read_object_bytes(&self, index: usize, maximum_length: u64) -> Result<Vec<u8>> {
        self.object_region(index)?.read_to_vec(maximum_length)
    }

    pub fn read_text_asset(&self, index: usize, maximum_script_bytes: usize) -> Result<TextAsset> {
        let object = self.objects.get(index).ok_or_else(|| {
            Error::invalid_data(format!("serialized object index {index} is out of range"))
        })?;
        if object.class_id != TEXT_ASSET_CLASS_ID {
            return Err(Error::unsupported(format!(
                "object {} has class ID {}, not TextAsset ({TEXT_ASSET_CLASS_ID})",
                object.path_id, object.class_id
            )));
        }

        let payload = self.object_region(index)?;
        let endian = if self.header.endianness == 0 {
            Endian::Little
        } else {
            Endian::Big
        };
        let mut reader = EndianReader::new(payload.cursor(), endian);
        if self.target_platform == NO_TARGET_PLATFORM {
            let _object_hide_flags = reader.read_u32()?;
            for _ in 0..2 {
                let _file_id = reader.read_i32()?;
                if self.header.version.0 < 14 {
                    let _path_id = reader.read_i32()?;
                } else {
                    let _path_id = reader.read_i64()?;
                }
            }
        }

        let name = read_aligned_string_limited(
            &mut reader,
            object.byte_start,
            maximum_script_bytes,
            "TextAsset name",
        )?;
        let script_length = checked_length(reader.read_i32()?, "TextAsset script")?;
        if script_length > maximum_script_bytes {
            return Err(Error::invalid_data(format!(
                "TextAsset script is {script_length} bytes, exceeding limit {maximum_script_bytes}"
            )));
        }
        let script = reader.read_bytes(script_length)?;

        Ok(TextAsset {
            path_id: object.path_id,
            name,
            script,
        })
    }

    pub fn read_asset_bundle_metadata(
        &self,
        index: usize,
        limits: ContainerMetadataReadLimits,
    ) -> Result<AssetBundleMetadata> {
        let mut reader = ContainerObjectReader::new(self, index, ASSET_BUNDLE_CLASS_ID, limits)?;
        reader.skip_editor_extension()?;
        let object_name = reader.read_string("AssetBundle m_Name")?;
        let preload_count = reader.read_count(
            "AssetBundle preload table",
            limits.maximum_preload_references,
            reader.pptr_size(),
        )?;
        let mut preload_table = reserve_vec(preload_count, "AssetBundle preload table")?;
        for _ in 0..preload_count {
            preload_table.push(reader.read_pptr()?);
        }

        let container_count =
            reader.read_count("AssetBundle container", limits.maximum_container_entries, 4)?;
        let mut container = reserve_vec(container_count, "AssetBundle container")?;
        for _ in 0..container_count {
            let key = reader.read_string("AssetBundle container key")?;
            let preload_index = checked_length(
                reader.reader.read_i32()?,
                "AssetBundle container preload index",
            )?;
            let preload_size = checked_length(
                reader.reader.read_i32()?,
                "AssetBundle container preload size",
            )?;
            let asset = reader.read_pptr()?;
            container.push(AssetBundleContainerEntry {
                key,
                preload_index,
                preload_size,
                asset,
            });
        }

        let _main_asset_preload_index = reader.reader.read_i32()?;
        let _main_asset_preload_size = reader.reader.read_i32()?;
        let _main_asset = reader.read_pptr()?;
        if self.unity_version.components().0 == 5 && self.unity_version.components().1 == 4 {
            let count = reader.read_count(
                "AssetBundle class version map",
                limits.maximum_class_version_entries,
                8,
            )?;
            for _ in 0..count {
                let _class_id = reader.reader.read_i32()?;
                let _class_version = reader.reader.read_i32()?;
            }
        }
        if self.unity_version.components() >= (4, 2, 0) {
            let _runtime_compatibility = reader.reader.read_u32()?;
        }

        let mut asset_bundle_name = None;
        let mut dependencies = Vec::new();
        let mut is_streamed_scene_asset_bundle = false;
        if self.unity_version.components().0 >= 5 {
            asset_bundle_name = Some(reader.read_string("AssetBundle name")?);
            let dependency_count =
                reader.read_count("AssetBundle dependencies", limits.maximum_dependencies, 4)?;
            dependencies = reserve_vec(dependency_count, "AssetBundle dependencies")?;
            for _ in 0..dependency_count {
                dependencies.push(reader.read_string("AssetBundle dependency")?);
            }
            is_streamed_scene_asset_bundle = reader.reader.read_bool()?;
        }
        let name = match asset_bundle_name
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            Some(value) => reader.copy_string(value, "AssetBundle effective name")?,
            None => reader.copy_string(&object_name, "AssetBundle effective name")?,
        };

        Ok(AssetBundleMetadata {
            name,
            object_name,
            asset_bundle_name,
            preload_table,
            container,
            dependencies,
            is_streamed_scene_asset_bundle,
        })
    }

    pub fn read_resource_manager_metadata(
        &self,
        index: usize,
        limits: ContainerMetadataReadLimits,
    ) -> Result<ResourceManagerMetadata> {
        let mut reader =
            ContainerObjectReader::new(self, index, RESOURCE_MANAGER_CLASS_ID, limits)?;
        let container_count = reader.read_count(
            "ResourceManager container",
            limits.maximum_container_entries,
            4,
        )?;
        let mut container = reserve_vec(container_count, "ResourceManager container")?;
        for _ in 0..container_count {
            container.push(ResourceManagerContainerEntry {
                key: reader.read_string("ResourceManager container key")?,
                asset: reader.read_pptr()?,
            });
        }
        Ok(ResourceManagerMetadata { container })
    }

    pub fn read_preload_data_metadata(
        &self,
        index: usize,
        limits: ContainerMetadataReadLimits,
    ) -> Result<PreloadDataMetadata> {
        let mut reader = ContainerObjectReader::new(self, index, PRELOAD_DATA_CLASS_ID, limits)?;
        reader.skip_editor_extension()?;
        let name = reader.read_string("PreloadData m_Name")?;
        let count = reader.read_count(
            "PreloadData assets",
            limits.maximum_preload_references,
            reader.pptr_size(),
        )?;
        let mut assets = reserve_vec(count, "PreloadData assets")?;
        for _ in 0..count {
            assets.push(reader.read_pptr()?);
        }
        Ok(PreloadDataMetadata { name, assets })
    }
}

struct ContainerObjectReader {
    reader: EndianReader<RegionCursor>,
    format_version: u32,
    no_target: bool,
    limits: ContainerMetadataReadLimits,
    total_string_bytes: usize,
}

impl ContainerObjectReader {
    fn new(
        file: &SerializedFile,
        index: usize,
        expected_class_id: i32,
        limits: ContainerMetadataReadLimits,
    ) -> Result<Self> {
        let object = file.objects.get(index).ok_or_else(|| {
            Error::invalid_data(format!("serialized object index {index} is out of range"))
        })?;
        if object.class_id != expected_class_id {
            return Err(Error::unsupported(format!(
                "object {} has class ID {}, not expected class {expected_class_id}",
                object.path_id, object.class_id
            )));
        }
        let payload = file.object_region(index)?;
        let endian = if file.header.endianness == 0 {
            Endian::Little
        } else {
            Endian::Big
        };
        let mut result = Self {
            reader: EndianReader::new(payload.cursor(), endian),
            format_version: file.header.version.0,
            no_target: file.target_platform == NO_TARGET_PLATFORM,
            limits,
            total_string_bytes: 0,
        };
        if result.no_target {
            let _object_hide_flags = result.reader.read_u32()?;
        }
        Ok(result)
    }

    const fn pptr_size(&self) -> usize {
        if self.format_version < 14 { 8 } else { 12 }
    }

    fn skip_editor_extension(&mut self) -> Result<()> {
        // EditorExtension serializes two PPtrs only for the NoTarget editor layout. The caller
        // reaches this method only after Object's hide flags have already been consumed.
        if self.no_target {
            let _prefab_parent_object = self.read_pptr()?;
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

    fn read_count(&mut self, field: &str, maximum: usize, minimum_size: usize) -> Result<usize> {
        let count = checked_length(self.reader.read_i32()?, field)?;
        if count > maximum {
            return Err(Error::invalid_data(format!(
                "{field} has {count} entries, exceeding limit {maximum}"
            )));
        }
        let minimum_bytes = count
            .checked_mul(minimum_size)
            .ok_or_else(|| Error::invalid_data(format!("{field} byte length overflowed")))?;
        if u64::try_from(minimum_bytes).unwrap_or(u64::MAX) > self.reader.remaining()? {
            return Err(Error::invalid_data(format!(
                "{field} needs at least {minimum_bytes} bytes beyond the bounded payload"
            )));
        }
        Ok(count)
    }

    fn read_string(&mut self, field: &str) -> Result<String> {
        let length = checked_length(self.reader.read_i32()?, field)?;
        if length > self.limits.maximum_string_bytes {
            return Err(Error::invalid_data(format!(
                "{field} is {length} bytes, exceeding limit {}",
                self.limits.maximum_string_bytes
            )));
        }
        self.charge_string_bytes(length)?;
        let value = self.reader.read_utf8(length)?;
        if length != 0 {
            align_absolute(&mut self.reader, 0, 4)?;
        }
        Ok(value)
    }

    fn copy_string(&mut self, value: &str, field: &str) -> Result<String> {
        self.charge_string_bytes(value.len())?;
        let mut copy = String::new();
        copy.try_reserve_exact(value.len()).map_err(|error| {
            Error::invalid_data(format!("cannot allocate {field} string: {error}"))
        })?;
        copy.push_str(value);
        Ok(copy)
    }

    fn charge_string_bytes(&mut self, length: usize) -> Result<()> {
        self.total_string_bytes = self
            .total_string_bytes
            .checked_add(length)
            .ok_or_else(|| Error::invalid_data("container metadata string budget overflowed"))?;
        if self.total_string_bytes > self.limits.maximum_total_string_bytes {
            return Err(Error::invalid_data(format!(
                "container metadata strings total {} bytes, exceeding limit {}",
                self.total_string_bytes, self.limits.maximum_total_string_bytes
            )));
        }
        Ok(())
    }
}

fn read_serialized_type<R: Read + Seek>(
    reader: &mut EndianReader<R>,
    version: SerializedFileFormatVersion,
    type_tree_enabled: bool,
    is_reference_type: bool,
    limits: &SerializedParseLimits,
    budget: &mut SerializedParseBudget,
) -> Result<SerializedType> {
    let class_id = reader.read_i32()?;
    let is_stripped_type = if version.0 >= 16 {
        reader.read_bool()?
    } else {
        false
    };
    let script_type_index = if version.0 >= 17 {
        reader.read_i16()?
    } else {
        -1
    };

    let (script_id, old_type_hash) = if version >= SerializedFileFormatVersion::HAS_TYPE_TREE_HASHES
    {
        let has_script_id = (is_reference_type && script_type_index >= 0)
            || (version.0 < 16 && class_id < 0)
            || (version.0 >= 16 && class_id == 114);
        let script_id = has_script_id.then(|| read_hash(reader)).transpose()?;
        (script_id, Some(read_hash(reader)?))
    } else {
        (None, None)
    };

    let mut type_dependencies = Vec::new();
    let mut class_name = None;
    let mut namespace = None;
    let mut assembly_name = None;
    let type_tree = if type_tree_enabled {
        let tree = if version.0 >= 12 || version.0 == 10 {
            read_type_tree_blob(reader, version, limits, budget)?
        } else {
            read_type_tree_recursive(reader, version, limits, budget)?
        };
        if version >= SerializedFileFormatVersion::STORES_TYPE_DEPENDENCIES {
            if is_reference_type {
                class_name = Some(read_metadata_string(
                    reader,
                    "reference type class name",
                    limits,
                    budget,
                )?);
                namespace = Some(read_metadata_string(
                    reader,
                    "reference type namespace",
                    limits,
                    budget,
                )?);
                assembly_name = Some(read_metadata_string(
                    reader,
                    "reference type assembly name",
                    limits,
                    budget,
                )?);
            } else {
                let count = read_record_count(
                    reader,
                    limits.maximum_type_dependencies,
                    "type dependency",
                    4,
                )?;
                type_dependencies = reserve_vec(count, "type dependencies")?;
                for _ in 0..count {
                    type_dependencies.push(reader.read_i32()?);
                }
            }
        }
        Some(tree)
    } else {
        None
    };

    Ok(SerializedType {
        class_id,
        is_stripped_type,
        script_type_index,
        script_id,
        old_type_hash,
        type_tree,
        type_dependencies,
        class_name,
        namespace,
        assembly_name,
    })
}

fn read_type_tree_blob<R: Read + Seek>(
    reader: &mut EndianReader<R>,
    version: SerializedFileFormatVersion,
    limits: &SerializedParseLimits,
    budget: &mut SerializedParseBudget,
) -> Result<TypeTree> {
    let node_count = read_count(reader, limits.maximum_type_tree_nodes, "type tree node")?;
    let string_buffer_size = read_count(
        reader,
        limits.maximum_string_buffer_bytes,
        "type tree string buffer",
    )?;
    charge_type_tree_nodes(node_count, limits, budget)?;
    charge_type_tree_string_buffer(string_buffer_size, limits, budget)?;
    let node_size = if version >= SerializedFileFormatVersion::TYPE_TREE_NODE_WITH_TYPE_FLAGS {
        32_u64
    } else {
        24_u64
    };
    let required_nodes = u64::try_from(node_count)
        .expect("node count fits in u64")
        .checked_mul(node_size)
        .ok_or_else(|| Error::invalid_data("type tree node byte size overflowed"))?;
    let required = required_nodes
        .checked_add(u64::try_from(string_buffer_size).expect("buffer size fits in u64"))
        .ok_or_else(|| Error::invalid_data("type tree byte size overflowed"))?;
    if required > reader.remaining()? {
        return Err(Error::invalid_data(format!(
            "type tree requires {required} bytes but only {} remain",
            reader.remaining()?
        )));
    }

    let mut nodes = reserve_vec(node_count, "type tree nodes")?;
    for _ in 0..node_count {
        let node_version = i32::from(reader.read_u16()?);
        let level = u32::from(reader.read_u8()?);
        let type_flags = i32::from(reader.read_u8()?);
        let type_string_offset = reader.read_u32()?;
        let name_string_offset = reader.read_u32()?;
        let byte_size = reader.read_i32()?;
        let index = reader.read_i32()?;
        let meta_flags = reader.read_i32()?;
        let reference_type_hash =
            if version >= SerializedFileFormatVersion::TYPE_TREE_NODE_WITH_TYPE_FLAGS {
                reader.read_u64()?
            } else {
                0
            };
        nodes.push(TypeTreeNode {
            type_name: String::new(),
            field_name: String::new(),
            byte_size,
            index,
            type_flags,
            version: node_version,
            meta_flags,
            level,
            type_string_offset: Some(type_string_offset),
            name_string_offset: Some(name_string_offset),
            reference_type_hash,
        });
    }
    let string_buffer = reader.read_bytes(string_buffer_size)?;
    for node in &mut nodes {
        node.type_name = resolve_type_tree_string(
            &string_buffer,
            node.type_string_offset.expect("blob nodes have an offset"),
            limits.maximum_string_bytes,
        )?;
        charge_materialized_metadata_string(node.type_name.len(), limits, budget)?;
        charge_type_tree_string_budget(node.type_name.len(), limits, budget)?;
        node.field_name = resolve_type_tree_string(
            &string_buffer,
            node.name_string_offset.expect("blob nodes have an offset"),
            limits.maximum_string_bytes,
        )?;
        charge_materialized_metadata_string(node.field_name.len(), limits, budget)?;
        charge_type_tree_string_budget(node.field_name.len(), limits, budget)?;
    }
    validate_blob_type_tree_levels(&nodes)?;

    Ok(TypeTree {
        nodes,
        string_buffer,
    })
}

fn read_type_tree_recursive<R: Read + Seek>(
    reader: &mut EndianReader<R>,
    version: SerializedFileFormatVersion,
    limits: &SerializedParseLimits,
    budget: &mut SerializedParseBudget,
) -> Result<TypeTree> {
    let mut nodes = Vec::new();
    let (root, root_children) = read_recursive_type_tree_node(reader, version, limits, budget, 0)?;
    nodes.try_reserve(1).map_err(|error| {
        Error::invalid_data(format!("cannot allocate a type tree node: {error}"))
    })?;
    nodes.push(root);

    let mut remaining_children = Vec::new();
    if root_children != 0 {
        remaining_children.try_reserve(1).map_err(|error| {
            Error::invalid_data(format!("cannot allocate the type tree stack: {error}"))
        })?;
        remaining_children.push(root_children);
    }
    while !remaining_children.is_empty() {
        let level = remaining_children.len();
        if level > limits.maximum_type_tree_depth {
            return Err(Error::invalid_data(format!(
                "type tree exceeds depth limit {}",
                limits.maximum_type_tree_depth
            )));
        }
        if nodes.len() >= limits.maximum_type_tree_nodes {
            return Err(Error::invalid_data(format!(
                "type tree exceeds node limit {}",
                limits.maximum_type_tree_nodes
            )));
        }

        let (node, child_count) =
            read_recursive_type_tree_node(reader, version, limits, budget, level)?;
        let parent_remaining = remaining_children
            .last_mut()
            .expect("the iterative type tree stack is non-empty");
        *parent_remaining = parent_remaining
            .checked_sub(1)
            .ok_or_else(|| Error::invalid_data("type tree child count underflowed"))?;
        nodes.try_reserve(1).map_err(|error| {
            Error::invalid_data(format!("cannot allocate a type tree node: {error}"))
        })?;
        nodes.push(node);
        if child_count != 0 {
            remaining_children.try_reserve(1).map_err(|error| {
                Error::invalid_data(format!("cannot grow the type tree stack: {error}"))
            })?;
            remaining_children.push(child_count);
        }
        while remaining_children.last() == Some(&0) {
            remaining_children.pop();
        }
    }

    Ok(TypeTree {
        nodes,
        string_buffer: Vec::new(),
    })
}

fn read_recursive_type_tree_node<R: Read + Seek>(
    reader: &mut EndianReader<R>,
    version: SerializedFileFormatVersion,
    limits: &SerializedParseLimits,
    budget: &mut SerializedParseBudget,
    level: usize,
) -> Result<(TypeTreeNode, usize)> {
    charge_type_tree_nodes(1, limits, budget)?;
    let type_name = read_metadata_string(reader, "type tree type name", limits, budget)?;
    let field_name = read_metadata_string(reader, "type tree field name", limits, budget)?;
    charge_type_tree_string_budget(type_name.len(), limits, budget)?;
    charge_type_tree_string_budget(field_name.len(), limits, budget)?;
    let byte_size = reader.read_i32()?;
    if version.0 == 2 {
        let _variable_count = reader.read_i32()?;
    }
    let index = if version.0 == 3 {
        0
    } else {
        reader.read_i32()?
    };
    let type_flags = reader.read_i32()?;
    let node_version = reader.read_i32()?;
    let meta_flags = if version.0 == 3 {
        0
    } else {
        reader.read_i32()?
    };
    let child_count = read_record_count(
        reader,
        limits.maximum_type_tree_nodes,
        "type tree child",
        26,
    )?;
    Ok((
        TypeTreeNode {
            type_name,
            field_name,
            byte_size,
            index,
            type_flags,
            version: node_version,
            meta_flags,
            level: u32::try_from(level).expect("type tree depth limit fits in u32"),
            type_string_offset: None,
            name_string_offset: None,
            reference_type_hash: 0,
        },
        child_count,
    ))
}

fn resolve_type_tree_string(
    buffer: &[u8],
    value: u32,
    maximum_string_bytes: usize,
) -> Result<String> {
    if value & 0x8000_0000 != 0 {
        let offset = value & 0x7fff_ffff;
        return Ok(
            common_type_tree_string(offset).map_or_else(|| offset.to_string(), str::to_owned)
        );
    }

    let offset = usize::try_from(value)
        .map_err(|_| Error::invalid_data("type tree string offset does not fit in usize"))?;
    let suffix = buffer.get(offset..).ok_or_else(|| {
        Error::invalid_data(format!(
            "type tree string offset {offset} exceeds {} bytes",
            buffer.len()
        ))
    })?;
    let length = suffix.iter().position(|byte| *byte == 0).ok_or_else(|| {
        Error::invalid_data(format!(
            "type tree string at offset {offset} is not nul-terminated"
        ))
    })?;
    if length > maximum_string_bytes {
        return Err(Error::invalid_data(format!(
            "type tree string at offset {offset} is {length} bytes, exceeding limit {maximum_string_bytes}"
        )));
    }
    Ok(String::from_utf8_lossy(&suffix[..length]).into_owned())
}

fn charge_type_tree_string_budget(
    length: usize,
    limits: &SerializedParseLimits,
    budget: &mut SerializedParseBudget,
) -> Result<()> {
    budget.resolved_type_tree_string_bytes = budget
        .resolved_type_tree_string_bytes
        .checked_add(length)
        .ok_or_else(|| Error::invalid_data("resolved type tree string budget overflowed"))?;
    if budget.resolved_type_tree_string_bytes > limits.maximum_resolved_type_tree_string_bytes {
        return Err(Error::invalid_data(format!(
            "resolved type tree strings exceed the {} byte limit",
            limits.maximum_resolved_type_tree_string_bytes
        )));
    }
    Ok(())
}

fn read_metadata_string<R: Read + Seek>(
    reader: &mut EndianReader<R>,
    field: &str,
    limits: &SerializedParseLimits,
    budget: &mut SerializedParseBudget,
) -> Result<String> {
    let value = reader.read_c_string_required(limits.maximum_string_bytes, field)?;
    charge_materialized_metadata_string(value.len(), limits, budget)?;
    Ok(value)
}

fn charge_materialized_metadata_string(
    length: usize,
    limits: &SerializedParseLimits,
    budget: &mut SerializedParseBudget,
) -> Result<()> {
    budget.materialized_metadata_string_bytes = budget
        .materialized_metadata_string_bytes
        .checked_add(length)
        .ok_or_else(|| Error::invalid_data("metadata string budget overflowed"))?;
    if budget.materialized_metadata_string_bytes > limits.maximum_materialized_metadata_string_bytes
    {
        return Err(Error::invalid_data(format!(
            "serialized metadata strings exceed the {} byte limit",
            limits.maximum_materialized_metadata_string_bytes
        )));
    }
    Ok(())
}

fn charge_type_tree_nodes(
    count: usize,
    limits: &SerializedParseLimits,
    budget: &mut SerializedParseBudget,
) -> Result<()> {
    budget.type_tree_nodes = budget
        .type_tree_nodes
        .checked_add(count)
        .ok_or_else(|| Error::invalid_data("type tree node budget overflowed"))?;
    if budget.type_tree_nodes > limits.maximum_total_type_tree_nodes {
        return Err(Error::invalid_data(format!(
            "serialized file exceeds the {} total type tree node limit",
            limits.maximum_total_type_tree_nodes
        )));
    }
    Ok(())
}

fn charge_type_tree_string_buffer(
    length: usize,
    limits: &SerializedParseLimits,
    budget: &mut SerializedParseBudget,
) -> Result<()> {
    budget.type_tree_string_buffer_bytes = budget
        .type_tree_string_buffer_bytes
        .checked_add(length)
        .ok_or_else(|| Error::invalid_data("type tree string buffer budget overflowed"))?;
    if budget.type_tree_string_buffer_bytes > limits.maximum_total_string_buffer_bytes {
        return Err(Error::invalid_data(format!(
            "serialized file exceeds the {} byte total type tree string-buffer limit",
            limits.maximum_total_string_buffer_bytes
        )));
    }
    Ok(())
}

fn validate_blob_type_tree_levels(nodes: &[TypeTreeNode]) -> Result<()> {
    let Some(root) = nodes.first() else {
        return Ok(());
    };
    if root.level != 0 {
        return Err(Error::invalid_data(format!(
            "type tree root has level {}, expected 0",
            root.level
        )));
    }
    let mut previous_level = root.level;
    for node in &nodes[1..] {
        if node.level == 0 {
            return Err(Error::invalid_data("type tree contains more than one root"));
        }
        if node.level > previous_level.saturating_add(1) {
            return Err(Error::invalid_data(format!(
                "type tree level jumps from {previous_level} to {}",
                node.level
            )));
        }
        previous_level = node.level;
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn common_type_tree_string(offset: u32) -> Option<&'static str> {
    Some(match offset {
        0 => "AABB",
        5 => "AnimationClip",
        19 => "AnimationCurve",
        34 => "AnimationState",
        49 => "Array",
        55 => "Base",
        60 => "BitField",
        69 => "bitset",
        76 => "bool",
        81 => "char",
        86 => "ColorRGBA",
        96 => "Component",
        106 => "data",
        111 => "deque",
        117 => "double",
        124 => "dynamic_array",
        138 => "FastPropertyName",
        155 => "first",
        161 => "float",
        167 => "Font",
        172 => "GameObject",
        183 => "Generic Mono",
        196 => "GradientNEW",
        208 => "GUID",
        213 => "GUIStyle",
        222 => "int",
        226 => "list",
        231 => "long long",
        241 => "map",
        245 => "Matrix4x4f",
        256 => "MdFour",
        263 => "MonoBehaviour",
        277 => "MonoScript",
        288 => "m_ByteSize",
        299 => "m_Curve",
        307 => "m_EditorClassIdentifier",
        331 => "m_EditorHideFlags",
        349 => "m_Enabled",
        359 => "m_ExtensionPtr",
        374 => "m_GameObject",
        387 => "m_Index",
        395 => "m_IsArray",
        405 => "m_IsStatic",
        416 => "m_MetaFlag",
        427 => "m_Name",
        434 => "m_ObjectHideFlags",
        452 => "m_PrefabInternal",
        469 => "m_PrefabParentObject",
        490 => "m_Script",
        499 => "m_StaticEditorFlags",
        519 => "m_Type",
        526 => "m_Version",
        536 => "Object",
        543 => "pair",
        548 => "PPtr<Component>",
        564 => "PPtr<GameObject>",
        581 => "PPtr<Material>",
        596 => "PPtr<MonoBehaviour>",
        616 => "PPtr<MonoScript>",
        633 => "PPtr<Object>",
        646 => "PPtr<Prefab>",
        659 => "PPtr<Sprite>",
        672 => "PPtr<TextAsset>",
        688 => "PPtr<Texture>",
        702 => "PPtr<Texture2D>",
        718 => "PPtr<Transform>",
        734 => "Prefab",
        741 => "Quaternionf",
        753 => "Rectf",
        759 => "RectInt",
        767 => "RectOffset",
        778 => "second",
        785 => "set",
        789 => "short",
        795 => "size",
        800 => "SInt16",
        807 => "SInt32",
        814 => "SInt64",
        821 => "SInt8",
        827 => "staticvector",
        840 => "string",
        847 => "TextAsset",
        857 => "TextMesh",
        866 => "Texture",
        874 => "Texture2D",
        884 => "Transform",
        894 => "TypelessData",
        907 => "UInt16",
        914 => "UInt32",
        921 => "UInt64",
        928 => "UInt8",
        934 => "unsigned int",
        947 => "unsigned long long",
        966 => "unsigned short",
        981 => "vector",
        988 => "Vector2f",
        997 => "Vector3f",
        1006 => "Vector4f",
        1015 => "m_ScriptingClassIdentifier",
        1042 => "Gradient",
        1051 => "Type*",
        1057 => "int2_storage",
        1070 => "int3_storage",
        1083 => "BoundsInt",
        1093 => "m_CorrespondingSourceObject",
        1121 => "m_PrefabInstance",
        1138 => "m_PrefabAsset",
        1152 => "FileSize",
        1161 => "Hash128",
        1169 => "RenderingLayerMask",
        1188 => "fixed_array",
        1200 => "EntityId",
        _ => return None,
    })
}

fn read_hash<R: Read + Seek>(reader: &mut EndianReader<R>) -> Result<[u8; 16]> {
    reader
        .read_bytes(16)?
        .try_into()
        .map_err(|_| Error::invalid_data("failed to read a 16-byte hash"))
}

fn read_count<R: Read + Seek>(
    reader: &mut EndianReader<R>,
    maximum: usize,
    field: &str,
) -> Result<usize> {
    let value = checked_length(reader.read_i32()?, field)?;
    if value > maximum {
        return Err(Error::invalid_data(format!(
            "{field} count {value} exceeds limit {maximum}"
        )));
    }
    Ok(value)
}

fn read_record_count<R: Read + Seek>(
    reader: &mut EndianReader<R>,
    maximum: usize,
    field: &str,
    minimum_record_size: u64,
) -> Result<usize> {
    let count = read_count(reader, maximum, field)?;
    let required = u64::try_from(count)
        .expect("record count fits in u64")
        .checked_mul(minimum_record_size)
        .ok_or_else(|| Error::invalid_data(format!("{field} minimum byte size overflowed")))?;
    let remaining = reader.remaining()?;
    if required > remaining {
        return Err(Error::invalid_data(format!(
            "{field} count {count} requires at least {required} bytes but only {remaining} remain"
        )));
    }
    Ok(count)
}

const fn minimum_serialized_type_size(
    version: SerializedFileFormatVersion,
    type_tree_enabled: bool,
    is_reference_type: bool,
) -> u64 {
    let mut size = 4;
    if version.0 >= 16 {
        size += 1;
    }
    if version.0 >= 17 {
        size += 2;
    }
    if version.0 >= 13 {
        size += 16;
    }
    if type_tree_enabled {
        size += if version.0 >= 12 || version.0 == 10 {
            8
        } else {
            26
        };
        if version.0 >= 21 {
            size += if is_reference_type { 3 } else { 4 };
        }
    }
    size
}

const fn minimum_object_info_size(
    version: SerializedFileFormatVersion,
    big_id_enabled: i32,
) -> u64 {
    let mut size = if big_id_enabled != 0 || version.0 >= 14 {
        8
    } else {
        4
    };
    size += if version.0 >= 22 { 8 } else { 4 };
    size += 8;
    if version.0 < 16 {
        size += 2;
    }
    if version.0 < 11 {
        size += 2;
    }
    if version.0 >= 11 && version.0 < 17 {
        size += 2;
    }
    if version.0 == 15 || version.0 == 16 {
        size += 1;
    }
    size
}

fn reject_duplicate_path_ids(objects: &[ObjectInfo]) -> Result<()> {
    let mut path_ids = Vec::new();
    path_ids.try_reserve_exact(objects.len()).map_err(|error| {
        Error::invalid_data(format!("cannot allocate the Path ID index: {error}"))
    })?;
    path_ids.extend(objects.iter().map(|object| object.path_id));
    path_ids.sort_unstable();
    if let Some(duplicate) = path_ids
        .windows(2)
        .find(|pair| pair[0] == pair[1])
        .map(|pair| pair[0])
    {
        return Err(Error::invalid_data(format!(
            "serialized file contains duplicate Path ID {duplicate}"
        )));
    }
    Ok(())
}

fn reserve_vec<T>(capacity: usize, field: &str) -> Result<Vec<T>> {
    let mut values = Vec::new();
    values.try_reserve_exact(capacity).map_err(|error| {
        Error::invalid_data(format!("cannot allocate {capacity} {field}: {error}"))
    })?;
    Ok(values)
}

fn consume_metadata_tail<R: Read + Seek>(reader: &mut EndianReader<R>) -> Result<()> {
    // Unity generators may retain reserved or forward-version bytes inside the
    // declared metadata range. The reader is already bounded to that range, so
    // accepting the unparsed tail cannot cross into object data.
    let end = reader.len()?;
    reader.set_position(end)
}

fn positive_i64(value: i64, field: &str) -> Result<u64> {
    u64::try_from(value).map_err(|_| Error::invalid_data(format!("{field} cannot be negative")))
}

fn validate_object_range(start: u64, size: u64, data_start: u64, data_end: u64) -> Result<()> {
    if start < data_start {
        return Err(Error::invalid_data(format!(
            "serialized object starts at {start}, before data offset {data_start}"
        )));
    }
    let end = start
        .checked_add(size)
        .ok_or_else(|| Error::invalid_data("serialized object range overflowed"))?;
    if end > data_end {
        return Err(Error::invalid_data(format!(
            "serialized object range {start}..{end} exceeds object-data end {data_end}"
        )));
    }
    Ok(())
}

fn align_absolute<R: Read + Seek>(
    reader: &mut EndianReader<R>,
    base_offset: u64,
    alignment: u64,
) -> Result<()> {
    let position = reader.position()?;
    let absolute = base_offset
        .checked_add(position)
        .ok_or_else(|| Error::invalid_data("absolute alignment position overflowed"))?;
    let remainder = absolute % alignment;
    if remainder == 0 {
        return Ok(());
    }
    let padding = alignment - remainder;
    let target = position
        .checked_add(padding)
        .ok_or_else(|| Error::invalid_data("aligned reader position overflowed"))?;
    if target > reader.len()? {
        return Err(Error::invalid_data("alignment exceeds the bounded input"));
    }
    reader.set_position(target)
}

fn read_aligned_string_limited<R: Read + Seek>(
    reader: &mut EndianReader<R>,
    base_offset: u64,
    maximum: usize,
    field: &str,
) -> Result<String> {
    let length = checked_length(reader.read_i32()?, field)?;
    if length > maximum {
        return Err(Error::invalid_data(format!(
            "{field} is {length} bytes, exceeding limit {maximum}"
        )));
    }
    let value = reader.read_utf8(length)?;
    if length != 0 {
        align_absolute(reader, base_offset, 4)?;
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::{SerializedFile, SerializedOpenOptions, SerializedParseLimits};
    use crate::Error;
    use crate::source::Region;
    use crate::unity_version::UnityVersion;

    #[test]
    fn rejects_pre_v5_formats_as_explicitly_unsupported() {
        for version in 1..=4_u32 {
            // Legacy headers put the endian byte at file_size - metadata_size.
            // No invented metadata follows: the production parser must reject
            // the unsupported format immediately after validating its header.
            let mut bytes = vec![0_u8; 17];
            bytes[0..4].copy_from_slice(&1_u32.to_be_bytes());
            bytes[4..8].copy_from_slice(&17_u32.to_be_bytes());
            bytes[8..12].copy_from_slice(&version.to_be_bytes());
            bytes[12..16].copy_from_slice(&16_u32.to_be_bytes());
            bytes[16] = 0;

            match SerializedFile::open(Region::from_bytes(bytes)) {
                Err(Error::Unsupported(message)) => assert!(
                    message.contains(&format!("version {version}")),
                    "format {version} error did not identify its version: {message}"
                ),
                Err(other) => {
                    panic!("format {version} must be unsupported rather than malformed: {other}")
                }
                Ok(_) => panic!("format {version} unexpectedly parsed without a verified layout"),
            }
        }
    }

    #[test]
    fn parses_v22_metadata_object_index_type_tree_and_text_asset() {
        let bytes = synthetic_v22_text_asset(false);
        let file = SerializedFile::open(Region::from_bytes(bytes)).unwrap();

        assert_eq!(file.header.version.0, 22);
        assert_eq!(file.unity_version.to_string(), "2022.3.62f1");
        assert_eq!(file.target_platform, 13);
        assert!(file.type_tree_enabled);
        assert_eq!(file.types.len(), 1);
        assert_eq!(file.types[0].class_id, 49);
        let tree = file.types[0].type_tree.as_ref().unwrap();
        assert_eq!(tree.nodes[0].type_name, "TextAsset");
        assert_eq!(tree.nodes[0].field_name, "m_Name");
        assert_eq!(file.objects.len(), 1);
        assert_eq!(file.objects[0].path_id, 7);

        let text = file.read_text_asset(0, 1024).unwrap();
        assert_eq!(text.path_id, 7);
        assert_eq!(text.name, "demo");
        assert_eq!(text.script, b"payload");
        assert_eq!(
            file.object_region_by_path_id(7)
                .unwrap()
                .read_to_vec(1024)
                .unwrap(),
            file.read_object_bytes(0, 1024).unwrap()
        );
    }

    #[test]
    fn parses_no_target_text_asset_prefix() {
        let bytes = synthetic_v22_text_asset(true);
        let file = SerializedFile::open(Region::from_bytes(bytes)).unwrap();
        let text = file.read_text_asset(0, 1024).unwrap();
        assert_eq!(text.name, "demo");
        assert_eq!(text.script, b"payload");
    }

    #[test]
    fn accepts_reserved_nonzero_bytes_at_the_end_of_declared_metadata() {
        let mut bytes = synthetic_v22_text_asset(false);
        let metadata_size = u32::from_be_bytes(bytes[20..24].try_into().unwrap());
        let metadata_end = 48_usize + usize::try_from(metadata_size).unwrap();
        let data_offset =
            usize::try_from(i64::from_be_bytes(bytes[32..40].try_into().unwrap())).unwrap();
        assert!(metadata_end < data_offset);
        bytes[metadata_end] = 0xa5;
        bytes[20..24].copy_from_slice(&(metadata_size + 1).to_be_bytes());

        let file = SerializedFile::open(Region::from_bytes(bytes)).unwrap();
        assert_eq!(file.objects[0].path_id, 7);
        assert_eq!(file.read_text_asset(0, 1024).unwrap().script, b"payload");
    }

    #[test]
    fn rejects_object_ranges_outside_the_source() {
        let mut bytes = synthetic_v22_text_asset(false);
        let data_offset = i64::from_be_bytes(bytes[32..40].try_into().unwrap());
        let metadata_start = 48_usize;
        let path_marker = 7_i64.to_le_bytes();
        let path_position = bytes[metadata_start..usize::try_from(data_offset).unwrap()]
            .windows(path_marker.len())
            .position(|window| window == path_marker)
            .unwrap()
            + metadata_start;
        let relative_start_position = path_position + 8;
        bytes[relative_start_position..relative_start_position + 8]
            .copy_from_slice(&i64::MAX.to_le_bytes());

        assert!(SerializedFile::open(Region::from_bytes(bytes)).is_err());
    }

    #[test]
    fn enforces_metadata_count_limits_before_allocation() {
        let bytes = synthetic_v22_text_asset(false);
        let limits = SerializedParseLimits {
            maximum_types: 0,
            ..SerializedParseLimits::default()
        };
        assert!(SerializedFile::open_with_limits(Region::from_bytes(bytes), limits).is_err());
    }

    #[test]
    fn a_bundle_revision_only_overrides_files_that_cannot_state_their_own_version() {
        let hint = UnityVersion::new(2022, 3, 62);
        let explicit = UnityVersion::new(6000, 2, 0);
        let options = |bundle_version_hint, unity_version_override| SerializedOpenOptions {
            unity_version_override,
            bundle_version_hint,
            ..SerializedOpenOptions::default()
        };

        // Format 6 stores no version string, so the managed reader substitutes
        // the enclosing bundle's revision and so does this one.
        let legacy = synthetic_versioned_file(SyntheticOptions {
            version: 6,
            ..SyntheticOptions::default()
        });
        let file = SerializedFile::open_with_options(
            Region::from_bytes(legacy.bytes.clone()),
            options(Some(hint.clone()), None),
        )
        .unwrap();
        assert_eq!(file.unity_version_string, "2.5.0f1");
        assert_eq!(file.unity_version, hint);

        // Format 13 declares 2018.4.0f1. The managed reader leaves that alone
        // for anything at or above format 7, so the bundle revision must not
        // silently shift every downstream version gate.
        let modern = synthetic_versioned_file(SyntheticOptions {
            version: 13,
            ..SyntheticOptions::default()
        });
        let file = SerializedFile::open_with_options(
            Region::from_bytes(modern.bytes.clone()),
            options(Some(hint.clone()), None),
        )
        .unwrap();
        assert_eq!(file.unity_version_string, "2018.4.0f1");
        assert_eq!(file.unity_version.full_version, "2018.4.0f1");

        // An explicit caller override outranks both, at either format version.
        for fixture in [&legacy, &modern] {
            let file = SerializedFile::open_with_options(
                Region::from_bytes(fixture.bytes.clone()),
                options(Some(hint.clone()), Some(explicit.clone())),
            )
            .unwrap();
            assert_eq!(file.unity_version, explicit);
        }
    }

    #[test]
    fn parses_v5_and_v8_legacy_eof_metadata_in_both_endiannesses() {
        let v5 = synthetic_versioned_file(SyntheticOptions {
            version: 5,
            endian: SyntheticEndian::Little,
            ..SyntheticOptions::default()
        });
        let v5_file = SerializedFile::open(Region::from_bytes(v5.bytes)).unwrap();
        assert_eq!(v5_file.header.metadata_range().unwrap().start, 20);
        assert_eq!(v5_file.header.object_data_end().unwrap(), 20);
        assert_eq!(v5_file.unity_version_string, "2.5.0f1");
        assert_eq!(v5_file.target_platform, 9_999);
        assert_eq!(v5_file.objects[0].path_id, v5.path_id);
        assert_eq!(v5_file.objects[0].destroyed, 9);
        assert_eq!(v5_file.objects[0].serialized_type_index, Some(0));
        assert_eq!(v5_file.read_object_bytes(0, 16).unwrap(), v5.object);

        let v8 = synthetic_versioned_file(SyntheticOptions {
            version: 8,
            endian: SyntheticEndian::Big,
            big_id_enabled: true,
            ..SyntheticOptions::default()
        });
        let v8_file = SerializedFile::open(Region::from_bytes(v8.bytes)).unwrap();
        assert_eq!(v8_file.header.metadata_range().unwrap().start, 20);
        assert_eq!(v8_file.header.endianness, 1);
        assert_eq!(v8_file.unity_version_string, "2018.4.0f1");
        assert_eq!(v8_file.target_platform, 13);
        assert_eq!(v8_file.big_id_enabled, 1);
        assert_eq!(v8_file.objects[0].path_id, v8.path_id);
        assert_eq!(v8_file.objects[0].byte_start, 16);
        assert_eq!(v8_file.read_object_bytes(0, 16).unwrap(), v8.object);
    }

    #[test]
    fn selects_recursive_and_blob_type_trees_across_v10_v11_v12() {
        for version in [10, 11, 12] {
            let fixture = synthetic_versioned_file(SyntheticOptions {
                version,
                big_id_enabled: true,
                ..SyntheticOptions::default()
            });
            let file = SerializedFile::open(Region::from_bytes(fixture.bytes)).unwrap();
            let tree = file.types[0].type_tree.as_ref().unwrap();
            let uses_blob = version == 10 || version == 12;

            assert_eq!(tree.nodes.len(), 1);
            assert_eq!(tree.nodes[0].type_name, "TextAsset");
            assert_eq!(tree.nodes[0].field_name, "m_Name");
            assert_eq!(tree.nodes[0].type_string_offset.is_some(), uses_blob);
            assert_eq!(tree.string_buffer.is_empty(), !uses_blob);
            assert_eq!(file.objects[0].path_id, fixture.path_id);
            assert_eq!(file.objects[0].destroyed, if version == 10 { 9 } else { 0 });
            assert_eq!(
                file.objects[0].script_type_index,
                (version >= 11).then_some(-3)
            );
            assert_eq!(file.script_types.len(), usize::from(version >= 11));
        }
    }

    #[test]
    fn parses_v13_with_type_tree_disabled() {
        let fixture = synthetic_versioned_file(SyntheticOptions {
            version: 13,
            type_tree_enabled: false,
            ..SyntheticOptions::default()
        });
        let file = SerializedFile::open(Region::from_bytes(fixture.bytes)).unwrap();

        assert!(!file.type_tree_enabled);
        assert!(file.types[0].type_tree.is_none());
        assert_eq!(file.types[0].old_type_hash, Some([0xa5; 16]));
        assert_eq!(file.big_id_enabled, 0);
        assert_eq!(file.objects[0].path_id, fixture.path_id);
        assert_eq!(file.objects[0].script_type_index, Some(-3));
    }

    #[test]
    fn parses_v14_aligned_64_bit_path_ids() {
        let fixture = synthetic_versioned_file(SyntheticOptions {
            version: 14,
            type_tree_enabled: false,
            ..SyntheticOptions::default()
        });
        let file = SerializedFile::open(Region::from_bytes(fixture.bytes)).unwrap();

        assert_eq!(file.big_id_enabled, 0);
        assert_eq!(file.objects[0].path_id, fixture.path_id);
        assert_eq!(file.objects[0].path_id, 0x0102_0304_0506_0708);
        assert_eq!(file.script_types.len(), 1);
        assert_eq!(
            file.script_types[0].local_identifier_in_file,
            0x1112_1314_1516_1718
        );
        assert_eq!(file.read_object_bytes(0, 16).unwrap(), fixture.object);
    }

    #[test]
    fn switches_from_v16_object_script_fields_to_v17_type_indexing() {
        for version in [16, 17] {
            let fixture = synthetic_versioned_file(SyntheticOptions {
                version,
                type_tree_enabled: false,
                ..SyntheticOptions::default()
            });
            let file = SerializedFile::open(Region::from_bytes(fixture.bytes)).unwrap();
            let kind = &file.types[0];
            let object = &file.objects[0];

            assert!(kind.is_stripped_type);
            assert_eq!(kind.script_type_index, if version == 16 { -1 } else { 3 });
            assert_eq!(object.type_id, 0);
            assert_eq!(object.class_id, 49);
            assert_eq!(object.serialized_type_index, Some(0));
            assert_eq!(object.script_type_index, (version == 16).then_some(-3));
            assert_eq!(object.stripped, u8::from(version == 16));
        }
    }

    #[test]
    fn parses_v19_v20_v21_type_tree_reference_and_dependency_gates() {
        let v19 = synthetic_versioned_file(SyntheticOptions {
            version: 19,
            ..SyntheticOptions::default()
        });
        let v19_file = SerializedFile::open(Region::from_bytes(v19.bytes)).unwrap();
        let v19_tree = v19_file.types[0].type_tree.as_ref().unwrap();
        assert_eq!(v19_tree.nodes[0].reference_type_hash, 0x0102_0304_0506_0708);
        assert!(v19_file.reference_types.is_empty());
        assert!(v19_file.types[0].type_dependencies.is_empty());

        let v20 = synthetic_versioned_file(SyntheticOptions {
            version: 20,
            type_tree_enabled: false,
            include_reference_type: true,
            ..SyntheticOptions::default()
        });
        let v20_file = SerializedFile::open(Region::from_bytes(v20.bytes)).unwrap();
        assert_eq!(v20_file.reference_types.len(), 1);
        assert_eq!(v20_file.reference_types[0].class_id, 47);
        assert!(v20_file.reference_types[0].type_tree.is_none());
        assert!(v20_file.reference_types[0].class_name.is_none());

        let v21 = synthetic_versioned_file(SyntheticOptions {
            version: 21,
            include_reference_type: true,
            ..SyntheticOptions::default()
        });
        let v21_file = SerializedFile::open(Region::from_bytes(v21.bytes)).unwrap();
        assert_eq!(v21_file.types[0].type_dependencies, [77]);
        assert_eq!(v21_file.reference_types.len(), 1);
        assert_eq!(
            v21_file.reference_types[0].class_name.as_deref(),
            Some("ReferenceClass")
        );
        assert_eq!(
            v21_file.reference_types[0].namespace.as_deref(),
            Some("Tests")
        );
        assert_eq!(
            v21_file.reference_types[0].assembly_name.as_deref(),
            Some("Tests.dll")
        );
        assert!(v21_file.reference_types[0].type_dependencies.is_empty());
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum SyntheticEndian {
        Little,
        Big,
    }

    impl SyntheticEndian {
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

        fn push_u64(self, output: &mut Vec<u8>, value: u64) {
            match self {
                Self::Little => output.extend_from_slice(&value.to_le_bytes()),
                Self::Big => output.extend_from_slice(&value.to_be_bytes()),
            }
        }
    }

    #[derive(Debug, Clone, Copy)]
    struct SyntheticOptions {
        version: u32,
        endian: SyntheticEndian,
        type_tree_enabled: bool,
        big_id_enabled: bool,
        include_reference_type: bool,
    }

    impl Default for SyntheticOptions {
        fn default() -> Self {
            Self {
                version: 21,
                endian: SyntheticEndian::Little,
                type_tree_enabled: true,
                big_id_enabled: false,
                include_reference_type: false,
            }
        }
    }

    struct SyntheticFixture {
        bytes: Vec<u8>,
        object: Vec<u8>,
        path_id: i64,
    }

    #[allow(clippy::too_many_lines)]
    fn synthetic_versioned_file(options: SyntheticOptions) -> SyntheticFixture {
        assert!((5..=21).contains(&options.version));
        assert!(options.version >= 20 || !options.include_reference_type);
        let endian = options.endian;
        let object = vec![0xde, 0xad, 0xbe, 0xef];
        let metadata_base = if options.version < 9 { 0 } else { 20 };
        let type_tree_enabled = options.version < 13 || options.type_tree_enabled;
        let big_id_enabled = (7..14).contains(&options.version) && options.big_id_enabled;
        let path_id = if big_id_enabled || options.version >= 14 {
            0x0102_0304_0506_0708
        } else {
            0x0102_0304
        };

        let mut metadata = Vec::new();
        if options.version >= 7 {
            push_c_string(&mut metadata, "2018.4.0f1");
        }
        if options.version >= 8 {
            endian.push_i32(&mut metadata, 13);
        }
        if options.version >= 13 {
            metadata.push(u8::from(type_tree_enabled));
        }
        endian.push_i32(&mut metadata, 1);
        push_synthetic_type(&mut metadata, options, false, type_tree_enabled);

        if (7..14).contains(&options.version) {
            endian.push_i32(&mut metadata, i32::from(big_id_enabled));
        }
        endian.push_i32(&mut metadata, 1);
        if big_id_enabled {
            endian.push_i64(&mut metadata, path_id);
        } else if options.version < 14 {
            endian.push_i32(
                &mut metadata,
                i32::try_from(path_id).expect("short synthetic path ID fits in i32"),
            );
        } else {
            align_vec_with_base(&mut metadata, metadata_base, 4);
            endian.push_i64(&mut metadata, path_id);
        }
        endian.push_u32(&mut metadata, 0);
        endian.push_u32(
            &mut metadata,
            u32::try_from(object.len()).expect("synthetic object length fits in u32"),
        );
        endian.push_i32(&mut metadata, if options.version < 16 { 49 } else { 0 });
        if options.version < 16 {
            endian.push_u16(&mut metadata, 49);
        }
        if options.version < 11 {
            endian.push_u16(&mut metadata, 9);
        }
        if (11..17).contains(&options.version) {
            endian.push_i16(&mut metadata, -3);
        }
        if matches!(options.version, 15 | 16) {
            metadata.push(1);
        }

        if options.version >= 11 {
            endian.push_i32(&mut metadata, 1);
            endian.push_i32(&mut metadata, 2);
            if options.version < 14 {
                endian.push_i32(&mut metadata, 0x1112_1314);
            } else {
                align_vec_with_base(&mut metadata, metadata_base, 4);
                endian.push_i64(&mut metadata, 0x1112_1314_1516_1718);
            }
        }

        endian.push_i32(&mut metadata, 0);
        if options.version >= 20 {
            endian.push_i32(&mut metadata, i32::from(options.include_reference_type));
            if options.include_reference_type {
                push_synthetic_type(&mut metadata, options, true, type_tree_enabled);
            }
        }
        push_c_string(&mut metadata, "fixture-user");

        let bytes = assemble_synthetic_file(options, &metadata, &object);
        SyntheticFixture {
            bytes,
            object,
            path_id,
        }
    }

    fn push_synthetic_type(
        output: &mut Vec<u8>,
        options: SyntheticOptions,
        is_reference_type: bool,
        type_tree_enabled: bool,
    ) {
        let endian = options.endian;
        let class_id = if is_reference_type { 47 } else { 49 };
        endian.push_i32(output, class_id);
        if options.version >= 16 {
            output.push(u8::from(!is_reference_type));
        }
        let script_type_index = if is_reference_type { -1 } else { 3 };
        if options.version >= 17 {
            endian.push_i16(output, script_type_index);
        }
        if options.version >= 13 {
            output.extend_from_slice(&[0xa5; 16]);
        }
        if !type_tree_enabled {
            return;
        }

        if options.version >= 12 || options.version == 10 {
            push_synthetic_blob_type_tree(output, options);
        } else {
            push_synthetic_recursive_type_tree(output, options);
        }
        if options.version >= 21 {
            if is_reference_type {
                push_c_string(output, "ReferenceClass");
                push_c_string(output, "Tests");
                push_c_string(output, "Tests.dll");
            } else {
                endian.push_i32(output, 1);
                endian.push_i32(output, 77);
            }
        }
    }

    fn push_synthetic_blob_type_tree(output: &mut Vec<u8>, options: SyntheticOptions) {
        let endian = options.endian;
        endian.push_i32(output, 1);
        endian.push_i32(output, 1);
        endian.push_u16(output, 2);
        output.push(0);
        output.push(1);
        endian.push_u32(output, 0x8000_0000 | 0x034f);
        endian.push_u32(output, 0x8000_0000 | 0x01ab);
        endian.push_i32(output, -1);
        endian.push_i32(output, 0);
        endian.push_i32(output, 0x4000);
        if options.version >= 19 {
            endian.push_u64(output, 0x0102_0304_0506_0708);
        }
        output.push(0);
    }

    fn push_synthetic_recursive_type_tree(output: &mut Vec<u8>, options: SyntheticOptions) {
        let endian = options.endian;
        push_c_string(output, "TextAsset");
        push_c_string(output, "m_Name");
        endian.push_i32(output, -1);
        endian.push_i32(output, 0);
        endian.push_i32(output, 1);
        endian.push_i32(output, 2);
        endian.push_i32(output, 0x4000);
        endian.push_i32(output, 0);
    }

    fn assemble_synthetic_file(
        options: SyntheticOptions,
        metadata: &[u8],
        object: &[u8],
    ) -> Vec<u8> {
        if options.version < 9 {
            let metadata_size = 1 + metadata.len();
            let file_size = 16 + object.len() + metadata_size;
            let mut bytes = vec![0_u8; 16];
            bytes[0..4].copy_from_slice(
                &u32::try_from(metadata_size)
                    .expect("synthetic metadata size fits in u32")
                    .to_be_bytes(),
            );
            bytes[4..8].copy_from_slice(
                &u32::try_from(file_size)
                    .expect("synthetic file size fits in u32")
                    .to_be_bytes(),
            );
            bytes[8..12].copy_from_slice(&options.version.to_be_bytes());
            bytes[12..16].copy_from_slice(&16_u32.to_be_bytes());
            bytes.extend_from_slice(object);
            bytes.push(options.endian.marker());
            bytes.extend_from_slice(metadata);
            return bytes;
        }

        let data_offset = (20 + metadata.len()).next_multiple_of(16);
        let file_size = data_offset + object.len();
        let mut bytes = vec![0_u8; 20];
        bytes[0..4].copy_from_slice(
            &u32::try_from(metadata.len())
                .expect("synthetic metadata size fits in u32")
                .to_be_bytes(),
        );
        bytes[4..8].copy_from_slice(
            &u32::try_from(file_size)
                .expect("synthetic file size fits in u32")
                .to_be_bytes(),
        );
        bytes[8..12].copy_from_slice(&options.version.to_be_bytes());
        bytes[12..16].copy_from_slice(
            &u32::try_from(data_offset)
                .expect("synthetic data offset fits in u32")
                .to_be_bytes(),
        );
        bytes[16] = options.endian.marker();
        bytes.extend_from_slice(metadata);
        bytes.resize(data_offset, 0);
        bytes.extend_from_slice(object);
        bytes
    }

    fn push_c_string(output: &mut Vec<u8>, value: &str) {
        output.extend_from_slice(value.as_bytes());
        output.push(0);
    }

    fn synthetic_v22_text_asset(no_target: bool) -> Vec<u8> {
        let mut object = Vec::new();
        if no_target {
            push_u32_le(&mut object, 0);
            for _ in 0..2 {
                push_i32_le(&mut object, 0);
                push_i64_le(&mut object, 0);
            }
        }
        push_i32_le(&mut object, 4);
        object.extend_from_slice(b"demo");
        align_vec(&mut object, 4);
        push_i32_le(&mut object, 7);
        object.extend_from_slice(b"payload");

        let mut metadata = Vec::new();
        metadata.extend_from_slice(b"2022.3.62f1\0");
        push_i32_le(&mut metadata, if no_target { -2 } else { 13 });
        metadata.push(1);
        push_i32_le(&mut metadata, 1);
        push_i32_le(&mut metadata, 49);
        metadata.push(0);
        metadata.extend_from_slice(&(-1_i16).to_le_bytes());
        metadata.extend_from_slice(&[0_u8; 16]);
        push_i32_le(&mut metadata, 1);
        push_i32_le(&mut metadata, 0);
        metadata.extend_from_slice(&2_u16.to_le_bytes());
        metadata.push(0);
        metadata.push(0);
        push_u32_le(&mut metadata, 0x8000_0000 | 0x034f);
        push_u32_le(&mut metadata, 0x8000_0000 | 0x01ab);
        push_i32_le(&mut metadata, -1);
        push_i32_le(&mut metadata, 0);
        push_i32_le(&mut metadata, 0);
        metadata.extend_from_slice(&0_u64.to_le_bytes());
        push_i32_le(&mut metadata, 0);
        push_i32_le(&mut metadata, 1);
        align_vec_with_base(&mut metadata, 48, 4);
        push_i64_le(&mut metadata, 7);
        push_i64_le(&mut metadata, 0);
        push_u32_le(
            &mut metadata,
            u32::try_from(object.len()).expect("test object length fits in u32"),
        );
        push_i32_le(&mut metadata, 0);
        push_i32_le(&mut metadata, 0);
        push_i32_le(&mut metadata, 0);
        push_i32_le(&mut metadata, 0);
        metadata.push(0);

        let metadata_end = 48_u64 + u64::try_from(metadata.len()).unwrap();
        let data_offset = metadata_end
            .checked_add(15)
            .expect("test data offset does not overflow")
            / 16
            * 16;
        let file_size = data_offset + u64::try_from(object.len()).unwrap();
        let mut bytes = vec![0_u8; 48];
        bytes[8..12].copy_from_slice(&22_u32.to_be_bytes());
        bytes[16] = 0;
        bytes[20..24].copy_from_slice(
            &u32::try_from(metadata.len())
                .expect("test metadata length fits in u32")
                .to_be_bytes(),
        );
        bytes[24..32].copy_from_slice(
            &i64::try_from(file_size)
                .expect("test file size fits in i64")
                .to_be_bytes(),
        );
        bytes[32..40].copy_from_slice(
            &i64::try_from(data_offset)
                .expect("test data offset fits in i64")
                .to_be_bytes(),
        );
        bytes.extend_from_slice(&metadata);
        bytes.resize(
            usize::try_from(data_offset).expect("test data offset fits in usize"),
            0,
        );
        bytes.extend_from_slice(&object);
        bytes
    }

    fn push_i32_le(output: &mut Vec<u8>, value: i32) {
        output.extend_from_slice(&value.to_le_bytes());
    }

    fn push_u32_le(output: &mut Vec<u8>, value: u32) {
        output.extend_from_slice(&value.to_le_bytes());
    }

    fn push_i64_le(output: &mut Vec<u8>, value: i64) {
        output.extend_from_slice(&value.to_le_bytes());
    }

    fn align_vec(output: &mut Vec<u8>, alignment: usize) {
        while !output.len().is_multiple_of(alignment) {
            output.push(0);
        }
    }

    fn align_vec_with_base(output: &mut Vec<u8>, base: usize, alignment: usize) {
        while !(base + output.len()).is_multiple_of(alignment) {
            output.push(0);
        }
    }
}
