//! Bounded readers for Unity build and player identity settings.

use crate::endian::{Endian, EndianReader, checked_length};
use crate::serialized::SerializedFile;
use crate::source::{Region, RegionCursor};
use crate::{Error, Result};

pub const PLAYER_SETTINGS_CLASS_ID: i32 = 129;
pub const BUILD_SETTINGS_CLASS_ID: i32 = 141;

const NO_TARGET_PLATFORM: i32 = -2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProjectSettingsReadLimits {
    pub maximum_object_bytes: u64,
    pub maximum_scene_paths: usize,
    pub maximum_string_bytes: usize,
    pub maximum_total_string_bytes: usize,
}

impl Default for ProjectSettingsReadLimits {
    fn default() -> Self {
        Self {
            maximum_object_bytes: 256 * 1024 * 1024,
            maximum_scene_paths: 1_000_000,
            maximum_string_bytes: 16 * 1024 * 1024,
            maximum_total_string_bytes: 256 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildSettings {
    pub path_id: i64,
    pub levels: Option<Vec<String>>,
    pub scenes: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerSettings {
    pub path_id: i64,
    pub company_name: String,
    pub product_name: String,
}

pub fn read_build_settings(
    file: &SerializedFile,
    object_index: usize,
    limits: ProjectSettingsReadLimits,
) -> Result<BuildSettings> {
    let version = require_version(file, "BuildSettings")?;
    let mut reader = SettingsReader::new(
        file,
        object_index,
        BUILD_SETTINGS_CLASS_ID,
        "BuildSettings",
        limits,
    )?;
    reader.read_object_prefix()?;
    let paths = reader.read_string_array("BuildSettings scene paths")?;
    if version < (5, 1, 0) {
        Ok(BuildSettings {
            path_id: reader.path_id,
            levels: Some(paths),
            scenes: None,
        })
    } else {
        Ok(BuildSettings {
            path_id: reader.path_id,
            levels: None,
            scenes: Some(paths),
        })
    }
}

pub fn read_player_settings(
    file: &SerializedFile,
    object_index: usize,
    limits: ProjectSettingsReadLimits,
) -> Result<PlayerSettings> {
    let version = require_version(file, "PlayerSettings")?;
    let mut reader = SettingsReader::new(
        file,
        object_index,
        PLAYER_SETTINGS_CLASS_ID,
        "PlayerSettings",
        limits,
    )?;
    reader.read_object_prefix()?;
    if version >= (3, 0, 0) {
        reader.read_player_platform_prefix(version)?;
    }
    let company_name = reader.read_aligned_string("PlayerSettings company name")?;
    let product_name = reader.read_aligned_string("PlayerSettings product name")?;
    Ok(PlayerSettings {
        path_id: reader.path_id,
        company_name,
        product_name,
    })
}

fn require_version(file: &SerializedFile, class_name: &str) -> Result<(u32, u32, u32)> {
    if file.unity_version.is_stripped() {
        return Err(Error::unsupported(format!(
            "{class_name} layout requires a Unity version"
        )));
    }
    Ok(file.unity_version.components())
}

struct SettingsReader {
    reader: EndianReader<RegionCursor>,
    region: Region,
    absolute_start: u64,
    path_id: i64,
    target_platform: i32,
    format_version: u32,
    limits: ProjectSettingsReadLimits,
    total_string_bytes: usize,
}

impl SettingsReader {
    fn new(
        file: &SerializedFile,
        object_index: usize,
        expected_class_id: i32,
        class_name: &str,
        limits: ProjectSettingsReadLimits,
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
            total_string_bytes: 0,
        })
    }

    fn read_object_prefix(&mut self) -> Result<()> {
        if self.target_platform == NO_TARGET_PLATFORM {
            self.skip(4, "Object hide flags")?;
            self.skip_pptr("Object prefab parent")?;
            self.skip_pptr("Object prefab internal")?;
        }
        Ok(())
    }

    fn read_player_platform_prefix(&mut self, version: (u32, u32, u32)) -> Result<()> {
        if version >= (5, 4, 0) {
            self.skip(16, "PlayerSettings product GUID")?;
        }
        let _android_profiler = self.reader.read_bool()?;
        self.align(4)?;
        let _default_screen_orientation = self.reader.read_i32()?;
        let _target_device = self.reader.read_i32()?;
        if version < (5, 3, 0) {
            if version < (5, 0, 0) {
                let _target_platform = self.reader.read_i32()?;
                if version >= (4, 6, 0) {
                    let _target_ios_graphics = self.reader.read_i32()?;
                }
            }
            let _target_resolution = self.reader.read_i32()?;
        } else {
            let _use_on_demand_resources = self.reader.read_bool()?;
            self.align(4)?;
        }
        if version >= (3, 5, 0) {
            let _accelerometer_frequency = self.reader.read_i32()?;
        }
        Ok(())
    }

    fn read_string_array(&mut self, field: &str) -> Result<Vec<String>> {
        let count = checked_length(self.reader.read_i32()?, field)?;
        if count > self.limits.maximum_scene_paths {
            return Err(Error::invalid_data(format!(
                "{field} has {count} entries, exceeding limit {}",
                self.limits.maximum_scene_paths
            )));
        }
        let minimum = u64::try_from(count)
            .ok()
            .and_then(|count| count.checked_mul(4))
            .ok_or_else(|| Error::invalid_data(format!("{field} minimum size overflowed")))?;
        if minimum > self.remaining()? {
            return Err(Error::invalid_data(format!(
                "{field} count {count} cannot fit in the remaining object bytes"
            )));
        }
        let mut paths = Vec::new();
        paths
            .try_reserve_exact(count)
            .map_err(|error| Error::invalid_data(format!("cannot allocate {field}: {error}")))?;
        for _ in 0..count {
            paths.push(self.read_aligned_string(field)?);
        }
        Ok(paths)
    }

    fn read_aligned_string(&mut self, field: &str) -> Result<String> {
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
            .ok_or_else(|| Error::invalid_data("project settings string budget overflowed"))?;
        if self.total_string_bytes > self.limits.maximum_total_string_bytes {
            return Err(Error::invalid_data(format!(
                "project settings strings use {} bytes, exceeding limit {}",
                self.total_string_bytes, self.limits.maximum_total_string_bytes
            )));
        }
        let value = self.reader.read_utf8(length)?;
        if length != 0 {
            self.align(4)?;
        }
        Ok(value)
    }

    fn skip_pptr(&mut self, field: &str) -> Result<()> {
        let length = if self.format_version < 14 { 8 } else { 12 };
        self.skip(length, field)
    }

    fn skip(&mut self, length: u64, field: &str) -> Result<()> {
        let position = self.reader.position()?;
        let end = position
            .checked_add(length)
            .ok_or_else(|| Error::invalid_data(format!("{field} range overflowed")))?;
        if end > self.region.len() {
            return Err(Error::invalid_data(format!(
                "{field} ends at {end}, beyond object size {}",
                self.region.len()
            )));
        }
        self.reader.set_position(end)?;
        Ok(())
    }

    fn align(&mut self, alignment: u64) -> Result<()> {
        let position = self.reader.position()?;
        let absolute = self
            .absolute_start
            .checked_add(position)
            .ok_or_else(|| Error::invalid_data("project settings alignment overflowed"))?;
        let aligned = absolute
            .checked_add(alignment - 1)
            .map(|value| value / alignment * alignment)
            .ok_or_else(|| Error::invalid_data("project settings alignment overflowed"))?;
        let relative = aligned
            .checked_sub(self.absolute_start)
            .ok_or_else(|| Error::invalid_data("project settings alignment underflowed"))?;
        if relative > self.region.len() {
            return Err(Error::invalid_data(
                "project settings alignment exceeds object payload",
            ));
        }
        self.reader.set_position(relative)?;
        Ok(())
    }

    fn remaining(&mut self) -> Result<u64> {
        self.region
            .len()
            .checked_sub(self.reader.position()?)
            .ok_or_else(|| Error::invalid_data("project settings cursor is outside object"))
    }
}

#[cfg(test)]
mod tests {
    use crate::endian::{Endian, EndianReader};
    use crate::source::Region;

    use super::{ProjectSettingsReadLimits, SettingsReader};

    #[test]
    fn reads_legacy_build_levels_and_modern_scene_arrays_with_absolute_alignment() {
        for (format_version, absolute_start, endian) in [
            (13_u32, 3_u64, Endian::Big),
            (22_u32, 0_u64, Endian::Little),
        ] {
            let mut bytes = Vec::new();
            push_object_prefix(&mut bytes, absolute_start, format_version, endian);
            push_i32(&mut bytes, 2, endian);
            push_string(&mut bytes, "Assets/A.unity", absolute_start, endian);
            push_string(&mut bytes, "Assets/世界.unity", absolute_start, endian);
            let mut reader = test_reader(bytes, absolute_start, format_version, endian);
            reader.read_object_prefix().unwrap();
            let paths = reader.read_string_array("scenes").unwrap();
            assert_eq!(paths, ["Assets/A.unity", "Assets/世界.unity"]);
            assert_eq!(reader.remaining().unwrap(), 0);
        }
    }

    #[test]
    fn follows_player_settings_version_gates_and_reads_identity_strings() {
        for version in [(2, 6, 0), (3, 0, 0), (4, 6, 0), (5, 4, 0), (2022, 3, 0)] {
            let endian = if version == (4, 6, 0) {
                Endian::Big
            } else {
                Endian::Little
            };
            let absolute_start = 5;
            let mut bytes = Vec::new();
            push_object_prefix(&mut bytes, absolute_start, 22, endian);
            push_player_prefix(&mut bytes, absolute_start, version, endian);
            push_string(&mut bytes, "Haruki", absolute_start, endian);
            push_string(&mut bytes, "Asset Studio", absolute_start, endian);
            bytes.extend_from_slice(&[0xaa, 0xbb]);
            let mut reader = test_reader(bytes, absolute_start, 22, endian);
            reader.read_object_prefix().unwrap();
            if version >= (3, 0, 0) {
                reader.read_player_platform_prefix(version).unwrap();
            }
            assert_eq!(reader.read_aligned_string("company").unwrap(), "Haruki");
            assert_eq!(
                reader.read_aligned_string("product").unwrap(),
                "Asset Studio"
            );
            assert_eq!(reader.remaining().unwrap(), 2);
        }
    }

    #[test]
    fn rejects_negative_truncated_and_over_budget_arrays_and_strings() {
        let mut negative = test_reader((-1_i32).to_le_bytes().to_vec(), 0, 22, Endian::Little);
        assert!(negative.read_string_array("scenes").is_err());

        let mut truncated = test_reader(2_i32.to_le_bytes().to_vec(), 0, 22, Endian::Little);
        assert!(truncated.read_string_array("scenes").is_err());

        let bytes = [
            1_i32.to_le_bytes().as_slice(),
            5_i32.to_le_bytes().as_slice(),
            b"hello",
        ]
        .concat();
        let mut limited = test_reader(bytes, 0, 22, Endian::Little);
        limited.limits.maximum_string_bytes = 4;
        assert!(limited.read_string_array("scenes").is_err());

        let mut count_limited = test_reader(1_i32.to_le_bytes().to_vec(), 0, 22, Endian::Little);
        count_limited.limits.maximum_scene_paths = 0;
        assert!(count_limited.read_string_array("scenes").is_err());
    }

    fn test_reader(
        bytes: Vec<u8>,
        absolute_start: u64,
        format_version: u32,
        endian: Endian,
    ) -> SettingsReader {
        let region = Region::from_bytes(bytes);
        SettingsReader {
            reader: EndianReader::new(region.cursor(), endian),
            region,
            absolute_start,
            path_id: 7,
            target_platform: -2,
            format_version,
            limits: ProjectSettingsReadLimits::default(),
            total_string_bytes: 0,
        }
    }

    fn push_object_prefix(
        output: &mut Vec<u8>,
        _absolute_start: u64,
        format_version: u32,
        endian: Endian,
    ) {
        push_u32(output, 0x1234_5678, endian);
        push_i32(output, 0, endian);
        push_path_id(output, 0, format_version, endian);
        push_i32(output, 0, endian);
        push_path_id(output, 0, format_version, endian);
    }

    fn push_player_prefix(
        output: &mut Vec<u8>,
        absolute_start: u64,
        version: (u32, u32, u32),
        endian: Endian,
    ) {
        if version < (3, 0, 0) {
            return;
        }
        if version >= (5, 4, 0) {
            output.extend_from_slice(&[0x55; 16]);
        }
        output.push(1);
        align(output, absolute_start, 4);
        push_i32(output, 1, endian);
        push_i32(output, 2, endian);
        if version < (5, 3, 0) {
            if version < (5, 0, 0) {
                push_i32(output, 3, endian);
                if version >= (4, 6, 0) {
                    push_i32(output, 4, endian);
                }
            }
            push_i32(output, 5, endian);
        } else {
            output.push(1);
            align(output, absolute_start, 4);
        }
        if version >= (3, 5, 0) {
            push_i32(output, 60, endian);
        }
    }

    fn push_string(output: &mut Vec<u8>, value: &str, absolute_start: u64, endian: Endian) {
        push_i32(output, i32::try_from(value.len()).unwrap(), endian);
        output.extend_from_slice(value.as_bytes());
        if !value.is_empty() {
            align(output, absolute_start, 4);
        }
    }

    fn push_path_id(output: &mut Vec<u8>, value: i64, format_version: u32, endian: Endian) {
        if format_version < 14 {
            push_i32(output, i32::try_from(value).unwrap(), endian);
        } else {
            let bytes = match endian {
                Endian::Little => value.to_le_bytes(),
                Endian::Big => value.to_be_bytes(),
            };
            output.extend_from_slice(&bytes);
        }
    }

    fn push_i32(output: &mut Vec<u8>, value: i32, endian: Endian) {
        let bytes = match endian {
            Endian::Little => value.to_le_bytes(),
            Endian::Big => value.to_be_bytes(),
        };
        output.extend_from_slice(&bytes);
    }

    fn push_u32(output: &mut Vec<u8>, value: u32, endian: Endian) {
        let bytes = match endian {
            Endian::Little => value.to_le_bytes(),
            Endian::Big => value.to_be_bytes(),
        };
        output.extend_from_slice(&bytes);
    }

    fn align(output: &mut Vec<u8>, absolute_start: u64, alignment: usize) {
        while (usize::try_from(absolute_start).unwrap() + output.len()) % alignment != 0 {
            output.push(0);
        }
    }
}
