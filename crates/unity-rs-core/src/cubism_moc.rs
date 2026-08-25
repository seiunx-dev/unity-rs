//! Source-bound extraction and bounded metadata inspection for Cubism `.moc3`
//! payloads stored by the `CubismMoc` `MonoBehaviour`.

use std::borrow::Cow;
use std::io::Write;

use crate::endian::{Endian, EndianReader};
use crate::loader::AssetCollection;
use crate::monobehaviour::{MONO_SCRIPT_CLASS_ID, read_mono_script};
use crate::monobehaviour::{MonoBehaviourReadLimits, read_mono_behaviour_with_script_data};
use crate::scene::resolve_object_reference;
use crate::serialized::{ObjectReference, SerializedFile};
use crate::source::Region;
use crate::{Error, Result};

const MOC3_MAGIC: &[u8; 4] = b"MOC3";
const MINIMUM_MOC3_HEADER_BYTES: u64 = 268;
const IDENTIFIER_BYTES: usize = 64;
const IDENTIFIER_BYTES_U64: u64 = 64;

/// Cubism SDK generation encoded in the MOC3 header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CubismSdkVersion {
    V30,
    V33,
    V40,
    V42,
    V50,
    Unknown(u8),
}

impl CubismSdkVersion {
    #[must_use]
    pub fn description(self) -> Cow<'static, str> {
        match self {
            Self::V30 => Cow::Borrowed("SDK3.0/Cubism3.0(3.2)"),
            Self::V33 => Cow::Borrowed("SDK3.3/Cubism3.3"),
            Self::V40 => Cow::Borrowed("SDK4.0/Cubism4.0"),
            Self::V42 => Cow::Borrowed("SDK4.2/Cubism4.2"),
            Self::V50 => Cow::Borrowed("SDK5.0/Cubism5.0"),
            Self::Unknown(value) => Cow::Owned(format!("Unknown SDK version ({value})")),
        }
    }

    #[must_use]
    pub const fn from_byte(value: u8) -> Self {
        match value {
            1 => Self::V30,
            2 => Self::V33,
            3 => Self::V40,
            4 => Self::V42,
            5 => Self::V50,
            _ => Self::Unknown(value),
        }
    }
}

/// Budgets for the `MonoBehaviour` prefix, MOC3 payload, and fixed identifier
/// tables. The binary payload itself remains source-bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CubismMocReadLimits {
    pub maximum_model_bytes: u64,
    pub maximum_part_count: usize,
    pub maximum_parameter_count: usize,
    pub maximum_total_identifier_bytes: u64,
    pub mono_behaviour: MonoBehaviourReadLimits,
}

impl Default for CubismMocReadLimits {
    fn default() -> Self {
        Self {
            maximum_model_bytes: 512 * 1024 * 1024,
            maximum_part_count: 1_000_000,
            maximum_parameter_count: 1_000_000,
            maximum_total_identifier_bytes: 128 * 1024 * 1024,
            mono_behaviour: MonoBehaviourReadLimits::default(),
        }
    }
}

/// Verified MOC3 header metadata together with its exact source-bound bytes.
#[derive(Debug, Clone, PartialEq)]
pub struct CubismMoc {
    pub path_id: i64,
    pub name: String,
    pub sdk_version: CubismSdkVersion,
    pub is_big_endian: bool,
    pub canvas_width: f32,
    pub canvas_height: f32,
    pub central_position_x: f32,
    pub central_position_y: f32,
    pub pixels_per_unit: f32,
    pub part_count: u32,
    pub parameter_count: u32,
    pub part_names: Vec<String>,
    pub parameter_names: Vec<String>,
    pub model_data: Region,
}

impl CubismMoc {
    /// Streams the exact MOC3 bytes after rechecking the caller's output
    /// budget. No second full-size model buffer is allocated.
    pub fn write_moc3<W: Write>(&self, output: &mut W, maximum_output_bytes: u64) -> Result<u64> {
        if self.model_data.len() > maximum_output_bytes {
            return Err(Error::invalid_data(format!(
                "Cubism MOC3 payload is {} bytes, exceeding output limit {maximum_output_bytes}",
                self.model_data.len()
            )));
        }
        self.model_data.copy_range(0, self.model_data.len(), output)
    }
}

/// Reads the verified `CubismMoc` `MonoBehaviour` byte-array layout and inspects
/// only the stable MOC3 header tables used by the managed implementation.
///
/// Callers are responsible for identifying the matching `CubismMoc` script;
/// this function deliberately does not guess a script schema from arbitrary
/// `MonoBehaviour` bytes.
pub fn read_cubism_moc(
    file: &SerializedFile,
    object_index: usize,
    limits: CubismMocReadLimits,
) -> Result<CubismMoc> {
    let (behaviour, script_data) =
        read_mono_behaviour_with_script_data(file, object_index, limits.mono_behaviour)?;
    let serialized_endian = if file.header.endianness == 0 {
        Endian::Little
    } else {
        Endian::Big
    };
    let mut reader = EndianReader::new(script_data.cursor(), serialized_endian);
    let model_length = u64::from(reader.read_u32()?);
    if model_length > limits.maximum_model_bytes {
        return Err(Error::invalid_data(format!(
            "Cubism MOC3 payload is {model_length} bytes, exceeding limit {}",
            limits.maximum_model_bytes
        )));
    }
    let model_offset = reader.position()?;
    let model_data = script_data.subregion(model_offset, model_length)?;
    let metadata = inspect_moc3(&model_data, limits)?;

    Ok(CubismMoc {
        path_id: behaviour.path_id,
        name: behaviour.name,
        sdk_version: metadata.sdk_version,
        is_big_endian: metadata.is_big_endian,
        canvas_width: metadata.canvas_width,
        canvas_height: metadata.canvas_height,
        central_position_x: metadata.central_position_x,
        central_position_y: metadata.central_position_y,
        pixels_per_unit: metadata.pixels_per_unit,
        part_count: metadata.part_count,
        parameter_count: metadata.parameter_count,
        part_names: metadata.part_names,
        parameter_names: metadata.parameter_names,
        model_data,
    })
}

/// Identifies a `CubismMoc` through its actual `MonoScript` reference before
/// interpreting script-defined bytes. Missing, unresolved, or differently
/// typed script pointers follow managed `PPtr.TryGet` semantics and return
/// `Ok(None)`; a resolved `CubismMoc` target remains strictly parsed.
pub fn try_read_cubism_moc(
    collection: &AssetCollection,
    source_file_index: usize,
    object_index: usize,
    limits: CubismMocReadLimits,
) -> Result<Option<CubismMoc>> {
    let source = collection
        .serialized_files
        .get(source_file_index)
        .ok_or_else(|| {
            Error::invalid_data(format!(
                "Cubism source file index {source_file_index} is out of range"
            ))
        })?;
    let (behaviour, _script_data) =
        read_mono_behaviour_with_script_data(&source.file, object_index, limits.mono_behaviour)?;
    let reference = ObjectReference {
        file_id: behaviour.script.file_id,
        path_id: behaviour.script.path_id,
    };
    let Some(script) = resolve_object_reference(collection, source_file_index, reference)
        .ok()
        .flatten()
    else {
        return Ok(None);
    };
    if script.object.class_id != MONO_SCRIPT_CLASS_ID {
        return Ok(None);
    }
    let metadata = read_mono_script(script.file, script.object_index, limits.mono_behaviour)?;
    if metadata.class_name != "CubismMoc" {
        return Ok(None);
    }
    read_cubism_moc(&source.file, object_index, limits).map(Some)
}

struct MocMetadata {
    sdk_version: CubismSdkVersion,
    is_big_endian: bool,
    canvas_width: f32,
    canvas_height: f32,
    central_position_x: f32,
    central_position_y: f32,
    pixels_per_unit: f32,
    part_count: u32,
    parameter_count: u32,
    part_names: Vec<String>,
    parameter_names: Vec<String>,
}

fn inspect_moc3(data: &Region, limits: CubismMocReadLimits) -> Result<MocMetadata> {
    if data.len() < 5 {
        return Err(Error::invalid_data(format!(
            "Cubism MOC3 payload is {} bytes; at least 5 version bytes are required",
            data.len()
        )));
    }
    let mut prefix = [0_u8; 5];
    data.read_exact_at(0, &mut prefix)?;
    let sdk_version = CubismSdkVersion::from_byte(prefix[4]);
    if matches!(sdk_version, CubismSdkVersion::Unknown(_)) {
        return Ok(empty_metadata(sdk_version));
    }
    if data.len() < MINIMUM_MOC3_HEADER_BYTES {
        return Err(Error::invalid_data(format!(
            "Cubism MOC3 payload is {} bytes; SDK version {} requires at least {MINIMUM_MOC3_HEADER_BYTES}",
            data.len(),
            prefix[4]
        )));
    }
    if &prefix[..4] != MOC3_MAGIC {
        return Err(Error::invalid_data(format!(
            "Cubism MOC3 magic is {:?}, expected {:?}",
            &prefix[..4],
            MOC3_MAGIC
        )));
    }
    let mut endian_byte = [0_u8; 1];
    data.read_exact_at(5, &mut endian_byte)?;
    inspect_known_moc3(data, limits, sdk_version, endian_byte[0] != 0)
}

fn empty_metadata(sdk_version: CubismSdkVersion) -> MocMetadata {
    MocMetadata {
        sdk_version,
        is_big_endian: false,
        canvas_width: 0.0,
        canvas_height: 0.0,
        central_position_x: 0.0,
        central_position_y: 0.0,
        pixels_per_unit: 0.0,
        part_count: 0,
        parameter_count: 0,
        part_names: Vec::new(),
        parameter_names: Vec::new(),
    }
}

fn inspect_known_moc3(
    data: &Region,
    limits: CubismMocReadLimits,
    sdk_version: CubismSdkVersion,
    is_big_endian: bool,
) -> Result<MocMetadata> {
    let endian = if is_big_endian {
        Endian::Big
    } else {
        Endian::Little
    };

    let count_table_offset = u64::from(read_u32_at(data, 64, endian)?);
    let canvas_info_offset = u64::from(read_u32_at(data, 68, endian)?);
    let part_ids_offset = u64::from(read_u32_at(data, 76, endian)?);
    let parameter_ids_offset = u64::from(read_u32_at(data, 264, endian)?);
    check_range(data, count_table_offset, 24, "MOC3 count table")?;
    check_range(data, canvas_info_offset, 20, "MOC3 canvas info")?;

    let part_count = read_u32_at(data, count_table_offset, endian)?;
    let parameter_count = read_u32_at(data, count_table_offset + 20, endian)?;
    let part_count_usize = check_identifier_count(part_count, limits.maximum_part_count, "parts")?;
    let parameter_count_usize = check_identifier_count(
        parameter_count,
        limits.maximum_parameter_count,
        "parameters",
    )?;
    let mut retained_bytes = initial_identifier_allocation(
        data,
        part_ids_offset,
        part_count_usize,
        parameter_ids_offset,
        parameter_count_usize,
        limits.maximum_total_identifier_bytes,
    )?;
    let part_names = read_identifiers(
        data,
        part_ids_offset,
        part_count_usize,
        &mut retained_bytes,
        limits.maximum_total_identifier_bytes,
    )?;
    let parameter_names = read_identifiers(
        data,
        parameter_ids_offset,
        parameter_count_usize,
        &mut retained_bytes,
        limits.maximum_total_identifier_bytes,
    )?;

    Ok(MocMetadata {
        sdk_version,
        is_big_endian,
        pixels_per_unit: read_f32_at(data, canvas_info_offset, endian)?,
        central_position_x: read_f32_at(data, canvas_info_offset + 4, endian)?,
        central_position_y: read_f32_at(data, canvas_info_offset + 8, endian)?,
        canvas_width: read_f32_at(data, canvas_info_offset + 12, endian)?,
        canvas_height: read_f32_at(data, canvas_info_offset + 16, endian)?,
        part_count,
        parameter_count,
        part_names,
        parameter_names,
    })
}

fn check_identifier_count(value: u32, maximum: usize, field: &str) -> Result<usize> {
    let count = usize::try_from(value).map_err(|_| {
        Error::invalid_data(format!("MOC3 {field} count does not fit this platform"))
    })?;
    if count > maximum {
        return Err(Error::invalid_data(format!(
            "MOC3 has {count} {field}, exceeding limit {maximum}"
        )));
    }
    Ok(count)
}

fn initial_identifier_allocation(
    data: &Region,
    part_offset: u64,
    part_count: usize,
    parameter_offset: u64,
    parameter_count: usize,
    maximum: u64,
) -> Result<u64> {
    let part_bytes = identifier_table_bytes(part_count, "part")?;
    let parameter_bytes = identifier_table_bytes(parameter_count, "parameter")?;
    let table_bytes = part_bytes
        .checked_add(parameter_bytes)
        .ok_or_else(|| Error::invalid_data("MOC3 identifier table bytes overflowed"))?;
    if table_bytes > maximum {
        return Err(Error::invalid_data(format!(
            "MOC3 identifier tables total {table_bytes} bytes, exceeding limit {maximum}"
        )));
    }
    if part_count != 0 {
        check_range(data, part_offset, part_bytes, "MOC3 part identifiers")?;
    }
    if parameter_count != 0 {
        check_range(
            data,
            parameter_offset,
            parameter_bytes,
            "MOC3 parameter identifiers",
        )?;
    }
    let entries = part_count
        .checked_add(parameter_count)
        .ok_or_else(|| Error::invalid_data("MOC3 identifier entry count overflowed"))?;
    let retained = u64::try_from(entries)
        .ok()
        .and_then(|count| count.checked_mul(u64::try_from(std::mem::size_of::<String>()).ok()?))
        .ok_or_else(|| Error::invalid_data("MOC3 identifier allocation budget overflowed"))?;
    if retained > maximum {
        return Err(Error::invalid_data(format!(
            "MOC3 identifier vector needs {retained} bytes, exceeding limit {maximum}"
        )));
    }
    Ok(retained)
}

fn identifier_table_bytes(count: usize, field: &str) -> Result<u64> {
    u64::try_from(count)
        .ok()
        .and_then(|count| count.checked_mul(IDENTIFIER_BYTES_U64))
        .ok_or_else(|| Error::invalid_data(format!("MOC3 {field} identifier bytes overflowed")))
}

fn read_identifiers(
    data: &Region,
    start: u64,
    count: usize,
    retained_bytes: &mut u64,
    maximum_retained_bytes: u64,
) -> Result<Vec<String>> {
    let mut result = Vec::new();
    result.try_reserve_exact(count).map_err(|error| {
        Error::invalid_data(format!("cannot allocate MOC3 identifier list: {error}"))
    })?;
    let mut bytes = [0_u8; IDENTIFIER_BYTES];
    for index in 0..count {
        let index = u64::try_from(index)
            .map_err(|_| Error::invalid_data("MOC3 identifier index does not fit in u64"))?;
        let offset = index
            .checked_mul(IDENTIFIER_BYTES_U64)
            .and_then(|relative| start.checked_add(relative))
            .ok_or_else(|| Error::invalid_data("MOC3 identifier offset overflowed"))?;
        data.read_exact_at(offset, &mut bytes)?;
        let length = bytes
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(bytes.len());
        let value = lossy_utf8(&bytes[..length])?;
        *retained_bytes = retained_bytes
            .checked_add(u64::try_from(value.capacity()).unwrap_or(u64::MAX))
            .ok_or_else(|| Error::invalid_data("MOC3 decoded identifier budget overflowed"))?;
        if *retained_bytes > maximum_retained_bytes {
            return Err(Error::invalid_data(format!(
                "MOC3 retained identifiers exceed limit {maximum_retained_bytes}"
            )));
        }
        result.push(value);
    }
    result.sort_unstable();
    result.dedup();
    Ok(result)
}

fn lossy_utf8(input: &[u8]) -> Result<String> {
    let capacity = lossy_utf8_length(input)?;
    let mut output = String::new();
    output.try_reserve_exact(capacity).map_err(|error| {
        Error::invalid_data(format!("cannot allocate decoded MOC3 identifier: {error}"))
    })?;
    let mut remaining = input;
    while !remaining.is_empty() {
        match std::str::from_utf8(remaining) {
            Ok(valid) => {
                output.push_str(valid);
                break;
            }
            Err(error) => {
                let (valid, rest) = remaining.split_at(error.valid_up_to());
                output.push_str(std::str::from_utf8(valid).expect("validated UTF-8 prefix"));
                output.push('\u{fffd}');
                let invalid_length = error.error_len().unwrap_or(rest.len());
                remaining = &rest[invalid_length..];
            }
        }
    }
    Ok(output)
}

fn lossy_utf8_length(mut input: &[u8]) -> Result<usize> {
    let mut output_length = 0_usize;
    while !input.is_empty() {
        match std::str::from_utf8(input) {
            Ok(valid) => {
                output_length = output_length.checked_add(valid.len()).ok_or_else(|| {
                    Error::invalid_data("MOC3 decoded identifier length overflowed")
                })?;
                break;
            }
            Err(error) => {
                output_length = output_length
                    .checked_add(error.valid_up_to())
                    .and_then(|length| length.checked_add('\u{fffd}'.len_utf8()))
                    .ok_or_else(|| {
                        Error::invalid_data("MOC3 decoded identifier length overflowed")
                    })?;
                let rest = &input[error.valid_up_to()..];
                let invalid_length = error.error_len().unwrap_or(rest.len());
                input = &rest[invalid_length..];
            }
        }
    }
    Ok(output_length)
}

fn read_u32_at(data: &Region, offset: u64, endian: Endian) -> Result<u32> {
    let mut bytes = [0_u8; 4];
    data.read_exact_at(offset, &mut bytes)?;
    Ok(match endian {
        Endian::Little => u32::from_le_bytes(bytes),
        Endian::Big => u32::from_be_bytes(bytes),
    })
}

fn read_f32_at(data: &Region, offset: u64, endian: Endian) -> Result<f32> {
    Ok(f32::from_bits(read_u32_at(data, offset, endian)?))
}

fn check_range(data: &Region, offset: u64, length: u64, field: &str) -> Result<()> {
    let end = offset
        .checked_add(length)
        .ok_or_else(|| Error::invalid_data(format!("{field} range overflowed")))?;
    if end > data.len() {
        return Err(Error::invalid_data(format!(
            "{field} range {offset}..{end} exceeds MOC3 length {}",
            data.len()
        )));
    }
    Ok(())
}

/// MOC3 byte builders shared by the tests of modules that consume a MOC.
#[cfg(test)]
pub(crate) mod test_support {
    pub(crate) fn synthetic_moc3(big_endian: bool, sdk_version: u8) -> Vec<u8> {
        const COUNT_TABLE: usize = 280;
        const CANVAS_INFO: usize = 320;
        const PART_IDS: usize = 384;
        const PARAMETER_IDS: usize = 512;
        let mut model = vec![0_u8; 576];
        model[..4].copy_from_slice(b"MOC3");
        model[4] = sdk_version;
        model[5] = u8::from(big_endian);
        write_u32(
            &mut model,
            64,
            u32::try_from(COUNT_TABLE).unwrap(),
            big_endian,
        );
        write_u32(
            &mut model,
            68,
            u32::try_from(CANVAS_INFO).unwrap(),
            big_endian,
        );
        write_u32(&mut model, 76, u32::try_from(PART_IDS).unwrap(), big_endian);
        write_u32(
            &mut model,
            264,
            u32::try_from(PARAMETER_IDS).unwrap(),
            big_endian,
        );
        write_u32(&mut model, COUNT_TABLE, 2, big_endian);
        write_u32(&mut model, COUNT_TABLE + 20, 1, big_endian);
        write_f32(&mut model, CANVAS_INFO, 100.0, big_endian);
        write_f32(&mut model, CANVAS_INFO + 4, 1.5, big_endian);
        write_f32(&mut model, CANVAS_INFO + 8, -2.0, big_endian);
        write_f32(&mut model, CANVAS_INFO + 12, 800.0, big_endian);
        write_f32(&mut model, CANVAS_INFO + 16, 600.0, big_endian);
        write_identifier(&mut model, PART_IDS, "PartHead");
        write_identifier(&mut model, PART_IDS + 64, "PartArm");
        write_identifier(&mut model, PARAMETER_IDS, "ParamAngleX");
        model
    }

    pub(crate) fn write_u32(output: &mut [u8], offset: usize, value: u32, big_endian: bool) {
        let bytes = if big_endian {
            value.to_be_bytes()
        } else {
            value.to_le_bytes()
        };
        output[offset..offset + 4].copy_from_slice(&bytes);
    }

    pub(crate) fn write_f32(output: &mut [u8], offset: usize, value: f32, big_endian: bool) {
        write_u32(output, offset, value.to_bits(), big_endian);
    }

    pub(crate) fn write_identifier(output: &mut [u8], offset: usize, value: &str) {
        output[offset..offset + value.len()].copy_from_slice(value.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{synthetic_moc3, write_u32};
    use crate::loader::{AssetCollection, LoadedSerializedFile};
    use crate::monobehaviour::MONO_BEHAVIOUR_CLASS_ID;
    use crate::serialized::SerializedFile;
    use crate::source::Region;

    use super::{CubismMocReadLimits, CubismSdkVersion, read_cubism_moc, try_read_cubism_moc};

    #[test]
    fn reads_little_endian_source_bound_moc3_and_streams_exact_bytes() {
        let model = synthetic_moc3(false, 5);
        let object = cubism_object("Haru", &model, false);
        let file = parse_v22(MONO_BEHAVIOUR_CLASS_ID, 13, &object);

        let moc = read_cubism_moc(&file, 0, CubismMocReadLimits::default()).unwrap();

        assert_eq!(moc.path_id, 7);
        assert_eq!(moc.name, "Haru");
        assert_eq!(moc.sdk_version, CubismSdkVersion::V50);
        assert!(!moc.is_big_endian);
        assert_eq!(moc.part_count, 2);
        assert_eq!(moc.parameter_count, 1);
        assert_eq!(moc.part_names, ["PartArm", "PartHead"]);
        assert_eq!(moc.parameter_names, ["ParamAngleX"]);
        assert_eq!(moc.pixels_per_unit.to_bits(), 100.0_f32.to_bits());
        assert_eq!(moc.central_position_x.to_bits(), 1.5_f32.to_bits());
        assert_eq!(moc.central_position_y.to_bits(), (-2.0_f32).to_bits());
        assert_eq!(moc.canvas_width.to_bits(), 800.0_f32.to_bits());
        assert_eq!(moc.canvas_height.to_bits(), 600.0_f32.to_bits());
        assert_eq!(moc.model_data.read_to_vec(u64::MAX).unwrap(), model);
        let mut output = Vec::new();
        assert_eq!(
            moc.write_moc3(&mut output, u64::MAX).unwrap(),
            u64::try_from(model.len()).unwrap()
        );
        assert_eq!(output, model);
    }

    #[test]
    fn reads_big_endian_moc3_after_no_target_monobehaviour_prefix() {
        let model = synthetic_moc3(true, 1);
        let object = cubism_object("NoTarget", &model, true);
        let file = parse_v22(MONO_BEHAVIOUR_CLASS_ID, -2, &object);

        let moc = read_cubism_moc(&file, 0, CubismMocReadLimits::default()).unwrap();

        assert_eq!(moc.sdk_version, CubismSdkVersion::V30);
        assert!(moc.is_big_endian);
        assert_eq!(moc.canvas_width.to_bits(), 800.0_f32.to_bits());
        assert_eq!(moc.part_names.len(), 2);
    }

    #[test]
    fn reads_big_endian_v13_object_with_32_bit_pptrs_and_absolute_alignment() {
        let model = synthetic_moc3(false, 4);
        let probe = synthetic_v13_big_endian_no_target(&[]);
        let object_start =
            usize::try_from(u32::from_be_bytes(probe[12..16].try_into().unwrap())).unwrap();
        assert!(!object_start.is_multiple_of(4));
        let object = cubism_object_v13_big_endian("Legacy", &model, object_start);
        let file = SerializedFile::open(Region::from_bytes(synthetic_v13_big_endian_no_target(
            &object,
        )))
        .unwrap();

        let moc = read_cubism_moc(&file, 0, CubismMocReadLimits::default()).unwrap();

        assert_eq!(moc.name, "Legacy");
        assert_eq!(moc.sdk_version, CubismSdkVersion::V42);
        assert_eq!(moc.model_data.read_to_vec(u64::MAX).unwrap(), model);
    }

    #[test]
    fn rejects_unknown_headers_ranges_classes_and_budgets() {
        let valid = synthetic_moc3(false, 5);
        let object = cubism_object("Model", &valid, false);
        let file = parse_v22(MONO_BEHAVIOUR_CLASS_ID, 13, &object);
        let short_payload = CubismMocReadLimits {
            maximum_model_bytes: u64::try_from(valid.len() - 1).unwrap(),
            ..CubismMocReadLimits::default()
        };
        assert!(read_cubism_moc(&file, 0, short_payload).is_err());
        let short_parts = CubismMocReadLimits {
            maximum_part_count: 1,
            ..CubismMocReadLimits::default()
        };
        assert!(read_cubism_moc(&file, 0, short_parts).is_err());
        let short_output = read_cubism_moc(&file, 0, CubismMocReadLimits::default()).unwrap();
        assert!(
            short_output
                .write_moc3(&mut Vec::new(), u64::try_from(valid.len() - 1).unwrap())
                .is_err()
        );

        let mut unknown = valid.clone();
        unknown[4] = 99;
        let unknown_file = parse_v22(
            MONO_BEHAVIOUR_CLASS_ID,
            13,
            &cubism_object("Unknown", &unknown, false),
        );
        let unknown = read_cubism_moc(&unknown_file, 0, CubismMocReadLimits::default()).unwrap();
        assert_eq!(unknown.sdk_version, CubismSdkVersion::Unknown(99));
        assert_eq!(
            unknown.sdk_version.description(),
            "Unknown SDK version (99)"
        );
        assert_eq!(unknown.part_count, 0);
        assert!(unknown.part_names.is_empty());

        let short_future = b"MOC3\x63";
        let short_future_file = parse_v22(
            MONO_BEHAVIOUR_CLASS_ID,
            13,
            &cubism_object("Future", short_future, false),
        );
        let short_future =
            read_cubism_moc(&short_future_file, 0, CubismMocReadLimits::default()).unwrap();
        assert_eq!(short_future.sdk_version, CubismSdkVersion::Unknown(99));
        let mut short_future_output = Vec::new();
        short_future
            .write_moc3(&mut short_future_output, 5)
            .unwrap();
        assert_eq!(short_future_output, b"MOC3\x63");

        let mut invalid_magic = valid.clone();
        invalid_magic[..4].copy_from_slice(b"BAD!");
        let invalid_magic_file = parse_v22(
            MONO_BEHAVIOUR_CLASS_ID,
            13,
            &cubism_object("Bad magic", &invalid_magic, false),
        );
        assert!(read_cubism_moc(&invalid_magic_file, 0, CubismMocReadLimits::default()).is_err());

        let mut bad_range = valid.clone();
        write_u32(&mut bad_range, 64, u32::MAX, false);
        let bad_range_file = parse_v22(
            MONO_BEHAVIOUR_CLASS_ID,
            13,
            &cubism_object("Bad", &bad_range, false),
        );
        assert!(read_cubism_moc(&bad_range_file, 0, CubismMocReadLimits::default()).is_err());

        let wrong_class = parse_v22(115, 13, &object);
        assert!(read_cubism_moc(&wrong_class, 0, CubismMocReadLimits::default()).is_err());

        let mut truncated_object = cubism_object("Truncated", &valid, false);
        let length_offset = 28 + aligned_string_bytes("Truncated");
        truncated_object[length_offset..length_offset + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        let truncated = parse_v22(MONO_BEHAVIOUR_CLASS_ID, 13, &truncated_object);
        assert!(read_cubism_moc(&truncated, 0, CubismMocReadLimits::default()).is_err());

        let mut zero_counts = valid.clone();
        write_u32(&mut zero_counts, 280, 0, false);
        write_u32(&mut zero_counts, 300, 0, false);
        write_u32(&mut zero_counts, 76, u32::MAX, false);
        write_u32(&mut zero_counts, 264, u32::MAX, false);
        let zero_counts_file = parse_v22(
            MONO_BEHAVIOUR_CLASS_ID,
            13,
            &cubism_object("Empty", &zero_counts, false),
        );
        let empty = read_cubism_moc(&zero_counts_file, 0, CubismMocReadLimits::default()).unwrap();
        assert!(empty.part_names.is_empty());
        assert!(empty.parameter_names.is_empty());

        let mut invalid_utf8 = valid.clone();
        invalid_utf8[384..448].fill(0xff);
        let invalid_utf8_file = parse_v22(
            MONO_BEHAVIOUR_CLASS_ID,
            13,
            &cubism_object("Lossy", &invalid_utf8, false),
        );
        let decoded_budget = CubismMocReadLimits {
            maximum_total_identifier_bytes: 192,
            ..CubismMocReadLimits::default()
        };
        assert!(read_cubism_moc(&invalid_utf8_file, 0, decoded_budget).is_err());
    }

    #[test]
    fn collection_reader_requires_the_resolved_cubism_moc_script() {
        let cubism = collection_with_script_class("CubismMoc", 8);
        let moc = try_read_cubism_moc(&cubism, 0, 0, CubismMocReadLimits::default())
            .unwrap()
            .unwrap();
        assert_eq!(moc.name, "Verified");

        let other = collection_with_script_class("UnrelatedBehaviour", 8);
        assert!(
            try_read_cubism_moc(&other, 0, 0, CubismMocReadLimits::default())
                .unwrap()
                .is_none()
        );

        let missing = collection_with_script_class("CubismMoc", 999);
        assert!(
            try_read_cubism_moc(&missing, 0, 0, CubismMocReadLimits::default())
                .unwrap()
                .is_none()
        );
    }

    fn cubism_object(name: &str, model: &[u8], no_target: bool) -> Vec<u8> {
        cubism_object_with_script(name, model, no_target, 2)
    }

    fn cubism_object_with_script(
        name: &str,
        model: &[u8],
        no_target: bool,
        script_path_id: i64,
    ) -> Vec<u8> {
        let mut object = Vec::new();
        if no_target {
            object.extend_from_slice(&0_u32.to_le_bytes());
            push_pptr(&mut object, 0, 0);
            push_pptr(&mut object, 0, 0);
        }
        push_pptr(&mut object, 0, 1);
        object.push(1);
        align(&mut object, 4);
        push_pptr(&mut object, 0, script_path_id);
        push_aligned_string(&mut object, name);
        object.extend_from_slice(&u32::try_from(model.len()).unwrap().to_le_bytes());
        object.extend_from_slice(model);
        object
    }

    fn cubism_object_v13_big_endian(name: &str, model: &[u8], object_start: usize) -> Vec<u8> {
        let mut object = Vec::new();
        push_be_u32(&mut object, 0);
        push_be_pptr32(&mut object, 0, 0);
        push_be_pptr32(&mut object, 0, 0);
        push_be_pptr32(&mut object, 0, 1);
        object.push(1);
        align_with_base(&mut object, object_start, 4);
        push_be_pptr32(&mut object, 0, 2);
        push_be_aligned_string(&mut object, name, object_start);
        push_be_u32(&mut object, u32::try_from(model.len()).unwrap());
        object.extend_from_slice(model);
        object
    }

    fn collection_with_script_class(class_name: &str, script_path_id: i64) -> AssetCollection {
        let model = synthetic_moc3(false, 5);
        let behaviour = cubism_object_with_script("Verified", &model, false, script_path_id);
        let script = mono_script(class_name);
        let file =
            SerializedFile::open(Region::from_bytes(synthetic_v22_pair(&behaviour, &script)))
                .unwrap();
        AssetCollection::from_loaded_parts(
            vec![LoadedSerializedFile {
                path: "model.assets".to_owned(),
                file,
            }],
            Vec::new(),
        )
    }

    fn mono_script(class_name: &str) -> Vec<u8> {
        let mut script = Vec::new();
        push_aligned_string(&mut script, "Cubism script");
        script.extend_from_slice(&0_i32.to_le_bytes());
        script.extend_from_slice(&[0x55; 16]);
        push_aligned_string(&mut script, class_name);
        push_aligned_string(&mut script, "Live2D.Cubism.Core");
        push_aligned_string(&mut script, "Live2D.Cubism.dll");
        script
    }

    fn synthetic_v22_pair(behaviour: &[u8], script: &[u8]) -> Vec<u8> {
        let mut metadata = Vec::new();
        metadata.extend_from_slice(b"2022.3.62f1\0");
        metadata.extend_from_slice(&13_i32.to_le_bytes());
        metadata.push(0);
        metadata.extend_from_slice(&2_i32.to_le_bytes());
        push_serialized_type(&mut metadata, MONO_BEHAVIOUR_CLASS_ID);
        push_serialized_type(&mut metadata, 115);
        metadata.extend_from_slice(&2_i32.to_le_bytes());
        push_object_info(&mut metadata, 7, 0, behaviour.len(), 0);
        push_object_info(&mut metadata, 8, behaviour.len(), script.len(), 1);
        metadata.extend_from_slice(&0_i32.to_le_bytes());
        metadata.extend_from_slice(&0_i32.to_le_bytes());
        metadata.extend_from_slice(&0_i32.to_le_bytes());
        metadata.push(0);

        let metadata_size = u32::try_from(metadata.len()).unwrap();
        let metadata_end = 48_u64 + u64::from(metadata_size);
        let data_offset = metadata_end.div_ceil(16) * 16;
        let payload_length = behaviour.len().checked_add(script.len()).unwrap();
        let file_size = data_offset + u64::try_from(payload_length).unwrap();
        let mut bytes = vec![0_u8; 48];
        bytes[8..12].copy_from_slice(&22_u32.to_be_bytes());
        bytes[16] = 0;
        bytes[20..24].copy_from_slice(&metadata_size.to_be_bytes());
        bytes[24..32].copy_from_slice(&i64::try_from(file_size).unwrap().to_be_bytes());
        bytes[32..40].copy_from_slice(&i64::try_from(data_offset).unwrap().to_be_bytes());
        bytes.extend_from_slice(&metadata);
        bytes.resize(usize::try_from(data_offset).unwrap(), 0);
        bytes.extend_from_slice(behaviour);
        bytes.extend_from_slice(script);
        bytes
    }

    fn push_serialized_type(output: &mut Vec<u8>, class_id: i32) {
        output.extend_from_slice(&class_id.to_le_bytes());
        output.push(0);
        output.extend_from_slice(&(-1_i16).to_le_bytes());
        if class_id == MONO_BEHAVIOUR_CLASS_ID {
            output.extend_from_slice(&[0x11; 16]);
        }
        output.extend_from_slice(&[0x22; 16]);
    }

    fn push_object_info(
        output: &mut Vec<u8>,
        path_id: i64,
        relative_start: usize,
        byte_size: usize,
        type_index: i32,
    ) {
        align_with_base(output, 48, 4);
        output.extend_from_slice(&path_id.to_le_bytes());
        output.extend_from_slice(&i64::try_from(relative_start).unwrap().to_le_bytes());
        output.extend_from_slice(&u32::try_from(byte_size).unwrap().to_le_bytes());
        output.extend_from_slice(&type_index.to_le_bytes());
    }

    fn aligned_string_bytes(value: &str) -> usize {
        let unaligned = 4 + value.len();
        unaligned.div_ceil(4) * 4
    }

    fn parse_v22(
        class_id: i32,
        target_platform: i32,
        object: &[u8],
    ) -> crate::serialized::SerializedFile {
        crate::serialized::SerializedFile::open(Region::from_bytes(synthetic_v22(
            class_id,
            target_platform,
            object,
        )))
        .unwrap()
    }

    fn synthetic_v22(class_id: i32, target_platform: i32, object: &[u8]) -> Vec<u8> {
        let mut metadata = Vec::new();
        metadata.extend_from_slice(b"2022.3.62f1\0");
        metadata.extend_from_slice(&target_platform.to_le_bytes());
        metadata.push(0);
        metadata.extend_from_slice(&1_i32.to_le_bytes());
        metadata.extend_from_slice(&class_id.to_le_bytes());
        metadata.push(0);
        metadata.extend_from_slice(&(-1_i16).to_le_bytes());
        if class_id == MONO_BEHAVIOUR_CLASS_ID {
            metadata.extend_from_slice(&[0x11; 16]);
        }
        metadata.extend_from_slice(&[0x22; 16]);
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

    fn synthetic_v13_big_endian_no_target(object: &[u8]) -> Vec<u8> {
        let mut metadata = Vec::new();
        metadata.extend_from_slice(b"5.0.0f1\0");
        push_be_i32(&mut metadata, -2);
        metadata.push(0);
        push_be_i32(&mut metadata, 1);
        push_be_i32(&mut metadata, MONO_BEHAVIOUR_CLASS_ID);
        metadata.extend_from_slice(&[0_u8; 16]);
        push_be_i32(&mut metadata, 0);
        push_be_i32(&mut metadata, 1);
        push_be_i32(&mut metadata, 7);
        push_be_u32(&mut metadata, 0);
        push_be_u32(&mut metadata, u32::try_from(object.len()).unwrap());
        push_be_i32(&mut metadata, MONO_BEHAVIOUR_CLASS_ID);
        metadata.extend_from_slice(
            &u16::try_from(MONO_BEHAVIOUR_CLASS_ID)
                .unwrap()
                .to_be_bytes(),
        );
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

    fn push_pptr(output: &mut Vec<u8>, file_id: i32, path_id: i64) {
        output.extend_from_slice(&file_id.to_le_bytes());
        output.extend_from_slice(&path_id.to_le_bytes());
    }

    fn push_be_pptr32(output: &mut Vec<u8>, file_id: i32, path_id: i32) {
        push_be_i32(output, file_id);
        push_be_i32(output, path_id);
    }

    fn push_be_i32(output: &mut Vec<u8>, value: i32) {
        output.extend_from_slice(&value.to_be_bytes());
    }

    fn push_be_u32(output: &mut Vec<u8>, value: u32) {
        output.extend_from_slice(&value.to_be_bytes());
    }

    fn push_be_aligned_string(output: &mut Vec<u8>, value: &str, base: usize) {
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
