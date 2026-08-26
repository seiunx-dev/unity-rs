//! Stable reference-only animation components.
//!
//! These readers cover the small engine-owned `Animation` and
//! `AnimatorOverrideController` layouts. They do not decode clip curves; the
//! dedicated `AnimationClip` reader owns that version-dependent data model.

use crate::endian::{Endian, EndianReader, checked_length};
use crate::scene::{BehaviourHeader, ComponentHeader, EditorExtensionHeader};
use crate::serialized::{ObjectReference, SerializedFile};
use crate::source::{Region, RegionCursor};
use crate::{Error, Result};

pub const ANIMATION_CLASS_ID: i32 = 111;
pub const ANIMATOR_OVERRIDE_CONTROLLER_CLASS_ID: i32 = 221;

const NO_TARGET_PLATFORM: i32 = -2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnimationComponentReadLimits {
    pub maximum_object_bytes: u64,
    pub maximum_string_bytes: usize,
    pub maximum_clips: usize,
    pub maximum_reference_bytes: u64,
}

impl Default for AnimationComponentReadLimits {
    fn default() -> Self {
        Self {
            maximum_object_bytes: 256 * 1024 * 1024,
            maximum_string_bytes: 16 * 1024 * 1024,
            maximum_clips: 1_000_000,
            maximum_reference_bytes: 256 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyAnimationComponent {
    pub path_id: i64,
    pub behaviour: BehaviourHeader,
    pub default_clip: ObjectReference,
    pub clips: Vec<ObjectReference>,
    pub trailing_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnimationClipOverride {
    pub original_clip: ObjectReference,
    pub override_clip: ObjectReference,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnimatorOverrideController {
    pub path_id: i64,
    pub name: String,
    pub controller: ObjectReference,
    pub clips: Vec<AnimationClipOverride>,
    pub trailing_bytes: u64,
}

pub fn read_legacy_animation_component(
    file: &SerializedFile,
    object_index: usize,
    limits: AnimationComponentReadLimits,
) -> Result<LegacyAnimationComponent> {
    let mut reader =
        AnimationObjectReader::new(file, object_index, ANIMATION_CLASS_ID, "Animation", limits)?;
    let behaviour = reader.read_behaviour()?;
    let default_clip = reader.read_pptr("Animation default clip")?;
    let count = reader.read_count("Animation clip", limits.maximum_clips, reader.pptr_size())?;
    reader.check_reference_budget(count, "Animation clips")?;
    let mut clips = reserve_vec(count, "Animation clips")?;
    for _ in 0..count {
        clips.push(reader.read_pptr("Animation clip")?);
    }
    let trailing_bytes = reader.reader.remaining()?;
    Ok(LegacyAnimationComponent {
        path_id: reader.path_id,
        behaviour,
        default_clip,
        clips,
        trailing_bytes,
    })
}

pub fn read_animator_override_controller(
    file: &SerializedFile,
    object_index: usize,
    limits: AnimationComponentReadLimits,
) -> Result<AnimatorOverrideController> {
    let mut reader = AnimationObjectReader::new(
        file,
        object_index,
        ANIMATOR_OVERRIDE_CONTROLLER_CLASS_ID,
        "AnimatorOverrideController",
        limits,
    )?;
    let name = reader.read_named_object()?;
    let controller = reader.read_pptr("AnimatorOverrideController controller")?;
    let pair_size = reader
        .pptr_size()
        .checked_mul(2)
        .ok_or_else(|| Error::invalid_data("animation override pair size overflowed"))?;
    let count = reader.read_count("animation clip override", limits.maximum_clips, pair_size)?;
    let reference_count = count
        .checked_mul(2)
        .ok_or_else(|| Error::invalid_data("animation override reference count overflowed"))?;
    reader.check_reference_budget(reference_count, "animation clip overrides")?;
    let mut clips = reserve_vec(count, "animation clip overrides")?;
    for _ in 0..count {
        clips.push(AnimationClipOverride {
            original_clip: reader.read_pptr("original animation clip")?,
            override_clip: reader.read_pptr("override animation clip")?,
        });
    }
    let trailing_bytes = reader.reader.remaining()?;
    Ok(AnimatorOverrideController {
        path_id: reader.path_id,
        name,
        controller,
        clips,
        trailing_bytes,
    })
}

struct AnimationObjectReader {
    region: Region,
    reader: EndianReader<RegionCursor>,
    absolute_start: u64,
    path_id: i64,
    target_platform: i32,
    format_version: u32,
    limits: AnimationComponentReadLimits,
    reference_bytes: u64,
}

impl AnimationObjectReader {
    fn new(
        file: &SerializedFile,
        object_index: usize,
        expected_class_id: i32,
        class_name: &str,
        limits: AnimationComponentReadLimits,
    ) -> Result<Self> {
        let object = file.objects.get(object_index).ok_or_else(|| {
            Error::invalid_data(format!(
                "serialized object index {object_index} is out of range"
            ))
        })?;
        if object.class_id != expected_class_id {
            return Err(Error::unsupported(format!(
                "object {} has class ID {}, expected {expected_class_id} ({class_name})",
                object.path_id, object.class_id
            )));
        }
        if object.byte_size > limits.maximum_object_bytes {
            return Err(Error::invalid_data(format!(
                "{class_name} object is {} bytes, exceeding limit {}",
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
            reference_bytes: 0,
        })
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
            game_object: self.read_pptr("Animation component GameObject")?,
        })
    }

    fn read_behaviour(&mut self) -> Result<BehaviourHeader> {
        let component = self.read_component()?;
        let enabled = self.reader.read_u8()?;
        self.align(4)?;
        Ok(BehaviourHeader { component, enabled })
    }

    fn read_named_object(&mut self) -> Result<String> {
        let _editor_extension = self.read_editor_extension()?;
        self.read_aligned_string("AnimatorOverrideController name")
    }

    fn read_pptr(&mut self, field: &str) -> Result<ObjectReference> {
        let pptr_size = u64::try_from(self.pptr_size())
            .map_err(|_| Error::invalid_data("animation PPtr size does not fit in u64"))?;
        self.reference_bytes = self
            .reference_bytes
            .checked_add(pptr_size)
            .ok_or_else(|| Error::invalid_data("animation reference byte budget overflowed"))?;
        if self.reference_bytes > self.limits.maximum_reference_bytes {
            return Err(Error::invalid_data(format!(
                "animation references total {} bytes while reading {field}, exceeding limit {}",
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

    fn read_count(&mut self, field: &str, maximum: usize, element_size: usize) -> Result<usize> {
        let count = checked_length(self.reader.read_i32()?, field)?;
        if count > maximum {
            return Err(Error::invalid_data(format!(
                "{field} count {count} exceeds limit {maximum}"
            )));
        }
        let bytes = count
            .checked_mul(element_size)
            .ok_or_else(|| Error::invalid_data(format!("{field} byte size overflowed")))?;
        self.ensure_remaining(bytes, field)?;
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
            .ok_or_else(|| Error::invalid_data("animation reference byte budget overflowed"))?;
        if total > self.limits.maximum_reference_bytes {
            return Err(Error::invalid_data(format!(
                "animation references would total {total} bytes while reading {field}, exceeding limit {}",
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

    fn align(&mut self, alignment: u64) -> Result<()> {
        let position = self.reader.position()?;
        let absolute = self
            .absolute_start
            .checked_add(position)
            .ok_or_else(|| Error::invalid_data("animation alignment position overflowed"))?;
        let remainder = absolute % alignment;
        if remainder == 0 {
            return Ok(());
        }
        self.skip(alignment - remainder, "animation alignment")
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

fn reserve_vec<T>(count: usize, field: &str) -> Result<Vec<T>> {
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

    use super::{
        ANIMATION_CLASS_ID, ANIMATOR_OVERRIDE_CONTROLLER_CLASS_ID, AnimationComponentReadLimits,
        read_animator_override_controller, read_legacy_animation_component,
    };

    #[test]
    fn reads_animation_component_and_override_controller_references() {
        let mut animation = Vec::new();
        push_pptr(&mut animation, 0, 1);
        animation.push(1);
        align(&mut animation, 4);
        push_pptr(&mut animation, 0, 70);
        animation.extend_from_slice(&2_i32.to_le_bytes());
        push_pptr(&mut animation, 0, 71);
        push_pptr(&mut animation, 1, 72);

        let mut override_controller = Vec::new();
        push_aligned_string(&mut override_controller, "night controller");
        push_pptr(&mut override_controller, 0, 90);
        override_controller.extend_from_slice(&2_i32.to_le_bytes());
        push_pptr(&mut override_controller, 0, 71);
        push_pptr(&mut override_controller, 0, 73);
        push_pptr(&mut override_controller, 1, 72);
        push_pptr(&mut override_controller, 0, 74);

        let file = parse_v22(&[
            (ANIMATION_CLASS_ID, 7, animation),
            (
                ANIMATOR_OVERRIDE_CONTROLLER_CLASS_ID,
                8,
                override_controller,
            ),
        ]);
        let animation =
            read_legacy_animation_component(&file, 0, AnimationComponentReadLimits::default())
                .unwrap();
        assert_eq!(animation.path_id, 7);
        assert_eq!(animation.behaviour.component.game_object.path_id, 1);
        assert_eq!(animation.default_clip.path_id, 70);
        assert_eq!(animation.clips[0].path_id, 71);
        assert_eq!(animation.clips[1].file_id, 1);
        assert_eq!(animation.trailing_bytes, 0);

        let controller =
            read_animator_override_controller(&file, 1, AnimationComponentReadLimits::default())
                .unwrap();
        assert_eq!(controller.path_id, 8);
        assert_eq!(controller.name, "night controller");
        assert_eq!(controller.controller.path_id, 90);
        assert_eq!(controller.clips[0].original_clip.path_id, 71);
        assert_eq!(controller.clips[0].override_clip.path_id, 73);
        assert_eq!(controller.clips[1].original_clip.file_id, 1);
        assert_eq!(controller.clips[1].override_clip.path_id, 74);
        assert_eq!(controller.trailing_bytes, 0);
    }

    #[test]
    fn reads_big_endian_v13_no_target_prefix_with_absolute_alignment() {
        let animation_file = parse_v13_big_endian_no_target(ANIMATION_CLASS_ID, |object_start| {
            let mut object = Vec::new();
            push_be_u32(&mut object, 0x1020_3040);
            push_be_pptr32(&mut object, 2, 0x1122_3344);
            push_be_pptr32(&mut object, 3, -7);
            push_be_pptr32(&mut object, 0, 41);
            object.push(1);
            align_with_base(&mut object, object_start, 4);
            push_be_pptr32(&mut object, 0, 70);
            push_be_i32(&mut object, 2);
            push_be_pptr32(&mut object, 0, 71);
            push_be_pptr32(&mut object, 1, 72);
            object
        });

        let animation = read_legacy_animation_component(
            &animation_file,
            0,
            AnimationComponentReadLimits::default(),
        )
        .unwrap();
        let editor = animation.behaviour.component.editor_extension;
        assert_eq!(editor.object_hide_flags, Some(0x1020_3040));
        assert_eq!(editor.prefab_parent_object.unwrap().file_id, 2);
        assert_eq!(editor.prefab_parent_object.unwrap().path_id, 0x1122_3344);
        assert_eq!(editor.prefab_internal.unwrap().path_id, -7);
        assert_eq!(animation.behaviour.component.game_object.path_id, 41);
        assert_eq!(animation.behaviour.enabled, 1);
        assert_eq!(animation.default_clip.path_id, 70);
        assert_eq!(animation.clips[0].path_id, 71);
        assert_eq!(animation.clips[1].file_id, 1);
        assert_eq!(animation.clips[1].path_id, 72);
        assert_eq!(animation.trailing_bytes, 0);

        let controller_file =
            parse_v13_big_endian_no_target(ANIMATOR_OVERRIDE_CONTROLLER_CLASS_ID, |object_start| {
                let mut object = Vec::new();
                push_be_u32(&mut object, 0xa0b0_c0d0);
                push_be_pptr32(&mut object, 0, 5);
                push_be_pptr32(&mut object, 0, 6);
                push_be_aligned_string(&mut object, object_start, "old");
                push_be_pptr32(&mut object, 0, 90);
                push_be_i32(&mut object, 1);
                push_be_pptr32(&mut object, 0, 71);
                push_be_pptr32(&mut object, 0, 73);
                object
            });

        let controller = read_animator_override_controller(
            &controller_file,
            0,
            AnimationComponentReadLimits::default(),
        )
        .unwrap();
        assert_eq!(controller.name, "old");
        assert_eq!(controller.controller.path_id, 90);
        assert_eq!(controller.clips[0].original_clip.path_id, 71);
        assert_eq!(controller.clips[0].override_clip.path_id, 73);
        assert_eq!(controller.trailing_bytes, 0);
    }

    #[test]
    fn rejects_wrong_classes_truncation_and_all_limits() {
        let mut animation = Vec::new();
        push_pptr(&mut animation, 0, 1);
        animation.push(1);
        align(&mut animation, 4);
        push_pptr(&mut animation, 0, 70);
        animation.extend_from_slice(&1_i32.to_le_bytes());
        push_pptr(&mut animation, 0, 71);
        let file = parse_v22(&[(ANIMATION_CLASS_ID, 7, animation.clone())]);

        let object_limit = AnimationComponentReadLimits {
            maximum_object_bytes: u64::try_from(animation.len() - 1).unwrap(),
            ..AnimationComponentReadLimits::default()
        };
        assert!(read_legacy_animation_component(&file, 0, object_limit).is_err());
        let count_limit = AnimationComponentReadLimits {
            maximum_clips: 0,
            ..AnimationComponentReadLimits::default()
        };
        assert!(read_legacy_animation_component(&file, 0, count_limit).is_err());
        let reference_limit = AnimationComponentReadLimits {
            maximum_reference_bytes: 24,
            ..AnimationComponentReadLimits::default()
        };
        assert!(read_legacy_animation_component(&file, 0, reference_limit).is_err());

        let truncated = parse_v22(&[(
            ANIMATION_CLASS_ID,
            7,
            animation[..animation.len() - 1].to_vec(),
        )]);
        assert!(
            read_legacy_animation_component(&truncated, 0, AnimationComponentReadLimits::default())
                .is_err()
        );
        assert!(
            read_animator_override_controller(&file, 0, AnimationComponentReadLimits::default())
                .is_err()
        );

        let mut controller = Vec::new();
        push_aligned_string(&mut controller, "long name");
        push_pptr(&mut controller, 0, 90);
        controller.extend_from_slice(&0_i32.to_le_bytes());
        let file = parse_v22(&[(ANIMATOR_OVERRIDE_CONTROLLER_CLASS_ID, 8, controller)]);
        let string_limit = AnimationComponentReadLimits {
            maximum_string_bytes: 4,
            ..AnimationComponentReadLimits::default()
        };
        assert!(read_animator_override_controller(&file, 0, string_limit).is_err());
    }

    fn parse_v22(objects: &[(i32, i64, Vec<u8>)]) -> SerializedFile {
        SerializedFile::open(Region::from_bytes(synthetic_v22(objects))).unwrap()
    }

    fn parse_v13_big_endian_no_target(
        class_id: i32,
        build_object: impl FnOnce(usize) -> Vec<u8>,
    ) -> SerializedFile {
        let probe = synthetic_v13_big_endian_no_target(class_id, &[]);
        let object_start =
            usize::try_from(u32::from_be_bytes(probe[12..16].try_into().unwrap())).unwrap();
        assert!(!object_start.is_multiple_of(4));
        let object = build_object(object_start);
        SerializedFile::open(Region::from_bytes(synthetic_v13_big_endian_no_target(
            class_id, &object,
        )))
        .unwrap()
    }

    fn synthetic_v13_big_endian_no_target(class_id: i32, object: &[u8]) -> Vec<u8> {
        let mut metadata = Vec::new();
        metadata.extend_from_slice(b"5.0.0f1\0");
        push_be_i32(&mut metadata, -2);
        metadata.push(0);
        push_be_i32(&mut metadata, 1);
        push_be_i32(&mut metadata, class_id);
        metadata.extend_from_slice(&[0_u8; 16]);
        push_be_i32(&mut metadata, 0);
        push_be_i32(&mut metadata, 1);
        push_be_i32(&mut metadata, 7);
        push_be_u32(&mut metadata, 0);
        push_be_u32(&mut metadata, u32::try_from(object.len()).unwrap());
        push_be_i32(&mut metadata, class_id);
        metadata.extend_from_slice(&u16::try_from(class_id).unwrap().to_be_bytes());
        metadata.extend_from_slice(&(-1_i16).to_be_bytes());
        push_be_i32(&mut metadata, 0);
        push_be_i32(&mut metadata, 0);
        metadata.push(0);

        let metadata_size = u32::try_from(metadata.len()).unwrap();
        let data_offset = 20 + metadata.len();
        let file_size = data_offset + object.len();
        let mut bytes = vec![0_u8; 20];
        bytes[0..4].copy_from_slice(&metadata_size.to_be_bytes());
        bytes[4..8].copy_from_slice(&u32::try_from(file_size).unwrap().to_be_bytes());
        bytes[8..12].copy_from_slice(&13_u32.to_be_bytes());
        bytes[12..16].copy_from_slice(&u32::try_from(data_offset).unwrap().to_be_bytes());
        bytes[16] = 1;
        bytes.extend_from_slice(&metadata);
        bytes.extend_from_slice(object);
        bytes
    }

    fn synthetic_v22(objects: &[(i32, i64, Vec<u8>)]) -> Vec<u8> {
        let mut classes = Vec::new();
        for (class_id, _, _) in objects {
            if !classes.contains(class_id) {
                classes.push(*class_id);
            }
        }
        let mut metadata = Vec::new();
        metadata.extend_from_slice(b"2022.3.62f1\0");
        metadata.extend_from_slice(&13_i32.to_le_bytes());
        metadata.push(0);
        metadata.extend_from_slice(&i32::try_from(classes.len()).unwrap().to_le_bytes());
        for class_id in &classes {
            metadata.extend_from_slice(&class_id.to_le_bytes());
            metadata.push(0);
            metadata.extend_from_slice(&(-1_i16).to_le_bytes());
            metadata.extend_from_slice(&[0_u8; 16]);
        }
        let mut data = Vec::new();
        let mut records = Vec::new();
        for (class_id, path_id, object) in objects {
            align(&mut data, 4);
            let offset = i64::try_from(data.len()).unwrap();
            let type_index = classes.iter().position(|value| value == class_id).unwrap();
            records.push((*path_id, offset, object.len(), type_index));
            data.extend_from_slice(object);
        }
        metadata.extend_from_slice(&i32::try_from(records.len()).unwrap().to_le_bytes());
        for (path_id, offset, length, type_index) in records {
            align_with_base(&mut metadata, 48, 4);
            metadata.extend_from_slice(&path_id.to_le_bytes());
            metadata.extend_from_slice(&offset.to_le_bytes());
            metadata.extend_from_slice(&u32::try_from(length).unwrap().to_le_bytes());
            metadata.extend_from_slice(&i32::try_from(type_index).unwrap().to_le_bytes());
        }
        for _ in 0..3 {
            metadata.extend_from_slice(&0_i32.to_le_bytes());
        }
        metadata.push(0);
        let metadata_size = u32::try_from(metadata.len()).unwrap();
        let data_offset = (48 + metadata.len()).next_multiple_of(16);
        let file_size = data_offset + data.len();
        let mut bytes = vec![0_u8; 48];
        bytes[8..12].copy_from_slice(&22_u32.to_be_bytes());
        bytes[20..24].copy_from_slice(&metadata_size.to_be_bytes());
        bytes[24..32].copy_from_slice(&i64::try_from(file_size).unwrap().to_be_bytes());
        bytes[32..40].copy_from_slice(&i64::try_from(data_offset).unwrap().to_be_bytes());
        bytes.extend_from_slice(&metadata);
        bytes.resize(data_offset, 0);
        bytes.extend_from_slice(&data);
        bytes
    }

    fn push_pptr(output: &mut Vec<u8>, file_id: i32, path_id: i64) {
        output.extend_from_slice(&file_id.to_le_bytes());
        output.extend_from_slice(&path_id.to_le_bytes());
    }

    fn push_be_i32(output: &mut Vec<u8>, value: i32) {
        output.extend_from_slice(&value.to_be_bytes());
    }

    fn push_be_u32(output: &mut Vec<u8>, value: u32) {
        output.extend_from_slice(&value.to_be_bytes());
    }

    fn push_be_pptr32(output: &mut Vec<u8>, file_id: i32, path_id: i32) {
        push_be_i32(output, file_id);
        push_be_i32(output, path_id);
    }

    fn push_be_aligned_string(output: &mut Vec<u8>, base: usize, value: &str) {
        push_be_i32(output, i32::try_from(value.len()).unwrap());
        output.extend_from_slice(value.as_bytes());
        if !value.is_empty() {
            align_with_base(output, base, 4);
        }
    }

    fn push_aligned_string(output: &mut Vec<u8>, value: &str) {
        output.extend_from_slice(&i32::try_from(value.len()).unwrap().to_le_bytes());
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

    fn align_with_base(output: &mut Vec<u8>, base: usize, alignment: usize) {
        while !(base + output.len()).is_multiple_of(alignment) {
            output.push(0);
        }
    }
}
