use std::collections::HashMap;
use std::fmt;
use std::io::{Cursor, Write};

use crate::endian::{Endian, EndianReader, checked_length};
use crate::serialized::SerializedFile;
use crate::source::{Region, RegionCursor};
use crate::unity_version::UnityVersion;
use crate::{Error, Result};

pub const SHADER_CLASS_ID: i32 = 48;

/// Prefix emitted by `AssetStudio`'s C# `ShaderConverter` for every shader.
pub const SHADER_TEXT_HEADER: &str = "//////////////////////////////////////////\n\
//\n\
// NOTE: This is *not* a valid shader file\n\
//\n\
///////////////////////////////////////////\n";

const NO_TARGET_PLATFORM: i32 = -2;
const SHADER_PROGRAM_STAGES: [&str; 6] = ["vp", "fp", "gp", "hp", "dp", "rtp"];

/// Defensive limits for source-bound Shader parsing and output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShaderReadLimits {
    pub maximum_string_bytes: usize,
    pub maximum_total_string_bytes: usize,
    pub maximum_array_elements: usize,
    pub maximum_total_array_elements: usize,
    pub maximum_script_bytes: u64,
    pub maximum_compressed_blob_bytes: u64,
    pub maximum_total_decompressed_bytes: u64,
    pub maximum_program_code_bytes: u64,
    pub maximum_output_bytes: u64,
}

impl Default for ShaderReadLimits {
    fn default() -> Self {
        Self {
            maximum_string_bytes: 16 * 1024 * 1024,
            maximum_total_string_bytes: 32 * 1024 * 1024,
            maximum_array_elements: 1_000_000,
            maximum_total_array_elements: 4_000_000,
            maximum_script_bytes: 512 * 1024 * 1024,
            maximum_compressed_blob_bytes: 512 * 1024 * 1024,
            maximum_total_decompressed_bytes: 1024 * 1024 * 1024,
            maximum_program_code_bytes: 512 * 1024 * 1024,
            maximum_output_bytes: 512 * 1024 * 1024,
        }
    }
}

/// A Unity Shader converted to `AssetStudio`'s textual payload contract.
///
/// Direct legacy scripts remain bound to the input source. Converted 5.3+
/// scripts use a bounded in-memory [`Region`]. Both forms write
/// [`SHADER_TEXT_HEADER`] followed by the script bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShaderTextAsset {
    pub path_id: i64,
    pub name: String,
    pub path_name: String,
    pub script: Region,
}

impl ShaderTextAsset {
    #[must_use]
    pub const fn payload_kind(&self) -> &'static str {
        "shader_text"
    }

    #[must_use]
    pub const fn suggested_extension(&self) -> &'static str {
        ".shader"
    }

    pub fn output_len(&self) -> Result<u64> {
        header_length()?
            .checked_add(self.script.len())
            .ok_or_else(|| Error::invalid_data("Shader text output length overflowed"))
    }

    /// Streams the C#-compatible `shader_text` payload without materializing
    /// the source-bound script.
    pub fn write_to<W: Write>(&self, output: &mut W) -> Result<u64> {
        output.write_all(SHADER_TEXT_HEADER.as_bytes())?;
        self.script.copy_range(0, self.script.len(), output)?;
        self.output_len()
    }

    /// Materializes the complete payload under a caller-selected output cap.
    pub fn read_to_vec(&self, maximum_output_bytes: u64) -> Result<Vec<u8>> {
        let output_length = self.output_len()?;
        if output_length > maximum_output_bytes {
            return Err(Error::invalid_data(format!(
                "Shader text output is {output_length} bytes, exceeding limit {maximum_output_bytes}"
            )));
        }
        let capacity = usize::try_from(output_length).map_err(|_| {
            Error::invalid_data("Shader text output is too large for this platform")
        })?;
        let mut output = Vec::new();
        output.try_reserve_exact(capacity).map_err(|error| {
            Error::invalid_data(format!(
                "cannot allocate {capacity} Shader text output bytes: {error}"
            ))
        })?;
        self.write_to(&mut output)?;
        Ok(output)
    }
}

/// Reads and converts a Unity Shader using the same textual contract as the
/// managed `AssetStudio` converter.
pub fn read_shader(
    file: &SerializedFile,
    object_index: usize,
    limits: ShaderReadLimits,
) -> Result<ShaderTextAsset> {
    let object = file.objects.get(object_index).ok_or_else(|| {
        Error::invalid_data(format!(
            "serialized object index {object_index} is out of range"
        ))
    })?;
    if object.class_id != SHADER_CLASS_ID {
        return Err(Error::unsupported(format!(
            "object {} has class ID {}, not Shader ({SHADER_CLASS_ID})",
            object.path_id, object.class_id
        )));
    }
    if file.unity_version.is_stripped() {
        return Err(Error::unsupported(
            "Shader requires a Unity version because its layout is version-dependent",
        ));
    }
    validate_unity_shader_version(&file.unity_version)?;

    let region = file.object_region(object_index)?;
    let endian = if file.header.endianness == 0 {
        Endian::Little
    } else {
        Endian::Big
    };
    let mut reader = ShaderObjectReader {
        reader: EndianReader::new(region.cursor(), endian),
        region,
        absolute_start: object.byte_start,
        target_platform: file.target_platform,
        format_version: file.header.version.0,
        version: file.unity_version.clone(),
        limits,
        budget: ParseBudget::default(),
    };

    let name = reader.read_named_object()?;
    if file.unity_version.components() >= (5, 5, 0) {
        return read_modern_shader(&mut reader, object.path_id, name);
    }
    read_legacy_shader(&mut reader, object.path_id, name)
}

fn read_modern_shader(
    reader: &mut ShaderObjectReader,
    path_id: i64,
    name: String,
) -> Result<ShaderTextAsset> {
    let parsed = reader.read_serialized_shader()?;
    let platforms = reader.read_platforms()?;
    let tables = reader.read_chunk_tables(platforms.len())?;
    let compressed_blob = reader.read_blob_region(
        "Shader compressed blob",
        reader.limits.maximum_compressed_blob_bytes,
    )?;
    reader.align(4)?;
    reader.read_modern_shader_tail()?;
    let programs = decompress_shader_programs(
        &platforms,
        &tables,
        &compressed_blob,
        &reader.version,
        reader.limits,
        &mut reader.budget,
    )?;
    let converted = convert_serialized_shader(&parsed, &platforms, &programs, reader.limits)?;
    shader_from_converted(path_id, name, String::new(), converted, reader.limits)
}

fn read_legacy_shader(
    reader: &mut ShaderObjectReader,
    path_id: i64,
    name: String,
) -> Result<ShaderTextAsset> {
    let script_length = reader.read_script_length()?;
    let script = reader.payload_region(script_length, "Shader script")?;
    reader.skip(script_length, "Shader script")?;
    reader.align(4)?;
    let path_name = reader.read_aligned_string("Shader path name")?;

    if reader.version.major == 5 && reader.version.minor >= 3 {
        let decompressed_length = u64::from(reader.reader.read_u32()?);
        if decompressed_length > reader.limits.maximum_total_decompressed_bytes {
            return Err(Error::invalid_data(format!(
                "Shader subprogram output is {decompressed_length} bytes, exceeding limit {}",
                reader.limits.maximum_total_decompressed_bytes
            )));
        }
        let compressed = reader.read_blob(
            "Shader subprogram blob",
            reader.limits.maximum_compressed_blob_bytes,
        )?;
        let decompressed =
            decompress_lz4_block(&compressed, decompressed_length, "Shader subprogram blob")?;
        let mut program = ShaderProgram::read_table(
            &decompressed,
            &reader.version,
            reader.limits,
            &mut reader.budget,
        )?;
        let tolerate = undecodable_programs_expected(&reader.version);
        program.read_segment(
            &decompressed,
            0,
            &reader.version,
            reader.limits,
            &mut reader.budget,
            tolerate,
        )?;
        program.ensure_complete(1, tolerate)?;
        let script_bytes = script.read_to_vec(reader.limits.maximum_script_bytes)?;
        let mut decoded_script =
            BoundedText::for_shader_output(reader.limits.maximum_output_bytes)?;
        decoded_script.push_utf8_lossy(&script_bytes)?;
        let script_text = String::from_utf8(decoded_script.into_bytes()).map_err(|_| {
            Error::invalid_data("lossy Shader UTF-8 decoding produced invalid UTF-8")
        })?;
        let converted = replace_gpu_program_indices(
            &script_text,
            &program,
            reader.limits.maximum_output_bytes,
        )?;
        return shader_from_converted(path_id, name, path_name, converted, reader.limits);
    }

    let output_length = header_length()?
        .checked_add(script_length)
        .ok_or_else(|| Error::invalid_data("Shader text output length overflowed"))?;
    if output_length > reader.limits.maximum_output_bytes {
        return Err(Error::invalid_data(format!(
            "Shader text output is {output_length} bytes, exceeding limit {}",
            reader.limits.maximum_output_bytes
        )));
    }

    Ok(ShaderTextAsset {
        path_id,
        name,
        path_name,
        script,
    })
}

fn header_length() -> Result<u64> {
    u64::try_from(SHADER_TEXT_HEADER.len())
        .map_err(|_| Error::invalid_data("Shader text header length does not fit in u64"))
}

/// Accepts the versions whose `Shader` layout this reader implements.
///
/// The managed implementation stops at 2021: its object table skips class 48
/// entirely for `version >= 2021`, so an `AssetStudio` dump of a modern game
/// lists no shaders at all. That is not because 2021 is unreadable but because
/// its `SerializedProgram` definition was never finished -- 2022.2 added
/// `m_PlayerSubPrograms` and `m_ParameterBlobIndices` and the managed struct
/// still has neither, so reading one walks off the structure.
///
/// Those two fields are implemented here, taken from the serialized layout
/// rather than from the managed source, so this reads what the managed one
/// declines. Every shader in a real Unity 2022.3 game parses and converts.
fn validate_unity_shader_version(version: &UnityVersion) -> Result<()> {
    if version.major <= 5 || (2017..=2023).contains(&version.major) || version.major == 6000 {
        Ok(())
    } else {
        Err(Error::unsupported(format!(
            "Unity {version} Shader serialization version"
        )))
    }
}

#[derive(Debug, Default)]
struct ParseBudget {
    string_bytes: usize,
    array_elements: usize,
    decompressed_bytes: u64,
    program_code_bytes: u64,
}

impl ParseBudget {
    fn add_string(&mut self, length: usize, limits: ShaderReadLimits, field: &str) -> Result<()> {
        if length > limits.maximum_string_bytes {
            return Err(Error::invalid_data(format!(
                "{field} is {length} bytes, exceeding limit {}",
                limits.maximum_string_bytes
            )));
        }
        self.string_bytes = self
            .string_bytes
            .checked_add(length)
            .ok_or_else(|| Error::invalid_data("Shader string byte budget overflowed"))?;
        if self.string_bytes > limits.maximum_total_string_bytes {
            return Err(Error::invalid_data(format!(
                "Shader strings total {} bytes, exceeding limit {}",
                self.string_bytes, limits.maximum_total_string_bytes
            )));
        }
        Ok(())
    }

    fn add_array(&mut self, count: usize, limits: ShaderReadLimits, field: &str) -> Result<()> {
        if count > limits.maximum_array_elements {
            return Err(Error::invalid_data(format!(
                "{field} has {count} elements, exceeding limit {}",
                limits.maximum_array_elements
            )));
        }
        self.array_elements = self
            .array_elements
            .checked_add(count)
            .ok_or_else(|| Error::invalid_data("Shader array element budget overflowed"))?;
        if self.array_elements > limits.maximum_total_array_elements {
            return Err(Error::invalid_data(format!(
                "Shader arrays total {} elements, exceeding limit {}",
                self.array_elements, limits.maximum_total_array_elements
            )));
        }
        Ok(())
    }

    fn add_decompressed(
        &mut self,
        length: u64,
        limits: ShaderReadLimits,
        field: &str,
    ) -> Result<()> {
        self.decompressed_bytes = self
            .decompressed_bytes
            .checked_add(length)
            .ok_or_else(|| Error::invalid_data("Shader decompressed byte budget overflowed"))?;
        if self.decompressed_bytes > limits.maximum_total_decompressed_bytes {
            return Err(Error::invalid_data(format!(
                "{field} raises decompressed Shader data to {} bytes, exceeding limit {}",
                self.decompressed_bytes, limits.maximum_total_decompressed_bytes
            )));
        }
        Ok(())
    }

    fn add_program_code(
        &mut self,
        length: u64,
        limits: ShaderReadLimits,
        field: &str,
    ) -> Result<()> {
        self.program_code_bytes = self
            .program_code_bytes
            .checked_add(length)
            .ok_or_else(|| Error::invalid_data("Shader program-code byte budget overflowed"))?;
        if self.program_code_bytes > limits.maximum_program_code_bytes {
            return Err(Error::invalid_data(format!(
                "{field} raises Shader program code to {} bytes, exceeding limit {}",
                self.program_code_bytes, limits.maximum_program_code_bytes
            )));
        }
        Ok(())
    }
}

struct ShaderObjectReader {
    reader: EndianReader<RegionCursor>,
    region: Region,
    absolute_start: u64,
    target_platform: i32,
    format_version: u32,
    version: UnityVersion,
    limits: ShaderReadLimits,
    budget: ParseBudget,
}

impl ShaderObjectReader {
    fn read_named_object(&mut self) -> Result<String> {
        if self.target_platform == NO_TARGET_PLATFORM {
            self.skip(4, "Object hide flags")?;
            self.read_pptr()?;
            self.read_pptr()?;
        }
        self.read_aligned_string("Shader name")
    }

    fn read_pptr(&mut self) -> Result<()> {
        self.skip(4, "PPtr file ID")?;
        if self.format_version < 14 {
            self.skip(4, "PPtr path ID")
        } else {
            self.skip(8, "PPtr path ID")
        }
    }

    fn read_modern_shader_tail(&mut self) -> Result<()> {
        // 2022.2 inserted a per-platform stage count between the compressed
        // blob and the dependencies. It is the second field the managed
        // definition never gained, and the reason a shader with no passes at
        // all still failed: nothing above it depends on the pass count, so the
        // four bytes were simply read as the dependency count.
        if self.version.components() >= (2022, 2, 0) {
            self.skip_u32_array("Shader stage count")?;
            self.align(4)?;
        }
        let dependency_count = self.read_count("Shader object dependency")?;
        for _ in 0..dependency_count {
            self.read_pptr()?;
        }
        if self.version.major >= 2018 {
            let texture_count = self.read_count("Shader non-modifiable texture")?;
            for _ in 0..texture_count {
                let _name = self.read_aligned_string("Shader non-modifiable texture name")?;
                self.read_pptr()?;
            }
        }
        let _shader_is_baked = self.reader.read_bool()?;
        self.align(4)?;
        // Unity 6 appends the source asset's GUID, four unsigned ints.
        if self.version.major >= 6000 {
            self.skip(16, "Shader asset GUID")?;
        }
        Ok(())
    }

    fn read_aligned_string(&mut self, field: &str) -> Result<String> {
        let length = checked_length(self.reader.read_i32()?, field)?;
        self.budget.add_string(length, self.limits, field)?;
        let value = self.reader.read_utf8(length)?;
        if length != 0 {
            self.align(4)?;
        }
        Ok(value)
    }

    fn read_script_length(&mut self) -> Result<u64> {
        let length = checked_length(self.reader.read_i32()?, "Shader script")?;
        let length = u64::try_from(length)
            .map_err(|_| Error::invalid_data("Shader script length does not fit in u64"))?;
        if length > self.limits.maximum_script_bytes {
            return Err(Error::invalid_data(format!(
                "Shader script is {length} bytes, exceeding limit {}",
                self.limits.maximum_script_bytes
            )));
        }
        Ok(length)
    }

    fn payload_region(&mut self, length: u64, field: &str) -> Result<Region> {
        self.region
            .subregion(self.reader.position()?, length)
            .map_err(|error| Error::invalid_data(format!("{field} exceeds its object: {error}")))
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
            .ok_or_else(|| Error::invalid_data("Shader object alignment position overflowed"))?;
        let remainder = absolute % alignment;
        if remainder == 0 {
            return Ok(());
        }
        self.skip(alignment - remainder, "Shader object alignment")
    }
}

#[derive(Debug)]
struct ShaderProgramEntry {
    offset: usize,
    length: usize,
    segment: usize,
}

struct ShaderChunkTables {
    offsets: Vec<Vec<u32>>,
    compressed_lengths: Vec<Vec<u32>>,
    decompressed_lengths: Vec<Vec<u32>>,
}

#[derive(Debug)]
struct ShaderProgram {
    entries: Vec<ShaderProgramEntry>,
    entries_by_segment: Vec<usize>,
    sub_programs: Vec<Option<ShaderSubProgram>>,
    /// Why an entry has no decoded program, when that was tolerated.
    ///
    /// Kept so the exported text can report the mismatch it actually hit
    /// rather than a guess at the cause.
    undecoded: Vec<Option<String>>,
}

#[derive(Debug)]
struct ShaderSubProgram {
    program_type: i32,
    keywords: Vec<String>,
    local_keywords: Vec<String>,
    program_code: Vec<u8>,
}

struct ProgramReader<'a, 'b> {
    reader: EndianReader<Cursor<&'a [u8]>>,
    alignment_base: u64,
    limits: ShaderReadLimits,
    budget: &'b mut ParseBudget,
}

impl<'a, 'b> ProgramReader<'a, 'b> {
    fn new(
        bytes: &'a [u8],
        alignment_base: usize,
        limits: ShaderReadLimits,
        budget: &'b mut ParseBudget,
    ) -> Result<Self> {
        Ok(Self {
            reader: EndianReader::new(Cursor::new(bytes), Endian::Little),
            alignment_base: u64::try_from(alignment_base).map_err(|_| {
                Error::invalid_data("Shader program alignment base does not fit in u64")
            })?,
            limits,
            budget,
        })
    }

    fn read_count(&mut self, field: &str) -> Result<usize> {
        let count = checked_length(self.reader.read_i32()?, field)?;
        self.budget.add_array(count, self.limits, field)?;
        Ok(count)
    }

    fn read_aligned_string(&mut self, field: &str) -> Result<String> {
        let length = checked_length(self.reader.read_i32()?, field)?;
        self.budget.add_string(length, self.limits, field)?;
        let value = self.reader.read_utf8(length)?;
        if length != 0 {
            self.align(4)?;
        }
        Ok(value)
    }

    fn read_string_array(&mut self, field: &str) -> Result<Vec<String>> {
        let count = self.read_count(field)?;
        let mut values = new_vec(count, field)?;
        for _ in 0..count {
            values.push(self.read_aligned_string(field)?);
        }
        Ok(values)
    }

    fn read_program_code(&mut self) -> Result<Vec<u8>> {
        let length = self.read_count("Shader program code")?;
        let length_u64 = u64::try_from(length)
            .map_err(|_| Error::invalid_data("Shader program code length does not fit in u64"))?;
        self.budget
            .add_program_code(length_u64, self.limits, "Shader program code")?;
        self.reader.read_bytes(length)
    }

    fn skip(&mut self, length: u64, field: &str) -> Result<()> {
        let position = self.reader.position()?;
        let target = position
            .checked_add(length)
            .ok_or_else(|| Error::invalid_data(format!("{field} position overflowed")))?;
        let total = self.reader.len()?;
        if target > total {
            return Err(Error::invalid_data(format!(
                "{field} ends at {target}, beyond program record size {total}"
            )));
        }
        self.reader.set_position(target)
    }

    fn align(&mut self, alignment: u64) -> Result<()> {
        let position = self.reader.position()?;
        let absolute = self
            .alignment_base
            .checked_add(position)
            .ok_or_else(|| Error::invalid_data("Shader program alignment position overflowed"))?;
        let remainder = absolute % alignment;
        if remainder == 0 {
            Ok(())
        } else {
            self.skip(alignment - remainder, "Shader program alignment")
        }
    }
}

impl ShaderProgram {
    fn read_table(
        bytes: &[u8],
        version: &UnityVersion,
        limits: ShaderReadLimits,
        budget: &mut ParseBudget,
    ) -> Result<Self> {
        let mut reader = ProgramReader::new(bytes, 0, limits, budget)?;
        let count = reader.read_count("Shader program entry")?;
        let mut entries = new_vec(count, "Shader program entry")?;
        for _ in 0..count {
            let offset = checked_length(reader.reader.read_i32()?, "Shader program entry offset")?;
            let length = checked_length(reader.reader.read_i32()?, "Shader program entry length")?;
            let segment = if version.components() >= (2019, 3, 0) {
                checked_length(reader.reader.read_i32()?, "Shader program entry segment")?
            } else {
                0
            };
            entries.push(ShaderProgramEntry {
                offset,
                length,
                segment,
            });
        }
        let mut sub_programs = Vec::new();
        sub_programs.try_reserve_exact(count).map_err(|error| {
            Error::invalid_data(format!(
                "cannot allocate {count} Shader subprogram slots: {error}"
            ))
        })?;
        sub_programs.resize_with(count, || None);
        let mut entries_by_segment = Vec::new();
        entries_by_segment
            .try_reserve_exact(count)
            .map_err(|error| {
                Error::invalid_data(format!(
                    "cannot allocate {count} Shader segment-index entries: {error}"
                ))
            })?;
        entries_by_segment.extend(0..count);
        entries_by_segment.sort_unstable_by_key(|&index| (entries[index].segment, index));
        let mut undecoded = Vec::new();
        undecoded.try_reserve_exact(count).map_err(|error| {
            Error::invalid_data(format!(
                "cannot allocate {count} Shader undecoded-entry slots: {error}"
            ))
        })?;
        undecoded.resize_with(count, || None);
        Ok(Self {
            entries,
            entries_by_segment,
            sub_programs,
            undecoded,
        })
    }

    fn read_segment(
        &mut self,
        bytes: &[u8],
        segment: usize,
        _version: &UnityVersion,
        limits: ShaderReadLimits,
        budget: &mut ParseBudget,
        undecodable_programs_expected: bool,
    ) -> Result<()> {
        let start = self
            .entries_by_segment
            .partition_point(|&index| self.entries[index].segment < segment);
        let end = self
            .entries_by_segment
            .partition_point(|&index| self.entries[index].segment <= segment);
        for &index in &self.entries_by_segment[start..end] {
            let entry = &self.entries[index];
            let end = entry.offset.checked_add(entry.length).ok_or_else(|| {
                Error::invalid_data(format!("Shader program entry {index} range overflowed"))
            })?;
            let record = bytes.get(entry.offset..end).ok_or_else(|| {
                Error::invalid_data(format!(
                    "Shader program entry {index} range {}..{end} exceeds segment {segment} size {}",
                    entry.offset,
                    bytes.len()
                ))
            })?;
            if self.sub_programs[index].is_some() {
                return Err(Error::invalid_data(format!(
                    "Shader program entry {index} was supplied more than once"
                )));
            }
            match ShaderSubProgram::read(record, entry.offset, limits, budget) {
                Ok(decoded) => self.sub_programs[index] = Some(decoded),
                // The compiled-program record changed shape in 2022 while
                // keeping its 202012090 version stamp, and neither this reader
                // nor the managed one models the new one. Refusing the whole
                // shader would throw away the properties, sub-shaders, passes
                // and render state, all of which parsed correctly; guessing at
                // the record would put invented programs in the output. So the
                // entry stays undecoded and the exported text says so.
                //
                // Only a parse mismatch is absorbed, and the message it failed
                // with is carried into that text rather than replaced by an
                // assumption about the cause. A declared refusal -- an unknown
                // record version, an unknown GPU program type -- still fails
                // the shader, because that says something about the input this
                // reader must not paper over.
                //
                // Running off the end counts as a mismatch here. The record is
                // a slice of memory this function was handed, so the only I/O
                // it can perform is reading past that slice, which is what a
                // wrong layout does; no filesystem failure can reach this arm.
                Err(error @ (Error::InvalidData(_) | Error::Io(_)))
                    if undecodable_programs_expected =>
                {
                    self.undecoded[index] = Some(error.to_string());
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    fn ensure_complete(
        &self,
        segment_count: usize,
        undecodable_programs_expected: bool,
    ) -> Result<()> {
        for (index, entry) in self.entries.iter().enumerate() {
            if entry.segment >= segment_count {
                return Err(Error::invalid_data(format!(
                    "Shader program entry {index} selects segment {}, but only {segment_count} segments exist",
                    entry.segment
                )));
            }
            if self.sub_programs[index].is_none() && !undecodable_programs_expected {
                return Err(Error::invalid_data(format!(
                    "Shader program entry {index} was not decoded"
                )));
            }
        }
        Ok(())
    }
}

impl ShaderSubProgram {
    fn read(
        bytes: &[u8],
        alignment_base: usize,
        limits: ShaderReadLimits,
        budget: &mut ParseBudget,
    ) -> Result<Self> {
        let mut reader = ProgramReader::new(bytes, alignment_base, limits, budget)?;
        let version = reader.reader.read_i32()?;
        validate_shader_program_version(version)?;
        let program_type = reader.reader.read_i32()?;
        let program_type_i8 = i8::try_from(program_type)
            .map_err(|_| Error::unsupported(format!("Shader GPU program type {program_type}")))?;
        validate_gpu_program_type(program_type_i8)?;
        reader.skip(12, "Shader subprogram header")?;
        if version >= 201_608_170 {
            reader.skip(4, "Shader subprogram 5.5 header")?;
        }
        let keywords = reader.read_string_array("Shader program keyword")?;
        let local_keywords = if (201_806_140..202_012_090).contains(&version) {
            reader.read_string_array("Shader program local keyword")?
        } else {
            Vec::new()
        };
        let program_code = reader.read_program_code()?;
        reader.align(4)?;
        Ok(Self {
            program_type,
            keywords,
            local_keywords,
            program_code,
        })
    }
}

/// Whether a compiled-program record may fail to decode without failing the
/// shader.
///
/// Unity reshaped the record in 2022 without changing its 202012090 version
/// stamp, so the record cannot say for itself that it is one this reader does
/// not model -- only the asset's Unity version can. Below that, an
/// undecodable record still means something is wrong and is still reported.
const fn undecodable_programs_expected(version: &UnityVersion) -> bool {
    version.major >= 2022 || version.major == 6000
}

fn validate_shader_program_version(version: i32) -> Result<()> {
    const KNOWN: [i32; 8] = [
        201_509_030,
        201_510_240,
        201_608_170,
        201_609_010,
        201_708_220,
        201_802_150,
        201_806_140,
        202_012_090,
    ];
    if KNOWN.contains(&version) {
        Ok(())
    } else {
        Err(Error::unsupported(format!(
            "Shader GPU program record version {version}"
        )))
    }
}

fn validate_gpu_program_type(program_type: i8) -> Result<()> {
    if (0..=32).contains(&program_type) {
        Ok(())
    } else {
        Err(Error::unsupported(format!(
            "Shader GPU program type {program_type}"
        )))
    }
}

fn validate_platform(platform: i32) -> Result<()> {
    if (0..=24).contains(&platform) {
        Ok(())
    } else {
        Err(Error::unsupported(format!(
            "Shader compiler platform {platform}"
        )))
    }
}

fn decompress_lz4_block(input: &[u8], expected_length: u64, field: &str) -> Result<Vec<u8>> {
    let expected = usize::try_from(expected_length).map_err(|_| {
        Error::invalid_data(format!("{field} output is too large for this platform"))
    })?;
    let mut output = Vec::new();
    output.try_reserve_exact(expected).map_err(|error| {
        Error::invalid_data(format!("cannot allocate {expected} {field} bytes: {error}"))
    })?;
    output.resize(expected, 0);
    let written = lz4_flex::block::decompress_into(input, &mut output)
        .map_err(|error| Error::invalid_data(format!("invalid {field} LZ4 data: {error}")))?;
    if written != expected {
        return Err(Error::invalid_data(format!(
            "{field} LZ4 decoder wrote {written} bytes; expected {expected}"
        )));
    }
    Ok(output)
}

fn decompress_shader_programs(
    platforms: &[i32],
    tables: &ShaderChunkTables,
    compressed_blob: &Region,
    version: &UnityVersion,
    limits: ShaderReadLimits,
    budget: &mut ParseBudget,
) -> Result<Vec<ShaderProgram>> {
    let mut programs = new_vec(platforms.len(), "Shader platform program")?;
    for platform in 0..platforms.len() {
        let segment_count = tables.offsets[platform].len();
        let mut program = None;
        for segment in 0..segment_count {
            let offset = u64::from(tables.offsets[platform][segment]);
            let compressed_length = u64::from(tables.compressed_lengths[platform][segment]);
            let decompressed_length = u64::from(tables.decompressed_lengths[platform][segment]);
            budget.add_decompressed(decompressed_length, limits, "Shader compressed chunk")?;
            let end = offset.checked_add(compressed_length).ok_or_else(|| {
                Error::invalid_data(format!(
                    "Shader platform {platform} segment {segment} compressed range overflowed"
                ))
            })?;
            let compressed_region = compressed_blob
                .subregion(offset, compressed_length)
                .map_err(|_| {
                Error::invalid_data(format!(
                    "Shader platform {platform} segment {segment} compressed range {offset}..{end} exceeds blob size {}",
                    compressed_blob.len()
                ))
            })?;
            let compressed = compressed_region.read_to_vec(compressed_length)?;
            let decompressed =
                decompress_lz4_block(&compressed, decompressed_length, "Shader compressed chunk")?;
            if segment == 0 {
                program = Some(ShaderProgram::read_table(
                    &decompressed,
                    version,
                    limits,
                    budget,
                )?);
            }
            let current = program.as_mut().ok_or_else(|| {
                Error::invalid_data(format!(
                    "Shader platform {platform} has no first program segment"
                ))
            })?;
            current.read_segment(
                &decompressed,
                segment,
                version,
                limits,
                budget,
                undecodable_programs_expected(version),
            )?;
        }
        let program = program.ok_or_else(|| {
            Error::invalid_data(format!("Shader platform {platform} has no program table"))
        })?;
        program.ensure_complete(segment_count, undecodable_programs_expected(version))?;
        programs.push(program);
    }
    Ok(programs)
}

struct BoundedText {
    bytes: Vec<u8>,
    maximum: usize,
}

impl BoundedText {
    fn for_shader_output(maximum_output_bytes: u64) -> Result<Self> {
        let header = header_length()?;
        let maximum = maximum_output_bytes.checked_sub(header).ok_or_else(|| {
            Error::invalid_data(format!(
                "Shader output limit {maximum_output_bytes} is smaller than its {header}-byte header"
            ))
        })?;
        let maximum = usize::try_from(maximum).map_err(|_| {
            Error::invalid_data("Shader output limit is too large for this platform")
        })?;
        Ok(Self {
            bytes: Vec::new(),
            maximum,
        })
    }

    fn push_str(&mut self, value: &str) -> Result<()> {
        let new_length = self
            .bytes
            .len()
            .checked_add(value.len())
            .ok_or_else(|| Error::invalid_data("Shader text output length overflowed"))?;
        if new_length > self.maximum {
            return Err(Error::invalid_data(format!(
                "Shader converted text exceeds its {}-byte output limit",
                self.maximum
            )));
        }
        self.bytes.try_reserve(value.len()).map_err(|error| {
            Error::invalid_data(format!("cannot grow Shader text output: {error}"))
        })?;
        self.bytes.extend_from_slice(value.as_bytes());
        Ok(())
    }

    fn push_fmt(&mut self, arguments: fmt::Arguments<'_>) -> Result<()> {
        let mut value = String::new();
        fmt::write(&mut value, arguments)
            .map_err(|_| Error::invalid_data("failed to format Shader text"))?;
        self.push_str(&value)
    }

    fn push_utf8_lossy(&mut self, mut bytes: &[u8]) -> Result<()> {
        while !bytes.is_empty() {
            match std::str::from_utf8(bytes) {
                Ok(value) => {
                    self.push_str(value)?;
                    break;
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    if valid != 0 {
                        let prefix = std::str::from_utf8(&bytes[..valid])
                            .map_err(|_| Error::invalid_data("UTF-8 prefix validation changed"))?;
                        self.push_str(prefix)?;
                    }
                    self.push_str("\u{fffd}")?;
                    if let Some(invalid) = error.error_len() {
                        bytes = &bytes[valid + invalid..];
                    } else {
                        break;
                    }
                }
            }
        }
        Ok(())
    }

    fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

fn shader_from_converted(
    path_id: i64,
    name: String,
    path_name: String,
    converted: Vec<u8>,
    limits: ShaderReadLimits,
) -> Result<ShaderTextAsset> {
    let converted_length = u64::try_from(converted.len())
        .map_err(|_| Error::invalid_data("Shader converted text length does not fit in u64"))?;
    let output_length = header_length()?
        .checked_add(converted_length)
        .ok_or_else(|| Error::invalid_data("Shader text output length overflowed"))?;
    if output_length > limits.maximum_output_bytes {
        return Err(Error::invalid_data(format!(
            "Shader text output is {output_length} bytes, exceeding limit {}",
            limits.maximum_output_bytes
        )));
    }
    Ok(ShaderTextAsset {
        path_id,
        name,
        path_name,
        script: Region::from_bytes(converted),
    })
}

fn replace_gpu_program_indices(
    script: &str,
    program: &ShaderProgram,
    maximum_output_bytes: u64,
) -> Result<Vec<u8>> {
    const PREFIX: &str = "GpuProgramIndex ";
    let mut output = BoundedText::for_shader_output(maximum_output_bytes)?;
    let mut cursor = 0;
    while let Some(relative) = script[cursor..].find(PREFIX) {
        let start = cursor + relative;
        output.push_str(&script[cursor..start])?;
        let value_start = start + PREFIX.len();
        let value_end = script[value_start..]
            .find('\n')
            .map_or(script.len(), |relative_end| value_start + relative_end);
        if value_start == value_end {
            output.push_str(PREFIX)?;
            cursor = value_start;
            continue;
        }
        let index = script[value_start..value_end]
            .trim()
            .parse::<usize>()
            .map_err(|error| {
                Error::invalid_data(format!(
                    "invalid Shader GpuProgramIndex {:?}: {error}",
                    &script[value_start..value_end]
                ))
            })?;
        let sub_program = program
            .sub_programs
            .get(index)
            .and_then(Option::as_ref)
            .ok_or_else(|| {
                Error::invalid_data(format!(
                    "Shader GpuProgramIndex {index} is outside the decoded program table"
                ))
            })?;
        export_shader_sub_program(sub_program, &mut output)?;
        cursor = value_end;
    }
    output.push_str(&script[cursor..])?;
    Ok(output.into_bytes())
}

fn convert_serialized_shader(
    shader: &SerializedShader,
    platforms: &[i32],
    programs: &[ShaderProgram],
    limits: ShaderReadLimits,
) -> Result<Vec<u8>> {
    if platforms.len() != programs.len() {
        return Err(Error::invalid_data(
            "Shader platform and decoded-program counts differ",
        ));
    }
    let mut output = BoundedText::for_shader_output(limits.maximum_output_bytes)?;
    output.push_fmt(format_args!("Shader \"{}\" {{\n", shader.name))?;
    convert_properties(&shader.properties, &mut output)?;
    for sub_shader in &shader.sub_shaders {
        convert_sub_shader(sub_shader, platforms, programs, limits, &mut output)?;
    }
    if !shader.fallback_name.is_empty() {
        output.push_fmt(format_args!("Fallback \"{}\"\n", shader.fallback_name))?;
    }
    if !shader.custom_editor_name.is_empty() {
        output.push_fmt(format_args!(
            "CustomEditor \"{}\"\n",
            shader.custom_editor_name
        ))?;
    }
    output.push_str("}")?;
    Ok(output.into_bytes())
}

fn convert_properties(properties: &[SerializedProperty], output: &mut BoundedText) -> Result<()> {
    output.push_str("Properties {\n")?;
    for property in properties {
        for attribute in &property.attributes {
            output.push_fmt(format_args!("[{attribute}] "))?;
        }
        output.push_fmt(format_args!(
            "{} (\"{}\", ",
            property.name, property.description
        ))?;
        match property.property_type {
            0 => output.push_str("Color")?,
            1 => output.push_str("Vector")?,
            2 => output.push_str("Float")?,
            3 => output.push_fmt(format_args!(
                "Range({}, {})",
                property.default_values[1], property.default_values[2]
            ))?,
            4 => match property.texture_dimension {
                -1 | 0 => {}
                1 => output.push_str("any")?,
                2 => output.push_str("2D")?,
                3 => output.push_str("3D")?,
                4 => output.push_str("Cube")?,
                5 => output.push_str("2DArray")?,
                6 => output.push_str("CubeArray")?,
                dimension => {
                    return Err(Error::unsupported(format!(
                        "Shader texture dimension {dimension}"
                    )));
                }
            },
            5 => {
                return Err(Error::unsupported(
                    "Serialized Shader Int property text conversion",
                ));
            }
            property_type => {
                return Err(Error::unsupported(format!(
                    "Shader serialized property type {property_type}"
                )));
            }
        }
        output.push_str(") = ")?;
        match property.property_type {
            0 | 1 => output.push_fmt(format_args!(
                "({},{},{},{})",
                property.default_values[0],
                property.default_values[1],
                property.default_values[2],
                property.default_values[3]
            ))?,
            2 | 3 => output.push_fmt(format_args!("{}", property.default_values[0]))?,
            4 => output.push_fmt(format_args!("\"{}\" {{ }}", property.default_texture_name))?,
            _ => unreachable!("validated property type"),
        }
        output.push_str("\n")?;
    }
    output.push_str("}\n")
}

fn convert_tag_map(
    tags: &[(String, String)],
    indent: usize,
    output: &mut BoundedText,
) -> Result<()> {
    if tags.is_empty() {
        return Ok(());
    }
    for _ in 0..indent {
        output.push_str(" ")?;
    }
    output.push_str("Tags { ")?;
    for (name, value) in tags {
        output.push_fmt(format_args!("\"{name}\" = \"{value}\" "))?;
    }
    output.push_str("}\n")
}

fn convert_sub_shader(
    sub_shader: &SerializedSubShader,
    platforms: &[i32],
    programs: &[ShaderProgram],
    limits: ShaderReadLimits,
    output: &mut BoundedText,
) -> Result<()> {
    output.push_str("SubShader {\n")?;
    if sub_shader.lod != 0 {
        output.push_fmt(format_args!(" LOD {}\n", sub_shader.lod))?;
    }
    convert_tag_map(&sub_shader.tags, 1, output)?;
    for pass in &sub_shader.passes {
        convert_pass(pass, platforms, programs, limits, output)?;
    }
    output.push_str("}\n")
}

fn convert_pass(
    pass: &SerializedPass,
    platforms: &[i32],
    programs: &[ShaderProgram],
    limits: ShaderReadLimits,
    output: &mut BoundedText,
) -> Result<()> {
    match pass.pass_type {
        0 => output.push_str(" Pass ")?,
        1 => output.push_str(" UsePass ")?,
        2 => output.push_str(" GrabPass ")?,
        pass_type => return Err(Error::unsupported(format!("Shader pass type {pass_type}"))),
    }
    if pass.pass_type == 1 {
        return output.push_fmt(format_args!("\"{}\"\n", pass.use_name));
    }
    output.push_str("{\n")?;
    if pass.pass_type == 2 {
        if !pass.texture_name.is_empty() {
            output.push_fmt(format_args!("  \"{}\"\n", pass.texture_name))?;
        }
    } else {
        convert_shader_state(&pass.state, output)?;
        for (stage, program) in pass.programs.iter().enumerate() {
            if program.sub_programs.is_empty() {
                continue;
            }
            output.push_fmt(format_args!(
                "Program \"{}\" {{\n",
                SHADER_PROGRAM_STAGES[stage]
            ))?;
            convert_serialized_sub_programs(
                &program.sub_programs,
                platforms,
                programs,
                limits.maximum_array_elements,
                output,
            )?;
            output.push_str("}\n")?;
        }
    }
    output.push_str("}\n")
}

/// `ShaderLab` state values are serialized enum values in `f32` fields. Exact
/// equality is part of the managed converter contract, not a numeric estimate.
#[allow(clippy::float_cmp)]
fn csharp_float_equals(left: f32, right: f32) -> bool {
    left == right
}

fn shader_float_code(value: f32) -> Option<usize> {
    const VALUES: [f32; 21] = [
        0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0, 12.0, 13.0, 14.0, 15.0, 16.0,
        17.0, 18.0, 19.0, 20.0,
    ];
    VALUES
        .iter()
        .position(|expected| csharp_float_equals(value, *expected))
}

#[allow(clippy::cast_possible_truncation)]
fn csharp_truncate_float(value: f32) -> i32 {
    value as i32
}

fn convert_shader_state(state: &SerializedShaderState, output: &mut BoundedText) -> Result<()> {
    if !state.name.is_empty() {
        output.push_fmt(format_args!("  Name \"{}\"\n", state.name))?;
    }
    if state.lod != 0 {
        output.push_fmt(format_args!("  LOD {}\n", state.lod))?;
    }
    convert_tag_map(&state.tags, 2, output)?;
    convert_blend_states(&state.blends, state.separate_blend, output)?;
    if state.alpha_to_mask > 0.0 {
        output.push_str("  AlphaToMask On\n")?;
    }
    if state
        .z_clip
        .is_none_or(|value| !csharp_float_equals(value, 1.0))
    {
        output.push_str("  ZClip Off\n")?;
    }
    if !csharp_float_equals(state.z_test, 4.0) {
        output.push_str("  ZTest ")?;
        match shader_float_code(state.z_test) {
            Some(0) => output.push_str("Off")?,
            Some(1) => output.push_str("Never")?,
            Some(2) => output.push_str("Less")?,
            Some(3) => output.push_str("Equal")?,
            Some(5) => output.push_str("Greater")?,
            Some(6) => output.push_str("NotEqual")?,
            Some(7) => output.push_str("GEqual")?,
            Some(8) => output.push_str("Always")?,
            _ => {}
        }
        output.push_str("\n")?;
    }
    if !csharp_float_equals(state.z_write, 1.0) {
        output.push_str("  ZWrite Off\n")?;
    }
    if !csharp_float_equals(state.culling, 2.0) {
        output.push_str("  Cull ")?;
        match shader_float_code(state.culling) {
            Some(0) => output.push_str("Off")?,
            Some(1) => output.push_str("Front")?,
            _ => {}
        }
        output.push_str("\n")?;
    }
    if !csharp_float_equals(state.offset_factor, 0.0)
        || !csharp_float_equals(state.offset_units, 0.0)
    {
        output.push_fmt(format_args!(
            "  Offset {}, {}\n",
            state.offset_factor, state.offset_units
        ))?;
    }
    convert_stencil_state(state, output)?;
    convert_fog_state(state, output)?;
    if state.lighting {
        output.push_str("  Lighting On\n")?;
    }
    output.push_fmt(format_args!("  GpuProgramID {}\n", state.gpu_program_id))
}

fn stencil_is_default(operation: &SerializedStencilOp) -> bool {
    csharp_float_equals(operation.pass, 0.0)
        && csharp_float_equals(operation.fail, 0.0)
        && csharp_float_equals(operation.z_fail, 0.0)
        && csharp_float_equals(operation.comparison, 8.0)
}

fn convert_stencil_state(state: &SerializedShaderState, output: &mut BoundedText) -> Result<()> {
    if csharp_float_equals(state.stencil_ref, 0.0)
        && csharp_float_equals(state.stencil_read_mask, 255.0)
        && csharp_float_equals(state.stencil_write_mask, 255.0)
        && stencil_is_default(&state.stencil)
        && stencil_is_default(&state.stencil_front)
        && stencil_is_default(&state.stencil_back)
    {
        return Ok(());
    }
    output.push_str("  Stencil {\n")?;
    if !csharp_float_equals(state.stencil_ref, 0.0) {
        output.push_fmt(format_args!("   Ref {}\n", state.stencil_ref))?;
    }
    if !csharp_float_equals(state.stencil_read_mask, 255.0) {
        output.push_fmt(format_args!("   ReadMask {}\n", state.stencil_read_mask))?;
    }
    if !csharp_float_equals(state.stencil_write_mask, 255.0) {
        output.push_fmt(format_args!("   WriteMask {}\n", state.stencil_write_mask))?;
    }
    if !stencil_is_default(&state.stencil) {
        convert_stencil_op(&state.stencil, "", output)?;
    }
    if !stencil_is_default(&state.stencil_front) {
        convert_stencil_op(&state.stencil_front, "Front", output)?;
    }
    if !stencil_is_default(&state.stencil_back) {
        convert_stencil_op(&state.stencil_back, "Back", output)?;
    }
    output.push_str("  }\n")
}

fn convert_stencil_op(
    operation: &SerializedStencilOp,
    suffix: &str,
    output: &mut BoundedText,
) -> Result<()> {
    output.push_fmt(format_args!(
        "   Comp{suffix} {}\n",
        stencil_comparison_name(operation.comparison)
    ))?;
    output.push_fmt(format_args!(
        "   Pass{suffix} {}\n",
        stencil_operation_name(operation.pass)
    ))?;
    output.push_fmt(format_args!(
        "   Fail{suffix} {}\n",
        stencil_operation_name(operation.fail)
    ))?;
    output.push_fmt(format_args!(
        "   ZFail{suffix} {}\n",
        stencil_operation_name(operation.z_fail)
    ))
}

fn stencil_operation_name(value: f32) -> &'static str {
    match shader_float_code(value) {
        Some(1) => "Zero",
        Some(2) => "Replace",
        Some(3) => "IncrSat",
        Some(4) => "DecrSat",
        Some(5) => "Invert",
        Some(6) => "IncrWrap",
        Some(7) => "DecrWrap",
        _ => "Keep",
    }
}

fn stencil_comparison_name(value: f32) -> &'static str {
    match shader_float_code(value) {
        Some(0) => "Disabled",
        Some(1) => "Never",
        Some(2) => "Less",
        Some(3) => "Equal",
        Some(4) => "LEqual",
        Some(5) => "Greater",
        Some(6) => "NotEqual",
        Some(7) => "GEqual",
        _ => "Always",
    }
}

fn convert_fog_state(state: &SerializedShaderState, output: &mut BoundedText) -> Result<()> {
    if state.fog_mode == -1
        && state
            .fog_color
            .iter()
            .all(|value| csharp_float_equals(*value, 0.0))
        && csharp_float_equals(state.fog_density, 0.0)
        && csharp_float_equals(state.fog_start, 0.0)
        && csharp_float_equals(state.fog_end, 0.0)
    {
        return Ok(());
    }
    output.push_str("  Fog {\n")?;
    if state.fog_mode != -1 {
        output.push_str("   Mode ")?;
        output.push_str(match state.fog_mode {
            0 => "Off",
            1 => "Linear",
            2 => "Exp",
            3 => "Exp2",
            _ => "",
        })?;
        output.push_str("\n")?;
    }
    if state
        .fog_color
        .iter()
        .any(|value| !csharp_float_equals(*value, 0.0))
    {
        output.push_fmt(format_args!(
            "   Color ({},{},{},{})\n",
            state.fog_color[0], state.fog_color[1], state.fog_color[2], state.fog_color[3]
        ))?;
    }
    if !csharp_float_equals(state.fog_density, 0.0) {
        output.push_fmt(format_args!("   Density {}\n", state.fog_density))?;
    }
    if !csharp_float_equals(state.fog_start, 0.0) || !csharp_float_equals(state.fog_end, 0.0) {
        output.push_fmt(format_args!(
            "   Range {}, {}\n",
            state.fog_start, state.fog_end
        ))?;
    }
    output.push_str("  }\n")
}

fn convert_blend_states(
    blends: &[SerializedBlendState],
    separate_blend: bool,
    output: &mut BoundedText,
) -> Result<()> {
    for (index, blend) in blends.iter().enumerate() {
        if !csharp_float_equals(blend.source, 1.0)
            || !csharp_float_equals(blend.destination, 0.0)
            || !csharp_float_equals(blend.source_alpha, 1.0)
            || !csharp_float_equals(blend.destination_alpha, 0.0)
        {
            output.push_str("  Blend ")?;
            if index != 0 || separate_blend {
                output.push_fmt(format_args!("{index} "))?;
            }
            output.push_fmt(format_args!(
                "{} {}",
                blend_factor_name(blend.source),
                blend_factor_name(blend.destination)
            ))?;
            if !csharp_float_equals(blend.source_alpha, 1.0)
                || !csharp_float_equals(blend.destination_alpha, 0.0)
            {
                output.push_fmt(format_args!(
                    ", {} {}",
                    blend_factor_name(blend.source_alpha),
                    blend_factor_name(blend.destination_alpha)
                ))?;
            }
            output.push_str("\n")?;
        }
        if !csharp_float_equals(blend.operation, 0.0)
            || !csharp_float_equals(blend.operation_alpha, 0.0)
        {
            output.push_str("  BlendOp ")?;
            if index != 0 || separate_blend {
                output.push_fmt(format_args!("{index} "))?;
            }
            output.push_str(blend_operation_name(blend.operation))?;
            if !csharp_float_equals(blend.operation_alpha, 0.0) {
                output.push_fmt(format_args!(
                    ", {}",
                    blend_operation_name(blend.operation_alpha)
                ))?;
            }
            output.push_str("\n")?;
        }
        let mask = csharp_truncate_float(blend.color_mask);
        if mask != 0xf {
            output.push_str("  ColorMask ")?;
            if mask == 0 {
                output.push_str("0")?;
            } else {
                if mask & 0x2 != 0 {
                    output.push_str("R")?;
                }
                if mask & 0x4 != 0 {
                    output.push_str("G")?;
                }
                if mask & 0x8 != 0 {
                    output.push_str("B")?;
                }
                if mask & 0x1 != 0 {
                    output.push_str("A")?;
                }
            }
            output.push_fmt(format_args!(" {index}\n"))?;
        }
    }
    Ok(())
}

fn blend_factor_name(value: f32) -> &'static str {
    match shader_float_code(value) {
        Some(0) => "Zero",
        Some(2) => "DstColor",
        Some(3) => "SrcColor",
        Some(4) => "OneMinusDstColor",
        Some(5) => "SrcAlpha",
        Some(6) => "OneMinusSrcColor",
        Some(7) => "DstAlpha",
        Some(8) => "OneMinusDstAlpha",
        Some(9) => "SrcAlphaSaturate",
        Some(10) => "OneMinusSrcAlpha",
        _ => "One",
    }
}

fn blend_operation_name(value: f32) -> &'static str {
    const NAMES: [&str; 21] = [
        "Add",
        "Sub",
        "RevSub",
        "Min",
        "Max",
        "LogicalClear",
        "LogicalSet",
        "LogicalCopy",
        "LogicalCopyInverted",
        "LogicalNoop",
        "LogicalInvert",
        "LogicalAnd",
        "LogicalNand",
        "LogicalOr",
        "LogicalNor",
        "LogicalXor",
        "LogicalEquiv",
        "LogicalAndReverse",
        "LogicalAndInverted",
        "LogicalOrReverse",
        "LogicalOrInverted",
    ];
    shader_float_code(value)
        .and_then(|index| NAMES.get(index))
        .copied()
        .unwrap_or("Add")
}

fn convert_serialized_sub_programs(
    serialized: &[SerializedSubProgram],
    platforms: &[i32],
    programs: &[ShaderProgram],
    maximum_grouping_entries: usize,
    output: &mut BoundedText,
) -> Result<()> {
    let grouped = grouped_sub_program_indexes(serialized, maximum_grouping_entries)?;
    let mut group_start = 0;
    while group_start < grouped.len() {
        let leader = serialized[grouped[group_start]];
        let mut group_end = group_start + 1;
        while group_end < grouped.len() {
            let candidate = serialized[grouped[group_end]];
            if candidate.blob_index != leader.blob_index
                || candidate.gpu_program_type != leader.gpu_program_type
            {
                break;
            }
            group_end += 1;
        }
        let mut selected_platform = None;
        for (index, platform) in platforms.iter().copied().enumerate() {
            if gpu_program_usable(platform, leader.gpu_program_type)? {
                selected_platform = Some((index, platform));
                break;
            }
        }
        if let Some((platform_index, platform)) = selected_platform {
            let is_tier = group_end - group_start > 1;
            for &sub_program_index in &grouped[group_start..group_end] {
                let sub_program = &serialized[sub_program_index];
                output.push_fmt(format_args!("SubProgram \"{} ", platform_name(platform)?))?;
                if is_tier {
                    output.push_fmt(format_args!("hw_tier{:02} ", sub_program.hardware_tier))?;
                }
                output.push_str("\" {\n")?;
                let blob_index = usize::try_from(sub_program.blob_index).map_err(|_| {
                    Error::invalid_data("Shader subprogram blob index does not fit in usize")
                })?;
                let decoded = programs
                    .get(platform_index)
                    .and_then(|program| program.sub_programs.get(blob_index))
                    .and_then(Option::as_ref);
                if let Some(decoded) = decoded {
                    if decoded.program_type != i32::from(sub_program.gpu_program_type) {
                        return Err(Error::invalid_data(format!(
                            "Shader blob index {} metadata type {} disagrees with decoded type {}",
                            sub_program.blob_index,
                            sub_program.gpu_program_type,
                            decoded.program_type
                        )));
                    }
                    export_shader_sub_program(decoded, output)?;
                } else {
                    // Said rather than left out. The reader knows this record
                    // exists and where it is; what it does not know is the
                    // shape Unity 2022 gave it. A listing silently missing from
                    // an otherwise complete shader would read as a shader that
                    // has no program.
                    let reason = programs
                        .get(platform_index)
                        .and_then(|program| program.undecoded.get(blob_index))
                        .and_then(Option::as_deref)
                        .unwrap_or("no program was supplied for this entry");
                    output.push_fmt(format_args!(
                        "// blob index {} was not decoded: {reason}\n",
                        sub_program.blob_index
                    ))?;
                }
                output.push_str("\n}\n")?;
            }
        }
        group_start = group_end;
    }
    Ok(())
}

fn grouped_sub_program_indexes(
    serialized: &[SerializedSubProgram],
    maximum_entries: usize,
) -> Result<Vec<usize>> {
    if serialized.len() > maximum_entries {
        return Err(Error::invalid_data(format!(
            "Shader subprogram grouping has {} entries, exceeding limit {maximum_entries}",
            serialized.len()
        )));
    }
    // `serialized.len()` was already charged to `ParseBudget` by
    // `read_serialized_program`. Keep every derived allocation linear in that
    // bounded count and reserve it fallibly before inserting any keys.
    let mut first_blob = HashMap::new();
    first_blob.try_reserve(serialized.len()).map_err(|error| {
        Error::invalid_data(format!("cannot allocate Shader blob grouping map: {error}"))
    })?;
    let mut first_type = HashMap::new();
    first_type.try_reserve(serialized.len()).map_err(|error| {
        Error::invalid_data(format!(
            "cannot allocate Shader program-type grouping map: {error}"
        ))
    })?;
    for (index, sub_program) in serialized.iter().enumerate() {
        first_blob.entry(sub_program.blob_index).or_insert(index);
        first_type
            .entry((sub_program.blob_index, sub_program.gpu_program_type))
            .or_insert(index);
    }

    let mut indexes = Vec::new();
    indexes
        .try_reserve_exact(serialized.len())
        .map_err(|error| {
            Error::invalid_data(format!(
                "cannot allocate {} Shader grouping indexes: {error}",
                serialized.len()
            ))
        })?;
    indexes.extend(0..serialized.len());
    indexes.sort_unstable_by_key(|&index| {
        let sub_program = serialized[index];
        (
            first_blob[&sub_program.blob_index],
            first_type[&(sub_program.blob_index, sub_program.gpu_program_type)],
            index,
        )
    });
    Ok(indexes)
}

fn gpu_program_usable(platform: i32, program_type: i8) -> Result<bool> {
    let usable = match platform {
        0 => program_type == 1,
        1 => (9..=12).contains(&program_type),
        2 | 3 | 10 | 11 | 12 | 16 | 17 | 19 | 20 | 21 | 22 | 23 => {
            (26..=30).contains(&program_type)
        }
        4 => (15..=22).contains(&program_type),
        5 => program_type == 5,
        6 => {
            return Err(Error::unsupported("obsolete NaCl Shader compiler platform"));
        }
        7 => {
            return Err(Error::unsupported(
                "obsolete Flash Shader compiler platform",
            ));
        }
        8 => (13..=14).contains(&program_type),
        9 => (2..=4).contains(&program_type),
        13 => return Err(Error::unsupported("PSM Shader compiler platform")),
        14 => (23..=24).contains(&program_type),
        15 => (6..=8).contains(&program_type),
        18 => program_type == 25,
        24 => program_type == 32,
        value => {
            return Err(Error::unsupported(format!(
                "Shader compiler platform {value}"
            )));
        }
    };
    Ok(usable)
}

fn platform_name(platform: i32) -> Result<&'static str> {
    match platform {
        0 => Ok("openGL"),
        1 => Ok("d3d9"),
        2 => Ok("xbox360"),
        3 => Ok("ps3"),
        4 => Ok("d3d11"),
        5 => Ok("gles"),
        6 => Ok("glesdesktop"),
        7 => Ok("flash"),
        8 => Ok("d3d11_9x"),
        9 => Ok("gles3"),
        10 => Ok("psp2"),
        11 => Ok("ps4"),
        12 | 21 => Ok("xboxone"),
        13 => Ok("psm"),
        14 => Ok("metal"),
        15 => Ok("glcore"),
        16 => Ok("n3ds"),
        17 => Ok("wiiu"),
        18 => Ok("vulkan"),
        19 => Ok("switch"),
        20 => Ok("xboxone_d3d12"),
        22 => Ok("xbox_scarlett"),
        23 => Ok("ps5"),
        24 => Ok("ps5_nggc"),
        value => Err(Error::unsupported(format!(
            "Shader compiler platform {value}"
        ))),
    }
}

fn export_shader_sub_program(program: &ShaderSubProgram, output: &mut BoundedText) -> Result<()> {
    if !program.keywords.is_empty() {
        output.push_str("Keywords { ")?;
        for keyword in &program.keywords {
            output.push_fmt(format_args!("\"{keyword}\" "))?;
        }
        output.push_str("}\n")?;
    }
    if !program.local_keywords.is_empty() {
        output.push_str("Local Keywords { ")?;
        for keyword in &program.local_keywords {
            output.push_fmt(format_args!("\"{keyword}\" "))?;
        }
        output.push_str("}\n")?;
    }
    output.push_str("\"")?;
    if !program.program_code.is_empty() {
        match program.program_type {
            0 => output.push_str("//shader disassembly not supported on Unknown")?,
            1..=8 | 26..=30 => output.push_utf8_lossy(&program.program_code)?,
            9..=22 => output.push_str("// shader disassembly not supported on DXBC")?,
            23 | 24 => export_metal_program(&program.program_code, output)?,
            25 => output.push_str(
                "// disassembly error SPIR-V conversion is not implemented in native Rust\n",
            )?,
            31 | 32 => output.push_fmt(format_args!(
                "//shader disassembly not supported on {}",
                gpu_program_type_name(program.program_type)?
            ))?,
            value => {
                return Err(Error::unsupported(format!(
                    "Shader GPU program type {value}"
                )));
            }
        }
    }
    output.push_str("\"")
}

fn export_metal_program(code: &[u8], output: &mut BoundedText) -> Result<()> {
    let four_cc = code.get(..4).ok_or_else(|| {
        Error::invalid_data("Metal Shader program is shorter than its fourCC field")
    })?;
    let four_cc = u32::from_le_bytes(
        four_cc
            .try_into()
            .map_err(|_| Error::invalid_data("invalid Metal Shader fourCC"))?,
    );
    let mut position = 4_usize;
    if four_cc == 0xf00d_cafe {
        let offset_bytes = code.get(4..8).ok_or_else(|| {
            Error::invalid_data("Metal Shader program is shorter than its source offset")
        })?;
        let offset = i32::from_le_bytes(
            offset_bytes
                .try_into()
                .map_err(|_| Error::invalid_data("invalid Metal Shader source offset"))?,
        );
        position = checked_length(offset, "Metal Shader source offset")?;
        if position > code.len() {
            return Err(Error::invalid_data(format!(
                "Metal Shader source offset {position} exceeds code size {}",
                code.len()
            )));
        }
    }
    let entry_limit = position.saturating_add(32_767).min(code.len());
    while position < entry_limit {
        let byte = code[position];
        position += 1;
        if byte == 0 {
            break;
        }
    }
    output.push_utf8_lossy(&code[position..])
}

fn gpu_program_type_name(program_type: i32) -> Result<&'static str> {
    const NAMES: [&str; 33] = [
        "Unknown",
        "GLLegacy",
        "GLES31AEP",
        "GLES31",
        "GLES3",
        "GLES",
        "GLCore32",
        "GLCore41",
        "GLCore43",
        "DX9VertexSM20",
        "DX9VertexSM30",
        "DX9PixelSM20",
        "DX9PixelSM30",
        "DX10Level9Vertex",
        "DX10Level9Pixel",
        "DX11VertexSM40",
        "DX11VertexSM50",
        "DX11PixelSM40",
        "DX11PixelSM50",
        "DX11GeometrySM40",
        "DX11GeometrySM50",
        "DX11HullSM50",
        "DX11DomainSM50",
        "MetalVS",
        "MetalFS",
        "SPIRV",
        "ConsoleVS",
        "ConsoleFS",
        "ConsoleHS",
        "ConsoleDS",
        "ConsoleGS",
        "RayTracing",
        "PS5NGGC",
    ];
    usize::try_from(program_type)
        .ok()
        .and_then(|index| NAMES.get(index))
        .copied()
        .ok_or_else(|| Error::unsupported(format!("Shader GPU program type {program_type}")))
}

#[derive(Debug)]
struct SerializedShader {
    properties: Vec<SerializedProperty>,
    sub_shaders: Vec<SerializedSubShader>,
    name: String,
    custom_editor_name: String,
    fallback_name: String,
}

#[derive(Debug)]
struct SerializedProperty {
    name: String,
    description: String,
    attributes: Vec<String>,
    property_type: i32,
    default_values: [f32; 4],
    default_texture_name: String,
    texture_dimension: i32,
}

#[derive(Debug)]
struct SerializedSubShader {
    passes: Vec<SerializedPass>,
    tags: Vec<(String, String)>,
    lod: i32,
}

#[derive(Debug)]
struct SerializedPass {
    pass_type: i32,
    state: SerializedShaderState,
    programs: Vec<SerializedProgram>,
    use_name: String,
    texture_name: String,
}

#[derive(Debug)]
struct SerializedProgram {
    sub_programs: Vec<SerializedSubProgram>,
}

#[derive(Debug, Clone, Copy)]
struct SerializedSubProgram {
    blob_index: u32,
    hardware_tier: i8,
    gpu_program_type: i8,
}

#[derive(Debug)]
struct SerializedShaderState {
    name: String,
    blends: Vec<SerializedBlendState>,
    separate_blend: bool,
    z_clip: Option<f32>,
    z_test: f32,
    z_write: f32,
    culling: f32,
    offset_factor: f32,
    offset_units: f32,
    alpha_to_mask: f32,
    stencil: SerializedStencilOp,
    stencil_front: SerializedStencilOp,
    stencil_back: SerializedStencilOp,
    stencil_read_mask: f32,
    stencil_write_mask: f32,
    stencil_ref: f32,
    fog_start: f32,
    fog_end: f32,
    fog_density: f32,
    fog_color: [f32; 4],
    fog_mode: i32,
    gpu_program_id: i32,
    tags: Vec<(String, String)>,
    lod: i32,
    lighting: bool,
}

#[derive(Debug)]
struct SerializedBlendState {
    source: f32,
    destination: f32,
    source_alpha: f32,
    destination_alpha: f32,
    operation: f32,
    operation_alpha: f32,
    color_mask: f32,
}

#[derive(Debug)]
struct SerializedStencilOp {
    pass: f32,
    fail: f32,
    z_fail: f32,
    comparison: f32,
}

fn new_vec<T>(capacity: usize, field: &str) -> Result<Vec<T>> {
    let mut values = Vec::new();
    values.try_reserve_exact(capacity).map_err(|error| {
        Error::invalid_data(format!(
            "cannot allocate {capacity} {field} elements: {error}"
        ))
    })?;
    Ok(values)
}

fn singleton_arrays(values: Vec<u32>, field: &str) -> Result<Vec<Vec<u32>>> {
    let mut arrays = new_vec(values.len(), field)?;
    for value in values {
        let mut singleton = new_vec(1, field)?;
        singleton.push(value);
        arrays.push(singleton);
    }
    Ok(arrays)
}

impl ShaderObjectReader {
    fn read_count(&mut self, field: &str) -> Result<usize> {
        let count = checked_length(self.reader.read_i32()?, field)?;
        self.budget.add_array(count, self.limits, field)?;
        Ok(count)
    }

    fn read_blob(&mut self, field: &str, maximum_bytes: u64) -> Result<Vec<u8>> {
        self.read_blob_region(field, maximum_bytes)?
            .read_to_vec(maximum_bytes)
    }

    fn read_blob_region(&mut self, field: &str, maximum_bytes: u64) -> Result<Region> {
        let length = checked_length(self.reader.read_i32()?, field)?;
        let length_u64 = u64::try_from(length)
            .map_err(|_| Error::invalid_data(format!("{field} length does not fit in u64")))?;
        if length_u64 > maximum_bytes {
            return Err(Error::invalid_data(format!(
                "{field} is {length_u64} bytes, exceeding limit {maximum_bytes}"
            )));
        }
        let region = self.payload_region(length_u64, field)?;
        self.skip(length_u64, field)?;
        Ok(region)
    }

    fn read_u8_array(&mut self, field: &str) -> Result<Vec<u8>> {
        let length = self.read_count(field)?;
        self.reader.read_bytes(length)
    }

    fn skip_u16_array(&mut self, field: &str) -> Result<()> {
        let count = self.read_count(field)?;
        let bytes = u64::try_from(count)
            .map_err(|_| Error::invalid_data(format!("{field} length does not fit in u64")))?
            .checked_mul(2)
            .ok_or_else(|| Error::invalid_data(format!("{field} byte length overflowed")))?;
        self.skip(bytes, field)
    }

    fn read_u32_array(&mut self, field: &str) -> Result<Vec<u32>> {
        let count = self.read_count(field)?;
        let mut values = new_vec(count, field)?;
        for _ in 0..count {
            values.push(self.reader.read_u32()?);
        }
        Ok(values)
    }

    fn read_string_array(&mut self, field: &str) -> Result<Vec<String>> {
        let count = self.read_count(field)?;
        let mut values = new_vec(count, field)?;
        for _ in 0..count {
            values.push(self.read_aligned_string(field)?);
        }
        Ok(values)
    }

    fn read_platforms(&mut self) -> Result<Vec<i32>> {
        let raw = self.read_u32_array("Shader platform")?;
        let mut platforms = new_vec(raw.len(), "Shader platform")?;
        for value in raw {
            let platform = i32::from_ne_bytes(value.to_ne_bytes());
            validate_platform(platform)?;
            platforms.push(platform);
        }
        Ok(platforms)
    }

    fn read_u32_array_array(&mut self, field: &str) -> Result<Vec<Vec<u32>>> {
        let count = self.read_count(field)?;
        let mut arrays = new_vec(count, field)?;
        for _ in 0..count {
            arrays.push(self.read_u32_array(field)?);
        }
        Ok(arrays)
    }

    fn read_chunk_tables(&mut self, platform_count: usize) -> Result<ShaderChunkTables> {
        let (offsets, compressed, decompressed) = if self.version.components() >= (2019, 3, 0) {
            (
                self.read_u32_array_array("Shader chunk offset")?,
                self.read_u32_array_array("Shader chunk compressed length")?,
                self.read_u32_array_array("Shader chunk decompressed length")?,
            )
        } else {
            let offsets = self.read_u32_array("Shader chunk offset")?;
            let compressed = self.read_u32_array("Shader chunk compressed length")?;
            let decompressed = self.read_u32_array("Shader chunk decompressed length")?;
            (
                singleton_arrays(offsets, "Shader chunk offset")?,
                singleton_arrays(compressed, "Shader chunk compressed length")?,
                singleton_arrays(decompressed, "Shader chunk decompressed length")?,
            )
        };

        for (field, length) in [
            ("offset", offsets.len()),
            ("compressed-length", compressed.len()),
            ("decompressed-length", decompressed.len()),
        ] {
            if length != platform_count {
                return Err(Error::invalid_data(format!(
                    "Shader {field} table has {length} platforms; expected {platform_count}"
                )));
            }
        }
        for platform in 0..platform_count {
            let expected = offsets[platform].len();
            if compressed[platform].len() != expected || decompressed[platform].len() != expected {
                return Err(Error::invalid_data(format!(
                    "Shader platform {platform} chunk tables have mismatched lengths"
                )));
            }
            if expected == 0 {
                return Err(Error::invalid_data(format!(
                    "Shader platform {platform} has no compressed chunks"
                )));
            }
        }
        Ok(ShaderChunkTables {
            offsets,
            compressed_lengths: compressed,
            decompressed_lengths: decompressed,
        })
    }

    fn read_serialized_shader(&mut self) -> Result<SerializedShader> {
        let property_count = self.read_count("Shader property")?;
        let mut properties = new_vec(property_count, "Shader property")?;
        for _ in 0..property_count {
            properties.push(self.read_serialized_property()?);
        }

        let sub_shader_count = self.read_count("Shader subshader")?;
        let mut sub_shaders = new_vec(sub_shader_count, "Shader subshader")?;
        for _ in 0..sub_shader_count {
            sub_shaders.push(self.read_serialized_sub_shader()?);
        }

        if self.version.components() >= (2021, 2, 0) {
            let _keyword_names = self.read_string_array("Shader keyword name")?;
            let _keyword_flags = self.read_u8_array("Shader keyword flags")?;
            self.align(4)?;
        }

        let name = self.read_aligned_string("Serialized Shader name")?;
        let custom_editor_name = self.read_aligned_string("Shader custom editor")?;
        let fallback_name = self.read_aligned_string("Shader fallback")?;

        let dependency_count = self.read_count("Shader dependency")?;
        for _ in 0..dependency_count {
            let _from = self.read_aligned_string("Shader dependency source")?;
            let _to = self.read_aligned_string("Shader dependency target")?;
        }

        if self.version.major >= 2021 {
            let editor_count = self.read_count("Shader render-pipeline custom editor")?;
            for _ in 0..editor_count {
                let _editor = self.read_aligned_string("Shader render-pipeline custom editor")?;
                let _pipeline = self.read_aligned_string("Shader render-pipeline type")?;
            }
        }

        let _disable_no_subshaders_message = self.reader.read_bool()?;
        self.align(4)?;
        Ok(SerializedShader {
            properties,
            sub_shaders,
            name,
            custom_editor_name,
            fallback_name,
        })
    }

    fn read_serialized_property(&mut self) -> Result<SerializedProperty> {
        let name = self.read_aligned_string("Shader property name")?;
        let description = self.read_aligned_string("Shader property description")?;
        let attributes = self.read_string_array("Shader property attribute")?;
        let property_type = self.reader.read_i32()?;
        if !(0..=5).contains(&property_type) {
            return Err(Error::unsupported(format!(
                "Shader serialized property type {property_type}"
            )));
        }
        let _flags = self.reader.read_u32()?;
        let default_values = [
            self.reader.read_f32()?,
            self.reader.read_f32()?,
            self.reader.read_f32()?,
            self.reader.read_f32()?,
        ];
        let default_texture_name = self.read_aligned_string("Shader default texture")?;
        let texture_dimension = self.reader.read_i32()?;
        if !(-1..=6).contains(&texture_dimension) {
            return Err(Error::unsupported(format!(
                "Shader texture dimension {texture_dimension}"
            )));
        }
        Ok(SerializedProperty {
            name,
            description,
            attributes,
            property_type,
            default_values,
            default_texture_name,
            texture_dimension,
        })
    }

    fn read_tag_map(&mut self, field: &str) -> Result<Vec<(String, String)>> {
        let count = self.read_count(field)?;
        let mut tags = new_vec(count, field)?;
        for _ in 0..count {
            tags.push((
                self.read_aligned_string("Shader tag name")?,
                self.read_aligned_string("Shader tag value")?,
            ));
        }
        Ok(tags)
    }

    fn read_serialized_sub_shader(&mut self) -> Result<SerializedSubShader> {
        let pass_count = self.read_count("Shader pass")?;
        let mut passes = new_vec(pass_count, "Shader pass")?;
        for _ in 0..pass_count {
            passes.push(self.read_serialized_pass()?);
        }
        let tags = self.read_tag_map("Shader subshader tag")?;
        let lod = self.reader.read_i32()?;
        Ok(SerializedSubShader { passes, tags, lod })
    }

    fn read_serialized_pass(&mut self) -> Result<SerializedPass> {
        if self.version.components() >= (2020, 2, 0) {
            let hash_count = self.read_count("Shader pass editor-data hash")?;
            let hash_bytes = u64::try_from(hash_count)
                .map_err(|_| Error::invalid_data("Shader hash count does not fit in u64"))?
                .checked_mul(16)
                .ok_or_else(|| Error::invalid_data("Shader hash byte length overflowed"))?;
            self.skip(hash_bytes, "Shader pass editor-data hashes")?;
            self.align(4)?;
            let _platforms = self.read_u8_array("Shader pass platforms")?;
            self.align(4)?;
            if self.version.components() <= (2021, 1, u32::MAX) {
                self.skip_u16_array("Shader pass local keyword mask")?;
                self.align(4)?;
                self.skip_u16_array("Shader pass global keyword mask")?;
                self.align(4)?;
            }
        }

        let name_index_count = self.read_count("Shader pass name index")?;
        for _ in 0..name_index_count {
            let _name = self.read_aligned_string("Shader pass name index")?;
            let _index = self.reader.read_i32()?;
        }

        let pass_type = self.reader.read_i32()?;
        if !(0..=2).contains(&pass_type) {
            return Err(Error::unsupported(format!("Shader pass type {pass_type}")));
        }
        let state = self.read_shader_state()?;
        let _program_mask = self.reader.read_u32()?;
        let program_count = if self.version.components() >= (2019, 3, 0) {
            6
        } else {
            5
        };
        let mut programs = new_vec(program_count, "Shader stage program")?;
        for _ in 0..program_count {
            programs.push(self.read_serialized_program()?);
        }
        let _has_instancing_variant = self.reader.read_bool()?;
        if self.version.major >= 2018 {
            let _has_procedural_instancing_variant = self.reader.read_bool()?;
        }
        self.align(4)?;
        let use_name = self.read_aligned_string("Shader UsePass name")?;
        let _name = self.read_aligned_string("Shader pass name")?;
        let texture_name = self.read_aligned_string("Shader GrabPass texture")?;
        let _tags = self.read_tag_map("Shader pass tag")?;
        if self.version.major == 2021 && self.version.minor >= 2 {
            self.skip_u16_array("Shader pass serialized keyword state mask")?;
            self.align(4)?;
        }
        Ok(SerializedPass {
            pass_type,
            state,
            programs,
            use_name,
            texture_name,
        })
    }

    fn read_float_value(&mut self, field: &str) -> Result<f32> {
        let value = self.reader.read_f32()?;
        let _name = self.read_aligned_string(field)?;
        Ok(value)
    }

    fn read_blend_state(&mut self) -> Result<SerializedBlendState> {
        Ok(SerializedBlendState {
            source: self.read_float_value("Shader source blend value name")?,
            destination: self.read_float_value("Shader destination blend value name")?,
            source_alpha: self.read_float_value("Shader alpha source blend value name")?,
            destination_alpha: self
                .read_float_value("Shader alpha destination blend value name")?,
            operation: self.read_float_value("Shader blend operation value name")?,
            operation_alpha: self.read_float_value("Shader alpha blend operation value name")?,
            color_mask: self.read_float_value("Shader color mask value name")?,
        })
    }

    fn read_stencil_op(&mut self) -> Result<SerializedStencilOp> {
        Ok(SerializedStencilOp {
            pass: self.read_float_value("Shader stencil pass value name")?,
            fail: self.read_float_value("Shader stencil fail value name")?,
            z_fail: self.read_float_value("Shader stencil depth-fail value name")?,
            comparison: self.read_float_value("Shader stencil comparison value name")?,
        })
    }

    fn read_vector_value(&mut self) -> Result<[f32; 4]> {
        let value = [
            self.read_float_value("Shader vector x value name")?,
            self.read_float_value("Shader vector y value name")?,
            self.read_float_value("Shader vector z value name")?,
            self.read_float_value("Shader vector w value name")?,
        ];
        let _name = self.read_aligned_string("Shader vector value name")?;
        Ok(value)
    }

    fn read_shader_state(&mut self) -> Result<SerializedShaderState> {
        let name = self.read_aligned_string("Shader state name")?;
        let mut blends = new_vec(8, "Shader render-target blend state")?;
        for _ in 0..8 {
            blends.push(self.read_blend_state()?);
        }
        let separate_blend = self.reader.read_bool()?;
        self.align(4)?;
        let z_clip = if self.version.components() >= (2017, 2, 0) {
            Some(self.read_float_value("Shader ZClip value name")?)
        } else {
            None
        };
        let z_test = self.read_float_value("Shader ZTest value name")?;
        let z_write = self.read_float_value("Shader ZWrite value name")?;
        let culling = self.read_float_value("Shader culling value name")?;
        if self.version.major >= 2020 {
            let _conservative = self.read_float_value("Shader conservative value name")?;
        }
        let offset_factor = self.read_float_value("Shader offset-factor value name")?;
        let offset_units = self.read_float_value("Shader offset-units value name")?;
        let alpha_to_mask = self.read_float_value("Shader alpha-to-mask value name")?;
        let stencil = self.read_stencil_op()?;
        let stencil_front = self.read_stencil_op()?;
        let stencil_back = self.read_stencil_op()?;
        let stencil_read_mask = self.read_float_value("Shader stencil read-mask value name")?;
        let stencil_write_mask = self.read_float_value("Shader stencil write-mask value name")?;
        let stencil_ref = self.read_float_value("Shader stencil reference value name")?;
        let fog_start = self.read_float_value("Shader fog-start value name")?;
        let fog_end = self.read_float_value("Shader fog-end value name")?;
        let fog_density = self.read_float_value("Shader fog-density value name")?;
        let fog_color = self.read_vector_value()?;
        let fog_mode = self.reader.read_i32()?;
        if !(-1..=3).contains(&fog_mode) {
            return Err(Error::unsupported(format!("Shader fog mode {fog_mode}")));
        }
        let gpu_program_id = self.reader.read_i32()?;
        let tags = self.read_tag_map("Shader state tag")?;
        let lod = self.reader.read_i32()?;
        let lighting = self.reader.read_bool()?;
        self.align(4)?;
        Ok(SerializedShaderState {
            name,
            blends,
            separate_blend,
            z_clip,
            z_test,
            z_write,
            culling,
            offset_factor,
            offset_units,
            alpha_to_mask,
            stencil,
            stencil_front,
            stencil_back,
            stencil_read_mask,
            stencil_write_mask,
            stencil_ref,
            fog_start,
            fog_end,
            fog_density,
            fog_color,
            fog_mode,
            gpu_program_id,
            tags,
            lod,
            lighting,
        })
    }

    fn read_serialized_program(&mut self) -> Result<SerializedProgram> {
        let sub_program_count = self.read_count("Shader serialized subprogram")?;
        let mut sub_programs = new_vec(sub_program_count, "Shader serialized subprogram")?;
        for _ in 0..sub_program_count {
            sub_programs.push(self.read_serialized_sub_program()?);
        }
        let version = self.version.components();
        // 2022.2 put two more vectors between the sub-programs and the common
        // parameters. The managed definition never gained them, which is why
        // its object table refuses class 48 from 2021 on rather than reading
        // it: without these the reader walks off the structure and reports
        // whatever the following bytes say. Every shader in a real 2022.3 game
        // has them.
        if version >= (2022, 2, 0) {
            self.skip_player_sub_programs()?;
            self.skip_parameter_blob_indices()?;
        }
        if ((2020, 3, 2)..(2021, 0, 0)).contains(&version) || version >= (2021, 1, 1) {
            self.skip_program_parameters()?;
        }
        if version >= (2022, 1, 0) {
            self.skip_u16_array("Shader program serialized keyword state mask")?;
            self.align(4)?;
        }
        Ok(SerializedProgram { sub_programs })
    }

    /// `vector<vector<SerializedPlayerSubProgram>>`, 2022.2 and up.
    ///
    /// One inner list per platform. Each element is a blob index, a keyword
    /// index list, 64-bit shader requirements and a one-byte program type;
    /// both the inner and the outer array align afterwards, as every Unity
    /// array with the align flag does.
    fn skip_player_sub_programs(&mut self) -> Result<()> {
        let platforms = self.read_count("Shader player sub-program platform")?;
        for _ in 0..platforms {
            let count = self.read_count("Shader player sub-program")?;
            for _ in 0..count {
                self.skip(4, "Shader player sub-program blob index")?;
                self.skip_u16_array("Shader player sub-program keyword index")?;
                self.align(4)?;
                self.skip(8, "Shader player sub-program requirements")?;
                self.skip(1, "Shader player sub-program GPU program type")?;
                self.align(4)?;
            }
            self.align(4)?;
        }
        self.align(4)?;
        Ok(())
    }

    /// `vector<vector<u32>>`, 2022.2 and up.
    fn skip_parameter_blob_indices(&mut self) -> Result<()> {
        let platforms = self.read_count("Shader parameter blob platform")?;
        for _ in 0..platforms {
            let count = self.read_count("Shader parameter blob index")?;
            let bytes = u64::try_from(count)
                .map_err(|_| Error::invalid_data("Shader parameter blob count does not fit u64"))?
                .checked_mul(4)
                .ok_or_else(|| Error::invalid_data("Shader parameter blob length overflowed"))?;
            self.skip(bytes, "Shader parameter blob indices")?;
            self.align(4)?;
        }
        self.align(4)?;
        Ok(())
    }

    fn read_serialized_sub_program(&mut self) -> Result<SerializedSubProgram> {
        let blob_index = self.reader.read_u32()?;
        self.skip_bind_channels()?;
        let version = self.version.components();
        if ((2019, 0, 0)..(2021, 2, 0)).contains(&version) {
            self.skip_u16_array("Shader global keyword index")?;
            self.align(4)?;
            self.skip_u16_array("Shader local keyword index")?;
            self.align(4)?;
        } else {
            self.skip_u16_array("Shader keyword index")?;
            if self.version.major >= 2017 {
                self.align(4)?;
            }
        }
        let hardware_tier = self.reader.read_i8()?;
        let gpu_program_type = self.reader.read_i8()?;
        validate_gpu_program_type(gpu_program_type)?;
        self.align(4)?;
        self.skip_program_parameters()?;
        if version >= (2017, 2, 0) {
            if self.version.major >= 2021 {
                self.skip(8, "Shader requirements")?;
            } else {
                self.skip(4, "Shader requirements")?;
            }
        }
        Ok(SerializedSubProgram {
            blob_index,
            hardware_tier,
            gpu_program_type,
        })
    }

    fn skip_u32_array(&mut self, field: &str) -> Result<()> {
        let count = self.read_count(field)?;
        let bytes = u64::try_from(count)
            .map_err(|_| Error::invalid_data(format!("{field} count does not fit u64")))?
            .checked_mul(4)
            .ok_or_else(|| Error::invalid_data(format!("{field} byte length overflowed")))?;
        self.skip(bytes, field)
    }

    fn skip_bind_channels(&mut self) -> Result<()> {
        let count = self.read_count("Shader bind channel")?;
        let bytes = u64::try_from(count)
            .map_err(|_| Error::invalid_data("Shader bind-channel count does not fit in u64"))?
            .checked_mul(2)
            .ok_or_else(|| Error::invalid_data("Shader bind-channel byte length overflowed"))?;
        self.skip(bytes, "Shader bind channels")?;
        self.align(4)?;
        self.skip(4, "Shader bind-channel source map")
    }

    fn skip_vector_parameters(&mut self, field: &str) -> Result<()> {
        let count = self.read_count(field)?;
        for _ in 0..count {
            self.skip(14, field)?;
            self.align(4)?;
        }
        Ok(())
    }

    fn skip_matrix_parameters(&mut self, field: &str) -> Result<()> {
        self.skip_vector_parameters(field)
    }

    fn skip_texture_parameters(&mut self) -> Result<()> {
        let count = self.read_count("Shader texture parameter")?;
        for _ in 0..count {
            self.skip(12, "Shader texture parameter indices")?;
            if self.version.components() >= (2017, 3, 0) {
                self.skip(1, "Shader multisampled flag")?;
            }
            self.skip(1, "Shader texture dimension")?;
            self.align(4)?;
        }
        Ok(())
    }

    fn skip_buffer_bindings(&mut self, field: &str) -> Result<()> {
        let count = self.read_count(field)?;
        let element_size = if self.version.major >= 2020 { 12 } else { 8 };
        for _ in 0..count {
            self.skip(element_size, field)?;
        }
        Ok(())
    }

    fn skip_struct_parameters(&mut self) -> Result<()> {
        let count = self.read_count("Shader struct parameter")?;
        for _ in 0..count {
            self.skip(16, "Shader struct parameter header")?;
            self.skip_vector_parameters("Shader struct vector parameter")?;
            self.skip_matrix_parameters("Shader struct matrix parameter")?;
        }
        Ok(())
    }

    fn skip_constant_buffers(&mut self) -> Result<()> {
        let count = self.read_count("Shader constant buffer")?;
        for _ in 0..count {
            self.skip(4, "Shader constant-buffer name index")?;
            self.skip_matrix_parameters("Shader constant-buffer matrix parameter")?;
            self.skip_vector_parameters("Shader constant-buffer vector parameter")?;
            if self.version.components() >= (2017, 3, 0) {
                self.skip_struct_parameters()?;
            }
            self.skip(4, "Shader constant-buffer size")?;
            let version = self.version.components();
            if ((2020, 3, 2)..(2021, 0, 0)).contains(&version) || version >= (2021, 1, 4) {
                self.skip(1, "Shader partial constant-buffer flag")?;
                self.align(4)?;
            }
        }
        Ok(())
    }

    fn skip_program_parameters(&mut self) -> Result<()> {
        self.skip_vector_parameters("Shader vector parameter")?;
        self.skip_matrix_parameters("Shader matrix parameter")?;
        self.skip_texture_parameters()?;
        self.skip_buffer_bindings("Shader buffer parameter")?;
        self.skip_constant_buffers()?;
        self.skip_buffer_bindings("Shader constant-buffer binding")?;
        let uav_count = self.read_count("Shader UAV parameter")?;
        for _ in 0..uav_count {
            self.skip(12, "Shader UAV parameter")?;
        }
        if self.version.major >= 2017 {
            let sampler_count = self.read_count("Shader sampler parameter")?;
            for _ in 0..sampler_count {
                self.skip(8, "Shader sampler parameter")?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::endian::Endian;
    use crate::error::Error;
    use crate::serialized::SerializedFile;
    use crate::source::Region;
    use lz4_flex::block::compress;

    use super::{
        SHADER_CLASS_ID, SHADER_TEXT_HEADER, SerializedBlendState, SerializedPass,
        SerializedProgram, SerializedProperty, SerializedShader, SerializedShaderState,
        SerializedStencilOp, SerializedSubProgram, SerializedSubShader, ShaderChunkTables,
        ShaderProgram, ShaderReadLimits, ShaderSubProgram, convert_serialized_shader,
        decompress_shader_programs, export_shader_sub_program, grouped_sub_program_indexes,
        read_shader, validate_gpu_program_type,
    };

    #[test]
    fn groups_serialized_subprograms_in_csharp_first_seen_order() {
        let serialized = [
            SerializedSubProgram {
                blob_index: 9,
                hardware_tier: 1,
                gpu_program_type: 4,
            },
            SerializedSubProgram {
                blob_index: 2,
                hardware_tier: 2,
                gpu_program_type: 7,
            },
            SerializedSubProgram {
                blob_index: 9,
                hardware_tier: 3,
                gpu_program_type: 5,
            },
            SerializedSubProgram {
                blob_index: 9,
                hardware_tier: 4,
                gpu_program_type: 4,
            },
            SerializedSubProgram {
                blob_index: 2,
                hardware_tier: 5,
                gpu_program_type: 6,
            },
        ];

        assert_eq!(
            grouped_sub_program_indexes(&serialized, serialized.len()).unwrap(),
            [0, 3, 2, 1, 4]
        );
    }

    #[test]
    fn groups_many_unique_blobs_without_quadratic_scans() {
        const UNIQUE_BLOBS: usize = 20_000;
        let mut serialized = Vec::with_capacity(UNIQUE_BLOBS + 3);
        serialized.push(serialized_sub_program(0, 1));
        for blob_index in 1..=UNIQUE_BLOBS {
            serialized.push(serialized_sub_program(
                u32::try_from(blob_index).unwrap(),
                1,
            ));
        }
        serialized.push(serialized_sub_program(0, 2));
        serialized.push(serialized_sub_program(0, 1));

        let error = grouped_sub_program_indexes(&serialized, serialized.len() - 1).unwrap_err();
        assert!(matches!(error, Error::InvalidData(message) if message.contains("grouping")));

        let grouped = grouped_sub_program_indexes(&serialized, serialized.len()).unwrap();

        assert_eq!(grouped.len(), serialized.len());
        assert_eq!(grouped[..3], [0, UNIQUE_BLOBS + 2, UNIQUE_BLOBS + 1]);
        assert!(grouped[3..].iter().copied().eq(1..=UNIQUE_BLOBS));
    }

    #[test]
    fn indexes_many_segmented_entries_and_preserves_program_slots() {
        const ENTRY_COUNT: usize = 20_000;
        const LAST_SEGMENT: usize = 19_999;
        let version = "2022.3.0f1".parse().unwrap();
        let record = shader_sub_program_record(202_012_090, 1, &[], &[], b"slot");
        let entries = vec![(0, record.len(), LAST_SEGMENT); ENTRY_COUNT];
        let table = shader_program_segment(&entries, &[], true);
        let mut budget = super::ParseBudget::default();
        let limits = ShaderReadLimits::default();
        let mut program = ShaderProgram::read_table(&table, &version, limits, &mut budget).unwrap();

        for segment in 0..LAST_SEGMENT {
            program
                .read_segment(&[], segment, &version, limits, &mut budget, false)
                .unwrap();
        }
        program
            .read_segment(&record, LAST_SEGMENT, &version, limits, &mut budget, false)
            .unwrap();
        program.ensure_complete(LAST_SEGMENT + 1, false).unwrap();

        assert_eq!(program.sub_programs.len(), ENTRY_COUNT);
        assert!(program.sub_programs.iter().all(|sub_program| {
            sub_program
                .as_ref()
                .is_some_and(|sub_program| sub_program.program_code == b"slot")
        }));
    }

    #[test]
    fn unknown_and_spirv_programs_emit_diagnostics_instead_of_failing_the_shader() {
        validate_gpu_program_type(0).unwrap();
        for (program_type, expected) in [
            (0, "//shader disassembly not supported on Unknown"),
            (
                25,
                "// disassembly error SPIR-V conversion is not implemented in native Rust\n",
            ),
        ] {
            let program = ShaderSubProgram {
                program_type,
                keywords: Vec::new(),
                local_keywords: Vec::new(),
                program_code: vec![1, 2, 3, 4],
            };
            let mut output = super::BoundedText::for_shader_output(1024).unwrap();
            export_shader_sub_program(&program, &mut output).unwrap();
            assert_eq!(
                String::from_utf8(output.into_bytes()).unwrap(),
                format!("\"{expected}\"")
            );
        }
    }

    #[test]
    fn parses_nonempty_unknown_and_spirv_programs_end_to_end() {
        for (program_type, diagnostic) in [
            (0, "//shader disassembly not supported on Unknown"),
            (
                25,
                "// disassembly error SPIR-V conversion is not implemented in native Rust\n",
            ),
        ] {
            let record =
                shader_sub_program_record(201_510_240, program_type, &[], &[], b"not empty");
            let segment = shader_program_segment(&[(12, record.len(), 0)], &[&record], false);
            let script = b"Shader \"Tolerant\" {\nGpuProgramIndex 0\n}";
            let object = subprogram_shader_object("Tolerant", script, "Tolerant.shader", &segment);
            let file = parse_asset("5.4.6f3", 13, &object, Endian::Little);

            let shader = read_shader(&file, 0, ShaderReadLimits::default()).unwrap();
            let expected_script = format!("Shader \"Tolerant\" {{\n\"{diagnostic}\"\n}}");
            let mut expected = SHADER_TEXT_HEADER.as_bytes().to_vec();
            expected.extend_from_slice(expected_script.as_bytes());
            assert_eq!(
                shader.read_to_vec(shader.output_len().unwrap()).unwrap(),
                expected
            );

            let bounded = ShaderReadLimits {
                maximum_output_bytes: shader.output_len().unwrap() - 1,
                ..ShaderReadLimits::default()
            };
            let error = read_shader(&file, 0, bounded).unwrap_err();
            assert!(
                matches!(error, Error::InvalidData(message) if message.contains("output limit"))
            );
        }
    }

    #[test]
    fn streams_unity_4_script_with_the_exact_csharp_header() {
        let script = b"Shader \"Legacy\" {\n\xff}\n";
        let object = direct_shader_object("Legacy", script, "Legacy.shader", Endian::Little, false);
        let file = parse_asset("4.7.2f1", 13, &object, Endian::Little);

        let shader = read_shader(&file, 0, ShaderReadLimits::default()).unwrap();

        assert_eq!(shader.path_id, 7);
        assert_eq!(shader.name, "Legacy");
        assert_eq!(shader.path_name, "Legacy.shader");
        assert_eq!(shader.payload_kind(), "shader_text");
        assert_eq!(shader.suggested_extension(), ".shader");
        let mut expected = SHADER_TEXT_HEADER.as_bytes().to_vec();
        expected.extend_from_slice(script);
        assert_eq!(shader.read_to_vec(1024).unwrap(), expected);
    }

    #[test]
    fn reads_big_endian_unity_5_2_no_target_layout() {
        let script = b"Shader \"BigEndian\" {}";
        let object = direct_shader_object(
            "BigEndian",
            script,
            "Assets/BigEndian.shader",
            Endian::Big,
            true,
        );
        let file = parse_asset("5.2.7f1", NO_TARGET, &object, Endian::Big);

        let shader = read_shader(&file, 0, ShaderReadLimits::default()).unwrap();

        assert_eq!(shader.name, "BigEndian");
        assert_eq!(shader.script.read_to_vec(1024).unwrap(), script);
        let mut streamed = Vec::new();
        assert_eq!(
            shader.write_to(&mut streamed).unwrap(),
            shader.output_len().unwrap()
        );
        assert!(streamed.ends_with(script));
    }

    #[test]
    fn converts_unity_5_4_subprogram_indices_byte_for_byte() {
        let record = shader_sub_program_record(201_510_240, 1, &["LEGACY"], &[], b"void main() {}");
        let segment = shader_program_segment(&[(12, record.len(), 0)], &[&record], false);
        let script = b"Shader \"Legacy\" {\nGpuProgramIndex 0\n}";
        let object = subprogram_shader_object("Legacy", script, "Legacy.shader", &segment);
        let file = parse_asset("5.4.6f3", 13, &object, Endian::Little);

        let shader = read_shader(&file, 0, ShaderReadLimits::default()).unwrap();

        let expected_script = concat!(
            "Shader \"Legacy\" {\n",
            "Keywords { \"LEGACY\" }\n",
            "\"void main() {}\"\n",
            "}"
        );
        let mut expected = SHADER_TEXT_HEADER.as_bytes().to_vec();
        expected.extend_from_slice(expected_script.as_bytes());
        assert_eq!(shader.path_name, "Legacy.shader");
        assert_eq!(shader.read_to_vec(4096).unwrap(), expected);
    }

    #[test]
    fn converts_unity_5_5_flat_compressed_table_byte_for_byte() {
        let segment = shader_program_segment(&[], &[], false);
        let object = serialized_shader_object("5.5.0f3", "ObjectName", "Parsed/Flat", &[segment]);
        let file = parse_asset("5.5.0f3", 13, &object, Endian::Little);

        let shader = read_shader(&file, 0, ShaderReadLimits::default()).unwrap();

        let mut expected = SHADER_TEXT_HEADER.as_bytes().to_vec();
        expected.extend_from_slice(b"Shader \"Parsed/Flat\" {\nProperties {\n}\n}");
        assert_eq!(shader.name, "ObjectName");
        assert_eq!(shader.read_to_vec(4096).unwrap(), expected);
    }

    #[test]
    fn converts_unity_5_5_serialized_pass_end_to_end() {
        let object = serialized_shader_with_pass_5_5();
        let file = parse_asset("5.5.0f3", 13, &object, Endian::Little);

        let shader = read_shader(&file, 0, ShaderReadLimits::default()).unwrap();

        let expected_script = concat!(
            "Shader \"Parsed/Pass\" {\n",
            "Properties {\n",
            "[Toggle] _Value (\"Value\", Float) = 0.25\n",
            "}\n",
            "SubShader {\n",
            " LOD 100\n",
            " Tags { \"RenderType\" = \"Opaque\" }\n",
            " Pass {\n",
            "  Name \"FORWARD\"\n",
            "  Tags { \"LightMode\" = \"ForwardBase\" }\n",
            "  ZClip Off\n",
            "  GpuProgramID 7\n",
            "Program \"vp\" {\n",
            "SubProgram \"openGL \" {\n",
            "Keywords { \"KW\" }\n",
            "\"void main() {}\"\n",
            "}\n",
            "}\n",
            "}\n",
            "}\n",
            "Fallback \"Diffuse\"\n",
            "CustomEditor \"Example.Editor\"\n",
            "}"
        );
        let mut expected = SHADER_TEXT_HEADER.as_bytes().to_vec();
        expected.extend_from_slice(expected_script.as_bytes());
        assert_eq!(shader.read_to_vec(16 * 1024).unwrap(), expected);
    }

    #[test]
    fn reads_segmented_compressed_programs() {
        // 2020.3 rather than 2022: the segmented blob arrived in 2019.3, so
        // this still covers it, and shaders from 2021 on are refused outright.
        let record = shader_sub_program_record(202_012_090, 1, &["MODERN"], &[], b"modern source");
        let first = shader_program_segment(&[(0, record.len(), 1)], &[], true);
        let object = serialized_shader_object(
            "2020.3.48f1",
            "ModernObject",
            "Parsed/Segmented",
            &[first, record],
        );
        let file = parse_asset("2020.3.48f1", 13, &object, Endian::Little);

        let shader = read_shader(&file, 0, ShaderReadLimits::default()).unwrap();

        let mut expected = SHADER_TEXT_HEADER.as_bytes().to_vec();
        expected.extend_from_slice(b"Shader \"Parsed/Segmented\" {\nProperties {\n}\n}");
        assert_eq!(shader.read_to_vec(4096).unwrap(), expected);
    }

    #[test]
    fn reads_the_fields_unity_2022_added_to_the_shader() {
        // Unity 2022.2 added `stageCounts` after the compressed blob and
        // `m_PlayerSubPrograms` plus `m_ParameterBlobIndices` inside each
        // program. The managed definition gained none of them, which is why
        // its object table skips class 48 from 2021 on rather than reading it
        // -- and why this reader, which had copied that definition, turned
        // every shader in a real 2022.3 game into nonsense instead of a
        // shader.
        //
        // The fixture carries them, so the reader has to consume them. A
        // fixture without them is a file Unity does not write, and that is
        // exactly what the previous one was.
        for revision in ["2022.2.0f1", "2022.3.62f1", "6000.2.0f1"] {
            let record =
                shader_sub_program_record(202_012_090, 1, &["MODERN"], &[], b"modern source");
            let first = shader_program_segment(&[(0, record.len(), 1)], &[], true);
            let object =
                serialized_shader_object(revision, "Object", "Parsed/Modern", &[first, record]);
            let file = parse_asset(revision, 13, &object, Endian::Little);

            let shader = read_shader(&file, 0, ShaderReadLimits::default())
                .unwrap_or_else(|error| panic!("{revision}: {error}"));
            let text = shader.read_to_vec(4096).unwrap();
            let text = String::from_utf8(text).unwrap();
            assert!(
                text.contains("Shader \"Parsed/Modern\""),
                "{revision} produced {text}"
            );
        }

        // And the stage count is genuinely read rather than skipped past by
        // luck: a fixture that omits it no longer lines up.
        let record = shader_sub_program_record(202_012_090, 1, &[], &[], b"source");
        let first = shader_program_segment(&[(0, record.len(), 1)], &[], true);
        let without = serialized_shader_object("2022.1.0f1", "Object", "Parsed", &[first, record]);
        let file = parse_asset("2022.3.62f1", 13, &without, Endian::Little);
        assert!(
            read_shader(&file, 0, ShaderReadLimits::default()).is_err(),
            "a 2022.3 reader must not accept a body written without the 2022.2 fields"
        );
    }

    #[test]
    fn serialized_shader_converter_matches_csharp_text_bytes() {
        let shader = SerializedShader {
            properties: vec![SerializedProperty {
                name: "_Gloss".to_owned(),
                description: "Gloss".to_owned(),
                attributes: vec!["HDR".to_owned()],
                property_type: 3,
                default_values: [0.5, 0.0, 1.0, 0.0],
                default_texture_name: String::new(),
                texture_dimension: 0,
            }],
            sub_shaders: vec![SerializedSubShader {
                passes: vec![SerializedPass {
                    pass_type: 0,
                    state: default_shader_state(),
                    programs: vec![
                        SerializedProgram {
                            sub_programs: vec![SerializedSubProgram {
                                blob_index: 0,
                                hardware_tier: 0,
                                gpu_program_type: 1,
                            }],
                        },
                        SerializedProgram {
                            sub_programs: Vec::new(),
                        },
                        SerializedProgram {
                            sub_programs: Vec::new(),
                        },
                        SerializedProgram {
                            sub_programs: Vec::new(),
                        },
                        SerializedProgram {
                            sub_programs: Vec::new(),
                        },
                    ],
                    use_name: String::new(),
                    texture_name: String::new(),
                }],
                tags: vec![("RenderType".to_owned(), "Opaque".to_owned())],
                lod: 100,
            }],
            name: "Modern/Text".to_owned(),
            custom_editor_name: String::new(),
            fallback_name: String::new(),
        };
        let programs = vec![ShaderProgram {
            entries: Vec::new(),
            entries_by_segment: Vec::new(),
            sub_programs: vec![Some(ShaderSubProgram {
                program_type: 1,
                keywords: vec!["FOO".to_owned()],
                local_keywords: Vec::new(),
                program_code: b"void main() {}".to_vec(),
            })],
            undecoded: vec![None],
        }];

        let limits = ShaderReadLimits {
            maximum_output_bytes: 16 * 1024,
            ..ShaderReadLimits::default()
        };
        let converted = convert_serialized_shader(&shader, &[0], &programs, limits).unwrap();

        let expected = concat!(
            "Shader \"Modern/Text\" {\n",
            "Properties {\n",
            "[HDR] _Gloss (\"Gloss\", Range(0, 1)) = 0.5\n",
            "}\n",
            "SubShader {\n",
            " LOD 100\n",
            " Tags { \"RenderType\" = \"Opaque\" }\n",
            " Pass {\n",
            "  Tags { \"LightMode\" = \"Forward\" }\n",
            "  GpuProgramID 7\n",
            "Program \"vp\" {\n",
            "SubProgram \"openGL \" {\n",
            "Keywords { \"FOO\" }\n",
            "\"void main() {}\"\n",
            "}\n",
            "}\n",
            "}\n",
            "}\n",
            "}"
        );
        assert_eq!(converted, expected.as_bytes());
    }

    #[test]
    fn rejects_damaged_compressed_chunks_and_unknown_program_types() {
        let segment = shader_program_segment(&[], &[], false);
        let compressed = compress(&segment);
        let mut damaged = serialized_shader_object("5.5.0f3", "Bad", "Bad", &[segment]);
        let compressed_offset = damaged
            .windows(compressed.len())
            .position(|window| window == compressed)
            .unwrap();
        damaged[compressed_offset] ^= 0xff;
        let damaged_file = parse_asset("5.5.0f3", 13, &damaged, Endian::Little);
        assert!(read_shader(&damaged_file, 0, ShaderReadLimits::default()).is_err());

        let record = shader_sub_program_record(202_012_090, 99, &[], &[], b"bad");
        let first = shader_program_segment(&[(0, record.len(), 1)], &[], true);
        let object = serialized_shader_object("2020.3.0f1", "BadType", "BadType", &[first, record]);
        let file = parse_asset("2020.3.0f1", 13, &object, Endian::Little);
        let error = read_shader(&file, 0, ShaderReadLimits::default()).unwrap_err();
        assert!(matches!(error, Error::Unsupported(message) if message.contains("type 99")));

        let version = "2022.3.0f1".parse().unwrap();
        let tables = ShaderChunkTables {
            offsets: vec![vec![u32::MAX]],
            compressed_lengths: vec![vec![1]],
            decompressed_lengths: vec![vec![4]],
        };
        let mut budget = super::ParseBudget::default();
        let blob = Region::from_bytes([0]);
        let error = decompress_shader_programs(
            &[0],
            &tables,
            &blob,
            &version,
            ShaderReadLimits::default(),
            &mut budget,
        )
        .unwrap_err();
        assert!(matches!(error, Error::InvalidData(message) if message.contains("exceeds blob")));
    }

    #[test]
    fn enforces_modern_blob_decompression_program_and_array_limits() {
        let object = serialized_shader_with_pass_5_5();
        let file = parse_asset("5.5.0f3", 13, &object, Endian::Little);

        let compressed = ShaderReadLimits {
            maximum_compressed_blob_bytes: 0,
            ..ShaderReadLimits::default()
        };
        assert!(read_shader(&file, 0, compressed).is_err());

        let decompressed = ShaderReadLimits {
            maximum_total_decompressed_bytes: 4,
            ..ShaderReadLimits::default()
        };
        assert!(read_shader(&file, 0, decompressed).is_err());

        let program = ShaderReadLimits {
            maximum_program_code_bytes: 1,
            ..ShaderReadLimits::default()
        };
        assert!(read_shader(&file, 0, program).is_err());

        let arrays = ShaderReadLimits {
            maximum_array_elements: 0,
            ..ShaderReadLimits::default()
        };
        assert!(read_shader(&file, 0, arrays).is_err());
    }

    #[test]
    fn rejects_unknown_unity_and_gpu_record_versions() {
        let future = parse_asset("7000.0.0f1", 13, &[], Endian::Little);
        let error = read_shader(&future, 0, ShaderReadLimits::default()).unwrap_err();
        assert!(matches!(error, Error::Unsupported(message) if message.contains("7000")));

        let record = shader_sub_program_record(123_456_789, 1, &[], &[], b"bad");
        let first = shader_program_segment(&[(0, record.len(), 1)], &[], true);
        let object =
            serialized_shader_object("2020.3.0f1", "BadVersion", "BadVersion", &[first, record]);
        let file = parse_asset("2020.3.0f1", 13, &object, Endian::Little);
        let error = read_shader(&file, 0, ShaderReadLimits::default()).unwrap_err();
        assert!(matches!(error, Error::Unsupported(message) if message.contains("123456789")));
    }

    #[test]
    fn enforces_string_script_and_output_limits() {
        let object = direct_shader_object("limit", b"12345", "p", Endian::Little, false);
        let file = parse_asset("5.2.0f3", 13, &object, Endian::Little);

        let string_limit = ShaderReadLimits {
            maximum_string_bytes: 4,
            ..ShaderReadLimits::default()
        };
        assert!(read_shader(&file, 0, string_limit).is_err());

        let total_string_limit = ShaderReadLimits {
            maximum_string_bytes: 8,
            maximum_total_string_bytes: 5,
            ..ShaderReadLimits::default()
        };
        assert!(read_shader(&file, 0, total_string_limit).is_err());

        let script_limit = ShaderReadLimits {
            maximum_script_bytes: 4,
            ..ShaderReadLimits::default()
        };
        assert!(read_shader(&file, 0, script_limit).is_err());

        let output_limit = ShaderReadLimits {
            maximum_output_bytes: u64::try_from(SHADER_TEXT_HEADER.len()).unwrap() + 4,
            ..ShaderReadLimits::default()
        };
        assert!(read_shader(&file, 0, output_limit).is_err());

        let shader = read_shader(&file, 0, ShaderReadLimits::default()).unwrap();
        assert!(
            shader
                .read_to_vec(shader.output_len().unwrap() - 1)
                .is_err()
        );
    }

    const NO_TARGET: i32 = -2;

    fn direct_shader_object(
        name: &str,
        script: &[u8],
        path: &str,
        endian: Endian,
        no_target: bool,
    ) -> Vec<u8> {
        let mut output = Vec::new();
        if no_target {
            push_u32(&mut output, 0, endian);
            for _ in 0..2 {
                push_i32(&mut output, 0, endian);
                push_i64(&mut output, 0, endian);
            }
        }
        push_aligned_string(&mut output, name, endian);
        push_i32(&mut output, i32::try_from(script.len()).unwrap(), endian);
        output.extend_from_slice(script);
        align(&mut output, 4);
        push_aligned_string(&mut output, path, endian);
        output
    }

    fn subprogram_shader_object(
        name: &str,
        script: &[u8],
        path: &str,
        decompressed: &[u8],
    ) -> Vec<u8> {
        let mut output = Vec::new();
        push_aligned_string(&mut output, name, Endian::Little);
        push_i32(
            &mut output,
            i32::try_from(script.len()).unwrap(),
            Endian::Little,
        );
        output.extend_from_slice(script);
        align(&mut output, 4);
        push_aligned_string(&mut output, path, Endian::Little);
        push_u32(
            &mut output,
            u32::try_from(decompressed.len()).unwrap(),
            Endian::Little,
        );
        let compressed = compress(decompressed);
        push_i32(
            &mut output,
            i32::try_from(compressed.len()).unwrap(),
            Endian::Little,
        );
        output.extend_from_slice(&compressed);
        output
    }

    fn serialized_shader_object(
        unity_version: &str,
        object_name: &str,
        parsed_name: &str,
        segments: &[Vec<u8>],
    ) -> Vec<u8> {
        let modern_2019 = unity_version.starts_with("2019.")
            || unity_version.starts_with("202")
            || unity_version.starts_with("6000.");
        let modern_2018 = modern_2019 || unity_version.starts_with("2018.");
        let modern_2021 = unity_version.starts_with("2021.")
            || unity_version.starts_with("2022.")
            || unity_version.starts_with("2023.")
            || unity_version.starts_with("6000.");
        let keyword_layout = unity_version.starts_with("2022.")
            || unity_version.starts_with("2023.")
            || unity_version.starts_with("6000.")
            || unity_version.starts_with("2021.2")
            || unity_version.starts_with("2021.3");
        // 2022.2 added a per-platform stage count after the blob. Fixtures that
        // omit it describe a file Unity does not write, which is how the two
        // missing fields went unnoticed for so long.
        let modern_2022_2 = unity_version.starts_with("2022.2")
            || unity_version.starts_with("2022.3")
            || unity_version.starts_with("2023.")
            || unity_version.starts_with("6000.");

        let mut output = Vec::new();
        push_aligned_string(&mut output, object_name, Endian::Little);
        push_i32(&mut output, 0, Endian::Little); // properties
        push_i32(&mut output, 0, Endian::Little); // subshaders
        if keyword_layout {
            push_i32(&mut output, 0, Endian::Little); // keyword names
            push_i32(&mut output, 0, Endian::Little); // keyword flags
        }
        push_aligned_string(&mut output, parsed_name, Endian::Little);
        push_aligned_string(&mut output, "", Endian::Little);
        push_aligned_string(&mut output, "", Endian::Little);
        push_i32(&mut output, 0, Endian::Little); // dependencies
        if modern_2021 {
            push_i32(&mut output, 0, Endian::Little); // pipeline editors
        }
        output.push(0); // disable no subshaders message
        align(&mut output, 4);

        push_i32(&mut output, 1, Endian::Little); // platforms
        push_u32(&mut output, 0, Endian::Little); // GL
        let compressed: Vec<Vec<u8>> = segments.iter().map(|segment| compress(segment)).collect();
        push_blob_tables(&mut output, segments, &compressed, modern_2019);
        let blob_length: usize = compressed.iter().map(Vec::len).sum();
        push_i32(
            &mut output,
            i32::try_from(blob_length).unwrap(),
            Endian::Little,
        );
        for segment in compressed {
            output.extend_from_slice(&segment);
        }
        align(&mut output, 4);
        if modern_2022_2 {
            push_u32_array(&mut output, &[1]); // stage counts
        }
        push_i32(&mut output, 0, Endian::Little); // object dependencies
        if modern_2018 {
            push_i32(&mut output, 0, Endian::Little); // non-modifiable textures
        }
        output.push(0); // shader is baked
        align(&mut output, 4);
        if unity_version.starts_with("6000.") {
            output.extend_from_slice(&[0; 16]); // asset GUID
        }
        output
    }

    /// The offset, compressed-length and decompressed-length tables, which are
    /// nested one level per platform from 2019.3 on.
    fn push_blob_tables(
        output: &mut Vec<u8>,
        segments: &[Vec<u8>],
        compressed: &[Vec<u8>],
        modern_2019: bool,
    ) {
        let mut offsets = Vec::new();
        let mut cursor = 0_u32;
        for segment in compressed {
            offsets.push(cursor);
            cursor = cursor
                .checked_add(u32::try_from(segment.len()).unwrap())
                .unwrap();
        }
        if modern_2019 {
            push_nested_u32_array(output, &offsets);
            push_nested_u32_array(
                output,
                &compressed
                    .iter()
                    .map(|value| u32::try_from(value.len()).unwrap())
                    .collect::<Vec<_>>(),
            );
            push_nested_u32_array(
                output,
                &segments
                    .iter()
                    .map(|value| u32::try_from(value.len()).unwrap())
                    .collect::<Vec<_>>(),
            );
        } else {
            assert_eq!(segments.len(), 1);
            push_u32_array(output, &offsets);
            push_u32_array(
                output,
                &compressed
                    .iter()
                    .map(|value| u32::try_from(value.len()).unwrap())
                    .collect::<Vec<_>>(),
            );
            push_u32_array(
                output,
                &segments
                    .iter()
                    .map(|value| u32::try_from(value.len()).unwrap())
                    .collect::<Vec<_>>(),
            );
        }
    }

    fn serialized_shader_with_pass_5_5() -> Vec<u8> {
        let record = shader_sub_program_record(201_608_170, 1, &["KW"], &[], b"void main() {}");
        let segment = shader_program_segment(&[(12, record.len(), 0)], &[&record], false);
        let compressed = compress(&segment);
        let mut output = Vec::new();
        push_aligned_string(&mut output, "PassObject", Endian::Little);

        push_i32(&mut output, 1, Endian::Little); // properties
        push_aligned_string(&mut output, "_Value", Endian::Little);
        push_aligned_string(&mut output, "Value", Endian::Little);
        push_i32(&mut output, 1, Endian::Little);
        push_aligned_string(&mut output, "Toggle", Endian::Little);
        push_i32(&mut output, 2, Endian::Little); // Float
        push_u32(&mut output, 0, Endian::Little);
        for value in [0.25_f32, 0.0, 0.0, 0.0] {
            push_f32(&mut output, value, Endian::Little);
        }
        push_aligned_string(&mut output, "", Endian::Little);
        push_i32(&mut output, 0, Endian::Little);

        push_i32(&mut output, 1, Endian::Little); // subshaders
        push_i32(&mut output, 1, Endian::Little); // passes
        push_i32(&mut output, 0, Endian::Little); // name indices
        push_i32(&mut output, 0, Endian::Little); // normal pass
        push_default_shader_state_5_5(&mut output);
        push_u32(&mut output, 0, Endian::Little); // program mask
        push_serialized_program_5_5(&mut output, true);
        for _ in 0..4 {
            push_serialized_program_5_5(&mut output, false);
        }
        output.push(0); // has instancing variant
        align(&mut output, 4);
        for _ in 0..3 {
            push_aligned_string(&mut output, "", Endian::Little);
        }
        push_i32(&mut output, 0, Endian::Little); // pass tags
        push_i32(&mut output, 1, Endian::Little); // subshader tags
        push_aligned_string(&mut output, "RenderType", Endian::Little);
        push_aligned_string(&mut output, "Opaque", Endian::Little);
        push_i32(&mut output, 100, Endian::Little);

        push_aligned_string(&mut output, "Parsed/Pass", Endian::Little);
        push_aligned_string(&mut output, "Example.Editor", Endian::Little);
        push_aligned_string(&mut output, "Diffuse", Endian::Little);
        push_i32(&mut output, 0, Endian::Little); // dependencies
        output.push(0);
        align(&mut output, 4);

        push_i32(&mut output, 1, Endian::Little); // platforms
        push_u32(&mut output, 0, Endian::Little);
        push_u32_array(&mut output, &[0]);
        push_u32_array(&mut output, &[u32::try_from(compressed.len()).unwrap()]);
        push_u32_array(&mut output, &[u32::try_from(segment.len()).unwrap()]);
        push_i32(
            &mut output,
            i32::try_from(compressed.len()).unwrap(),
            Endian::Little,
        );
        output.extend_from_slice(&compressed);
        align(&mut output, 4);
        push_i32(&mut output, 0, Endian::Little); // object dependencies
        output.push(0); // shader is baked
        align(&mut output, 4);
        output
    }

    fn push_default_shader_state_5_5(output: &mut Vec<u8>) {
        push_aligned_string(output, "FORWARD", Endian::Little);
        for _ in 0..8 {
            for value in [1.0_f32, 0.0, 1.0, 0.0, 0.0, 0.0, 15.0] {
                push_float_value(output, value);
            }
        }
        output.push(0); // separate blend
        align(output, 4);
        for value in [4.0_f32, 1.0, 2.0, 0.0, 0.0, 0.0] {
            push_float_value(output, value);
        }
        for _ in 0..3 {
            for value in [0.0_f32, 0.0, 0.0, 8.0] {
                push_float_value(output, value);
            }
        }
        for value in [255.0_f32, 255.0, 0.0, 0.0, 0.0, 0.0] {
            push_float_value(output, value);
        }
        for _ in 0..4 {
            push_float_value(output, 0.0);
        }
        push_aligned_string(output, "", Endian::Little); // vector name
        push_i32(output, -1, Endian::Little); // fog mode
        push_i32(output, 7, Endian::Little); // GPU program ID
        push_i32(output, 1, Endian::Little); // state tags
        push_aligned_string(output, "LightMode", Endian::Little);
        push_aligned_string(output, "ForwardBase", Endian::Little);
        push_i32(output, 0, Endian::Little); // LOD
        output.push(0); // lighting
        align(output, 4);
    }

    fn push_float_value(output: &mut Vec<u8>, value: f32) {
        push_f32(output, value, Endian::Little);
        push_aligned_string(output, "", Endian::Little);
    }

    fn push_serialized_program_5_5(output: &mut Vec<u8>, populated: bool) {
        push_i32(output, i32::from(populated), Endian::Little);
        if !populated {
            return;
        }
        push_u32(output, 0, Endian::Little); // blob index
        push_i32(output, 0, Endian::Little); // bind channels
        push_u32(output, 0, Endian::Little); // source map
        push_i32(output, 0, Endian::Little); // keyword indices
        output.push(0); // tier
        output.push(1); // GLLegacy
        align(output, 4);
        for _ in 0..7 {
            push_i32(output, 0, Endian::Little); // program parameter arrays
        }
    }

    fn push_u32_array(output: &mut Vec<u8>, values: &[u32]) {
        push_i32(output, i32::try_from(values.len()).unwrap(), Endian::Little);
        for value in values {
            push_u32(output, *value, Endian::Little);
        }
    }

    fn push_nested_u32_array(output: &mut Vec<u8>, values: &[u32]) {
        push_i32(output, 1, Endian::Little);
        push_u32_array(output, values);
    }

    fn shader_program_segment(
        entries: &[(usize, usize, usize)],
        inline_records: &[&[u8]],
        segmented: bool,
    ) -> Vec<u8> {
        let mut output = Vec::new();
        push_i32(
            &mut output,
            i32::try_from(entries.len()).unwrap(),
            Endian::Little,
        );
        for (offset, length, segment) in entries {
            push_i32(&mut output, i32::try_from(*offset).unwrap(), Endian::Little);
            push_i32(&mut output, i32::try_from(*length).unwrap(), Endian::Little);
            if segmented {
                push_i32(
                    &mut output,
                    i32::try_from(*segment).unwrap(),
                    Endian::Little,
                );
            }
        }
        for record in inline_records {
            output.extend_from_slice(record);
        }
        output
    }

    fn shader_sub_program_record(
        version: i32,
        program_type: i32,
        keywords: &[&str],
        local_keywords: &[&str],
        code: &[u8],
    ) -> Vec<u8> {
        let mut output = Vec::new();
        push_i32(&mut output, version, Endian::Little);
        push_i32(&mut output, program_type, Endian::Little);
        output.extend_from_slice(&[0; 12]);
        if version >= 201_608_170 {
            output.extend_from_slice(&[0; 4]);
        }
        push_i32(
            &mut output,
            i32::try_from(keywords.len()).unwrap(),
            Endian::Little,
        );
        for keyword in keywords {
            push_aligned_string(&mut output, keyword, Endian::Little);
        }
        if (201_806_140..202_012_090).contains(&version) {
            push_i32(
                &mut output,
                i32::try_from(local_keywords.len()).unwrap(),
                Endian::Little,
            );
            for keyword in local_keywords {
                push_aligned_string(&mut output, keyword, Endian::Little);
            }
        }
        push_i32(
            &mut output,
            i32::try_from(code.len()).unwrap(),
            Endian::Little,
        );
        output.extend_from_slice(code);
        align(&mut output, 4);
        output
    }

    const fn serialized_sub_program(blob_index: u32, gpu_program_type: i8) -> SerializedSubProgram {
        SerializedSubProgram {
            blob_index,
            hardware_tier: 0,
            gpu_program_type,
        }
    }

    fn default_shader_state() -> SerializedShaderState {
        let blends = (0..8)
            .map(|_| SerializedBlendState {
                source: 1.0,
                destination: 0.0,
                source_alpha: 1.0,
                destination_alpha: 0.0,
                operation: 0.0,
                operation_alpha: 0.0,
                color_mask: 15.0,
            })
            .collect();
        let stencil = || SerializedStencilOp {
            pass: 0.0,
            fail: 0.0,
            z_fail: 0.0,
            comparison: 8.0,
        };
        SerializedShaderState {
            name: String::new(),
            blends,
            separate_blend: false,
            z_clip: Some(1.0),
            z_test: 4.0,
            z_write: 1.0,
            culling: 2.0,
            offset_factor: 0.0,
            offset_units: 0.0,
            alpha_to_mask: 0.0,
            stencil: stencil(),
            stencil_front: stencil(),
            stencil_back: stencil(),
            stencil_read_mask: 255.0,
            stencil_write_mask: 255.0,
            stencil_ref: 0.0,
            fog_start: 0.0,
            fog_end: 0.0,
            fog_density: 0.0,
            fog_color: [0.0; 4],
            fog_mode: -1,
            gpu_program_id: 7,
            tags: vec![("LightMode".to_owned(), "Forward".to_owned())],
            lod: 0,
            lighting: false,
        }
    }

    fn parse_asset(
        unity_version: &str,
        target_platform: i32,
        object: &[u8],
        endian: Endian,
    ) -> SerializedFile {
        SerializedFile::open(Region::from_bytes(synthetic_v22_asset(
            unity_version,
            target_platform,
            object,
            endian,
        )))
        .unwrap()
    }

    fn synthetic_v22_asset(
        unity_version: &str,
        target_platform: i32,
        object: &[u8],
        endian: Endian,
    ) -> Vec<u8> {
        let mut metadata = Vec::new();
        metadata.extend_from_slice(unity_version.as_bytes());
        metadata.push(0);
        push_i32(&mut metadata, target_platform, endian);
        metadata.push(0);
        push_i32(&mut metadata, 1, endian);
        push_i32(&mut metadata, SHADER_CLASS_ID, endian);
        metadata.push(0);
        push_i16(&mut metadata, -1, endian);
        metadata.extend_from_slice(&[0_u8; 16]);
        push_i32(&mut metadata, 1, endian);
        align_with_base(&mut metadata, 48, 4);
        push_i64(&mut metadata, 7, endian);
        push_i64(&mut metadata, 0, endian);
        push_u32(&mut metadata, u32::try_from(object.len()).unwrap(), endian);
        push_i32(&mut metadata, 0, endian);
        for _ in 0..3 {
            push_i32(&mut metadata, 0, endian);
        }
        metadata.push(0);

        let metadata_size = u32::try_from(metadata.len()).unwrap();
        let metadata_end = 48_u64 + u64::from(metadata_size);
        let data_offset = metadata_end.div_ceil(16) * 16;
        let file_size = data_offset + u64::try_from(object.len()).unwrap();
        let mut bytes = vec![0_u8; 48];
        bytes[8..12].copy_from_slice(&22_u32.to_be_bytes());
        bytes[16] = u8::from(endian == Endian::Big);
        bytes[20..24].copy_from_slice(&metadata_size.to_be_bytes());
        bytes[24..32].copy_from_slice(&i64::try_from(file_size).unwrap().to_be_bytes());
        bytes[32..40].copy_from_slice(&i64::try_from(data_offset).unwrap().to_be_bytes());
        bytes.extend_from_slice(&metadata);
        bytes.resize(usize::try_from(data_offset).unwrap(), 0);
        bytes.extend_from_slice(object);
        bytes
    }

    fn push_aligned_string(output: &mut Vec<u8>, value: &str, endian: Endian) {
        push_i32(output, i32::try_from(value.len()).unwrap(), endian);
        output.extend_from_slice(value.as_bytes());
        if !value.is_empty() {
            align(output, 4);
        }
    }

    fn push_i16(output: &mut Vec<u8>, value: i16, endian: Endian) {
        let bytes = match endian {
            Endian::Little => value.to_le_bytes(),
            Endian::Big => value.to_be_bytes(),
        };
        output.extend_from_slice(&bytes);
    }

    fn push_i32(output: &mut Vec<u8>, value: i32, endian: Endian) {
        let bytes = match endian {
            Endian::Little => value.to_le_bytes(),
            Endian::Big => value.to_be_bytes(),
        };
        output.extend_from_slice(&bytes);
    }

    fn push_f32(output: &mut Vec<u8>, value: f32, endian: Endian) {
        push_u32(output, value.to_bits(), endian);
    }

    fn push_u32(output: &mut Vec<u8>, value: u32, endian: Endian) {
        let bytes = match endian {
            Endian::Little => value.to_le_bytes(),
            Endian::Big => value.to_be_bytes(),
        };
        output.extend_from_slice(&bytes);
    }

    fn push_i64(output: &mut Vec<u8>, value: i64, endian: Endian) {
        let bytes = match endian {
            Endian::Little => value.to_le_bytes(),
            Endian::Big => value.to_be_bytes(),
        };
        output.extend_from_slice(&bytes);
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
