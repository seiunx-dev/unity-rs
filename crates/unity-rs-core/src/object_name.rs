//! Bounded readers for the object fields that `AssetStudio` uses to build its
//! display-name index.
//!
//! Unity does not put `m_Name` at a universal offset. In particular,
//! `GameObject`, `Animator`, and `MonoBehaviour` are not `NamedObject`s. Keep
//! the supported classes explicit so an unknown object is never interpreted as
//! a named object merely because its first four bytes look like a string size.

use crate::endian::{Endian, EndianReader, checked_length};
use crate::serialized::{ObjectReference, SerializedFile};
use crate::source::{Region, RegionCursor};
use crate::{Error, Result};

pub const GAME_OBJECT_CLASS_ID: i32 = 1;
pub const ANIMATOR_CLASS_ID: i32 = 95;
pub const MONO_BEHAVIOUR_CLASS_ID: i32 = 114;

const NO_TARGET_PLATFORM: i32 = -2;

/// Limits for the small prefix readers used while constructing object lookup
/// metadata. They are independent from export limits because lookup is built
/// eagerly when a collection is loaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectNameReadLimits {
    pub maximum_string_bytes: usize,
    pub maximum_game_object_components: usize,
}

impl Default for ObjectNameReadLimits {
    fn default() -> Self {
        Self {
            maximum_string_bytes: 16 * 1024 * 1024,
            maximum_game_object_components: 1_000_000,
        }
    }
}

/// Name-related fields read directly from one object. References are resolved
/// by `AssetCollection` only after every serialized file has been discovered.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ObjectNameMetadata {
    pub name: Option<String>,
    pub game_object: Option<ObjectReference>,
    pub mono_script: Option<ObjectReference>,
}

/// Reads only the class-specific prefix needed by the managed
/// `AssetStudioEngineInstance.ParseAssets` naming rules.
///
/// `Ok(None)` means the managed parser does not treat this class as one of the
/// explicitly supported named classes. Callers must use the managed fallback
/// name instead of guessing a `NamedObject` layout.
pub fn read_object_name_metadata(
    file: &SerializedFile,
    object_index: usize,
    limits: ObjectNameReadLimits,
) -> Result<Option<ObjectNameMetadata>> {
    let object = file.objects.get(object_index).ok_or_else(|| {
        Error::invalid_data(format!(
            "serialized object index {object_index} is out of range"
        ))
    })?;
    let mut reader = ObjectNameReader::new(file, object_index, limits)?;
    match object.class_id {
        GAME_OBJECT_CLASS_ID => {
            reader.read_editor_extension_prefix()?;
            let component_count = reader.read_count(
                "GameObject component",
                limits.maximum_game_object_components,
                if file.unity_version.components() < (5, 5, 0) {
                    4_usize.checked_add(reader.pptr_size()).ok_or_else(|| {
                        Error::invalid_data("GameObject component size overflowed")
                    })?
                } else {
                    reader.pptr_size()
                },
            )?;
            for _ in 0..component_count {
                if file.unity_version.components() < (5, 5, 0) {
                    reader.skip(4, "GameObject legacy component index")?;
                }
                let _component = reader.read_pptr()?;
            }
            reader.skip(4, "GameObject layer")?;
            if has_tuanjie_game_object_editor_info(file) {
                reader.skip(1, "GameObject Tuanjie editor-info flag")?;
                reader.align(4)?;
            }
            Ok(Some(ObjectNameMetadata {
                name: Some(reader.read_aligned_string("GameObject name")?),
                ..ObjectNameMetadata::default()
            }))
        }
        ANIMATOR_CLASS_ID => {
            reader.read_editor_extension_prefix()?;
            Ok(Some(ObjectNameMetadata {
                game_object: Some(reader.read_pptr()?),
                ..ObjectNameMetadata::default()
            }))
        }
        MONO_BEHAVIOUR_CLASS_ID => {
            reader.read_editor_extension_prefix()?;
            let game_object = reader.read_pptr()?;
            reader.skip(1, "MonoBehaviour enabled flag")?;
            reader.align(4)?;
            let mono_script = reader.read_pptr()?;
            Ok(Some(ObjectNameMetadata {
                name: Some(reader.read_aligned_string("MonoBehaviour name")?),
                game_object: Some(game_object),
                mono_script: Some(mono_script),
            }))
        }
        class_id if is_managed_named_object_class(class_id) => {
            reader.read_editor_extension_prefix()?;
            Ok(Some(ObjectNameMetadata {
                name: Some(reader.read_aligned_string("NamedObject name")?),
                ..ObjectNameMetadata::default()
            }))
        }
        _ => Ok(None),
    }
}

/// Classes for which the C# `AssetsManager` constructs a type derived from
/// `NamedObject`. This deliberately mirrors the parser switch, not Unity's
/// entire inheritance hierarchy.
const fn is_managed_named_object_class(class_id: i32) -> bool {
    matches!(
        class_id,
        21 // Material
            | 28 // Texture2D
            | 43 // Mesh
            | 48 // Shader
            | 49 // TextAsset
            | 74 // AnimationClip
            | 83 // AudioClip
            | 90 // Avatar
            | 91 // AnimatorController
            | 115 // MonoScript
            | 128 // Font
            | 142 // AssetBundle
            | 152 // MovieTexture
            | 187 // Texture2DArray
            | 213 // Sprite
            | 221 // AnimatorOverrideController
            | 329 // VideoClip
            | 687_078_895 // SpriteAtlas
    )
}

fn has_tuanjie_game_object_editor_info(file: &SerializedFile) -> bool {
    let version = &file.unity_version;
    version.is_tuanjie()
        && (version.components() > (2022, 3, 2)
            || (version.components() == (2022, 3, 2) && version.build >= 11))
}

struct ObjectNameReader {
    reader: EndianReader<RegionCursor>,
    region: Region,
    absolute_start: u64,
    target_platform: i32,
    format_version: u32,
    limits: ObjectNameReadLimits,
}

impl ObjectNameReader {
    fn new(
        file: &SerializedFile,
        object_index: usize,
        limits: ObjectNameReadLimits,
    ) -> Result<Self> {
        let object = file.objects.get(object_index).ok_or_else(|| {
            Error::invalid_data(format!(
                "serialized object index {object_index} is out of range"
            ))
        })?;
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
            target_platform: file.target_platform,
            format_version: file.header.version.0,
            limits,
        })
    }

    fn pptr_size(&self) -> usize {
        if self.format_version < 14 { 8 } else { 12 }
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
                "{field} needs at least {minimum_bytes} bytes beyond the bounded payload"
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
        let absolute = self
            .absolute_start
            .checked_add(self.reader.position()?)
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
    use crate::serialized::SerializedFile;
    use crate::source::Region;

    use super::{
        ANIMATOR_CLASS_ID, GAME_OBJECT_CLASS_ID, MONO_BEHAVIOUR_CLASS_ID, ObjectNameReadLimits,
        read_object_name_metadata,
    };

    #[test]
    fn reads_named_game_object_animator_and_mono_behaviour_prefixes() {
        let material = parse_v22(21, &aligned_string("Shared Material"));
        assert_eq!(
            read_object_name_metadata(&material, 0, ObjectNameReadLimits::default())
                .unwrap()
                .unwrap()
                .name
                .as_deref(),
            Some("Shared Material")
        );

        let mut game_object = Vec::new();
        push_i32(&mut game_object, 2);
        push_pptr(&mut game_object, 0, 10);
        push_pptr(&mut game_object, 1, 20);
        push_i32(&mut game_object, 7);
        push_aligned_string(&mut game_object, "Player");
        let game_object = parse_v22(GAME_OBJECT_CLASS_ID, &game_object);
        assert_eq!(
            read_object_name_metadata(&game_object, 0, ObjectNameReadLimits::default())
                .unwrap()
                .unwrap()
                .name
                .as_deref(),
            Some("Player")
        );

        let animator = parse_v22(ANIMATOR_CLASS_ID, &pptr(1, 42));
        assert_eq!(
            read_object_name_metadata(&animator, 0, ObjectNameReadLimits::default())
                .unwrap()
                .unwrap()
                .game_object
                .unwrap()
                .path_id,
            42
        );

        let mut behaviour = pptr(0, 7);
        behaviour.push(1);
        while !behaviour.len().is_multiple_of(4) {
            behaviour.push(0);
        }
        behaviour.extend_from_slice(&pptr(2, 99));
        push_aligned_string(&mut behaviour, "");
        let behaviour = parse_v22(MONO_BEHAVIOUR_CLASS_ID, &behaviour);
        let metadata = read_object_name_metadata(&behaviour, 0, ObjectNameReadLimits::default())
            .unwrap()
            .unwrap();
        assert_eq!(metadata.name.as_deref(), Some(""));
        assert_eq!(metadata.game_object.unwrap().path_id, 7);
        assert_eq!(metadata.mono_script.unwrap().file_id, 2);
    }

    #[test]
    fn rejects_negative_oversized_and_truncated_fields_within_limits() {
        let negative = parse_v22(GAME_OBJECT_CLASS_ID, &(-1_i32).to_le_bytes());
        assert!(read_object_name_metadata(&negative, 0, ObjectNameReadLimits::default()).is_err());

        let mut oversized = Vec::new();
        push_i32(&mut oversized, 2);
        let oversized = parse_v22(GAME_OBJECT_CLASS_ID, &oversized);
        let limits = ObjectNameReadLimits {
            maximum_game_object_components: 1,
            ..ObjectNameReadLimits::default()
        };
        assert!(read_object_name_metadata(&oversized, 0, limits).is_err());

        let named = parse_v22(21, &8_i32.to_le_bytes());
        let limits = ObjectNameReadLimits {
            maximum_string_bytes: 4,
            ..ObjectNameReadLimits::default()
        };
        assert!(read_object_name_metadata(&named, 0, limits).is_err());
    }

    #[test]
    fn unknown_classes_are_not_guessed_to_be_named_objects() {
        let file = parse_v22(364, &aligned_string("looks like a name"));
        assert_eq!(
            read_object_name_metadata(&file, 0, ObjectNameReadLimits::default()).unwrap(),
            None
        );

        // ParseAssets handles PreloadData before its NamedObject base and uses it only to update
        // the current preload table, so its display name is the managed fallback.
        let preload = parse_v22(150, &aligned_string("ignored preload name"));
        assert_eq!(
            read_object_name_metadata(&preload, 0, ObjectNameReadLimits::default()).unwrap(),
            None
        );
    }

    #[test]
    fn handles_no_target_prefix_and_legacy_pptr_width() {
        let mut no_target = Vec::new();
        no_target.extend_from_slice(&0x1234_u32.to_le_bytes());
        push_pptr(&mut no_target, 0, 0);
        push_pptr(&mut no_target, 0, 0);
        push_aligned_string(&mut no_target, "Editor Material");
        let no_target = parse_v22_with("2022.3.62f1", -2, 21, &no_target);
        assert_eq!(
            read_object_name_metadata(&no_target, 0, ObjectNameReadLimits::default())
                .unwrap()
                .unwrap()
                .name
                .as_deref(),
            Some("Editor Material")
        );

        let mut legacy_animator = Vec::new();
        push_i32(&mut legacy_animator, 3);
        push_i32(&mut legacy_animator, 0x1234_5678);
        let legacy_animator = parse_v13(ANIMATOR_CLASS_ID, &legacy_animator);
        let metadata =
            read_object_name_metadata(&legacy_animator, 0, ObjectNameReadLimits::default())
                .unwrap()
                .unwrap();
        let game_object = metadata.game_object.unwrap();
        assert_eq!(game_object.file_id, 3);
        assert_eq!(game_object.path_id, 0x1234_5678);
    }

    #[test]
    fn gates_tuanjie_game_object_editor_info_at_build_eleven() {
        let mut before = Vec::new();
        push_i32(&mut before, 0);
        push_i32(&mut before, 4);
        push_aligned_string(&mut before, "Before");
        let before = parse_v22_with("2022.3.2t10", 13, GAME_OBJECT_CLASS_ID, &before);
        assert_eq!(
            read_object_name_metadata(&before, 0, ObjectNameReadLimits::default())
                .unwrap()
                .unwrap()
                .name
                .as_deref(),
            Some("Before")
        );

        let mut after = Vec::new();
        push_i32(&mut after, 0);
        push_i32(&mut after, 4);
        after.push(1);
        while !after.len().is_multiple_of(4) {
            after.push(0);
        }
        push_aligned_string(&mut after, "After");
        let after = parse_v22_with("2022.3.2t11", 13, GAME_OBJECT_CLASS_ID, &after);
        assert_eq!(
            read_object_name_metadata(&after, 0, ObjectNameReadLimits::default())
                .unwrap()
                .unwrap()
                .name
                .as_deref(),
            Some("After")
        );
    }

    fn parse_v22(class_id: i32, object: &[u8]) -> SerializedFile {
        parse_v22_with("2022.3.62f1", 13, class_id, object)
    }

    fn parse_v22_with(
        unity_version: &str,
        target_platform: i32,
        class_id: i32,
        object: &[u8],
    ) -> SerializedFile {
        let mut metadata = Vec::new();
        metadata.extend_from_slice(unity_version.as_bytes());
        metadata.push(0);
        push_i32(&mut metadata, target_platform);
        metadata.push(0);
        push_i32(&mut metadata, 1);
        push_i32(&mut metadata, class_id);
        metadata.push(0);
        metadata.extend_from_slice(&(-1_i16).to_le_bytes());
        if class_id == MONO_BEHAVIOUR_CLASS_ID {
            metadata.extend_from_slice(&[0_u8; 16]);
        }
        metadata.extend_from_slice(&[0_u8; 16]);
        push_i32(&mut metadata, 1);
        while !(48 + metadata.len()).is_multiple_of(4) {
            metadata.push(0);
        }
        metadata.extend_from_slice(&7_i64.to_le_bytes());
        metadata.extend_from_slice(&0_i64.to_le_bytes());
        metadata.extend_from_slice(&u32::try_from(object.len()).unwrap().to_le_bytes());
        push_i32(&mut metadata, 0);
        for _ in 0..3 {
            push_i32(&mut metadata, 0);
        }
        metadata.push(0);

        let metadata_end = 48_u64 + u64::try_from(metadata.len()).unwrap();
        let data_offset = metadata_end.div_ceil(16) * 16;
        let file_size = data_offset + u64::try_from(object.len()).unwrap();
        let mut bytes = vec![0_u8; 48];
        bytes[8..12].copy_from_slice(&22_u32.to_be_bytes());
        bytes[20..24].copy_from_slice(&u32::try_from(metadata.len()).unwrap().to_be_bytes());
        bytes[24..32].copy_from_slice(&i64::try_from(file_size).unwrap().to_be_bytes());
        bytes[32..40].copy_from_slice(&i64::try_from(data_offset).unwrap().to_be_bytes());
        bytes.extend_from_slice(&metadata);
        bytes.resize(usize::try_from(data_offset).unwrap(), 0);
        bytes.extend_from_slice(object);
        SerializedFile::open(Region::from_bytes(bytes)).unwrap()
    }

    fn parse_v13(class_id: i32, object: &[u8]) -> SerializedFile {
        let mut metadata = Vec::new();
        metadata.extend_from_slice(b"5.4.0f1\0");
        push_i32(&mut metadata, 13);
        metadata.push(0); // type tree disabled
        push_i32(&mut metadata, 1);
        push_i32(&mut metadata, class_id);
        metadata.extend_from_slice(&[0_u8; 16]);
        push_i32(&mut metadata, 0); // big IDs disabled
        push_i32(&mut metadata, 1);
        push_i32(&mut metadata, 7);
        metadata.extend_from_slice(&0_u32.to_le_bytes());
        metadata.extend_from_slice(&u32::try_from(object.len()).unwrap().to_le_bytes());
        push_i32(&mut metadata, class_id);
        metadata.extend_from_slice(&u16::try_from(class_id).unwrap().to_le_bytes());
        metadata.extend_from_slice(&(-1_i16).to_le_bytes());
        push_i32(&mut metadata, 0); // script types
        push_i32(&mut metadata, 0); // externals
        metadata.push(0); // user information

        let data_offset = (20 + metadata.len()).div_ceil(16) * 16;
        let file_size = data_offset + object.len();
        let mut bytes = vec![0_u8; 20];
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

    fn aligned_string(value: &str) -> Vec<u8> {
        let mut output = Vec::new();
        push_aligned_string(&mut output, value);
        output
    }

    fn pptr(file_id: i32, path_id: i64) -> Vec<u8> {
        let mut output = Vec::new();
        push_pptr(&mut output, file_id, path_id);
        output
    }

    fn push_pptr(output: &mut Vec<u8>, file_id: i32, path_id: i64) {
        push_i32(output, file_id);
        output.extend_from_slice(&path_id.to_le_bytes());
    }

    fn push_aligned_string(output: &mut Vec<u8>, value: &str) {
        push_i32(output, i32::try_from(value.len()).unwrap());
        output.extend_from_slice(value.as_bytes());
        while !output.len().is_multiple_of(4) {
            output.push(0);
        }
    }

    fn push_i32(output: &mut Vec<u8>, value: i32) {
        output.extend_from_slice(&value.to_le_bytes());
    }
}
