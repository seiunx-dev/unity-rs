//! Python bindings for the high-level Rust [`unity_rs_core::studio::Studio`] API.

use std::fmt::Write as _;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use pyo3::exceptions::{
    PyKeyError, PyMemoryError, PyNotImplementedError, PyTypeError, PyValueError,
};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyList, PyString, PyTuple};
use unity_rs_core::Error;
use unity_rs_core::acl::{
    AclCompressedTracksLimits, AclDecodeLimits, AclDecodeRequest, AclDecodedClip, AclDecoder,
    AclDecoderInputLimits,
};
use unity_rs_core::animation_clip::AnimationClipReadLimits;
use unity_rs_core::animation_component::{
    AnimationClipOverride, AnimationComponentReadLimits, AnimatorOverrideController,
    LegacyAnimationComponent,
};
use unity_rs_core::animator_controller::{AnimatorController, AnimatorControllerReadLimits};
use unity_rs_core::avatar::{Avatar, AvatarReadLimits};
use unity_rs_core::bundle::OodleDecoder;
use unity_rs_core::compression::CompressionLimits;
use unity_rs_core::export::{AudioExportFormat, ExportMode, ExportOptions, ExportReport};
use unity_rs_core::extraction::{ExtractionLimits, ExtractionOptions, ExtractionReport};
use unity_rs_core::image_export::ImageFormat;
use unity_rs_core::live2d_clip_motion::CubismClipMotionReadLimits;
use unity_rs_core::live2d_motion::{
    CubismFadeMotion, CubismFadeMotionReadLimits, CubismMotionTargetIndexLimits,
    CubismMotionTargetNames,
};
use unity_rs_core::live2d_package::{
    Live2dPackageBytes, Live2dPackageBytesSet, Live2dPackageLimits, Live2dPackageMaterializeLimits,
};
use unity_rs_core::live2d_physics::{CubismPhysicsReadLimits, CubismPhysicsRig};
use unity_rs_core::live2d_schema::{
    CubismAuxiliaryReadLimits, CubismExpression, CubismExpressionBlend, CubismExpressionReadLimits,
};
use unity_rs_core::loader::{AssetLoadLimits, AssetLoadOptions, LoadDiagnostic, LoadFailurePolicy};
use unity_rs_core::material::{Material, MaterialReadLimits, NamedMaterialProperty};
use unity_rs_core::mesh::MeshReadLimits;
use unity_rs_core::model_export::{ModelExportCandidate, ModelExportPlanLimits};
use unity_rs_core::mono_schema::{
    MonoBehaviourSchemaEntry, MonoBehaviourSchemaProvider, MonoBehaviourSchemaRegistry,
    MonoBehaviourSchemaRegistrySet, MonoBehaviourSchemaSource,
};
use unity_rs_core::monobehaviour::{MonoBehaviourReadLimits, MonoScript};
use unity_rs_core::project_settings::ProjectSettingsReadLimits;
use unity_rs_core::scene_hierarchy::{SceneHierarchyLimits, SceneHierarchyNode, SceneObjectKey};
use unity_rs_core::scene_textures::{SceneTexture, SceneTextureLimits, SceneTextureSkip};
use unity_rs_core::serialized::{
    AssetBundleMetadata, ContainerMetadataReadLimits, PreloadDataMetadata, ResourceManagerMetadata,
    TypeTree, TypeTreeNode,
};
use unity_rs_core::simple_assets::{
    AudioClipAsset, SimpleAssetReadLimits, SimpleBinaryAsset, direct_wav_output_size,
    write_direct_wav,
};
use unity_rs_core::source::Region;
use unity_rs_core::sprite::{Sprite, SpriteMeshType, SpritePackingMode, SpriteReadLimits};
use unity_rs_core::sprite_atlas::{SpriteAtlas, SpriteAtlasReadLimits};
use unity_rs_core::studio::{Studio, StudioFile, StudioObject, StudioResource};
use unity_rs_core::texture::TextureReadLimits;
use unity_rs_core::texture_array::TextureArrayReadLimits;
use unity_rs_core::unity_cn::UnityCnKey;
use unity_rs_core::unity_version::UnityVersion;

const MAXIMUM_SCHEMA_NODES: usize = 1_000_000;
const MAXIMUM_SCHEMA_ENTRIES: usize = 100_000;
const MAXIMUM_SCHEMA_STRING_BYTES: usize = 256 * 1024 * 1024;
const MAXIMUM_METADATA_PAGE_ITEMS: usize = 1_000_000;

type PyObjectReference = (i32, i64);
type PyAnimationClipOverride = (PyObjectReference, PyObjectReference);
type PyAssetBundleContainerEntry = (String, usize, usize, PyObjectReference);
type PyResourceManagerContainerEntry = (String, PyObjectReference);
type PySpriteTriangle = ((f32, f32), (f32, f32), (f32, f32));
type PyMonoBehaviourSchemaNode = (String, String, u32, bool);

struct PythonOodleDecoder {
    callback: Py<PyAny>,
}

struct PythonAclDecoder {
    callback: Py<PyAny>,
    limits: AclDecodeLimits,
}

impl AclDecoder for PythonAclDecoder {
    fn decode(&self, request: &AclDecodeRequest<'_>) -> unity_rs_core::Result<AclDecodedClip> {
        Python::attach(|py| {
            let compressed_tracks = python_callback_bytes(py, &request.input.compressed_tracks)?;
            let mut decoder_map = Vec::new();
            decoder_map
                .try_reserve_exact(request.input.decoder_map.len())
                .map_err(|error| python_allocation_error("ACL decoder map", error))?;
            decoder_map.extend_from_slice(&request.input.decoder_map);
            let result = self
                .callback
                .call1(
                    py,
                    (
                        compressed_tracks,
                        decoder_map,
                        request.frame_count,
                        request.bone_count,
                        request.sample_rate(),
                        request.declared_curve_count,
                        request.use_fast_sample_mode,
                    ),
                )
                .map_err(|error| {
                    Error::invalid_data(format!("Python ACL decoder raised an error: {error}"))
                })?;
            extract_python_acl_output(result.bind(py), request, self.limits).map_err(|error| {
                Error::invalid_data(format!(
                    "Python ACL decoder must return (times, binding_indices, values, following_curve_offset): {error}"
                ))
            })
        })
    }
}

fn extract_python_acl_output(
    result: &Bound<'_, PyAny>,
    request: &AclDecodeRequest<'_>,
    limits: AclDecodeLimits,
) -> PyResult<AclDecodedClip> {
    let tuple = result.cast::<PyTuple>()?;
    if tuple.len() != 4 {
        return Err(PyTypeError::new_err(
            "ACL decoder output tuple must contain four values",
        ));
    }
    let times = tuple.get_item(0)?.cast_into::<PyList>()?;
    let binding_indices = tuple.get_item(1)?.cast_into::<PyList>()?;
    let values = tuple.get_item(2)?.cast_into::<PyList>()?;
    let following_curve_offset = tuple.get_item(3)?.extract::<u32>()?;

    let expected_frames = usize::try_from(request.frame_count)
        .map_err(|_| PyValueError::new_err("ACL frame count does not fit this platform"))?;
    if expected_frames > limits.maximum_frames {
        return Err(PyValueError::new_err(format!(
            "ACL frame count {expected_frames} exceeds limit {}",
            limits.maximum_frames
        )));
    }
    if times.len() != expected_frames {
        return Err(PyValueError::new_err(format!(
            "ACL decoder returned {} times for {expected_frames} declared frames",
            times.len()
        )));
    }
    let curve_count = binding_indices.len();
    if curve_count > limits.maximum_curves {
        return Err(PyValueError::new_err(format!(
            "ACL decoder returned {curve_count} curves, exceeding limit {}",
            limits.maximum_curves
        )));
    }
    if let Some(declared) = request.declared_curve_count {
        let declared = usize::try_from(declared)
            .map_err(|_| PyValueError::new_err("ACL curve count does not fit this platform"))?;
        if curve_count != declared {
            return Err(PyValueError::new_err(format!(
                "ACL decoder returned {curve_count} curves for {declared} declared curves"
            )));
        }
    }
    let expected_values = expected_frames
        .checked_mul(curve_count)
        .ok_or_else(|| PyValueError::new_err("ACL decoded value count overflowed"))?;
    if expected_values > limits.maximum_values {
        return Err(PyValueError::new_err(format!(
            "ACL decoder output requires {expected_values} values, exceeding limit {}",
            limits.maximum_values
        )));
    }
    if values.len() != expected_values {
        return Err(PyValueError::new_err(format!(
            "ACL decoder returned {} values; {expected_values} are required",
            values.len()
        )));
    }

    Ok(AclDecodedClip {
        times: copy_python_f32_list(&times, "ACL decoder times")?,
        binding_indices: copy_python_u32_list(&binding_indices, "ACL decoder binding indices")?,
        values: copy_python_f32_list(&values, "ACL decoder values")?,
        following_curve_offset,
    })
}

fn copy_python_f32_list(values: &Bound<'_, PyList>, field: &'static str) -> PyResult<Vec<f32>> {
    let mut copied = reserve_metadata(values.len(), field)?;
    for value in values.iter() {
        copied.push(value.extract()?);
    }
    Ok(copied)
}

fn copy_python_u32_list(values: &Bound<'_, PyList>, field: &'static str) -> PyResult<Vec<u32>> {
    let mut copied = reserve_metadata(values.len(), field)?;
    for value in values.iter() {
        copied.push(value.extract()?);
    }
    Ok(copied)
}

impl OodleDecoder for PythonOodleDecoder {
    fn decompress(&self, input: &[u8], output: &mut [u8]) -> unity_rs_core::Result<usize> {
        Python::attach(|py| {
            let input = python_callback_bytes(py, input)?;
            let result = self
                .callback
                .call1(py, (input, output.len()))
                .map_err(|error| {
                    Error::invalid_data(format!("Python Oodle decoder raised an error: {error}"))
                })?;
            let bytes = result
                .bind(py)
                .cast::<PyBytes>()
                .map_err(|_| Error::invalid_data("Python Oodle decoder must return bytes"))?;
            let decoded = bytes.as_bytes();
            if decoded.len() != output.len() {
                return Err(Error::invalid_data(format!(
                    "Python Oodle decoder returned {} bytes for expected output length {}",
                    decoded.len(),
                    output.len()
                )));
            }
            output.copy_from_slice(decoded);
            Ok(output.len())
        })
    }
}

#[pyclass(name = "FileInfo", frozen, get_all)]
#[derive(Debug)]
struct PyFileInfo {
    index: usize,
    path: String,
    unity_version: String,
    object_count: usize,
}

#[pyclass(name = "ObjectInfo", frozen, get_all)]
#[derive(Debug)]
struct PyObjectInfo {
    file_index: usize,
    object_index: usize,
    source_path: String,
    path_id: i64,
    class_id: i32,
    byte_size: u64,
    name: Option<String>,
    container: Option<String>,
}

#[pyclass(name = "ResourceInfo", frozen, get_all)]
#[derive(Debug)]
struct PyResourceInfo {
    index: usize,
    path: String,
    byte_size: u64,
}

#[pyclass(name = "LoadDiagnostic", frozen, get_all)]
#[derive(Debug)]
struct PyLoadDiagnostic {
    path: String,
    message: String,
}

/// Bounded metadata for one parsed Unity or Tuanjie `AnimationClip`.
#[pyclass(name = "AnimationClip", frozen)]
#[derive(Debug)]
struct PyAnimationClip {
    #[pyo3(get)]
    path_id: i64,
    #[pyo3(get)]
    name: String,
    state_bits: u8,
    #[pyo3(get)]
    sample_rate: f32,
    #[pyo3(get)]
    wrap_mode: i32,
    #[pyo3(get)]
    rotation_curve_count: usize,
    #[pyo3(get)]
    euler_curve_count: usize,
    #[pyo3(get)]
    position_curve_count: usize,
    #[pyo3(get)]
    scale_curve_count: usize,
    #[pyo3(get)]
    float_curve_count: usize,
    #[pyo3(get)]
    pptr_curve_count: usize,
    #[pyo3(get)]
    muscle_clip_size: u32,
    #[pyo3(get)]
    streamed_curve_count: Option<u32>,
    #[pyo3(get)]
    dense_curve_count: Option<u32>,
    #[pyo3(get)]
    constant_value_count: Option<usize>,
    #[pyo3(get)]
    acl_frame_count: Option<u32>,
    #[pyo3(get)]
    acl_bone_count: Option<u32>,
    #[pyo3(get)]
    acl_sample_rate: Option<f32>,
    #[pyo3(get)]
    acl_curve_count: Option<u32>,
    #[pyo3(get)]
    acl_track_byte_count: Option<u64>,
    #[pyo3(get)]
    acl_decoder_count: Option<usize>,
    #[pyo3(get)]
    acl_use_fast_sample_mode: Option<bool>,
    #[pyo3(get)]
    streaming_offset: Option<i64>,
    #[pyo3(get)]
    streaming_size: Option<u32>,
    #[pyo3(get)]
    streaming_path: Option<String>,
}

/// Stable references from one legacy Unity `Animation` component.
#[pyclass(name = "LegacyAnimation", frozen, get_all)]
#[derive(Debug)]
struct PyLegacyAnimation {
    path_id: i64,
    game_object: PyObjectReference,
    enabled: u8,
    default_clip: PyObjectReference,
    clips: Vec<PyObjectReference>,
    trailing_bytes: u64,
}

/// Stable controller and clip substitutions from `AnimatorOverrideController`.
#[pyclass(name = "AnimatorOverrideController", frozen, get_all)]
#[derive(Debug)]
struct PyAnimatorOverrideController {
    path_id: i64,
    name: String,
    controller: PyObjectReference,
    clip_overrides: Vec<PyAnimationClipOverride>,
    trailing_bytes: u64,
}

/// Bounded preload and named-container metadata from one Unity `AssetBundle`.
#[pyclass(name = "AssetBundle", frozen, get_all)]
#[derive(Debug)]
struct PyAssetBundle {
    path_id: i64,
    name: String,
    object_name: String,
    asset_bundle_name: Option<String>,
    preload_table: Vec<PyObjectReference>,
    container: Vec<PyAssetBundleContainerEntry>,
    dependencies: Vec<String>,
    is_streamed_scene_asset_bundle: bool,
}

/// Bounded named-container metadata from one Unity `ResourceManager`.
#[pyclass(name = "ResourceManager", frozen, get_all)]
#[derive(Debug)]
struct PyResourceManager {
    path_id: i64,
    container: Vec<PyResourceManagerContainerEntry>,
}

/// Bounded object-reference metadata from one Unity `PreloadData`.
#[pyclass(name = "PreloadData", frozen, get_all)]
#[derive(Debug)]
struct PyPreloadData {
    path_id: i64,
    name: String,
    assets: Vec<PyObjectReference>,
}

/// Serialized composite key used by a `SpriteAtlas` render-data entry.
#[pyclass(name = "SpriteAtlasRenderDataKey", frozen)]
#[derive(Debug)]
struct PySpriteAtlasRenderDataKey {
    guid_bytes: [u8; 16],
    value: i64,
}

#[pymethods]
impl PySpriteAtlasRenderDataKey {
    /// Returns the GUID in Unity's original serialized byte order.
    #[getter]
    fn guid_bytes<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        python_bytes(py, &self.guid_bytes)
    }

    #[getter]
    const fn value(&self) -> i64 {
        self.value
    }
}

/// One optional secondary texture named by a `SpriteAtlas` entry.
#[pyclass(name = "SpriteAtlasSecondaryTexture", frozen, get_all)]
#[derive(Debug)]
struct PySpriteAtlasSecondaryTexture {
    texture: PyObjectReference,
    name: String,
}

/// Complete crop, texture and packing metadata for one atlas key.
#[pyclass(name = "SpriteAtlasRenderData", frozen)]
#[derive(Debug)]
struct PySpriteAtlasRenderData {
    key: Py<PySpriteAtlasRenderDataKey>,
    #[pyo3(get)]
    texture: PyObjectReference,
    #[pyo3(get)]
    alpha_texture: PyObjectReference,
    #[pyo3(get)]
    texture_rect: (f32, f32, f32, f32),
    #[pyo3(get)]
    texture_rect_offset: (f32, f32),
    #[pyo3(get)]
    atlas_rect_offset: (f32, f32),
    #[pyo3(get)]
    uv_transform: (f32, f32, f32, f32),
    #[pyo3(get)]
    downscale_multiplier: f32,
    #[pyo3(get)]
    settings_raw: u32,
    #[pyo3(get)]
    packed: bool,
    #[pyo3(get)]
    packing_mode: u8,
    #[pyo3(get)]
    packing_rotation: u8,
    #[pyo3(get)]
    mesh_type: u8,
    secondary_textures: Option<Vec<Py<PySpriteAtlasSecondaryTexture>>>,
}

#[pymethods]
impl PySpriteAtlasRenderData {
    #[getter]
    fn key(&self, py: Python<'_>) -> Py<PySpriteAtlasRenderDataKey> {
        self.key.clone_ref(py)
    }

    #[getter]
    fn secondary_textures(
        &self,
        py: Python<'_>,
    ) -> PyResult<Option<Vec<Py<PySpriteAtlasSecondaryTexture>>>> {
        self.secondary_textures
            .as_deref()
            .map(|textures| clone_python_references(py, textures, "SpriteAtlas secondary textures"))
            .transpose()
    }
}

/// Complete, bounded metadata from one Unity `SpriteAtlas` object.
#[pyclass(name = "SpriteAtlas", frozen)]
#[derive(Debug)]
struct PySpriteAtlas {
    #[pyo3(get)]
    path_id: i64,
    #[pyo3(get)]
    name: String,
    #[pyo3(get)]
    packed_sprites: Vec<PyObjectReference>,
    #[pyo3(get)]
    packed_sprite_names: Vec<String>,
    render_data_entries: Vec<Py<PySpriteAtlasRenderData>>,
    #[pyo3(get)]
    tag: String,
    #[pyo3(get)]
    is_variant: bool,
}

struct PreparedSpriteAtlasRenderData {
    key: PySpriteAtlasRenderDataKey,
    texture: PyObjectReference,
    alpha_texture: PyObjectReference,
    texture_rect: (f32, f32, f32, f32),
    texture_rect_offset: (f32, f32),
    atlas_rect_offset: (f32, f32),
    uv_transform: (f32, f32, f32, f32),
    downscale_multiplier: f32,
    settings_raw: u32,
    packed: bool,
    packing_mode: u8,
    packing_rotation: u8,
    mesh_type: u8,
    secondary_textures: Option<Vec<PySpriteAtlasSecondaryTexture>>,
}

struct PreparedSpriteAtlas {
    path_id: i64,
    name: String,
    packed_sprites: Vec<PyObjectReference>,
    packed_sprite_names: Vec<String>,
    render_data_entries: Vec<PreparedSpriteAtlasRenderData>,
    tag: String,
    is_variant: bool,
}

#[pymethods]
impl PySpriteAtlas {
    #[getter]
    fn render_data_entries(&self, py: Python<'_>) -> PyResult<Vec<Py<PySpriteAtlasRenderData>>> {
        clone_python_references(
            py,
            &self.render_data_entries,
            "SpriteAtlas render-data entries",
        )
    }
}

/// Raw and decoded packing bits stored in one `SpriteRenderData` object.
#[pyclass(name = "SpriteSettings", frozen)]
#[derive(Debug)]
struct PySpriteSettings {
    raw: u32,
    packed: bool,
    packing_mode_tight: bool,
    packing_rotation: u8,
    mesh_type_tight: bool,
}

#[pymethods]
impl PySpriteSettings {
    #[getter]
    const fn raw(&self) -> u32 {
        self.raw
    }

    #[getter]
    const fn packed(&self) -> bool {
        self.packed
    }

    #[getter]
    const fn packing_mode(&self) -> &'static str {
        if self.packing_mode_tight {
            "tight"
        } else {
            "rectangle"
        }
    }

    #[getter]
    const fn packing_rotation(&self) -> u8 {
        self.packing_rotation
    }

    #[getter]
    const fn mesh_type(&self) -> &'static str {
        if self.mesh_type_tight {
            "tight"
        } else {
            "full_rect"
        }
    }
}

/// Caller-configurable budgets for metadata-only `Sprite` parsing.
#[pyclass(name = "SpriteMetadataLimits", frozen, skip_from_py_object)]
#[derive(Debug, Clone, Copy)]
struct PySpriteMetadataLimits {
    entries: usize,
    string_bytes: usize,
    total_string_bytes: usize,
    mesh_bytes: u64,
}

#[pymethods]
impl PySpriteMetadataLimits {
    #[new]
    #[pyo3(signature = (
        *,
        maximum_entries=1_000_000,
        maximum_string_bytes=16_777_216,
        maximum_total_string_bytes=33_554_432,
        maximum_mesh_bytes=536_870_912
    ))]
    const fn new(
        maximum_entries: usize,
        maximum_string_bytes: usize,
        maximum_total_string_bytes: usize,
        maximum_mesh_bytes: u64,
    ) -> Self {
        Self {
            entries: maximum_entries,
            string_bytes: maximum_string_bytes,
            total_string_bytes: maximum_total_string_bytes,
            mesh_bytes: maximum_mesh_bytes,
        }
    }

    #[getter]
    const fn maximum_entries(&self) -> usize {
        self.entries
    }

    #[getter]
    const fn maximum_string_bytes(&self) -> usize {
        self.string_bytes
    }

    #[getter]
    const fn maximum_total_string_bytes(&self) -> usize {
        self.total_string_bytes
    }

    #[getter]
    const fn maximum_mesh_bytes(&self) -> u64 {
        self.mesh_bytes
    }
}

impl From<PySpriteMetadataLimits> for SpriteReadLimits {
    fn from(value: PySpriteMetadataLimits) -> Self {
        Self {
            maximum_string_bytes: value.string_bytes,
            maximum_total_string_bytes: value.total_string_bytes,
            maximum_array_elements: value.entries,
            maximum_mesh_bytes: value.mesh_bytes,
            ..Self::default()
        }
    }
}

#[pyclass(name = "SpriteSecondaryTexture", frozen, get_all)]
#[derive(Debug)]
struct PySpriteSecondaryTexture {
    texture: PyObjectReference,
    name: String,
}

/// Complete resident render metadata stored directly on one `Sprite`.
#[pyclass(name = "SpriteRenderData", frozen)]
#[derive(Debug)]
struct PySpriteRenderData {
    #[pyo3(get)]
    texture: PyObjectReference,
    #[pyo3(get)]
    alpha_texture: PyObjectReference,
    secondary_textures: Vec<Py<PySpriteSecondaryTexture>>,
    #[pyo3(get)]
    texture_rect: (f32, f32, f32, f32),
    #[pyo3(get)]
    texture_rect_offset: (f32, f32),
    #[pyo3(get)]
    atlas_rect_offset: (f32, f32),
    settings: Py<PySpriteSettings>,
    #[pyo3(get)]
    uv_transform: (f32, f32, f32, f32),
    #[pyo3(get)]
    downscale_multiplier: f32,
    #[pyo3(get)]
    mesh_triangles: Vec<PySpriteTriangle>,
}

#[pymethods]
impl PySpriteRenderData {
    #[getter]
    fn secondary_textures(&self, py: Python<'_>) -> PyResult<Vec<Py<PySpriteSecondaryTexture>>> {
        clone_python_references(py, &self.secondary_textures, "Sprite secondary textures")
    }

    #[getter]
    fn settings(&self, py: Python<'_>) -> Py<PySpriteSettings> {
        self.settings.clone_ref(py)
    }
}

/// Complete, bounded metadata from one Unity `Sprite` object.
#[pyclass(name = "SpriteMetadata", frozen)]
#[derive(Debug)]
struct PySpriteMetadata {
    #[pyo3(get)]
    object_index: usize,
    #[pyo3(get)]
    path_id: i64,
    #[pyo3(get)]
    name: String,
    #[pyo3(get)]
    rect: (f32, f32, f32, f32),
    #[pyo3(get)]
    offset: (f32, f32),
    #[pyo3(get)]
    border: (f32, f32, f32, f32),
    #[pyo3(get)]
    pixels_to_units: f32,
    #[pyo3(get)]
    pivot: (f32, f32),
    #[pyo3(get)]
    extrude: u32,
    #[pyo3(get)]
    is_polygon: bool,
    render_data_key: Option<Py<PySpriteAtlasRenderDataKey>>,
    #[pyo3(get)]
    atlas_tags: Vec<String>,
    #[pyo3(get)]
    sprite_atlas: PyObjectReference,
    render_data: Py<PySpriteRenderData>,
}

#[pymethods]
impl PySpriteMetadata {
    #[getter]
    fn render_data_key(&self, py: Python<'_>) -> Option<Py<PySpriteAtlasRenderDataKey>> {
        self.render_data_key.as_ref().map(|key| key.clone_ref(py))
    }

    #[getter]
    fn render_data(&self, py: Python<'_>) -> Py<PySpriteRenderData> {
        self.render_data.clone_ref(py)
    }
}

/// Validated metadata from one source-bound ACL 2.x `compressed_tracks` blob.
#[pyclass(name = "AclCompressedTracks", frozen)]
#[derive(Debug)]
struct PyAclCompressedTracks {
    #[pyo3(get)]
    declared_size: u32,
    #[pyo3(get)]
    stored_hash: u32,
    #[pyo3(get)]
    version: u16,
    #[pyo3(get)]
    track_type: String,
    #[pyo3(get)]
    num_tracks: u32,
    #[pyo3(get)]
    num_samples_per_track: u32,
    #[pyo3(get)]
    sample_rate: f32,
    #[pyo3(get)]
    decompressed_value_count: u64,
    state_bits: u8,
}

/// Validated frame-major curves returned by a caller-supplied ACL decoder.
#[pyclass(name = "AclDecodedClip", frozen, get_all)]
#[derive(Debug)]
struct PyAclDecodedClip {
    times: Vec<f32>,
    binding_indices: Vec<u32>,
    values: Vec<f32>,
    following_curve_offset: u32,
}

#[pymethods]
impl PyAclCompressedTracks {
    #[getter]
    const fn has_metadata(&self) -> bool {
        self.state_bits & 1 != 0
    }

    #[getter]
    const fn is_wrap_optimized(&self) -> bool {
        self.state_bits & 2 != 0
    }

    #[getter]
    const fn has_database(&self) -> bool {
        self.state_bits & 4 != 0
    }

    #[getter]
    const fn has_stripped_keyframes(&self) -> bool {
        self.state_bits & 8 != 0
    }
}

#[pymethods]
impl PyAnimationClip {
    #[getter]
    const fn legacy(&self) -> bool {
        self.state_bits & 1 != 0
    }

    #[getter]
    const fn compressed(&self) -> bool {
        self.state_bits & 2 != 0
    }

    #[getter]
    const fn use_high_quality_curve(&self) -> bool {
        self.state_bits & 4 != 0
    }

    #[getter]
    const fn muscle_present(&self) -> bool {
        self.state_bits & 8 != 0
    }

    #[getter]
    const fn acl_present(&self) -> bool {
        self.state_bits & 16 != 0
    }
}

/// Stable metadata and references from a fully parsed `AnimatorController`.
#[pyclass(name = "AnimatorController", frozen, get_all)]
#[derive(Debug)]
struct PyAnimatorController {
    path_id: i64,
    name: String,
    controller_size: u32,
    layer_count: usize,
    state_machine_count: usize,
    value_count: usize,
    entity_id_count: Option<usize>,
    tos: Vec<(u32, String)>,
    animation_clips: Vec<(i32, i64)>,
}

/// Stable skeleton, TOS, and human-description metadata from one `Avatar`.
#[pyclass(name = "Avatar", frozen, get_all)]
#[derive(Debug)]
struct PyAvatar {
    path_id: i64,
    name: String,
    declared_avatar_size: u32,
    skeleton_node_count: usize,
    human_skeleton_node_count: usize,
    path_count: usize,
    paths: Vec<(u32, String)>,
    has_human_description: bool,
    human_bone_count: usize,
    skeleton_bone_count: usize,
    root_motion_bone_name: Option<String>,
}

/// A trusted, complete Unity object tree for one managed script type.
#[pyclass(name = "MonoBehaviourSchema", frozen, skip_from_py_object)]
#[derive(Debug)]
struct PyMonoBehaviourSchema {
    assembly_name: String,
    namespace: String,
    class_name: String,
    unity_version: Option<String>,
    node_count: usize,
    registry: Arc<MonoBehaviourSchemaRegistry>,
}

#[pymethods]
impl PyMonoBehaviourSchema {
    #[new]
    #[pyo3(signature = (
        assembly_name,
        class_name,
        nodes,
        *,
        namespace=None,
        unity_version=None
    ))]
    fn new(
        py: Python<'_>,
        assembly_name: String,
        class_name: String,
        nodes: &Bound<'_, PyList>,
        namespace: Option<String>,
        unity_version: Option<String>,
    ) -> PyResult<Self> {
        let namespace = namespace.unwrap_or_default();
        let initial_string_bytes = checked_schema_string_bytes(
            [&assembly_name, &namespace, &class_name]
                .into_iter()
                .map(String::as_str),
        )?;
        let nodes = extract_schema_nodes(nodes, initial_string_bytes)?;
        py.detach(move || {
            let mut tree_nodes = Vec::new();
            tree_nodes.try_reserve_exact(nodes.len()).map_err(|error| {
                PyMemoryError::new_err(format!(
                    "cannot allocate MonoBehaviour schema nodes: {error}"
                ))
            })?;
            for (index, (type_name, field_name, level, align)) in nodes.into_iter().enumerate() {
                tree_nodes.push(TypeTreeNode {
                    type_name,
                    field_name,
                    byte_size: -1,
                    index: i32::try_from(index).map_err(|_| {
                        PyValueError::new_err("MonoBehaviour schema node index exceeds i32")
                    })?,
                    type_flags: 0,
                    version: 1,
                    meta_flags: if align { 0x4000 } else { 0 },
                    level,
                    type_string_offset: None,
                    name_string_offset: None,
                    reference_type_hash: 0,
                });
            }
            let node_count = tree_nodes.len();
            let mut registry = MonoBehaviourSchemaRegistry::new();
            registry
                .push(MonoBehaviourSchemaEntry {
                    assembly_name: try_copy_string(&assembly_name, "schema assembly name")?,
                    namespace: try_copy_string(&namespace, "schema namespace")?,
                    class_name: try_copy_string(&class_name, "schema class name")?,
                    unity_version: try_copy_optional_string(
                        unity_version.as_deref(),
                        "schema Unity version",
                    )?,
                    tree: TypeTree {
                        nodes: tree_nodes,
                        string_buffer: Vec::new(),
                    },
                })
                .map_err(core_error)?;
            Ok(Self {
                assembly_name,
                namespace,
                class_name,
                unity_version,
                node_count,
                registry: Arc::new(registry),
            })
        })
    }

    #[getter]
    fn assembly_name(&self) -> &str {
        &self.assembly_name
    }

    #[getter]
    fn namespace(&self) -> &str {
        &self.namespace
    }

    #[getter]
    fn class_name(&self) -> &str {
        &self.class_name
    }

    #[getter]
    fn unity_version(&self) -> Option<&str> {
        self.unity_version.as_deref()
    }

    #[getter]
    const fn node_count(&self) -> usize {
        self.node_count
    }
}

/// A reusable collection of trusted, complete managed object schemas.
#[pyclass(name = "MonoBehaviourSchemas", frozen, skip_from_py_object)]
#[derive(Debug)]
struct PyMonoBehaviourSchemas {
    schema_count: usize,
    provider: Arc<MonoBehaviourSchemaRegistrySet>,
}

#[pymethods]
impl PyMonoBehaviourSchemas {
    #[new]
    fn new(py: Python<'_>, schemas: &Bound<'_, PyList>) -> PyResult<Self> {
        if schemas.len() > MAXIMUM_SCHEMA_ENTRIES {
            return Err(PyValueError::new_err(format!(
                "MonoBehaviour schema collection has {} entries; maximum is {MAXIMUM_SCHEMA_ENTRIES}",
                schemas.len()
            )));
        }
        let mut registries =
            reserve_metadata(schemas.len(), "MonoBehaviour schema collection entries")?;
        for schema in schemas.iter() {
            let schema: PyRef<'_, PyMonoBehaviourSchema> = schema.extract()?;
            registries.push(Arc::clone(&schema.registry));
        }
        let schema_count = registries.len();
        let provider = py
            .detach(move || MonoBehaviourSchemaRegistrySet::from_registries(registries))
            .map_err(core_error)?;
        Ok(Self {
            schema_count,
            provider: Arc::new(provider),
        })
    }

    #[getter]
    const fn schema_count(&self) -> usize {
        self.schema_count
    }
}

#[pyclass(name = "BuildSettings", frozen, get_all)]
#[derive(Debug)]
struct PyBuildSettings {
    path_id: i64,
    levels: Option<Vec<String>>,
    scenes: Option<Vec<String>>,
}

#[pyclass(name = "PlayerSettings", frozen, get_all)]
#[derive(Debug)]
struct PyPlayerSettings {
    path_id: i64,
    company_name: String,
    product_name: String,
}

#[pyclass(name = "CubismExpressionParameter", frozen, get_all)]
#[derive(Debug)]
struct PyCubismExpressionParameter {
    id: String,
    value: f64,
    blend: String,
}

#[pyclass(name = "CubismExpression", frozen)]
#[derive(Debug)]
struct PyCubismExpression {
    path_id: i64,
    source_name: String,
    expression_type: String,
    fade_in_time: f64,
    fade_out_time: f64,
    parameters: Vec<Py<PyCubismExpressionParameter>>,
    json: Vec<u8>,
}

#[pyclass(name = "CubismPosePart", frozen, get_all)]
#[derive(Debug)]
struct PyCubismPosePart {
    path_id: i64,
    group_index: i32,
    links: Vec<String>,
}

#[pyclass(name = "CubismDisplayInfo", frozen, get_all)]
#[derive(Debug)]
struct PyCubismDisplayInfo {
    path_id: i64,
    name: String,
    display_name: Option<String>,
}

#[pyclass(name = "CubismPhysics", frozen)]
#[derive(Debug)]
struct PyCubismPhysics {
    path_id: i64,
    fps: f64,
    gravity: (f64, f64),
    wind: (f64, f64),
    sub_rig_count: usize,
    input_count: usize,
    output_count: usize,
    particle_count: usize,
    json: Vec<u8>,
}

#[pymethods]
impl PyCubismPhysics {
    #[getter]
    const fn path_id(&self) -> i64 {
        self.path_id
    }

    #[getter]
    const fn fps(&self) -> f64 {
        self.fps
    }

    #[getter]
    const fn gravity(&self) -> (f64, f64) {
        self.gravity
    }

    #[getter]
    const fn wind(&self) -> (f64, f64) {
        self.wind
    }

    #[getter]
    const fn sub_rig_count(&self) -> usize {
        self.sub_rig_count
    }

    #[getter]
    const fn input_count(&self) -> usize {
        self.input_count
    }

    #[getter]
    const fn output_count(&self) -> usize {
        self.output_count
    }

    #[getter]
    const fn particle_count(&self) -> usize {
        self.particle_count
    }

    #[getter]
    fn json<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        python_bytes(py, &self.json)
    }
}

#[pyclass(name = "CubismFadeMotion", frozen)]
#[derive(Debug)]
struct PyCubismFadeMotion {
    path_id: i64,
    source_name: String,
    motion_name: String,
    fade_in_time: f64,
    fade_out_time: f64,
    motion_length: f64,
    curve_count: usize,
    keyframe_count: usize,
    json: Vec<u8>,
}

#[pymethods]
impl PyCubismFadeMotion {
    #[getter]
    const fn path_id(&self) -> i64 {
        self.path_id
    }
    #[getter]
    fn source_name(&self) -> &str {
        &self.source_name
    }
    #[getter]
    fn motion_name(&self) -> &str {
        &self.motion_name
    }
    #[getter]
    const fn fade_in_time(&self) -> f64 {
        self.fade_in_time
    }
    #[getter]
    const fn fade_out_time(&self) -> f64 {
        self.fade_out_time
    }
    #[getter]
    const fn motion_length(&self) -> f64 {
        self.motion_length
    }
    #[getter]
    const fn curve_count(&self) -> usize {
        self.curve_count
    }
    #[getter]
    const fn keyframe_count(&self) -> usize {
        self.keyframe_count
    }
    #[getter]
    fn json<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        python_bytes(py, &self.json)
    }
}

#[pyclass(name = "CubismClipMotion", frozen)]
#[derive(Debug)]
struct PyCubismClipMotion {
    file_index: usize,
    path_id: i64,
    name: String,
    duration: f64,
    fps: f64,
    curve_count: usize,
    keyframe_count: usize,
    event_count: usize,
    json: Vec<u8>,
}

#[pyclass(name = "CubismMotionTargets", frozen, get_all, skip_from_py_object)]
#[derive(Debug, Clone)]
struct PyCubismMotionTargets {
    parameters: Vec<String>,
    parts: Vec<String>,
}

#[pymethods]
impl PyCubismMotionTargets {
    #[new]
    #[pyo3(signature = (*, parameters=None, parts=None))]
    fn new(
        parameters: Option<&Bound<'_, PyList>>,
        parts: Option<&Bound<'_, PyList>>,
    ) -> PyResult<Self> {
        Ok(Self {
            parameters: copy_python_string_list(parameters, "Cubism parameter names")?,
            parts: copy_python_string_list(parts, "Cubism part names")?,
        })
    }
}

#[pymethods]
impl PyCubismClipMotion {
    #[getter]
    const fn file_index(&self) -> usize {
        self.file_index
    }
    #[getter]
    const fn path_id(&self) -> i64 {
        self.path_id
    }
    #[getter]
    fn name(&self) -> &str {
        &self.name
    }
    #[getter]
    const fn duration(&self) -> f64 {
        self.duration
    }
    #[getter]
    const fn fps(&self) -> f64 {
        self.fps
    }
    #[getter]
    const fn curve_count(&self) -> usize {
        self.curve_count
    }
    #[getter]
    const fn keyframe_count(&self) -> usize {
        self.keyframe_count
    }
    #[getter]
    const fn event_count(&self) -> usize {
        self.event_count
    }
    #[getter]
    fn json<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        python_bytes(py, &self.json)
    }
}

#[pymethods]
impl PyCubismDisplayInfo {
    #[getter]
    fn effective_name(&self) -> &str {
        self.display_name
            .as_deref()
            .filter(|value| !value.is_empty())
            .unwrap_or(&self.name)
    }
}

#[pymethods]
impl PyCubismExpression {
    #[getter]
    const fn path_id(&self) -> i64 {
        self.path_id
    }

    #[getter]
    fn source_name(&self) -> &str {
        &self.source_name
    }

    #[getter]
    fn expression_type(&self) -> &str {
        &self.expression_type
    }

    #[getter]
    const fn fade_in_time(&self) -> f64 {
        self.fade_in_time
    }

    #[getter]
    const fn fade_out_time(&self) -> f64 {
        self.fade_out_time
    }

    #[getter]
    fn parameters(&self, py: Python<'_>) -> PyResult<Vec<Py<PyCubismExpressionParameter>>> {
        clone_python_references(py, &self.parameters, "Cubism expression parameters")
    }

    #[getter]
    fn json<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        python_bytes(py, &self.json)
    }
}

#[pyclass(name = "SceneNode", frozen, get_all, skip_from_py_object)]
#[derive(Debug, Clone)]
struct PySceneNode {
    file_index: usize,
    path_id: i64,
    name: String,
    parent: Option<(usize, i64)>,
    children: Vec<(usize, i64)>,
    local_position: Option<(f32, f32, f32)>,
    local_rotation: Option<(f32, f32, f32, f32)>,
    local_scale: Option<(f32, f32, f32)>,
    mesh: Option<(usize, i64)>,
    materials: Vec<Option<(usize, i64)>>,
    bones: Vec<Option<(usize, i64)>>,
    animator: Option<(usize, i64)>,
}

/// Caller-configurable collection-wide scene assembly budgets.
#[pyclass(name = "SceneLimits", frozen, skip_from_py_object)]
#[derive(Debug, Clone, Copy)]
struct PySceneLimits {
    game_objects: usize,
    total_components: usize,
    total_transform_child_references: usize,
    total_material_references: usize,
    total_bone_references: usize,
    hierarchy_edges: usize,
    index_bytes: usize,
}

#[pymethods]
impl PySceneLimits {
    #[new]
    #[pyo3(signature = (
        *,
        maximum_game_objects=1_000_000,
        maximum_total_components=10_000_000,
        maximum_total_transform_child_references=10_000_000,
        maximum_total_material_references=10_000_000,
        maximum_total_bone_references=10_000_000,
        maximum_hierarchy_edges=1_000_000,
        maximum_index_bytes=268_435_456
    ))]
    const fn new(
        maximum_game_objects: usize,
        maximum_total_components: usize,
        maximum_total_transform_child_references: usize,
        maximum_total_material_references: usize,
        maximum_total_bone_references: usize,
        maximum_hierarchy_edges: usize,
        maximum_index_bytes: usize,
    ) -> Self {
        Self {
            game_objects: maximum_game_objects,
            total_components: maximum_total_components,
            total_transform_child_references: maximum_total_transform_child_references,
            total_material_references: maximum_total_material_references,
            total_bone_references: maximum_total_bone_references,
            hierarchy_edges: maximum_hierarchy_edges,
            index_bytes: maximum_index_bytes,
        }
    }

    #[getter]
    const fn maximum_game_objects(&self) -> usize {
        self.game_objects
    }

    #[getter]
    const fn maximum_total_components(&self) -> usize {
        self.total_components
    }

    #[getter]
    const fn maximum_total_transform_child_references(&self) -> usize {
        self.total_transform_child_references
    }

    #[getter]
    const fn maximum_total_material_references(&self) -> usize {
        self.total_material_references
    }

    #[getter]
    const fn maximum_total_bone_references(&self) -> usize {
        self.total_bone_references
    }

    #[getter]
    const fn maximum_hierarchy_edges(&self) -> usize {
        self.hierarchy_edges
    }

    #[getter]
    const fn maximum_index_bytes(&self) -> usize {
        self.index_bytes
    }
}

impl From<PySceneLimits> for SceneHierarchyLimits {
    fn from(value: PySceneLimits) -> Self {
        Self {
            maximum_game_objects: value.game_objects,
            maximum_total_components: value.total_components,
            maximum_total_transform_child_references: value.total_transform_child_references,
            maximum_total_material_references: value.total_material_references,
            maximum_total_bone_references: value.total_bone_references,
            maximum_hierarchy_edges: value.hierarchy_edges,
            maximum_index_bytes: value.index_bytes,
            ..Self::default()
        }
    }
}

/// Caller-configurable budgets for textures returned beside a model.
#[pyclass(name = "ModelTextureLimits", frozen, skip_from_py_object)]
#[derive(Debug, Clone, Copy)]
struct PyModelTextureLimits {
    texture_references: usize,
    textures: usize,
    name_index_bytes: u64,
    metadata_bytes: u64,
    total_encoded_bytes: u64,
    single_texture_bytes: u64,
}

#[pymethods]
impl PyModelTextureLimits {
    #[new]
    #[pyo3(signature = (
        *,
        maximum_texture_references=1_000_000,
        maximum_textures=4_096,
        maximum_name_index_bytes=67_108_864,
        maximum_metadata_bytes=268_435_456,
        maximum_total_encoded_bytes=2_147_483_648,
        maximum_single_texture_bytes=536_870_912
    ))]
    const fn new(
        maximum_texture_references: usize,
        maximum_textures: usize,
        maximum_name_index_bytes: u64,
        maximum_metadata_bytes: u64,
        maximum_total_encoded_bytes: u64,
        maximum_single_texture_bytes: u64,
    ) -> Self {
        Self {
            texture_references: maximum_texture_references,
            textures: maximum_textures,
            name_index_bytes: maximum_name_index_bytes,
            metadata_bytes: maximum_metadata_bytes,
            total_encoded_bytes: maximum_total_encoded_bytes,
            single_texture_bytes: maximum_single_texture_bytes,
        }
    }

    #[getter]
    const fn maximum_texture_references(&self) -> usize {
        self.texture_references
    }

    #[getter]
    const fn maximum_textures(&self) -> usize {
        self.textures
    }

    #[getter]
    const fn maximum_name_index_bytes(&self) -> u64 {
        self.name_index_bytes
    }

    #[getter]
    const fn maximum_metadata_bytes(&self) -> u64 {
        self.metadata_bytes
    }

    #[getter]
    const fn maximum_total_encoded_bytes(&self) -> u64 {
        self.total_encoded_bytes
    }

    #[getter]
    const fn maximum_single_texture_bytes(&self) -> u64 {
        self.single_texture_bytes
    }
}

impl From<PyModelTextureLimits> for SceneTextureLimits {
    fn from(value: PyModelTextureLimits) -> Self {
        let texture = TextureReadLimits {
            maximum_payload_bytes: value.single_texture_bytes,
            maximum_output_bytes: value.single_texture_bytes,
            maximum_decoder_working_bytes: value.single_texture_bytes,
            ..TextureReadLimits::default()
        };
        Self {
            maximum_texture_references: value.texture_references,
            maximum_textures: value.textures,
            maximum_name_index_bytes: value.name_index_bytes,
            maximum_metadata_bytes: value.metadata_bytes,
            maximum_total_encoded_bytes: value.total_encoded_bytes,
            texture,
        }
    }
}

#[pyclass(name = "FbxCandidate", frozen, get_all, skip_from_py_object)]
#[derive(Debug, Clone)]
struct PyFbxCandidate {
    file_index: usize,
    path_id: i64,
    animator: Option<(usize, i64)>,
    name: String,
}

#[pyclass(name = "RgbaImage", frozen)]
#[derive(Debug)]
struct PyRgbaImage {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

/// A decoded texture whose pixels already use the top-down row order exposed
/// to Python. Returning this type from `Python::detach` keeps the O(pixel
/// bytes) row conversion outside the GIL; the attached path can only move the
/// owned buffer into its Python wrapper.
struct DisplayRowPyImage(unity_rs_core::texture::RgbaImage);

impl DisplayRowPyImage {
    fn from_decoded(mut image: unity_rs_core::texture::RgbaImage) -> PyResult<Self> {
        flip_rgba_rows(&mut image)?;
        Ok(Self(image))
    }

    fn into_python(self) -> PyRgbaImage {
        let image = self.0;
        PyRgbaImage {
            width: image.width,
            height: image.height,
            pixels: image.pixels,
        }
    }
}

/// The same detached row-order invariant for every `Texture2DArray` layer.
struct DisplayRowPyImages(Vec<unity_rs_core::texture::RgbaImage>);

impl DisplayRowPyImages {
    fn from_decoded(mut images: Vec<unity_rs_core::texture::RgbaImage>) -> PyResult<Self> {
        for image in &mut images {
            flip_rgba_rows(image)?;
        }
        Ok(Self(images))
    }

    fn into_python(self) -> PyResult<Vec<PyRgbaImage>> {
        let mut output = reserve_metadata(self.0.len(), "Python Texture2DArray images")?;
        for image in self.0 {
            output.push(DisplayRowPyImage(image).into_python());
        }
        Ok(output)
    }
}

#[pyclass(name = "AudioClip", frozen)]
#[derive(Debug)]
struct PyAudioClip {
    name: String,
    extension: String,
    payload_kind: &'static str,
    bytes: Vec<u8>,
}

/// A decoded byte-oriented Unity asset payload rather than its serialized wrapper.
#[pyclass(name = "BinaryAsset", frozen)]
#[derive(Debug)]
struct PyBinaryAsset {
    name: String,
    extension: String,
    payload_kind: &'static str,
    bytes: Vec<u8>,
}

type PyMaterialTextureEnvironment = (String, (i32, i64), (f32, f32), (f32, f32));
type PyMaterialColor = (String, (f32, f32, f32, f32));

/// A bounded Unity `Material` property sheet with ordered duplicate entries preserved.
#[pyclass(name = "Material", frozen, get_all)]
#[derive(Debug)]
struct PyMaterial {
    path_id: i64,
    name: String,
    shader: (i32, i64),
    legacy_shader_keywords: Vec<String>,
    valid_keywords: Vec<String>,
    invalid_keywords: Vec<String>,
    lightmap_flags: Option<u32>,
    enable_instancing_variants: Option<bool>,
    custom_render_queue: Option<i32>,
    string_tags: Vec<(String, String)>,
    disabled_shader_passes: Vec<String>,
    texture_environments: Vec<PyMaterialTextureEnvironment>,
    integers: Vec<(String, i32)>,
    floats: Vec<(String, f32)>,
    colors: Vec<PyMaterialColor>,
    trailing_bytes: u64,
}

/// Managed type identity stored in one Unity `MonoScript` object.
#[pyclass(name = "MonoScript", frozen, get_all)]
#[derive(Debug)]
struct PyMonoScript {
    path_id: i64,
    name: String,
    execution_order: Option<i32>,
    class_name: String,
    namespace: String,
    assembly_name: String,
    is_editor_script: Option<bool>,
}

#[pymethods]
impl PyBinaryAsset {
    #[getter]
    fn name(&self) -> &str {
        &self.name
    }

    #[getter]
    fn extension(&self) -> &str {
        &self.extension
    }

    #[getter]
    const fn payload_kind(&self) -> &'static str {
        self.payload_kind
    }

    #[getter]
    fn data<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        python_bytes(py, &self.bytes)
    }

    fn __repr__(&self) -> String {
        format!(
            "BinaryAsset(name={:?}, extension={:?}, payload_kind={:?}, bytes={})",
            self.name,
            self.extension,
            self.payload_kind,
            self.bytes.len()
        )
    }
}

#[pymethods]
impl PyAudioClip {
    #[getter]
    fn name(&self) -> &str {
        &self.name
    }

    #[getter]
    fn extension(&self) -> &str {
        &self.extension
    }

    #[getter]
    const fn payload_kind(&self) -> &'static str {
        self.payload_kind
    }

    #[getter]
    fn data<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        python_bytes(py, &self.bytes)
    }

    fn __repr__(&self) -> String {
        format!(
            "AudioClip(name={:?}, extension={:?}, bytes={})",
            self.name,
            self.extension,
            self.bytes.len()
        )
    }
}

#[pymethods]
impl PyRgbaImage {
    #[getter]
    const fn width(&self) -> u32 {
        self.width
    }

    #[getter]
    const fn height(&self) -> u32 {
        self.height
    }

    #[getter]
    fn rgba<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        python_bytes(py, &self.pixels)
    }

    fn __repr__(&self) -> String {
        format!("RgbaImage(width={}, height={})", self.width, self.height)
    }
}

/// The name the bindings report for a schema source.
const fn schema_source_name(source: MonoBehaviourSchemaSource) -> &'static str {
    match source {
        MonoBehaviourSchemaSource::Embedded => "embedded",
        MonoBehaviourSchemaSource::External => "schema",
    }
}

/// A `MonoBehaviour` read as JSON, and which tree it was read through.
#[pyclass(name = "MonoBehaviourJson", frozen, get_all)]
#[derive(Debug)]
struct PyMonoBehaviourJson {
    json: String,
    /// `"embedded"` when the file carried its own type tree, `"schema"` when
    /// the layout came from a supplied schema. Worth distinguishing: a value
    /// read through a schema is only as good as that schema.
    source: String,
}

#[pyclass(name = "ExportReport", frozen, get_all)]
#[derive(Debug)]
struct PyExportReport {
    exported: Vec<String>,
    failures: Vec<String>,
    /// Objects declined by design rather than broken. A modern build carries
    /// hundreds of them, and folding them into `failures` would make every
    /// such export look like it went wrong.
    unsupported: Vec<String>,
}

#[pyclass(name = "ExtractionLimits", frozen, skip_from_py_object)]
#[derive(Debug, Clone, Copy)]
struct PyExtractionLimits {
    input_files: usize,
    nesting_depth: usize,
    entries: usize,
    single_entry_bytes: u64,
    expanded_bytes: u64,
    output_bytes: u64,
    path_bytes: usize,
    total_path_bytes: usize,
    metadata_bytes: usize,
}

#[pymethods]
impl PyExtractionLimits {
    #[new]
    // PyO3 exposes these as named-only policy fields; grouping them would make
    // the Python constructor less explicit without reducing the public surface.
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (
        *,
        maximum_input_files=1_000_000,
        maximum_nesting_depth=32,
        maximum_entries=1_000_000,
        maximum_single_entry_bytes=536_870_912,
        maximum_expanded_bytes=4_294_967_296,
        maximum_output_bytes=4_294_967_296,
        maximum_path_bytes=32_767,
        maximum_total_path_bytes=67_108_864,
        maximum_metadata_bytes=268_435_456
    ))]
    const fn new(
        maximum_input_files: usize,
        maximum_nesting_depth: usize,
        maximum_entries: usize,
        maximum_single_entry_bytes: u64,
        maximum_expanded_bytes: u64,
        maximum_output_bytes: u64,
        maximum_path_bytes: usize,
        maximum_total_path_bytes: usize,
        maximum_metadata_bytes: usize,
    ) -> Self {
        Self {
            input_files: maximum_input_files,
            nesting_depth: maximum_nesting_depth,
            entries: maximum_entries,
            single_entry_bytes: maximum_single_entry_bytes,
            expanded_bytes: maximum_expanded_bytes,
            output_bytes: maximum_output_bytes,
            path_bytes: maximum_path_bytes,
            total_path_bytes: maximum_total_path_bytes,
            metadata_bytes: maximum_metadata_bytes,
        }
    }

    #[getter]
    const fn maximum_input_files(&self) -> usize {
        self.input_files
    }

    #[getter]
    const fn maximum_nesting_depth(&self) -> usize {
        self.nesting_depth
    }

    #[getter]
    const fn maximum_entries(&self) -> usize {
        self.entries
    }

    #[getter]
    const fn maximum_single_entry_bytes(&self) -> u64 {
        self.single_entry_bytes
    }

    #[getter]
    const fn maximum_expanded_bytes(&self) -> u64 {
        self.expanded_bytes
    }

    #[getter]
    const fn maximum_output_bytes(&self) -> u64 {
        self.output_bytes
    }

    #[getter]
    const fn maximum_path_bytes(&self) -> usize {
        self.path_bytes
    }

    #[getter]
    const fn maximum_total_path_bytes(&self) -> usize {
        self.total_path_bytes
    }

    #[getter]
    const fn maximum_metadata_bytes(&self) -> usize {
        self.metadata_bytes
    }
}

#[pyclass(name = "ExtractionRecord", frozen, get_all)]
#[derive(Debug)]
struct PyExtractionRecord {
    source: String,
    output_path: String,
    bytes: u64,
}

#[pyclass(name = "ExtractionSkip", frozen, get_all)]
#[derive(Debug)]
struct PyExtractionSkip {
    source: String,
    output_path: String,
}

#[pyclass(name = "ExtractionFailure", frozen, get_all)]
#[derive(Debug)]
struct PyExtractionFailure {
    source: String,
    error: String,
}

#[pyclass(name = "ExtractionReport", frozen)]
#[derive(Debug)]
struct PyExtractionReport {
    extracted: Vec<PyExtractionRecord>,
    skipped_existing: Vec<PyExtractionSkip>,
    failures: Vec<PyExtractionFailure>,
    output_bytes: u64,
}

#[pymethods]
impl PyExtractionReport {
    #[getter]
    fn extracted(&self) -> PyResult<Vec<PyExtractionRecord>> {
        try_clone_extraction_records(&self.extracted)
    }

    #[getter]
    fn skipped_existing(&self) -> PyResult<Vec<PyExtractionSkip>> {
        try_clone_extraction_skips(&self.skipped_existing)
    }

    #[getter]
    fn failures(&self) -> PyResult<Vec<PyExtractionFailure>> {
        try_clone_extraction_failures(&self.failures)
    }

    #[getter]
    const fn output_bytes(&self) -> u64 {
        self.output_bytes
    }
}

impl From<PyExtractionLimits> for ExtractionLimits {
    fn from(value: PyExtractionLimits) -> Self {
        Self {
            maximum_input_files: value.input_files,
            maximum_nesting_depth: value.nesting_depth,
            maximum_entries: value.entries,
            maximum_single_entry_bytes: value.single_entry_bytes,
            maximum_expanded_bytes: value.expanded_bytes,
            maximum_output_bytes: value.output_bytes,
            maximum_path_bytes: value.path_bytes,
            maximum_total_path_bytes: value.total_path_bytes,
            maximum_metadata_bytes: value.metadata_bytes,
            compression: CompressionLimits {
                maximum_input_bytes: value.single_entry_bytes,
                maximum_output_bytes: value.single_entry_bytes,
                maximum_zip_entries: value.entries,
                maximum_zip_path_bytes: value.path_bytes,
                maximum_zip_entry_bytes: value.single_entry_bytes,
                maximum_zip_total_bytes: value.expanded_bytes,
            },
        }
    }
}

#[pyclass(name = "ExportLimits", frozen, skip_from_py_object)]
#[derive(Debug, Clone, Copy)]
struct PyExportLimits {
    objects: usize,
    total_output_bytes: u64,
    metadata_bytes: u64,
}

/// One file a model export names by file name.
#[pyclass(name = "ModelFile", frozen)]
#[derive(Debug)]
struct PyModelFile {
    file_name: String,
    data: Vec<u8>,
}

#[pymethods]
impl PyModelFile {
    #[getter]
    fn file_name(&self) -> &str {
        &self.file_name
    }

    #[getter]
    fn data<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        python_bytes(py, &self.data)
    }
}

/// A scene written as Wavefront OBJ, with the files it names.
///
/// The OBJ's `mtllib` line names the material library and the library's
/// `map_*` lines name the textures, all resolved by file name against the
/// OBJ's own directory. They come back rather than being written because this
/// call has no directory of its own, and splitting them across directories
/// breaks the references.
#[pyclass(name = "ModelObj", frozen)]
#[derive(Debug)]
struct PyModelObj {
    obj: Vec<u8>,
    material_library_name: String,
    material_library: Vec<u8>,
    textures: Vec<Py<PyModelFile>>,
    /// Texture references this reader could not resolve or decode, with the
    /// reason. Reported rather than raised so one bad texture does not cost
    /// the model.
    skipped: Vec<String>,
}

#[pymethods]
impl PyModelObj {
    #[getter]
    fn obj<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        python_bytes(py, &self.obj)
    }

    #[getter]
    fn material_library_name(&self) -> &str {
        &self.material_library_name
    }

    #[getter]
    fn material_library<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        python_bytes(py, &self.material_library)
    }

    #[getter]
    fn textures(&self, py: Python<'_>) -> PyResult<Vec<Py<PyModelFile>>> {
        clone_python_references(py, &self.textures, "model texture files")
    }

    #[getter]
    fn skipped(&self) -> PyResult<Vec<String>> {
        copy_strings(&self.skipped, "skipped model textures")
    }
}

/// An FBX and the texture files it references by name.
#[pyclass(name = "TexturedFbx", frozen)]
#[derive(Debug)]
struct PyTexturedFbx {
    fbx: Vec<u8>,
    textures: Vec<Py<PyModelFile>>,
    skipped: Vec<String>,
}

#[pymethods]
impl PyTexturedFbx {
    #[getter]
    fn fbx<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        python_bytes(py, &self.fbx)
    }

    #[getter]
    fn textures(&self, py: Python<'_>) -> PyResult<Vec<Py<PyModelFile>>> {
        clone_python_references(py, &self.textures, "FBX texture files")
    }

    #[getter]
    fn skipped(&self) -> PyResult<Vec<String>> {
        copy_strings(&self.skipped, "skipped FBX textures")
    }
}

#[pyclass(name = "Live2dTexture", frozen)]
struct PyLive2dTexture {
    file_name: String,
    png: Vec<u8>,
}

#[pymethods]
impl PyLive2dTexture {
    #[getter]
    fn file_name(&self) -> &str {
        &self.file_name
    }

    #[getter]
    fn png<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        python_bytes(py, &self.png)
    }
}

#[pyclass(name = "Live2dExpressionFile", frozen)]
struct PyLive2dExpressionFile {
    name: String,
    file_name: String,
    json: Vec<u8>,
}

#[pyclass(name = "Live2dMotionFile", frozen)]
struct PyLive2dMotionFile {
    name: String,
    file_name: String,
    json: Vec<u8>,
}

#[pymethods]
impl PyLive2dMotionFile {
    #[getter]
    fn name(&self) -> &str {
        &self.name
    }

    #[getter]
    fn file_name(&self) -> &str {
        &self.file_name
    }

    #[getter]
    fn json<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        python_bytes(py, &self.json)
    }
}

#[pymethods]
impl PyLive2dExpressionFile {
    #[getter]
    fn name(&self) -> &str {
        &self.name
    }

    #[getter]
    fn file_name(&self) -> &str {
        &self.file_name
    }

    #[getter]
    fn json<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        python_bytes(py, &self.json)
    }
}

#[pyclass(name = "Live2dJsonFile", frozen)]
struct PyLive2dJsonFile {
    file_name: String,
    json: Vec<u8>,
}

#[pymethods]
impl PyLive2dJsonFile {
    #[getter]
    fn file_name(&self) -> &str {
        &self.file_name
    }

    #[getter]
    fn json<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        python_bytes(py, &self.json)
    }
}

#[pyclass(name = "Live2dPackage", frozen)]
struct PyLive2dPackage {
    model: (usize, i64),
    moc_object: (usize, i64),
    name: String,
    directory_name: String,
    moc_file_name: String,
    moc: Vec<u8>,
    manifest_file_name: String,
    manifest: Vec<u8>,
    textures: Vec<Py<PyLive2dTexture>>,
    expressions: Vec<Py<PyLive2dExpressionFile>>,
    motions: Vec<Py<PyLive2dMotionFile>>,
    eye_blink_parameters: Vec<String>,
    lip_sync_parameters: Vec<String>,
    physics: Option<Py<PyLive2dJsonFile>>,
    pose: Option<Py<PyLive2dJsonFile>>,
    display_info: Option<Py<PyLive2dJsonFile>>,
}

#[pymethods]
impl PyLive2dPackage {
    #[getter]
    const fn model(&self) -> (usize, i64) {
        self.model
    }

    #[getter]
    const fn moc_object(&self) -> (usize, i64) {
        self.moc_object
    }

    #[getter]
    fn name(&self) -> &str {
        &self.name
    }

    #[getter]
    fn directory_name(&self) -> &str {
        &self.directory_name
    }

    #[getter]
    fn moc_file_name(&self) -> &str {
        &self.moc_file_name
    }

    #[getter]
    fn moc<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        python_bytes(py, &self.moc)
    }

    #[getter]
    fn manifest_file_name(&self) -> &str {
        &self.manifest_file_name
    }

    #[getter]
    fn manifest<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        python_bytes(py, &self.manifest)
    }

    #[getter]
    fn textures(&self, py: Python<'_>) -> PyResult<Vec<Py<PyLive2dTexture>>> {
        clone_python_references(py, &self.textures, "Live2D textures")
    }

    #[getter]
    fn expressions(&self, py: Python<'_>) -> PyResult<Vec<Py<PyLive2dExpressionFile>>> {
        clone_python_references(py, &self.expressions, "Live2D expressions")
    }

    #[getter]
    fn motions(&self, py: Python<'_>) -> PyResult<Vec<Py<PyLive2dMotionFile>>> {
        clone_python_references(py, &self.motions, "Live2D motions")
    }

    #[getter]
    fn eye_blink_parameters(&self) -> PyResult<Vec<String>> {
        copy_strings(&self.eye_blink_parameters, "Live2D eye-blink parameters")
    }

    #[getter]
    fn lip_sync_parameters(&self) -> PyResult<Vec<String>> {
        copy_strings(&self.lip_sync_parameters, "Live2D lip-sync parameters")
    }

    #[getter]
    fn physics(&self, py: Python<'_>) -> Option<Py<PyLive2dJsonFile>> {
        self.physics.as_ref().map(|value| value.clone_ref(py))
    }

    #[getter]
    fn pose(&self, py: Python<'_>) -> Option<Py<PyLive2dJsonFile>> {
        self.pose.as_ref().map(|value| value.clone_ref(py))
    }

    #[getter]
    fn display_info(&self, py: Python<'_>) -> Option<Py<PyLive2dJsonFile>> {
        self.display_info.as_ref().map(|value| value.clone_ref(py))
    }
}

#[pyclass(name = "Live2dDiagnostic", frozen, get_all)]
struct PyLive2dDiagnostic {
    object: (usize, i64),
    kind: String,
    message: String,
}

#[pyclass(name = "Live2dPackageSet", frozen)]
struct PyLive2dPackageSet {
    packages: Vec<Py<PyLive2dPackage>>,
    diagnostics: Vec<Py<PyLive2dDiagnostic>>,
}

#[pymethods]
impl PyLive2dPackageSet {
    #[getter]
    fn packages(&self, py: Python<'_>) -> PyResult<Vec<Py<PyLive2dPackage>>> {
        clone_python_references(py, &self.packages, "Live2D packages")
    }

    #[getter]
    fn diagnostics(&self, py: Python<'_>) -> PyResult<Vec<Py<PyLive2dDiagnostic>>> {
        clone_python_references(py, &self.diagnostics, "Live2D diagnostics")
    }
}

#[pymethods]
impl PyExportLimits {
    #[new]
    #[pyo3(signature = (*, maximum_objects=1_000_000, maximum_total_output_bytes=17_179_869_184, maximum_metadata_bytes=268_435_456))]
    const fn new(
        maximum_objects: usize,
        maximum_total_output_bytes: u64,
        maximum_metadata_bytes: u64,
    ) -> Self {
        Self {
            objects: maximum_objects,
            total_output_bytes: maximum_total_output_bytes,
            metadata_bytes: maximum_metadata_bytes,
        }
    }

    #[getter]
    const fn maximum_objects(&self) -> usize {
        self.objects
    }

    #[getter]
    const fn maximum_total_output_bytes(&self) -> u64 {
        self.total_output_bytes
    }

    #[getter]
    const fn maximum_metadata_bytes(&self) -> u64 {
        self.metadata_bytes
    }
}

#[pyclass(name = "UnityRs", frozen)]
struct PyUnityRs {
    studio: Studio,
}

/// Lazy iterator which keeps its originating `UnityRs` alive.
#[pyclass(name = "FileIterator")]
struct PyFileIterator {
    studio: Py<PyUnityRs>,
    index: usize,
}

#[pymethods]
impl PyFileIterator {
    fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    fn __next__(&mut self, py: Python<'_>) -> PyResult<Option<PyFileInfo>> {
        let item = {
            let owner = self.studio.bind(py).try_borrow()?;
            owner
                .studio
                .file(self.index)
                .map(python_file_info)
                .transpose()?
        };
        if item.is_some() {
            self.index = self
                .index
                .checked_add(1)
                .ok_or_else(|| PyValueError::new_err("file iterator index overflowed"))?;
        }
        Ok(item)
    }
}

/// Lazy iterator over the collection's serialized object-table order.
#[pyclass(name = "ObjectIterator")]
struct PyObjectIterator {
    studio: Py<PyUnityRs>,
    file_index: usize,
    object_index: usize,
}

#[pymethods]
impl PyObjectIterator {
    fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    fn __next__(&mut self, py: Python<'_>) -> PyResult<Option<PyObjectInfo>> {
        loop {
            let item = {
                let owner = self.studio.bind(py).try_borrow()?;
                let Some(file) = owner.studio.file(self.file_index) else {
                    return Ok(None);
                };
                if self.object_index >= file.object_count() {
                    None
                } else {
                    let object = owner
                        .studio
                        .object_by_index(self.file_index, self.object_index)
                        .ok_or_else(|| {
                            PyValueError::new_err(
                                "object iterator could not resolve a validated object index",
                            )
                        })?;
                    Some(python_object_info(object)?)
                }
            };
            if let Some(item) = item {
                self.object_index = self
                    .object_index
                    .checked_add(1)
                    .ok_or_else(|| PyValueError::new_err("object iterator index overflowed"))?;
                return Ok(Some(item));
            }
            self.file_index = self
                .file_index
                .checked_add(1)
                .ok_or_else(|| PyValueError::new_err("object iterator file index overflowed"))?;
            self.object_index = 0;
        }
    }
}

/// Lazy iterator over external resources in discovery order.
#[pyclass(name = "ResourceIterator")]
struct PyResourceIterator {
    studio: Py<PyUnityRs>,
    index: usize,
}

#[pymethods]
impl PyResourceIterator {
    fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    fn __next__(&mut self, py: Python<'_>) -> PyResult<Option<PyResourceInfo>> {
        let item = {
            let owner = self.studio.bind(py).try_borrow()?;
            owner
                .studio
                .resource(self.index)
                .map(python_resource_info)
                .transpose()?
        };
        if item.is_some() {
            self.index = self
                .index
                .checked_add(1)
                .ok_or_else(|| PyValueError::new_err("resource iterator index overflowed"))?;
        }
        Ok(item)
    }
}

#[pymethods]
impl PyUnityRs {
    #[new]
    #[pyo3(signature = (
        path,
        *,
        unity_version=None,
        maximum_input_files=1_000_000,
        maximum_input_directories=1_000_000,
        maximum_directory_entries=2_000_000,
        maximum_path_bytes=1_048_576,
        maximum_total_path_bytes=67_108_864,
        maximum_diagnostic_bytes=268_435_456,
        oodle_decoder=None,
        skip_unreadable_inputs=false,
        unity_cn_key=None
    ))]
    // PyO3 keyword arguments are the Python signature, so they cannot be
    // grouped into a struct without changing the public API.
    #[allow(clippy::too_many_arguments)]
    fn new(
        py: Python<'_>,
        path: PathBuf,
        unity_version: Option<String>,
        maximum_input_files: usize,
        maximum_input_directories: usize,
        maximum_directory_entries: usize,
        maximum_path_bytes: usize,
        maximum_total_path_bytes: usize,
        maximum_diagnostic_bytes: usize,
        oodle_decoder: Option<Py<PyAny>>,
        skip_unreadable_inputs: bool,
        unity_cn_key: Option<Py<PyAny>>,
    ) -> PyResult<Self> {
        let unity_version_override = parse_unity_version_override(unity_version)?;
        let oodle_decoder = python_oodle_decoder(py, oodle_decoder)?;
        let options = AssetLoadOptions {
            limits: AssetLoadLimits {
                maximum_input_files,
                maximum_input_directories,
                maximum_directory_entries,
                maximum_path_bytes,
                maximum_total_path_bytes,
                maximum_diagnostic_bytes,
                ..AssetLoadLimits::default()
            },
            unity_version_override,
            oodle_decoder,
            unity_cn_key: parse_unity_cn_key(py, unity_cn_key)?,
            failure_policy: failure_policy(skip_unreadable_inputs),
        };
        py.detach(move || Studio::open_with_options(path, options))
            .map(|studio| Self { studio })
            .map_err(core_error)
    }

    /// Opens an asset, bundle, or web container directly from Python bytes.
    #[staticmethod]
    #[pyo3(signature = (
        data,
        *,
        name="memory.assets",
        unity_version=None,
        maximum_bytes=4_294_967_296,
        maximum_path_bytes=1_048_576,
        maximum_total_path_bytes=67_108_864,
        oodle_decoder=None
    ))]
    // PyO3 keyword arguments are the Python signature, so they cannot be
    // grouped into a struct without changing the public API.
    #[allow(clippy::too_many_arguments)]
    fn from_bytes(
        py: Python<'_>,
        data: &Bound<'_, PyBytes>,
        name: &str,
        unity_version: Option<String>,
        maximum_bytes: u64,
        maximum_path_bytes: usize,
        maximum_total_path_bytes: usize,
        oodle_decoder: Option<Py<PyAny>>,
    ) -> PyResult<Self> {
        let bytes = copy_python_input(data.as_bytes(), maximum_bytes)?;
        let name = copy_python_input_name(
            name,
            maximum_path_bytes,
            maximum_total_path_bytes,
            "in-memory input name",
        )?;
        let options = AssetLoadOptions {
            limits: AssetLoadLimits {
                maximum_path_bytes,
                maximum_total_path_bytes,
                ..AssetLoadLimits::default()
            },
            unity_version_override: parse_unity_version_override(unity_version)?,
            oodle_decoder: python_oodle_decoder(py, oodle_decoder)?,
            ..AssetLoadOptions::default()
        };
        py.detach(move || {
            Studio::open_region_with_options(name, Region::from_bytes(bytes), options)
        })
        .map(|studio| Self { studio })
        .map_err(core_error)
    }

    /// Opens multiple named in-memory files as one cross-reference-capable collection.
    #[staticmethod]
    #[pyo3(signature = (
        files,
        *,
        unity_version=None,
        maximum_files=100_000,
        maximum_file_bytes=536_870_912,
        maximum_total_bytes=4_294_967_296,
        maximum_path_bytes=1_048_576,
        maximum_total_path_bytes=67_108_864,
        maximum_diagnostic_bytes=268_435_456,
        oodle_decoder=None,
        skip_unreadable_inputs=false,
        unity_cn_key=None
    ))]
    // PyO3 keyword arguments are the Python signature, so they cannot be
    // grouped into a struct without changing the public API.
    #[allow(clippy::too_many_arguments)]
    fn from_memory_files(
        py: Python<'_>,
        files: &Bound<'_, PyList>,
        unity_version: Option<String>,
        maximum_files: usize,
        maximum_file_bytes: u64,
        maximum_total_bytes: u64,
        maximum_path_bytes: usize,
        maximum_total_path_bytes: usize,
        maximum_diagnostic_bytes: usize,
        oodle_decoder: Option<Py<PyAny>>,
        skip_unreadable_inputs: bool,
        unity_cn_key: Option<Py<PyAny>>,
    ) -> PyResult<Self> {
        let inputs = copy_python_files(
            files,
            maximum_files,
            maximum_file_bytes,
            maximum_total_bytes,
            maximum_path_bytes,
            maximum_total_path_bytes,
        )?;
        let options = AssetLoadOptions {
            limits: AssetLoadLimits {
                maximum_input_files: maximum_files,
                maximum_expanded_bytes: maximum_total_bytes,
                maximum_single_entry_bytes: maximum_file_bytes,
                maximum_path_bytes,
                maximum_total_path_bytes,
                maximum_diagnostic_bytes,
                ..AssetLoadLimits::default()
            },
            unity_version_override: parse_unity_version_override(unity_version)?,
            oodle_decoder: python_oodle_decoder(py, oodle_decoder)?,
            unity_cn_key: parse_unity_cn_key(py, unity_cn_key)?,
            failure_policy: failure_policy(skip_unreadable_inputs),
        };
        py.detach(move || Studio::open_regions_with_options(inputs, options))
            .map(|studio| Self { studio })
            .map_err(core_error)
    }

    #[getter]
    fn file_count(&self) -> usize {
        self.studio.file_count()
    }

    #[getter]
    fn object_count(&self) -> usize {
        self.studio.object_count()
    }

    #[getter]
    fn resource_count(&self) -> usize {
        self.studio.resource_count()
    }

    #[getter]
    fn load_diagnostic_count(&self) -> usize {
        self.studio.load_diagnostics().len()
    }

    /// Returns a bounded page of inputs skipped by the tolerant load policy.
    #[pyo3(signature = (*, offset=0, limit=4_096))]
    fn load_diagnostic_page(
        &self,
        py: Python<'_>,
        offset: usize,
        limit: usize,
    ) -> PyResult<Vec<PyLoadDiagnostic>> {
        py.detach(|| prepare_load_diagnostic_page(&self.studio, offset, limit))
    }

    /// Returns all file metadata for convenience. Use `iter_files()` or
    /// `file_page()` for collections near the one-million-item safety limit.
    fn files(&self, py: Python<'_>) -> PyResult<Vec<PyFileInfo>> {
        py.detach(|| prepare_files(&self.studio))
    }

    /// Returns all object metadata for convenience. Use `iter_objects()` or
    /// `object_page()` for large collections.
    fn objects(&self, py: Python<'_>) -> PyResult<Vec<PyObjectInfo>> {
        py.detach(|| prepare_objects(&self.studio))
    }

    /// Returns all external resource metadata for convenience. Use
    /// `iter_resources()` or `resource_page()` for very large collections.
    fn resources(&self, py: Python<'_>) -> PyResult<Vec<PyResourceInfo>> {
        py.detach(|| prepare_resources(&self.studio))
    }

    /// Iterates file metadata without first materializing a Python list.
    fn iter_files(slf: &Bound<'_, Self>) -> PyFileIterator {
        PyFileIterator {
            studio: slf.clone().unbind(),
            index: 0,
        }
    }

    /// Iterates object metadata lazily in stable file/object-table order.
    fn iter_objects(slf: &Bound<'_, Self>) -> PyObjectIterator {
        PyObjectIterator {
            studio: slf.clone().unbind(),
            file_index: 0,
            object_index: 0,
        }
    }

    /// Iterates external resource metadata without materializing a list.
    fn iter_resources(slf: &Bound<'_, Self>) -> PyResourceIterator {
        PyResourceIterator {
            studio: slf.clone().unbind(),
            index: 0,
        }
    }

    /// Returns a bounded page of collection file metadata.
    #[pyo3(signature = (*, offset=0, limit=4_096))]
    fn file_page(&self, py: Python<'_>, offset: usize, limit: usize) -> PyResult<Vec<PyFileInfo>> {
        py.detach(|| prepare_file_page(&self.studio, offset, limit))
    }

    /// Returns a bounded page within one serialized file's object table.
    #[pyo3(signature = (file_index, *, offset=0, limit=4_096))]
    fn object_page(
        &self,
        py: Python<'_>,
        file_index: usize,
        offset: usize,
        limit: usize,
    ) -> PyResult<Vec<PyObjectInfo>> {
        py.detach(|| prepare_object_page(&self.studio, file_index, offset, limit))
    }

    /// Returns a bounded page of external resource metadata.
    #[pyo3(signature = (*, offset=0, limit=4_096))]
    fn resource_page(
        &self,
        py: Python<'_>,
        offset: usize,
        limit: usize,
    ) -> PyResult<Vec<PyResourceInfo>> {
        py.detach(|| prepare_resource_page(&self.studio, offset, limit))
    }

    /// Reads one external resource by stable collection index.
    #[pyo3(signature = (resource_index, *, maximum_bytes=536_870_912))]
    fn read_resource<'py>(
        &self,
        py: Python<'py>,
        resource_index: usize,
        maximum_bytes: u64,
    ) -> PyResult<Bound<'py, PyBytes>> {
        let resource = self.studio.resource(resource_index).ok_or_else(|| {
            PyKeyError::new_err(format!("resource index {resource_index} was not found"))
        })?;
        let bytes = py
            .detach(|| resource.read(maximum_bytes))
            .map_err(core_error)?;
        python_bytes(py, &bytes)
    }

    /// Reads one checked byte range from an external resource.
    #[pyo3(signature = (resource_index, offset, length, *, maximum_bytes=536_870_912))]
    fn read_resource_range<'py>(
        &self,
        py: Python<'py>,
        resource_index: usize,
        offset: u64,
        length: u64,
        maximum_bytes: u64,
    ) -> PyResult<Bound<'py, PyBytes>> {
        let resource = self.studio.resource(resource_index).ok_or_else(|| {
            PyKeyError::new_err(format!("resource index {resource_index} was not found"))
        })?;
        let bytes = py
            .detach(|| resource.read_range(offset, length, maximum_bytes))
            .map_err(core_error)?;
        python_bytes(py, &bytes)
    }

    /// Reads the first resource matching a portable, ASCII-insensitive path.
    #[pyo3(signature = (path, *, maximum_bytes=536_870_912))]
    fn read_resource_by_path<'py>(
        &self,
        py: Python<'py>,
        path: &str,
        maximum_bytes: u64,
    ) -> PyResult<Bound<'py, PyBytes>> {
        let resource = self.studio.resource_by_path(path).ok_or_else(|| {
            PyKeyError::new_err(format!("external resource path {path:?} was not found"))
        })?;
        let bytes = py
            .detach(|| resource.read(maximum_bytes))
            .map_err(core_error)?;
        python_bytes(py, &bytes)
    }

    /// Builds the bounded, collection-wide `GameObject` hierarchy.
    #[pyo3(signature = (*, limits=None))]
    fn scene(
        &self,
        py: Python<'_>,
        limits: Option<PyRef<'_, PySceneLimits>>,
    ) -> PyResult<Vec<PySceneNode>> {
        let limits = limits.map_or_else(SceneHierarchyLimits::default, |limits| {
            SceneHierarchyLimits::from(*limits)
        });
        py.detach(|| {
            let hierarchy = self.studio.scene_hierarchy(limits).map_err(core_error)?;
            prepare_scene_nodes(hierarchy.nodes)
        })
    }

    /// Builds general static ASCII FBX 7.4, including direct-bone skin clusters.
    #[pyo3(signature = (*, maximum_bytes=536_870_912))]
    fn read_static_fbx<'py>(
        &self,
        py: Python<'py>,
        maximum_bytes: u64,
    ) -> PyResult<Bound<'py, PyBytes>> {
        let bytes = py
            .detach(|| self.studio.read_static_fbx(maximum_bytes))
            .map_err(core_error)?;
        python_bytes(py, &bytes)
    }

    /// The same static scene in FBX 7.4's binary encoding.
    ///
    /// Some importers accept only the binary form, and it is smaller and faster
    /// to parse; the scene itself is identical to `read_static_fbx`.
    #[pyo3(signature = (*, maximum_bytes=536_870_912))]
    fn read_static_fbx_binary<'py>(
        &self,
        py: Python<'py>,
        maximum_bytes: u64,
    ) -> PyResult<Bound<'py, PyBytes>> {
        let bytes = py
            .detach(|| self.studio.read_static_fbx_binary(maximum_bytes))
            .map_err(core_error)?;
        python_bytes(py, &bytes)
    }

    /// The same animated scene in FBX 7.4's binary encoding.
    #[pyo3(signature = (*, maximum_bytes=536_870_912, acl_decoder=None))]
    fn read_fbx_binary<'py>(
        &self,
        py: Python<'py>,
        maximum_bytes: u64,
        acl_decoder: Option<Py<PyAny>>,
    ) -> PyResult<Bound<'py, PyBytes>> {
        if acl_decoder
            .as_ref()
            .is_some_and(|decoder| !decoder.bind(py).is_callable())
        {
            return Err(PyTypeError::new_err("acl_decoder must be callable"));
        }
        let bytes = py.detach(|| {
            let decoder = acl_decoder.map(|callback| PythonAclDecoder {
                callback,
                limits: AclDecodeLimits::default(),
            });
            self.studio
                .read_fbx_binary_with_acl_decoder(
                    maximum_bytes,
                    decoder.as_ref().map(|value| value as &dyn AclDecoder),
                )
                .map_err(core_error)
        })?;
        python_bytes(py, &bytes)
    }

    /// Builds ASCII FBX 7.4 with supported bound animations and an optional
    /// caller-supplied Tuanjie ACL decoder.
    #[pyo3(signature = (*, maximum_bytes=536_870_912, acl_decoder=None))]
    fn read_fbx<'py>(
        &self,
        py: Python<'py>,
        maximum_bytes: u64,
        acl_decoder: Option<Py<PyAny>>,
    ) -> PyResult<Bound<'py, PyBytes>> {
        if acl_decoder
            .as_ref()
            .is_some_and(|decoder| !decoder.bind(py).is_callable())
        {
            return Err(PyTypeError::new_err("acl_decoder must be callable"));
        }
        let bytes = py.detach(|| {
            let decoder = acl_decoder.map(|callback| PythonAclDecoder {
                callback,
                limits: AclDecodeLimits::default(),
            });
            self.studio
                .read_fbx_with_acl_decoder(
                    maximum_bytes,
                    decoder.as_ref().map(|value| value as &dyn AclDecoder),
                )
                .map_err(core_error)
        })?;
        python_bytes(py, &bytes)
    }

    /// Enumerates managed-compatible `SplitObjects` FBX roots.
    fn split_object_fbx_candidates(&self, py: Python<'_>) -> PyResult<Vec<PyFbxCandidate>> {
        py.detach(|| {
            let candidates = self
                .studio
                .split_object_fbx_candidates(ModelExportPlanLimits::default())
                .map_err(core_error)?;
            python_fbx_candidates(candidates, "SplitObjects FBX candidates")
        })
    }

    /// Enumerates Animator-owned FBX roots.
    fn animator_fbx_candidates(&self, py: Python<'_>) -> PyResult<Vec<PyFbxCandidate>> {
        py.detach(|| {
            let candidates = self
                .studio
                .animator_fbx_candidates(ModelExportPlanLimits::default())
                .map_err(core_error)?;
            python_fbx_candidates(candidates, "Animator FBX candidates")
        })
    }

    /// Materializes one selected `GameObject` FBX branch.
    #[pyo3(signature = (
        file_index,
        path_id,
        *,
        include_animations=true,
        maximum_bytes=536_870_912,
        acl_decoder=None
    ))]
    fn read_game_object_fbx<'py>(
        &self,
        py: Python<'py>,
        file_index: usize,
        path_id: i64,
        include_animations: bool,
        maximum_bytes: u64,
        acl_decoder: Option<Py<PyAny>>,
    ) -> PyResult<Bound<'py, PyBytes>> {
        if acl_decoder
            .as_ref()
            .is_some_and(|decoder| !decoder.bind(py).is_callable())
        {
            return Err(PyTypeError::new_err("acl_decoder must be callable"));
        }
        let bytes = py
            .detach(|| {
                let decoder = acl_decoder.map(|callback| PythonAclDecoder {
                    callback,
                    limits: AclDecodeLimits::default(),
                });
                self.studio.read_game_object_fbx_with_acl_decoder(
                    SceneObjectKey {
                        file_index,
                        path_id,
                    },
                    include_animations,
                    maximum_bytes,
                    decoder.as_ref().map(|value| value as &dyn AclDecoder),
                )
            })
            .map_err(core_error)?;
        python_bytes(py, &bytes)
    }

    #[pyo3(signature = (file_index, path_id, *, maximum_bytes=536_870_912))]
    fn read_raw<'py>(
        &self,
        py: Python<'py>,
        file_index: usize,
        path_id: i64,
        maximum_bytes: u64,
    ) -> PyResult<Bound<'py, PyBytes>> {
        let bytes = py.detach(|| {
            self.object(file_index, path_id)?
                .read_raw(maximum_bytes)
                .map_err(core_error)
        })?;
        python_bytes(py, &bytes)
    }

    #[pyo3(signature = (file_index, path_id, *, maximum_bytes=536_870_912))]
    fn read_text<'py>(
        &self,
        py: Python<'py>,
        file_index: usize,
        path_id: i64,
        maximum_bytes: u64,
    ) -> PyResult<Bound<'py, PyBytes>> {
        let maximum_bytes = usize::try_from(maximum_bytes)
            .map_err(|_| PyValueError::new_err("maximum_bytes does not fit this platform"))?;
        let bytes = py.detach(|| {
            self.object(file_index, path_id)?
                .read_text_bytes(maximum_bytes)
                .map_err(core_error)
        })?;
        python_bytes(py, &bytes)
    }

    /// Converts one Unity `Shader` to UnityRs's bounded text payload.
    #[pyo3(signature = (file_index, path_id, *, maximum_bytes=536_870_912))]
    fn read_shader<'py>(
        &self,
        py: Python<'py>,
        file_index: usize,
        path_id: i64,
        maximum_bytes: u64,
    ) -> PyResult<Bound<'py, PyBytes>> {
        let bytes = py.detach(|| {
            self.object(file_index, path_id)?
                .read_shader_text(maximum_bytes)
                .map_err(core_error)
        })?;
        python_bytes(py, &bytes)
    }

    /// Writes the whole scene as one Wavefront OBJ, with the material library
    /// it names and that library's textures.
    ///
    /// Distinct from `read_mesh_obj`, which writes one mesh the way the
    /// managed exporter does. This is the scene: every renderer placed in
    /// world space, and face references naming only the channels a mesh has.
    ///
    /// `material_library_name` is what the OBJ's `mtllib` line will say, so it
    /// has to be the name the library is actually written under.
    #[pyo3(signature = (
        *,
        material_library_name="model.mtl",
        texture_format="png",
        maximum_bytes=536_870_912,
        texture_limits=None
    ))]
    fn read_model_obj(
        &self,
        py: Python<'_>,
        material_library_name: &str,
        texture_format: &str,
        maximum_bytes: u64,
        texture_limits: Option<PyRef<'_, PyModelTextureLimits>>,
    ) -> PyResult<PyModelObj> {
        let texture_format = parse_image_format(texture_format)?;
        let texture_limits = texture_limits.map_or_else(SceneTextureLimits::default, |limits| {
            SceneTextureLimits::from(*limits)
        });
        let (model, skipped) = py.detach(|| {
            let mut model = self
                .studio
                .read_model_obj(
                    material_library_name,
                    maximum_bytes,
                    texture_format,
                    texture_limits,
                )
                .map_err(core_error)?;
            let skipped = skipped_textures(std::mem::take(&mut model.textures.skipped))?;
            Ok::<_, PyErr>((model, skipped))
        })?;
        let textures = model_files(py, model.textures.textures)?;
        Ok(PyModelObj {
            obj: model.obj,
            material_library_name: model.material_library_name,
            material_library: model.material_library,
            textures,
            skipped,
        })
    }

    /// Writes the scene as ASCII FBX with its animations, and returns the
    /// material textures it references.
    ///
    /// The FBX names each texture by file name, so they have to be written
    /// beside it for those references to resolve. They come back rather than
    /// being written because this call has no directory of its own.
    #[pyo3(signature = (
        *,
        texture_format="png",
        maximum_bytes=536_870_912,
        texture_limits=None
    ))]
    fn read_fbx_with_textures(
        &self,
        py: Python<'_>,
        texture_format: &str,
        maximum_bytes: u64,
        texture_limits: Option<PyRef<'_, PyModelTextureLimits>>,
    ) -> PyResult<PyTexturedFbx> {
        let maximum = usize::try_from(maximum_bytes)
            .map_err(|_| PyValueError::new_err("maximum_bytes does not fit this platform"))?;
        let texture_format = parse_image_format(texture_format)?;
        let texture_limits = texture_limits.map_or_else(SceneTextureLimits::default, |limits| {
            SceneTextureLimits::from(*limits)
        });
        let (fbx, textures, skipped) = py.detach(|| {
            let (fbx, mut textures) =
                materialize_python_output(maximum, "ASCII FBX with textures", |output| {
                    self.studio.write_fbx_with_textures(
                        output,
                        maximum_bytes,
                        texture_format,
                        texture_limits,
                    )
                })?;
            let skipped = skipped_textures(std::mem::take(&mut textures.skipped))?;
            Ok::<_, PyErr>((fbx, textures, skipped))
        })?;
        let texture_files = model_files(py, textures.textures)?;
        Ok(PyTexturedFbx {
            fbx,
            textures: texture_files,
            skipped,
        })
    }

    /// Reads one supported resident or externally streamed Unity `Mesh` as
    /// managed-compatible OBJ.
    #[pyo3(signature = (file_index, path_id, *, maximum_bytes=536_870_912))]
    fn read_mesh_obj<'py>(
        &self,
        py: Python<'py>,
        file_index: usize,
        path_id: i64,
        maximum_bytes: u64,
    ) -> PyResult<Bound<'py, PyBytes>> {
        let maximum_usize = usize::try_from(maximum_bytes)
            .map_err(|_| PyValueError::new_err("maximum_bytes does not fit this platform"))?;
        let limits = MeshReadLimits {
            maximum_object_bytes: maximum_bytes,
            maximum_vertex_data_bytes: maximum_usize,
            maximum_compressed_data_bytes: maximum_bytes,
            maximum_auxiliary_bytes: maximum_bytes,
            maximum_decoded_bytes: maximum_bytes,
            maximum_output_bytes: maximum_bytes,
            ..MeshReadLimits::default()
        };
        let bytes = py.detach(|| {
            self.object(file_index, path_id)?
                .read_mesh_obj(limits)
                .map_err(core_error)
        })?;
        python_bytes(py, &bytes)
    }

    /// Parses bounded curve, muscle, ACL, and streaming metadata from one
    /// Unity or Tuanjie `AnimationClip`.
    #[pyo3(signature = (file_index, path_id, *, maximum_bytes=536_870_912))]
    fn read_animation_clip(
        &self,
        py: Python<'_>,
        file_index: usize,
        path_id: i64,
        maximum_bytes: u64,
    ) -> PyResult<PyAnimationClip> {
        let limits = AnimationClipReadLimits {
            maximum_object_bytes: maximum_bytes,
            maximum_packed_bytes: maximum_bytes,
            maximum_total_packed_bytes: maximum_bytes,
            maximum_reference_bytes: maximum_bytes,
            maximum_total_allocation_bytes: maximum_bytes,
            ..AnimationClipReadLimits::default()
        };
        let clip = py.detach(|| {
            self.object(file_index, path_id)?
                .read_animation_clip(limits)
                .map_err(core_error)
        })?;
        let muscle = clip.muscle_clip.as_ref();
        let acl = muscle.and_then(|value| value.clip.acl.as_ref());
        let streaming = clip.streaming_info.as_ref();
        let state_bits = u8::from(clip.legacy)
            | (u8::from(clip.compressed) << 1)
            | (u8::from(clip.use_high_quality_curve) << 2)
            | (u8::from(muscle.is_some()) << 3)
            | (u8::from(acl.is_some()) << 4);
        let streaming_path = streaming
            .map(|value| try_copy_string(&value.path, "AnimationClip streaming path"))
            .transpose()?;
        Ok(PyAnimationClip {
            path_id: clip.path_id,
            name: clip.name,
            state_bits,
            sample_rate: clip.sample_rate,
            wrap_mode: clip.wrap_mode,
            rotation_curve_count: clip.rotation_curves.len(),
            euler_curve_count: clip.euler_curves.len(),
            position_curve_count: clip.position_curves.len(),
            scale_curve_count: clip.scale_curves.len(),
            float_curve_count: clip.float_curves.len(),
            pptr_curve_count: clip.pptr_curves.len(),
            muscle_clip_size: clip.muscle_clip_size,
            streamed_curve_count: muscle.map(|value| value.clip.streamed.curve_count),
            dense_curve_count: muscle.map(|value| value.clip.dense.curve_count),
            constant_value_count: muscle.map(|value| value.clip.constant.values.count),
            acl_frame_count: acl.map(|value| value.frame_count),
            acl_bone_count: acl.map(|value| value.bone_count),
            acl_sample_rate: acl.map(unity_rs_core::animation_clip::AclClip::sample_rate),
            acl_curve_count: acl.and_then(|value| value.curve_count),
            acl_track_byte_count: acl.map(|value| value.tracks.byte_length),
            acl_decoder_count: acl.map(|value| value.decoder_map.count),
            acl_use_fast_sample_mode: acl.and_then(|value| value.use_fast_sample_mode),
            streaming_offset: streaming.map(|value| value.offset),
            streaming_size: streaming.map(|value| value.size),
            streaming_path,
        })
    }

    /// Parses the stable `GameObject`, default clip, and ordered clip table
    /// from one legacy Unity `Animation` component.
    #[pyo3(signature = (file_index, path_id, *, maximum_bytes=268_435_456))]
    fn read_legacy_animation(
        &self,
        py: Python<'_>,
        file_index: usize,
        path_id: i64,
        maximum_bytes: u64,
    ) -> PyResult<PyLegacyAnimation> {
        let limits = animation_component_limits(maximum_bytes)?;
        py.detach(|| {
            let animation = self
                .object(file_index, path_id)?
                .read_legacy_animation(limits)
                .map_err(core_error)?;
            prepare_legacy_animation(animation)
        })
    }

    /// Parses one bounded `AnimatorOverrideController` controller reference
    /// and ordered `(original, replacement)` clip table.
    #[pyo3(signature = (file_index, path_id, *, maximum_bytes=268_435_456))]
    fn read_animator_override_controller(
        &self,
        py: Python<'_>,
        file_index: usize,
        path_id: i64,
        maximum_bytes: u64,
    ) -> PyResult<PyAnimatorOverrideController> {
        let limits = animation_component_limits(maximum_bytes)?;
        py.detach(|| {
            let controller = self
                .object(file_index, path_id)?
                .read_animator_override_controller(limits)
                .map_err(core_error)?;
            prepare_animator_override_controller(controller)
        })
    }

    /// Parses one bounded Unity `AssetBundle` preload and named-container table.
    #[pyo3(signature = (
        file_index,
        path_id,
        *,
        maximum_entries=1_000_000,
        maximum_string_bytes=16_777_216,
        maximum_total_string_bytes=67_108_864
    ))]
    fn read_asset_bundle(
        &self,
        py: Python<'_>,
        file_index: usize,
        path_id: i64,
        maximum_entries: usize,
        maximum_string_bytes: usize,
        maximum_total_string_bytes: usize,
    ) -> PyResult<PyAssetBundle> {
        let limits = container_metadata_limits(
            maximum_entries,
            maximum_string_bytes,
            maximum_total_string_bytes,
        );
        py.detach(|| {
            let object = self.object(file_index, path_id)?;
            let path_id = object.path_id();
            let bundle = object.read_asset_bundle(limits).map_err(core_error)?;
            prepare_asset_bundle(path_id, bundle)
        })
    }

    /// Parses one bounded Unity `ResourceManager` named-container table.
    #[pyo3(signature = (
        file_index,
        path_id,
        *,
        maximum_entries=1_000_000,
        maximum_string_bytes=16_777_216,
        maximum_total_string_bytes=67_108_864
    ))]
    fn read_resource_manager(
        &self,
        py: Python<'_>,
        file_index: usize,
        path_id: i64,
        maximum_entries: usize,
        maximum_string_bytes: usize,
        maximum_total_string_bytes: usize,
    ) -> PyResult<PyResourceManager> {
        let limits = container_metadata_limits(
            maximum_entries,
            maximum_string_bytes,
            maximum_total_string_bytes,
        );
        py.detach(|| {
            let object = self.object(file_index, path_id)?;
            let path_id = object.path_id();
            let manager = object.read_resource_manager(limits).map_err(core_error)?;
            prepare_resource_manager(path_id, manager)
        })
    }

    /// Parses one bounded Unity `PreloadData` object-reference table.
    #[pyo3(signature = (file_index, path_id, *, maximum_entries=1_000_000))]
    fn read_preload_data(
        &self,
        py: Python<'_>,
        file_index: usize,
        path_id: i64,
        maximum_entries: usize,
    ) -> PyResult<PyPreloadData> {
        let limits = container_metadata_limits(
            maximum_entries,
            ContainerMetadataReadLimits::default().maximum_string_bytes,
            ContainerMetadataReadLimits::default().maximum_total_string_bytes,
        );
        py.detach(|| {
            let object = self.object(file_index, path_id)?;
            let path_id = object.path_id();
            let preload = object.read_preload_data(limits).map_err(core_error)?;
            prepare_preload_data(path_id, preload)
        })
    }

    /// Validates and inspects the official ACL 2.x outer track container used
    /// by one Tuanjie `AnimationClip`. This does not decompress ACL samples.
    #[pyo3(signature = (
        file_index,
        path_id,
        *,
        maximum_bytes=536_870_912,
        maximum_decompressed_values=134_217_728
    ))]
    fn inspect_acl_tracks(
        &self,
        py: Python<'_>,
        file_index: usize,
        path_id: i64,
        maximum_bytes: u64,
        maximum_decompressed_values: u64,
    ) -> PyResult<PyAclCompressedTracks> {
        let read_limits = AnimationClipReadLimits {
            maximum_object_bytes: maximum_bytes,
            maximum_packed_bytes: maximum_bytes,
            maximum_total_packed_bytes: maximum_bytes,
            maximum_reference_bytes: maximum_bytes,
            maximum_total_allocation_bytes: maximum_bytes,
            ..AnimationClipReadLimits::default()
        };
        let tracks = py.detach(|| {
            let clip = self
                .object(file_index, path_id)?
                .read_animation_clip(read_limits)
                .map_err(core_error)?;
            let acl = clip
                .muscle_clip
                .as_ref()
                .and_then(|value| value.clip.acl.as_ref())
                .ok_or_else(|| {
                    PyValueError::new_err("AnimationClip does not contain ACL tracks")
                })?;
            acl.inspect_compressed_tracks(AclCompressedTracksLimits {
                maximum_compressed_bytes: maximum_bytes,
                maximum_tracks: 2_000_000,
                maximum_samples_per_track: 2_000_000,
                maximum_decompressed_values,
            })
            .map_err(core_error)
        })?;
        Ok(PyAclCompressedTracks {
            declared_size: tracks.declared_size,
            stored_hash: tracks.stored_hash,
            version: tracks.version,
            track_type: tracks.track_type.name().to_owned(),
            num_tracks: tracks.num_tracks,
            num_samples_per_track: tracks.num_samples_per_track,
            sample_rate: tracks.sample_rate(),
            decompressed_value_count: tracks.decompressed_value_count,
            state_bits: u8::from(tracks.has_metadata())
                | (u8::from(tracks.is_wrap_optimized()) << 1)
                | (u8::from(tracks.has_database()) << 2)
                | (u8::from(tracks.has_stripped_keyframes()) << 3),
        })
    }

    /// Returns the validated compressed ACL blob and Tuanjie decoder map for
    /// a caller-supplied decoder. No project-specific C ABI is involved.
    #[pyo3(signature = (
        file_index,
        path_id,
        *,
        maximum_bytes=536_870_912,
        maximum_decoder_map_entries=2_000_000,
        maximum_materialized_bytes=536_870_912
    ))]
    fn read_acl_decoder_input<'py>(
        &self,
        py: Python<'py>,
        file_index: usize,
        path_id: i64,
        maximum_bytes: u64,
        maximum_decoder_map_entries: usize,
        maximum_materialized_bytes: u64,
    ) -> PyResult<(Bound<'py, PyBytes>, Vec<u32>)> {
        let read_limits = AnimationClipReadLimits {
            maximum_object_bytes: maximum_bytes,
            maximum_packed_bytes: maximum_bytes,
            maximum_total_packed_bytes: maximum_bytes,
            maximum_reference_bytes: maximum_bytes,
            maximum_total_allocation_bytes: maximum_bytes,
            ..AnimationClipReadLimits::default()
        };
        let input = py.detach(|| {
            let clip = self
                .object(file_index, path_id)?
                .read_animation_clip(read_limits)
                .map_err(core_error)?;
            let acl = clip
                .muscle_clip
                .as_ref()
                .and_then(|value| value.clip.acl.as_ref())
                .ok_or_else(|| {
                    PyValueError::new_err("AnimationClip does not contain ACL tracks")
                })?;
            acl.materialize_decoder_input(AclDecoderInputLimits {
                compressed_tracks: AclCompressedTracksLimits {
                    maximum_compressed_bytes: maximum_bytes,
                    ..AclCompressedTracksLimits::default()
                },
                maximum_decoder_map_entries,
                maximum_materialized_bytes,
            })
            .map_err(core_error)
        })?;
        Ok((
            python_bytes(py, &input.compressed_tracks)?,
            input.decoder_map,
        ))
    }

    /// Runs a caller-supplied ACL decoder and validates every returned time,
    /// binding index, value, shape, and output budget in Rust.
    ///
    /// The callable receives `(compressed_tracks, decoder_map, frame_count,
    /// bone_count, sample_rate, declared_curve_count, use_fast_sample_mode)`
    /// and returns `(times, binding_indices, frame_major_values,
    /// following_curve_offset)`.
    #[pyo3(signature = (
        file_index,
        path_id,
        decoder,
        *,
        maximum_bytes=536_870_912,
        maximum_values=134_217_728
    ))]
    fn decode_acl_tracks(
        &self,
        py: Python<'_>,
        file_index: usize,
        path_id: i64,
        decoder: Py<PyAny>,
        maximum_bytes: u64,
        maximum_values: usize,
    ) -> PyResult<PyAclDecodedClip> {
        if !decoder.bind(py).is_callable() {
            return Err(PyTypeError::new_err("decoder must be callable"));
        }
        let read_limits = AnimationClipReadLimits {
            maximum_object_bytes: maximum_bytes,
            maximum_packed_bytes: maximum_bytes,
            maximum_total_packed_bytes: maximum_bytes,
            maximum_reference_bytes: maximum_bytes,
            maximum_total_allocation_bytes: maximum_bytes,
            ..AnimationClipReadLimits::default()
        };
        let output = py.detach(|| {
            let clip = self
                .object(file_index, path_id)?
                .read_animation_clip(read_limits)
                .map_err(core_error)?;
            let acl = clip
                .muscle_clip
                .as_ref()
                .and_then(|value| value.clip.acl.as_ref())
                .ok_or_else(|| {
                    PyValueError::new_err("AnimationClip does not contain ACL tracks")
                })?;
            let decode_limits = AclDecodeLimits {
                input: AclDecoderInputLimits {
                    compressed_tracks: AclCompressedTracksLimits {
                        maximum_compressed_bytes: maximum_bytes,
                        ..AclCompressedTracksLimits::default()
                    },
                    maximum_decoder_map_entries: 2_000_000,
                    maximum_materialized_bytes: maximum_bytes,
                },
                maximum_values,
                ..AclDecodeLimits::default()
            };
            acl.decode_with(
                &PythonAclDecoder {
                    callback: decoder,
                    limits: decode_limits,
                },
                decode_limits,
            )
            .map_err(core_error)
        })?;
        Ok(PyAclDecodedClip {
            times: output.times,
            binding_indices: output.binding_indices,
            values: output.values,
            following_curve_offset: output.following_curve_offset,
        })
    }

    /// Parses one complete Unity or Tuanjie `AnimatorController` and returns
    /// its stable controller, TOS, and clip-reference metadata.
    #[pyo3(signature = (file_index, path_id, *, maximum_bytes=536_870_912))]
    fn read_animator_controller(
        &self,
        py: Python<'_>,
        file_index: usize,
        path_id: i64,
        maximum_bytes: u64,
    ) -> PyResult<PyAnimatorController> {
        let maximum_usize = usize::try_from(maximum_bytes)
            .map_err(|_| PyValueError::new_err("maximum_bytes does not fit this platform"))?;
        let limits = AnimatorControllerReadLimits {
            maximum_object_bytes: maximum_bytes,
            maximum_string_bytes: maximum_usize,
            maximum_total_string_bytes: maximum_usize,
            maximum_reference_bytes: maximum_bytes,
            maximum_total_allocation_bytes: maximum_bytes,
            ..AnimatorControllerReadLimits::default()
        };
        py.detach(|| {
            let controller = self
                .object(file_index, path_id)?
                .read_animator_controller(limits)
                .map_err(core_error)?;
            prepare_animator_controller(controller)
        })
    }

    /// Parses one complete Unity or Tuanjie `Avatar` and returns its stable
    /// skeleton, TOS, and human-description metadata.
    #[pyo3(signature = (file_index, path_id, *, maximum_bytes=536_870_912))]
    fn read_avatar(
        &self,
        py: Python<'_>,
        file_index: usize,
        path_id: i64,
        maximum_bytes: u64,
    ) -> PyResult<PyAvatar> {
        let maximum_usize = usize::try_from(maximum_bytes)
            .map_err(|_| PyValueError::new_err("maximum_bytes does not fit this platform"))?;
        let limits = AvatarReadLimits {
            maximum_object_bytes: maximum_bytes,
            maximum_string_bytes: maximum_usize,
            maximum_total_string_bytes: maximum_usize,
            maximum_total_allocation_bytes: maximum_bytes,
            maximum_reference_bytes: maximum_bytes,
            ..AvatarReadLimits::default()
        };
        py.detach(|| {
            let avatar = self
                .object(file_index, path_id)?
                .read_avatar(limits)
                .map_err(core_error)?;
            prepare_avatar(avatar)
        })
    }

    #[pyo3(signature = (file_index, path_id, *, pretty=false, maximum_bytes=268_435_456))]
    fn read_type_tree_json(
        &self,
        py: Python<'_>,
        file_index: usize,
        path_id: i64,
        pretty: bool,
        maximum_bytes: usize,
    ) -> PyResult<String> {
        let bytes = py.detach(|| {
            self.object(file_index, path_id)?
                .read_type_tree_json(pretty, maximum_bytes)
                .map_err(core_error)
        })?;
        String::from_utf8(bytes)
            .map_err(|error| PyValueError::new_err(format!("JSON is not UTF-8: {error}")))
    }

    /// Reads the managed-compatible, tab-indented CRLF `TypeTree` dump.
    #[pyo3(signature = (file_index, path_id, *, maximum_bytes=268_435_456))]
    fn read_type_tree_dump(
        &self,
        py: Python<'_>,
        file_index: usize,
        path_id: i64,
        maximum_bytes: u64,
    ) -> PyResult<String> {
        let bytes = py.detach(|| {
            self.object(file_index, path_id)?
                .read_type_tree_dump(maximum_bytes)
                .map_err(core_error)
        })?;
        String::from_utf8(bytes)
            .map_err(|error| PyValueError::new_err(format!("TypeTree dump is not UTF-8: {error}")))
    }

    /// Reads a stripped `MonoBehaviour` with a trusted full-object schema.
    #[pyo3(signature = (
        file_index,
        path_id,
        schema,
        *,
        pretty=false,
        maximum_bytes=268_435_456
    ))]
    fn read_mono_behaviour_json(
        &self,
        py: Python<'_>,
        file_index: usize,
        path_id: i64,
        schema: PyRef<'_, PyMonoBehaviourSchema>,
        pretty: bool,
        maximum_bytes: usize,
    ) -> PyResult<PyMonoBehaviourJson> {
        let registry = Arc::clone(&schema.registry);
        drop(schema);
        let limits = MonoBehaviourReadLimits {
            maximum_json_bytes: maximum_bytes,
            ..MonoBehaviourReadLimits::default()
        };
        let resolved = py.detach(move || {
            self.object(file_index, path_id)?
                .read_mono_behaviour_json(registry.as_ref(), pretty, limits)
                .map_err(core_error)
        })?;
        Ok(PyMonoBehaviourJson {
            json: resolved.json,
            source: schema_source_name(resolved.source).to_owned(),
        })
    }

    /// Reads a stripped `MonoBehaviour` using a reusable schema collection.
    #[pyo3(signature = (
        file_index,
        path_id,
        schemas,
        *,
        pretty=false,
        maximum_bytes=268_435_456
    ))]
    fn read_mono_behaviour_json_with_schemas(
        &self,
        py: Python<'_>,
        file_index: usize,
        path_id: i64,
        schemas: PyRef<'_, PyMonoBehaviourSchemas>,
        pretty: bool,
        maximum_bytes: usize,
    ) -> PyResult<PyMonoBehaviourJson> {
        let provider = Arc::clone(&schemas.provider);
        drop(schemas);
        let limits = MonoBehaviourReadLimits {
            maximum_json_bytes: maximum_bytes,
            ..MonoBehaviourReadLimits::default()
        };
        let resolved = py.detach(move || {
            self.object(file_index, path_id)?
                .read_mono_behaviour_json(provider.as_ref(), pretty, limits)
                .map_err(core_error)
        })?;
        Ok(PyMonoBehaviourJson {
            json: resolved.json,
            source: schema_source_name(resolved.source).to_owned(),
        })
    }

    #[pyo3(signature = (file_index, path_id, *, mip_level=0, maximum_bytes=536_870_912))]
    fn read_texture(
        &self,
        py: Python<'_>,
        file_index: usize,
        path_id: i64,
        mip_level: u32,
        maximum_bytes: u64,
    ) -> PyResult<PyRgbaImage> {
        let limits = TextureReadLimits {
            maximum_payload_bytes: maximum_bytes,
            maximum_output_bytes: maximum_bytes,
            maximum_decoder_working_bytes: maximum_bytes,
            ..TextureReadLimits::default()
        };
        let image = py.detach(|| {
            let image = self
                .object(file_index, path_id)?
                .decode_texture_mip(mip_level, limits)
                .map_err(core_error)?;
            DisplayRowPyImage::from_decoded(image)
        })?;
        Ok(image.into_python())
    }

    #[pyo3(signature = (file_index, path_id, *, maximum_bytes=536_870_912))]
    fn read_texture_array(
        &self,
        py: Python<'_>,
        file_index: usize,
        path_id: i64,
        maximum_bytes: u64,
    ) -> PyResult<Vec<PyRgbaImage>> {
        let limits = TextureArrayReadLimits {
            maximum_payload_bytes: maximum_bytes,
            maximum_output_bytes: maximum_bytes,
            maximum_decoder_working_bytes: maximum_bytes,
            maximum_bundle_bytes: maximum_bytes,
            ..TextureArrayReadLimits::default()
        };
        let images = py.detach(|| {
            let images = self
                .object(file_index, path_id)?
                .decode_texture_array_mip0(limits)
                .map_err(core_error)?;
            DisplayRowPyImages::from_decoded(images)
        })?;
        images.into_python()
    }

    /// Parses one complete, bounded Unity `SpriteAtlas` metadata table.
    #[pyo3(signature = (
        file_index,
        path_id,
        *,
        maximum_entries=1_000_000,
        maximum_string_bytes=16_777_216,
        maximum_total_string_bytes=33_554_432
    ))]
    fn read_sprite_atlas(
        &self,
        py: Python<'_>,
        file_index: usize,
        path_id: i64,
        maximum_entries: usize,
        maximum_string_bytes: usize,
        maximum_total_string_bytes: usize,
    ) -> PyResult<PySpriteAtlas> {
        let limits = SpriteAtlasReadLimits {
            maximum_string_bytes,
            maximum_total_string_bytes,
            maximum_packed_sprites: maximum_entries,
            maximum_packed_sprite_names: maximum_entries,
            maximum_render_data_entries: maximum_entries,
            maximum_secondary_textures: maximum_entries,
        };
        let atlas = py.detach(|| {
            let atlas = self
                .object(file_index, path_id)?
                .read_sprite_atlas(limits)
                .map_err(core_error)?;
            prepare_sprite_atlas(atlas)
        })?;
        python_sprite_atlas(py, atlas)
    }

    /// Parses one complete, bounded Unity `Sprite` without resolving or
    /// decoding its texture references.
    #[pyo3(signature = (file_index, path_id, *, limits=None))]
    fn read_sprite_metadata(
        &self,
        py: Python<'_>,
        file_index: usize,
        path_id: i64,
        limits: Option<PyRef<'_, PySpriteMetadataLimits>>,
    ) -> PyResult<PySpriteMetadata> {
        let limits = limits.map_or_else(SpriteReadLimits::default, |limits| (*limits).into());
        let sprite = py.detach(|| {
            self.object(file_index, path_id)?
                .read_sprite(limits)
                .map_err(core_error)
        })?;
        convert_sprite_metadata(py, sprite)
    }

    #[pyo3(signature = (file_index, path_id, *, maximum_bytes=536_870_912))]
    fn read_sprite(
        &self,
        py: Python<'_>,
        file_index: usize,
        path_id: i64,
        maximum_bytes: u64,
    ) -> PyResult<PyRgbaImage> {
        let sprite_limits = SpriteReadLimits {
            maximum_mesh_bytes: maximum_bytes,
            maximum_output_pixels: maximum_bytes / 4,
            maximum_output_bytes: maximum_bytes,
            maximum_working_bytes: maximum_bytes,
            ..SpriteReadLimits::default()
        };
        let texture_limits = TextureReadLimits {
            maximum_payload_bytes: maximum_bytes,
            maximum_output_bytes: maximum_bytes,
            maximum_decoder_working_bytes: maximum_bytes,
            ..TextureReadLimits::default()
        };
        let image = py.detach(|| {
            self.object(file_index, path_id)?
                .decode_sprite(sprite_limits, texture_limits)
                .map_err(core_error)
        })?;
        Ok(PyRgbaImage {
            width: image.width,
            height: image.height,
            pixels: image.pixels,
        })
    }

    #[pyo3(signature = (file_index, path_id, *, format="auto", maximum_bytes=536_870_912))]
    fn read_audio_clip(
        &self,
        py: Python<'_>,
        file_index: usize,
        path_id: i64,
        format: &str,
        maximum_bytes: u64,
    ) -> PyResult<PyAudioClip> {
        let format = parse_audio_format(format)?;
        let limits = SimpleAssetReadLimits {
            maximum_payload_bytes: maximum_bytes,
            ..SimpleAssetReadLimits::default()
        };
        let audio = py.detach(|| {
            let audio = self
                .object(file_index, path_id)?
                .read_audio_clip(limits)
                .map_err(core_error)?;
            materialize_audio_clip(audio, format, maximum_bytes)
        })?;
        Ok(audio)
    }

    /// Reads the embedded font program rather than the serialized `Font` wrapper.
    #[pyo3(signature = (file_index, path_id, *, maximum_bytes=536_870_912))]
    fn read_font(
        &self,
        py: Python<'_>,
        file_index: usize,
        path_id: i64,
        maximum_bytes: u64,
    ) -> PyResult<PyBinaryAsset> {
        let limits = simple_asset_limits(maximum_bytes);
        let asset = py.detach(|| {
            let asset = self
                .object(file_index, path_id)?
                .read_font(limits)
                .map_err(core_error)?;
            materialize_binary_asset(asset, maximum_bytes)
        })?;
        Ok(asset)
    }

    /// Reads the resident Ogg payload from a legacy `MovieTexture`.
    #[pyo3(signature = (file_index, path_id, *, maximum_bytes=536_870_912))]
    fn read_movie_texture(
        &self,
        py: Python<'_>,
        file_index: usize,
        path_id: i64,
        maximum_bytes: u64,
    ) -> PyResult<PyBinaryAsset> {
        let limits = simple_asset_limits(maximum_bytes);
        let asset = py.detach(|| {
            let asset = self
                .object(file_index, path_id)?
                .read_movie_texture(limits)
                .map_err(core_error)?;
            materialize_binary_asset(asset, maximum_bytes)
        })?;
        Ok(asset)
    }

    /// Reads one inline or externally streamed `VideoClip` payload.
    #[pyo3(signature = (file_index, path_id, *, maximum_bytes=536_870_912))]
    fn read_video_clip(
        &self,
        py: Python<'_>,
        file_index: usize,
        path_id: i64,
        maximum_bytes: u64,
    ) -> PyResult<PyBinaryAsset> {
        let limits = simple_asset_limits(maximum_bytes);
        let asset = py.detach(|| {
            let asset = self
                .object(file_index, path_id)?
                .read_video_clip(limits)
                .map_err(core_error)?;
            materialize_binary_asset(asset, maximum_bytes)
        })?;
        Ok(asset)
    }

    /// Reads one `Material` while preserving property order and duplicate names.
    #[pyo3(signature = (
        file_index,
        path_id,
        *,
        maximum_object_bytes=268_435_456,
        maximum_string_bytes=16_777_216,
        maximum_array_elements=1_000_000
    ))]
    fn read_material(
        &self,
        py: Python<'_>,
        file_index: usize,
        path_id: i64,
        maximum_object_bytes: u64,
        maximum_string_bytes: usize,
        maximum_array_elements: usize,
    ) -> PyResult<PyMaterial> {
        let limits = MaterialReadLimits {
            maximum_object_bytes,
            maximum_string_bytes,
            maximum_total_string_bytes: maximum_string_bytes
                .checked_mul(maximum_array_elements.max(1))
                .unwrap_or(usize::MAX)
                .min(MaterialReadLimits::default().maximum_total_string_bytes),
            maximum_array_elements,
        };
        py.detach(|| {
            let material = self
                .object(file_index, path_id)?
                .read_material(limits)
                .map_err(core_error)?;
            convert_material(material)
        })
    }

    /// Reads one `MonoScript` identity without loading its managed assembly.
    #[pyo3(signature = (file_index, path_id, *, maximum_string_bytes=16_777_216))]
    fn read_mono_script(
        &self,
        py: Python<'_>,
        file_index: usize,
        path_id: i64,
        maximum_string_bytes: usize,
    ) -> PyResult<PyMonoScript> {
        let defaults = MonoBehaviourReadLimits::default();
        let limits = MonoBehaviourReadLimits {
            maximum_string_bytes,
            maximum_total_string_bytes: maximum_string_bytes
                .checked_mul(4)
                .unwrap_or(usize::MAX)
                .min(defaults.maximum_total_string_bytes),
            ..defaults
        };
        let script = py.detach(|| {
            self.object(file_index, path_id)?
                .read_mono_script(limits)
                .map_err(core_error)
        })?;
        Ok(convert_mono_script(script))
    }

    #[pyo3(signature = (file_index, path_id, *, maximum_string_bytes=16_777_216, maximum_paths=1_000_000))]
    fn read_build_settings(
        &self,
        py: Python<'_>,
        file_index: usize,
        path_id: i64,
        maximum_string_bytes: usize,
        maximum_paths: usize,
    ) -> PyResult<PyBuildSettings> {
        let limits = ProjectSettingsReadLimits {
            maximum_scene_paths: maximum_paths,
            maximum_string_bytes,
            maximum_total_string_bytes: maximum_string_bytes
                .checked_mul(maximum_paths.max(1))
                .unwrap_or(usize::MAX)
                .min(ProjectSettingsReadLimits::default().maximum_total_string_bytes),
            ..ProjectSettingsReadLimits::default()
        };
        let settings = py.detach(|| {
            self.object(file_index, path_id)?
                .read_build_settings(limits)
                .map_err(core_error)
        })?;
        Ok(PyBuildSettings {
            path_id: settings.path_id,
            levels: settings.levels,
            scenes: settings.scenes,
        })
    }

    #[pyo3(signature = (file_index, path_id, *, maximum_string_bytes=16_777_216))]
    fn read_player_settings(
        &self,
        py: Python<'_>,
        file_index: usize,
        path_id: i64,
        maximum_string_bytes: usize,
    ) -> PyResult<PyPlayerSettings> {
        let limits = ProjectSettingsReadLimits {
            maximum_string_bytes,
            maximum_total_string_bytes: maximum_string_bytes.saturating_mul(2),
            ..ProjectSettingsReadLimits::default()
        };
        let settings = py.detach(|| {
            self.object(file_index, path_id)?
                .read_player_settings(limits)
                .map_err(core_error)
        })?;
        Ok(PyPlayerSettings {
            path_id: settings.path_id,
            company_name: settings.company_name,
            product_name: settings.product_name,
        })
    }

    #[pyo3(signature = (
        file_index,
        path_id,
        *,
        maximum_parameters=1_000_000,
        maximum_string_bytes=16_777_216,
        maximum_output_bytes=268_435_456
    ))]
    fn read_cubism_expression(
        &self,
        py: Python<'_>,
        file_index: usize,
        path_id: i64,
        maximum_parameters: usize,
        maximum_string_bytes: usize,
        maximum_output_bytes: usize,
    ) -> PyResult<PyCubismExpression> {
        let output_limit = u64::try_from(maximum_output_bytes)
            .map_err(|_| PyValueError::new_err("maximum_output_bytes does not fit u64"))?;
        let limits = CubismExpressionReadLimits {
            maximum_parameters,
            maximum_string_bytes,
            maximum_total_string_bytes: CubismExpressionReadLimits::default()
                .maximum_total_string_bytes
                .min(maximum_output_bytes),
            maximum_output_bytes: output_limit,
            ..CubismExpressionReadLimits::default()
        };
        let prepared = py.detach(|| {
            let expression = self
                .object(file_index, path_id)?
                .read_cubism_expression(limits)
                .map_err(core_error)?;
            prepare_cubism_expression(expression, maximum_output_bytes, output_limit)
        })?;
        let PreparedCubismExpression { expression, json } = prepared;
        let mut parameters = Vec::new();
        parameters
            .try_reserve(expression.parameters.len())
            .map_err(|error| {
                PyMemoryError::new_err(format!(
                    "cannot allocate Python Cubism expression parameters: {error}"
                ))
            })?;
        for parameter in expression.parameters {
            parameters.push(Py::new(
                py,
                PyCubismExpressionParameter {
                    id: parameter.id,
                    value: f64::from(parameter.value),
                    blend: cubism_expression_blend_name(parameter.blend).to_owned(),
                },
            )?);
        }
        Ok(PyCubismExpression {
            path_id: expression.path_id,
            source_name: expression.source_name,
            expression_type: expression.expression_type,
            fade_in_time: f64::from(expression.fade_in_time),
            fade_out_time: f64::from(expression.fade_out_time),
            parameters,
            json,
        })
    }

    #[pyo3(signature = (
        file_index,
        path_id,
        *,
        maximum_links=1_000_000,
        maximum_string_bytes=16_777_216
    ))]
    fn read_cubism_pose_part(
        &self,
        py: Python<'_>,
        file_index: usize,
        path_id: i64,
        maximum_links: usize,
        maximum_string_bytes: usize,
    ) -> PyResult<PyCubismPosePart> {
        let limits = CubismAuxiliaryReadLimits {
            maximum_links,
            maximum_string_bytes,
            maximum_total_string_bytes: CubismAuxiliaryReadLimits::default()
                .maximum_total_string_bytes
                .min(maximum_string_bytes.saturating_mul(maximum_links.max(1))),
            ..CubismAuxiliaryReadLimits::default()
        };
        let pose = py.detach(|| {
            self.object(file_index, path_id)?
                .read_cubism_pose_part(limits)
                .map_err(core_error)
        })?;
        Ok(PyCubismPosePart {
            path_id: pose.path_id,
            group_index: pose.group_index,
            links: pose.links,
        })
    }

    #[pyo3(signature = (file_index, path_id, *, maximum_string_bytes=16_777_216))]
    fn read_cubism_display_info(
        &self,
        py: Python<'_>,
        file_index: usize,
        path_id: i64,
        maximum_string_bytes: usize,
    ) -> PyResult<PyCubismDisplayInfo> {
        let limits = CubismAuxiliaryReadLimits {
            maximum_string_bytes,
            maximum_total_string_bytes: maximum_string_bytes.saturating_mul(2),
            ..CubismAuxiliaryReadLimits::default()
        };
        let info = py.detach(|| {
            self.object(file_index, path_id)?
                .read_cubism_display_info(limits)
                .map_err(core_error)
        })?;
        Ok(PyCubismDisplayInfo {
            path_id: info.path_id,
            name: info.name,
            display_name: info.display_name,
        })
    }

    #[pyo3(signature = (
        file_index,
        path_id,
        *,
        motion_fps=0.0,
        maximum_elements=1_000_000,
        maximum_output_bytes=268_435_456
    ))]
    fn read_cubism_physics(
        &self,
        py: Python<'_>,
        file_index: usize,
        path_id: i64,
        motion_fps: f64,
        maximum_elements: usize,
        maximum_output_bytes: usize,
    ) -> PyResult<PyCubismPhysics> {
        let output_limit = u64::try_from(maximum_output_bytes)
            .map_err(|_| PyValueError::new_err("maximum_output_bytes does not fit u64"))?;
        let limits = CubismPhysicsReadLimits {
            maximum_sub_rigs: maximum_elements,
            maximum_inputs: maximum_elements,
            maximum_outputs: maximum_elements,
            maximum_particles: maximum_elements,
            maximum_output_bytes: output_limit,
            ..CubismPhysicsReadLimits::default()
        };
        // The physics document is a float document; Python has only doubles,
        // so the width changes at this boundary in both directions.
        #[expect(
            clippy::cast_possible_truncation,
            reason = "physics3.json's fps field is a float"
        )]
        let motion_fps = motion_fps as f32;
        py.detach(|| {
            let rig = self
                .object(file_index, path_id)?
                .read_cubism_physics(limits)
                .map_err(core_error)?;
            python_cubism_physics(&rig, motion_fps, maximum_output_bytes, output_limit)
        })
    }

    #[pyo3(signature = (
        file_index,
        path_id,
        *,
        force_bezier=false,
        maximum_curves=1_000_000,
        maximum_output_bytes=268_435_456
    ))]
    fn read_cubism_fade_motion(
        &self,
        py: Python<'_>,
        file_index: usize,
        path_id: i64,
        force_bezier: bool,
        maximum_curves: usize,
        maximum_output_bytes: usize,
    ) -> PyResult<PyCubismFadeMotion> {
        let output_limit = u64::try_from(maximum_output_bytes)
            .map_err(|_| PyValueError::new_err("maximum_output_bytes does not fit u64"))?;
        let limits = CubismFadeMotionReadLimits {
            maximum_curves,
            maximum_output_bytes: output_limit,
            ..CubismFadeMotionReadLimits::default()
        };
        py.detach(|| {
            let motion = self
                .object(file_index, path_id)?
                .read_cubism_fade_motion(limits)
                .map_err(core_error)?;
            python_cubism_fade_motion(motion, force_bezier, maximum_output_bytes, output_limit)
        })
    }

    #[pyo3(signature = (
        file_index,
        path_id,
        *,
        targets=None,
        force_bezier=false,
        maximum_output_bytes=268_435_456
    ))]
    fn read_cubism_clip_motion(
        &self,
        py: Python<'_>,
        file_index: usize,
        path_id: i64,
        targets: Option<PyRef<'_, PyCubismMotionTargets>>,
        force_bezier: bool,
        maximum_output_bytes: usize,
    ) -> PyResult<PyCubismClipMotion> {
        let output_limit = u64::try_from(maximum_output_bytes)
            .map_err(|_| PyValueError::new_err("maximum_output_bytes does not fit u64"))?;
        let targets = targets
            .map(|targets| copy_motion_targets(&targets))
            .transpose()?
            .unwrap_or_default();
        let limits = CubismClipMotionReadLimits {
            maximum_output_bytes: output_limit,
            ..CubismClipMotionReadLimits::default()
        };
        py.detach(|| {
            let motion = self
                .object(file_index, path_id)?
                .read_cubism_clip_motion(&targets, limits)
                .map_err(core_error)?;
            python_cubism_clip_motion(motion, force_bezier, maximum_output_bytes, output_limit)
        })
    }

    /// Projects one Tuanjie ACL-backed clip through Cubism bindings using a
    /// caller-supplied decoder. Output uses the standard non-forced Bezier mode.
    #[pyo3(signature = (
        file_index,
        path_id,
        decoder,
        *,
        targets=None,
        maximum_output_bytes=268_435_456
    ))]
    fn read_cubism_acl_clip_motion(
        &self,
        py: Python<'_>,
        file_index: usize,
        path_id: i64,
        decoder: Py<PyAny>,
        targets: Option<PyRef<'_, PyCubismMotionTargets>>,
        maximum_output_bytes: usize,
    ) -> PyResult<PyCubismClipMotion> {
        if !decoder.bind(py).is_callable() {
            return Err(PyTypeError::new_err("decoder must be callable"));
        }
        let output_limit = u64::try_from(maximum_output_bytes)
            .map_err(|_| PyValueError::new_err("maximum_output_bytes does not fit u64"))?;
        let targets = targets
            .map(|targets| copy_motion_targets(&targets))
            .transpose()?
            .unwrap_or_default();
        let limits = CubismClipMotionReadLimits {
            maximum_output_bytes: output_limit,
            ..CubismClipMotionReadLimits::default()
        };
        py.detach(|| {
            let motion = self
                .object(file_index, path_id)?
                .read_cubism_clip_motion_with_acl_decoder(
                    &targets,
                    limits,
                    &PythonAclDecoder {
                        callback: decoder,
                        limits: AclDecodeLimits::default(),
                    },
                )
                .map_err(core_error)?;
            python_cubism_clip_motion(motion, false, maximum_output_bytes, output_limit)
        })
    }

    #[pyo3(signature = (
        output,
        *,
        mode="auto",
        image_format="png",
        jpeg_quality=75,
        overwrite=false,
        limits=None
    ))]
    fn export(
        &self,
        output: PathBuf,
        mode: &str,
        image_format: &str,
        jpeg_quality: i64,
        overwrite: bool,
        limits: Option<PyRef<'_, PyExportLimits>>,
    ) -> PyResult<PyExportReport> {
        let mode = parse_export_mode(mode)?;
        let image_format = parse_image_format(image_format)?;
        if !(1..=100).contains(&jpeg_quality) {
            return Err(PyValueError::new_err(format!(
                "JPEG quality {jpeg_quality} is outside the supported range 1 through 100"
            )));
        }
        let jpeg_quality = u8::try_from(jpeg_quality)
            .map_err(|_| PyValueError::new_err("JPEG quality must fit in one byte"))?;
        let limits = limits.map_or_else(
            || {
                let defaults = ExportOptions::default();
                PyExportLimits {
                    objects: defaults.maximum_objects,
                    total_output_bytes: defaults.maximum_total_output_bytes,
                    metadata_bytes: defaults.maximum_metadata_bytes,
                }
            },
            |limits| *limits,
        );
        let options = ExportOptions {
            mode,
            image_format,
            jpeg_quality,
            overwrite_existing: overwrite,
            maximum_objects: limits.objects,
            maximum_total_output_bytes: limits.total_output_bytes,
            maximum_metadata_bytes: limits.metadata_bytes,
            ..ExportOptions::default()
        };
        Python::attach(|py| {
            py.detach(move || {
                let report = self.studio.export(output, options).map_err(core_error)?;
                prepare_export_report(report)
            })
        })
    }

    /// Materializes the verified Live2D package slice in memory.
    #[pyo3(signature = (
        *,
        schemas=None,
        acl_decoder=None,
        maximum_file_bytes=536_870_912,
        maximum_total_bytes=4_294_967_296
    ))]
    fn read_live2d_packages(
        &self,
        py: Python<'_>,
        schemas: Option<PyRef<'_, PyMonoBehaviourSchemas>>,
        acl_decoder: Option<Py<PyAny>>,
        maximum_file_bytes: u64,
        maximum_total_bytes: u64,
    ) -> PyResult<PyLive2dPackageSet> {
        if acl_decoder
            .as_ref()
            .is_some_and(|decoder| !decoder.bind(py).is_callable())
        {
            return Err(PyTypeError::new_err("acl_decoder must be callable"));
        }
        let planning_limits = Live2dPackageLimits {
            maximum_total_moc_bytes: maximum_total_bytes,
            maximum_total_texture_payload_bytes: maximum_total_bytes,
            maximum_total_manifest_bytes: maximum_total_bytes,
            texture: TextureReadLimits {
                maximum_output_bytes: maximum_file_bytes,
                ..TextureReadLimits::default()
            },
            ..Live2dPackageLimits::default()
        };
        let materialize_limits = Live2dPackageMaterializeLimits {
            maximum_file_bytes,
            maximum_total_bytes,
            texture: planning_limits.texture,
            motion_target_index: CubismMotionTargetIndexLimits::default(),
        };
        let schema_provider = schemas.map(|schemas| Arc::clone(&schemas.provider));
        let set = py
            .detach(move || {
                let decoder = acl_decoder.map(|callback| PythonAclDecoder {
                    callback,
                    limits: AclDecodeLimits::default(),
                });
                let provider = schema_provider
                    .as_deref()
                    .map(|value| value as &dyn MonoBehaviourSchemaProvider);
                self.studio.read_live2d_packages_with_adapters(
                    planning_limits,
                    materialize_limits,
                    provider,
                    decoder.as_ref().map(|value| value as &dyn AclDecoder),
                )
            })
            .map_err(core_error)?;
        convert_live2d_package_set(py, set)
    }

    fn __repr__(&self) -> String {
        format!(
            "UnityRs(files={}, objects={}, resources={})",
            self.studio.file_count(),
            self.studio.object_count(),
            self.studio.resource_count()
        )
    }
}

impl PyUnityRs {
    fn object(&self, file_index: usize, path_id: i64) -> PyResult<StudioObject<'_>> {
        self.studio.object(file_index, path_id).ok_or_else(|| {
            PyKeyError::new_err(format!(
                "object path_id {path_id} was not found in file index {file_index}"
            ))
        })
    }
}

fn prepare_legacy_animation(animation: LegacyAnimationComponent) -> PyResult<PyLegacyAnimation> {
    let mut clips = reserve_metadata(
        animation.clips.len(),
        "Python legacy Animation clip references",
    )?;
    for reference in animation.clips {
        clips.push(object_reference_tuple(reference));
    }
    Ok(PyLegacyAnimation {
        path_id: animation.path_id,
        game_object: object_reference_tuple(animation.behaviour.component.game_object),
        enabled: animation.behaviour.enabled,
        default_clip: object_reference_tuple(animation.default_clip),
        clips,
        trailing_bytes: animation.trailing_bytes,
    })
}

fn prepare_animator_override_controller(
    controller: AnimatorOverrideController,
) -> PyResult<PyAnimatorOverrideController> {
    let mut clip_overrides = reserve_metadata(
        controller.clips.len(),
        "Python AnimatorOverrideController clip overrides",
    )?;
    for pair in controller.clips {
        clip_overrides.push((
            object_reference_tuple(pair.original_clip),
            object_reference_tuple(pair.override_clip),
        ));
    }
    Ok(PyAnimatorOverrideController {
        path_id: controller.path_id,
        name: controller.name,
        controller: object_reference_tuple(controller.controller),
        clip_overrides,
        trailing_bytes: controller.trailing_bytes,
    })
}

fn prepare_asset_bundle(path_id: i64, bundle: AssetBundleMetadata) -> PyResult<PyAssetBundle> {
    let mut preload_table = reserve_metadata(
        bundle.preload_table.len(),
        "Python AssetBundle preload references",
    )?;
    for reference in bundle.preload_table {
        preload_table.push(object_reference_tuple(reference));
    }
    let mut container = reserve_metadata(
        bundle.container.len(),
        "Python AssetBundle container entries",
    )?;
    for entry in bundle.container {
        container.push((
            entry.key,
            entry.preload_index,
            entry.preload_size,
            object_reference_tuple(entry.asset),
        ));
    }
    Ok(PyAssetBundle {
        path_id,
        name: bundle.name,
        object_name: bundle.object_name,
        asset_bundle_name: bundle.asset_bundle_name,
        preload_table,
        container,
        dependencies: bundle.dependencies,
        is_streamed_scene_asset_bundle: bundle.is_streamed_scene_asset_bundle,
    })
}

fn prepare_resource_manager(
    path_id: i64,
    manager: ResourceManagerMetadata,
) -> PyResult<PyResourceManager> {
    let mut container = reserve_metadata(
        manager.container.len(),
        "Python ResourceManager container entries",
    )?;
    for entry in manager.container {
        container.push((entry.key, object_reference_tuple(entry.asset)));
    }
    Ok(PyResourceManager { path_id, container })
}

fn prepare_preload_data(path_id: i64, preload: PreloadDataMetadata) -> PyResult<PyPreloadData> {
    let mut assets = reserve_metadata(preload.assets.len(), "Python PreloadData asset references")?;
    for reference in preload.assets {
        assets.push(object_reference_tuple(reference));
    }
    Ok(PyPreloadData {
        path_id,
        name: preload.name,
        assets,
    })
}

fn prepare_animator_controller(controller: AnimatorController) -> PyResult<PyAnimatorController> {
    let layer_count = controller.controller.layers.len();
    let state_machine_count = controller.controller.state_machines.len();
    let value_count = controller.controller.values.values.len();
    let entity_id_count = controller
        .controller
        .default_values
        .entity_ids
        .as_ref()
        .map(|values| values.count);
    let mut tos = reserve_metadata(controller.tos.len(), "Python AnimatorController TOS")?;
    for entry in controller.tos {
        tos.push((entry.key, entry.value));
    }
    let mut animation_clips = reserve_metadata(
        controller.animation_clips.len(),
        "Python AnimatorController clip references",
    )?;
    for reference in controller.animation_clips {
        animation_clips.push((reference.file_id, reference.path_id));
    }
    Ok(PyAnimatorController {
        path_id: controller.path_id,
        name: controller.name,
        controller_size: controller.controller_size,
        layer_count,
        state_machine_count,
        value_count,
        entity_id_count,
        tos,
        animation_clips,
    })
}

fn prepare_avatar(avatar: Avatar) -> PyResult<PyAvatar> {
    let skeleton_node_count = avatar.constant.avatar_skeleton.nodes.len();
    let human_skeleton_node_count = avatar.constant.human.skeleton.nodes.len();
    let (has_human_description, human_bone_count, skeleton_bone_count, root_motion_bone_name) =
        avatar
            .human_description
            .map_or((false, 0, 0, None), |description| {
                (
                    true,
                    description.human_bones.len(),
                    description.skeleton_bones.len(),
                    Some(description.root_motion_bone_name),
                )
            });
    let path_count = avatar.paths.len();
    let mut paths = reserve_metadata(path_count, "Python Avatar paths")?;
    for entry in avatar.paths {
        paths.push((entry.hash, entry.path));
    }
    Ok(PyAvatar {
        path_id: avatar.path_id,
        name: avatar.name,
        declared_avatar_size: avatar.declared_avatar_size,
        skeleton_node_count,
        human_skeleton_node_count,
        path_count,
        paths,
        has_human_description,
        human_bone_count,
        skeleton_bone_count,
        root_motion_bone_name,
    })
}

struct PreparedCubismExpression {
    expression: CubismExpression,
    json: Vec<u8>,
}

fn prepare_cubism_expression(
    expression: CubismExpression,
    maximum_output_bytes: usize,
    output_limit: u64,
) -> PyResult<PreparedCubismExpression> {
    let json =
        materialize_python_bytes(maximum_output_bytes, "Cubism expression JSON", |output| {
            expression.write_exp3_json(output, output_limit)
        })?;
    Ok(PreparedCubismExpression { expression, json })
}

fn python_cubism_physics(
    rig: &CubismPhysicsRig,
    motion_fps: f32,
    maximum_output_bytes: usize,
    output_limit: u64,
) -> PyResult<PyCubismPhysics> {
    let json = materialize_python_bytes(maximum_output_bytes, "Cubism physics JSON", |output| {
        rig.write_physics3_json(motion_fps, output, output_limit)
    })?;
    let input_count = checked_element_count(
        rig.sub_rigs.iter().map(|value| value.inputs.len()),
        "physics inputs",
    )?;
    let output_count = checked_element_count(
        rig.sub_rigs.iter().map(|value| value.outputs.len()),
        "physics outputs",
    )?;
    let particle_count = checked_element_count(
        rig.sub_rigs.iter().map(|value| value.particles.len()),
        "physics particles",
    )?;
    Ok(PyCubismPhysics {
        path_id: rig.path_id,
        fps: f64::from(rig.fps),
        gravity: (f64::from(rig.gravity.x), f64::from(rig.gravity.y)),
        wind: (f64::from(rig.wind.x), f64::from(rig.wind.y)),
        sub_rig_count: rig.sub_rigs.len(),
        input_count,
        output_count,
        particle_count,
        json,
    })
}

fn python_cubism_fade_motion(
    motion: CubismFadeMotion,
    force_bezier: bool,
    maximum_output_bytes: usize,
    output_limit: u64,
) -> PyResult<PyCubismFadeMotion> {
    let json = materialize_python_bytes(maximum_output_bytes, "Cubism motion JSON", |output| {
        motion.write_motion3_json(
            &CubismMotionTargetNames::default(),
            force_bezier,
            output,
            output_limit,
        )
    })?;
    let keyframe_count = checked_element_count(
        motion.curves.iter().map(|curve| curve.keyframes.len()),
        "motion keyframes",
    )?;
    Ok(PyCubismFadeMotion {
        path_id: motion.path_id,
        source_name: motion.source_name,
        motion_name: motion.motion_name,
        fade_in_time: f64::from(motion.fade_in_time),
        fade_out_time: f64::from(motion.fade_out_time),
        motion_length: f64::from(motion.motion_length),
        curve_count: motion.curves.len(),
        keyframe_count,
        json,
    })
}

fn python_cubism_clip_motion(
    motion: unity_rs_core::live2d_clip_motion::CubismClipMotion,
    force_bezier: bool,
    maximum_output_bytes: usize,
    output_limit: u64,
) -> PyResult<PyCubismClipMotion> {
    let json = materialize_python_bytes(maximum_output_bytes, "Cubism clip JSON", |output| {
        motion.write_motion3_json(force_bezier, output, output_limit)
    })?;
    let keyframe_count = checked_element_count(
        motion.curves.iter().map(|curve| curve.keyframes.len()),
        "clip-motion keyframes",
    )?;
    Ok(PyCubismClipMotion {
        file_index: motion.object.file_index,
        path_id: motion.object.path_id,
        name: motion.name,
        duration: f64::from(motion.duration),
        fps: f64::from(motion.fps),
        curve_count: motion.curves.len(),
        keyframe_count,
        event_count: motion.events.len(),
        json,
    })
}

fn prepare_load_diagnostic_page(
    studio: &Studio,
    offset: usize,
    limit: usize,
) -> PyResult<Vec<PyLoadDiagnostic>> {
    check_metadata_page_limit(limit)?;
    let diagnostics = studio.load_diagnostics();
    let available = diagnostics.len().saturating_sub(offset);
    let count = available.min(limit);
    let mut output = reserve_metadata(count, "Python load diagnostic page")?;
    for diagnostic in diagnostics.iter().skip(offset).take(count) {
        output.push(python_load_diagnostic(diagnostic)?);
    }
    Ok(output)
}

fn prepare_files(studio: &Studio) -> PyResult<Vec<PyFileInfo>> {
    checked_convenience_list(studio.file_count(), "files", "iter_files")?;
    let mut output = reserve_metadata(studio.file_count(), "Python file metadata")?;
    for file in studio.files() {
        output.push(python_file_info(file)?);
    }
    Ok(output)
}

fn prepare_objects(studio: &Studio) -> PyResult<Vec<PyObjectInfo>> {
    checked_convenience_list(studio.object_count(), "objects", "iter_objects")?;
    let mut output = reserve_metadata(studio.object_count(), "Python object metadata")?;
    for object in studio.objects() {
        output.push(python_object_info(object)?);
    }
    Ok(output)
}

fn prepare_resources(studio: &Studio) -> PyResult<Vec<PyResourceInfo>> {
    checked_convenience_list(studio.resource_count(), "resources", "iter_resources")?;
    let mut output =
        reserve_metadata(studio.resource_count(), "Python external resource metadata")?;
    for resource in studio.resources() {
        output.push(python_resource_info(resource)?);
    }
    Ok(output)
}

fn prepare_file_page(studio: &Studio, offset: usize, limit: usize) -> PyResult<Vec<PyFileInfo>> {
    check_metadata_page_limit(limit)?;
    let available = studio.file_count().saturating_sub(offset);
    let count = available.min(limit);
    let mut output = reserve_metadata(count, "Python file metadata page")?;
    for file in studio.files().skip(offset).take(count) {
        output.push(python_file_info(file)?);
    }
    Ok(output)
}

fn prepare_object_page(
    studio: &Studio,
    file_index: usize,
    offset: usize,
    limit: usize,
) -> PyResult<Vec<PyObjectInfo>> {
    check_metadata_page_limit(limit)?;
    let file = studio
        .file(file_index)
        .ok_or_else(|| PyKeyError::new_err(format!("file index {file_index} was not found")))?;
    let available = file.object_count().saturating_sub(offset);
    let count = available.min(limit);
    let mut output = reserve_metadata(count, "Python object metadata page")?;
    for object_index in offset..offset + count {
        let object = studio
            .object_by_index(file_index, object_index)
            .ok_or_else(|| {
                PyValueError::new_err("object page could not resolve a validated object index")
            })?;
        output.push(python_object_info(object)?);
    }
    Ok(output)
}

fn prepare_resource_page(
    studio: &Studio,
    offset: usize,
    limit: usize,
) -> PyResult<Vec<PyResourceInfo>> {
    check_metadata_page_limit(limit)?;
    let available = studio.resource_count().saturating_sub(offset);
    let count = available.min(limit);
    let mut output = reserve_metadata(count, "Python external resource metadata page")?;
    for resource in studio.resources().skip(offset).take(count) {
        output.push(python_resource_info(resource)?);
    }
    Ok(output)
}

fn python_file_info(file: StudioFile<'_>) -> PyResult<PyFileInfo> {
    Ok(PyFileInfo {
        index: file.index(),
        path: try_copy_string(file.path(), "file path")?,
        unity_version: try_copy_string(file.unity_version(), "Unity version")?,
        object_count: file.object_count(),
    })
}

fn python_object_info(object: StudioObject<'_>) -> PyResult<PyObjectInfo> {
    Ok(PyObjectInfo {
        file_index: object.file_index(),
        object_index: object.object_index(),
        source_path: try_copy_string(object.source_path(), "object source path")?,
        path_id: object.path_id(),
        class_id: object.class_id(),
        byte_size: object.byte_size(),
        name: try_copy_optional_string(object.name(), "object name")?,
        container: try_copy_optional_string(object.container(), "object container")?,
    })
}

fn python_resource_info(resource: StudioResource<'_>) -> PyResult<PyResourceInfo> {
    Ok(PyResourceInfo {
        index: resource.index(),
        path: try_copy_string(resource.path(), "external resource path")?,
        byte_size: resource.byte_size(),
    })
}

fn python_load_diagnostic(diagnostic: &LoadDiagnostic) -> PyResult<PyLoadDiagnostic> {
    Ok(PyLoadDiagnostic {
        path: try_copy_string(&diagnostic.path, "load diagnostic path")?,
        message: try_copy_string(&diagnostic.message, "load diagnostic message")?,
    })
}

/// Parses a caller-supplied `UnityCN` key from 16 bytes or a 16-byte string.
///
/// The package never ships or derives keys; obtaining one for a title is the
/// caller's responsibility.
fn model_files(py: Python<'_>, textures: Vec<SceneTexture>) -> PyResult<Vec<Py<PyModelFile>>> {
    let mut files = Vec::new();
    files.try_reserve(textures.len()).map_err(|error| {
        PyValueError::new_err(format!("cannot allocate model texture files: {error}"))
    })?;
    for texture in textures {
        files.push(Py::new(
            py,
            PyModelFile {
                file_name: texture.file_name,
                data: texture.encoded,
            },
        )?);
    }
    Ok(files)
}

fn skipped_textures(texture_skips: Vec<SceneTextureSkip>) -> PyResult<Vec<String>> {
    let mut skipped = Vec::new();
    skipped.try_reserve(texture_skips.len()).map_err(|error| {
        PyMemoryError::new_err(format!("cannot allocate skipped model textures: {error}"))
    })?;
    for skip in texture_skips {
        let length = skip
            .property
            .len()
            .checked_add(2)
            .and_then(|length| length.checked_add(skip.reason.len()))
            .ok_or_else(|| PyValueError::new_err("skipped model texture text overflowed"))?;
        let mut description = String::new();
        description.try_reserve_exact(length).map_err(|error| {
            PyMemoryError::new_err(format!(
                "cannot allocate skipped model texture text: {error}"
            ))
        })?;
        description.push_str(&skip.property);
        description.push_str(": ");
        description.push_str(&skip.reason);
        skipped.push(description);
    }
    Ok(skipped)
}

fn parse_unity_cn_key(py: Python<'_>, value: Option<Py<PyAny>>) -> PyResult<Option<UnityCnKey>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let bound = value.bind(py);
    let key = if let Ok(text) = bound.cast::<PyString>() {
        let text = text.to_owned();
        let text = text.to_cow()?;
        copy_unity_cn_key(text.as_bytes())?
    } else if let Ok(bytes) = bound.cast::<PyBytes>() {
        copy_unity_cn_key(bytes.as_bytes())?
    } else {
        return Err(PyValueError::new_err(
            "unity_cn_key must be 16 bytes or a 16-byte string",
        ));
    };
    Ok(Some(UnityCnKey::new(key)))
}

fn copy_unity_cn_key(bytes: &[u8]) -> PyResult<[u8; 16]> {
    bytes.try_into().map_err(|_| {
        PyValueError::new_err(format!(
            "unity_cn_key must be exactly 16 bytes; got {}",
            bytes.len()
        ))
    })
}

const fn failure_policy(skip_unreadable_inputs: bool) -> LoadFailurePolicy {
    if skip_unreadable_inputs {
        LoadFailurePolicy::SkipInput
    } else {
        LoadFailurePolicy::Abort
    }
}

fn parse_unity_version_override(value: Option<String>) -> PyResult<Option<UnityVersion>> {
    value
        .map(|value| {
            value
                .parse::<UnityVersion>()
                .map_err(|error| PyValueError::new_err(error.to_string()))
        })
        .transpose()
}

fn python_oodle_decoder(
    py: Python<'_>,
    callback: Option<Py<PyAny>>,
) -> PyResult<Option<Arc<dyn OodleDecoder>>> {
    callback
        .map(|callback| {
            if !callback.bind(py).is_callable() {
                return Err(PyTypeError::new_err("oodle_decoder must be callable"));
            }
            Ok(Arc::new(PythonOodleDecoder { callback }) as Arc<dyn OodleDecoder>)
        })
        .transpose()
}

fn copy_python_input(input: &[u8], maximum_bytes: u64) -> PyResult<Vec<u8>> {
    let length = u64::try_from(input.len())
        .map_err(|_| PyValueError::new_err("Python input length does not fit u64"))?;
    if length > maximum_bytes {
        return Err(PyValueError::new_err(format!(
            "Python input has {length} bytes, exceeding limit {maximum_bytes}"
        )));
    }
    let mut copy = Vec::new();
    copy.try_reserve_exact(input.len()).map_err(|error| {
        PyMemoryError::new_err(format!(
            "cannot allocate {length} Python input bytes: {error}"
        ))
    })?;
    copy.extend_from_slice(input);
    Ok(copy)
}

fn copy_python_files(
    files: &Bound<'_, PyList>,
    maximum_files: usize,
    maximum_file_bytes: u64,
    maximum_total_bytes: u64,
    maximum_path_bytes: usize,
    maximum_total_path_bytes: usize,
) -> PyResult<Vec<(String, Region)>> {
    let file_count = files.len();
    if file_count > maximum_files {
        return Err(PyValueError::new_err(format!(
            "memory input has {file_count} files, exceeding limit {maximum_files}"
        )));
    }
    let mut inputs = Vec::new();
    inputs.try_reserve_exact(file_count).map_err(|error| {
        PyMemoryError::new_err(format!("cannot allocate memory input table: {error}"))
    })?;
    let mut total_bytes = 0_u64;
    let mut total_name_bytes = 0_usize;
    for (index, file) in files.iter().enumerate() {
        let tuple = file
            .cast::<PyTuple>()
            .map_err(|_| PyTypeError::new_err(format!("memory input {index} must be a tuple")))?;
        if tuple.len() != 2 {
            return Err(PyTypeError::new_err(format!(
                "memory input {index} must contain a name and bytes"
            )));
        }
        let name_object = tuple.get_item(0)?.cast_into::<PyString>()?;
        let data = tuple.get_item(1)?.cast_into::<PyBytes>()?;
        let name = name_object.to_cow()?;
        if name.len() > maximum_path_bytes {
            return Err(PyValueError::new_err(format!(
                "memory input name has {} bytes, exceeding limit {maximum_path_bytes}",
                name.len()
            )));
        }
        total_name_bytes = total_name_bytes
            .checked_add(name.len())
            .ok_or_else(|| PyValueError::new_err("memory input filename byte count overflowed"))?;
        if total_name_bytes > maximum_total_path_bytes {
            return Err(PyValueError::new_err(format!(
                "memory input names total {total_name_bytes} bytes, exceeding limit {maximum_total_path_bytes}"
            )));
        }
        let source = data.as_bytes();
        let length = u64::try_from(source.len())
            .map_err(|_| PyValueError::new_err("memory input length does not fit u64"))?;
        if length > maximum_file_bytes {
            return Err(PyValueError::new_err(format!(
                "memory input {name:?} has {length} bytes, exceeding per-file limit {maximum_file_bytes}"
            )));
        }
        total_bytes = total_bytes
            .checked_add(length)
            .ok_or_else(|| PyValueError::new_err("memory input byte count overflowed"))?;
        if total_bytes > maximum_total_bytes {
            return Err(PyValueError::new_err(format!(
                "memory inputs have {total_bytes} bytes, exceeding total limit {maximum_total_bytes}"
            )));
        }
        inputs.push((
            try_copy_string(name.as_ref(), "memory input name")?,
            Region::from_bytes(copy_python_input(source, maximum_file_bytes)?),
        ));
    }
    Ok(inputs)
}

fn copy_python_input_name(
    value: &str,
    maximum_path_bytes: usize,
    maximum_total_path_bytes: usize,
    field: &'static str,
) -> PyResult<String> {
    if value.len() > maximum_path_bytes {
        return Err(PyValueError::new_err(format!(
            "{field} has {} bytes, exceeding path limit {maximum_path_bytes}",
            value.len()
        )));
    }
    if value.len() > maximum_total_path_bytes {
        return Err(PyValueError::new_err(format!(
            "{field} has {} bytes, exceeding total path limit {maximum_total_path_bytes}",
            value.len()
        )));
    }
    try_copy_string(value, field)
}

fn try_copy_optional_string(value: Option<&str>, field: &'static str) -> PyResult<Option<String>> {
    value.map(|value| try_copy_string(value, field)).transpose()
}

fn try_copy_string(value: &str, field: &'static str) -> PyResult<String> {
    let mut copy = String::new();
    copy.try_reserve_exact(value.len())
        .map_err(|error| PyMemoryError::new_err(format!("cannot allocate {field}: {error}")))?;
    copy.push_str(value);
    Ok(copy)
}

fn reserve_metadata<T>(count: usize, field: &'static str) -> PyResult<Vec<T>> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(count)
        .map_err(|error| PyMemoryError::new_err(format!("cannot allocate {field}: {error}")))?;
    Ok(values)
}

fn checked_convenience_list(count: usize, field: &str, iterator: &str) -> PyResult<()> {
    if count > MAXIMUM_METADATA_PAGE_ITEMS {
        return Err(PyValueError::new_err(format!(
            "{field}() would materialize {count} entries; use {iterator}() or a bounded page"
        )));
    }
    Ok(())
}

fn check_metadata_page_limit(limit: usize) -> PyResult<()> {
    if limit > MAXIMUM_METADATA_PAGE_ITEMS {
        return Err(PyValueError::new_err(format!(
            "metadata page limit {limit} exceeds {MAXIMUM_METADATA_PAGE_ITEMS}"
        )));
    }
    Ok(())
}

/// Recursively extracts supported Unity containers without spawning the CLI.
#[pyfunction]
#[pyo3(signature = (
    input,
    output,
    *,
    overwrite=false,
    limits=None,
    oodle_decoder=None,
    unity_cn_key=None
))]
fn extract(
    py: Python<'_>,
    input: PathBuf,
    output: PathBuf,
    overwrite: bool,
    limits: Option<PyRef<'_, PyExtractionLimits>>,
    oodle_decoder: Option<Py<PyAny>>,
    unity_cn_key: Option<Py<PyAny>>,
) -> PyResult<PyExtractionReport> {
    let limits = limits.map_or_else(ExtractionLimits::default, |limits| {
        ExtractionLimits::from(*limits)
    });
    let oodle_decoder = python_oodle_decoder(py, oodle_decoder)?;
    let unity_cn_key = parse_unity_cn_key(py, unity_cn_key)?;
    py.detach(move || {
        let report = Studio::extract(
            input,
            output,
            ExtractionOptions {
                limits,
                overwrite_existing: overwrite,
                oodle_decoder,
                unity_cn_key,
            },
        )
        .map_err(core_error)?;
        convert_extraction_report(report)
    })
}

fn convert_extraction_report(report: ExtractionReport) -> PyResult<PyExtractionReport> {
    let mut extracted = reserve_metadata(report.extracted.len(), "Python extraction records")?;
    for record in report.extracted {
        extracted.push(PyExtractionRecord {
            source: record.source,
            output_path: try_path_string(&record.output_path, "extracted output path")?,
            bytes: record.bytes,
        });
    }
    let mut skipped_existing = reserve_metadata(
        report.skipped_existing.len(),
        "Python extraction skip records",
    )?;
    for record in report.skipped_existing {
        skipped_existing.push(PyExtractionSkip {
            source: record.source,
            output_path: try_path_string(&record.output_path, "skipped output path")?,
        });
    }
    let mut failures = reserve_metadata(report.failures.len(), "Python extraction failures")?;
    for failure in report.failures {
        failures.push(PyExtractionFailure {
            source: failure.source,
            error: failure.error,
        });
    }
    Ok(PyExtractionReport {
        extracted,
        skipped_existing,
        failures,
        output_bytes: report.output_bytes,
    })
}

fn try_clone_extraction_records(
    source: &[PyExtractionRecord],
) -> PyResult<Vec<PyExtractionRecord>> {
    let mut output = reserve_metadata(source.len(), "Python extraction record list")?;
    for record in source {
        output.push(PyExtractionRecord {
            source: try_copy_string(&record.source, "extraction source")?,
            output_path: try_copy_string(&record.output_path, "extracted output path")?,
            bytes: record.bytes,
        });
    }
    Ok(output)
}

fn try_clone_extraction_skips(source: &[PyExtractionSkip]) -> PyResult<Vec<PyExtractionSkip>> {
    let mut output = reserve_metadata(source.len(), "Python extraction skip list")?;
    for record in source {
        output.push(PyExtractionSkip {
            source: try_copy_string(&record.source, "extraction source")?,
            output_path: try_copy_string(&record.output_path, "skipped output path")?,
        });
    }
    Ok(output)
}

fn try_clone_extraction_failures(
    source: &[PyExtractionFailure],
) -> PyResult<Vec<PyExtractionFailure>> {
    let mut output = reserve_metadata(source.len(), "Python extraction failure list")?;
    for failure in source {
        output.push(PyExtractionFailure {
            source: try_copy_string(&failure.source, "extraction source")?,
            error: try_copy_string(&failure.error, "extraction error")?,
        });
    }
    Ok(output)
}

#[cfg(windows)]
fn for_each_path_char_lossy(
    value: &std::ffi::OsStr,
    mut visitor: impl FnMut(char) -> PyResult<()>,
) -> PyResult<()> {
    use std::char::decode_utf16;
    use std::os::windows::ffi::OsStrExt;

    for character in decode_utf16(value.encode_wide()) {
        visitor(character.unwrap_or(char::REPLACEMENT_CHARACTER))?;
    }
    Ok(())
}

#[cfg(not(windows))]
fn for_each_path_char_lossy(
    value: &std::ffi::OsStr,
    mut visitor: impl FnMut(char) -> PyResult<()>,
) -> PyResult<()> {
    let mut input = value.as_encoded_bytes();
    while !input.is_empty() {
        match std::str::from_utf8(input) {
            Ok(valid) => {
                for character in valid.chars() {
                    visitor(character)?;
                }
                return Ok(());
            }
            Err(error) => {
                let valid_length = error.valid_up_to();
                if valid_length != 0 {
                    let valid = std::str::from_utf8(&input[..valid_length]).map_err(|_| {
                        PyValueError::new_err("valid filesystem UTF-8 prefix could not be decoded")
                    })?;
                    for character in valid.chars() {
                        visitor(character)?;
                    }
                }
                visitor(char::REPLACEMENT_CHARACTER)?;
                let invalid_length = error
                    .error_len()
                    .unwrap_or_else(|| input.len() - valid_length);
                input = &input[valid_length + invalid_length..];
            }
        }
    }
    Ok(())
}

fn path_lossy_utf8_length(value: &std::ffi::OsStr, field: &'static str) -> PyResult<usize> {
    let mut length = 0_usize;
    for_each_path_char_lossy(value, |character| {
        length = length.checked_add(character.len_utf8()).ok_or_else(|| {
            PyValueError::new_err(format!("{field} replacement length overflowed"))
        })?;
        Ok(())
    })?;
    Ok(length)
}

fn try_path_string(path: &std::path::Path, field: &'static str) -> PyResult<String> {
    let value = path.as_os_str();
    let utf8_length = path_lossy_utf8_length(value, field)?;
    let mut copy = String::new();
    copy.try_reserve_exact(utf8_length)
        .map_err(|error| PyMemoryError::new_err(format!("cannot allocate {field}: {error}")))?;
    for_each_path_char_lossy(value, |character| {
        copy.push(character);
        Ok(())
    })?;
    if copy.len() != utf8_length {
        return Err(PyValueError::new_err(format!(
            "{field} changed while converting the filesystem path"
        )));
    }
    Ok(copy)
}

fn core_error(error: Error) -> PyErr {
    match error {
        // Let PyO3 retain Rust's io::ErrorKind classification. Converting the
        // error to a string first collapses FileNotFoundError,
        // PermissionError, FileExistsError, and the other standard OSError
        // subclasses into a generic OSError.
        Error::Io(error) => error.into(),
        Error::InvalidData(message) => PyValueError::new_err(message),
        Error::Unsupported(message) => PyNotImplementedError::new_err(message),
    }
}

/// Copies bounded Rust output into a Python `bytes` object without turning a
/// Python allocation failure into a Rust panic.
fn python_bytes<'py>(py: Python<'py>, bytes: &[u8]) -> PyResult<Bound<'py, PyBytes>> {
    PyBytes::new_with(py, bytes.len(), |output| {
        output.copy_from_slice(bytes);
        Ok(())
    })
}

struct BoundedPythonOutput {
    bytes: Vec<u8>,
    maximum: usize,
    field: &'static str,
    allocation_failed: bool,
    limit_exceeded: bool,
}

impl BoundedPythonOutput {
    const fn new(maximum: usize, field: &'static str) -> Self {
        Self {
            bytes: Vec::new(),
            maximum,
            field,
            allocation_failed: false,
            limit_exceeded: false,
        }
    }
}

impl io::Write for BoundedPythonOutput {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        let Some(next) = self.bytes.len().checked_add(input.len()) else {
            self.limit_exceeded = true;
            return Err(io::Error::other(format!(
                "{} length overflowed",
                self.field
            )));
        };
        if next > self.maximum {
            self.limit_exceeded = true;
            return Err(io::Error::other(format!(
                "{} exceeds {} bytes",
                self.field, self.maximum
            )));
        }
        if let Err(error) = self.bytes.try_reserve(input.len()) {
            self.allocation_failed = true;
            return Err(io::Error::new(
                io::ErrorKind::OutOfMemory,
                format!("cannot allocate {}: {error}", self.field),
            ));
        }
        self.bytes.extend_from_slice(input);
        Ok(input.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn materialize_python_output<T>(
    maximum: usize,
    field: &'static str,
    write: impl FnOnce(&mut BoundedPythonOutput) -> unity_rs_core::Result<(u64, T)>,
) -> PyResult<(Vec<u8>, T)> {
    let mut output = BoundedPythonOutput::new(maximum, field);
    let write_result = write(&mut output);
    if output.allocation_failed {
        return Err(PyMemoryError::new_err(format!("cannot allocate {field}")));
    }
    if output.limit_exceeded {
        return Err(PyValueError::new_err(format!(
            "{field} exceeds {maximum} bytes"
        )));
    }
    let (written, value) = write_result.map_err(python_writer_error)?;
    let actual = u64::try_from(output.bytes.len())
        .map_err(|_| PyValueError::new_err(format!("{field} length does not fit u64")))?;
    if written != actual {
        return Err(PyValueError::new_err(format!(
            "{field} writer reported {written} bytes but produced {actual}"
        )));
    }
    Ok((output.bytes, value))
}

fn materialize_python_bytes(
    maximum: usize,
    field: &'static str,
    write: impl FnOnce(&mut BoundedPythonOutput) -> unity_rs_core::Result<u64>,
) -> PyResult<Vec<u8>> {
    materialize_python_output(maximum, field, |output| {
        write(output).map(|written| (written, ()))
    })
    .map(|(bytes, ())| bytes)
}

fn python_writer_error(error: Error) -> PyErr {
    match error {
        Error::Io(error) if is_output_limit_error(&error) => {
            PyValueError::new_err(error.to_string())
        }
        error => core_error(error),
    }
}

fn is_output_limit_error(error: &io::Error) -> bool {
    if error.kind() == io::ErrorKind::WriteZero {
        return true;
    }
    let message = error.to_string();
    message.contains("exceeds") && (message.contains(" byte") || message.contains(" bytes"))
}

fn python_allocation_error(field: &str, error: impl std::fmt::Display) -> unity_rs_core::Error {
    io::Error::new(
        io::ErrorKind::OutOfMemory,
        format!("cannot allocate Python {field}: {error}"),
    )
    .into()
}

fn python_callback_bytes<'py>(
    py: Python<'py>,
    bytes: &[u8],
) -> unity_rs_core::Result<Bound<'py, PyBytes>> {
    python_bytes(py, bytes).map_err(|error| python_allocation_error("callback bytes", error))
}

fn checked_schema_string_bytes<'a>(values: impl Iterator<Item = &'a str>) -> PyResult<usize> {
    let mut total = 0_usize;
    for value in values {
        total = total
            .checked_add(value.len())
            .ok_or_else(|| PyValueError::new_err("MonoBehaviour schema strings overflowed"))?;
    }
    if total > MAXIMUM_SCHEMA_STRING_BYTES {
        return Err(PyValueError::new_err(format!(
            "MonoBehaviour schema strings exceed {MAXIMUM_SCHEMA_STRING_BYTES} bytes"
        )));
    }
    Ok(total)
}

fn extract_schema_nodes(
    nodes: &Bound<'_, PyList>,
    mut total_string_bytes: usize,
) -> PyResult<Vec<PyMonoBehaviourSchemaNode>> {
    let node_count = nodes.len();
    if node_count == 0 {
        return Err(PyValueError::new_err(
            "MonoBehaviour schema must contain a root node",
        ));
    }
    if node_count > MAXIMUM_SCHEMA_NODES {
        return Err(PyValueError::new_err(format!(
            "MonoBehaviour schema has {node_count} nodes; maximum is {MAXIMUM_SCHEMA_NODES}"
        )));
    }

    let mut extracted = reserve_metadata(node_count, "MonoBehaviour schema input nodes")?;
    for (index, node) in nodes.iter().enumerate() {
        let tuple = node.cast::<PyTuple>().map_err(|_| {
            PyTypeError::new_err(format!("MonoBehaviour schema node {index} must be a tuple"))
        })?;
        if tuple.len() != 4 {
            return Err(PyTypeError::new_err(format!(
                "MonoBehaviour schema node {index} must contain four values"
            )));
        }
        let type_name_object = tuple.get_item(0)?.cast_into::<PyString>()?;
        let field_name_object = tuple.get_item(1)?.cast_into::<PyString>()?;
        let type_name = type_name_object.to_cow()?;
        let field_name = field_name_object.to_cow()?;
        total_string_bytes = total_string_bytes
            .checked_add(type_name.len())
            .and_then(|value| value.checked_add(field_name.len()))
            .ok_or_else(|| PyValueError::new_err("MonoBehaviour schema strings overflowed"))?;
        if total_string_bytes > MAXIMUM_SCHEMA_STRING_BYTES {
            return Err(PyValueError::new_err(format!(
                "MonoBehaviour schema strings exceed {MAXIMUM_SCHEMA_STRING_BYTES} bytes"
            )));
        }
        extracted.push((
            try_copy_string(type_name.as_ref(), "schema node type name")?,
            try_copy_string(field_name.as_ref(), "schema node field name")?,
            tuple.get_item(2)?.extract()?,
            tuple.get_item(3)?.extract()?,
        ));
    }
    Ok(extracted)
}

fn copy_python_string_list(
    values: Option<&Bound<'_, PyList>>,
    field: &'static str,
) -> PyResult<Vec<String>> {
    let Some(values) = values else {
        return Ok(Vec::new());
    };
    let mut copied = reserve_metadata(values.len(), field)?;
    for value in values.iter() {
        let value = value.cast_into::<PyString>()?;
        let value = value.to_cow()?;
        copied.push(try_copy_string(value.as_ref(), field)?);
    }
    Ok(copied)
}

fn clone_python_references<T>(
    py: Python<'_>,
    values: &[Py<T>],
    field: &'static str,
) -> PyResult<Vec<Py<T>>> {
    let mut cloned = Vec::new();
    cloned
        .try_reserve(values.len())
        .map_err(|error| PyMemoryError::new_err(format!("cannot allocate {field}: {error}")))?;
    cloned.extend(values.iter().map(|value| value.clone_ref(py)));
    Ok(cloned)
}

fn convert_live2d_package_set(
    py: Python<'_>,
    set: Live2dPackageBytesSet,
) -> PyResult<PyLive2dPackageSet> {
    let mut packages = Vec::new();
    packages.try_reserve(set.packages.len()).map_err(|error| {
        PyMemoryError::new_err(format!("cannot allocate Python Live2D packages: {error}"))
    })?;
    for package in set.packages {
        packages.push(convert_live2d_package(py, package)?);
    }
    let mut diagnostics = Vec::new();
    diagnostics
        .try_reserve(set.diagnostics.len())
        .map_err(|error| {
            PyMemoryError::new_err(format!(
                "cannot allocate Python Live2D diagnostics: {error}"
            ))
        })?;
    for diagnostic in set.diagnostics {
        diagnostics.push(Py::new(
            py,
            PyLive2dDiagnostic {
                object: object_key_tuple(diagnostic.object),
                kind: format!("{:?}", diagnostic.kind),
                message: diagnostic.message,
            },
        )?);
    }
    Ok(PyLive2dPackageSet {
        packages,
        diagnostics,
    })
}

fn convert_live2d_package(
    py: Python<'_>,
    package: Live2dPackageBytes,
) -> PyResult<Py<PyLive2dPackage>> {
    let mut textures = Vec::new();
    textures
        .try_reserve(package.textures.len())
        .map_err(|error| {
            PyMemoryError::new_err(format!("cannot allocate Python Live2D textures: {error}"))
        })?;
    for texture in package.textures {
        textures.push(Py::new(
            py,
            PyLive2dTexture {
                file_name: texture.file_name,
                png: texture.png,
            },
        )?);
    }
    let mut expressions = Vec::new();
    expressions
        .try_reserve(package.expressions.len())
        .map_err(|error| {
            PyMemoryError::new_err(format!(
                "cannot allocate Python Live2D expressions: {error}"
            ))
        })?;
    for expression in package.expressions {
        expressions.push(Py::new(
            py,
            PyLive2dExpressionFile {
                name: expression.name,
                file_name: expression.file_name,
                json: expression.json,
            },
        )?);
    }
    let mut motions = Vec::new();
    motions
        .try_reserve(package.motions.len())
        .map_err(|error| {
            PyMemoryError::new_err(format!("cannot allocate Python Live2D motions: {error}"))
        })?;
    for motion in package.motions {
        motions.push(Py::new(
            py,
            PyLive2dMotionFile {
                name: motion.name,
                file_name: motion.file_name,
                json: motion.json,
            },
        )?);
    }
    let pose = package
        .pose
        .map(|file| convert_live2d_json(py, file))
        .transpose()?;
    let physics = package
        .physics
        .map(|file| convert_live2d_json(py, file))
        .transpose()?;
    let display_info = package
        .display_info
        .map(|file| convert_live2d_json(py, file))
        .transpose()?;
    Py::new(
        py,
        PyLive2dPackage {
            model: object_key_tuple(package.model),
            moc_object: object_key_tuple(package.moc_object),
            name: package.name,
            directory_name: package.directory_name,
            moc_file_name: package.moc_file_name,
            moc: package.moc,
            manifest_file_name: package.manifest_file_name,
            manifest: package.manifest,
            textures,
            expressions,
            motions,
            eye_blink_parameters: package.eye_blink_parameters,
            lip_sync_parameters: package.lip_sync_parameters,
            physics,
            pose,
            display_info,
        },
    )
}

fn convert_live2d_json(
    py: Python<'_>,
    file: unity_rs_core::live2d_package::Live2dPackageJsonFile,
) -> PyResult<Py<PyLive2dJsonFile>> {
    Py::new(
        py,
        PyLive2dJsonFile {
            file_name: file.file_name,
            json: file.bytes,
        },
    )
}

fn prepare_scene_nodes(nodes: Vec<SceneHierarchyNode>) -> PyResult<Vec<PySceneNode>> {
    let mut output = reserve_metadata(nodes.len(), "Python scene nodes")?;
    for node in nodes {
        output.push(python_scene_node(node)?);
    }
    Ok(output)
}

fn python_scene_node(node: SceneHierarchyNode) -> PyResult<PySceneNode> {
    let (local_position, local_rotation, local_scale) =
        node.transform
            .as_ref()
            .map_or((None, None, None), |transform| {
                (
                    Some((
                        transform.local_position.x,
                        transform.local_position.y,
                        transform.local_position.z,
                    )),
                    Some((
                        transform.local_rotation.x,
                        transform.local_rotation.y,
                        transform.local_rotation.z,
                        transform.local_rotation.w,
                    )),
                    Some((
                        transform.local_scale.x,
                        transform.local_scale.y,
                        transform.local_scale.z,
                    )),
                )
            });
    let mesh = node
        .skinned_mesh_renderer
        .as_ref()
        .and_then(|renderer| renderer.mesh)
        .or_else(|| node.mesh_filter.as_ref().and_then(|filter| filter.mesh));
    let materials = node
        .skinned_mesh_renderer
        .as_ref()
        .map(|renderer| renderer.materials.as_slice())
        .or_else(|| {
            node.mesh_renderer
                .as_ref()
                .map(|renderer| renderer.materials.as_slice())
        })
        .unwrap_or_default();
    let bones = node
        .skinned_mesh_renderer
        .as_ref()
        .map_or(&[][..], |renderer| renderer.bones.as_slice());
    Ok(PySceneNode {
        file_index: node.object.file_index,
        path_id: node.object.path_id,
        name: node.name,
        parent: node.parent.map(object_key_tuple),
        children: copy_scene_keys(&node.children, "scene child references")?,
        local_position,
        local_rotation,
        local_scale,
        mesh: mesh.map(object_key_tuple),
        materials: copy_optional_scene_keys(materials, "scene material references")?,
        bones: copy_optional_scene_keys(bones, "scene bone references")?,
        animator: node
            .animator
            .map(|animator| object_key_tuple(animator.component)),
    })
}

fn copy_scene_keys(source: &[SceneObjectKey], field: &str) -> PyResult<Vec<(usize, i64)>> {
    let mut output = Vec::new();
    output
        .try_reserve_exact(source.len())
        .map_err(|error| PyMemoryError::new_err(format!("cannot allocate {field}: {error}")))?;
    output.extend(source.iter().copied().map(object_key_tuple));
    Ok(output)
}

fn copy_optional_scene_keys(
    source: &[Option<SceneObjectKey>],
    field: &str,
) -> PyResult<Vec<Option<(usize, i64)>>> {
    let mut output = Vec::new();
    output
        .try_reserve_exact(source.len())
        .map_err(|error| PyMemoryError::new_err(format!("cannot allocate {field}: {error}")))?;
    output.extend(
        source
            .iter()
            .copied()
            .map(|value| value.map(object_key_tuple)),
    );
    Ok(output)
}

impl From<ModelExportCandidate> for PyFbxCandidate {
    fn from(candidate: ModelExportCandidate) -> Self {
        Self {
            file_index: candidate.game_object.file_index,
            path_id: candidate.game_object.path_id,
            animator: candidate
                .animator
                .map(|animator| (animator.file_index, animator.path_id)),
            name: candidate.name,
        }
    }
}

fn python_fbx_candidates(
    candidates: Vec<ModelExportCandidate>,
    field: &'static str,
) -> PyResult<Vec<PyFbxCandidate>> {
    let mut output = reserve_metadata(candidates.len(), field)?;
    output.extend(candidates.into_iter().map(PyFbxCandidate::from));
    Ok(output)
}

fn prepare_export_report(report: ExportReport) -> PyResult<PyExportReport> {
    let mut exported = reserve_metadata(report.exported.len(), "Python exported paths")?;
    for record in report.exported {
        exported.push(try_path_string(
            &record.output_path,
            "exported output path",
        )?);
    }
    let failures = python_export_failures(report.failures, "Python export failures")?;
    let unsupported = python_export_failures(report.unsupported, "Python unsupported exports")?;
    Ok(PyExportReport {
        exported,
        failures,
        unsupported,
    })
}

fn python_export_failures(
    failures: Vec<unity_rs_core::export::ExportFailure>,
    field: &'static str,
) -> PyResult<Vec<String>> {
    let mut output = reserve_metadata(failures.len(), field)?;
    for failure in failures {
        let capacity = failure
            .source
            .len()
            .checked_add(failure.error.len())
            .and_then(|length| length.checked_add(96))
            .ok_or_else(|| PyValueError::new_err("export failure text length overflowed"))?;
        let mut description = String::new();
        description.try_reserve_exact(capacity).map_err(|error| {
            PyMemoryError::new_err(format!("cannot allocate export failure text: {error}"))
        })?;
        write!(
            description,
            "{}::{} (class {}): {}",
            failure.source, failure.path_id, failure.class_id, failure.error
        )
        .map_err(|error| PyValueError::new_err(format!("cannot format export failure: {error}")))?;
        output.push(description);
    }
    Ok(output)
}

const fn object_key_tuple(key: SceneObjectKey) -> (usize, i64) {
    (key.file_index, key.path_id)
}

const MAXIMUM_OPTION_DIAGNOSTIC_BYTES: usize = 64;

fn unsupported_option(field: &str, value: &str) -> PyErr {
    if value.len() <= MAXIMUM_OPTION_DIAGNOSTIC_BYTES {
        PyValueError::new_err(format!("unsupported {field} {value:?}"))
    } else {
        PyValueError::new_err(format!(
            "unsupported {field} value of {} UTF-8 bytes",
            value.len()
        ))
    }
}

fn parse_export_mode(value: &str) -> PyResult<ExportMode> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("auto") {
        Ok(ExportMode::Auto)
    } else if value.eq_ignore_ascii_case("raw") {
        Ok(ExportMode::Raw)
    } else if value.eq_ignore_ascii_case("typetree_json") || value.eq_ignore_ascii_case("json") {
        Ok(ExportMode::TypeTreeJson)
    } else if value.eq_ignore_ascii_case("dump_text") || value.eq_ignore_ascii_case("dump") {
        Ok(ExportMode::DumpText)
    } else {
        Err(unsupported_option("export mode", value))
    }
}

fn checked_element_count(
    mut counts: impl Iterator<Item = usize>,
    field: &'static str,
) -> PyResult<usize> {
    counts.try_fold(0_usize, |total, count| {
        total
            .checked_add(count)
            .ok_or_else(|| PyValueError::new_err(format!("{field} count overflowed")))
    })
}

fn copy_motion_targets(source: &PyCubismMotionTargets) -> PyResult<CubismMotionTargetNames> {
    Ok(CubismMotionTargetNames {
        parameters: copy_strings(&source.parameters, "Cubism parameter targets")?,
        parts: copy_strings(&source.parts, "Cubism part targets")?,
    })
}

fn copy_strings(source: &[String], field: &str) -> PyResult<Vec<String>> {
    let mut output = Vec::new();
    output
        .try_reserve(source.len())
        .map_err(|error| PyMemoryError::new_err(format!("cannot allocate {field}: {error}")))?;
    for value in source {
        let mut copy = String::new();
        copy.try_reserve_exact(value.len()).map_err(|error| {
            PyMemoryError::new_err(format!("cannot allocate {field} string: {error}"))
        })?;
        copy.push_str(value);
        output.push(copy);
    }
    Ok(output)
}

fn parse_image_format(value: &str) -> PyResult<ImageFormat> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("jpg") || value.eq_ignore_ascii_case("jpeg") {
        Ok(ImageFormat::Jpeg)
    } else if value.eq_ignore_ascii_case("png") {
        Ok(ImageFormat::Png)
    } else if value.eq_ignore_ascii_case("bmp") {
        Ok(ImageFormat::Bmp)
    } else if value.eq_ignore_ascii_case("tga") {
        Ok(ImageFormat::Tga)
    } else if value.eq_ignore_ascii_case("webp") {
        Ok(ImageFormat::Webp)
    } else if value.eq_ignore_ascii_case("raw_rgba")
        || value.eq_ignore_ascii_case("raw-rgba")
        || value.eq_ignore_ascii_case("rgba")
    {
        Ok(ImageFormat::RawRgba)
    } else {
        Err(unsupported_option("image format", value))
    }
}

fn parse_audio_format(value: &str) -> PyResult<AudioExportFormat> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("auto") {
        Ok(AudioExportFormat::Auto)
    } else if value.eq_ignore_ascii_case("raw") || value.eq_ignore_ascii_case("none") {
        Ok(AudioExportFormat::Raw)
    } else if value.eq_ignore_ascii_case("wav") || value.eq_ignore_ascii_case("wave") {
        Ok(AudioExportFormat::Wav)
    } else {
        Err(unsupported_option("audio format", value))
    }
}

fn simple_asset_limits(maximum_bytes: u64) -> SimpleAssetReadLimits {
    SimpleAssetReadLimits {
        maximum_payload_bytes: maximum_bytes,
        ..SimpleAssetReadLimits::default()
    }
}

fn animation_component_limits(maximum_bytes: u64) -> PyResult<AnimationComponentReadLimits> {
    let maximum_usize = usize::try_from(maximum_bytes)
        .map_err(|_| PyValueError::new_err("maximum_bytes does not fit this platform"))?;
    let maximum_clips = maximum_usize / std::mem::size_of::<AnimationClipOverride>();
    Ok(AnimationComponentReadLimits {
        maximum_object_bytes: maximum_bytes,
        maximum_string_bytes: maximum_usize,
        maximum_clips: maximum_clips.min(AnimationComponentReadLimits::default().maximum_clips),
        maximum_reference_bytes: maximum_bytes,
    })
}

const fn container_metadata_limits(
    maximum_entries: usize,
    maximum_string_bytes: usize,
    maximum_total_string_bytes: usize,
) -> ContainerMetadataReadLimits {
    ContainerMetadataReadLimits {
        maximum_preload_references: maximum_entries,
        maximum_container_entries: maximum_entries,
        maximum_dependencies: maximum_entries,
        maximum_class_version_entries: maximum_entries,
        maximum_string_bytes,
        maximum_total_string_bytes,
    }
}

const fn object_reference_tuple(
    reference: unity_rs_core::serialized::ObjectReference,
) -> PyObjectReference {
    (reference.file_id, reference.path_id)
}

fn prepare_sprite_atlas(atlas: SpriteAtlas) -> PyResult<PreparedSpriteAtlas> {
    let mut packed_sprites = reserve_metadata(
        atlas.packed_sprites.len(),
        "Python SpriteAtlas packed sprite references",
    )?;
    for reference in atlas.packed_sprites {
        packed_sprites.push(object_reference_tuple(reference));
    }

    let mut render_data_entries = reserve_metadata(
        atlas.render_data_entries.len(),
        "Python SpriteAtlas render-data entries",
    )?;
    for entry in atlas.render_data_entries {
        let settings = entry.data.settings;
        let secondary_textures = entry
            .data
            .secondary_textures
            .map(|textures| {
                let mut output =
                    reserve_metadata(textures.len(), "Python SpriteAtlas secondary textures")?;
                for texture in textures {
                    output.push(PySpriteAtlasSecondaryTexture {
                        texture: object_reference_tuple(texture.texture),
                        name: texture.name,
                    });
                }
                Ok::<_, PyErr>(output)
            })
            .transpose()?;
        render_data_entries.push(PreparedSpriteAtlasRenderData {
            key: PySpriteAtlasRenderDataKey {
                guid_bytes: entry.key.guid_bytes,
                value: entry.key.value,
            },
            texture: object_reference_tuple(entry.data.texture),
            alpha_texture: object_reference_tuple(entry.data.alpha_texture),
            texture_rect: (
                entry.data.texture_rect.x,
                entry.data.texture_rect.y,
                entry.data.texture_rect.width,
                entry.data.texture_rect.height,
            ),
            texture_rect_offset: (
                entry.data.texture_rect_offset.x,
                entry.data.texture_rect_offset.y,
            ),
            atlas_rect_offset: (
                entry.data.atlas_rect_offset.x,
                entry.data.atlas_rect_offset.y,
            ),
            uv_transform: (
                entry.data.uv_transform.x,
                entry.data.uv_transform.y,
                entry.data.uv_transform.z,
                entry.data.uv_transform.w,
            ),
            downscale_multiplier: entry.data.downscale_multiplier,
            settings_raw: settings.raw,
            packed: settings.packed(),
            packing_mode: settings.packing_mode(),
            packing_rotation: settings.packing_rotation(),
            mesh_type: settings.mesh_type(),
            secondary_textures,
        });
    }

    Ok(PreparedSpriteAtlas {
        path_id: atlas.path_id,
        name: atlas.name,
        packed_sprites,
        packed_sprite_names: atlas.packed_sprite_names,
        render_data_entries,
        tag: atlas.tag,
        is_variant: atlas.is_variant,
    })
}

fn python_sprite_atlas(py: Python<'_>, atlas: PreparedSpriteAtlas) -> PyResult<PySpriteAtlas> {
    let mut render_data_entries = reserve_metadata(
        atlas.render_data_entries.len(),
        "Python SpriteAtlas render-data entries",
    )?;
    for entry in atlas.render_data_entries {
        let key = Py::new(py, entry.key)?;
        let secondary_textures = entry
            .secondary_textures
            .map(|textures| {
                let mut output =
                    reserve_metadata(textures.len(), "Python SpriteAtlas secondary textures")?;
                for texture in textures {
                    output.push(Py::new(py, texture)?);
                }
                Ok::<_, PyErr>(output)
            })
            .transpose()?;
        render_data_entries.push(Py::new(
            py,
            PySpriteAtlasRenderData {
                key,
                texture: entry.texture,
                alpha_texture: entry.alpha_texture,
                texture_rect: entry.texture_rect,
                texture_rect_offset: entry.texture_rect_offset,
                atlas_rect_offset: entry.atlas_rect_offset,
                uv_transform: entry.uv_transform,
                downscale_multiplier: entry.downscale_multiplier,
                settings_raw: entry.settings_raw,
                packed: entry.packed,
                packing_mode: entry.packing_mode,
                packing_rotation: entry.packing_rotation,
                mesh_type: entry.mesh_type,
                secondary_textures,
            },
        )?);
    }

    Ok(PySpriteAtlas {
        path_id: atlas.path_id,
        name: atlas.name,
        packed_sprites: atlas.packed_sprites,
        packed_sprite_names: atlas.packed_sprite_names,
        render_data_entries,
        tag: atlas.tag,
        is_variant: atlas.is_variant,
    })
}

const fn sprite_object_reference_tuple(
    reference: unity_rs_core::sprite::ObjectReference,
) -> PyObjectReference {
    (reference.file_id, reference.path_id)
}

fn convert_sprite_metadata(py: Python<'_>, sprite: Sprite) -> PyResult<PySpriteMetadata> {
    let Sprite {
        object_index,
        path_id,
        name,
        rect,
        offset,
        border,
        pixels_to_units,
        pivot,
        extrude,
        is_polygon,
        render_data_key,
        atlas_tags,
        sprite_atlas,
        render_data,
    } = sprite;
    let key = render_data_key
        .map(|(guid_bytes, value)| Py::new(py, PySpriteAtlasRenderDataKey { guid_bytes, value }))
        .transpose()?;
    let unity_rs_core::sprite::SpriteRenderData {
        texture,
        alpha_texture,
        secondary_textures,
        texture_rect,
        texture_rect_offset,
        atlas_rect_offset,
        settings,
        uv_transform,
        downscale_multiplier,
        mesh_triangles,
    } = render_data;
    let mut python_secondary =
        reserve_metadata(secondary_textures.len(), "Python Sprite secondary textures")?;
    for secondary in secondary_textures {
        python_secondary.push(Py::new(
            py,
            PySpriteSecondaryTexture {
                texture: sprite_object_reference_tuple(secondary.texture),
                name: secondary.name,
            },
        )?);
    }
    let mut python_triangles =
        reserve_metadata(mesh_triangles.len(), "Python Sprite mesh triangles")?;
    for [first, second, third] in mesh_triangles {
        python_triangles.push(((first.x, first.y), (second.x, second.y), (third.x, third.y)));
    }
    let python_settings = Py::new(
        py,
        PySpriteSettings {
            raw: settings.raw,
            packed: settings.packed,
            packing_mode_tight: settings.packing_mode == SpritePackingMode::Tight,
            packing_rotation: settings.packing_rotation,
            mesh_type_tight: settings.mesh_type == SpriteMeshType::Tight,
        },
    )?;
    let python_render_data = Py::new(
        py,
        PySpriteRenderData {
            texture: sprite_object_reference_tuple(texture),
            alpha_texture: sprite_object_reference_tuple(alpha_texture),
            secondary_textures: python_secondary,
            texture_rect: (
                texture_rect.x,
                texture_rect.y,
                texture_rect.width,
                texture_rect.height,
            ),
            texture_rect_offset: (texture_rect_offset.x, texture_rect_offset.y),
            atlas_rect_offset: (atlas_rect_offset.x, atlas_rect_offset.y),
            settings: python_settings,
            uv_transform: (
                uv_transform.x,
                uv_transform.y,
                uv_transform.z,
                uv_transform.w,
            ),
            downscale_multiplier,
            mesh_triangles: python_triangles,
        },
    )?;
    Ok(PySpriteMetadata {
        object_index,
        path_id,
        name,
        rect: (rect.x, rect.y, rect.width, rect.height),
        offset: (offset.x, offset.y),
        border: (border.x, border.y, border.z, border.w),
        pixels_to_units,
        pivot: (pivot.x, pivot.y),
        extrude,
        is_polygon,
        render_data_key: key,
        atlas_tags,
        sprite_atlas: sprite_object_reference_tuple(sprite_atlas),
        render_data: python_render_data,
    })
}

fn materialize_binary_asset(
    asset: SimpleBinaryAsset,
    maximum_bytes: u64,
) -> PyResult<PyBinaryAsset> {
    let SimpleBinaryAsset {
        name,
        payload,
        payload_kind,
        suggested_extension,
        ..
    } = asset;
    let bytes = payload.read_to_vec(maximum_bytes).map_err(core_error)?;
    Ok(PyBinaryAsset {
        name,
        extension: suggested_extension,
        payload_kind,
        bytes,
    })
}

fn convert_material(material: Material) -> PyResult<PyMaterial> {
    let Material {
        path_id,
        name,
        shader,
        legacy_shader_keywords,
        valid_keywords,
        invalid_keywords,
        lightmap_flags,
        enable_instancing_variants,
        custom_render_queue,
        string_tags,
        disabled_shader_passes,
        saved_properties,
        trailing_bytes,
    } = material;
    let mut texture_environments = reserve_python_values(
        saved_properties.texture_environments.len(),
        "Material texture environments",
    )?;
    for property in saved_properties.texture_environments {
        texture_environments.push((
            property.name,
            (
                property.value.texture.file_id,
                property.value.texture.path_id,
            ),
            (property.value.scale[0], property.value.scale[1]),
            (property.value.offset[0], property.value.offset[1]),
        ));
    }
    let integers = convert_named_material_properties(saved_properties.integers, "integers")?;
    let floats = convert_named_material_properties(saved_properties.floats, "floats")?;
    let mut colors = reserve_python_values(saved_properties.colors.len(), "Material colors")?;
    for property in saved_properties.colors {
        colors.push((
            property.name,
            (
                property.value[0],
                property.value[1],
                property.value[2],
                property.value[3],
            ),
        ));
    }
    Ok(PyMaterial {
        path_id,
        name,
        shader: (shader.file_id, shader.path_id),
        legacy_shader_keywords,
        valid_keywords,
        invalid_keywords,
        lightmap_flags,
        enable_instancing_variants,
        custom_render_queue,
        string_tags,
        disabled_shader_passes,
        texture_environments,
        integers,
        floats,
        colors,
        trailing_bytes,
    })
}

fn convert_mono_script(script: MonoScript) -> PyMonoScript {
    PyMonoScript {
        path_id: script.path_id,
        name: script.name,
        execution_order: script.execution_order,
        class_name: script.class_name,
        namespace: script.namespace,
        assembly_name: script.assembly_name,
        is_editor_script: script.is_editor_script,
    }
}

fn convert_named_material_properties<T>(
    properties: Vec<NamedMaterialProperty<T>>,
    field: &str,
) -> PyResult<Vec<(String, T)>> {
    let mut output = reserve_python_values(properties.len(), field)?;
    for property in properties {
        output.push((property.name, property.value));
    }
    Ok(output)
}

fn reserve_python_values<T>(count: usize, field: &str) -> PyResult<Vec<T>> {
    let mut output = Vec::new();
    output.try_reserve_exact(count).map_err(|error| {
        PyMemoryError::new_err(format!("cannot allocate {count} {field}: {error}"))
    })?;
    Ok(output)
}

fn materialize_audio_clip(
    audio: AudioClipAsset,
    format: AudioExportFormat,
    maximum_bytes: u64,
) -> PyResult<PyAudioClip> {
    let AudioClipAsset {
        name,
        payload,
        raw_extension,
        direct_wav,
        ..
    } = audio;
    let wav_kind = match format {
        AudioExportFormat::Auto => direct_wav,
        AudioExportFormat::Raw => None,
        AudioExportFormat::Wav => Some(direct_wav.ok_or_else(|| {
            PyNotImplementedError::new_err(
                "AudioClip uses a compressed or unsupported audio codec and cannot be exported directly as WAV",
            )
        })?),
    };
    if let Some(kind) = wav_kind {
        let expected = direct_wav_output_size(&payload, kind).map_err(core_error)?;
        if expected > maximum_bytes {
            return Err(PyValueError::new_err(format!(
                "WAV output is {expected} bytes, exceeding limit {maximum_bytes}"
            )));
        }
        let expected = usize::try_from(expected)
            .map_err(|_| PyValueError::new_err("WAV output is too large for this platform"))?;
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(expected).map_err(|error| {
            PyMemoryError::new_err(format!("cannot allocate {expected} WAV bytes: {error}"))
        })?;
        write_direct_wav(&payload, kind, maximum_bytes, &mut bytes).map_err(core_error)?;
        Ok(PyAudioClip {
            name,
            extension: ".wav".to_owned(),
            payload_kind: "audio_wav",
            bytes,
        })
    } else {
        let bytes = payload.read_to_vec(maximum_bytes).map_err(core_error)?;
        Ok(PyAudioClip {
            name,
            extension: raw_extension,
            payload_kind: "audio_raw",
            bytes,
        })
    }
}

const fn cubism_expression_blend_name(blend: CubismExpressionBlend) -> &'static str {
    match blend {
        CubismExpressionBlend::Add => "Add",
        CubismExpressionBlend::Multiply => "Multiply",
        CubismExpressionBlend::Overwrite => "Overwrite",
    }
}

fn flip_rgba_rows(image: &mut unity_rs_core::texture::RgbaImage) -> PyResult<()> {
    unity_rs_core::image_export::flip_rgba_rows(image).map_err(core_error)
}

#[pymodule]
fn _native(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add("__version__", env!("CARGO_PKG_VERSION"))?;
    module.add_function(wrap_pyfunction!(extract, module)?)?;
    module.add_class::<PyUnityRs>()?;
    module.add_class::<PyFileIterator>()?;
    module.add_class::<PyObjectIterator>()?;
    module.add_class::<PyResourceIterator>()?;
    module.add_class::<PyFileInfo>()?;
    module.add_class::<PyObjectInfo>()?;
    module.add_class::<PyResourceInfo>()?;
    module.add_class::<PyLoadDiagnostic>()?;
    module.add_class::<PyAnimationClip>()?;
    module.add_class::<PyLegacyAnimation>()?;
    module.add_class::<PyAnimatorOverrideController>()?;
    module.add_class::<PyAssetBundle>()?;
    module.add_class::<PyResourceManager>()?;
    module.add_class::<PyPreloadData>()?;
    module.add_class::<PySpriteAtlasRenderDataKey>()?;
    module.add_class::<PySpriteAtlasSecondaryTexture>()?;
    module.add_class::<PySpriteAtlasRenderData>()?;
    module.add_class::<PySpriteAtlas>()?;
    module.add_class::<PySpriteSettings>()?;
    module.add_class::<PySpriteMetadataLimits>()?;
    module.add_class::<PySpriteSecondaryTexture>()?;
    module.add_class::<PySpriteRenderData>()?;
    module.add_class::<PySpriteMetadata>()?;
    module.add_class::<PyAclCompressedTracks>()?;
    module.add_class::<PyAclDecodedClip>()?;
    module.add_class::<PyAnimatorController>()?;
    module.add_class::<PyAvatar>()?;
    module.add_class::<PyMonoBehaviourSchema>()?;
    module.add_class::<PyMonoBehaviourSchemas>()?;
    module.add_class::<PyBuildSettings>()?;
    module.add_class::<PyPlayerSettings>()?;
    module.add_class::<PyCubismExpressionParameter>()?;
    module.add_class::<PyCubismExpression>()?;
    module.add_class::<PyCubismPosePart>()?;
    module.add_class::<PyCubismDisplayInfo>()?;
    module.add_class::<PyCubismPhysics>()?;
    module.add_class::<PyCubismFadeMotion>()?;
    module.add_class::<PyCubismClipMotion>()?;
    module.add_class::<PyCubismMotionTargets>()?;
    module.add_class::<PySceneNode>()?;
    module.add_class::<PySceneLimits>()?;
    module.add_class::<PyModelTextureLimits>()?;
    module.add_class::<PyFbxCandidate>()?;
    module.add_class::<PyRgbaImage>()?;
    module.add_class::<PyBinaryAsset>()?;
    module.add_class::<PyAudioClip>()?;
    module.add_class::<PyMaterial>()?;
    module.add_class::<PyMonoScript>()?;
    module.add_class::<PyExportReport>()?;
    module.add_class::<PyMonoBehaviourJson>()?;
    module.add_class::<PyModelFile>()?;
    module.add_class::<PyModelObj>()?;
    module.add_class::<PyTexturedFbx>()?;
    module.add_class::<PyExportLimits>()?;
    module.add_class::<PyExtractionLimits>()?;
    module.add_class::<PyExtractionRecord>()?;
    module.add_class::<PyExtractionSkip>()?;
    module.add_class::<PyExtractionFailure>()?;
    module.add_class::<PyExtractionReport>()?;
    module.add_class::<PyLive2dTexture>()?;
    module.add_class::<PyLive2dExpressionFile>()?;
    module.add_class::<PyLive2dMotionFile>()?;
    module.add_class::<PyLive2dJsonFile>()?;
    module.add_class::<PyLive2dPackage>()?;
    module.add_class::<PyLive2dDiagnostic>()?;
    module.add_class::<PyLive2dPackageSet>()?;
    Ok(())
}
