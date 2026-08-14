use std::fmt;
use std::io::{self, Write};

use crate::endian::{Endian, EndianReader, checked_length};
use crate::loader::AssetCollection;
use crate::managed_number::ManagedNumber;
use crate::packed_bits::{PackedFloatVector, PackedIntVector};
use crate::serialized::{ObjectInfo, SerializedFile};
use crate::source::{Region, RegionCursor};
use crate::{Error, Result};

pub const MESH_CLASS_ID: i32 = 43;

const NO_TARGET_PLATFORM: i32 = -2;
const STREAM_ALIGNMENT: usize = 16;

/// Defensive bounds for the resident, uncompressed `Mesh` slice implemented
/// here. Standard Unity 2017.3 through 2023 and 6000.0 through 6000.1, plus
/// Tuanjie 2022.3.x meshes whose virtual-geometry flag is clear, use the
/// covered layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeshReadLimits {
    pub maximum_object_bytes: u64,
    pub maximum_string_bytes: usize,
    pub maximum_total_string_bytes: usize,
    pub maximum_sub_meshes: usize,
    pub maximum_vertices: usize,
    pub maximum_indices: usize,
    pub maximum_bones: usize,
    pub maximum_skin_weights: usize,
    pub maximum_blend_shape_vertices: usize,
    pub maximum_blend_shape_frames: usize,
    pub maximum_blend_shape_channels: usize,
    pub maximum_channels: usize,
    pub maximum_array_elements: usize,
    pub maximum_vertex_data_bytes: usize,
    pub maximum_compressed_data_bytes: u64,
    pub maximum_auxiliary_bytes: u64,
    pub maximum_decoded_bytes: u64,
    pub maximum_output_bytes: u64,
}

impl Default for MeshReadLimits {
    fn default() -> Self {
        Self {
            maximum_object_bytes: 512 * 1024 * 1024,
            maximum_string_bytes: 16 * 1024 * 1024,
            maximum_total_string_bytes: 32 * 1024 * 1024,
            maximum_sub_meshes: 100_000,
            maximum_vertices: 10_000_000,
            maximum_indices: 30_000_000,
            maximum_bones: 1_000_000,
            maximum_skin_weights: 10_000_000,
            maximum_blend_shape_vertices: 10_000_000,
            maximum_blend_shape_frames: 1_000_000,
            maximum_blend_shape_channels: 1_000_000,
            maximum_channels: 64,
            maximum_array_elements: 10_000_000,
            maximum_vertex_data_bytes: 512 * 1024 * 1024,
            maximum_compressed_data_bytes: 64 * 1024 * 1024,
            maximum_auxiliary_bytes: 256 * 1024 * 1024,
            maximum_decoded_bytes: 512 * 1024 * 1024,
            maximum_output_bytes: 512 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeshSubMesh {
    pub first_byte: u32,
    pub index_count: u32,
    pub first_vertex: u32,
    pub vertex_count: u32,
    pub indices: Vec<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MeshBoneWeight {
    pub weights: [f32; 4],
    pub bone_indices: [u32; 4],
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MeshBlendShapeVertex {
    pub vertex: [f32; 3],
    pub normal: [f32; 3],
    pub tangent: [f32; 3],
    pub index: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeshBlendShapeFrame {
    pub first_vertex: u32,
    pub vertex_count: u32,
    pub has_normals: bool,
    pub has_tangents: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeshBlendShapeChannel {
    pub name: String,
    pub name_hash: u32,
    pub frame_index: usize,
    pub frame_count: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MeshBlendShapes {
    pub vertices: Vec<MeshBlendShapeVertex>,
    pub frames: Vec<MeshBlendShapeFrame>,
    pub channels: Vec<MeshBlendShapeChannel>,
    pub full_weights: Vec<f32>,
}

impl Default for MeshBoneWeight {
    fn default() -> Self {
        Self {
            weights: [0.0; 4],
            bone_indices: [0; 4],
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Mesh {
    pub path_id: i64,
    pub name: String,
    pub vertices: Vec<[f32; 3]>,
    pub normals: Option<Vec<[f32; 3]>>,
    pub uv0: Option<Vec<[f32; 2]>>,
    /// Column-major Unity matrices in serialized component order.
    pub bind_poses: Vec<[f32; 16]>,
    pub bone_name_hashes: Vec<u32>,
    pub root_bone_name_hash: u32,
    pub skin: Option<Vec<MeshBoneWeight>>,
    pub blend_shapes: Option<MeshBlendShapes>,
    pub sub_meshes: Vec<MeshSubMesh>,
}

/// Parses the common resident `Mesh` representation used by standard Unity
/// 2017.3-2023 and 6000.0-6000.1, and by non-virtual Tuanjie 2022.3.x meshes.
/// Unity 6000.2 adds a serialized mesh-LOD tail that remains explicit
/// unsupported until its structure is sample-verified.
/// Packed/compressed geometry, non-triangle topology, non-zero base vertices,
/// and non-float position/normal/UV layouts are rejected explicitly instead of
/// being interpreted heuristically. Use [`read_mesh_with_collection`] when the
/// object references an external vertex stream.
pub fn read_mesh(
    file: &SerializedFile,
    object_index: usize,
    limits: MeshReadLimits,
) -> Result<Mesh> {
    read_mesh_inner(None, file, object_index, limits)
}

/// Parses a mesh and resolves external `StreamingInfo` vertex bytes through
/// the collection's bounded resource table.
pub fn read_mesh_with_collection(
    collection: &AssetCollection,
    file: &SerializedFile,
    object_index: usize,
    limits: MeshReadLimits,
) -> Result<Mesh> {
    read_mesh_inner(Some(collection), file, object_index, limits)
}

/// The packed geometry Unity writes when a mesh is compressed.
///
/// Each vector is a quantized bit stream; nothing is reconstructed until
/// [`CompressedMesh::decode`] runs, so the budgets that bound the packed bytes
/// are charged while reading and the unpacked sizes are checked separately.
#[derive(Debug, Clone, Default)]
struct CompressedMesh {
    vertices: PackedFloatVector,
    uv: PackedFloatVector,
    normals: PackedFloatVector,
    tangents: PackedFloatVector,
    weights: PackedIntVector,
    normal_signs: PackedIntVector,
    tangent_signs: PackedIntVector,
    float_colors: PackedFloatVector,
    bone_indices: PackedIntVector,
    triangles: PackedIntVector,
    uv_info: u32,
}

impl CompressedMesh {
    fn has_items(&self) -> bool {
        !self.vertices.is_empty()
            || !self.uv.is_empty()
            || !self.normals.is_empty()
            || !self.tangents.is_empty()
            || !self.weights.is_empty()
            || !self.normal_signs.is_empty()
            || !self.tangent_signs.is_empty()
            || !self.float_colors.is_empty()
            || !self.bone_indices.is_empty()
            || !self.triangles.is_empty()
    }

    /// Reconstructs the attributes the `Mesh` type carries.
    ///
    /// Tangents and colours are decoded by the managed reader too, but this
    /// type has nowhere to put them, so their vectors are left packed rather
    /// than decoded and dropped.
    /// Writes whatever the packed vectors carry over `decoded`, field by field.
    ///
    /// An empty vector leaves the vertex-data value in place, which is what the
    /// managed reader does: each block is guarded on its own item count.
    fn overlay(&self, decoded: &mut DecodedVertexData, limits: MeshReadLimits) -> Result<()> {
        if !self.vertices.is_empty() {
            let flat = self.vertices.unpack(3, 0, None)?;
            let vertex_count = flat.len() / 3;
            if vertex_count == 0 {
                return Err(Error::invalid_data("compressed Mesh has no vertices"));
            }
            if vertex_count > limits.maximum_vertices {
                return Err(Error::invalid_data(format!(
                    "compressed Mesh has {vertex_count} vertices, exceeding limit {}",
                    limits.maximum_vertices
                )));
            }
            let mut vertices = reserve_vec(vertex_count, "compressed Mesh vertices")?;
            for chunk in flat.chunks_exact(3) {
                vertices.push([chunk[0], chunk[1], chunk[2]]);
            }
            decoded.vertices = vertices;
        }
        let vertex_count = decoded.vertices.len();
        if let Some(normals) = self.decode_normals()? {
            decoded.normals = Some(normals);
        }
        if let Some(uv0) = self.decode_uv0(vertex_count)? {
            decoded.uv0 = Some(uv0);
        }
        if let Some(skin) = self.decode_skin(vertex_count)? {
            decoded.skin = Some(skin);
        }
        Ok(())
    }

    /// Rebuilds normals from their octahedral pair plus a sign bit.
    fn decode_normals(&self) -> Result<Option<Vec<[f32; 3]>>> {
        if self.normals.is_empty() {
            return Ok(None);
        }
        let pairs = self.normals.unpack(2, 0, None)?;
        let signs = self.normal_signs.unpack()?;
        let count = pairs.len() / 2;
        if signs.len() < count {
            return Err(Error::invalid_data(
                "compressed Mesh has fewer normal signs than normals",
            ));
        }
        let mut normals = reserve_vec(count, "compressed Mesh normals")?;
        for (index, pair) in pairs.chunks_exact(2).enumerate() {
            let mut normal = restore_octahedral(pair[0], pair[1]);
            if signs[index] == 0 {
                normal[2] = -normal[2];
            }
            normals.push(normal);
        }
        Ok(Some(normals))
    }

    /// Rebuilds UV0, honouring the packed channel descriptor when present.
    fn decode_uv0(&self, vertex_count: usize) -> Result<Option<Vec<[f32; 2]>>> {
        const BITS_PER_CHANNEL: u32 = 4;
        const DIMENSION_MASK: u32 = 3;
        const CHANNEL_EXISTS: u32 = 4;

        if self.uv.is_empty() {
            return Ok(None);
        }
        let dimension = if self.uv_info == 0 {
            2
        } else {
            let descriptor = self.uv_info & ((1 << BITS_PER_CHANNEL) - 1);
            if descriptor & CHANNEL_EXISTS == 0 {
                return Ok(None);
            }
            usize::try_from(1 + (descriptor & DIMENSION_MASK))
                .map_err(|_| Error::invalid_data("compressed Mesh UV dimension is out of range"))?
        };
        let values = self.uv.unpack(dimension, 0, Some(vertex_count))?;
        let mut uv0 = reserve_vec(vertex_count, "compressed Mesh UV0")?;
        for chunk in values.chunks_exact(dimension) {
            uv0.push([chunk[0], chunk.get(1).copied().unwrap_or(0.0)]);
        }
        Ok(Some(uv0))
    }

    /// Rebuilds four-influence skin weights from the 31-quantized run.
    ///
    /// A vertex ends either when its weights reach the full 31 or after three
    /// influences, in which case the fourth takes the remainder.
    fn decode_skin(&self, vertex_count: usize) -> Result<Option<Vec<MeshBoneWeight>>> {
        if self.weights.is_empty() {
            return Ok(None);
        }
        let weights = self.weights.unpack()?;
        let bones = self.bone_indices.unpack()?;
        let mut skin: Vec<MeshBoneWeight> = reserve_vec(vertex_count, "compressed Mesh skin")?;
        let mut current = MeshBoneWeight::default();
        let mut influence = 0_usize;
        let mut total = 0_u32;
        let mut bone_cursor = 0_usize;
        let take_bone = |cursor: &mut usize| -> Result<u32> {
            let value = bones
                .get(*cursor)
                .copied()
                .ok_or_else(|| Error::invalid_data("compressed Mesh ran out of bone indices"))?;
            *cursor += 1;
            Ok(value)
        };

        for weight in weights {
            current.weights[influence] = f32::from(
                u16::try_from(weight)
                    .map_err(|_| Error::invalid_data("compressed Mesh weight is out of range"))?,
            ) / 31.0;
            current.bone_indices[influence] = take_bone(&mut bone_cursor)?;
            influence += 1;
            total += weight;

            if total >= 31 {
                skin.push(current);
                current = MeshBoneWeight::default();
                influence = 0;
                total = 0;
            } else if influence == 3 {
                current.weights[3] = f32::from(
                    u16::try_from(31 - total)
                        .map_err(|_| Error::invalid_data("compressed Mesh weight underflowed"))?,
                ) / 31.0;
                current.bone_indices[3] = take_bone(&mut bone_cursor)?;
                skin.push(current);
                current = MeshBoneWeight::default();
                influence = 0;
                total = 0;
            }
            if skin.len() > vertex_count {
                return Err(Error::invalid_data(
                    "compressed Mesh produced more skin records than vertices",
                ));
            }
        }
        Ok(Some(skin))
    }
}

/// Restores the third component of an octahedrally packed unit vector.
fn restore_octahedral(x: f32, y: f32) -> [f32; 3] {
    let squared = 1.0 - x * x - y * y;
    if squared >= 0.0 {
        return [x, y, squared.sqrt()];
    }
    // Off the unit sphere: fall back to normalizing the pair, as the managed
    // reader does, rather than producing a NaN.
    let length = x.hypot(y);
    if length == 0.0 {
        return [0.0, 0.0, 0.0];
    }
    [x / length, y / length, 0.0]
}

/// Expands a submesh's index run into a triangle list.
///
/// Unity still emits quads, and imported models bring strips, so refusing a
/// non-triangle topology outright failed the whole mesh read rather than just
/// the triangle-oriented output. Follows the managed reader: before Unity 4
/// every topology is treated as a strip, degenerate strip triangles are
/// dropped, odd strip positions flip their winding, and a quad becomes two
/// triangles sharing a diagonal. Lines and points stay explicitly unsupported
/// because there is no triangle to make from them.
fn triangulate(
    source: &[u32],
    topology: i32,
    version: (u32, u32, u32),
    limits: MeshReadLimits,
) -> Result<Vec<u32>> {
    const TRIANGLES: i32 = 0;
    const TRIANGLE_STRIP: i32 = 1;
    const QUADS: i32 = 2;

    if topology == TRIANGLES && version.0 >= 4 {
        let mut indices = reserve_vec(source.len(), "Mesh submesh indices")?;
        indices.extend_from_slice(source);
        return Ok(indices);
    }
    if version.0 >= 4 && !matches!(topology, TRIANGLE_STRIP | QUADS) {
        return Err(Error::unsupported(format!(
            "Mesh topology {topology}; lines and points have no triangles"
        )));
    }

    // A strip of n indices yields at most n - 2 triangles; a quad run grows by
    // half. Charge the upper bound before allocating.
    let expanded = if version.0 < 4 || topology == TRIANGLE_STRIP {
        source.len().saturating_sub(2).saturating_mul(3)
    } else {
        source.len() / 4 * 6
    };
    if expanded > limits.maximum_indices {
        return Err(Error::invalid_data(format!(
            "Mesh triangulation needs {expanded} indices, exceeding limit {}",
            limits.maximum_indices
        )));
    }
    let mut indices = reserve_vec(expanded, "Mesh triangulated indices")?;

    if version.0 < 4 || topology == TRIANGLE_STRIP {
        for (position, window) in source.windows(3).enumerate() {
            let [first, second, third] = [window[0], window[1], window[2]];
            if first == second || first == third || second == third {
                continue;
            }
            if position % 2 == 1 {
                indices.extend_from_slice(&[second, first, third]);
            } else {
                indices.extend_from_slice(&[first, second, third]);
            }
        }
    } else {
        for quad in source.chunks_exact(4) {
            indices.extend_from_slice(&[quad[0], quad[1], quad[2], quad[0], quad[2], quad[3]]);
        }
    }
    Ok(indices)
}

#[allow(clippy::too_many_lines)]
fn read_mesh_inner(
    collection: Option<&AssetCollection>,
    file: &SerializedFile,
    object_index: usize,
    limits: MeshReadLimits,
) -> Result<Mesh> {
    if file.unity_version.is_stripped() {
        return Err(Error::unsupported(
            "Mesh requires a Unity version because its layout is version-dependent",
        ));
    }
    let version = file.unity_version.components();
    let is_tuanjie = file.unity_version.build_type.as_deref() == Some("t");
    let supported = if is_tuanjie {
        version.0 == 2022 && version.1 == 3 && version.2 >= 2
    } else {
        ((2017, 3, 0)..(2024, 0, 0)).contains(&version) || (version.0 == 6000 && version.1 <= 1)
    };
    if !supported {
        return Err(Error::unsupported(format!(
            "resident Mesh layout for Unity {}; implemented ranges are standard Unity 2017.3-2023 and 6000.0-6000.1 plus Tuanjie 2022.3.x (6000.2 adds an unverified MeshLodInfo tail)",
            file.unity_version
        )));
    }

    let object = require_mesh(file, object_index)?;
    if object.byte_size > limits.maximum_object_bytes {
        return Err(Error::invalid_data(format!(
            "Mesh object is {} bytes, exceeding limit {}",
            object.byte_size, limits.maximum_object_bytes
        )));
    }
    let mut reader = MeshObjectReader::new(file, object_index, limits)?;
    let name = reader.read_named_object()?;

    let sub_mesh_count = reader.read_record_count(limits.maximum_sub_meshes, 48, "Mesh submesh")?;
    let mut raw_sub_meshes = reserve_vec(sub_mesh_count, "Mesh submeshes")?;
    let mut referenced_index_count = 0_usize;
    for _ in 0..sub_mesh_count {
        let first_byte = reader.reader.read_u32()?;
        let index_count = reader.reader.read_u32()?;
        let topology = reader.reader.read_i32()?;
        let base_vertex = reader.reader.read_u32()?;
        if base_vertex != 0 {
            return Err(Error::unsupported(format!(
                "Mesh submesh base vertex {base_vertex}; only zero-base index buffers are implemented"
            )));
        }
        let first_vertex = reader.reader.read_u32()?;
        let vertex_count = reader.reader.read_u32()?;
        reader.skip(24, "Mesh submesh local AABB")?;
        let index_count_usize = usize::try_from(index_count)
            .map_err(|_| Error::invalid_data("Mesh submesh index count does not fit in usize"))?;
        if !index_count_usize.is_multiple_of(3) {
            return Err(Error::invalid_data(format!(
                "Mesh triangle submesh index count {index_count} is not divisible by three"
            )));
        }
        referenced_index_count = referenced_index_count
            .checked_add(index_count_usize)
            .ok_or_else(|| Error::invalid_data("Mesh referenced index count overflowed"))?;
        if referenced_index_count > limits.maximum_indices {
            return Err(Error::invalid_data(format!(
                "Mesh submeshes reference {referenced_index_count} indices, exceeding limit {}",
                limits.maximum_indices
            )));
        }
        raw_sub_meshes.push(RawSubMesh {
            first_byte,
            index_count,
            first_vertex,
            vertex_count,
            topology,
        });
    }

    let blend_shapes = reader.read_blend_shapes()?;
    let bind_poses = reader.read_bind_poses()?;
    let bone_name_hashes = reader.read_bone_name_hashes()?;
    let root_bone_name_hash = reader.reader.read_u32()?;
    if version.0 >= 2019 {
        reader.skip_counted_auxiliary_records(24, "Mesh bone AABB")?;
        reader.skip_counted_auxiliary_records(4, "Mesh variable bone-count weight")?;
    }

    let mesh_compression = reader.reader.read_u8()?;
    reader.skip(3, "Mesh readability flags")?;
    if is_tuanjie {
        let revision = tuanjie_shared_cluster_revision(file);
        if revision == 3 {
            reader.align(4)?;
        }
        reader.skip_tuanjie_shared_cluster(revision)?;
        reader.align(4)?;
    } else {
        reader.align(4)?;
    }
    let index_width = match reader.reader.read_i32()? {
        0 => 2_usize,
        1 => 4_usize,
        value => {
            return Err(Error::unsupported(format!(
                "Mesh index format {value}; expected 16-bit or 32-bit indices"
            )));
        }
    };
    let index_byte_length = reader.read_length("Mesh index buffer")?;
    if !index_byte_length.is_multiple_of(index_width) {
        return Err(Error::invalid_data(format!(
            "Mesh index buffer length {index_byte_length} is not divisible by index width {index_width}"
        )));
    }
    let index_count = index_byte_length / index_width;
    if index_count > limits.maximum_indices {
        return Err(Error::invalid_data(format!(
            "Mesh index buffer has {index_count} indices, exceeding limit {}",
            limits.maximum_indices
        )));
    }
    let mut index_buffer = reserve_vec(index_count, "Mesh indices")?;
    for _ in 0..index_count {
        index_buffer.push(if index_width == 2 {
            u32::from(reader.reader.read_u16()?)
        } else {
            reader.reader.read_u32()?
        });
    }
    if index_width == 2 {
        reader.align(4)?;
    }

    let legacy_skin = if version < (2018, 2, 0) {
        reader.read_legacy_skin()?
    } else {
        None
    };
    let mut vertex_data = reader.read_vertex_data(version)?;
    let compressed = reader.read_compressed_mesh()?;
    reader.skip(24, "Mesh local AABB")?;
    reader.skip(4, "Mesh usage flags")?;
    if version >= (2022, 1, 0) {
        reader.skip(4, "Mesh cooking options")?;
    }
    reader.skip_counted_auxiliary_bytes("Mesh baked convex collision data")?;
    reader.align(4)?;
    reader.skip_counted_auxiliary_bytes("Mesh baked triangle collision data")?;
    reader.align(4)?;
    if version >= (2018, 2, 0) {
        reader.skip(8, "Mesh metrics")?;
    }
    if version >= (2018, 3, 0) {
        reader.align(4)?;
        let stream_offset = if version >= (2020, 1, 0) {
            reader.reader.read_i64()?
        } else {
            i64::from(reader.reader.read_u32()?)
        };
        if stream_offset < 0 {
            return Err(Error::invalid_data(format!(
                "Mesh stream offset cannot be negative: {stream_offset}"
            )));
        }
        let stream_size = reader.reader.read_u32()?;
        let stream_path = reader.read_aligned_string("Mesh stream path")?;
        if stream_path.is_empty() {
            if stream_offset != 0 || stream_size != 0 {
                return Err(Error::invalid_data(format!(
                    "Mesh has empty stream path with nonzero offset {stream_offset} or size {stream_size}"
                )));
            }
        } else {
            let collection = collection.ok_or_else(|| {
                Error::unsupported(format!(
                    "streamed Mesh vertex data requires an AssetCollection (path {stream_path:?})"
                ))
            })?;
            let stream_size = usize::try_from(stream_size)
                .map_err(|_| Error::invalid_data("Mesh stream size does not fit in usize"))?;
            if stream_size > limits.maximum_vertex_data_bytes {
                return Err(Error::invalid_data(format!(
                    "Mesh external vertex data is {stream_size} bytes, exceeding limit {}",
                    limits.maximum_vertex_data_bytes
                )));
            }
            let resource = collection.resource(&stream_path).ok_or_else(|| {
                Error::invalid_data(format!("external resource was not found: {stream_path}"))
            })?;
            let stream_offset =
                u64::try_from(stream_offset).expect("nonnegative Mesh stream offset fits in u64");
            let stream_size_u64 = u64::try_from(stream_size)
                .expect("Mesh stream size fits in u64 after usize conversion");
            vertex_data.bytes = resource
                .region
                .subregion(stream_offset, stream_size_u64)?
                .read_to_vec(stream_size_u64)?;
        }
    }
    let has_virtual_geometry = if is_tuanjie && tuanjie_has_virtual_geometry_flags(file) {
        let _generate_geometry_buffer = reader.reader.read_bool()?;
        reader.reader.read_bool()?
    } else {
        false
    };
    reader.finish()?;

    if has_virtual_geometry {
        return Err(Error::unsupported(
            "Tuanjie Mesh uses virtual geometry, whose cluster pages are not decoded",
        ));
    }

    if mesh_compression != 0 && !compressed.has_items() {
        return Err(Error::invalid_data(format!(
            "Mesh declares compression mode {mesh_compression} but carries no packed geometry"
        )));
    }

    // The packed vectors overlay the vertex data rather than replacing it.
    // Unity writes one form or the other, never both, but the managed reader
    // decodes whatever each source carries and lets the compressed values win
    // per field. Choosing one source outright would drop the channels the other
    // holds if a file ever arrived with both.
    let mut decoded = if vertex_data.has_vertices() {
        vertex_data.decode(reader.reader.endian(), limits, version)?
    } else {
        DecodedVertexData::default()
    };
    if compressed.has_items() {
        let indices = compressed.triangles.unpack()?;
        if indices.len() > limits.maximum_indices {
            return Err(Error::invalid_data(format!(
                "compressed Mesh has {} indices, exceeding limit {}",
                indices.len(),
                limits.maximum_indices
            )));
        }
        if !indices.is_empty() {
            index_buffer = indices;
        }
        compressed.overlay(&mut decoded, limits)?;
    }
    if decoded.vertices.is_empty() {
        return Err(Error::invalid_data("Mesh has no vertices"));
    }
    let decoded = decoded;
    let vertex_count = decoded.vertices.len();
    if let Some(shapes) = &blend_shapes {
        validate_blend_shapes(shapes, vertex_count)?;
    }
    let skin = legacy_skin.or(decoded.skin);
    if let Some(weights) = &skin
        && weights.len() != vertex_count
    {
        return Err(Error::invalid_data(format!(
            "Mesh has {} skin-weight records for {vertex_count} vertices",
            weights.len()
        )));
    }
    let mut sub_meshes = reserve_vec(raw_sub_meshes.len(), "decoded Mesh submeshes")?;
    for raw in raw_sub_meshes {
        let first_byte = usize::try_from(raw.first_byte)
            .map_err(|_| Error::invalid_data("Mesh first-byte offset does not fit in usize"))?;
        if !first_byte.is_multiple_of(index_width) {
            return Err(Error::invalid_data(format!(
                "Mesh submesh first-byte offset {} is not aligned to index width {index_width}",
                raw.first_byte
            )));
        }
        let first_index = first_byte / index_width;
        let sub_count = usize::try_from(raw.index_count)
            .map_err(|_| Error::invalid_data("Mesh submesh index count does not fit in usize"))?;
        let end = first_index
            .checked_add(sub_count)
            .ok_or_else(|| Error::invalid_data("Mesh submesh index range overflowed"))?;
        let source_indices = index_buffer.get(first_index..end).ok_or_else(|| {
            Error::invalid_data(format!(
                "Mesh submesh index range {first_index}..{end} exceeds index buffer length {}",
                index_buffer.len()
            ))
        })?;
        if let Some(index) = source_indices
            .iter()
            .copied()
            .find(|index| usize::try_from(*index).map_or(true, |value| value >= vertex_count))
        {
            return Err(Error::invalid_data(format!(
                "Mesh index {index} exceeds vertex count {vertex_count}"
            )));
        }
        let vertex_end = usize::try_from(raw.first_vertex)
            .ok()
            .and_then(|start| {
                usize::try_from(raw.vertex_count)
                    .ok()
                    .and_then(|count| start.checked_add(count))
            })
            .ok_or_else(|| Error::invalid_data("Mesh submesh vertex range overflowed"))?;
        if vertex_end > vertex_count {
            return Err(Error::invalid_data(format!(
                "Mesh submesh vertex range ends at {vertex_end}, beyond vertex count {vertex_count}"
            )));
        }
        let indices = triangulate(source_indices, raw.topology, version, limits)?;
        let index_count = u32::try_from(indices.len())
            .map_err(|_| Error::invalid_data("Mesh triangulated index count does not fit u32"))?;
        sub_meshes.push(MeshSubMesh {
            first_byte: raw.first_byte,
            index_count,
            first_vertex: raw.first_vertex,
            vertex_count: raw.vertex_count,
            indices,
        });
    }

    Ok(Mesh {
        path_id: object.path_id,
        name,
        vertices: decoded.vertices,
        normals: decoded.normals,
        uv0: decoded.uv0,
        bind_poses,
        bone_name_hashes,
        root_bone_name_hash,
        skin,
        blend_shapes,
        sub_meshes,
    })
}

/// Parses a resident mesh and streams the legacy `AssetStudio` OBJ contract.
pub fn write_mesh_object_obj<W: Write>(
    file: &SerializedFile,
    object_index: usize,
    output: &mut W,
    limits: MeshReadLimits,
) -> Result<u64> {
    let mesh = read_mesh(file, object_index, limits)?;
    write_mesh_obj(&mesh, output, limits.maximum_output_bytes)
}

/// Resolves collection-backed vertex streams and writes one mesh as OBJ.
pub fn write_mesh_object_obj_with_collection<W: Write>(
    collection: &AssetCollection,
    file: &SerializedFile,
    object_index: usize,
    output: &mut W,
    limits: MeshReadLimits,
) -> Result<u64> {
    let mesh = read_mesh_with_collection(collection, file, object_index, limits)?;
    write_mesh_obj(&mesh, output, limits.maximum_output_bytes)
}

/// Writes AssetStudio-compatible OBJ text: X is mirrored for positions and
/// normals, triangle winding is reversed, every line ends in CRLF, number
/// formatting is locale-independent, and `NaN` components are written as `0`.
///
/// The managed repository holds three writers for this format, and they do not
/// agree with each other. The headless one -- `AssetStudioSession`, the path
/// behind the managed library's own object payloads, and so the one this
/// project replaces -- writes CRLF throughout, formats under the invariant
/// culture, and substitutes `NaN` per line. This matches it byte for byte, and
/// the differential compares the two documents directly rather than comparing
/// only the geometry they are written from.
///
/// Against the other two, the exporters behind the GUI and the CLI, two
/// differences remain, and both make the output depend on less:
///
/// * Line endings are CRLF throughout. Those writers emit their `g` lines with
///   `StringBuilder.AppendLine`, which uses the platform's newline, while
///   every other line carries an explicit CRLF -- so their output has mixed
///   line endings on Linux and macOS and uniform ones on Windows. This matches
///   the Windows form everywhere rather than making a mesh's bytes depend on
///   which machine exported it.
/// * `NaN` is replaced per value rather than per document. Those writers finish
///   with `sb.Replace("NaN", "0")` over the whole text, which also rewrites a
///   mesh whose *name* contains `NaN`. Here, as in the headless writer, only
///   numeric components are substituted, so the name survives.
pub fn write_mesh_obj<W: Write>(
    mesh: &Mesh,
    output: &mut W,
    maximum_output_bytes: u64,
) -> Result<u64> {
    validate_mesh_for_obj(mesh)?;
    let mut bounded = BoundedWriter::new(output, maximum_output_bytes);
    let result = write_mesh_obj_inner(mesh, &mut bounded);
    if bounded.limit_exceeded {
        return Err(Error::invalid_data(format!(
            "Mesh OBJ exceeds the {maximum_output_bytes} byte output limit"
        )));
    }
    result?;
    Ok(bounded.written)
}

fn write_mesh_obj_inner<W: Write>(mesh: &Mesh, output: &mut W) -> io::Result<()> {
    output.write_all(b"g ")?;
    output.write_all(mesh.name.as_bytes())?;
    output.write_all(b"\r\n")?;

    for vertex in &mesh.vertices {
        write!(
            output,
            "v {} {} {}\r\n",
            ObjFloat(-vertex[0]),
            ObjFloat(vertex[1]),
            ObjFloat(vertex[2])
        )?;
    }
    if let Some(uv0) = &mesh.uv0 {
        for uv in uv0 {
            write!(output, "vt {} {}\r\n", ObjFloat(uv[0]), ObjFloat(uv[1]))?;
        }
    }
    if let Some(normals) = &mesh.normals {
        for normal in normals {
            write!(
                output,
                "vn {} {} {}\r\n",
                ObjFloat(-normal[0]),
                ObjFloat(normal[1]),
                ObjFloat(normal[2])
            )?;
        }
    }
    for (sub_mesh_index, sub_mesh) in mesh.sub_meshes.iter().enumerate() {
        output.write_all(b"g ")?;
        output.write_all(mesh.name.as_bytes())?;
        write!(output, "_{sub_mesh_index}\r\n")?;
        for triangle in sub_mesh.indices.chunks_exact(3) {
            let a = u64::from(triangle[2]) + 1;
            let b = u64::from(triangle[1]) + 1;
            let c = u64::from(triangle[0]) + 1;
            write!(output, "f {a}/{a}/{a} {b}/{b}/{b} {c}/{c}/{c}\r\n")?;
        }
    }
    Ok(())
}

fn validate_mesh_for_obj(mesh: &Mesh) -> Result<()> {
    if mesh.vertices.is_empty() {
        return Err(Error::invalid_data("Mesh has no vertices"));
    }
    if let Some(normals) = &mesh.normals
        && normals.len() != mesh.vertices.len()
    {
        return Err(Error::invalid_data(format!(
            "Mesh has {} normals for {} vertices",
            normals.len(),
            mesh.vertices.len()
        )));
    }
    if let Some(uv0) = &mesh.uv0
        && uv0.len() != mesh.vertices.len()
    {
        return Err(Error::invalid_data(format!(
            "Mesh has {} UV0 values for {} vertices",
            uv0.len(),
            mesh.vertices.len()
        )));
    }
    for (sub_mesh_index, sub_mesh) in mesh.sub_meshes.iter().enumerate() {
        if !sub_mesh.indices.len().is_multiple_of(3) {
            return Err(Error::invalid_data(format!(
                "Mesh submesh {sub_mesh_index} has a non-triangle index count {}",
                sub_mesh.indices.len()
            )));
        }
        if let Some(index) = sub_mesh.indices.iter().copied().find(|index| {
            usize::try_from(*index).map_or(true, |value| value >= mesh.vertices.len())
        }) {
            return Err(Error::invalid_data(format!(
                "Mesh submesh {sub_mesh_index} index {index} exceeds vertex count {}",
                mesh.vertices.len()
            )));
        }
    }
    Ok(())
}

fn validate_blend_shapes(shapes: &MeshBlendShapes, mesh_vertex_count: usize) -> Result<()> {
    if shapes.frames.len() != shapes.full_weights.len() {
        return Err(Error::invalid_data(format!(
            "Mesh has {} blend-shape frames but {} full weights",
            shapes.frames.len(),
            shapes.full_weights.len()
        )));
    }
    for (frame_index, frame) in shapes.frames.iter().enumerate() {
        let start = usize::try_from(frame.first_vertex)
            .map_err(|_| Error::invalid_data("blend-shape first vertex does not fit usize"))?;
        let count = usize::try_from(frame.vertex_count)
            .map_err(|_| Error::invalid_data("blend-shape vertex count does not fit usize"))?;
        let end = start
            .checked_add(count)
            .ok_or_else(|| Error::invalid_data("blend-shape vertex range overflowed"))?;
        if end > shapes.vertices.len() {
            return Err(Error::invalid_data(format!(
                "Mesh blend-shape frame {frame_index} vertex range {start}..{end} exceeds {} vertices",
                shapes.vertices.len()
            )));
        }
    }
    for (channel_index, channel) in shapes.channels.iter().enumerate() {
        let end = channel
            .frame_index
            .checked_add(channel.frame_count)
            .ok_or_else(|| Error::invalid_data("blend-shape channel frame range overflowed"))?;
        if end > shapes.frames.len() {
            return Err(Error::invalid_data(format!(
                "Mesh blend-shape channel {channel_index} frame range {}..{end} exceeds {} frames",
                channel.frame_index,
                shapes.frames.len()
            )));
        }
    }
    if let Some(vertex) = shapes.vertices.iter().find(|vertex| {
        usize::try_from(vertex.index).map_or(true, |index| index >= mesh_vertex_count)
    }) {
        return Err(Error::invalid_data(format!(
            "Mesh blend-shape vertex index {} exceeds mesh vertex count {mesh_vertex_count}",
            vertex.index
        )));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct RawSubMesh {
    first_byte: u32,
    index_count: u32,
    first_vertex: u32,
    vertex_count: u32,
    topology: i32,
}

#[derive(Debug, Clone, Copy)]
struct Channel {
    stream: u8,
    offset: u8,
    format: u8,
    dimension: u8,
}

#[derive(Debug, Clone, Copy)]
struct Stream {
    offset: usize,
    stride: usize,
}

struct VertexData {
    vertex_count: usize,
    channels: Vec<Channel>,
    streams: Vec<Stream>,
    bytes: Vec<u8>,
}

#[derive(Default)]
struct DecodedVertexData {
    vertices: Vec<[f32; 3]>,
    normals: Option<Vec<[f32; 3]>>,
    uv0: Option<Vec<[f32; 2]>>,
    skin: Option<Vec<MeshBoneWeight>>,
}

impl VertexData {
    /// Whether the uncompressed stream declares any vertices to decode.
    const fn has_vertices(&self) -> bool {
        self.vertex_count != 0
    }

    fn decode(
        self,
        endian: Endian,
        limits: MeshReadLimits,
        version: (u32, u32, u32),
    ) -> Result<DecodedVertexData> {
        if self.vertex_count == 0 {
            return Err(Error::invalid_data("Mesh has no vertices"));
        }
        for (index, channel) in self.channels.iter().copied().enumerate() {
            if channel.dimension == 0 {
                continue;
            }
            let size = vertex_format_size(channel.format, version)?;
            let stream = self
                .streams
                .get(usize::from(channel.stream))
                .ok_or_else(|| {
                    Error::invalid_data(format!(
                        "Mesh channel {index} references missing stream {}",
                        channel.stream
                    ))
                })?;
            let component_bytes = usize::from(channel.dimension)
                .checked_mul(size)
                .ok_or_else(|| Error::invalid_data("Mesh channel byte size overflowed"))?;
            let final_vertex_offset = stream
                .stride
                .checked_mul(self.vertex_count - 1)
                .and_then(|value| stream.offset.checked_add(value))
                .and_then(|value| value.checked_add(usize::from(channel.offset)))
                .and_then(|value| value.checked_add(component_bytes))
                .ok_or_else(|| Error::invalid_data("Mesh channel data range overflowed"))?;
            if final_vertex_offset > self.bytes.len() {
                return Err(Error::invalid_data(format!(
                    "Mesh channel {index} ends at byte {final_vertex_offset}, beyond vertex data size {}",
                    self.bytes.len()
                )));
            }
        }

        let position = self
            .channels
            .first()
            .ok_or_else(|| Error::unsupported("Mesh VertexData has no position channel"))?;
        require_float_channel(*position, 0, 3, 4, "position", version)?;
        let normal_channel = self
            .channels
            .get(1)
            .copied()
            .filter(|channel| channel.dimension != 0);
        if let Some(channel) = normal_channel {
            require_float_channel(channel, 1, 3, 4, "normal", version)?;
        }
        let uv0_channel_index = if version.0 < 2018 { 3 } else { 4 };
        let uv0_channel = self
            .channels
            .get(uv0_channel_index)
            .copied()
            .filter(|channel| channel.dimension != 0);
        if let Some(channel) = uv0_channel {
            require_float_channel(channel, uv0_channel_index, 2, 4, "UV0", version)?;
        }
        let has_skin_channels = version >= (2018, 2, 0)
            && self
                .channels
                .get(12..=13)
                .is_some_and(|channels| channels.iter().any(|channel| channel.dimension != 0));
        let decoded_components = 3_usize
            .checked_add(if normal_channel.is_some() { 3 } else { 0 })
            .and_then(|value| value.checked_add(if uv0_channel.is_some() { 2 } else { 0 }))
            .and_then(|value| value.checked_add(if has_skin_channels { 8 } else { 0 }))
            .ok_or_else(|| Error::invalid_data("Mesh decoded component count overflowed"))?;
        let total_decoded = decoded_bytes(self.vertex_count, decoded_components)?;
        if total_decoded > limits.maximum_decoded_bytes {
            return Err(Error::invalid_data(format!(
                "Mesh decoded attributes require {total_decoded} bytes, exceeding limit {}",
                limits.maximum_decoded_bytes
            )));
        }

        let vertices = self.decode_vec3(*position, endian, version)?;
        let normals = normal_channel
            .map(|channel| self.decode_vec3(channel, endian, version))
            .transpose()?;
        let uv0 = uv0_channel
            .map(|channel| self.decode_vec2(channel, endian, version))
            .transpose()?;
        let skin = self.decode_skin(version, endian)?;

        Ok(DecodedVertexData {
            vertices,
            normals,
            uv0,
            skin,
        })
    }

    fn decode_skin(
        &self,
        version: (u32, u32, u32),
        endian: Endian,
    ) -> Result<Option<Vec<MeshBoneWeight>>> {
        if version < (2018, 2, 0) {
            return Ok(None);
        }
        let weight_channel = self
            .channels
            .get(12)
            .copied()
            .filter(|channel| channel.dimension != 0);
        let index_channel = self
            .channels
            .get(13)
            .copied()
            .filter(|channel| channel.dimension != 0);
        if weight_channel.is_none() && index_channel.is_none() {
            return Ok(None);
        }
        if let Some(channel) = weight_channel {
            require_component_dimension(channel, 12, "blend weight")?;
            if vertex_format_kind(channel.format, version)?.is_integer() {
                return Err(Error::unsupported(format!(
                    "Mesh blend-weight channel uses integer format {}",
                    channel.format
                )));
            }
        }
        if let Some(channel) = index_channel {
            require_component_dimension(channel, 13, "blend index")?;
            if !vertex_format_kind(channel.format, version)?.is_integer() {
                return Err(Error::unsupported(format!(
                    "Mesh blend-index channel uses non-integer format {}",
                    channel.format
                )));
            }
        }

        let mut skin = reserve_vec(self.vertex_count, "Mesh skin weights")?;
        for vertex in 0..self.vertex_count {
            let mut influence = MeshBoneWeight::default();
            if let Some(channel) = weight_channel {
                for component in 0..channel.dimension {
                    influence.weights[usize::from(component)] =
                        self.read_float_component(channel, vertex, component, endian, version)?;
                }
            }
            if let Some(channel) = index_channel {
                if channel.dimension == 1 && weight_channel.is_none() {
                    influence.weights[0] = 1.0;
                }
                for component in 0..channel.dimension {
                    influence.bone_indices[usize::from(component)] =
                        self.read_integer_component(channel, vertex, component, endian, version)?;
                }
            }
            skin.push(influence);
        }
        Ok(Some(skin))
    }

    fn decode_vec3(
        &self,
        channel: Channel,
        endian: Endian,
        version: (u32, u32, u32),
    ) -> Result<Vec<[f32; 3]>> {
        let mut values = reserve_vec(self.vertex_count, "Mesh three-component attributes")?;
        for vertex in 0..self.vertex_count {
            values.push([
                self.read_float_component(channel, vertex, 0, endian, version)?,
                self.read_float_component(channel, vertex, 1, endian, version)?,
                self.read_float_component(channel, vertex, 2, endian, version)?,
            ]);
        }
        Ok(values)
    }

    fn decode_vec2(
        &self,
        channel: Channel,
        endian: Endian,
        version: (u32, u32, u32),
    ) -> Result<Vec<[f32; 2]>> {
        let mut values = reserve_vec(self.vertex_count, "Mesh two-component attributes")?;
        for vertex in 0..self.vertex_count {
            values.push([
                self.read_float_component(channel, vertex, 0, endian, version)?,
                self.read_float_component(channel, vertex, 1, endian, version)?,
            ]);
        }
        Ok(values)
    }

    fn read_float_component(
        &self,
        channel: Channel,
        vertex: usize,
        component: u8,
        endian: Endian,
        version: (u32, u32, u32),
    ) -> Result<f32> {
        let kind = vertex_format_kind(channel.format, version)?;
        let bytes = self.component_bytes(channel, vertex, component, kind.size())?;
        Ok(match kind {
            VertexFormatKind::Float32 => read_f32_bytes(bytes, endian),
            VertexFormatKind::Float16 => half_to_f32(read_u16_bytes(bytes, endian)),
            VertexFormatKind::UNorm8 => f32::from(bytes[0]) / 255.0,
            VertexFormatKind::SNorm8 => {
                (f32::from(i8::from_ne_bytes([bytes[0]])) / 127.0).max(-1.0)
            }
            VertexFormatKind::UNorm16 => f32::from(read_u16_bytes(bytes, endian)) / 65_535.0,
            VertexFormatKind::SNorm16 => {
                (f32::from(read_i16_bytes(bytes, endian)) / 32_767.0).max(-1.0)
            }
            _ => {
                return Err(Error::unsupported(format!(
                    "Mesh floating channel uses integer format {}",
                    channel.format
                )));
            }
        })
    }

    fn read_integer_component(
        &self,
        channel: Channel,
        vertex: usize,
        component: u8,
        endian: Endian,
        version: (u32, u32, u32),
    ) -> Result<u32> {
        let kind = vertex_format_kind(channel.format, version)?;
        let bytes = self.component_bytes(channel, vertex, component, kind.size())?;
        match kind {
            VertexFormatKind::UInt8 | VertexFormatKind::SInt8 => Ok(u32::from(bytes[0])),
            VertexFormatKind::UInt16 => Ok(u32::from(read_u16_bytes(bytes, endian))),
            VertexFormatKind::SInt16 => u32::try_from(read_i16_bytes(bytes, endian))
                .map_err(|_| Error::invalid_data("Mesh blend index cannot be negative")),
            VertexFormatKind::UInt32 => Ok(read_u32_bytes(bytes, endian)),
            VertexFormatKind::SInt32 => u32::try_from(read_i32_bytes(bytes, endian))
                .map_err(|_| Error::invalid_data("Mesh blend index cannot be negative")),
            _ => Err(Error::unsupported(format!(
                "Mesh integer channel uses floating format {}",
                channel.format
            ))),
        }
    }

    fn component_bytes(
        &self,
        channel: Channel,
        vertex: usize,
        component: u8,
        size: usize,
    ) -> Result<&[u8]> {
        let stream = self
            .streams
            .get(usize::from(channel.stream))
            .ok_or_else(|| Error::invalid_data("Mesh component references a missing stream"))?;
        let offset = stream
            .stride
            .checked_mul(vertex)
            .and_then(|value| stream.offset.checked_add(value))
            .and_then(|value| value.checked_add(usize::from(channel.offset)))
            .and_then(|value| {
                usize::from(component)
                    .checked_mul(size)
                    .and_then(|component| value.checked_add(component))
            })
            .ok_or_else(|| Error::invalid_data("Mesh component offset overflowed"))?;
        let end = offset
            .checked_add(size)
            .ok_or_else(|| Error::invalid_data("Mesh component range overflowed"))?;
        self.bytes
            .get(offset..end)
            .ok_or_else(|| Error::invalid_data("Mesh component extends beyond vertex data"))
    }
}

struct MeshObjectReader {
    reader: EndianReader<RegionCursor>,
    region: Region,
    absolute_start: u64,
    target_platform: i32,
    format_version: u32,
    limits: MeshReadLimits,
    string_bytes_read: usize,
    compressed_bytes_read: u64,
    auxiliary_bytes_read: u64,
}

impl MeshObjectReader {
    fn new(file: &SerializedFile, object_index: usize, limits: MeshReadLimits) -> Result<Self> {
        let object = require_mesh(file, object_index)?;
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
            string_bytes_read: 0,
            compressed_bytes_read: 0,
            auxiliary_bytes_read: 0,
        })
    }

    fn read_named_object(&mut self) -> Result<String> {
        if self.target_platform == NO_TARGET_PLATFORM {
            self.skip(4, "Object hide flags")?;
            self.skip_pptr("Mesh prefab parent")?;
            self.skip_pptr("Mesh prefab internal")?;
        }
        self.read_aligned_string("Mesh name")
    }

    fn skip_pptr(&mut self, field: &str) -> Result<()> {
        self.skip(4, field)?;
        self.skip(if self.format_version < 14 { 4 } else { 8 }, field)
    }

    fn read_blend_shapes(&mut self) -> Result<Option<MeshBlendShapes>> {
        let vertices = self.read_blend_shape_vertices()?;
        let frames = self.read_blend_shape_frames()?;
        let channels = self.read_blend_shape_channels()?;
        let full_weights = self.read_blend_shape_weights()?;
        if vertices.is_empty()
            && frames.is_empty()
            && channels.is_empty()
            && full_weights.is_empty()
        {
            Ok(None)
        } else {
            Ok(Some(MeshBlendShapes {
                vertices,
                frames,
                channels,
                full_weights,
            }))
        }
    }

    fn read_blend_shape_vertices(&mut self) -> Result<Vec<MeshBlendShapeVertex>> {
        let vertex_count = self.read_record_count(
            self.limits.maximum_blend_shape_vertices,
            40,
            "Mesh blend-shape vertex",
        )?;
        self.charge_auxiliary(
            u64::try_from(vertex_count)
                .expect("blend-shape vertex count fits u64")
                .checked_mul(40)
                .ok_or_else(|| Error::invalid_data("Mesh blend-shape vertex bytes overflowed"))?,
            "Mesh blend-shape vertices",
        )?;
        let mut vertices = reserve_vec(vertex_count, "Mesh blend-shape vertices")?;
        for _ in 0..vertex_count {
            vertices.push(MeshBlendShapeVertex {
                vertex: self.read_vector3()?,
                normal: self.read_vector3()?,
                tangent: self.read_vector3()?,
                index: self.reader.read_u32()?,
            });
        }
        Ok(vertices)
    }

    fn read_blend_shape_frames(&mut self) -> Result<Vec<MeshBlendShapeFrame>> {
        let shape_count = self.read_record_count(
            self.limits.maximum_blend_shape_frames,
            12,
            "Mesh blend shape",
        )?;
        self.charge_auxiliary(
            u64::try_from(shape_count)
                .expect("count fits u64")
                .checked_mul(12)
                .ok_or_else(|| Error::invalid_data("Mesh blend-shape byte count overflowed"))?,
            "Mesh blend shapes",
        )?;
        let mut frames = reserve_vec(shape_count, "Mesh blend-shape frames")?;
        for _ in 0..shape_count {
            let first_vertex = self.reader.read_u32()?;
            let vertex_count = self.reader.read_u32()?;
            let has_normals = self.reader.read_bool()?;
            let has_tangents = self.reader.read_bool()?;
            self.align(4)?;
            frames.push(MeshBlendShapeFrame {
                first_vertex,
                vertex_count,
                has_normals,
                has_tangents,
            });
        }
        Ok(frames)
    }

    fn read_blend_shape_channels(&mut self) -> Result<Vec<MeshBlendShapeChannel>> {
        let channel_count = self.read_record_count(
            self.limits.maximum_blend_shape_channels,
            16,
            "Mesh blend-shape channel",
        )?;
        let mut channels = reserve_vec(channel_count, "Mesh blend-shape channels")?;
        for _ in 0..channel_count {
            let name = self.read_aligned_string("Mesh blend-shape channel name")?;
            self.charge_auxiliary(12, "Mesh blend-shape channels")?;
            let name_hash = self.reader.read_u32()?;
            let frame_index = checked_length(
                self.reader.read_i32()?,
                "Mesh blend-shape channel frame index",
            )?;
            let frame_count = checked_length(
                self.reader.read_i32()?,
                "Mesh blend-shape channel frame count",
            )?;
            channels.push(MeshBlendShapeChannel {
                name,
                name_hash,
                frame_index,
                frame_count,
            });
        }
        Ok(channels)
    }

    fn read_blend_shape_weights(&mut self) -> Result<Vec<f32>> {
        let weight_count = self.read_record_count(
            self.limits.maximum_blend_shape_frames,
            4,
            "Mesh blend-shape full weight",
        )?;
        self.charge_auxiliary(
            u64::try_from(weight_count)
                .expect("blend-shape weight count fits u64")
                .checked_mul(4)
                .ok_or_else(|| Error::invalid_data("Mesh blend-shape weight bytes overflowed"))?,
            "Mesh blend-shape full weights",
        )?;
        let mut full_weights = reserve_vec(weight_count, "Mesh blend-shape weights")?;
        for _ in 0..weight_count {
            full_weights.push(self.reader.read_f32()?);
        }
        Ok(full_weights)
    }

    fn read_vector3(&mut self) -> Result<[f32; 3]> {
        Ok([
            self.reader.read_f32()?,
            self.reader.read_f32()?,
            self.reader.read_f32()?,
        ])
    }

    fn read_bind_poses(&mut self) -> Result<Vec<[f32; 16]>> {
        let count = self.read_record_count(self.limits.maximum_bones, 64, "Mesh bind pose")?;
        self.charge_auxiliary(
            u64::try_from(count)
                .expect("bind-pose count fits u64")
                .checked_mul(64)
                .ok_or_else(|| Error::invalid_data("Mesh bind-pose byte count overflowed"))?,
            "Mesh bind poses",
        )?;
        let mut matrices = reserve_vec(count, "Mesh bind poses")?;
        for _ in 0..count {
            let mut matrix = [0.0_f32; 16];
            for value in &mut matrix {
                *value = self.reader.read_f32()?;
            }
            matrices.push(matrix);
        }
        Ok(matrices)
    }

    fn read_bone_name_hashes(&mut self) -> Result<Vec<u32>> {
        let count = self.read_record_count(self.limits.maximum_bones, 4, "Mesh bone name hash")?;
        self.charge_auxiliary(
            u64::try_from(count)
                .expect("bone hash count fits u64")
                .checked_mul(4)
                .ok_or_else(|| Error::invalid_data("Mesh bone-hash byte count overflowed"))?,
            "Mesh bone name hashes",
        )?;
        let mut hashes = reserve_vec(count, "Mesh bone name hashes")?;
        for _ in 0..count {
            hashes.push(self.reader.read_u32()?);
        }
        Ok(hashes)
    }

    fn read_legacy_skin(&mut self) -> Result<Option<Vec<MeshBoneWeight>>> {
        let count =
            self.read_record_count(self.limits.maximum_skin_weights, 32, "Mesh skin weight")?;
        if count == 0 {
            return Ok(None);
        }
        self.charge_auxiliary(
            u64::try_from(count)
                .expect("skin-weight count fits u64")
                .checked_mul(32)
                .ok_or_else(|| Error::invalid_data("Mesh skin-weight byte count overflowed"))?,
            "Mesh skin weights",
        )?;
        let mut skin = reserve_vec(count, "Mesh skin weights")?;
        for _ in 0..count {
            let mut influence = MeshBoneWeight::default();
            for weight in &mut influence.weights {
                *weight = self.reader.read_f32()?;
            }
            for bone_index in &mut influence.bone_indices {
                *bone_index = u32::try_from(self.reader.read_i32()?)
                    .map_err(|_| Error::invalid_data("Mesh bone index cannot be negative"))?;
            }
            skin.push(influence);
        }
        Ok(Some(skin))
    }

    fn read_vertex_data(&mut self, version: (u32, u32, u32)) -> Result<VertexData> {
        if version.0 < 2018 {
            self.skip(4, "Mesh current vertex channels")?;
        }
        let vertex_count = usize::try_from(self.reader.read_u32()?)
            .map_err(|_| Error::invalid_data("Mesh vertex count does not fit in usize"))?;
        if vertex_count > self.limits.maximum_vertices {
            return Err(Error::invalid_data(format!(
                "Mesh has {vertex_count} vertices, exceeding limit {}",
                self.limits.maximum_vertices
            )));
        }
        let channel_count =
            self.read_record_count(self.limits.maximum_channels, 4, "Mesh vertex channel")?;
        if channel_count == 0 {
            return Err(Error::unsupported("Mesh VertexData has no channels"));
        }
        let mut channels = reserve_vec(channel_count, "Mesh vertex channels")?;
        for _ in 0..channel_count {
            channels.push(Channel {
                stream: self.reader.read_u8()?,
                offset: self.reader.read_u8()?,
                format: self.reader.read_u8()?,
                dimension: self.reader.read_u8()? & 0x0f,
            });
        }
        let stream_count = channels
            .iter()
            .map(|channel| usize::from(channel.stream))
            .max()
            .and_then(|value| value.checked_add(1))
            .ok_or_else(|| Error::invalid_data("Mesh vertex stream count overflowed"))?;
        if stream_count > self.limits.maximum_channels {
            return Err(Error::invalid_data(format!(
                "Mesh has {stream_count} vertex streams, exceeding limit {}",
                self.limits.maximum_channels
            )));
        }
        let mut streams = reserve_vec(stream_count, "Mesh vertex streams")?;
        let mut stream_offset = 0_usize;
        for stream_index in 0..stream_count {
            let mut stride = 0_usize;
            for channel in channels
                .iter()
                .filter(|channel| usize::from(channel.stream) == stream_index)
                .filter(|channel| channel.dimension != 0)
            {
                if channel.dimension > 4 {
                    return Err(Error::unsupported(format!(
                        "Mesh vertex channel dimension {}",
                        channel.dimension
                    )));
                }
                let size = vertex_format_size(channel.format, version)?;
                let channel_size = usize::from(channel.dimension)
                    .checked_mul(size)
                    .ok_or_else(|| Error::invalid_data("Mesh vertex stride overflowed"))?;
                if usize::from(channel.offset) != stride {
                    return Err(Error::unsupported(format!(
                        "Mesh vertex channel offset {} creates a non-packed stream layout",
                        channel.offset
                    )));
                }
                stride = stride
                    .checked_add(channel_size)
                    .ok_or_else(|| Error::invalid_data("Mesh vertex stride overflowed"))?;
            }
            streams.push(Stream {
                offset: stream_offset,
                stride,
            });
            stream_offset = stream_offset
                .checked_add(
                    vertex_count
                        .checked_mul(stride)
                        .ok_or_else(|| Error::invalid_data("Mesh stream size overflowed"))?,
                )
                .and_then(|value| align_up(value, STREAM_ALIGNMENT))
                .ok_or_else(|| Error::invalid_data("Mesh stream offset overflowed"))?;
        }

        let byte_length = self.read_length("Mesh vertex data")?;
        if byte_length > self.limits.maximum_vertex_data_bytes {
            return Err(Error::invalid_data(format!(
                "Mesh vertex data is {byte_length} bytes, exceeding limit {}",
                self.limits.maximum_vertex_data_bytes
            )));
        }
        let bytes = self.reader.read_bytes(byte_length)?;
        self.align(4)?;
        Ok(VertexData {
            vertex_count,
            channels,
            streams,
            bytes,
        })
    }

    fn read_compressed_mesh(&mut self) -> Result<CompressedMesh> {
        let vertices = self.read_packed_float("vertices")?;
        let uv = self.read_packed_float("UVs")?;
        let normals = self.read_packed_float("normals")?;
        let tangents = self.read_packed_float("tangents")?;
        let weights = self.read_packed_int("weights")?;
        let normal_signs = self.read_packed_int("normal signs")?;
        let tangent_signs = self.read_packed_int("tangent signs")?;
        let float_colors = self.read_packed_float("float colors")?;
        let bone_indices = self.read_packed_int("bone indices")?;
        let triangles = self.read_packed_int("triangles")?;
        let uv_info = self.reader.read_u32()?;
        Ok(CompressedMesh {
            vertices,
            uv,
            normals,
            tangents,
            weights,
            normal_signs,
            tangent_signs,
            float_colors,
            bone_indices,
            triangles,
            uv_info,
        })
    }

    fn skip_tuanjie_shared_cluster(&mut self, revision: u8) -> Result<()> {
        if !(1..=3).contains(&revision) {
            return Err(Error::invalid_data(format!(
                "Tuanjie SharedClusterData revision {revision} is invalid"
            )));
        }

        self.skip(8, "Tuanjie Mesh shared-cluster header")?;
        if revision == 1 {
            self.skip(16, "Tuanjie Mesh shared-cluster input counts")?;
        }
        self.skip_counted_auxiliary_bytes("Tuanjie Mesh root cluster page")?;
        if revision == 1 {
            self.skip_counted_auxiliary_records(2, "Tuanjie Mesh imposter atlas")?;
        }
        self.skip_counted_auxiliary_records(416, "Tuanjie Mesh hierarchy node")?;
        if revision == 1 {
            self.skip_counted_auxiliary_records(4, "Tuanjie Mesh hierarchy root offset")?;
        }
        self.skip_counted_auxiliary_records(24, "Tuanjie Mesh page streaming info")?;
        self.skip_counted_auxiliary_records(4, "Tuanjie Mesh page dependency index")?;
        if revision == 2 {
            self.skip(16, "Tuanjie Mesh shared-cluster input counts")?;
            self.skip_counted_auxiliary_records(2, "Tuanjie Mesh imposter atlas")?;
            self.skip_counted_auxiliary_records(4, "Tuanjie Mesh hierarchy root offset")?;
        }
        self.skip_counted_auxiliary_bytes("Tuanjie Mesh streamable cluster page")
    }

    fn read_packed_float(&mut self, field: &str) -> Result<PackedFloatVector> {
        let item_count = self.reader.read_u32()?;
        let range = self.reader.read_f32()?;
        let start = self.reader.read_f32()?;
        let byte_length = self.read_length(&format!("Mesh compressed {field}"))?;
        let data = self.read_compressed(byte_length, field)?;
        self.align(4)?;
        let bit_size = self.reader.read_u8()?;
        self.align(4)?;
        Ok(PackedFloatVector {
            item_count,
            range,
            start,
            data,
            bit_size,
        })
    }

    fn read_packed_int(&mut self, field: &str) -> Result<PackedIntVector> {
        let item_count = self.reader.read_u32()?;
        let byte_length = self.read_length(&format!("Mesh compressed {field}"))?;
        let data = self.read_compressed(byte_length, field)?;
        self.align(4)?;
        let bit_size = self.reader.read_u8()?;
        self.align(4)?;
        Ok(PackedIntVector {
            item_count,
            data,
            bit_size,
        })
    }

    /// Reads one packed payload, charging it against the compressed-data budget.
    fn read_compressed(&mut self, byte_length: usize, field: &str) -> Result<Vec<u8>> {
        self.charge_compressed(byte_length, field)?;
        self.reader.read_bytes(byte_length)
    }

    fn charge_compressed(&mut self, byte_length: usize, field: &str) -> Result<()> {
        let length = u64::try_from(byte_length)
            .map_err(|_| Error::invalid_data("Mesh compressed length does not fit in u64"))?;
        self.compressed_bytes_read = self
            .compressed_bytes_read
            .checked_add(length)
            .ok_or_else(|| Error::invalid_data("Mesh compressed byte budget overflowed"))?;
        if self.compressed_bytes_read > self.limits.maximum_compressed_data_bytes {
            return Err(Error::invalid_data(format!(
                "Mesh compressed data exceeds the {} byte limit while reading {field}",
                self.limits.maximum_compressed_data_bytes
            )));
        }
        Ok(())
    }

    fn skip_counted_auxiliary_records(&mut self, record_size: u64, field: &str) -> Result<()> {
        let count =
            self.read_record_count(self.limits.maximum_array_elements, record_size, field)?;
        let length = u64::try_from(count)
            .expect("array count fits u64")
            .checked_mul(record_size)
            .ok_or_else(|| Error::invalid_data(format!("{field} byte size overflowed")))?;
        self.charge_auxiliary(length, field)?;
        self.skip(length, field)
    }

    fn skip_counted_auxiliary_bytes(&mut self, field: &str) -> Result<()> {
        let length = self.read_length(field)?;
        let length = u64::try_from(length)
            .map_err(|_| Error::invalid_data(format!("{field} size does not fit in u64")))?;
        self.charge_auxiliary(length, field)?;
        self.skip(length, field)
    }

    fn charge_auxiliary(&mut self, length: u64, field: &str) -> Result<()> {
        self.auxiliary_bytes_read = self
            .auxiliary_bytes_read
            .checked_add(length)
            .ok_or_else(|| Error::invalid_data("Mesh auxiliary byte budget overflowed"))?;
        if self.auxiliary_bytes_read > self.limits.maximum_auxiliary_bytes {
            return Err(Error::invalid_data(format!(
                "Mesh auxiliary data exceeds the {} byte limit while reading {field}",
                self.limits.maximum_auxiliary_bytes
            )));
        }
        Ok(())
    }

    fn read_record_count(
        &mut self,
        maximum: usize,
        record_size: u64,
        field: &str,
    ) -> Result<usize> {
        let count = self.read_length(field)?;
        if count > maximum {
            return Err(Error::invalid_data(format!(
                "{field} count {count} exceeds limit {maximum}"
            )));
        }
        let required = u64::try_from(count)
            .expect("record count fits u64")
            .checked_mul(record_size)
            .ok_or_else(|| Error::invalid_data(format!("{field} byte size overflowed")))?;
        if required > self.reader.remaining()? {
            return Err(Error::invalid_data(format!(
                "{field} records require at least {required} bytes but only {} remain",
                self.reader.remaining()?
            )));
        }
        Ok(count)
    }

    fn read_length(&mut self, field: &str) -> Result<usize> {
        checked_length(self.reader.read_i32()?, field)
    }

    fn read_aligned_string(&mut self, field: &str) -> Result<String> {
        let length = self.read_length(field)?;
        if length > self.limits.maximum_string_bytes {
            return Err(Error::invalid_data(format!(
                "{field} is {length} bytes, exceeding limit {}",
                self.limits.maximum_string_bytes
            )));
        }
        self.string_bytes_read = self
            .string_bytes_read
            .checked_add(length)
            .ok_or_else(|| Error::invalid_data("Mesh string byte budget overflowed"))?;
        if self.string_bytes_read > self.limits.maximum_total_string_bytes {
            return Err(Error::invalid_data(format!(
                "Mesh strings exceed the {} byte total limit",
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
                "{field} ends at {target}, beyond Mesh object size {}",
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
            .ok_or_else(|| Error::invalid_data("Mesh alignment position overflowed"))?;
        let remainder = absolute % alignment;
        if remainder == 0 {
            return Ok(());
        }
        self.skip(alignment - remainder, "Mesh alignment")
    }

    fn finish(&mut self) -> Result<()> {
        let position = self.reader.position()?;
        if position != self.region.len() {
            return Err(Error::unsupported(format!(
                "Mesh layout has {} unparsed trailing bytes",
                self.region.len() - position
            )));
        }
        Ok(())
    }
}

fn tuanjie_shared_cluster_revision(file: &SerializedFile) -> u8 {
    let version = file.unity_version.components();
    if version < (2022, 3, 48) || (version == (2022, 3, 48) && file.unity_version.build < 3) {
        1
    } else if version < (2022, 3, 61) || (version == (2022, 3, 61) && file.unity_version.build < 2)
    {
        2
    } else {
        3
    }
}

fn tuanjie_has_virtual_geometry_flags(file: &SerializedFile) -> bool {
    let version = file.unity_version.components();
    version > (2022, 3, 2) || (version == (2022, 3, 2) && file.unity_version.build >= 13)
}

fn require_mesh(file: &SerializedFile, object_index: usize) -> Result<&ObjectInfo> {
    let object = file.objects.get(object_index).ok_or_else(|| {
        Error::invalid_data(format!(
            "serialized object index {object_index} is out of range"
        ))
    })?;
    if object.class_id != MESH_CLASS_ID {
        return Err(Error::unsupported(format!(
            "object {} has class ID {}, not Mesh ({MESH_CLASS_ID})",
            object.path_id, object.class_id
        )));
    }
    Ok(object)
}

/// Accepts any floating-point channel format, matching the managed reader.
///
/// Unity's Vertex Compression player setting stores normals, tangents and
/// texture coordinates as Float16, and platform meshes also use the normalized
/// 8- and 16-bit formats, so restricting this to Float32 would reject a large
/// share of shipped meshes. Integer formats stay unsupported here: they belong
/// to blend indices, not to positions or texture coordinates.
fn require_float_channel(
    channel: Channel,
    index: usize,
    minimum_dimension: u8,
    maximum_dimension: u8,
    label: &str,
    version: (u32, u32, u32),
) -> Result<()> {
    let is_float = vertex_format_kind(channel.format, version).is_ok_and(|kind| !kind.is_integer());
    if !is_float || !(minimum_dimension..=maximum_dimension).contains(&channel.dimension) {
        return Err(Error::unsupported(format!(
            "Mesh {label} channel {index} uses format {} and dimension {}; expected a floating-point format with {minimum_dimension}-{maximum_dimension} components",
            channel.format, channel.dimension
        )));
    }
    Ok(())
}

fn require_component_dimension(channel: Channel, index: usize, label: &str) -> Result<()> {
    if !(1..=4).contains(&channel.dimension) {
        return Err(Error::unsupported(format!(
            "Mesh {label} channel {index} has dimension {}; expected 1-4",
            channel.dimension
        )));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VertexFormatKind {
    Float32,
    Float16,
    UNorm8,
    SNorm8,
    UNorm16,
    SNorm16,
    UInt8,
    SInt8,
    UInt16,
    SInt16,
    UInt32,
    SInt32,
}

impl VertexFormatKind {
    const fn size(self) -> usize {
        match self {
            Self::Float32 | Self::UInt32 | Self::SInt32 => 4,
            Self::Float16 | Self::UNorm16 | Self::SNorm16 | Self::UInt16 | Self::SInt16 => 2,
            Self::UNorm8 | Self::SNorm8 | Self::UInt8 | Self::SInt8 => 1,
        }
    }

    const fn is_integer(self) -> bool {
        matches!(
            self,
            Self::UInt8 | Self::SInt8 | Self::UInt16 | Self::SInt16 | Self::UInt32 | Self::SInt32
        )
    }
}

fn vertex_format_kind(format: u8, version: (u32, u32, u32)) -> Result<VertexFormatKind> {
    let kind = if version.0 < 2019 {
        match format {
            0 => Some(VertexFormatKind::Float32),
            1 => Some(VertexFormatKind::Float16),
            2 | 3 => Some(VertexFormatKind::UNorm8),
            4 => Some(VertexFormatKind::SNorm8),
            5 => Some(VertexFormatKind::UNorm16),
            6 => Some(VertexFormatKind::SNorm16),
            7 => Some(VertexFormatKind::UInt8),
            8 => Some(VertexFormatKind::SInt8),
            9 => Some(VertexFormatKind::UInt16),
            10 => Some(VertexFormatKind::SInt16),
            11 => Some(VertexFormatKind::UInt32),
            12 => Some(VertexFormatKind::SInt32),
            _ => None,
        }
    } else {
        match format {
            0 => Some(VertexFormatKind::Float32),
            1 => Some(VertexFormatKind::Float16),
            2 => Some(VertexFormatKind::UNorm8),
            3 => Some(VertexFormatKind::SNorm8),
            4 => Some(VertexFormatKind::UNorm16),
            5 => Some(VertexFormatKind::SNorm16),
            6 => Some(VertexFormatKind::UInt8),
            7 => Some(VertexFormatKind::SInt8),
            8 => Some(VertexFormatKind::UInt16),
            9 => Some(VertexFormatKind::SInt16),
            10 => Some(VertexFormatKind::UInt32),
            11 => Some(VertexFormatKind::SInt32),
            _ => None,
        }
    };
    kind.ok_or_else(|| {
        Error::unsupported(format!(
            "Mesh vertex format {format} for Unity {}.{}",
            version.0, version.1
        ))
    })
}

pub(crate) fn vertex_format_size(format: u8, version: (u32, u32, u32)) -> Result<usize> {
    vertex_format_kind(format, version).map(VertexFormatKind::size)
}

fn read_u16_bytes(bytes: &[u8], endian: Endian) -> u16 {
    let bytes: [u8; 2] = bytes.try_into().expect("validated two-byte component");
    match endian {
        Endian::Little => u16::from_le_bytes(bytes),
        Endian::Big => u16::from_be_bytes(bytes),
    }
}

fn read_i16_bytes(bytes: &[u8], endian: Endian) -> i16 {
    let bytes: [u8; 2] = bytes.try_into().expect("validated two-byte component");
    match endian {
        Endian::Little => i16::from_le_bytes(bytes),
        Endian::Big => i16::from_be_bytes(bytes),
    }
}

fn read_u32_bytes(bytes: &[u8], endian: Endian) -> u32 {
    let bytes: [u8; 4] = bytes.try_into().expect("validated four-byte component");
    match endian {
        Endian::Little => u32::from_le_bytes(bytes),
        Endian::Big => u32::from_be_bytes(bytes),
    }
}

fn read_i32_bytes(bytes: &[u8], endian: Endian) -> i32 {
    let bytes: [u8; 4] = bytes.try_into().expect("validated four-byte component");
    match endian {
        Endian::Little => i32::from_le_bytes(bytes),
        Endian::Big => i32::from_be_bytes(bytes),
    }
}

fn read_f32_bytes(bytes: &[u8], endian: Endian) -> f32 {
    f32::from_bits(read_u32_bytes(bytes, endian))
}

fn half_to_f32(bits: u16) -> f32 {
    let sign = u32::from(bits & 0x8000) << 16;
    let exponent = (bits >> 10) & 0x1f;
    let fraction = bits & 0x03ff;
    match exponent {
        0 if fraction == 0 => f32::from_bits(sign),
        0 => {
            let magnitude = f32::from(fraction) * 2.0_f32.powi(-24);
            if sign == 0 { magnitude } else { -magnitude }
        }
        0x1f => f32::from_bits(sign | 0x7f80_0000 | (u32::from(fraction) << 13)),
        _ => {
            let adjusted = u32::from(exponent) + 112;
            f32::from_bits(sign | (adjusted << 23) | (u32::from(fraction) << 13))
        }
    }
}

fn decoded_bytes(count: usize, components: usize) -> Result<u64> {
    u64::try_from(count)
        .expect("count fits u64")
        .checked_mul(u64::try_from(components).expect("component count fits u64"))
        .and_then(|value| value.checked_mul(4))
        .ok_or_else(|| Error::invalid_data("Mesh decoded byte count overflowed"))
}

fn align_up(value: usize, alignment: usize) -> Option<usize> {
    value
        .checked_add(alignment.checked_sub(1)?)
        .map(|value| value / alignment * alignment)
}

fn reserve_vec<T>(capacity: usize, field: &str) -> Result<Vec<T>> {
    let mut values = Vec::new();
    values.try_reserve_exact(capacity).map_err(|error| {
        Error::invalid_data(format!("cannot allocate {capacity} {field}: {error}"))
    })?;
    Ok(values)
}

/// A float rendered the way the managed exporter renders it.
///
/// The managed writer formats each value with `string.Format` under the
/// invariant culture, which is .NET's general format: the shortest digits that
/// round-trip, laid out as a plain decimal when the value's exponent falls in
/// `[-4, 8]` and in scientific notation otherwise. Rust's `Display` never
/// switches to scientific notation, so a normal component of `4.3e-8` -- an
/// ordinary amount of floating-point noise in real mesh data, not an
/// artificial edge case -- came out as `0.000000043` where the managed file
/// has `4.3E-08`. Both describe the same geometry, so nothing an importer
/// reads changes, but the documents stop being comparable byte for byte, and
/// that comparison is what the differential is for.
pub(crate) struct ObjFloat(pub(crate) f32);

impl fmt::Display for ObjFloat {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = self.0;
        if value.is_nan() {
            // The managed writer substitutes NaN as it writes each line, so the
            // token never reaches the document.
            return formatter.write_str("0");
        }
        if value.is_infinite() {
            return formatter.write_str(if value < 0.0 { "-Infinity" } else { "Infinity" });
        }
        let text = ManagedNumber::single(value).ok_or(fmt::Error)?;
        formatter.write_str(text.as_str())
    }
}

pub(crate) struct BoundedWriter<'a, W> {
    inner: &'a mut W,
    maximum: u64,
    pub(crate) written: u64,
    pub(crate) limit_exceeded: bool,
}

impl<'a, W> BoundedWriter<'a, W> {
    pub(crate) const fn new(inner: &'a mut W, maximum: u64) -> Self {
        Self {
            inner,
            maximum,
            written: 0,
            limit_exceeded: false,
        }
    }
}

impl<W: Write> Write for BoundedWriter<'_, W> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let length = u64::try_from(buffer.len())
            .map_err(|_| io::Error::other("Mesh OBJ write length does not fit in u64"))?;
        let end = self
            .written
            .checked_add(length)
            .ok_or_else(|| io::Error::other("Mesh OBJ output length overflowed"))?;
        if end > self.maximum {
            self.limit_exceeded = true;
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "Mesh OBJ output byte limit exceeded",
            ));
        }
        let written = self.inner.write(buffer)?;
        self.written = self
            .written
            .checked_add(u64::try_from(written).expect("write count fits u64"))
            .ok_or_else(|| io::Error::other("Mesh OBJ output length overflowed"))?;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use crate::loader::{AssetCollection, LoadedResource, LoadedSerializedFile};
    use crate::source::Region;
    use crate::studio::Studio;

    use super::{
        MESH_CLASS_ID, Mesh, MeshReadLimits, MeshSubMesh, STREAM_ALIGNMENT, read_mesh,
        read_mesh_with_collection, triangulate, write_mesh_obj, write_mesh_object_obj,
    };

    #[test]
    fn obj_floats_render_the_way_the_managed_exporter_renders_them() {
        use super::ObjFloat;

        // Probed from .NET 10:
        //   string.Format(CultureInfo.InvariantCulture, "{0}", value)
        // which is what the managed OBJ writer calls for every coordinate.
        // Values are given as bit patterns so nothing is rounded to a
        // neighbour on the way into the test.
        const CASES: &[(u32, &str)] = &[
            (0, "0"),                          // zero
            (2_147_483_648, "-0"),             // negative zero, which `-x` produces
            (1_056_964_608, "0.5"),            // a half
            (3_214_934_016, "-1.25"),          // a negative
            (1_120_403_456, "100"),            // an integral value
            (1_036_831_949, "0.1"),            // a tenth
            (1_051_372_203, "0.33333334"),     // a third
            (953_267_991, "0.0001"),           // the smallest fixed-point magnitude
            (953_267_990, "9.999999E-05"),     // just below it, which turns scientific
            (925_353_388, "1E-05"),            // a round value past the threshold
            (1_315_859_238, "999999900"),      // the largest fixed-point magnitude
            (1_315_859_240, "1E+09"),          // just above it, which turns scientific
            (1_318_263_473, "1.234E+09"),      // scientific with a fraction
            (3_382_607_226, "-1298351.2"),     // a tie, rounded to even
            (1_184_677_024, "20062.312"),      // another tie
            (1, "1E-45"),                      // the smallest subnormal
            (8_388_607, "1.1754942E-38"),      // the largest subnormal
            (8_388_608, "1.1754944E-38"),      // the smallest normal
            (4_286_578_687, "-3.4028235E+38"), // the most negative finite value
            (2_139_095_039, "3.4028235E+38"),  // the largest finite value
            (1_266_679_808, "16777216"),       // the largest exact integer
            (1_290_500_515, "123456790"),      // nine significant digits
            (2_139_095_040, "Infinity"),
            (4_286_578_688, "-Infinity"),
        ];
        for (bits, expected) in CASES {
            assert_eq!(
                ObjFloat(f32::from_bits(*bits)).to_string(),
                *expected,
                "bits {bits}"
            );
        }
        // The managed writer substitutes NaN as it writes each line, so the
        // token never reaches the document.
        assert_eq!(ObjFloat(f32::NAN).to_string(), "0");
        assert_eq!(ObjFloat(-f32::NAN).to_string(), "0");
    }

    #[test]
    fn parses_resident_uncompressed_v22_mesh_and_writes_exact_obj_contract() {
        let object = resident_mesh_fixture(MeshFixtureOptions::default());
        let file = parse_v22_mesh(&object);

        let mesh = read_mesh(&file, 0, MeshReadLimits::default()).unwrap();

        assert_eq!(mesh.path_id, 7);
        assert_eq!(mesh.name, "tri");
        assert_float_slice(&mesh.vertices[0], &[1.5, 0.0, 0.0]);
        assert!(mesh.vertices[1][0].is_nan());
        assert_eq!(&mesh.vertices[1][1..], &[2.0, 0.0]);
        assert_float_slice(&mesh.vertices[2], &[0.0, 0.0, 3.0]);
        assert_float_slice(&mesh.normals.as_ref().unwrap()[0], &[1.0, 0.0, 0.0]);
        assert_float_slice(&mesh.uv0.as_ref().unwrap()[2], &[0.0, 1.0]);
        assert_eq!(mesh.sub_meshes[0].indices, [0, 1, 2]);

        let mut output = Vec::new();
        let written =
            write_mesh_object_obj(&file, 0, &mut output, MeshReadLimits::default()).unwrap();
        assert_eq!(written, u64::try_from(output.len()).unwrap());
        assert_eq!(
            String::from_utf8(output).unwrap(),
            concat!(
                "g tri\r\n",
                "v -1.5 0 0\r\n",
                "v 0 2 0\r\n",
                "v -0 0 3\r\n",
                "vt 0 0\r\n",
                "vt 1 0\r\n",
                "vt 0 1\r\n",
                "vn -1 0 0\r\n",
                "vn -1 0 0\r\n",
                "vn -1 0 0\r\n",
                "g tri_0\r\n",
                "f 3/3/3 2/2/2 1/1/1\r\n",
            )
        );
    }

    #[test]
    fn high_level_studio_reads_one_mesh_obj_with_an_output_limit() {
        let file = parse_v22_mesh(&resident_mesh_fixture(MeshFixtureOptions::default()));
        let studio = Studio::from_collection(AssetCollection::from_loaded_parts(
            vec![LoadedSerializedFile {
                path: "mesh.assets".to_owned(),
                file,
            }],
            Vec::new(),
        ));
        let object = studio.object(0, 7).unwrap();

        let obj = object.read_mesh_obj(MeshReadLimits::default()).unwrap();

        assert!(obj.starts_with(b"g tri\r\nv -1.5 0 0\r\n"));
        assert!(obj.ends_with(b"f 3/3/3 2/2/2 1/1/1\r\n"));
        assert!(
            object
                .read_mesh_obj(MeshReadLimits {
                    maximum_output_bytes: 8,
                    ..MeshReadLimits::default()
                })
                .is_err()
        );
    }

    #[test]
    fn retains_bounded_blend_shape_vertices_frames_channels_and_weights() {
        let object = resident_mesh_fixture(MeshFixtureOptions {
            blend_shapes: true,
            ..MeshFixtureOptions::default()
        });
        let mesh = read_mesh(&parse_v22_mesh(&object), 0, MeshReadLimits::default()).unwrap();
        let shapes = mesh.blend_shapes.as_ref().unwrap();
        assert_eq!(shapes.vertices.len(), 1);
        assert_eq!(shapes.vertices[0].index, 1);
        assert_float_slice(&shapes.vertices[0].vertex, &[0.5, 0.0, 0.0]);
        assert_float_slice(&shapes.vertices[0].normal, &[0.0, 0.25, 0.0]);
        assert_eq!(shapes.frames.len(), 1);
        assert!(shapes.frames[0].has_normals);
        assert!(!shapes.frames[0].has_tangents);
        assert_eq!(shapes.channels[0].name, "face.Smile");
        assert_eq!(shapes.channels[0].name_hash, 0x1234_5678);
        assert_eq!(shapes.channels[0].frame_index, 0);
        assert_eq!(shapes.channels[0].frame_count, 1);
        assert_float_slice(&shapes.full_weights, &[100.0]);

        let limits = MeshReadLimits {
            maximum_blend_shape_vertices: 0,
            ..MeshReadLimits::default()
        };
        assert!(read_mesh(&parse_v22_mesh(&object), 0, limits).is_err());
    }

    #[test]
    fn parses_resident_uncompressed_unity_2017_and_2018_layouts() {
        for (unity_version, layout_version, index_width, skin_records) in [
            ("2017.3.0f3", (2017, 3, 0), 2, 3),
            ("2018.1.9f2", (2018, 1, 9), 4, 3),
            ("2018.2.21f1", (2018, 2, 21), 2, 0),
            ("2018.3.14f1", (2018, 3, 14), 4, 0),
        ] {
            let object = resident_mesh_fixture(MeshFixtureOptions {
                index_width,
                skin_records,
                layout_version,
                ..MeshFixtureOptions::default()
            });
            let mesh = read_mesh(
                &parse_v22_mesh_version(&object, unity_version),
                0,
                MeshReadLimits::default(),
            )
            .unwrap();

            assert_eq!(mesh.sub_meshes[0].indices, [0, 1, 2]);
            assert_float_slice(&mesh.vertices[0], &[1.5, 0.0, 0.0]);
            assert_float_slice(&mesh.normals.as_ref().unwrap()[0], &[1.0, 0.0, 0.0]);
            assert_float_slice(&mesh.uv0.as_ref().unwrap()[2], &[0.0, 1.0]);
            assert_eq!(
                mesh.skin.as_ref().map(Vec::len),
                (skin_records != 0).then_some(3)
            );
        }
    }

    #[test]
    fn parses_resident_uncompressed_unity_2023_and_6000_layouts() {
        for (unity_version, layout_version) in [
            ("2023.3.0f1", (2023, 3, 0)),
            ("6000.0.0f1", (6000, 0, 0)),
            ("6000.1.0f1", (6000, 1, 0)),
        ] {
            let object = resident_mesh_fixture(MeshFixtureOptions {
                layout_version,
                ..MeshFixtureOptions::default()
            });
            let mesh = read_mesh(
                &parse_v22_mesh_version(&object, unity_version),
                0,
                MeshReadLimits::default(),
            )
            .unwrap();
            assert_eq!(mesh.sub_meshes[0].indices, [0, 1, 2], "{unity_version}");
            assert_float_slice(&mesh.vertices[0], &[1.5, 0.0, 0.0]);
            assert_float_slice(&mesh.uv0.as_ref().unwrap()[2], &[0.0, 1.0]);
        }
    }

    #[test]
    fn parses_tuanjie_shared_cluster_revisions_and_rejects_virtual_geometry() {
        for (unity_version, revision, has_tail_flags) in [
            ("2022.3.2t1", 1, false),
            ("2022.3.2t13", 1, true),
            ("2022.3.48t2", 1, true),
            ("2022.3.48t3", 2, true),
            ("2022.3.61t1", 2, true),
            ("2022.3.61t2", 3, true),
        ] {
            let object = resident_mesh_fixture(MeshFixtureOptions {
                tuanjie: Some(TuanjieMeshFixture {
                    revision,
                    has_tail_flags,
                    virtual_geometry: false,
                    root_cluster_bytes: 3,
                }),
                ..MeshFixtureOptions::default()
            });
            let mesh = read_mesh(
                &parse_v22_mesh_version(&object, unity_version),
                0,
                MeshReadLimits::default(),
            )
            .unwrap();

            assert_eq!(mesh.sub_meshes[0].indices, [0, 1, 2], "{unity_version}");
            assert_float_slice(&mesh.vertices[0], &[1.5, 0.0, 0.0]);
            assert_float_slice(&mesh.uv0.as_ref().unwrap()[2], &[0.0, 1.0]);
        }

        let virtual_geometry = resident_mesh_fixture(MeshFixtureOptions {
            tuanjie: Some(TuanjieMeshFixture {
                revision: 1,
                has_tail_flags: true,
                virtual_geometry: true,
                root_cluster_bytes: 0,
            }),
            ..MeshFixtureOptions::default()
        });
        let error = read_mesh(
            &parse_v22_mesh_version(&virtual_geometry, "2022.3.2t13"),
            0,
            MeshReadLimits::default(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("virtual geometry"));

        let auxiliary = resident_mesh_fixture(MeshFixtureOptions {
            tuanjie: Some(TuanjieMeshFixture {
                revision: 3,
                has_tail_flags: true,
                virtual_geometry: false,
                root_cluster_bytes: 8,
            }),
            ..MeshFixtureOptions::default()
        });
        let error = read_mesh(
            &parse_v22_mesh_version(&auxiliary, "2022.3.61t2"),
            0,
            MeshReadLimits {
                maximum_auxiliary_bytes: 7,
                ..MeshReadLimits::default()
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("auxiliary data"));
    }

    #[test]
    fn resolves_external_vertex_streams_for_core_studio_and_bounded_callers() {
        let options = MeshFixtureOptions {
            stream_path: "archive:/mesh.resS",
            stream_offset: 7,
            stream_size: 120,
            truncate_vertex_data: true,
            layout_version: (6000, 1, 0),
            ..MeshFixtureOptions::default()
        };
        let file = parse_v22_mesh_version(&resident_mesh_fixture(options), "6000.1.0f1");
        assert!(
            read_mesh(&file, 0, MeshReadLimits::default())
                .unwrap_err()
                .to_string()
                .contains("AssetCollection")
        );

        let mut resource = vec![0xaa; 7];
        resource.extend_from_slice(&resident_vertex_data(options));
        let collection = AssetCollection::from_loaded_parts(
            vec![LoadedSerializedFile {
                path: "mesh.assets".to_owned(),
                file,
            }],
            vec![LoadedResource {
                path: "bundle::MESH.RESS".to_owned(),
                region: Region::from_bytes(resource),
            }],
        );
        let loaded = &collection.serialized_files[0].file;
        let mesh =
            read_mesh_with_collection(&collection, loaded, 0, MeshReadLimits::default()).unwrap();
        assert_float_slice(&mesh.vertices[0], &[1.5, 0.0, 0.0]);
        assert_float_slice(&mesh.uv0.as_ref().unwrap()[2], &[0.0, 1.0]);

        let studio = Studio::from_collection(collection.clone());
        let obj = studio
            .object(0, 7)
            .unwrap()
            .read_mesh_obj(MeshReadLimits::default())
            .unwrap();
        assert!(obj.starts_with(b"g tri\r\nv -1.5 0 0\r\n"));

        let error = read_mesh_with_collection(
            &collection,
            loaded,
            0,
            MeshReadLimits {
                maximum_vertex_data_bytes: 119,
                ..MeshReadLimits::default()
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("exceeding limit 119"));

        let missing =
            AssetCollection::from_loaded_parts(collection.serialized_files.clone(), Vec::new());
        assert!(
            read_mesh_with_collection(
                &missing,
                &missing.serialized_files[0].file,
                0,
                MeshReadLimits::default(),
            )
            .unwrap_err()
            .to_string()
            .contains("external resource was not found")
        );
    }

    #[test]
    fn parses_legacy_and_vertex_channel_skinning_with_bind_poses_and_hashes() {
        for (unity_version, layout_version, skin_records) in [
            ("2017.4.40f1", (2017, 4, 40), 3),
            ("2022.3.62f1", (2022, 3, 62), 0),
        ] {
            let object = resident_mesh_fixture(MeshFixtureOptions {
                skin_records,
                skinning: true,
                layout_version,
                ..MeshFixtureOptions::default()
            });
            let file = parse_v22_mesh_version(&object, unity_version);
            let mesh = read_mesh(&file, 0, MeshReadLimits::default()).unwrap();

            assert_eq!(mesh.bind_poses.len(), 2);
            assert_eq!(mesh.bind_poses[0][0].to_bits(), 1.0_f32.to_bits());
            assert_eq!(mesh.bind_poses[1][12].to_bits(), 1.0_f32.to_bits());
            assert_eq!(mesh.bone_name_hashes, [111, 222]);
            assert_eq!(mesh.root_bone_name_hash, 333);
            let skin = mesh.skin.as_ref().unwrap();
            assert_eq!(skin.len(), 3);
            assert_float_slice(&skin[0].weights, &[1.0, 0.0, 0.0, 0.0]);
            assert_float_slice(&skin[1].weights, &[0.25, 0.75, 0.0, 0.0]);
            assert_eq!(skin[1].bone_indices, [0, 1, 0, 0]);
            assert_eq!(skin[2].bone_indices[0], 1);

            let limits = MeshReadLimits {
                maximum_bones: 1,
                ..MeshReadLimits::default()
            };
            assert!(read_mesh(&file, 0, limits).is_err());
        }

        let object = resident_mesh_fixture(MeshFixtureOptions {
            skin_records: 3,
            skinning: true,
            layout_version: (2017, 4, 40),
            ..MeshFixtureOptions::default()
        });
        let limits = MeshReadLimits {
            maximum_skin_weights: 2,
            ..MeshReadLimits::default()
        };
        assert!(read_mesh(&parse_v22_mesh_version(&object, "2017.4.40f1"), 0, limits).is_err());
    }

    #[test]
    fn enforces_legacy_mesh_version_boundaries_and_source_bounds() {
        let unsupported = resident_mesh_fixture(MeshFixtureOptions {
            layout_version: (2017, 2, 5),
            ..MeshFixtureOptions::default()
        });
        let error = read_mesh(
            &parse_v22_mesh_version(&unsupported, "2017.2.5f1"),
            0,
            MeshReadLimits::default(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("Unity 2017.3-2023"));

        for version in [
            "2022.3.1t1",
            "2023.1.0t1",
            "2024.1.0f1",
            "6000.2.0f1",
            "6000.3.0f1",
        ] {
            let file = parse_v22_mesh_version(&unsupported, version);
            assert!(
                read_mesh(&file, 0, MeshReadLimits::default()).is_err(),
                "{version}"
            );
        }

        let truncated = resident_mesh_fixture(MeshFixtureOptions {
            truncate_vertex_data: true,
            layout_version: (2017, 3, 0),
            ..MeshFixtureOptions::default()
        });
        let error = read_mesh(
            &parse_v22_mesh_version(&truncated, "2017.3.0f3"),
            0,
            MeshReadLimits::default(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("beyond vertex data size"));

        let streamed = resident_mesh_fixture(MeshFixtureOptions {
            stream_path: "mesh.resS",
            stream_size: 120,
            layout_version: (2018, 3, 14),
            ..MeshFixtureOptions::default()
        });
        let error = read_mesh(
            &parse_v22_mesh_version(&streamed, "2018.3.14f1"),
            0,
            MeshReadLimits::default(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("streamed Mesh"));

        assert_eq!(super::vertex_format_size(4, (2017, 4, 40)).unwrap(), 1);
        assert_eq!(super::vertex_format_size(4, (2019, 1, 0)).unwrap(), 2);
    }

    #[test]
    fn decodes_compressed_mesh_geometry_instead_of_refusing_it() {
        // Packed geometry used to fail the whole mesh read. The fixture packs a
        // three-vertex triangle whose 8-bit values decode to themselves.
        let object = resident_mesh_fixture(MeshFixtureOptions {
            compressed: true,
            ..MeshFixtureOptions::default()
        });
        let mesh = read_mesh(&parse_v22_mesh(&object), 0, MeshReadLimits::default()).unwrap();

        assert_eq!(mesh.vertices.len(), 3);
        assert!((mesh.vertices[0][0] - 1.0).abs() < 1e-4);
        assert!((mesh.vertices[1][1] - 2.0).abs() < 1e-4);
        assert!((mesh.vertices[2][2] - 3.0).abs() < 1e-4);

        // The octahedral pair (1, 0) restores a zero third component, and the
        // sign bit leaves it positive.
        let normals = mesh.normals.as_ref().expect("compressed normals");
        assert_eq!(normals.len(), 3);
        for normal in normals {
            let length =
                (normal[0] * normal[0] + normal[1] * normal[1] + normal[2] * normal[2]).sqrt();
            assert!((length - 1.0).abs() < 1e-3, "normal {normal:?} is not unit");
        }

        assert_eq!(mesh.sub_meshes[0].indices, [0, 1, 2]);
    }

    #[test]
    fn expands_strip_and_quad_topology_into_triangles() {
        // A three-index submesh as a strip is one triangle with the source
        // winding; the same run as a quad is not a whole quad, so nothing is
        // emitted. Refusing these outright used to fail the entire mesh read.
        let strip = resident_mesh_fixture(MeshFixtureOptions {
            topology: 1,
            ..MeshFixtureOptions::default()
        });
        let mesh = read_mesh(&parse_v22_mesh(&strip), 0, MeshReadLimits::default()).unwrap();
        assert_eq!(mesh.sub_meshes[0].indices, [0, 1, 2]);
        assert_eq!(mesh.sub_meshes[0].index_count, 3);

        let quads = resident_mesh_fixture(MeshFixtureOptions {
            topology: 2,
            ..MeshFixtureOptions::default()
        });
        let mesh = read_mesh(&parse_v22_mesh(&quads), 0, MeshReadLimits::default()).unwrap();
        assert!(mesh.sub_meshes[0].indices.is_empty());
        assert_eq!(mesh.sub_meshes[0].index_count, 0);
    }

    #[test]
    fn triangulation_drops_degenerates_and_flips_odd_strip_winding() {
        // Position 1 is odd, so its winding flips; a run containing a repeated
        // index contributes no triangle.
        let source = [0_u32, 1, 2, 3];
        let expanded = triangulate(&source, 1, (2022, 3, 62), MeshReadLimits::default()).unwrap();
        assert_eq!(expanded, [0, 1, 2, 2, 1, 3]);

        let degenerate = [0_u32, 0, 1, 2];
        let expanded =
            triangulate(&degenerate, 1, (2022, 3, 62), MeshReadLimits::default()).unwrap();
        assert_eq!(expanded, [1, 0, 2]);

        // Before Unity 4 every topology is a strip, whatever the field says.
        let legacy = triangulate(&source, 0, (3, 5, 0), MeshReadLimits::default()).unwrap();
        assert_eq!(legacy, [0, 1, 2, 2, 1, 3]);

        let quad = triangulate(&[0, 1, 2, 3], 2, (2022, 3, 62), MeshReadLimits::default()).unwrap();
        assert_eq!(quad, [0, 1, 2, 0, 2, 3]);

        let error = triangulate(&source, 4, (2022, 3, 62), MeshReadLimits::default()).unwrap_err();
        assert!(error.to_string().contains("lines and points"));
    }

    #[test]
    fn supports_32_bit_indices_and_rejects_unsupported_layouts() {
        let object = resident_mesh_fixture(MeshFixtureOptions {
            index_width: 4,
            ..MeshFixtureOptions::default()
        });
        let mesh = read_mesh(&parse_v22_mesh(&object), 0, MeshReadLimits::default()).unwrap();
        assert_eq!(mesh.sub_meshes[0].indices, [0, 1, 2]);

        let unity_2019 = resident_mesh_fixture(MeshFixtureOptions {
            layout_version: (2019, 4, 40),
            ..MeshFixtureOptions::default()
        });
        let mesh = read_mesh(
            &parse_v22_mesh_version(&unity_2019, "2019.4.40f1"),
            0,
            MeshReadLimits::default(),
        )
        .unwrap();
        assert_eq!(mesh.vertices.len(), 3);

        for (options, expected) in [
            (
                // Lines. There is no triangle to make from them.
                MeshFixtureOptions {
                    topology: 3,
                    ..MeshFixtureOptions::default()
                },
                "topology",
            ),
            (
                MeshFixtureOptions {
                    base_vertex: 1,
                    ..MeshFixtureOptions::default()
                },
                "base vertex",
            ),
            (
                // The flag claims compression but no packed vector carries data.
                MeshFixtureOptions {
                    mesh_compression: 1,
                    ..MeshFixtureOptions::default()
                },
                "carries no packed geometry",
            ),
            (
                MeshFixtureOptions {
                    stream_path: "mesh.resS",
                    stream_size: 144,
                    ..MeshFixtureOptions::default()
                },
                "streamed Mesh",
            ),
            (
                // Format 6 is UInt8 from 2019 on. Integer formats belong to
                // blend indices, never to positions.
                MeshFixtureOptions {
                    position_format: 6,
                    ..MeshFixtureOptions::default()
                },
                "position channel",
            ),
        ] {
            let error = read_mesh(
                &parse_v22_mesh(&resident_mesh_fixture(options)),
                0,
                MeshReadLimits::default(),
            )
            .unwrap_err();
            assert!(
                error.to_string().contains(expected),
                "expected {expected:?} in {error}"
            );
        }
    }

    #[test]
    fn decodes_float16_positions_like_the_managed_reader() {
        // Unity's Vertex Compression player setting is on by default in many
        // builds and stores positions, normals and texture coordinates as
        // Float16, so refusing anything but Float32 would reject a large share
        // of shipped meshes. The stream stride follows the declared format, so
        // this also covers the 6-byte-per-vertex position stream.
        let file = parse_v22_mesh(&resident_mesh_fixture(MeshFixtureOptions {
            position_format: 1,
            ..MeshFixtureOptions::default()
        }));
        let mesh = read_mesh(&file, 0, MeshReadLimits::default()).unwrap();

        assert_eq!(mesh.vertices.len(), FIXTURE_VERTICES.len());
        for (decoded, (expected, _, _)) in mesh.vertices.iter().zip(FIXTURE_VERTICES) {
            for (decoded, expected) in decoded.iter().zip(expected) {
                if expected.is_nan() {
                    assert!(decoded.is_nan());
                } else {
                    assert!(
                        (decoded - expected).abs() < f32::EPSILON,
                        "expected {expected}, decoded {decoded}"
                    );
                }
            }
        }
        // The Float32 normal and UV streams still decode from their own
        // strides, which shifted because stream 0 is now half the size.
        assert_eq!(
            mesh.normals.as_deref(),
            Some([[1.0, 0.0, 0.0]; 3].as_slice())
        );
        assert_eq!(
            mesh.uv0.as_deref(),
            Some([[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]].as_slice())
        );
    }

    #[test]
    fn enforces_counts_vertex_ranges_and_output_limit() {
        let file = parse_v22_mesh(&resident_mesh_fixture(MeshFixtureOptions::default()));
        let error = read_mesh(
            &file,
            0,
            MeshReadLimits {
                maximum_vertices: 2,
                ..MeshReadLimits::default()
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("vertices"));

        let error = read_mesh(
            &file,
            0,
            MeshReadLimits {
                maximum_indices: 2,
                ..MeshReadLimits::default()
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("indices"));

        let mut bad_index = resident_mesh_fixture(MeshFixtureOptions::default());
        let marker = [0_u8, 0, 1, 0, 2, 0];
        let index_start = bad_index
            .windows(marker.len())
            .position(|window| window == marker)
            .unwrap();
        bad_index[index_start + 4..index_start + 6].copy_from_slice(&3_u16.to_le_bytes());
        let error =
            read_mesh(&parse_v22_mesh(&bad_index), 0, MeshReadLimits::default()).unwrap_err();
        assert!(error.to_string().contains("exceeds vertex count"));

        let mesh = simple_mesh();
        let mut output = Vec::new();
        let error = write_mesh_obj(&mesh, &mut output, 8).unwrap_err();
        assert!(error.to_string().contains("output limit"));
    }

    #[test]
    fn validates_direct_obj_inputs_without_partial_indexing_panics() {
        let mut mesh = simple_mesh();
        mesh.normals = Some(vec![[0.0; 3]]);
        assert!(write_mesh_obj(&mesh, &mut Vec::new(), 1024).is_err());

        let mut mesh = simple_mesh();
        mesh.sub_meshes[0].indices = vec![0, 1];
        assert!(write_mesh_obj(&mesh, &mut Vec::new(), 1024).is_err());

        let mut mesh = simple_mesh();
        mesh.sub_meshes[0].indices = vec![0, 1, 3];
        assert!(write_mesh_obj(&mesh, &mut Vec::new(), 1024).is_err());
    }

    fn simple_mesh() -> Mesh {
        Mesh {
            path_id: 1,
            name: "tri".to_owned(),
            vertices: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            normals: None,
            uv0: None,
            bind_poses: Vec::new(),
            bone_name_hashes: Vec::new(),
            root_bone_name_hash: 0,
            skin: None,
            blend_shapes: None,
            sub_meshes: vec![MeshSubMesh {
                first_byte: 0,
                index_count: 3,
                first_vertex: 0,
                vertex_count: 3,
                indices: vec![0, 1, 2],
            }],
        }
    }

    fn assert_float_slice(actual: &[f32], expected: &[f32]) {
        assert_eq!(actual.len(), expected.len());
        for (actual, expected) in actual.iter().zip(expected) {
            assert!((actual - expected).abs() <= f32::EPSILON);
        }
    }

    #[derive(Debug, Clone, Copy)]
    struct TuanjieMeshFixture {
        revision: u8,
        has_tail_flags: bool,
        virtual_geometry: bool,
        root_cluster_bytes: usize,
    }

    #[derive(Debug, Clone, Copy)]
    #[allow(clippy::struct_excessive_bools)]
    struct MeshFixtureOptions {
        index_width: usize,
        topology: i32,
        base_vertex: u32,
        mesh_compression: u8,
        position_format: u8,
        stream_path: &'static str,
        stream_offset: u64,
        stream_size: u32,
        packed_vertex_items: u32,
        compressed: bool,
        skin_records: usize,
        skinning: bool,
        blend_shapes: bool,
        truncate_vertex_data: bool,
        layout_version: (u32, u32, u32),
        tuanjie: Option<TuanjieMeshFixture>,
    }

    impl Default for MeshFixtureOptions {
        fn default() -> Self {
            Self {
                index_width: 2,
                topology: 0,
                base_vertex: 0,
                mesh_compression: 0,
                position_format: 0,
                stream_path: "",
                stream_offset: 0,
                stream_size: 0,
                packed_vertex_items: 0,
                compressed: false,
                skin_records: 0,
                skinning: false,
                blend_shapes: false,
                truncate_vertex_data: false,
                layout_version: (2022, 3, 62),
                tuanjie: None,
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn resident_mesh_fixture(options: MeshFixtureOptions) -> Vec<u8> {
        let mut object = Vec::new();
        push_aligned_string(&mut object, "tri");
        push_i32(&mut object, 1);
        push_u32(&mut object, 0);
        push_u32(&mut object, 3);
        push_i32(&mut object, options.topology);
        push_u32(&mut object, options.base_vertex);
        push_u32(&mut object, 0);
        push_u32(&mut object, 3);
        object.extend_from_slice(&[0_u8; 24]);

        push_blend_shapes(&mut object, options.blend_shapes);
        let bone_count = usize::from(options.skinning) * 2;
        push_i32(&mut object, i32::try_from(bone_count).unwrap());
        for bone in 0..bone_count {
            for index in 0..16 {
                let value = if index % 5 == 0 {
                    1.0
                } else if index == 12 {
                    f32::from(u16::try_from(bone).unwrap())
                } else {
                    0.0
                };
                object.extend_from_slice(&value.to_le_bytes());
            }
        }
        push_i32(&mut object, i32::try_from(bone_count).unwrap());
        for hash in [111_u32, 222].into_iter().take(bone_count) {
            push_u32(&mut object, hash);
        }
        push_u32(&mut object, if options.skinning { 333 } else { 0 });
        if options.layout_version.0 >= 2019 {
            push_i32(&mut object, 0);
            push_i32(&mut object, 0);
        }

        object.push(if options.compressed {
            1
        } else {
            options.mesh_compression
        });
        object.extend_from_slice(&[1, 0, 0]);
        if let Some(tuanjie) = options.tuanjie {
            if tuanjie.revision == 3 {
                align(&mut object, 4);
            }
            push_tuanjie_shared_cluster(&mut object, tuanjie.revision, tuanjie.root_cluster_bytes);
            align(&mut object, 4);
        } else {
            align(&mut object, 4);
        }
        push_i32(&mut object, i32::from(options.index_width == 4));
        push_i32(&mut object, i32::try_from(3 * options.index_width).unwrap());
        for index in 0..3_u32 {
            if options.index_width == 2 {
                object.extend_from_slice(&u16::try_from(index).unwrap().to_le_bytes());
            } else {
                push_u32(&mut object, index);
            }
        }
        if options.index_width == 2 {
            align(&mut object, 4);
        }

        if options.layout_version < (2018, 2, 0) {
            push_i32(&mut object, i32::try_from(options.skin_records).unwrap());
            for vertex in 0..options.skin_records {
                let weights: [f32; 4] = if options.skinning {
                    match vertex {
                        0 => [1.0, 0.0, 0.0, 0.0],
                        1 => [0.25, 0.75, 0.0, 0.0],
                        _ => [0.0, 1.0, 0.0, 0.0],
                    }
                } else {
                    [0.0; 4]
                };
                for weight in weights {
                    object.extend_from_slice(&weight.to_le_bytes());
                }
                let indices = if options.skinning {
                    match vertex {
                        0 => [0, 0, 0, 0],
                        1 => [0, 1, 0, 0],
                        _ => [1, 0, 0, 0],
                    }
                } else {
                    [0; 4]
                };
                for index in indices {
                    push_i32(&mut object, index);
                }
            }
        }
        if options.layout_version.0 < 2018 {
            push_u32(&mut object, 0b1011);
        }
        push_u32(&mut object, 3);
        let channel_count = if options.skinning && options.layout_version >= (2018, 2, 0) {
            14
        } else if options.layout_version.0 < 2018 {
            4
        } else {
            5
        };
        push_i32(&mut object, channel_count);
        object.extend_from_slice(&[0, 0, options.position_format, 3]);
        object.extend_from_slice(&[1, 0, 0, 3]);
        object.extend_from_slice(&[0, 0, 0, 0]);
        if options.layout_version.0 >= 2018 {
            object.extend_from_slice(&[0, 0, 0, 0]);
        }
        object.extend_from_slice(&[2, 0, 0, 2]);
        if options.skinning && options.layout_version >= (2018, 2, 0) {
            for _ in 5..12 {
                object.extend_from_slice(&[0, 0, 0, 0]);
            }
            object.extend_from_slice(&[3, 0, 0, 4]);
            let index_format = if options.layout_version.0 < 2019 {
                7
            } else {
                6
            };
            object.extend_from_slice(&[4, 0, index_format, 4]);
        }
        let mut vertex_data = resident_vertex_data(options);
        if options.truncate_vertex_data {
            vertex_data.truncate(4);
        }
        push_i32(&mut object, i32::try_from(vertex_data.len()).unwrap());
        object.extend_from_slice(&vertex_data);
        align(&mut object, 4);

        if options.compressed {
            // A three-vertex triangle. Range 255 with an 8-bit width makes each
            // packed value decode to itself, so the expected geometry is plain.
            push_packed_float_data(&mut object, 9, 255.0, 0.0, &[1, 0, 0, 0, 2, 0, 0, 0, 3], 8);
            push_empty_packed_float(&mut object); // UVs
            // Normals as octahedral pairs plus their sign bits.
            push_packed_float_data(
                &mut object,
                6,
                2.0,
                -1.0,
                &[255, 128, 255, 128, 255, 128],
                8,
            );
            push_empty_packed_float(&mut object); // tangents
            push_empty_packed_int(&mut object); // weights
            push_packed_int_data(&mut object, &[1, 1, 1], 1); // normal signs
            push_empty_packed_int(&mut object); // tangent signs
            push_empty_packed_float(&mut object); // float colours
            push_empty_packed_int(&mut object); // bone indices
            push_packed_int_data(&mut object, &[0, 1, 2], 8); // triangles
            push_u32(&mut object, 0); // UV info
        } else {
            push_packed_float(&mut object, options.packed_vertex_items);
            for _ in 0..3 {
                push_empty_packed_float(&mut object);
            }
            for _ in 0..3 {
                push_empty_packed_int(&mut object);
            }
            push_empty_packed_float(&mut object);
            for _ in 0..2 {
                push_empty_packed_int(&mut object);
            }
            push_u32(&mut object, 0);
        }

        object.extend_from_slice(&[0_u8; 24]);
        push_i32(&mut object, 0);
        if options.layout_version >= (2022, 1, 0) {
            push_i32(&mut object, 0);
        }
        push_i32(&mut object, 0);
        push_i32(&mut object, 0);
        if options.layout_version >= (2018, 2, 0) {
            object.extend_from_slice(&[0_u8; 8]);
        }
        if options.layout_version >= (2018, 3, 0) {
            align(&mut object, 4);
            if options.layout_version >= (2020, 1, 0) {
                object.extend_from_slice(
                    &i64::try_from(options.stream_offset).unwrap().to_le_bytes(),
                );
            } else {
                push_u32(&mut object, u32::try_from(options.stream_offset).unwrap());
            }
            push_u32(&mut object, options.stream_size);
            push_aligned_string(&mut object, options.stream_path);
        }
        if let Some(tuanjie) = options.tuanjie.filter(|value| value.has_tail_flags) {
            object.push(1);
            object.push(u8::from(tuanjie.virtual_geometry));
        }
        object
    }

    fn push_tuanjie_shared_cluster(output: &mut Vec<u8>, revision: u8, root_page_bytes: usize) {
        assert!((1..=3).contains(&revision));
        push_i32(output, 0);
        output.extend_from_slice(&1.0_f32.to_le_bytes());
        if revision == 1 {
            output.extend_from_slice(&[0_u8; 16]);
        }
        push_i32(output, i32::try_from(root_page_bytes).unwrap());
        output.resize(output.len() + root_page_bytes, 0xa5);
        if revision == 1 {
            push_i32(output, 0);
        }
        push_i32(output, 0);
        if revision == 1 {
            push_i32(output, 0);
        }
        push_i32(output, 0);
        push_i32(output, 0);
        if revision == 2 {
            output.extend_from_slice(&[0_u8; 16]);
            push_i32(output, 0);
            push_i32(output, 0);
        }
        push_i32(output, 0);
    }

    const FIXTURE_VERTICES: [([f32; 3], [f32; 2], [f32; 3]); 3] = [
        ([1.5, 0.0, 0.0], [0.0, 0.0], [1.0, 0.0, 0.0]),
        ([f32::NAN, 2.0, 0.0], [1.0, 0.0], [1.0, 0.0, 0.0]),
        ([0.0, 0.0, 3.0], [0.0, 1.0], [1.0, 0.0, 0.0]),
    ];

    /// Encodes one component the way the declared channel format stores it.
    ///
    /// The reader derives each stream's stride from the channel formats, so a
    /// fixture that always wrote Float32 could never exercise anything else.
    fn push_float_component(output: &mut Vec<u8>, value: f32, format: u8) {
        match format {
            0 => output.extend_from_slice(&value.to_le_bytes()),
            1 => output.extend_from_slice(&exact_half_bits(value).to_le_bytes()),
            // The 2019+ single-byte integer formats appear only in rejection
            // cases: the reader refuses the channel before reading any of these
            // bytes, so only the width has to be right.
            6 | 7 => output.push(0),
            other => panic!("fixture cannot encode vertex format {other}"),
        }
    }

    /// Converts a fixture value to Float16, asserting it survives exactly.
    fn exact_half_bits(value: f32) -> u16 {
        if value.is_nan() {
            return 0x7e00;
        }
        let bits = value.to_bits();
        let sign = u16::try_from((bits >> 16) & 0x8000).unwrap();
        if value == 0.0 {
            return sign;
        }
        let exponent = i32::try_from((bits >> 23) & 0xff).unwrap() - 127;
        let mantissa = bits & 0x007f_ffff;
        assert!(
            (-14..=15).contains(&exponent),
            "fixture value {value} is outside the Float16 range"
        );
        assert_eq!(
            mantissa & 0x1fff,
            0,
            "fixture value {value} is not exactly representable as Float16"
        );
        sign | (u16::try_from(exponent + 15).unwrap() << 10)
            | u16::try_from(mantissa >> 13).unwrap()
    }

    /// Pads to the stream alignment the reader assumes between streams.
    fn pad_vertex_stream(vertex_data: &mut Vec<u8>) {
        let padded = vertex_data.len().next_multiple_of(STREAM_ALIGNMENT);
        vertex_data.resize(padded, 0);
    }

    fn resident_vertex_data(options: MeshFixtureOptions) -> Vec<u8> {
        let mut vertex_data = Vec::new();
        let vertices = FIXTURE_VERTICES;
        for (position, _, _) in vertices {
            for value in position {
                push_float_component(&mut vertex_data, value, options.position_format);
            }
        }
        pad_vertex_stream(&mut vertex_data);
        for (_, _, normal) in vertices {
            for value in normal {
                vertex_data.extend_from_slice(&value.to_le_bytes());
            }
        }
        pad_vertex_stream(&mut vertex_data);
        for (_, uv, _) in vertices {
            for value in uv {
                vertex_data.extend_from_slice(&value.to_le_bytes());
            }
        }
        if options.skinning && options.layout_version >= (2018, 2, 0) {
            pad_vertex_stream(&mut vertex_data);
            for weights in [
                [1.0_f32, 0.0, 0.0, 0.0],
                [0.25, 0.75, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
            ] {
                for weight in weights {
                    vertex_data.extend_from_slice(&weight.to_le_bytes());
                }
            }
            for indices in [[0_u8, 0, 0, 0], [0, 1, 0, 0], [1, 0, 0, 0]] {
                vertex_data.extend_from_slice(&indices);
            }
        }
        vertex_data
    }

    fn push_blend_shapes(output: &mut Vec<u8>, enabled: bool) {
        if !enabled {
            for _ in 0..4 {
                push_i32(output, 0);
            }
            return;
        }
        push_i32(output, 1);
        for value in [
            0.5_f32, 0.0, 0.0, // vertex delta
            0.0, 0.25, 0.0, // normal delta
            0.0, 0.0, 0.0, // tangent delta
        ] {
            output.extend_from_slice(&value.to_le_bytes());
        }
        push_u32(output, 1);
        push_i32(output, 1);
        push_u32(output, 0);
        push_u32(output, 1);
        output.extend_from_slice(&[1, 0]);
        align(output, 4);
        push_i32(output, 1);
        push_aligned_string(output, "face.Smile");
        push_u32(output, 0x1234_5678);
        push_i32(output, 0);
        push_i32(output, 1);
        push_i32(output, 1);
        output.extend_from_slice(&100.0_f32.to_le_bytes());
    }

    fn push_empty_packed_float(output: &mut Vec<u8>) {
        push_packed_float(output, 0);
    }

    fn push_packed_float(output: &mut Vec<u8>, item_count: u32) {
        push_packed_float_data(output, item_count, 0.0, 0.0, &[], 0);
    }

    /// Writes a `PackedFloatVector` carrying `values` at `bit_size` bits each.
    fn push_packed_float_data(
        output: &mut Vec<u8>,
        item_count: u32,
        range: f32,
        start: f32,
        values: &[u32],
        bit_size: u8,
    ) {
        push_u32(output, item_count);
        output.extend_from_slice(&range.to_le_bytes());
        output.extend_from_slice(&start.to_le_bytes());
        let data = pack_bits(values, bit_size);
        push_i32(output, i32::try_from(data.len()).unwrap());
        output.extend_from_slice(&data);
        align(output, 4);
        output.push(bit_size);
        align(output, 4);
    }

    /// Writes a `PackedIntVector` carrying `values` at `bit_size` bits each.
    fn push_packed_int_data(output: &mut Vec<u8>, values: &[u32], bit_size: u8) {
        push_u32(output, u32::try_from(values.len()).unwrap());
        let data = pack_bits(values, bit_size);
        push_i32(output, i32::try_from(data.len()).unwrap());
        output.extend_from_slice(&data);
        align(output, 4);
        output.push(bit_size);
        align(output, 4);
    }

    fn pack_bits(values: &[u32], bit_size: u8) -> Vec<u8> {
        if bit_size == 0 {
            return Vec::new();
        }
        let mut bits = Vec::new();
        for value in values {
            for bit in 0..bit_size {
                bits.push((value >> bit) & 1 == 1);
            }
        }
        let mut bytes = vec![0_u8; bits.len().div_ceil(8)];
        for (position, set) in bits.iter().enumerate() {
            if *set {
                bytes[position / 8] |= 1 << (position % 8);
            }
        }
        bytes
    }

    fn push_empty_packed_int(output: &mut Vec<u8>) {
        push_u32(output, 0);
        push_i32(output, 0);
        align(output, 4);
        output.push(0);
        align(output, 4);
    }

    fn parse_v22_mesh(object: &[u8]) -> crate::serialized::SerializedFile {
        parse_v22_mesh_version(object, "2022.3.62f1")
    }

    fn parse_v22_mesh_version(
        object: &[u8],
        unity_version: &str,
    ) -> crate::serialized::SerializedFile {
        crate::serialized::SerializedFile::open(Region::from_bytes(synthetic_v22_mesh(
            object,
            unity_version,
        )))
        .unwrap()
    }

    fn synthetic_v22_mesh(object: &[u8], unity_version: &str) -> Vec<u8> {
        let mut metadata = Vec::new();
        metadata.extend_from_slice(unity_version.as_bytes());
        metadata.push(0);
        push_i32(&mut metadata, 13);
        metadata.push(0);
        push_i32(&mut metadata, 1);
        push_i32(&mut metadata, MESH_CLASS_ID);
        metadata.push(0);
        metadata.extend_from_slice(&(-1_i16).to_le_bytes());
        metadata.extend_from_slice(&[0_u8; 16]);
        push_i32(&mut metadata, 1);
        align_with_base(&mut metadata, 48, 4);
        metadata.extend_from_slice(&7_i64.to_le_bytes());
        metadata.extend_from_slice(&0_i64.to_le_bytes());
        metadata.extend_from_slice(&u32::try_from(object.len()).unwrap().to_le_bytes());
        push_i32(&mut metadata, 0);
        for _ in 0..3 {
            push_i32(&mut metadata, 0);
        }
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

    fn push_u32(output: &mut Vec<u8>, value: u32) {
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
