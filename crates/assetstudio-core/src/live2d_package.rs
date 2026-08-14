//! Deterministic planning for the sample-verified part of a Cubism package.
//!
//! Embedded type trees are preferred. A caller may also supply complete trees
//! from a trusted, non-executing external schema provider for stripped
//! `CubismModel`, renderer, expression, motion, physics, pose, and display-info
//! objects. Opaque `MonoBehaviour` tails are never guessed.

use std::collections::{HashMap, HashSet};
use std::io::{self, Write};
use std::sync::Arc;

use crate::acl::AclDecoder;
use crate::animation_graph::{AnimationGraph, AnimationGraphLimits, build_animation_graph};
use crate::cubism_moc::{CubismMoc, CubismMocReadLimits, read_cubism_moc};
use crate::image_export::{ImageFormat, ImageRowOrder, write_rgba_image};
use crate::live2d::CubismRole;
use crate::live2d_clip_motion::{
    CubismClipMotion, CubismClipMotionReadLimits, read_cubism_clip_motion_with_acl_decoder,
};
use crate::live2d_motion::{
    CubismFadeMotion, CubismFadeMotionReadLimits, CubismMotionTargetNames,
    project_cubism_fade_motion,
};
use crate::live2d_physics::{CubismPhysicsReadLimits, project_cubism_physics};
use crate::live2d_schema::{
    CubismAuxiliaryReadLimits, CubismDisplayEntry, CubismExpression, CubismExpressionReadLimits,
    CubismPoseNode, project_cubism_display_info, project_cubism_expression,
    project_cubism_pose_part, write_cubism_cdi3_json, write_cubism_pose3_json,
};
use crate::loader::AssetCollection;
use crate::mono_schema::{MonoBehaviourSchemaProvider, read_mono_behaviour_value_with_provider};
use crate::monobehaviour::{
    MONO_BEHAVIOUR_CLASS_ID, MONO_SCRIPT_CLASS_ID, MonoBehaviour, MonoBehaviourReadLimits,
    read_mono_behaviour, read_mono_script,
};
use crate::scene::{
    GAME_OBJECT_CLASS_ID, RECT_TRANSFORM_CLASS_ID, TRANSFORM_CLASS_ID, read_game_object,
    resolve_object_reference,
};
use crate::scene_hierarchy::{
    SceneHierarchy, SceneHierarchyLimits, SceneObjectKey, build_scene_hierarchy,
};
use crate::serialized::ObjectReference;
use crate::texture::{TEXTURE_2D_CLASS_ID, Texture2D, TextureReadLimits, read_texture2d};
use crate::type_tree::{TypeTreeReadLimits, TypeValue};
use crate::{Error, Result};

/// Collection-wide budgets for `Live2D` package planning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Live2dPackageLimits {
    pub maximum_mono_behaviours: usize,
    pub maximum_scripts: usize,
    pub maximum_models: usize,
    pub maximum_renderers: usize,
    pub maximum_textures: usize,
    pub maximum_expressions: usize,
    pub maximum_motions: usize,
    pub maximum_pose_parts: usize,
    pub maximum_display_entries: usize,
    pub maximum_type_tree_reads: usize,
    pub maximum_total_type_tree_object_bytes: u64,
    pub maximum_total_moc_bytes: u64,
    pub maximum_total_moc_metadata_bytes: u64,
    pub maximum_total_texture_object_bytes: u64,
    pub maximum_total_texture_payload_bytes: u64,
    pub maximum_total_expression_bytes: u64,
    pub maximum_total_motion_bytes: u64,
    pub maximum_total_physics_bytes: u64,
    pub maximum_total_auxiliary_bytes: u64,
    pub maximum_auxiliary_file_bytes: u64,
    pub maximum_total_name_bytes: usize,
    pub maximum_total_diagnostic_bytes: usize,
    pub maximum_total_manifest_bytes: u64,
    pub mono_behaviour: MonoBehaviourReadLimits,
    pub moc: CubismMocReadLimits,
    pub texture: TextureReadLimits,
    pub expression: CubismExpressionReadLimits,
    pub motion: CubismFadeMotionReadLimits,
    pub clip_motion: CubismClipMotionReadLimits,
    pub animation_graph: AnimationGraphLimits,
    pub force_bezier_motions: bool,
    pub physics: CubismPhysicsReadLimits,
    pub auxiliary: CubismAuxiliaryReadLimits,
    pub type_tree: TypeTreeReadLimits,
    pub hierarchy: SceneHierarchyLimits,
}

impl Default for Live2dPackageLimits {
    fn default() -> Self {
        Self {
            maximum_mono_behaviours: 2_000_000,
            maximum_scripts: 1_000_000,
            maximum_models: 100_000,
            maximum_renderers: 2_000_000,
            maximum_textures: 1_000_000,
            maximum_expressions: 1_000_000,
            maximum_motions: 1_000_000,
            maximum_pose_parts: 1_000_000,
            maximum_display_entries: 1_000_000,
            maximum_type_tree_reads: 2_000_000,
            maximum_total_type_tree_object_bytes: 1024 * 1024 * 1024,
            maximum_total_moc_bytes: 4 * 1024 * 1024 * 1024,
            maximum_total_moc_metadata_bytes: 512 * 1024 * 1024,
            maximum_total_texture_object_bytes: 1024 * 1024 * 1024,
            maximum_total_texture_payload_bytes: 4 * 1024 * 1024 * 1024,
            maximum_total_expression_bytes: 1024 * 1024 * 1024,
            maximum_total_motion_bytes: 2 * 1024 * 1024 * 1024,
            maximum_total_physics_bytes: 1024 * 1024 * 1024,
            maximum_total_auxiliary_bytes: 1024 * 1024 * 1024,
            maximum_auxiliary_file_bytes: 256 * 1024 * 1024,
            maximum_total_name_bytes: 256 * 1024 * 1024,
            maximum_total_diagnostic_bytes: 64 * 1024 * 1024,
            maximum_total_manifest_bytes: 256 * 1024 * 1024,
            mono_behaviour: MonoBehaviourReadLimits::default(),
            moc: CubismMocReadLimits::default(),
            texture: TextureReadLimits::default(),
            expression: CubismExpressionReadLimits::default(),
            motion: CubismFadeMotionReadLimits::default(),
            clip_motion: CubismClipMotionReadLimits::default(),
            animation_graph: AnimationGraphLimits::default(),
            force_bezier_motions: false,
            physics: CubismPhysicsReadLimits::default(),
            auxiliary: CubismAuxiliaryReadLimits::default(),
            type_tree: TypeTreeReadLimits::default(),
            hierarchy: SceneHierarchyLimits::default(),
        }
    }
}

/// Why an otherwise recognized `Live2D` component could not be packaged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Live2dPackageDiagnosticKind {
    MissingEmbeddedTypeTree,
    MalformedEmbeddedTypeTree,
    MissingExternalSchema,
    MalformedExternalSchema,
    MissingRequiredField,
    MalformedRequiredField,
    NullReference,
    UnresolvedReference,
    WrongReferenceClass,
    WrongCubismRole,
    TextureReadFailed,
    ExpressionReadFailed,
    MotionReadFailed,
    PhysicsReadFailed,
    AuxiliaryReadFailed,
    DetachedRenderer,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Live2dPackageDiagnostic {
    pub object: SceneObjectKey,
    pub kind: Live2dPackageDiagnosticKind,
    pub message: String,
}

/// One source-bound texture and the exact relative file name used by model3.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Live2dPackageTexture {
    pub object: SceneObjectKey,
    pub source_name: String,
    pub file_name: String,
    pub texture: Texture2D,
}

/// One verified expression and its relative `exp3.json` file name.
#[derive(Debug, Clone, PartialEq)]
pub struct Live2dPackageExpression {
    pub object: SceneObjectKey,
    pub name: String,
    pub file_name: String,
    pub expression: CubismExpression,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Live2dPackageMotion {
    pub object: SceneObjectKey,
    pub name: String,
    pub file_name: String,
    pub motion: Live2dPackageMotionSource,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Live2dPackageMotionSource {
    Fade(CubismFadeMotion),
    AnimationClip(CubismClipMotion),
}

impl Live2dPackageMotionSource {
    pub fn write_motion3_json(
        &self,
        targets: &CubismMotionTargetNames,
        force_bezier: bool,
        output: &mut impl Write,
        maximum_bytes: u64,
    ) -> Result<u64> {
        match self {
            Self::Fade(motion) => {
                motion.write_motion3_json(targets, force_bezier, output, maximum_bytes)
            }
            Self::AnimationClip(motion) => {
                motion.write_motion3_json(force_bezier, output, maximum_bytes)
            }
        }
    }

    #[must_use]
    pub const fn fps(&self) -> f64 {
        match self {
            Self::Fade(_) => 30.0,
            Self::AnimationClip(motion) => motion.fps,
        }
    }
}

/// One already bounded auxiliary JSON file in a package plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Live2dPackageJsonFile {
    pub file_name: String,
    pub bytes: Vec<u8>,
}

/// Sample-verified package IR built from native fields and embedded schemas.
#[derive(Debug, Clone, PartialEq)]
pub struct Live2dPackage {
    pub model: SceneObjectKey,
    pub model_game_object: Option<SceneObjectKey>,
    pub moc_object: SceneObjectKey,
    pub name: String,
    pub directory_name: String,
    pub moc_file_name: String,
    pub manifest_file_name: String,
    pub moc: CubismMoc,
    pub textures: Vec<Live2dPackageTexture>,
    pub expressions: Vec<Live2dPackageExpression>,
    pub motions: Vec<Live2dPackageMotion>,
    pub motion_targets: CubismMotionTargetNames,
    pub eye_blink_parameters: Vec<String>,
    pub lip_sync_parameters: Vec<String>,
    pub force_bezier_motions: bool,
    pub physics: Option<Live2dPackageJsonFile>,
    pub pose: Option<Live2dPackageJsonFile>,
    pub display_info: Option<Live2dPackageJsonFile>,
}

impl Live2dPackage {
    /// Writes a deterministic Cubism 3 manifest containing only verified files.
    pub fn write_model3_json<W: Write>(
        &self,
        output: &mut W,
        maximum_output_bytes: u64,
    ) -> Result<u64> {
        let mut writer = BoundedWriter::new(output, maximum_output_bytes);
        writer.write_all(b"{\n  \"Version\": 3,\n  \"Name\": ")?;
        serde_json::to_writer(&mut writer, &self.name).map_err(|error| json_error(&error))?;
        writer.write_all(b",\n  \"FileReferences\": {\n    \"Moc\": ")?;
        serde_json::to_writer(&mut writer, &self.moc_file_name)
            .map_err(|error| json_error(&error))?;
        writer.write_all(b",\n    \"Textures\": [")?;
        for (index, texture) in self.textures.iter().enumerate() {
            if index == 0 {
                writer.write_all(b"\n      ")?;
            } else {
                writer.write_all(b",\n      ")?;
            }
            serde_json::to_writer(&mut writer, &texture.file_name)
                .map_err(|error| json_error(&error))?;
        }
        if !self.textures.is_empty() {
            writer.write_all(b"\n    ")?;
        }
        writer.write_all(b"]")?;
        if let Some(pose) = &self.pose {
            writer.write_all(b",\n    \"Pose\": ")?;
            serde_json::to_writer(&mut writer, &pose.file_name)
                .map_err(|error| json_error(&error))?;
        }
        if let Some(physics) = &self.physics {
            writer.write_all(b",\n    \"Physics\": ")?;
            serde_json::to_writer(&mut writer, &physics.file_name)
                .map_err(|error| json_error(&error))?;
        }
        if let Some(display_info) = &self.display_info {
            writer.write_all(b",\n    \"DisplayInfo\": ")?;
            serde_json::to_writer(&mut writer, &display_info.file_name)
                .map_err(|error| json_error(&error))?;
        }
        if !self.expressions.is_empty() {
            writer.write_all(b",\n    \"Expressions\": [")?;
            for (index, expression) in self.expressions.iter().enumerate() {
                if index == 0 {
                    writer.write_all(b"\n      { \"Name\": ")?;
                } else {
                    writer.write_all(b",\n      { \"Name\": ")?;
                }
                serde_json::to_writer(&mut writer, &expression.name)
                    .map_err(|error| json_error(&error))?;
                writer.write_all(b", \"File\": ")?;
                serde_json::to_writer(&mut writer, &expression.file_name)
                    .map_err(|error| json_error(&error))?;
                writer.write_all(b" }")?;
            }
            writer.write_all(b"\n    ]")?;
        }
        if !self.motions.is_empty() {
            writer.write_all(b",\n    \"Motions\": {")?;
            for (index, motion) in self.motions.iter().enumerate() {
                if index == 0 {
                    writer.write_all(b"\n      ")?;
                } else {
                    writer.write_all(b",\n      ")?;
                }
                serde_json::to_writer(&mut writer, &motion.name)
                    .map_err(|error| json_error(&error))?;
                writer.write_all(b": [ { \"File\": ")?;
                serde_json::to_writer(&mut writer, &motion.file_name)
                    .map_err(|error| json_error(&error))?;
                writer.write_all(b" } ]")?;
            }
            writer.write_all(b"\n    }")?;
        }
        writer.write_all(b"\n  },\n  \"Groups\": [")?;
        write_parameter_group(&mut writer, "EyeBlink", &self.eye_blink_parameters, true)?;
        write_parameter_group(&mut writer, "LipSync", &self.lip_sync_parameters, false)?;
        writer.write_all(b"\n  ]\n}\n")?;
        Ok(writer.written)
    }
}

fn write_parameter_group<W: Write>(
    writer: &mut BoundedWriter<'_, W>,
    name: &str,
    ids: &[String],
    first: bool,
) -> Result<()> {
    writer.write_all(if first {
        b"\n    { \"Target\": \"Parameter\", \"Name\": "
    } else {
        b",\n    { \"Target\": \"Parameter\", \"Name\": "
    })?;
    serde_json::to_writer(&mut *writer, name).map_err(|error| json_error(&error))?;
    writer.write_all(b", \"Ids\": [")?;
    for (index, id) in ids.iter().enumerate() {
        if index != 0 {
            writer.write_all(b", ")?;
        }
        serde_json::to_writer(&mut *writer, id).map_err(|error| json_error(&error))?;
    }
    writer.write_all(b"] }")?;
    Ok(())
}

#[derive(Debug, Clone, Default)]
pub struct Live2dPackageSet {
    pub packages: Vec<Live2dPackage>,
    pub diagnostics: Vec<Live2dPackageDiagnostic>,
}

/// Output budgets for materializing packages in memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Live2dPackageMaterializeLimits {
    pub maximum_file_bytes: u64,
    pub maximum_total_bytes: u64,
    pub texture: TextureReadLimits,
}

impl Default for Live2dPackageMaterializeLimits {
    fn default() -> Self {
        Self {
            maximum_file_bytes: 512 * 1024 * 1024,
            maximum_total_bytes: 4 * 1024 * 1024 * 1024,
            texture: TextureReadLimits::default(),
        }
    }
}

/// One materialized PNG file in a static package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Live2dPackageTextureBytes {
    pub file_name: String,
    pub png: Vec<u8>,
}

/// One materialized expression file in a package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Live2dPackageExpressionBytes {
    pub name: String,
    pub file_name: String,
    pub json: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Live2dPackageMotionBytes {
    pub name: String,
    pub file_name: String,
    pub json: Vec<u8>,
}

/// A fully materialized package suitable for language bindings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Live2dPackageBytes {
    pub model: SceneObjectKey,
    pub moc_object: SceneObjectKey,
    pub name: String,
    pub directory_name: String,
    pub moc_file_name: String,
    pub moc: Vec<u8>,
    pub manifest_file_name: String,
    pub manifest: Vec<u8>,
    pub textures: Vec<Live2dPackageTextureBytes>,
    pub expressions: Vec<Live2dPackageExpressionBytes>,
    pub motions: Vec<Live2dPackageMotionBytes>,
    pub eye_blink_parameters: Vec<String>,
    pub lip_sync_parameters: Vec<String>,
    pub physics: Option<Live2dPackageJsonFile>,
    pub pose: Option<Live2dPackageJsonFile>,
    pub display_info: Option<Live2dPackageJsonFile>,
}

/// Materialized packages plus non-fatal schema/reference diagnostics.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Live2dPackageBytesSet {
    pub packages: Vec<Live2dPackageBytes>,
    pub diagnostics: Vec<Live2dPackageDiagnostic>,
}

/// Builds `Live2D` package plans in serialized-file/object-table order.
pub fn build_live2d_packages(
    collection: &AssetCollection,
    limits: Live2dPackageLimits,
) -> Result<Live2dPackageSet> {
    build_live2d_packages_inner(collection, &limits, None, None)
}

/// Builds package plans while decoding Tuanjie ACL-backed `AnimationClip`s
/// through a caller-supplied safe decoder.
pub fn build_live2d_packages_with_acl_decoder(
    collection: &AssetCollection,
    limits: Live2dPackageLimits,
    acl_decoder: &dyn AclDecoder,
) -> Result<Live2dPackageSet> {
    build_live2d_packages_inner(collection, &limits, None, Some(acl_decoder))
}

/// Builds package plans using embedded trees first and a trusted external
/// full-object schema provider for stripped `MonoBehaviour` types.
pub fn build_live2d_packages_with_schema_provider(
    collection: &AssetCollection,
    limits: Live2dPackageLimits,
    provider: &dyn MonoBehaviourSchemaProvider,
) -> Result<Live2dPackageSet> {
    build_live2d_packages_inner(collection, &limits, Some(provider), None)
}

/// Builds package plans using both a trusted managed schema provider and a
/// caller-supplied Tuanjie ACL decoder.
pub fn build_live2d_packages_with_schema_provider_and_acl_decoder(
    collection: &AssetCollection,
    limits: Live2dPackageLimits,
    provider: &dyn MonoBehaviourSchemaProvider,
    acl_decoder: &dyn AclDecoder,
) -> Result<Live2dPackageSet> {
    build_live2d_packages_inner(collection, &limits, Some(provider), Some(acl_decoder))
}

fn build_live2d_packages_inner<'a>(
    collection: &'a AssetCollection,
    limits: &Live2dPackageLimits,
    provider: Option<&'a dyn MonoBehaviourSchemaProvider>,
    acl_decoder: Option<&'a dyn AclDecoder>,
) -> Result<Live2dPackageSet> {
    let hierarchy = build_scene_hierarchy(collection, limits.hierarchy)?;
    let mut state = PackageState::new(collection, limits, hierarchy, provider, acl_decoder);
    state.discover_components()?;
    state.build_packages()
}

/// Materializes planned MOC, manifest, and mip-zero PNG files under cumulative limits.
pub fn materialize_live2d_packages(
    set: Live2dPackageSet,
    limits: Live2dPackageMaterializeLimits,
) -> Result<Live2dPackageBytesSet> {
    let mut packages = Vec::new();
    packages.try_reserve(set.packages.len()).map_err(|error| {
        Error::invalid_data(format!(
            "cannot allocate materialized Live2D packages: {error}"
        ))
    })?;
    let mut total = 0_u64;
    for package in set.packages {
        let moc = materialize_package_file(&mut total, limits, "Live2D MOC", |output| {
            package.moc.write_moc3(output, limits.maximum_file_bytes)
        })?;
        let manifest =
            materialize_package_file(&mut total, limits, "Live2D model3 manifest", |output| {
                package.write_model3_json(output, limits.maximum_file_bytes)
            })?;
        let mut textures = Vec::new();
        textures
            .try_reserve(package.textures.len())
            .map_err(|error| {
                Error::invalid_data(format!(
                    "cannot allocate materialized Live2D textures: {error}"
                ))
            })?;
        for texture in package.textures {
            let remaining = limits
                .maximum_total_bytes
                .checked_sub(total)
                .ok_or_else(|| {
                    Error::invalid_data("materialized Live2D output accounting underflowed")
                })?;
            let texture_limits = TextureReadLimits {
                maximum_output_bytes: limits.texture.maximum_output_bytes.min(remaining),
                ..limits.texture
            };
            let decoded = texture.texture.decode_mip_rgba8(0, texture_limits)?;
            let png =
                materialize_package_file(&mut total, limits, "Live2D texture PNG", |output| {
                    write_rgba_image(
                        &decoded,
                        ImageFormat::Png,
                        ImageRowOrder::UnityDecoded,
                        limits.maximum_file_bytes,
                        output,
                    )
                })?;
            textures.push(Live2dPackageTextureBytes {
                file_name: texture.file_name,
                png,
            });
        }
        let expressions = materialize_expressions(&mut total, limits, package.expressions)?;
        let motions = materialize_motions(
            &mut total,
            limits,
            package.motions,
            &package.motion_targets,
            package.force_bezier_motions,
        )?;
        let pose = package
            .pose
            .map(|file| materialize_existing_json(&mut total, limits, file))
            .transpose()?;
        let display_info = package
            .display_info
            .map(|file| materialize_existing_json(&mut total, limits, file))
            .transpose()?;
        let physics = package
            .physics
            .map(|file| materialize_existing_json(&mut total, limits, file))
            .transpose()?;
        packages.push(Live2dPackageBytes {
            model: package.model,
            moc_object: package.moc_object,
            name: package.name,
            directory_name: package.directory_name,
            moc_file_name: package.moc_file_name,
            moc,
            manifest_file_name: package.manifest_file_name,
            manifest,
            textures,
            expressions,
            motions,
            eye_blink_parameters: package.eye_blink_parameters,
            lip_sync_parameters: package.lip_sync_parameters,
            physics,
            pose,
            display_info,
        });
    }
    Ok(Live2dPackageBytesSet {
        packages,
        diagnostics: set.diagnostics,
    })
}

fn materialize_existing_json(
    total: &mut u64,
    limits: Live2dPackageMaterializeLimits,
    file: Live2dPackageJsonFile,
) -> Result<Live2dPackageJsonFile> {
    let length = u64::try_from(file.bytes.len())
        .map_err(|_| Error::invalid_data("Live2D auxiliary JSON length does not fit u64"))?;
    if length > limits.maximum_file_bytes {
        return Err(Error::invalid_data(format!(
            "Live2D auxiliary JSON has {length} bytes, limit is {}",
            limits.maximum_file_bytes
        )));
    }
    *total = charge_u64(
        *total,
        length,
        limits.maximum_total_bytes,
        "materialized Live2D bytes",
    )?;
    Ok(file)
}

fn materialize_expressions(
    total: &mut u64,
    limits: Live2dPackageMaterializeLimits,
    source: Vec<Live2dPackageExpression>,
) -> Result<Vec<Live2dPackageExpressionBytes>> {
    let mut expressions = Vec::new();
    expressions.try_reserve(source.len()).map_err(|error| {
        Error::invalid_data(format!(
            "cannot allocate materialized Live2D expressions: {error}"
        ))
    })?;
    for expression in source {
        let json = materialize_package_file(total, limits, "Live2D expression JSON", |output| {
            expression
                .expression
                .write_exp3_json(output, limits.maximum_file_bytes)
        })?;
        expressions.push(Live2dPackageExpressionBytes {
            name: expression.name,
            file_name: expression.file_name,
            json,
        });
    }
    Ok(expressions)
}

fn materialize_motions(
    total: &mut u64,
    limits: Live2dPackageMaterializeLimits,
    source: Vec<Live2dPackageMotion>,
    targets: &CubismMotionTargetNames,
    force_bezier: bool,
) -> Result<Vec<Live2dPackageMotionBytes>> {
    let mut motions = Vec::new();
    motions.try_reserve(source.len()).map_err(|error| {
        Error::invalid_data(format!(
            "cannot allocate materialized Live2D motions: {error}"
        ))
    })?;
    for motion in source {
        let json = materialize_package_file(total, limits, "Live2D motion JSON", |output| {
            motion.motion.write_motion3_json(
                targets,
                force_bezier,
                output,
                limits.maximum_file_bytes,
            )
        })?;
        motions.push(Live2dPackageMotionBytes {
            name: motion.name,
            file_name: motion.file_name,
            json,
        });
    }
    Ok(motions)
}

fn materialize_package_file(
    total: &mut u64,
    limits: Live2dPackageMaterializeLimits,
    field: &'static str,
    write: impl FnOnce(&mut FallibleBuffer) -> Result<u64>,
) -> Result<Vec<u8>> {
    let remaining = limits
        .maximum_total_bytes
        .checked_sub(*total)
        .ok_or_else(|| Error::invalid_data("materialized Live2D output accounting underflowed"))?;
    let maximum = limits.maximum_file_bytes.min(remaining);
    let maximum = usize::try_from(maximum).map_err(|_| {
        Error::invalid_data(format!("{field} output limit does not fit this platform"))
    })?;
    let mut output = FallibleBuffer::new(maximum, field);
    let write_result = write(&mut output);
    if output.limit_exceeded {
        return Err(Error::invalid_data(format!(
            "{field} exceeds {maximum} bytes"
        )));
    }
    let written = write_result?;
    let actual = u64::try_from(output.bytes.len())
        .map_err(|_| Error::invalid_data(format!("{field} length does not fit u64")))?;
    if written != actual {
        return Err(Error::invalid_data(format!(
            "{field} writer reported {written} bytes but produced {actual}"
        )));
    }
    *total = total
        .checked_add(actual)
        .ok_or_else(|| Error::invalid_data("materialized Live2D byte count overflowed"))?;
    Ok(output.bytes)
}

struct FallibleBuffer {
    bytes: Vec<u8>,
    maximum: usize,
    field: &'static str,
    limit_exceeded: bool,
}

impl FallibleBuffer {
    const fn new(maximum: usize, field: &'static str) -> Self {
        Self {
            bytes: Vec::new(),
            maximum,
            field,
            limit_exceeded: false,
        }
    }
}

impl Write for FallibleBuffer {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        let next = self
            .bytes
            .len()
            .checked_add(input.len())
            .ok_or_else(|| io::Error::other(format!("{} length overflowed", self.field)))?;
        if next > self.maximum {
            self.limit_exceeded = true;
            return Err(io::Error::other(format!(
                "{} exceeds {} bytes",
                self.field, self.maximum
            )));
        }
        self.bytes.try_reserve(input.len()).map_err(|error| {
            io::Error::other(format!("cannot allocate {}: {error}", self.field))
        })?;
        self.bytes.extend_from_slice(input);
        Ok(input.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct ScriptedComponent {
    object: SceneObjectKey,
    object_index: usize,
    game_object: Option<SceneObjectKey>,
    behaviour: MonoBehaviour,
    role: CubismRole,
}

struct ComponentIndexes {
    roles: Vec<((usize, usize), CubismRole)>,
    models: Vec<ActiveModel>,
    expression_controllers: Vec<ScriptedComponent>,
    fade_controllers: Vec<ScriptedComponent>,
    renderers_by_model: Vec<(SceneObjectKey, Vec<ScriptedComponent>)>,
    pose_parts_by_model: Vec<(SceneObjectKey, Vec<ScriptedComponent>)>,
    display_parameters_by_model: Vec<(SceneObjectKey, Vec<ScriptedComponent>)>,
    display_parts_by_model: Vec<(SceneObjectKey, Vec<ScriptedComponent>)>,
    fade_motions_by_model: Vec<(SceneObjectKey, Vec<ScriptedComponent>)>,
    parameters_by_model: Vec<(SceneObjectKey, Vec<ScriptedComponent>)>,
    parts_by_model: Vec<(SceneObjectKey, Vec<ScriptedComponent>)>,
    eye_blink_parameters_by_model: Vec<(SceneObjectKey, Vec<ScriptedComponent>)>,
    mouth_parameters_by_model: Vec<(SceneObjectKey, Vec<ScriptedComponent>)>,
}

#[derive(Default)]
struct ComponentPartitions {
    roles: Vec<((usize, usize), CubismRole)>,
    models: Vec<ScriptedComponent>,
    renderers: Vec<ScriptedComponent>,
    expression_controllers: Vec<ScriptedComponent>,
    fade_controllers: Vec<ScriptedComponent>,
    pose_parts: Vec<ScriptedComponent>,
    display_parameters: Vec<ScriptedComponent>,
    display_parts: Vec<ScriptedComponent>,
    fade_motions: Vec<ScriptedComponent>,
    parameters: Vec<ScriptedComponent>,
    parts: Vec<ScriptedComponent>,
    eye_blink_parameters: Vec<ScriptedComponent>,
    mouth_parameters: Vec<ScriptedComponent>,
}

impl ComponentPartitions {
    fn new(component_count: usize, model_count: usize, renderer_count: usize) -> Result<Self> {
        let mut partitions = Self::default();
        partitions
            .roles
            .try_reserve(component_count)
            .map_err(|error| {
                Error::invalid_data(format!("cannot allocate Live2D role index: {error}"))
            })?;
        partitions
            .models
            .try_reserve(model_count)
            .map_err(|error| {
                Error::invalid_data(format!("cannot allocate Live2D model index: {error}"))
            })?;
        partitions
            .renderers
            .try_reserve(renderer_count)
            .map_err(|error| {
                Error::invalid_data(format!("cannot allocate Live2D renderer index: {error}"))
            })?;
        Ok(partitions)
    }

    fn push(&mut self, component: ScriptedComponent) -> Result<()> {
        self.roles.push((
            (component.object.file_index, component.object_index),
            component.role,
        ));
        match component.role {
            CubismRole::Model => self.models.push(component),
            CubismRole::Renderer => self.renderers.push(component),
            CubismRole::ExpressionController => push_component(
                &mut self.expression_controllers,
                component,
                "Live2D expression-controller index",
            )?,
            CubismRole::FadeController => push_component(
                &mut self.fade_controllers,
                component,
                "Live2D fade-controller index",
            )?,
            CubismRole::PosePart => {
                push_component(&mut self.pose_parts, component, "Live2D pose-part index")?;
            }
            CubismRole::DisplayInfoParameterName => push_component(
                &mut self.display_parameters,
                component,
                "Live2D parameter display-info index",
            )?,
            CubismRole::DisplayInfoPartName => push_component(
                &mut self.display_parts,
                component,
                "Live2D part display-info index",
            )?,
            CubismRole::FadeMotionData => push_component(
                &mut self.fade_motions,
                component,
                "Live2D fade-motion index",
            )?,
            CubismRole::Parameter => {
                push_component(&mut self.parameters, component, "Live2D parameter index")?;
            }
            CubismRole::Part => {
                push_component(&mut self.parts, component, "Live2D part index")?;
            }
            CubismRole::EyeBlinkParameter => push_component(
                &mut self.eye_blink_parameters,
                component,
                "Live2D eye-blink parameter index",
            )?,
            CubismRole::MouthParameter => push_component(
                &mut self.mouth_parameters,
                component,
                "Live2D mouth parameter index",
            )?,
            _ => {}
        }
        Ok(())
    }
}

struct ActiveModel {
    component: ScriptedComponent,
    expression_controller: Option<(usize, usize)>,
    physics_controller: Option<(usize, usize)>,
    fade_controller: Option<(usize, usize)>,
}

#[derive(Default)]
struct ActiveBindings {
    model: Option<(usize, usize)>,
    expression_controller: Option<(usize, usize)>,
    physics_controller: Option<(usize, usize)>,
    fade_controller: Option<(usize, usize)>,
}

impl ActiveBindings {
    fn observe(&mut self, identity: (usize, usize), role: CubismRole) {
        match role {
            CubismRole::Model => {
                self.model = Some(identity);
                self.expression_controller = None;
                self.physics_controller = None;
                self.fade_controller = None;
            }
            CubismRole::ExpressionController if self.model.is_some() => {
                self.expression_controller = Some(identity);
            }
            CubismRole::PhysicsController if self.model.is_some() => {
                self.physics_controller = Some(identity);
            }
            CubismRole::FadeController if self.model.is_some() => {
                self.fade_controller = Some(identity);
            }
            _ => {}
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct SelectedModel {
    model_index: usize,
    moc_identity: (usize, usize),
    moc_object: SceneObjectKey,
}

struct PackageState<'a> {
    collection: &'a AssetCollection,
    schema_provider: Option<&'a dyn MonoBehaviourSchemaProvider>,
    acl_decoder: Option<&'a dyn AclDecoder>,
    limits: Live2dPackageLimits,
    hierarchy: SceneHierarchy,
    animation_graph: Option<AnimationGraph>,
    scripts: HashMap<SceneObjectKey, Arc<str>>,
    components: Vec<ScriptedComponent>,
    diagnostics: Vec<Live2dPackageDiagnostic>,
    mono_behaviours: usize,
    models: usize,
    renderers: usize,
    textures: usize,
    expressions: usize,
    motions: usize,
    pose_parts: usize,
    display_entries: usize,
    type_tree_reads: usize,
    type_tree_object_bytes: u64,
    moc_bytes: u64,
    moc_metadata_bytes: u64,
    texture_object_bytes: u64,
    texture_payload_bytes: u64,
    expression_bytes: u64,
    motion_bytes: u64,
    physics_bytes: u64,
    auxiliary_bytes: u64,
    name_bytes: usize,
    diagnostic_bytes: usize,
    manifest_bytes: u64,
}

impl<'a> PackageState<'a> {
    fn new(
        collection: &'a AssetCollection,
        limits: &Live2dPackageLimits,
        hierarchy: SceneHierarchy,
        schema_provider: Option<&'a dyn MonoBehaviourSchemaProvider>,
        acl_decoder: Option<&'a dyn AclDecoder>,
    ) -> Self {
        Self {
            collection,
            schema_provider,
            acl_decoder,
            limits: *limits,
            hierarchy,
            animation_graph: None,
            scripts: HashMap::new(),
            components: Vec::new(),
            diagnostics: Vec::new(),
            mono_behaviours: 0,
            models: 0,
            renderers: 0,
            textures: 0,
            expressions: 0,
            motions: 0,
            pose_parts: 0,
            display_entries: 0,
            type_tree_reads: 0,
            type_tree_object_bytes: 0,
            moc_bytes: 0,
            moc_metadata_bytes: 0,
            texture_object_bytes: 0,
            texture_payload_bytes: 0,
            expression_bytes: 0,
            motion_bytes: 0,
            physics_bytes: 0,
            auxiliary_bytes: 0,
            name_bytes: 0,
            diagnostic_bytes: 0,
            manifest_bytes: 0,
        }
    }

    fn discover_components(&mut self) -> Result<()> {
        for (file_index, loaded) in self.collection.serialized_files.iter().enumerate() {
            for (object_index, object) in loaded.file.objects.iter().enumerate() {
                if object.class_id != MONO_BEHAVIOUR_CLASS_ID {
                    continue;
                }
                self.mono_behaviours = charge_usize(
                    self.mono_behaviours,
                    1,
                    self.limits.maximum_mono_behaviours,
                    "Live2D MonoBehaviours",
                )?;
                let behaviour =
                    read_mono_behaviour(&loaded.file, object_index, self.limits.mono_behaviour)?;
                let Some(script_class) = self.script_class(file_index, behaviour.script)? else {
                    continue;
                };
                let role = CubismRole::from_script_class(&script_class);
                if !role.is_live2d() {
                    continue;
                }
                match role {
                    CubismRole::Model => {
                        self.models = charge_usize(
                            self.models,
                            1,
                            self.limits.maximum_models,
                            "Live2D models",
                        )?;
                    }
                    CubismRole::Renderer => {
                        self.renderers = charge_usize(
                            self.renderers,
                            1,
                            self.limits.maximum_renderers,
                            "Live2D renderers",
                        )?;
                    }
                    _ => {}
                }
                self.name_bytes = charge_usize(
                    self.name_bytes,
                    behaviour.name.len(),
                    self.limits.maximum_total_name_bytes,
                    "Live2D names",
                )?;
                let game_object = self.resolve_game_object(file_index, behaviour.game_object);
                self.components.try_reserve(1).map_err(|error| {
                    Error::invalid_data(format!("cannot grow Live2D package components: {error}"))
                })?;
                self.components.push(ScriptedComponent {
                    object: SceneObjectKey {
                        file_index,
                        path_id: object.path_id,
                    },
                    object_index,
                    game_object,
                    behaviour,
                    role,
                });
            }
        }
        Ok(())
    }

    fn script_class(
        &mut self,
        source_file_index: usize,
        reference: crate::monobehaviour::ObjectReference,
    ) -> Result<Option<Arc<str>>> {
        let reference = ObjectReference {
            file_id: reference.file_id,
            path_id: reference.path_id,
        };
        let Some(target) = resolve_object_reference(self.collection, source_file_index, reference)
            .ok()
            .flatten()
        else {
            return Ok(None);
        };
        if target.object.class_id != MONO_SCRIPT_CLASS_ID {
            return Ok(None);
        }
        let key = SceneObjectKey {
            file_index: target.file_index,
            path_id: target.object.path_id,
        };
        if let Some(class_name) = self.scripts.get(&key) {
            return Ok(Some(Arc::clone(class_name)));
        }
        if self.scripts.len() >= self.limits.maximum_scripts {
            return Err(Error::invalid_data(format!(
                "Live2D package planning exceeds {} scripts",
                self.limits.maximum_scripts
            )));
        }
        let script =
            read_mono_script(target.file, target.object_index, self.limits.mono_behaviour)?;
        self.name_bytes = charge_usize(
            self.name_bytes,
            script.class_name.len(),
            self.limits.maximum_total_name_bytes,
            "Live2D names",
        )?;
        let class_name: Arc<str> = script.class_name.into();
        self.scripts.try_reserve(1).map_err(|error| {
            Error::invalid_data(format!("cannot grow Live2D script cache: {error}"))
        })?;
        self.scripts.insert(key, Arc::clone(&class_name));
        Ok(Some(class_name))
    }

    fn resolve_game_object(
        &self,
        source_file_index: usize,
        reference: crate::monobehaviour::ObjectReference,
    ) -> Option<SceneObjectKey> {
        let target = resolve_object_reference(
            self.collection,
            source_file_index,
            ObjectReference {
                file_id: reference.file_id,
                path_id: reference.path_id,
            },
        )
        .ok()
        .flatten()?;
        (target.object.class_id == GAME_OBJECT_CLASS_ID).then_some(SceneObjectKey {
            file_index: target.file_index,
            path_id: target.object.path_id,
        })
    }

    fn build_packages(mut self) -> Result<Live2dPackageSet> {
        let indexes = self.index_components()?;

        // AssetStudio seeds packages by CubismMoc identity and then lets the
        // active GameObject component binding replace the associated model.
        // Keep first-seen package order, but make the last active model which
        // refers to the same MOC win.
        let mut selected = Vec::new();
        let mut selected_by_moc = HashMap::new();
        selected
            .try_reserve(indexes.models.len())
            .map_err(|error| {
                Error::invalid_data(format!("cannot allocate Live2D selected models: {error}"))
            })?;
        selected_by_moc
            .try_reserve(indexes.models.len())
            .map_err(|error| {
                Error::invalid_data(format!("cannot allocate Live2D MOC index: {error}"))
            })?;
        for (model_index, model) in indexes.models.iter().enumerate() {
            let Some((moc_identity, moc_object)) =
                self.resolve_model_moc(&model.component, &indexes.roles)?
            else {
                continue;
            };
            let candidate = SelectedModel {
                model_index,
                moc_identity,
                moc_object,
            };
            if let Some(selected_index) = selected_by_moc.get(&moc_identity).copied() {
                selected[selected_index] = candidate;
            } else {
                let selected_index = selected.len();
                selected.push(candidate);
                selected_by_moc.insert(moc_identity, selected_index);
            }
        }

        let mut packages = Vec::new();
        packages.try_reserve(selected.len()).map_err(|error| {
            Error::invalid_data(format!("cannot allocate Live2D package list: {error}"))
        })?;
        let mut claimed_directories = HashSet::new();
        for selected_model in selected {
            let model = &indexes.models[selected_model.model_index];
            let moc = self.read_moc(selected_model.moc_identity)?;
            let package = self.build_package(
                model,
                selected_model.moc_object,
                moc,
                &indexes,
                &mut claimed_directories,
            )?;
            packages.push(package);
        }
        Ok(Live2dPackageSet {
            packages,
            diagnostics: self.diagnostics,
        })
    }

    fn index_components(&mut self) -> Result<ComponentIndexes> {
        let mut partitions =
            ComponentPartitions::new(self.components.len(), self.models, self.renderers)?;
        for component in std::mem::take(&mut self.components) {
            partitions.push(component)?;
        }
        let ComponentPartitions {
            mut roles,
            models,
            renderers,
            mut expression_controllers,
            mut fade_controllers,
            pose_parts,
            display_parameters,
            display_parts,
            fade_motions,
            parameters,
            parts,
            eye_blink_parameters,
            mouth_parameters,
        } = partitions;
        roles.sort_unstable_by_key(|entry| entry.0);
        expression_controllers.sort_unstable_by_key(component_identity);
        fade_controllers.sort_unstable_by_key(component_identity);

        let models = self.select_active_models(&roles, models)?;

        let renderers_by_model = self.assign_renderers(&models, renderers)?;
        let pose_parts_by_model = self.assign_auxiliary_components(&models, pose_parts)?;
        let display_parameters_by_model =
            self.assign_auxiliary_components(&models, display_parameters)?;
        let display_parts_by_model = self.assign_auxiliary_components(&models, display_parts)?;
        let fade_motions_by_model = self.assign_auxiliary_components(&models, fade_motions)?;
        let parameters_by_model = self.assign_auxiliary_components(&models, parameters)?;
        let parts_by_model = self.assign_auxiliary_components(&models, parts)?;
        let eye_blink_parameters_by_model =
            self.assign_auxiliary_components(&models, eye_blink_parameters)?;
        let mouth_parameters_by_model =
            self.assign_auxiliary_components(&models, mouth_parameters)?;
        Ok(ComponentIndexes {
            roles,
            models,
            expression_controllers,
            fade_controllers,
            renderers_by_model,
            pose_parts_by_model,
            display_parameters_by_model,
            display_parts_by_model,
            fade_motions_by_model,
            parameters_by_model,
            parts_by_model,
            eye_blink_parameters_by_model,
            mouth_parameters_by_model,
        })
    }

    fn assign_renderers(
        &mut self,
        models: &[ActiveModel],
        renderers: Vec<ScriptedComponent>,
    ) -> Result<Vec<(SceneObjectKey, Vec<ScriptedComponent>)>> {
        let mut model_objects = Vec::new();
        model_objects.try_reserve(models.len()).map_err(|error| {
            Error::invalid_data(format!(
                "cannot allocate Live2D model GameObject index: {error}"
            ))
        })?;
        for model in models {
            if let Some(game_object) = model.component.game_object {
                model_objects.push((game_object, model.component.object));
            }
        }
        model_objects.sort_unstable();

        let mut assignments = Vec::new();
        assignments.try_reserve(renderers.len()).map_err(|error| {
            Error::invalid_data(format!(
                "cannot allocate Live2D renderer assignments: {error}"
            ))
        })?;
        for renderer in renderers {
            let Some(game_object) = renderer.game_object else {
                self.push_diagnostic(
                    renderer.object,
                    Live2dPackageDiagnosticKind::DetachedRenderer,
                    "CubismRenderer has no resolved GameObject",
                )?;
                continue;
            };
            // AssetStudio's `TryGetModelGameObject` advances to `m_Father`
            // before checking for a model, so a renderer on the model's own
            // GameObject is intentionally not attached here.
            let Some(model) = nearest_parent_model(&self.hierarchy, game_object, &model_objects)
            else {
                self.push_diagnostic(
                    renderer.object,
                    Live2dPackageDiagnosticKind::DetachedRenderer,
                    "CubismRenderer is not below a resolved CubismModel GameObject",
                )?;
                continue;
            };
            assignments.push((model, renderer));
        }
        assignments.sort_unstable_by_key(|(model, renderer)| (*model, renderer.object));

        let mut grouped = Vec::new();
        grouped
            .try_reserve(models.len().min(assignments.len()))
            .map_err(|error| {
                Error::invalid_data(format!("cannot allocate Live2D renderer groups: {error}"))
            })?;
        for (model, renderer) in assignments {
            if grouped.last().is_none_or(
                |(last_model, _): &(SceneObjectKey, Vec<ScriptedComponent>)| *last_model != model,
            ) {
                let mut values = Vec::new();
                values.try_reserve(1).map_err(|error| {
                    Error::invalid_data(format!("cannot allocate Live2D renderer group: {error}"))
                })?;
                grouped.push((model, values));
            }
            let values = &mut grouped.last_mut().expect("renderer group was just added").1;
            values.try_reserve(1).map_err(|error| {
                Error::invalid_data(format!("cannot grow Live2D renderer group: {error}"))
            })?;
            values.push(renderer);
        }
        Ok(grouped)
    }

    fn assign_auxiliary_components(
        &self,
        models: &[ActiveModel],
        components: Vec<ScriptedComponent>,
    ) -> Result<Vec<(SceneObjectKey, Vec<ScriptedComponent>)>> {
        let mut model_objects = Vec::new();
        model_objects.try_reserve(models.len()).map_err(|error| {
            Error::invalid_data(format!(
                "cannot allocate Live2D auxiliary model index: {error}"
            ))
        })?;
        for model in models {
            if let Some(game_object) = model.component.game_object {
                model_objects.push((game_object, model.component.object));
            }
        }
        model_objects.sort_unstable();
        let mut assignments = Vec::new();
        assignments.try_reserve(components.len()).map_err(|error| {
            Error::invalid_data(format!(
                "cannot allocate Live2D auxiliary assignments: {error}"
            ))
        })?;
        for component in components {
            let Some(game_object) = component.game_object else {
                continue;
            };
            let Some(model) = nearest_parent_model(&self.hierarchy, game_object, &model_objects)
            else {
                continue;
            };
            assignments.push((model, component));
        }
        assignments.sort_unstable_by_key(|(model, component)| (*model, component.object));
        group_component_assignments(models.len(), assignments)
    }

    fn select_active_models(
        &self,
        roles: &[((usize, usize), CubismRole)],
        mut models: Vec<ScriptedComponent>,
    ) -> Result<Vec<ActiveModel>> {
        models.sort_unstable_by_key(component_identity);
        let mut model_identities = Vec::new();
        model_identities
            .try_reserve(models.len())
            .map_err(|error| {
                Error::invalid_data(format!("cannot allocate Live2D model identities: {error}"))
            })?;
        let mut model_slots = Vec::new();
        model_slots.try_reserve(models.len()).map_err(|error| {
            Error::invalid_data(format!("cannot allocate Live2D model slots: {error}"))
        })?;
        for model in models {
            model_identities.push(component_identity(&model));
            model_slots.push(Some(model));
        }

        let mut active = Vec::new();
        active.try_reserve(model_slots.len()).map_err(|error| {
            Error::invalid_data(format!("cannot allocate active Live2D models: {error}"))
        })?;
        for (file_index, loaded) in self.collection.serialized_files.iter().enumerate() {
            for (object_index, object) in loaded.file.objects.iter().enumerate() {
                if object.class_id != GAME_OBJECT_CLASS_ID {
                    continue;
                }
                let game_object =
                    read_game_object(&loaded.file, object_index, self.limits.hierarchy.scene)?;
                let game_object_key = SceneObjectKey {
                    file_index,
                    path_id: object.path_id,
                };
                let mut transform_seen = false;
                let mut bindings = ActiveBindings::default();
                for component in game_object.components {
                    let Some(target) =
                        resolve_object_reference(self.collection, file_index, component.component)
                            .ok()
                            .flatten()
                    else {
                        continue;
                    };
                    if matches!(
                        target.object.class_id,
                        TRANSFORM_CLASS_ID | RECT_TRANSFORM_CLASS_ID
                    ) {
                        transform_seen = true;
                        continue;
                    }
                    if !transform_seen || target.object.class_id != MONO_BEHAVIOUR_CLASS_ID {
                        continue;
                    }
                    let identity = (target.file_index, target.object_index);
                    let role_position =
                        roles.partition_point(|(candidate, _)| *candidate < identity);
                    if let Some(&(candidate, role)) = roles.get(role_position)
                        && candidate == identity
                    {
                        bindings.observe(identity, role);
                    }
                }
                let Some(identity) = bindings.model else {
                    continue;
                };
                let position = model_identities.partition_point(|candidate| *candidate < identity);
                if model_identities.get(position) != Some(&identity) {
                    continue;
                }
                let Some(slot) = model_slots.get_mut(position) else {
                    continue;
                };
                let Some(mut model) = slot.take() else {
                    continue;
                };
                model.game_object = Some(game_object_key);
                active.push(ActiveModel {
                    component: model,
                    expression_controller: bindings.expression_controller,
                    physics_controller: bindings.physics_controller,
                    fade_controller: bindings.fade_controller,
                });
            }
        }
        Ok(active)
    }

    fn build_package(
        &mut self,
        active_model: &ActiveModel,
        moc_key: SceneObjectKey,
        moc: CubismMoc,
        indexes: &ComponentIndexes,
        claimed_directories: &mut HashSet<String>,
    ) -> Result<Live2dPackage> {
        let model = &active_model.component;
        let source_name = model
            .game_object
            .and_then(|key| self.hierarchy.node(key))
            .map(|node| node.name.as_str())
            .filter(|name| !name.is_empty())
            .or_else(|| (!model.behaviour.name.is_empty()).then_some(model.behaviour.name.as_str()))
            .unwrap_or_else(|| {
                portable_file_stem(&self.collection.serialized_files[model.object.file_index].path)
            });
        let name = safe_file_stem(source_name)?;
        self.charge_name(&name)?;
        let directory_name = claim_name(&name, model.object, claimed_directories)?;
        self.charge_name(&directory_name)?;
        let moc_file_name = concatenate(&name, ".moc3", "Live2D MOC file name")?;
        let manifest_file_name = concatenate(&name, ".model3.json", "Live2D manifest file name")?;
        self.charge_name(&moc_file_name)?;
        self.charge_name(&manifest_file_name)?;
        let renderer_position = indexes
            .renderers_by_model
            .partition_point(|(model_key, _)| *model_key < model.object);
        let model_renderers = indexes
            .renderers_by_model
            .get(renderer_position)
            .filter(|(model_key, _)| *model_key == model.object)
            .map(|(_, renderers)| renderers.as_slice())
            .unwrap_or_default();
        let textures = self.read_model_textures(model_renderers)?;
        let expressions = self.read_model_expressions(
            active_model.expression_controller,
            &indexes.expression_controllers,
            &indexes.roles,
        )?;
        let (motion_targets, eye_blink_parameters, lip_sync_parameters) =
            self.parameter_metadata_for_model(model.object, indexes)?;
        let direct_motions = components_for_model(&indexes.fade_motions_by_model, model.object);
        let mut motions = self.read_model_motions(
            active_model.fade_controller,
            &indexes.fade_controllers,
            direct_motions,
            &indexes.roles,
            &motion_targets,
        )?;
        if motions.is_empty()
            && let Some(game_object) = model.game_object
        {
            motions = self.read_model_clip_motions(game_object, &motion_targets)?;
        }
        let motion_fps = motions.first().map_or(0.0, |motion| motion.motion.fps());
        let physics =
            self.build_physics_file(&name, active_model.physics_controller, motion_fps)?;
        let pose_components = components_for_model(&indexes.pose_parts_by_model, model.object);
        let pose = self.build_pose_file(&name, pose_components)?;
        let parameter_components =
            components_for_model(&indexes.display_parameters_by_model, model.object);
        let part_components = components_for_model(&indexes.display_parts_by_model, model.object);
        let display_info =
            self.build_display_info_file(&name, parameter_components, part_components)?;
        let package = Live2dPackage {
            model: model.object,
            model_game_object: model.game_object,
            moc_object: moc_key,
            name,
            directory_name,
            moc_file_name,
            manifest_file_name,
            moc,
            textures,
            expressions,
            motions,
            motion_targets,
            eye_blink_parameters,
            lip_sync_parameters,
            force_bezier_motions: self.limits.force_bezier_motions,
            physics,
            pose,
            display_info,
        };
        let remaining = self
            .limits
            .maximum_total_manifest_bytes
            .checked_sub(self.manifest_bytes)
            .ok_or_else(|| Error::invalid_data("Live2D manifest byte budget was exceeded"))?;
        let written = package.write_model3_json(&mut std::io::sink(), remaining)?;
        self.manifest_bytes = charge_u64(
            self.manifest_bytes,
            written,
            self.limits.maximum_total_manifest_bytes,
            "Live2D manifest bytes",
        )?;
        Ok(package)
    }

    fn resolve_model_moc(
        &mut self,
        model: &ScriptedComponent,
        roles: &[((usize, usize), CubismRole)],
    ) -> Result<Option<((usize, usize), SceneObjectKey)>> {
        let Some(reference) = self.required_reference(model, "_moc")? else {
            return Ok(None);
        };
        let Some(target) = self.resolve_required_target(
            model.object,
            reference,
            MONO_BEHAVIOUR_CLASS_ID,
            "CubismModel._moc",
        )?
        else {
            return Ok(None);
        };
        let key = SceneObjectKey {
            file_index: target.file_index,
            path_id: target.object.path_id,
        };
        let target_identity = (target.file_index, target.object_index);
        let position = roles.partition_point(|(candidate, _)| *candidate < target_identity);
        if roles.get(position) != Some(&(target_identity, CubismRole::Moc)) {
            self.push_diagnostic(
                model.object,
                Live2dPackageDiagnosticKind::WrongCubismRole,
                "CubismModel._moc does not resolve to a MonoBehaviour whose script is CubismMoc",
            )?;
            return Ok(None);
        }
        Ok(Some((target_identity, key)))
    }

    fn read_moc(&mut self, identity: (usize, usize)) -> Result<CubismMoc> {
        let loaded = self
            .collection
            .serialized_files
            .get(identity.0)
            .ok_or_else(|| Error::invalid_data("resolved CubismMoc file index is out of range"))?;
        let object = loaded.file.objects.get(identity.1).ok_or_else(|| {
            Error::invalid_data("resolved CubismMoc object index is out of range")
        })?;
        if object.class_id != MONO_BEHAVIOUR_CLASS_ID {
            return Err(Error::invalid_data(
                "resolved CubismMoc object no longer has MonoBehaviour class ID",
            ));
        }
        let moc = read_cubism_moc(&loaded.file, identity.1, self.limits.moc)?;
        self.moc_bytes = charge_u64(
            self.moc_bytes,
            moc.model_data.len(),
            self.limits.maximum_total_moc_bytes,
            "Live2D MOC bytes",
        )?;
        self.moc_metadata_bytes = charge_u64(
            self.moc_metadata_bytes,
            cubism_moc_retained_bytes(&moc)?,
            self.limits.maximum_total_moc_metadata_bytes,
            "Live2D retained MOC metadata bytes",
        )?;
        Ok(moc)
    }

    fn read_model_textures(
        &mut self,
        renderers: &[ScriptedComponent],
    ) -> Result<Vec<Live2dPackageTexture>> {
        let mut candidates = Vec::new();
        let mut seen = HashSet::new();
        candidates.try_reserve(renderers.len()).map_err(|error| {
            Error::invalid_data(format!(
                "cannot allocate Live2D texture candidates: {error}"
            ))
        })?;
        seen.try_reserve(renderers.len()).map_err(|error| {
            Error::invalid_data(format!("cannot allocate Live2D texture index: {error}"))
        })?;
        for renderer in renderers {
            let Some(reference) = self.required_reference(renderer, "_mainTexture")? else {
                continue;
            };
            let Some(target) = self.resolve_required_target(
                renderer.object,
                reference,
                TEXTURE_2D_CLASS_ID,
                "CubismRenderer._mainTexture",
            )?
            else {
                continue;
            };
            let key = SceneObjectKey {
                file_index: target.file_index,
                path_id: target.object.path_id,
            };
            if !seen.insert(key) {
                continue;
            }
            self.textures = charge_usize(
                self.textures,
                1,
                self.limits.maximum_textures,
                "Live2D textures",
            )?;
            self.texture_object_bytes = charge_u64(
                self.texture_object_bytes,
                target.object.byte_size,
                self.limits.maximum_total_texture_object_bytes,
                "Live2D Texture2D object bytes",
            )?;
            let texture = match read_texture2d(
                self.collection,
                target.file,
                target.object_index,
                self.limits.texture,
            ) {
                Ok(texture) => texture,
                Err(error) => {
                    self.push_diagnostic(
                        renderer.object,
                        Live2dPackageDiagnosticKind::TextureReadFailed,
                        format!("CubismRenderer._mainTexture cannot be parsed: {error}"),
                    )?;
                    continue;
                }
            };
            self.texture_payload_bytes = charge_u64(
                self.texture_payload_bytes,
                texture.data.len(),
                self.limits.maximum_total_texture_payload_bytes,
                "Live2D texture payload bytes",
            )?;
            candidates.push((key, texture));
        }
        candidates.sort_by(|left, right| {
            left.1
                .name
                .cmp(&right.1.name)
                .then_with(|| left.0.cmp(&right.0))
        });
        self.plan_texture_names(candidates)
    }

    fn plan_texture_names(
        &mut self,
        candidates: Vec<(SceneObjectKey, Texture2D)>,
    ) -> Result<Vec<Live2dPackageTexture>> {
        let mut claimed = HashSet::new();
        let mut textures = Vec::new();
        claimed.try_reserve(candidates.len()).map_err(|error| {
            Error::invalid_data(format!("cannot allocate Live2D texture names: {error}"))
        })?;
        textures.try_reserve(candidates.len()).map_err(|error| {
            Error::invalid_data(format!("cannot allocate Live2D texture plan: {error}"))
        })?;
        for (object, texture) in candidates {
            let stem = safe_file_stem(&texture.name)?;
            let claimed_name = claim_name(&stem, object, &mut claimed)?;
            let texture_path =
                concatenate("textures/", &claimed_name, "Live2D texture relative path")?;
            let file_name = concatenate(&texture_path, ".png", "Live2D texture file name")?;
            self.charge_name(&file_name)?;
            self.charge_name(&texture.name)?;
            textures.push(Live2dPackageTexture {
                object,
                source_name: texture.name.clone(),
                file_name,
                texture,
            });
        }
        Ok(textures)
    }

    fn read_model_expressions(
        &mut self,
        controller_identity: Option<(usize, usize)>,
        controllers: &[ScriptedComponent],
        roles: &[((usize, usize), CubismRole)],
    ) -> Result<Vec<Live2dPackageExpression>> {
        let Some(controller_identity) = controller_identity else {
            return Ok(Vec::new());
        };
        let position = controllers
            .partition_point(|component| component_identity(component) < controller_identity);
        let Some(controller) = controllers
            .get(position)
            .filter(|component| component_identity(component) == controller_identity)
        else {
            return Ok(Vec::new());
        };
        let Some(list_reference) = self.required_reference(controller, "ExpressionsList")? else {
            return Ok(Vec::new());
        };
        let Some(list_target) = self.resolve_required_target(
            controller.object,
            list_reference,
            MONO_BEHAVIOUR_CLASS_ID,
            "CubismExpressionController.ExpressionsList",
        )?
        else {
            return Ok(Vec::new());
        };
        let list_identity = (list_target.file_index, list_target.object_index);
        if role_for_identity(roles, list_identity) != Some(CubismRole::ExpressionList) {
            self.push_diagnostic(
                controller.object,
                Live2dPackageDiagnosticKind::WrongCubismRole,
                "CubismExpressionController.ExpressionsList does not resolve to CubismExpressionList",
            )?;
            return Ok(Vec::new());
        }
        let list = self.scripted_component(list_identity, CubismRole::ExpressionList)?;
        let Some(references) = self.required_references(&list, "CubismExpressionObjects")? else {
            return Ok(Vec::new());
        };
        self.read_expression_references(&list, &references, roles)
    }

    fn read_expression_references(
        &mut self,
        owner: &ScriptedComponent,
        references: &[ObjectReference],
        roles: &[((usize, usize), CubismRole)],
    ) -> Result<Vec<Live2dPackageExpression>> {
        let mut expressions = Vec::new();
        let mut seen = HashSet::new();
        let mut claimed_names = HashSet::new();
        expressions.try_reserve(references.len()).map_err(|error| {
            Error::invalid_data(format!("cannot allocate Live2D expressions: {error}"))
        })?;
        seen.try_reserve(references.len()).map_err(|error| {
            Error::invalid_data(format!("cannot allocate Live2D expression index: {error}"))
        })?;
        claimed_names
            .try_reserve(references.len())
            .map_err(|error| {
                Error::invalid_data(format!("cannot allocate Live2D expression names: {error}"))
            })?;
        for reference in references {
            let Some(target) = self.resolve_required_target(
                owner.object,
                *reference,
                MONO_BEHAVIOUR_CLASS_ID,
                "CubismExpressionList.CubismExpressionObjects",
            )?
            else {
                continue;
            };
            let identity = (target.file_index, target.object_index);
            if role_for_identity(roles, identity) != Some(CubismRole::ExpressionData) {
                self.push_diagnostic(
                    owner.object,
                    Live2dPackageDiagnosticKind::WrongCubismRole,
                    "CubismExpressionObjects entry does not resolve to CubismExpressionData",
                )?;
                continue;
            }
            if !seen.insert(identity) {
                continue;
            }
            if let Some(expression) = self.project_expression_target(&target, &mut claimed_names)? {
                expressions.push(expression);
            }
        }
        Ok(expressions)
    }

    fn project_expression_target(
        &mut self,
        target: &crate::scene::ResolvedObject<'a>,
        claimed_names: &mut HashSet<String>,
    ) -> Result<Option<Live2dPackageExpression>> {
        self.expressions = charge_usize(
            self.expressions,
            1,
            self.limits.maximum_expressions,
            "Live2D expressions",
        )?;
        self.type_tree_reads = charge_usize(
            self.type_tree_reads,
            1,
            self.limits.maximum_type_tree_reads,
            "Live2D TypeTree reads",
        )?;
        self.type_tree_object_bytes = charge_u64(
            self.type_tree_object_bytes,
            target.object.byte_size,
            self.limits.maximum_total_type_tree_object_bytes,
            "Live2D TypeTree object bytes",
        )?;
        let object = SceneObjectKey {
            file_index: target.file_index,
            path_id: target.object.path_id,
        };
        let expression = match self
            .read_schema_value(
                target.file_index,
                target.object_index,
                self.limits.expression.maximum_object_bytes,
                self.limits.expression.type_tree,
            )
            .and_then(|value| {
                project_cubism_expression(target.object.path_id, &value, self.limits.expression)
            }) {
            Ok(expression) => expression,
            Err(error) => {
                self.push_diagnostic(
                    object,
                    Live2dPackageDiagnosticKind::ExpressionReadFailed,
                    format!("CubismExpressionData cannot be projected: {error}"),
                )?;
                return Ok(None);
            }
        };
        let written = expression
            .write_exp3_json(&mut io::sink(), self.limits.expression.maximum_output_bytes)?;
        self.expression_bytes = charge_u64(
            self.expression_bytes,
            written,
            self.limits.maximum_total_expression_bytes,
            "Live2D expression JSON bytes",
        )?;
        let stem = remove_exp3_suffixes(&expression.source_name)?;
        let stem = safe_file_stem(&stem)?;
        let name = claim_name(&stem, object, claimed_names)?;
        let prefix = concatenate("expressions/", &name, "Live2D expression path")?;
        let file_name = concatenate(&prefix, ".exp3.json", "Live2D expression file name")?;
        self.charge_name(&name)?;
        self.charge_name(&file_name)?;
        Ok(Some(Live2dPackageExpression {
            object,
            name,
            file_name,
            expression,
        }))
    }

    fn parameter_metadata_for_model(
        &mut self,
        model: SceneObjectKey,
        indexes: &ComponentIndexes,
    ) -> Result<(CubismMotionTargetNames, Vec<String>, Vec<String>)> {
        let parameters = components_for_model(&indexes.parameters_by_model, model);
        let parts = components_for_model(&indexes.parts_by_model, model);
        let targets = CubismMotionTargetNames {
            parameters: self.component_names(parameters)?,
            parts: self.component_names(parts)?,
        };
        let mut eye_blink = self.component_names(components_for_model(
            &indexes.eye_blink_parameters_by_model,
            model,
        ))?;
        if eye_blink.is_empty() {
            eye_blink = self.fallback_parameter_names(&targets.parameters, is_eye_blink_name)?;
        }
        let mut lip_sync = self.component_names(components_for_model(
            &indexes.mouth_parameters_by_model,
            model,
        ))?;
        if lip_sync.is_empty() {
            lip_sync = self.fallback_parameter_names(&targets.parameters, is_lip_sync_name)?;
        }
        Ok((targets, eye_blink, lip_sync))
    }

    fn component_names(&mut self, components: &[ScriptedComponent]) -> Result<Vec<String>> {
        let mut names = Vec::new();
        names.try_reserve(components.len()).map_err(|error| {
            Error::invalid_data(format!("cannot allocate Live2D component names: {error}"))
        })?;
        for component in components {
            if let Some(name) = self.component_game_object_name(component)? {
                names.push(name);
            }
        }
        names.sort();
        names.dedup();
        Ok(names)
    }

    fn fallback_parameter_names(
        &mut self,
        parameters: &[String],
        matches: fn(&str) -> bool,
    ) -> Result<Vec<String>> {
        let count = parameters.iter().filter(|name| matches(name)).count();
        let mut names = Vec::new();
        names.try_reserve(count).map_err(|error| {
            Error::invalid_data(format!(
                "cannot allocate Live2D inferred parameter group: {error}"
            ))
        })?;
        for parameter in parameters.iter().filter(|name| matches(name)) {
            let name = copy_string(parameter, "Live2D inferred parameter name")?;
            self.charge_name(&name)?;
            names.push(name);
        }
        Ok(names)
    }

    fn read_model_motions(
        &mut self,
        controller_identity: Option<(usize, usize)>,
        controllers: &[ScriptedComponent],
        direct: &[ScriptedComponent],
        roles: &[((usize, usize), CubismRole)],
        targets: &CubismMotionTargetNames,
    ) -> Result<Vec<Live2dPackageMotion>> {
        let identities = if let Some(identity) = controller_identity {
            self.motion_identities_from_controller(identity, controllers, roles)?
        } else {
            let mut identities = Vec::new();
            identities.try_reserve(direct.len()).map_err(|error| {
                Error::invalid_data(format!("cannot allocate direct Live2D motions: {error}"))
            })?;
            identities.extend(direct.iter().map(component_identity));
            identities
        };
        let mut motions = Vec::new();
        let mut claimed = HashSet::new();
        motions.try_reserve(identities.len()).map_err(|error| {
            Error::invalid_data(format!("cannot allocate Live2D motion plan: {error}"))
        })?;
        claimed.try_reserve(identities.len()).map_err(|error| {
            Error::invalid_data(format!("cannot allocate Live2D motion names: {error}"))
        })?;
        for identity in identities {
            if let Some(motion) = self.project_motion(identity, targets, &mut claimed)? {
                motions.push(motion);
            }
        }
        Ok(motions)
    }

    fn motion_identities_from_controller(
        &mut self,
        identity: (usize, usize),
        controllers: &[ScriptedComponent],
        roles: &[((usize, usize), CubismRole)],
    ) -> Result<Vec<(usize, usize)>> {
        let position = controllers.partition_point(|value| component_identity(value) < identity);
        let Some(controller) = controllers
            .get(position)
            .filter(|value| component_identity(value) == identity)
        else {
            return Ok(Vec::new());
        };
        let Some(reference) = self.required_reference(controller, "CubismFadeMotionList")? else {
            return Ok(Vec::new());
        };
        let Some(target) = self.resolve_required_target(
            controller.object,
            reference,
            MONO_BEHAVIOUR_CLASS_ID,
            "CubismFadeController.CubismFadeMotionList",
        )?
        else {
            return Ok(Vec::new());
        };
        let list_identity = (target.file_index, target.object_index);
        if role_for_identity(roles, list_identity) != Some(CubismRole::FadeMotionList) {
            self.push_diagnostic(
                controller.object,
                Live2dPackageDiagnosticKind::WrongCubismRole,
                "CubismFadeMotionList does not resolve to CubismFadeMotionList",
            )?;
            return Ok(Vec::new());
        }
        let list = self.scripted_component(list_identity, CubismRole::FadeMotionList)?;
        let Some(references) = self.required_references(&list, "CubismFadeMotionObjects")? else {
            return Ok(Vec::new());
        };
        let mut identities = Vec::new();
        let mut seen = HashSet::new();
        identities.try_reserve(references.len()).map_err(|error| {
            Error::invalid_data(format!("cannot allocate Live2D motion references: {error}"))
        })?;
        seen.try_reserve(references.len()).map_err(|error| {
            Error::invalid_data(format!("cannot allocate Live2D motion index: {error}"))
        })?;
        for reference in references {
            let Some(target) = self.resolve_required_target(
                list.object,
                reference,
                MONO_BEHAVIOUR_CLASS_ID,
                "CubismFadeMotionList.CubismFadeMotionObjects",
            )?
            else {
                continue;
            };
            let identity = (target.file_index, target.object_index);
            if role_for_identity(roles, identity) != Some(CubismRole::FadeMotionData) {
                self.push_diagnostic(
                    list.object,
                    Live2dPackageDiagnosticKind::WrongCubismRole,
                    "CubismFadeMotionObjects entry is not CubismFadeMotionData",
                )?;
                continue;
            }
            if !seen.insert(identity) {
                continue;
            }
            identities.push(identity);
        }
        Ok(identities)
    }

    fn project_motion(
        &mut self,
        identity: (usize, usize),
        targets: &CubismMotionTargetNames,
        claimed: &mut HashSet<String>,
    ) -> Result<Option<Live2dPackageMotion>> {
        self.motions = charge_usize(
            self.motions,
            1,
            self.limits.maximum_motions,
            "Live2D motions",
        )?;
        let loaded = &self.collection.serialized_files[identity.0];
        let object = &loaded.file.objects[identity.1];
        self.type_tree_reads = charge_usize(
            self.type_tree_reads,
            1,
            self.limits.maximum_type_tree_reads,
            "Live2D TypeTree reads",
        )?;
        self.type_tree_object_bytes = charge_u64(
            self.type_tree_object_bytes,
            object.byte_size,
            self.limits.maximum_total_type_tree_object_bytes,
            "Live2D TypeTree object bytes",
        )?;
        let key = SceneObjectKey {
            file_index: identity.0,
            path_id: object.path_id,
        };
        let motion = match self
            .read_schema_value(
                identity.0,
                identity.1,
                self.limits.motion.maximum_object_bytes,
                self.limits.motion.type_tree,
            )
            .and_then(|value| {
                project_cubism_fade_motion(object.path_id, &value, self.limits.motion)
            }) {
            Ok(motion) => motion,
            Err(error) => {
                self.push_diagnostic(
                    key,
                    Live2dPackageDiagnosticKind::MotionReadFailed,
                    format!("CubismFadeMotionData cannot be projected: {error}"),
                )?;
                return Ok(None);
            }
        };
        let remaining = self
            .limits
            .maximum_total_motion_bytes
            .checked_sub(self.motion_bytes)
            .ok_or_else(|| Error::invalid_data("Live2D motion byte accounting underflowed"))?;
        let written = motion.write_motion3_json(
            targets,
            self.limits.force_bezier_motions,
            &mut io::sink(),
            remaining.min(self.limits.motion.maximum_output_bytes),
        )?;
        self.motion_bytes = charge_u64(
            self.motion_bytes,
            written,
            self.limits.maximum_total_motion_bytes,
            "Live2D motion JSON bytes",
        )?;
        let stem = safe_file_stem(portable_file_stem(&motion.source_name))?;
        let name = claim_name(&stem, key, claimed)?;
        let prefix = concatenate("motions/", &name, "Live2D motion path")?;
        let file_name = concatenate(&prefix, ".motion3.json", "Live2D motion file name")?;
        self.charge_name(&name)?;
        self.charge_name(&file_name)?;
        Ok(Some(Live2dPackageMotion {
            object: key,
            name,
            file_name,
            motion: Live2dPackageMotionSource::Fade(motion),
        }))
    }

    fn read_model_clip_motions(
        &mut self,
        game_object: SceneObjectKey,
        targets: &CubismMotionTargetNames,
    ) -> Result<Vec<Live2dPackageMotion>> {
        let clip_keys = self.model_clip_keys(game_object)?;
        let mut motions = Vec::new();
        let mut claimed = HashSet::new();
        motions.try_reserve(clip_keys.len()).map_err(|error| {
            Error::invalid_data(format!("cannot allocate Live2D clip motions: {error}"))
        })?;
        claimed.try_reserve(clip_keys.len()).map_err(|error| {
            Error::invalid_data(format!("cannot allocate Live2D clip-motion names: {error}"))
        })?;
        for key in clip_keys {
            if let Some(motion) = self.project_clip_motion(key, targets, &mut claimed)? {
                motions.push(motion);
            }
        }
        Ok(motions)
    }

    fn model_clip_keys(&mut self, game_object: SceneObjectKey) -> Result<Vec<SceneObjectKey>> {
        if self.animation_graph.is_none() {
            self.animation_graph = Some(build_animation_graph(
                self.collection,
                &self.hierarchy,
                self.limits.animation_graph,
            )?);
        }
        let graph = self
            .animation_graph
            .as_ref()
            .expect("animation graph was initialized");
        let Some(binding) = graph
            .animators
            .iter()
            .find(|binding| binding.game_object == game_object)
        else {
            return Ok(Vec::new());
        };
        let mut keys = Vec::new();
        let mut seen = HashSet::new();
        keys.try_reserve(binding.bound_clips.len())
            .map_err(|error| {
                Error::invalid_data(format!("cannot allocate bound Live2D clips: {error}"))
            })?;
        seen.try_reserve(binding.bound_clips.len())
            .map_err(|error| {
                Error::invalid_data(format!("cannot allocate bound Live2D clip index: {error}"))
            })?;
        for key in binding.bound_clips.iter().flatten().copied() {
            if !seen.insert(key) {
                continue;
            }
            keys.push(key);
        }
        Ok(keys)
    }

    fn project_clip_motion(
        &mut self,
        key: SceneObjectKey,
        targets: &CubismMotionTargetNames,
        claimed: &mut HashSet<String>,
    ) -> Result<Option<Live2dPackageMotion>> {
        self.motions = charge_usize(
            self.motions,
            1,
            self.limits.maximum_motions,
            "Live2D motions",
        )?;
        let object_index = self
            .collection
            .object_index_by_path_id(key.file_index, key.path_id)
            .ok_or_else(|| Error::invalid_data("bound AnimationClip vanished from collection"))?;
        let motion = match read_cubism_clip_motion_with_acl_decoder(
            self.collection,
            key.file_index,
            object_index,
            targets,
            self.limits.clip_motion,
            self.acl_decoder,
        ) {
            Ok(motion) => motion,
            Err(error) => {
                self.push_diagnostic(
                    key,
                    Live2dPackageDiagnosticKind::MotionReadFailed,
                    format!("AnimationClip cannot be projected to motion3: {error}"),
                )?;
                return Ok(None);
            }
        };
        if motion.curves.is_empty() && motion.events.is_empty() {
            return Ok(None);
        }
        let remaining = self
            .limits
            .maximum_total_motion_bytes
            .checked_sub(self.motion_bytes)
            .ok_or_else(|| Error::invalid_data("Live2D motion byte accounting underflowed"))?;
        let written = motion.write_motion3_json(
            self.limits.force_bezier_motions,
            &mut io::sink(),
            remaining.min(self.limits.clip_motion.maximum_output_bytes),
        )?;
        self.motion_bytes = charge_u64(
            self.motion_bytes,
            written,
            self.limits.maximum_total_motion_bytes,
            "Live2D motion JSON bytes",
        )?;
        let stem = safe_file_stem(portable_file_stem(&motion.name))?;
        let name = claim_name(&stem, key, claimed)?;
        let prefix = concatenate("motions/", &name, "Live2D motion path")?;
        let file_name = concatenate(&prefix, ".motion3.json", "Live2D motion file name")?;
        self.charge_name(&name)?;
        self.charge_name(&file_name)?;
        Ok(Some(Live2dPackageMotion {
            object: key,
            name,
            file_name,
            motion: Live2dPackageMotionSource::AnimationClip(motion),
        }))
    }

    fn build_physics_file(
        &mut self,
        model_name: &str,
        identity: Option<(usize, usize)>,
        motion_fps: f64,
    ) -> Result<Option<Live2dPackageJsonFile>> {
        let Some((file_index, object_index)) = identity else {
            return Ok(None);
        };
        let loaded = self
            .collection
            .serialized_files
            .get(file_index)
            .ok_or_else(|| {
                Error::invalid_data("Cubism physics controller file index is out of range")
            })?;
        let object = loaded.file.objects.get(object_index).ok_or_else(|| {
            Error::invalid_data("Cubism physics controller object index is out of range")
        })?;
        let key = SceneObjectKey {
            file_index,
            path_id: object.path_id,
        };
        self.type_tree_reads = charge_usize(
            self.type_tree_reads,
            1,
            self.limits.maximum_type_tree_reads,
            "Live2D TypeTree reads",
        )?;
        self.type_tree_object_bytes = charge_u64(
            self.type_tree_object_bytes,
            object.byte_size,
            self.limits.maximum_total_type_tree_object_bytes,
            "Live2D TypeTree object bytes",
        )?;
        let rig = match self
            .read_schema_value(
                file_index,
                object_index,
                self.limits.physics.maximum_object_bytes,
                self.limits.physics.type_tree,
            )
            .and_then(|value| project_cubism_physics(object.path_id, &value, self.limits.physics))
        {
            Ok(rig) => rig,
            Err(error) => {
                self.push_diagnostic(
                    key,
                    Live2dPackageDiagnosticKind::PhysicsReadFailed,
                    format!("CubismPhysicsController cannot be projected: {error}"),
                )?;
                return Ok(None);
            }
        };
        let remaining = self
            .limits
            .maximum_total_physics_bytes
            .checked_sub(self.physics_bytes)
            .ok_or_else(|| Error::invalid_data("Live2D physics byte accounting underflowed"))?;
        let maximum = self.limits.physics.maximum_output_bytes.min(remaining);
        let capacity = usize::try_from(maximum).map_err(|_| {
            Error::invalid_data("Live2D physics output limit does not fit this platform")
        })?;
        let mut output = FallibleBuffer::new(capacity, "Live2D physics JSON");
        let written = rig.write_physics3_json(motion_fps, &mut output, maximum)?;
        self.physics_bytes = charge_u64(
            self.physics_bytes,
            written,
            self.limits.maximum_total_physics_bytes,
            "Live2D physics JSON bytes",
        )?;
        let file_name = concatenate(model_name, ".physics3.json", "Live2D physics file name")?;
        self.charge_name(&file_name)?;
        Ok(Some(Live2dPackageJsonFile {
            file_name,
            bytes: output.bytes,
        }))
    }

    fn build_pose_file(
        &mut self,
        model_name: &str,
        components: &[ScriptedComponent],
    ) -> Result<Option<Live2dPackageJsonFile>> {
        let mut nodes = Vec::new();
        nodes.try_reserve(components.len()).map_err(|error| {
            Error::invalid_data(format!("cannot allocate Live2D pose nodes: {error}"))
        })?;
        for component in components {
            self.pose_parts = charge_usize(
                self.pose_parts,
                1,
                self.limits.maximum_pose_parts,
                "Live2D pose parts",
            )?;
            self.charge_auxiliary_input(component)?;
            let pose = match self
                .read_schema_value(
                    component.object.file_index,
                    component.object_index,
                    self.limits.auxiliary.maximum_object_bytes,
                    self.limits.auxiliary.type_tree,
                )
                .and_then(|value| {
                    project_cubism_pose_part(
                        component.object.path_id,
                        &value,
                        self.limits.auxiliary,
                    )
                }) {
                Ok(pose) => pose,
                Err(error) => {
                    self.push_diagnostic(
                        component.object,
                        Live2dPackageDiagnosticKind::AuxiliaryReadFailed,
                        format!("CubismPosePart cannot be projected: {error}"),
                    )?;
                    continue;
                }
            };
            let Some(id) = self.component_game_object_name(component)? else {
                continue;
            };
            nodes.push((
                pose.group_index,
                component.object,
                CubismPoseNode {
                    id,
                    links: pose.links,
                },
            ));
        }
        if nodes.is_empty() {
            return Ok(None);
        }
        nodes.sort_by_key(|(group, object, _)| (*group, *object));
        let groups = group_pose_nodes(nodes)?;
        let file_name = concatenate(model_name, ".pose3.json", "Live2D pose file name")?;
        self.charge_name(&file_name)?;
        self.write_auxiliary_json(file_name, "Live2D pose JSON", |output, maximum| {
            write_cubism_pose3_json(&groups, output, maximum)
        })
        .map(Some)
    }

    fn build_display_info_file(
        &mut self,
        model_name: &str,
        parameter_components: &[ScriptedComponent],
        part_components: &[ScriptedComponent],
    ) -> Result<Option<Live2dPackageJsonFile>> {
        let parameters = self.read_display_entries(parameter_components)?;
        let parts = self.read_display_entries(part_components)?;
        if parameters.is_empty() && parts.is_empty() {
            return Ok(None);
        }
        let file_name = concatenate(model_name, ".cdi3.json", "Live2D display-info file name")?;
        self.charge_name(&file_name)?;
        self.write_auxiliary_json(file_name, "Live2D display-info JSON", |output, maximum| {
            write_cubism_cdi3_json(&parameters, &parts, output, maximum)
        })
        .map(Some)
    }

    fn read_display_entries(
        &mut self,
        components: &[ScriptedComponent],
    ) -> Result<Vec<CubismDisplayEntry>> {
        let mut entries = Vec::new();
        entries.try_reserve(components.len()).map_err(|error| {
            Error::invalid_data(format!("cannot allocate Live2D display entries: {error}"))
        })?;
        for component in components {
            self.display_entries = charge_usize(
                self.display_entries,
                1,
                self.limits.maximum_display_entries,
                "Live2D display-info entries",
            )?;
            self.charge_auxiliary_input(component)?;
            let info = match self
                .read_schema_value(
                    component.object.file_index,
                    component.object_index,
                    self.limits.auxiliary.maximum_object_bytes,
                    self.limits.auxiliary.type_tree,
                )
                .and_then(|value| {
                    project_cubism_display_info(
                        component.object.path_id,
                        &value,
                        self.limits.auxiliary,
                    )
                }) {
                Ok(info) => info,
                Err(error) => {
                    self.push_diagnostic(
                        component.object,
                        Live2dPackageDiagnosticKind::AuxiliaryReadFailed,
                        format!("Cubism display info cannot be projected: {error}"),
                    )?;
                    continue;
                }
            };
            let Some(id) = self.component_game_object_name(component)? else {
                continue;
            };
            let name = match info.display_name {
                Some(display_name) if !display_name.is_empty() => display_name,
                _ => info.name,
            };
            entries.push((lowercase_key(&id)?, CubismDisplayEntry { id, name }));
        }
        entries.sort_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then_with(|| left.1.id.cmp(&right.1.id))
        });
        entries.dedup_by(|left, right| left.0 == right.0);
        let mut projected = Vec::new();
        projected.try_reserve(entries.len()).map_err(|error| {
            Error::invalid_data(format!("cannot finalize Live2D display entries: {error}"))
        })?;
        projected.extend(entries.into_iter().map(|(_, entry)| entry));
        Ok(projected)
    }

    fn charge_auxiliary_input(&mut self, component: &ScriptedComponent) -> Result<()> {
        self.type_tree_reads = charge_usize(
            self.type_tree_reads,
            1,
            self.limits.maximum_type_tree_reads,
            "Live2D TypeTree reads",
        )?;
        let object = &self.collection.serialized_files[component.object.file_index]
            .file
            .objects[component.object_index];
        self.type_tree_object_bytes = charge_u64(
            self.type_tree_object_bytes,
            object.byte_size,
            self.limits.maximum_total_type_tree_object_bytes,
            "Live2D TypeTree object bytes",
        )?;
        Ok(())
    }

    fn component_game_object_name(
        &mut self,
        component: &ScriptedComponent,
    ) -> Result<Option<String>> {
        let Some(game_object) = component.game_object else {
            return Ok(None);
        };
        let Some(node) = self.hierarchy.node(game_object) else {
            return Ok(None);
        };
        let name = copy_string(&node.name, "Live2D GameObject name")?;
        self.charge_name(&name)?;
        Ok(Some(name))
    }

    fn write_auxiliary_json(
        &mut self,
        file_name: String,
        field: &'static str,
        write: impl FnOnce(&mut FallibleBuffer, u64) -> Result<u64>,
    ) -> Result<Live2dPackageJsonFile> {
        let remaining = self
            .limits
            .maximum_total_auxiliary_bytes
            .checked_sub(self.auxiliary_bytes)
            .ok_or_else(|| Error::invalid_data("Live2D auxiliary byte accounting underflowed"))?;
        let maximum = self.limits.maximum_auxiliary_file_bytes.min(remaining);
        let capacity = usize::try_from(maximum).map_err(|_| {
            Error::invalid_data(format!("{field} output limit does not fit this platform"))
        })?;
        let mut output = FallibleBuffer::new(capacity, field);
        let written = write(&mut output, maximum)?;
        let actual = u64::try_from(output.bytes.len())
            .map_err(|_| Error::invalid_data(format!("{field} length does not fit u64")))?;
        if written != actual {
            return Err(Error::invalid_data(format!(
                "{field} writer reported {written} bytes but produced {actual}"
            )));
        }
        self.auxiliary_bytes = charge_u64(
            self.auxiliary_bytes,
            actual,
            self.limits.maximum_total_auxiliary_bytes,
            "Live2D auxiliary JSON bytes",
        )?;
        Ok(Live2dPackageJsonFile {
            file_name,
            bytes: output.bytes,
        })
    }

    fn scripted_component(
        &self,
        identity: (usize, usize),
        role: CubismRole,
    ) -> Result<ScriptedComponent> {
        let loaded = self
            .collection
            .serialized_files
            .get(identity.0)
            .ok_or_else(|| {
                Error::invalid_data("resolved Cubism component file index is out of range")
            })?;
        let object = loaded.file.objects.get(identity.1).ok_or_else(|| {
            Error::invalid_data("resolved Cubism component object index is out of range")
        })?;
        if object.class_id != MONO_BEHAVIOUR_CLASS_ID {
            return Err(Error::invalid_data(
                "resolved Cubism component no longer has MonoBehaviour class ID",
            ));
        }
        let behaviour = read_mono_behaviour(&loaded.file, identity.1, self.limits.mono_behaviour)?;
        Ok(ScriptedComponent {
            object: SceneObjectKey {
                file_index: identity.0,
                path_id: object.path_id,
            },
            object_index: identity.1,
            game_object: self.resolve_game_object(identity.0, behaviour.game_object),
            behaviour,
            role,
        })
    }

    fn required_reference(
        &mut self,
        component: &ScriptedComponent,
        field_name: &str,
    ) -> Result<Option<ObjectReference>> {
        let Some(value) = self.read_embedded_value(component, field_name)? else {
            return Ok(None);
        };
        let Some(field) = self.required_component_field(component, &value, field_name)? else {
            return Ok(None);
        };
        let Some(reference) = parse_pptr(field) else {
            self.push_diagnostic(
                component.object,
                Live2dPackageDiagnosticKind::MalformedRequiredField,
                format!("{}.{field_name} is not a valid PPtr", component.role_name()),
            )?;
            return Ok(None);
        };
        if reference.is_null() {
            self.push_diagnostic(
                component.object,
                Live2dPackageDiagnosticKind::NullReference,
                format!("{}.{field_name} is null", component.role_name()),
            )?;
            return Ok(None);
        }
        Ok(Some(reference))
    }

    fn required_references(
        &mut self,
        component: &ScriptedComponent,
        field_name: &str,
    ) -> Result<Option<Vec<ObjectReference>>> {
        let Some(value) = self.read_embedded_value(component, field_name)? else {
            return Ok(None);
        };
        let Some(field) = self.required_component_field(component, &value, field_name)? else {
            return Ok(None);
        };
        let TypeValue::Array(values) = field else {
            self.push_diagnostic(
                component.object,
                Live2dPackageDiagnosticKind::MalformedRequiredField,
                format!("{}.{field_name} is not an array", component.role_name()),
            )?;
            return Ok(None);
        };
        let mut references = Vec::new();
        references.try_reserve(values.len()).map_err(|error| {
            Error::invalid_data(format!("cannot allocate Live2D PPtr array: {error}"))
        })?;
        for (index, value) in values.iter().enumerate() {
            let Some(reference) = parse_pptr(value) else {
                self.push_diagnostic(
                    component.object,
                    Live2dPackageDiagnosticKind::MalformedRequiredField,
                    format!(
                        "{}.{field_name}[{index}] is not a valid PPtr",
                        component.role_name()
                    ),
                )?;
                continue;
            };
            if !reference.is_null() {
                references.push(reference);
            }
        }
        Ok(Some(references))
    }

    fn read_embedded_value(
        &mut self,
        component: &ScriptedComponent,
        field_name: &str,
    ) -> Result<Option<TypeValue>> {
        self.type_tree_reads = charge_usize(
            self.type_tree_reads,
            1,
            self.limits.maximum_type_tree_reads,
            "Live2D TypeTree reads",
        )?;
        let loaded = &self.collection.serialized_files[component.object.file_index];
        let object = &loaded.file.objects[component.object_index];
        self.type_tree_object_bytes = charge_u64(
            self.type_tree_object_bytes,
            object.byte_size,
            self.limits.maximum_total_type_tree_object_bytes,
            "Live2D TypeTree object bytes",
        )?;
        let has_embedded_tree = object
            .serialized_type_index
            .and_then(|index| loaded.file.types.get(index))
            .and_then(|serialized_type| serialized_type.type_tree.as_ref())
            .is_some_and(|tree| !tree.nodes.is_empty());
        if !has_embedded_tree && self.schema_provider.is_none() {
            self.push_diagnostic(
                component.object,
                Live2dPackageDiagnosticKind::MissingEmbeddedTypeTree,
                format!(
                    "{}.{field_name} cannot be inspected because the serialized type has no embedded TypeTree",
                    component.role_name()
                ),
            )?;
            return Ok(None);
        }
        let value = match self.read_schema_value(
            component.object.file_index,
            component.object_index,
            u64::MAX,
            self.limits.type_tree,
        ) {
            Ok(value) => value,
            Err(Error::Unsupported(message)) => {
                let kind = if self.schema_provider.is_some() {
                    Live2dPackageDiagnosticKind::MissingExternalSchema
                } else {
                    Live2dPackageDiagnosticKind::MissingEmbeddedTypeTree
                };
                self.push_diagnostic(
                    component.object,
                    kind,
                    format!(
                        "{}.{field_name} cannot be inspected through its schema: {message}",
                        component.role_name()
                    ),
                )?;
                return Ok(None);
            }
            Err(error) => {
                let kind = if has_embedded_tree {
                    Live2dPackageDiagnosticKind::MalformedEmbeddedTypeTree
                } else {
                    Live2dPackageDiagnosticKind::MalformedExternalSchema
                };
                self.push_diagnostic(
                    component.object,
                    kind,
                    format!(
                        "{} object schema cannot be decoded: {error}",
                        component.role_name()
                    ),
                )?;
                return Ok(None);
            }
        };
        if !matches!(value, TypeValue::Object(_)) {
            self.push_diagnostic(
                component.object,
                Live2dPackageDiagnosticKind::MalformedEmbeddedTypeTree,
                format!(
                    "{} embedded TypeTree root is not an object",
                    component.role_name()
                ),
            )?;
            return Ok(None);
        }
        Ok(Some(value))
    }

    fn read_schema_value(
        &self,
        file_index: usize,
        object_index: usize,
        maximum_object_bytes: u64,
        type_tree: TypeTreeReadLimits,
    ) -> Result<TypeValue> {
        let loaded = self
            .collection
            .serialized_files
            .get(file_index)
            .ok_or_else(|| Error::invalid_data("Live2D schema file index is out of range"))?;
        let object = loaded
            .file
            .objects
            .get(object_index)
            .ok_or_else(|| Error::invalid_data("Live2D schema object index is out of range"))?;
        if object.byte_size > maximum_object_bytes {
            return Err(Error::invalid_data(format!(
                "Live2D schema object is {} bytes, limit is {maximum_object_bytes}",
                object.byte_size
            )));
        }
        let Some(provider) = self.schema_provider else {
            return loaded
                .file
                .read_type_tree_value_with_limits(object_index, type_tree);
        };
        let limits = MonoBehaviourReadLimits {
            type_tree,
            ..self.limits.mono_behaviour
        };
        read_mono_behaviour_value_with_provider(
            self.collection,
            file_index,
            object_index,
            provider,
            limits,
        )
        .map(|resolved| resolved.value)
    }

    fn required_component_field<'b>(
        &mut self,
        component: &ScriptedComponent,
        value: &'b TypeValue,
        field_name: &str,
    ) -> Result<Option<&'b TypeValue>> {
        let TypeValue::Object(fields) = value else {
            return Ok(None);
        };
        let Some(field) = fields
            .iter()
            .find(|field| field.name.eq_ignore_ascii_case(field_name))
        else {
            self.push_diagnostic(
                component.object,
                Live2dPackageDiagnosticKind::MissingRequiredField,
                format!(
                    "{} embedded TypeTree has no {field_name} field",
                    component.role_name()
                ),
            )?;
            return Ok(None);
        };
        Ok(Some(&field.value))
    }

    fn resolve_required_target<'b>(
        &'b mut self,
        owner: SceneObjectKey,
        reference: ObjectReference,
        expected_class: i32,
        field: &str,
    ) -> Result<Option<crate::scene::ResolvedObject<'a>>> {
        let target = match resolve_object_reference(self.collection, owner.file_index, reference) {
            Ok(Some(target)) => target,
            Ok(None) => {
                self.push_diagnostic(
                    owner,
                    Live2dPackageDiagnosticKind::NullReference,
                    format!("{field} is null"),
                )?;
                return Ok(None);
            }
            Err(error) => {
                self.push_diagnostic(
                    owner,
                    Live2dPackageDiagnosticKind::UnresolvedReference,
                    format!("{field} cannot be resolved: {error}"),
                )?;
                return Ok(None);
            }
        };
        if target.object.class_id != expected_class {
            let actual = target.object.class_id;
            self.push_diagnostic(
                owner,
                Live2dPackageDiagnosticKind::WrongReferenceClass,
                format!("{field} resolves to class {actual}, expected {expected_class}"),
            )?;
            return Ok(None);
        }
        Ok(Some(target))
    }

    fn charge_name(&mut self, value: &str) -> Result<()> {
        self.name_bytes = charge_usize(
            self.name_bytes,
            value.len(),
            self.limits.maximum_total_name_bytes,
            "Live2D names",
        )?;
        Ok(())
    }

    fn push_diagnostic(
        &mut self,
        object: SceneObjectKey,
        kind: Live2dPackageDiagnosticKind,
        message: impl Into<String>,
    ) -> Result<()> {
        let message = message.into();
        self.diagnostic_bytes = charge_usize(
            self.diagnostic_bytes,
            message.len(),
            self.limits.maximum_total_diagnostic_bytes,
            "Live2D diagnostics",
        )?;
        self.diagnostics.try_reserve(1).map_err(|error| {
            Error::invalid_data(format!("cannot grow Live2D diagnostics: {error}"))
        })?;
        self.diagnostics.push(Live2dPackageDiagnostic {
            object,
            kind,
            message,
        });
        Ok(())
    }
}

impl ScriptedComponent {
    fn role_name(&self) -> &'static str {
        match self.role {
            CubismRole::Model => "CubismModel",
            CubismRole::Renderer => "CubismRenderer",
            CubismRole::Moc => "CubismMoc",
            CubismRole::ExpressionController => "CubismExpressionController",
            CubismRole::ExpressionList => "CubismExpressionList",
            CubismRole::ExpressionData => "CubismExpressionData",
            _ => "Cubism component",
        }
    }
}

fn component_identity(component: &ScriptedComponent) -> (usize, usize) {
    (component.object.file_index, component.object_index)
}

fn push_component(
    values: &mut Vec<ScriptedComponent>,
    component: ScriptedComponent,
    field: &str,
) -> Result<()> {
    values
        .try_reserve(1)
        .map_err(|error| Error::invalid_data(format!("cannot grow {field}: {error}")))?;
    values.push(component);
    Ok(())
}

fn group_component_assignments(
    maximum_groups: usize,
    assignments: Vec<(SceneObjectKey, ScriptedComponent)>,
) -> Result<Vec<(SceneObjectKey, Vec<ScriptedComponent>)>> {
    let mut grouped = Vec::new();
    grouped
        .try_reserve(maximum_groups.min(assignments.len()))
        .map_err(|error| {
            Error::invalid_data(format!("cannot allocate Live2D auxiliary groups: {error}"))
        })?;
    for (model, component) in assignments {
        if grouped.last().is_none_or(
            |(last_model, _): &(SceneObjectKey, Vec<ScriptedComponent>)| *last_model != model,
        ) {
            grouped.push((model, Vec::new()));
        }
        let components = &mut grouped
            .last_mut()
            .expect("auxiliary group was just added")
            .1;
        components.try_reserve(1).map_err(|error| {
            Error::invalid_data(format!("cannot grow Live2D auxiliary group: {error}"))
        })?;
        components.push(component);
    }
    Ok(grouped)
}

fn components_for_model(
    groups: &[(SceneObjectKey, Vec<ScriptedComponent>)],
    model: SceneObjectKey,
) -> &[ScriptedComponent] {
    let position = groups.partition_point(|(candidate, _)| *candidate < model);
    groups
        .get(position)
        .filter(|(candidate, _)| *candidate == model)
        .map_or(&[], |(_, components)| components.as_slice())
}

fn group_pose_nodes(
    nodes: Vec<(i32, SceneObjectKey, CubismPoseNode)>,
) -> Result<Vec<Vec<CubismPoseNode>>> {
    let mut keyed_groups: Vec<(i32, Vec<CubismPoseNode>)> = Vec::new();
    keyed_groups.try_reserve(nodes.len()).map_err(|error| {
        Error::invalid_data(format!("cannot allocate Live2D pose groups: {error}"))
    })?;
    for (group_index, _, node) in nodes {
        if keyed_groups
            .last()
            .is_none_or(|(last_group, _)| *last_group != group_index)
        {
            keyed_groups.push((group_index, Vec::new()));
        }
        let group = &mut keyed_groups
            .last_mut()
            .expect("pose group was just added")
            .1;
        group.try_reserve(1).map_err(|error| {
            Error::invalid_data(format!("cannot grow Live2D pose group: {error}"))
        })?;
        group.push(node);
    }
    let mut groups = Vec::new();
    groups.try_reserve(keyed_groups.len()).map_err(|error| {
        Error::invalid_data(format!("cannot finalize Live2D pose groups: {error}"))
    })?;
    groups.extend(keyed_groups.into_iter().map(|(_, group)| group));
    Ok(groups)
}

fn copy_string(value: &str, field: &str) -> Result<String> {
    let mut output = String::new();
    output
        .try_reserve_exact(value.len())
        .map_err(|error| Error::invalid_data(format!("cannot allocate {field}: {error}")))?;
    output.push_str(value);
    Ok(output)
}

fn role_for_identity(
    roles: &[((usize, usize), CubismRole)],
    identity: (usize, usize),
) -> Option<CubismRole> {
    let position = roles.partition_point(|(candidate, _)| *candidate < identity);
    roles
        .get(position)
        .filter(|(candidate, _)| *candidate == identity)
        .map(|(_, role)| *role)
}

fn remove_exp3_suffixes(value: &str) -> Result<String> {
    let length = value
        .split(".exp3")
        .try_fold(0_usize, |total, part| total.checked_add(part.len()))
        .ok_or_else(|| Error::invalid_data("Live2D expression name length overflowed"))?;
    let mut output = String::new();
    output.try_reserve_exact(length).map_err(|error| {
        Error::invalid_data(format!("cannot allocate Live2D expression name: {error}"))
    })?;
    for part in value.split(".exp3") {
        output.push_str(part);
    }
    Ok(output)
}

fn nearest_parent_model(
    hierarchy: &SceneHierarchy,
    renderer_game_object: SceneObjectKey,
    models: &[(SceneObjectKey, SceneObjectKey)],
) -> Option<SceneObjectKey> {
    let mut current = hierarchy.node(renderer_game_object)?.parent;
    while let Some(game_object) = current {
        let position = models.partition_point(|(candidate, _)| *candidate < game_object);
        if let Some((candidate, model)) = models.get(position)
            && *candidate == game_object
        {
            return Some(*model);
        }
        current = hierarchy.node(game_object)?.parent;
    }
    None
}

fn parse_pptr(value: &TypeValue) -> Option<ObjectReference> {
    let TypeValue::Object(fields) = value else {
        return None;
    };
    let file_id = fields
        .iter()
        .find(|field| field.name == "m_FileID")
        .and_then(|field| integer(&field.value))
        .and_then(|value| i32::try_from(value).ok())?;
    let path_id = fields
        .iter()
        .find(|field| field.name == "m_PathID")
        .and_then(|field| integer(&field.value))?;
    Some(ObjectReference { file_id, path_id })
}

fn integer(value: &TypeValue) -> Option<i64> {
    match value {
        TypeValue::Signed(value) => Some(*value),
        TypeValue::Unsigned(value) => i64::try_from(*value).ok(),
        _ => None,
    }
}

fn safe_file_stem(value: &str) -> Result<String> {
    let mut output = String::new();
    output.try_reserve_exact(value.len()).map_err(|error| {
        Error::invalid_data(format!("cannot allocate Live2D file name: {error}"))
    })?;
    for character in value.chars() {
        if character.is_control()
            || matches!(
                character,
                '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'
            )
        {
            output.push('_');
        } else {
            output.push(character);
        }
    }
    let output = output.trim_matches([' ', '.']);
    if output.is_empty() {
        Ok("unnamed".to_owned())
    } else {
        let mut trimmed = String::new();
        trimmed.try_reserve_exact(output.len()).map_err(|error| {
            Error::invalid_data(format!("cannot allocate trimmed Live2D file name: {error}"))
        })?;
        trimmed.push_str(output);
        Ok(trimmed)
    }
}

fn claim_name(base: &str, object: SceneObjectKey, claimed: &mut HashSet<String>) -> Result<String> {
    for ordinal in 0_u64.. {
        let suffix_bytes = if ordinal == 0 {
            0
        } else {
            3_usize
                .checked_add(decimal_digits_u128(object.file_index as u128))
                .and_then(|value| value.checked_add(2))
                .and_then(|value| value.checked_add(decimal_digits_i64(object.path_id)))
                .and_then(|value| {
                    if ordinal == 1 {
                        Some(value)
                    } else {
                        value.checked_add(1).and_then(|value| {
                            value.checked_add(decimal_digits_u128(ordinal.into()))
                        })
                    }
                })
                .ok_or_else(|| Error::invalid_data("Live2D collision suffix length overflowed"))?
        };
        let capacity = base
            .len()
            .checked_add(suffix_bytes)
            .ok_or_else(|| Error::invalid_data("Live2D collision file-name length overflowed"))?;
        let mut candidate = String::new();
        candidate.try_reserve_exact(capacity).map_err(|error| {
            Error::invalid_data(format!(
                "cannot allocate Live2D collision file name: {error}"
            ))
        })?;
        candidate.push_str(base);
        if ordinal != 0 {
            use std::fmt::Write as _;
            write!(candidate, " @f{}_p{}", object.file_index, object.path_id)
                .expect("reserved String writes cannot fail");
            if ordinal > 1 {
                write!(candidate, "_{ordinal}").expect("reserved String writes cannot fail");
            }
        }
        debug_assert_eq!(candidate.len(), capacity);
        let identity = lowercase_key(&candidate)?;
        if !claimed.contains(&identity) {
            claimed.try_reserve(1).map_err(|error| {
                Error::invalid_data(format!("cannot grow Live2D claimed-name index: {error}"))
            })?;
            claimed.insert(identity);
            return Ok(candidate);
        }
    }
    unreachable!("the unbounded ordinal supplies a unique file name")
}

fn lowercase_key(value: &str) -> Result<String> {
    let capacity = value.chars().try_fold(0_usize, |length, character| {
        character
            .to_lowercase()
            .try_fold(length, |length, lowercase| {
                length.checked_add(lowercase.len_utf8())
            })
    });
    let capacity = capacity
        .ok_or_else(|| Error::invalid_data("Live2D lowercase file-name length overflowed"))?;
    let mut key = String::new();
    key.try_reserve_exact(capacity).map_err(|error| {
        Error::invalid_data(format!(
            "cannot allocate Live2D lowercase file name: {error}"
        ))
    })?;
    for character in value.chars().flat_map(char::to_lowercase) {
        key.push(character);
    }
    Ok(key)
}

fn concatenate(left: &str, right: &str, field: &str) -> Result<String> {
    let length = left
        .len()
        .checked_add(right.len())
        .ok_or_else(|| Error::invalid_data(format!("{field} length overflowed")))?;
    let mut output = String::new();
    output
        .try_reserve_exact(length)
        .map_err(|error| Error::invalid_data(format!("cannot allocate {field}: {error}")))?;
    output.push_str(left);
    output.push_str(right);
    Ok(output)
}

fn decimal_digits_i64(value: i64) -> usize {
    usize::from(value.is_negative()) + decimal_digits_u128(value.unsigned_abs().into())
}

fn decimal_digits_u128(mut value: u128) -> usize {
    let mut digits = 1;
    while value >= 10 {
        value /= 10;
        digits += 1;
    }
    digits
}

fn portable_file_stem(path: &str) -> &str {
    let name = path
        .rsplit_once("::")
        .map_or(path, |(_, name)| name)
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(path);
    name.rsplit_once('.').map_or(name, |(stem, _)| stem)
}

fn is_eye_blink_name(name: &str) -> bool {
    contains_ascii_case_insensitive(name, b"eye")
        && contains_ascii_case_insensitive(name, b"open")
        && name
            .as_bytes()
            .iter()
            .any(|value| value.eq_ignore_ascii_case(&b'l') || value.eq_ignore_ascii_case(&b'r'))
}

fn is_lip_sync_name(name: &str) -> bool {
    contains_ascii_case_insensitive(name, b"mouth")
        && contains_ascii_case_insensitive(name, b"open")
        && name
            .as_bytes()
            .iter()
            .any(|value| value.eq_ignore_ascii_case(&b'y'))
}

fn contains_ascii_case_insensitive(value: &str, needle: &[u8]) -> bool {
    value.as_bytes().windows(needle.len()).any(|candidate| {
        candidate
            .iter()
            .zip(needle)
            .all(|(left, right)| left.eq_ignore_ascii_case(right))
    })
}

fn charge_usize(current: usize, additional: usize, maximum: usize, field: &str) -> Result<usize> {
    let total = current
        .checked_add(additional)
        .ok_or_else(|| Error::invalid_data(format!("{field} overflowed")))?;
    if total > maximum {
        return Err(Error::invalid_data(format!(
            "{field} total {total} exceeds limit {maximum}"
        )));
    }
    Ok(total)
}

fn charge_u64(current: u64, additional: u64, maximum: u64, field: &str) -> Result<u64> {
    let total = current
        .checked_add(additional)
        .ok_or_else(|| Error::invalid_data(format!("{field} overflowed")))?;
    if total > maximum {
        return Err(Error::invalid_data(format!(
            "{field} total {total} exceeds limit {maximum}"
        )));
    }
    Ok(total)
}

fn cubism_moc_retained_bytes(moc: &CubismMoc) -> Result<u64> {
    let mut bytes =
        u64::try_from(std::mem::size_of::<CubismMoc>()).expect("CubismMoc size fits in u64");
    let string_slots = moc
        .part_names
        .capacity()
        .checked_add(moc.parameter_names.capacity())
        .and_then(|capacity| capacity.checked_mul(std::mem::size_of::<String>()))
        .ok_or_else(|| Error::invalid_data("Live2D MOC metadata bytes overflowed"))?;
    for allocation in [moc.name.capacity(), string_slots] {
        bytes = bytes
            .checked_add(u64::try_from(allocation).unwrap_or(u64::MAX))
            .ok_or_else(|| Error::invalid_data("Live2D MOC metadata bytes overflowed"))?;
    }
    for value in moc.part_names.iter().chain(&moc.parameter_names) {
        bytes = bytes
            .checked_add(u64::try_from(value.capacity()).unwrap_or(u64::MAX))
            .ok_or_else(|| Error::invalid_data("Live2D MOC metadata bytes overflowed"))?;
    }
    Ok(bytes)
}

fn json_error(error: &serde_json::Error) -> Error {
    Error::invalid_data(format!("cannot write Live2D model3 JSON: {error}"))
}

struct BoundedWriter<'a, W> {
    output: &'a mut W,
    maximum: u64,
    written: u64,
}

impl<'a, W> BoundedWriter<'a, W> {
    const fn new(output: &'a mut W, maximum: u64) -> Self {
        Self {
            output,
            maximum,
            written: 0,
        }
    }
}

impl<W: Write> Write for BoundedWriter<'_, W> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let length = u64::try_from(buffer.len()).unwrap_or(u64::MAX);
        let end = self
            .written
            .checked_add(length)
            .ok_or_else(|| std::io::Error::other("Live2D model3 output byte count overflowed"))?;
        if end > self.maximum {
            return Err(std::io::Error::other(format!(
                "Live2D model3 output exceeds {} bytes",
                self.maximum
            )));
        }
        self.output.write_all(buffer)?;
        self.written = end;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.output.flush()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use crate::loader::{AssetCollection, LoadedSerializedFile};
    use crate::mono_schema::{MonoBehaviourSchemaEntry, MonoBehaviourSchemaRegistry};
    use crate::serialized::{SerializedFile, TypeTree, TypeTreeNode};
    use crate::source::Region;

    use super::{
        Live2dPackageDiagnosticKind, Live2dPackageLimits, Live2dPackageMaterializeLimits,
        PackageState, build_live2d_packages, build_live2d_packages_with_schema_provider,
        claim_name, materialize_live2d_packages, safe_file_stem,
    };

    const GAME_OBJECT: i32 = 1;
    const TRANSFORM: i32 = 4;
    const TEXTURE_2D: i32 = 28;
    const MONO_BEHAVIOUR: i32 = 114;
    const MONO_SCRIPT: i32 = 115;

    #[test]
    fn selects_animator_bound_clips_for_fade_motion_fallback() {
        const ANIMATION_CLIP: i32 = 74;
        const ANIMATOR_CONTROLLER: i32 = 91;
        const ANIMATOR: i32 = 95;
        let types = vec![
            TestType::plain(GAME_OBJECT),
            TestType::plain(TRANSFORM),
            TestType::plain(ANIMATION_CLIP),
            TestType::plain(ANIMATOR_CONTROLLER),
            TestType::plain(ANIMATOR),
        ];
        let objects = vec![
            TestObject::new(1, 0, game_object("Hero", &[(0, 10), (0, 2)])),
            TestObject::new(10, 1, transform((0, 1), &[], (0, 0))),
            TestObject::new(2, 4, animator(3)),
            TestObject::new(3, 3, animator_controller("Live2D", 4)),
            TestObject::new(4, 2, aligned_string("Idle")),
        ];
        let file =
            SerializedFile::open(Region::from_bytes(synthetic_v22(&types, &objects, &[]))).unwrap();
        let collection = AssetCollection::from_loaded_parts(
            vec![LoadedSerializedFile {
                path: "animation.assets".to_owned(),
                file,
            }],
            Vec::new(),
        );
        let limits = Live2dPackageLimits::default();
        let hierarchy = super::build_scene_hierarchy(&collection, limits.hierarchy).unwrap();
        let mut state = PackageState::new(&collection, &limits, hierarchy, None, None);

        let clips = state
            .model_clip_keys(super::SceneObjectKey {
                file_index: 0,
                path_id: 1,
            })
            .unwrap();

        assert_eq!(clips.len(), 1);
        assert_eq!(clips[0].path_id, 4);
    }

    #[test]
    fn builds_cross_file_static_package_and_exact_manifest() {
        let collection = synthetic_collection(true, true);

        let set = build_live2d_packages(&collection, Live2dPackageLimits::default()).unwrap();

        assert!(set.diagnostics.is_empty(), "{:?}", set.diagnostics);
        assert_eq!(set.packages.len(), 1);
        let package = &set.packages[0];
        assert_eq!(package.name, "Hero");
        assert_eq!(package.directory_name, "Hero");
        assert_eq!(package.moc_file_name, "Hero.moc3");
        assert_eq!(package.manifest_file_name, "Hero.model3.json");
        assert_eq!(package.moc_object.file_index, 0);
        assert_eq!(package.moc_object.path_id, 30);
        assert_eq!(package.textures.len(), 1);
        assert_eq!(package.textures[0].object.file_index, 1);
        assert_eq!(package.textures[0].object.path_id, 40);
        assert_eq!(package.textures[0].source_name, "face");
        assert_eq!(package.textures[0].file_name, "textures/face.png");

        let mut moc = Vec::new();
        assert_eq!(package.moc.write_moc3(&mut moc, 5).unwrap(), 5);
        assert_eq!(moc, b"MOC3\x09");

        let mut manifest = Vec::new();
        let length = package.write_model3_json(&mut manifest, u64::MAX).unwrap();
        assert_eq!(length, u64::try_from(manifest.len()).unwrap());
        assert_eq!(
            String::from_utf8(manifest.clone()).unwrap(),
            concat!(
                "{\n",
                "  \"Version\": 3,\n",
                "  \"Name\": \"Hero\",\n",
                "  \"FileReferences\": {\n",
                "    \"Moc\": \"Hero.moc3\",\n",
                "    \"Textures\": [\n",
                "      \"textures/face.png\"\n",
                "    ]\n",
                "  },\n",
                "  \"Groups\": [\n",
                "    { \"Target\": \"Parameter\", \"Name\": \"EyeBlink\", \"Ids\": [] },\n",
                "    { \"Target\": \"Parameter\", \"Name\": \"LipSync\", \"Ids\": [] }\n",
                "  ]\n",
                "}\n"
            )
        );
        assert!(
            package
                .write_model3_json(&mut Vec::new(), length - 1)
                .is_err()
        );
        let parsed: serde_json::Value = serde_json::from_slice(&manifest).unwrap();
        assert_eq!(parsed["Groups"][0]["Name"], "EyeBlink");
        assert_eq!(parsed["Groups"][0]["Ids"], serde_json::json!([]));
        assert_eq!(parsed["Groups"][1]["Name"], "LipSync");
        assert_eq!(parsed["Groups"][1]["Ids"], serde_json::json!([]));
        assert!(parsed["FileReferences"].get("Motions").is_none());
        assert!(parsed["FileReferences"].get("Expressions").is_none());
    }

    #[test]
    fn materializes_binding_friendly_static_package_bytes_with_cumulative_limits() {
        let collection = synthetic_collection(true, true);
        let set = build_live2d_packages(&collection, Live2dPackageLimits::default()).unwrap();
        let materialized =
            materialize_live2d_packages(set, Live2dPackageMaterializeLimits::default()).unwrap();

        assert!(materialized.diagnostics.is_empty());
        assert_eq!(materialized.packages.len(), 1);
        let package = &materialized.packages[0];
        assert_eq!(package.name, "Hero");
        assert_eq!(package.moc_file_name, "Hero.moc3");
        assert_eq!(package.moc, b"MOC3\x09");
        assert_eq!(package.manifest_file_name, "Hero.model3.json");
        assert!(package.manifest.starts_with(b"{\n  \"Version\": 3"));
        assert_eq!(package.textures.len(), 1);
        assert_eq!(package.textures[0].file_name, "textures/face.png");
        assert!(package.textures[0].png.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert!(package.eye_blink_parameters.is_empty());
        assert!(package.lip_sync_parameters.is_empty());

        let set = build_live2d_packages(&collection, Live2dPackageLimits::default()).unwrap();
        let limits = Live2dPackageMaterializeLimits {
            maximum_file_bytes: 4,
            ..Live2dPackageMaterializeLimits::default()
        };
        assert!(matches!(
            materialize_live2d_packages(set, limits),
            Err(crate::Error::InvalidData(_))
        ));

        let set = build_live2d_packages(&collection, Live2dPackageLimits::default()).unwrap();
        let limits = Live2dPackageMaterializeLimits {
            maximum_total_bytes: 5,
            ..Live2dPackageMaterializeLimits::default()
        };
        assert!(matches!(
            materialize_live2d_packages(set, limits),
            Err(crate::Error::InvalidData(_))
        ));
    }

    #[test]
    fn reports_missing_embedded_schema_without_guessing_renderer_tail() {
        let collection = synthetic_collection(true, false);

        let set = build_live2d_packages(&collection, Live2dPackageLimits::default()).unwrap();

        assert_eq!(set.packages.len(), 1);
        assert!(set.packages[0].textures.is_empty());
        assert_eq!(set.diagnostics.len(), 1);
        assert_eq!(
            set.diagnostics[0].kind,
            Live2dPackageDiagnosticKind::MissingEmbeddedTypeTree
        );
        assert!(set.diagnostics[0].message.contains("_mainTexture"));
    }

    #[test]
    fn external_schema_provider_restores_stripped_model_and_renderer_fields() {
        let collection = synthetic_collection(false, false);
        let mut schemas = MonoBehaviourSchemaRegistry::new();
        schemas
            .push(test_schema_entry(
                "CubismModel",
                &mono_behaviour_tree("CubismMoc", "_moc"),
            ))
            .unwrap();
        schemas
            .push(test_schema_entry(
                "CubismRenderer",
                &mono_behaviour_tree("Texture2D", "_mainTexture"),
            ))
            .unwrap();

        let set = build_live2d_packages_with_schema_provider(
            &collection,
            Live2dPackageLimits::default(),
            &schemas,
        )
        .unwrap();

        assert!(set.diagnostics.is_empty(), "{:?}", set.diagnostics);
        assert_eq!(set.packages.len(), 1);
        assert_eq!(set.packages[0].moc_object.path_id, 30);
        assert_eq!(set.packages[0].textures.len(), 1);
        assert_eq!(set.packages[0].textures[0].object.path_id, 40);
    }

    #[test]
    fn reports_missing_model_schema_and_enforces_cumulative_payload_budget() {
        let missing_model = synthetic_collection(false, true);
        let set = build_live2d_packages(&missing_model, Live2dPackageLimits::default()).unwrap();
        assert!(set.packages.is_empty());
        assert_eq!(
            set.diagnostics[0].kind,
            Live2dPackageDiagnosticKind::MissingEmbeddedTypeTree
        );

        let collection = synthetic_collection(true, true);
        let limits = Live2dPackageLimits {
            maximum_total_texture_payload_bytes: 3,
            ..Live2dPackageLimits::default()
        };
        assert!(build_live2d_packages(&collection, limits).is_err());
        let limits = Live2dPackageLimits {
            maximum_total_moc_metadata_bytes: 0,
            ..Live2dPackageLimits::default()
        };
        assert!(build_live2d_packages(&collection, limits).is_err());
    }

    #[test]
    fn duplicate_path_id_role_check_uses_the_resolved_object_table_entry() {
        let collection = synthetic_collection_options(true, true, true);

        let set = build_live2d_packages(&collection, Live2dPackageLimits::default()).unwrap();

        assert!(set.packages.is_empty());
        assert!(set.diagnostics.iter().any(|diagnostic| {
            diagnostic.object.path_id == 20
                && diagnostic.kind == Live2dPackageDiagnosticKind::WrongCubismRole
        }));
    }

    #[test]
    fn selects_model_only_after_transform_and_uses_component_order_last_wins() {
        let collection = active_model_collection(&[(0, 10), (0, 90), (0, 20)]);
        let set = build_live2d_packages(&collection, Live2dPackageLimits::default()).unwrap();
        assert_eq!(set.packages.len(), 1);
        assert_eq!(set.packages[0].model.path_id, 20);

        let collection = active_model_collection(&[(0, 10), (0, 20), (0, 90)]);
        let set = build_live2d_packages(&collection, Live2dPackageLimits::default()).unwrap();
        assert_eq!(set.packages.len(), 1);
        assert_eq!(set.packages[0].model.path_id, 90);

        let before_transform = active_model_collection(&[(0, 90), (0, 10)]);
        let set = build_live2d_packages(&before_transform, Live2dPackageLimits::default()).unwrap();
        assert!(set.packages.is_empty());

        let orphan = active_model_collection(&[(0, 10)]);
        let set = build_live2d_packages(&orphan, Live2dPackageLimits::default()).unwrap();
        assert!(set.packages.is_empty());
    }

    #[test]
    fn packages_shared_moc_once_and_uses_last_active_model() {
        let collection = shared_moc_collection();

        let set = build_live2d_packages(&collection, Live2dPackageLimits::default()).unwrap();

        assert_eq!(set.packages.len(), 1);
        assert_eq!(set.packages[0].model.path_id, 20);
        assert_eq!(set.packages[0].name, "Second");
        assert_eq!(set.packages[0].moc_object.path_id, 30);
    }

    #[test]
    fn writes_explicit_and_inferred_eye_blink_and_lip_sync_groups() {
        for (explicit_markers, expected_eye, expected_mouth) in [
            (true, "BlinkLeft", "MouthValue"),
            (false, "ParamEyeLOpen", "ParamMouthOpenY"),
        ] {
            let collection = parameter_group_collection(explicit_markers);
            let set = build_live2d_packages(&collection, Live2dPackageLimits::default()).unwrap();

            assert!(set.diagnostics.is_empty(), "{:?}", set.diagnostics);
            assert_eq!(set.packages.len(), 1);
            let package = &set.packages[0];
            assert_eq!(package.eye_blink_parameters, [expected_eye]);
            assert_eq!(package.lip_sync_parameters, [expected_mouth]);
            let mut expected_parameters = [expected_eye, expected_mouth, "ParamAngleX"];
            expected_parameters.sort_unstable();
            assert_eq!(package.motion_targets.parameters, expected_parameters);

            let mut manifest = Vec::new();
            package.write_model3_json(&mut manifest, u64::MAX).unwrap();
            let manifest: serde_json::Value = serde_json::from_slice(&manifest).unwrap();
            assert_eq!(manifest["Groups"][0]["Target"], "Parameter");
            assert_eq!(manifest["Groups"][0]["Name"], "EyeBlink");
            assert_eq!(manifest["Groups"][0]["Ids"][0], expected_eye);
            assert_eq!(manifest["Groups"][1]["Target"], "Parameter");
            assert_eq!(manifest["Groups"][1]["Name"], "LipSync");
            assert_eq!(manifest["Groups"][1]["Ids"][0], expected_mouth);
        }
    }

    #[test]
    fn follows_expression_controller_list_and_materializes_exp3_json() {
        let collection = expression_package_collection();
        let set = build_live2d_packages(&collection, Live2dPackageLimits::default()).unwrap();

        assert!(set.diagnostics.is_empty(), "{:?}", set.diagnostics);
        assert_eq!(set.packages.len(), 1);
        let package = &set.packages[0];
        assert_eq!(package.expressions.len(), 1);
        assert_eq!(package.motions.len(), 1);
        assert_eq!(package.motions[0].name, "idle.fade");
        assert_eq!(
            package.motions[0].file_name,
            "motions/idle.fade.motion3.json"
        );
        assert_eq!(package.expressions[0].object.path_id, 24);
        assert_eq!(package.expressions[0].name, "smile");
        assert_eq!(
            package.expressions[0].file_name,
            "expressions/smile.exp3.json"
        );
        assert_eq!(package.pose.as_ref().unwrap().file_name, "Hero.pose3.json");
        assert_eq!(
            package.physics.as_ref().unwrap().file_name,
            "Hero.physics3.json"
        );
        assert_eq!(
            package.display_info.as_ref().unwrap().file_name,
            "Hero.cdi3.json"
        );
        let mut manifest = Vec::new();
        package.write_model3_json(&mut manifest, u64::MAX).unwrap();
        let manifest: serde_json::Value = serde_json::from_slice(&manifest).unwrap();
        assert_eq!(
            manifest["FileReferences"]["Expressions"][0]["Name"],
            "smile"
        );
        assert_eq!(
            manifest["FileReferences"]["Expressions"][0]["File"],
            "expressions/smile.exp3.json"
        );
        assert_eq!(manifest["FileReferences"]["Pose"], "Hero.pose3.json");
        assert_eq!(manifest["FileReferences"]["Physics"], "Hero.physics3.json");
        assert_eq!(manifest["FileReferences"]["DisplayInfo"], "Hero.cdi3.json");
        assert_eq!(
            manifest["FileReferences"]["Motions"]["idle.fade"][0]["File"],
            "motions/idle.fade.motion3.json"
        );

        let set = build_live2d_packages(&collection, Live2dPackageLimits::default()).unwrap();
        let materialized =
            materialize_live2d_packages(set, Live2dPackageMaterializeLimits::default()).unwrap();
        assert_eq!(materialized.packages[0].expressions.len(), 1);
        assert_eq!(materialized.packages[0].motions.len(), 1);
        let motion_json: serde_json::Value =
            serde_json::from_slice(&materialized.packages[0].motions[0].json).unwrap();
        assert_eq!(motion_json["Meta"]["CurveCount"], 1);
        assert_eq!(motion_json["Curves"][0]["Id"], "ParamAngleX");
        let expression = &materialized.packages[0].expressions[0];
        assert_eq!(expression.name, "smile");
        assert_eq!(expression.file_name, "expressions/smile.exp3.json");
        let json: serde_json::Value = serde_json::from_slice(&expression.json).unwrap();
        assert_eq!(json["Parameters"][0]["Id"], "ParamAngleX");
        assert_eq!(json["Parameters"][0]["Blend"], 1);
        let pose = materialized.packages[0].pose.as_ref().unwrap();
        let pose_json: serde_json::Value = serde_json::from_slice(&pose.bytes).unwrap();
        assert_eq!(pose_json["Groups"][0][0]["Id"], "PartBody");
        assert_eq!(pose_json["Groups"][0][0]["Link"][0], "PartArm");
        let physics = materialized.packages[0].physics.as_ref().unwrap();
        let physics_json: serde_json::Value = serde_json::from_slice(&physics.bytes).unwrap();
        assert_eq!(physics_json["Meta"]["Fps"], 30.0);
        assert_eq!(physics_json["Meta"]["PhysicsSettingCount"], 1);
        assert_eq!(
            physics_json["PhysicsSettings"][0]["Output"][0]["Destination"]["Id"],
            "ParamHair"
        );
        let cdi = materialized.packages[0].display_info.as_ref().unwrap();
        let cdi_json: serde_json::Value = serde_json::from_slice(&cdi.bytes).unwrap();
        assert_eq!(cdi_json["Parameters"][0]["Id"], "PartBody");
        assert_eq!(cdi_json["Parameters"][0]["Name"], "Face Angle");
        assert_eq!(cdi_json["Parts"][0]["Name"], "Body");

        let limits = Live2dPackageLimits {
            maximum_total_expression_bytes: 0,
            ..Live2dPackageLimits::default()
        };
        assert!(build_live2d_packages(&collection, limits).is_err());
        let limits = Live2dPackageLimits {
            maximum_total_motion_bytes: 0,
            ..Live2dPackageLimits::default()
        };
        assert!(build_live2d_packages(&collection, limits).is_err());
        let limits = Live2dPackageLimits {
            maximum_total_auxiliary_bytes: 0,
            ..Live2dPackageLimits::default()
        };
        assert!(build_live2d_packages(&collection, limits).is_err());
        let limits = Live2dPackageLimits {
            maximum_total_physics_bytes: 0,
            ..Live2dPackageLimits::default()
        };
        assert!(build_live2d_packages(&collection, limits).is_err());
    }

    #[test]
    fn external_schemas_project_expression_motion_physics_pose_and_display_info() {
        let mut collection = expression_package_collection();
        for kind in &mut collection.serialized_files[0].file.types {
            if kind.class_id == MONO_BEHAVIOUR {
                kind.type_tree = None;
            }
        }
        let schemas = expression_schema_registry();

        let set = build_live2d_packages_with_schema_provider(
            &collection,
            Live2dPackageLimits::default(),
            &schemas,
        )
        .unwrap();

        assert!(set.diagnostics.is_empty(), "{:?}", set.diagnostics);
        assert_eq!(set.packages.len(), 1);
        let package = &set.packages[0];
        assert_eq!(package.expressions.len(), 1);
        assert_eq!(package.motions.len(), 1);
        assert!(package.physics.is_some());
        assert!(package.pose.is_some());
        assert!(package.display_info.is_some());
    }

    #[test]
    fn sanitizes_and_disambiguates_names_deterministically() {
        let mut claimed = HashSet::new();
        let first = super::SceneObjectKey {
            file_index: 0,
            path_id: 7,
        };
        let second = super::SceneObjectKey {
            file_index: 1,
            path_id: 9,
        };
        assert_eq!(safe_file_stem("../bad:name").unwrap(), "_bad_name");
        assert_eq!(claim_name("Face", first, &mut claimed).unwrap(), "Face");
        assert_eq!(
            claim_name("face", second, &mut claimed).unwrap(),
            "face @f1_p9"
        );
        assert_eq!(
            claim_name("face", second, &mut claimed).unwrap(),
            "face @f1_p9_2"
        );
    }

    fn synthetic_collection(model_tree: bool, renderer_tree: bool) -> AssetCollection {
        synthetic_collection_options(model_tree, renderer_tree, false)
    }

    fn animator(controller: i64) -> Vec<u8> {
        let mut output = Vec::new();
        push_pptr(&mut output, (0, 1));
        output.push(1);
        align(&mut output, 4);
        push_pptr(&mut output, (0, 0));
        push_pptr(&mut output, (0, controller));
        output
    }

    fn animator_controller(name: &str, clip: i64) -> Vec<u8> {
        let mut output = aligned_string(name);
        output.extend_from_slice(&0_u32.to_le_bytes());
        for _ in 0..10 {
            push_i32(&mut output, 0);
        }
        push_i32(&mut output, 1);
        push_pptr(&mut output, (0, clip));
        output
    }

    fn aligned_string(value: &str) -> Vec<u8> {
        let mut output = Vec::new();
        push_aligned_string(&mut output, value);
        output
    }

    fn synthetic_collection_options(
        model_tree: bool,
        renderer_tree: bool,
        duplicate_moc_path: bool,
    ) -> AssetCollection {
        let model_nodes = model_tree.then(|| mono_behaviour_tree("CubismMoc", "_moc"));
        let renderer_nodes =
            renderer_tree.then(|| mono_behaviour_tree("Texture2D", "_mainTexture"));
        let primary_types = vec![
            TestType::plain(GAME_OBJECT),
            TestType::plain(TRANSFORM),
            TestType::behaviour(0x20, model_nodes),
            TestType::behaviour(0x21, renderer_nodes),
            TestType::behaviour(0x30, None),
            TestType::plain(MONO_SCRIPT),
        ];
        let mut primary_objects = vec![
            TestObject::new(1, 0, game_object("Hero", &[(0, 10), (0, 20)])),
            TestObject::new(10, 1, transform((0, 1), &[(0, 11)], (0, 0))),
            TestObject::new(20, 2, mono_behaviour((0, 1), (0, 100), "", (0, 30))),
            TestObject::new(2, 0, game_object("Drawables", &[(0, 11), (0, 21)])),
            TestObject::new(11, 1, transform((0, 2), &[], (0, 10))),
            TestObject::new(21, 3, mono_behaviour((0, 2), (0, 101), "", (1, 40))),
            TestObject::new(30, 4, cubism_moc_behaviour((0, 1), (0, 102))),
            TestObject::new(100, 5, mono_script("CubismModel")),
            TestObject::new(101, 5, mono_script("CubismRenderer")),
            TestObject::new(102, 5, mono_script("CubismMoc")),
        ];
        if duplicate_moc_path {
            primary_objects.insert(
                6,
                TestObject::new(
                    31,
                    3,
                    mono_behaviour((0, 2), (0, 101), "duplicate renderer", (1, 40)),
                ),
            );
        }
        let texture_types = vec![TestType::plain(TEXTURE_2D)];
        let texture_objects = vec![TestObject::new(40, 0, texture_object("face"))];
        let mut primary = SerializedFile::open(Region::from_bytes(synthetic_v22(
            &primary_types,
            &primary_objects,
            &["archive:/textures.assets"],
        )))
        .unwrap();
        if duplicate_moc_path {
            // `SerializedFile::open` rejects duplicates. Mutate the parsed
            // fixture to exercise collections assembled by low-level callers
            // and keep role authentication tied to the resolved table entry.
            primary.objects[6].path_id = 30;
        }
        let textures = SerializedFile::open(Region::from_bytes(synthetic_v22(
            &texture_types,
            &texture_objects,
            &[],
        )))
        .unwrap();
        AssetCollection::from_loaded_parts(
            vec![
                LoadedSerializedFile {
                    path: "bundle::model.assets".to_owned(),
                    file: primary,
                },
                LoadedSerializedFile {
                    path: "bundle::folder/textures.assets".to_owned(),
                    file: textures,
                },
            ],
            Vec::new(),
        )
    }

    fn active_model_collection(components: &[(i32, i64)]) -> AssetCollection {
        let types = vec![
            TestType::plain(GAME_OBJECT),
            TestType::plain(TRANSFORM),
            TestType::behaviour(0x20, Some(mono_behaviour_tree("CubismMoc", "_moc"))),
            TestType::behaviour(0x30, None),
            TestType::plain(MONO_SCRIPT),
        ];
        let objects = vec![
            TestObject::new(1, 0, game_object("Owner", components)),
            TestObject::new(10, 1, transform((0, 1), &[], (0, 0))),
            TestObject::new(90, 2, mono_behaviour((0, 1), (0, 100), "first", (0, 30))),
            TestObject::new(20, 2, mono_behaviour((0, 1), (0, 100), "last", (0, 30))),
            TestObject::new(30, 3, cubism_moc_behaviour((0, 1), (0, 102))),
            TestObject::new(100, 4, mono_script("CubismModel")),
            TestObject::new(102, 4, mono_script("CubismMoc")),
        ];
        let file =
            SerializedFile::open(Region::from_bytes(synthetic_v22(&types, &objects, &[]))).unwrap();
        AssetCollection::from_loaded_parts(
            vec![LoadedSerializedFile {
                path: "bundle::active.assets".to_owned(),
                file,
            }],
            Vec::new(),
        )
    }

    fn shared_moc_collection() -> AssetCollection {
        let types = vec![
            TestType::plain(GAME_OBJECT),
            TestType::plain(TRANSFORM),
            TestType::behaviour(0x20, Some(mono_behaviour_tree("CubismMoc", "_moc"))),
            TestType::behaviour(0x30, None),
            TestType::plain(MONO_SCRIPT),
        ];
        let objects = vec![
            TestObject::new(1, 0, game_object("First", &[(0, 10), (0, 90)])),
            TestObject::new(10, 1, transform((0, 1), &[], (0, 0))),
            TestObject::new(90, 2, mono_behaviour((0, 1), (0, 100), "", (0, 30))),
            TestObject::new(2, 0, game_object("Second", &[(0, 11), (0, 20)])),
            TestObject::new(11, 1, transform((0, 2), &[], (0, 0))),
            TestObject::new(20, 2, mono_behaviour((0, 2), (0, 100), "", (0, 30))),
            TestObject::new(30, 3, cubism_moc_behaviour((0, 1), (0, 102))),
            TestObject::new(100, 4, mono_script("CubismModel")),
            TestObject::new(102, 4, mono_script("CubismMoc")),
        ];
        let file =
            SerializedFile::open(Region::from_bytes(synthetic_v22(&types, &objects, &[]))).unwrap();
        AssetCollection::from_loaded_parts(
            vec![LoadedSerializedFile {
                path: "bundle::shared.assets".to_owned(),
                file,
            }],
            Vec::new(),
        )
    }

    fn parameter_group_collection(explicit_markers: bool) -> AssetCollection {
        let types = vec![
            TestType::plain(GAME_OBJECT),
            TestType::plain(TRANSFORM),
            TestType::behaviour(0x20, Some(mono_behaviour_tree("CubismMoc", "_moc"))),
            TestType::behaviour(0x30, None),
            TestType::behaviour(0x31, None),
            TestType::behaviour(0x32, None),
            TestType::behaviour(0x33, None),
            TestType::plain(MONO_SCRIPT),
        ];
        let (eye_name, mouth_name) = if explicit_markers {
            ("BlinkLeft", "MouthValue")
        } else {
            ("ParamEyeLOpen", "ParamMouthOpenY")
        };
        let eye_components: &[(i32, i64)] = if explicit_markers {
            &[(0, 11), (0, 40), (0, 50)]
        } else {
            &[(0, 11), (0, 40)]
        };
        let mouth_components: &[(i32, i64)] = if explicit_markers {
            &[(0, 12), (0, 41), (0, 51)]
        } else {
            &[(0, 12), (0, 41)]
        };
        let mut objects = vec![
            TestObject::new(1, 0, game_object("Hero", &[(0, 10), (0, 20)])),
            TestObject::new(
                10,
                1,
                transform((0, 1), &[(0, 11), (0, 12), (0, 13)], (0, 0)),
            ),
            TestObject::new(20, 2, mono_behaviour((0, 1), (0, 103), "", (0, 30))),
            TestObject::new(30, 3, cubism_moc_behaviour((0, 1), (0, 104))),
            TestObject::new(2, 0, game_object(eye_name, eye_components)),
            TestObject::new(11, 1, transform((0, 2), &[], (0, 10))),
            TestObject::new(40, 4, mono_behaviour_prefix((0, 2), (0, 100), "")),
            TestObject::new(3, 0, game_object(mouth_name, mouth_components)),
            TestObject::new(12, 1, transform((0, 3), &[], (0, 10))),
            TestObject::new(41, 4, mono_behaviour_prefix((0, 3), (0, 100), "")),
            TestObject::new(4, 0, game_object("ParamAngleX", &[(0, 13), (0, 42)])),
            TestObject::new(13, 1, transform((0, 4), &[], (0, 10))),
            TestObject::new(42, 4, mono_behaviour_prefix((0, 4), (0, 100), "")),
            TestObject::new(100, 7, mono_script("CubismParameter")),
            TestObject::new(101, 7, mono_script("CubismEyeBlinkParameter")),
            TestObject::new(102, 7, mono_script("CubismMouthParameter")),
            TestObject::new(103, 7, mono_script("CubismModel")),
            TestObject::new(104, 7, mono_script("CubismMoc")),
        ];
        if explicit_markers {
            objects.push(TestObject::new(
                50,
                5,
                mono_behaviour_prefix((0, 2), (0, 101), ""),
            ));
            objects.push(TestObject::new(
                51,
                6,
                mono_behaviour_prefix((0, 3), (0, 102), ""),
            ));
        }
        let file =
            SerializedFile::open(Region::from_bytes(synthetic_v22(&types, &objects, &[]))).unwrap();
        AssetCollection::from_loaded_parts(
            vec![LoadedSerializedFile {
                path: "bundle::parameters.assets".to_owned(),
                file,
            }],
            Vec::new(),
        )
    }

    fn expression_package_collection() -> AssetCollection {
        let types = vec![
            TestType::plain(GAME_OBJECT),
            TestType::plain(TRANSFORM),
            TestType::behaviour(0x20, Some(mono_behaviour_tree("CubismMoc", "_moc"))),
            TestType::behaviour(0x30, None),
            TestType::behaviour(
                0x22,
                Some(mono_behaviour_tree(
                    "CubismExpressionList",
                    "ExpressionsList",
                )),
            ),
            TestType::behaviour(0x23, Some(expression_list_tree())),
            TestType::behaviour(0x24, Some(expression_data_tree())),
            TestType::behaviour(0x25, Some(pose_part_tree())),
            TestType::behaviour(0x26, Some(display_info_tree())),
            TestType::behaviour(0x27, Some(physics_tree())),
            TestType::behaviour(0x28, Some(fade_controller_tree())),
            TestType::behaviour(0x29, Some(fade_list_tree())),
            TestType::behaviour(0x2A, Some(fade_motion_tree())),
            TestType::plain(MONO_SCRIPT),
        ];
        let objects = vec![
            TestObject::new(
                1,
                0,
                game_object("Hero", &[(0, 10), (0, 20), (0, 22), (0, 28), (0, 29)]),
            ),
            TestObject::new(10, 1, transform((0, 1), &[(0, 11)], (0, 0))),
            TestObject::new(
                2,
                0,
                game_object("PartBody", &[(0, 11), (0, 25), (0, 26), (0, 27)]),
            ),
            TestObject::new(11, 1, transform((0, 2), &[], (0, 10))),
            TestObject::new(20, 2, mono_behaviour((0, 1), (0, 100), "", (0, 30))),
            TestObject::new(22, 4, mono_behaviour((0, 1), (0, 103), "", (0, 23))),
            TestObject::new(23, 5, expression_list_behaviour()),
            TestObject::new(24, 6, expression_data_behaviour()),
            TestObject::new(25, 7, pose_part_behaviour()),
            TestObject::new(
                26,
                8,
                display_info_behaviour((0, 107), "Angle X", "Face Angle"),
            ),
            TestObject::new(27, 8, display_info_behaviour((0, 108), "Body", "")),
            TestObject::new(28, 9, physics_behaviour()),
            TestObject::new(29, 10, fade_controller_behaviour()),
            TestObject::new(31, 11, fade_list_behaviour()),
            TestObject::new(32, 12, fade_motion_behaviour()),
            TestObject::new(30, 3, cubism_moc_behaviour((0, 1), (0, 102))),
            TestObject::new(100, 13, mono_script("CubismModel")),
            TestObject::new(102, 13, mono_script("CubismMoc")),
            TestObject::new(103, 13, mono_script("CubismExpressionController")),
            TestObject::new(104, 13, mono_script("CubismExpressionList")),
            TestObject::new(105, 13, mono_script("CubismExpressionData")),
            TestObject::new(106, 13, mono_script("CubismPosePart")),
            TestObject::new(107, 13, mono_script("CubismDisplayInfoParameterName")),
            TestObject::new(108, 13, mono_script("CubismDisplayInfoPartName")),
            TestObject::new(109, 13, mono_script("CubismPhysicsController")),
            TestObject::new(110, 13, mono_script("CubismFadeController")),
            TestObject::new(111, 13, mono_script("CubismFadeMotionList")),
            TestObject::new(112, 13, mono_script("CubismFadeMotionData")),
        ];
        let file =
            SerializedFile::open(Region::from_bytes(synthetic_v22(&types, &objects, &[]))).unwrap();
        AssetCollection::from_loaded_parts(
            vec![LoadedSerializedFile {
                path: "bundle::expression.assets".to_owned(),
                file,
            }],
            Vec::new(),
        )
    }

    #[derive(Clone)]
    struct TestType {
        class_id: i32,
        script_hash: Option<u8>,
        nodes: Option<Vec<TestNode>>,
    }

    impl TestType {
        fn plain(class_id: i32) -> Self {
            Self {
                class_id,
                script_hash: None,
                nodes: None,
            }
        }

        fn behaviour(script_hash: u8, nodes: Option<Vec<TestNode>>) -> Self {
            Self {
                class_id: MONO_BEHAVIOUR,
                script_hash: Some(script_hash),
                nodes,
            }
        }
    }

    struct TestObject {
        path_id: i64,
        type_index: usize,
        payload: Vec<u8>,
    }

    impl TestObject {
        fn new(path_id: i64, type_index: usize, payload: Vec<u8>) -> Self {
            Self {
                path_id,
                type_index,
                payload,
            }
        }
    }

    #[derive(Clone, Copy)]
    struct TestNode {
        type_name: &'static str,
        field_name: &'static str,
        level: u8,
        align: bool,
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

    fn mono_behaviour_tree(
        pointer_type: &'static str,
        pointer_name: &'static str,
    ) -> Vec<TestNode> {
        let mut nodes = mono_behaviour_base_tree();
        nodes.extend_from_slice(&[
            node(pointer_type, pointer_name, 1, false),
            node("int", "m_FileID", 2, false),
            node("SInt64", "m_PathID", 2, false),
        ]);
        nodes
    }

    fn mono_behaviour_base_tree() -> Vec<TestNode> {
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
        ]
    }

    fn test_schema_entry(class_name: &str, nodes: &[TestNode]) -> MonoBehaviourSchemaEntry {
        let mut tree_nodes = Vec::with_capacity(nodes.len());
        for (index, node) in nodes.iter().enumerate() {
            tree_nodes.push(TypeTreeNode {
                type_name: node.type_name.to_owned(),
                field_name: node.field_name.to_owned(),
                byte_size: -1,
                index: i32::try_from(index).unwrap(),
                type_flags: 0,
                version: 1,
                meta_flags: if node.align { 0x4000 } else { 0 },
                level: u32::from(node.level),
                type_string_offset: None,
                name_string_offset: None,
                reference_type_hash: 0,
            });
        }
        MonoBehaviourSchemaEntry {
            assembly_name: "Live2D.Cubism.dll".to_owned(),
            namespace: "Live2D.Cubism.Core".to_owned(),
            class_name: class_name.to_owned(),
            unity_version: None,
            tree: TypeTree {
                nodes: tree_nodes,
                string_buffer: Vec::new(),
            },
        }
    }

    fn expression_schema_registry() -> MonoBehaviourSchemaRegistry {
        let mut schemas = MonoBehaviourSchemaRegistry::new();
        let entries = [
            ("CubismModel", mono_behaviour_tree("CubismMoc", "_moc")),
            (
                "CubismExpressionController",
                mono_behaviour_tree("CubismExpressionList", "ExpressionsList"),
            ),
            ("CubismExpressionList", expression_list_tree()),
            ("CubismExpressionData", expression_data_tree()),
            ("CubismPosePart", pose_part_tree()),
            ("CubismDisplayInfoParameterName", display_info_tree()),
            ("CubismDisplayInfoPartName", display_info_tree()),
            ("CubismPhysicsController", physics_tree()),
            ("CubismFadeController", fade_controller_tree()),
            ("CubismFadeMotionList", fade_list_tree()),
            ("CubismFadeMotionData", fade_motion_tree()),
        ];
        for (class_name, nodes) in entries {
            schemas.push(test_schema_entry(class_name, &nodes)).unwrap();
        }
        schemas
    }

    fn expression_list_tree() -> Vec<TestNode> {
        let mut nodes = mono_behaviour_base_tree();
        nodes.extend_from_slice(&[
            node("vector", "CubismExpressionObjects", 1, false),
            node("Array", "Array", 2, true),
            node("int", "size", 3, false),
            node("PPtr<CubismExpressionData>", "data", 3, false),
            node("int", "m_FileID", 4, false),
            node("SInt64", "m_PathID", 4, false),
        ]);
        nodes
    }

    fn expression_data_tree() -> Vec<TestNode> {
        let mut nodes = mono_behaviour_base_tree();
        nodes.extend_from_slice(&[
            node("string", "Type", 1, false),
            node("Array", "Array", 2, true),
            node("int", "size", 3, false),
            node("char", "data", 3, false),
            node("float", "FadeInTime", 1, false),
            node("float", "FadeOutTime", 1, false),
            node("vector", "Parameters", 1, false),
            node("Array", "Array", 2, true),
            node("int", "size", 3, false),
            node("SerializableExpressionParameter", "data", 3, false),
            node("string", "Id", 4, false),
            node("Array", "Array", 5, true),
            node("int", "size", 6, false),
            node("char", "data", 6, false),
            node("float", "Value", 4, false),
            node("int", "Blend", 4, false),
        ]);
        nodes
    }

    fn pose_part_tree() -> Vec<TestNode> {
        let mut nodes = mono_behaviour_base_tree();
        nodes.extend_from_slice(&[
            node("int", "GroupIndex", 1, false),
            node("vector", "Link", 1, false),
            node("Array", "Array", 2, true),
            node("int", "size", 3, false),
            node("string", "data", 3, false),
            node("Array", "Array", 4, true),
            node("int", "size", 5, false),
            node("char", "data", 5, false),
        ]);
        nodes
    }

    fn display_info_tree() -> Vec<TestNode> {
        let mut nodes = mono_behaviour_base_tree();
        nodes.extend_from_slice(&[
            node("string", "Name", 1, false),
            node("Array", "Array", 2, true),
            node("int", "size", 3, false),
            node("char", "data", 3, false),
            node("string", "DisplayName", 1, false),
            node("Array", "Array", 2, true),
            node("int", "size", 3, false),
            node("char", "data", 3, false),
        ]);
        nodes
    }

    fn physics_tree() -> Vec<TestNode> {
        let mut nodes = mono_behaviour_base_tree();
        nodes.extend_from_slice(&[
            node("CubismPhysicsRig", "_rig", 1, false),
            node("vector", "SubRigs", 2, false),
            node("Array", "Array", 3, true),
            node("int", "size", 4, false),
            node("CubismPhysicsSubRig", "data", 4, false),
            node("vector", "Input", 5, false),
            node("Array", "Array", 6, true),
            node("int", "size", 7, false),
            node("CubismPhysicsInput", "data", 7, false),
            node("string", "SourceId", 8, false),
            node("Array", "Array", 9, true),
            node("int", "size", 10, false),
            node("char", "data", 10, false),
            node("float", "Weight", 8, false),
            node("int", "SourceComponent", 8, false),
            node("bool", "IsInverted", 8, true),
            node("vector", "Output", 5, false),
            node("Array", "Array", 6, true),
            node("int", "size", 7, false),
            node("CubismPhysicsOutput", "data", 7, false),
            node("string", "DestinationId", 8, false),
            node("Array", "Array", 9, true),
            node("int", "size", 10, false),
            node("char", "data", 10, false),
            node("int", "ParticleIndex", 8, false),
            node("float", "AngleScale", 8, false),
            node("float", "Weight", 8, false),
            node("int", "SourceComponent", 8, false),
            node("bool", "IsInverted", 8, true),
            node("vector", "Particles", 5, false),
            node("Array", "Array", 6, true),
            node("int", "size", 7, false),
            node("CubismPhysicsParticle", "data", 7, false),
            node("Vector2", "InitialPosition", 8, false),
            node("float", "X", 9, false),
            node("float", "Y", 9, false),
            node("float", "Mobility", 8, false),
            node("float", "Delay", 8, false),
            node("float", "Acceleration", 8, false),
            node("float", "Radius", 8, false),
            node("CubismPhysicsNormalization", "Normalization", 5, false),
            node("CubismPhysicsNormalizationTuplet", "Position", 6, false),
            node("float", "Maximum", 7, false),
            node("float", "Minimum", 7, false),
            node("float", "Default", 7, false),
            node("CubismPhysicsNormalizationTuplet", "Angle", 6, false),
            node("float", "Maximum", 7, false),
            node("float", "Minimum", 7, false),
            node("float", "Default", 7, false),
            node("Vector2", "Gravity", 2, false),
            node("float", "X", 3, false),
            node("float", "Y", 3, false),
            node("Vector2", "Wind", 2, false),
            node("float", "X", 3, false),
            node("float", "Y", 3, false),
            node("float", "Fps", 2, false),
        ]);
        nodes
    }

    fn fade_controller_tree() -> Vec<TestNode> {
        mono_behaviour_tree("CubismFadeMotionList", "CubismFadeMotionList")
    }

    fn fade_list_tree() -> Vec<TestNode> {
        let mut nodes = mono_behaviour_base_tree();
        nodes.extend_from_slice(&[
            node("vector", "MotionInstanceIds", 1, false),
            node("Array", "Array", 2, false),
            node("int", "size", 3, false),
            node("int", "data", 3, false),
            node("vector", "CubismFadeMotionObjects", 1, false),
            node("Array", "Array", 2, false),
            node("int", "size", 3, false),
            node("PPtr<CubismFadeMotionData>", "data", 3, false),
            node("int", "m_FileID", 4, false),
            node("SInt64", "m_PathID", 4, false),
        ]);
        nodes
    }

    fn fade_motion_tree() -> Vec<TestNode> {
        let mut nodes = mono_behaviour_base_tree();
        nodes.extend_from_slice(&[
            node("string", "MotionName", 1, false),
            node("Array", "Array", 2, true),
            node("int", "size", 3, false),
            node("char", "data", 3, false),
            node("float", "FadeInTime", 1, false),
            node("float", "FadeOutTime", 1, false),
            node("vector", "ParameterIds", 1, false),
            node("Array", "Array", 2, false),
            node("int", "size", 3, false),
            node("string", "data", 3, false),
            node("Array", "Array", 4, true),
            node("int", "size", 5, false),
            node("char", "data", 5, false),
            node("vector", "ParameterCurves", 1, false),
            node("Array", "Array", 2, false),
            node("int", "size", 3, false),
            node("AnimationCurve", "data", 3, false),
            node("vector", "m_Curve", 4, false),
            node("Array", "Array", 5, false),
            node("int", "size", 6, false),
            node("Keyframe", "data", 6, false),
            node("float", "time", 7, false),
            node("float", "value", 7, false),
            node("float", "inSlope", 7, false),
            node("float", "outSlope", 7, false),
            node("int", "weightedMode", 7, false),
            node("float", "inWeight", 7, false),
            node("float", "outWeight", 7, false),
            node("int", "m_PreInfinity", 4, false),
            node("int", "m_PostInfinity", 4, false),
            node("int", "m_RotationOrder", 4, false),
            node("vector", "ParameterFadeInTimes", 1, false),
            node("Array", "Array", 2, false),
            node("int", "size", 3, false),
            node("float", "data", 3, false),
            node("vector", "ParameterFadeOutTimes", 1, false),
            node("Array", "Array", 2, false),
            node("int", "size", 3, false),
            node("float", "data", 3, false),
            node("float", "MotionLength", 1, false),
        ]);
        nodes
    }

    fn game_object(name: &str, components: &[(i32, i64)]) -> Vec<u8> {
        let mut output = Vec::new();
        push_i32(&mut output, i32::try_from(components.len()).unwrap());
        for reference in components {
            push_pptr(&mut output, *reference);
        }
        push_i32(&mut output, 0);
        push_aligned_string(&mut output, name);
        output
    }

    fn transform(game_object: (i32, i64), children: &[(i32, i64)], father: (i32, i64)) -> Vec<u8> {
        let mut output = Vec::new();
        push_pptr(&mut output, game_object);
        for value in [0.0_f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0] {
            output.extend_from_slice(&value.to_le_bytes());
        }
        push_i32(&mut output, i32::try_from(children.len()).unwrap());
        for reference in children {
            push_pptr(&mut output, *reference);
        }
        push_pptr(&mut output, father);
        output
    }

    fn mono_behaviour(
        game_object: (i32, i64),
        script: (i32, i64),
        name: &str,
        field: (i32, i64),
    ) -> Vec<u8> {
        let mut output = mono_behaviour_prefix(game_object, script, name);
        push_pptr(&mut output, field);
        output
    }

    fn cubism_moc_behaviour(game_object: (i32, i64), script: (i32, i64)) -> Vec<u8> {
        let mut output = mono_behaviour_prefix(game_object, script, "moc");
        push_i32(&mut output, 5);
        output.extend_from_slice(b"MOC3\x09");
        output
    }

    fn expression_list_behaviour() -> Vec<u8> {
        let mut output = mono_behaviour_prefix((0, 1), (0, 104), "expression list");
        push_i32(&mut output, 1);
        push_pptr(&mut output, (0, 24));
        align(&mut output, 4);
        output
    }

    fn expression_data_behaviour() -> Vec<u8> {
        let mut output = mono_behaviour_prefix((0, 1), (0, 105), "smile.exp3");
        push_aligned_string(&mut output, "Live2D Expression");
        output.extend_from_slice(&0.5_f32.to_le_bytes());
        output.extend_from_slice(&0.75_f32.to_le_bytes());
        push_i32(&mut output, 1);
        push_aligned_string(&mut output, "ParamAngleX");
        output.extend_from_slice(&0.25_f32.to_le_bytes());
        push_i32(&mut output, 1);
        align(&mut output, 4);
        output
    }

    fn pose_part_behaviour() -> Vec<u8> {
        let mut output = mono_behaviour_prefix((0, 2), (0, 106), "pose part");
        push_i32(&mut output, 1);
        push_i32(&mut output, 1);
        push_aligned_string(&mut output, "PartArm");
        align(&mut output, 4);
        output
    }

    fn display_info_behaviour(script: (i32, i64), name: &str, display_name: &str) -> Vec<u8> {
        let mut output = mono_behaviour_prefix((0, 2), script, "display info");
        push_aligned_string(&mut output, name);
        push_aligned_string(&mut output, display_name);
        output
    }

    fn physics_behaviour() -> Vec<u8> {
        let mut output = mono_behaviour_prefix((0, 1), (0, 109), "physics");
        push_i32(&mut output, 1);

        push_i32(&mut output, 1);
        push_aligned_string(&mut output, "ParamAngleX");
        output.extend_from_slice(&80.0_f32.to_le_bytes());
        push_i32(&mut output, 0);
        output.push(0);
        align(&mut output, 4);

        push_i32(&mut output, 1);
        push_aligned_string(&mut output, "ParamHair");
        push_i32(&mut output, 1);
        output.extend_from_slice(&2.5_f32.to_le_bytes());
        output.extend_from_slice(&90.0_f32.to_le_bytes());
        push_i32(&mut output, 2);
        output.push(1);
        align(&mut output, 4);

        push_i32(&mut output, 1);
        for value in [0.0_f32, 1.0, 0.8, 0.2, 1.0, 10.0] {
            output.extend_from_slice(&value.to_le_bytes());
        }
        for value in [10.0_f32, -10.0, 0.0, 30.0, -30.0, 0.0] {
            output.extend_from_slice(&value.to_le_bytes());
        }
        for value in [0.0_f32, -1.0, 0.5, 0.0, 0.0] {
            output.extend_from_slice(&value.to_le_bytes());
        }
        output
    }

    fn fade_controller_behaviour() -> Vec<u8> {
        let mut output = mono_behaviour_prefix((0, 1), (0, 110), "fade controller");
        push_pptr(&mut output, (0, 31));
        output
    }

    fn fade_list_behaviour() -> Vec<u8> {
        let mut output = mono_behaviour_prefix((0, 1), (0, 111), "fade list");
        push_i32(&mut output, 1);
        push_i32(&mut output, 32);
        push_i32(&mut output, 1);
        push_pptr(&mut output, (0, 32));
        output
    }

    fn fade_motion_behaviour() -> Vec<u8> {
        let mut output = mono_behaviour_prefix((0, 1), (0, 112), "idle.fade.asset");
        push_aligned_string(&mut output, "idle");
        output.extend_from_slice(&0.2_f32.to_le_bytes());
        output.extend_from_slice(&0.3_f32.to_le_bytes());
        push_i32(&mut output, 1);
        push_aligned_string(&mut output, "ParamAngleX");
        push_i32(&mut output, 1);
        push_i32(&mut output, 2);
        for values in [[0.0_f32, 0.0, 0.0, 0.0], [1.0_f32, 1.0, 0.0, 0.0]] {
            for value in values {
                output.extend_from_slice(&value.to_le_bytes());
            }
            push_i32(&mut output, 0);
            output.extend_from_slice(&0.0_f32.to_le_bytes());
            output.extend_from_slice(&0.0_f32.to_le_bytes());
        }
        push_i32(&mut output, 0);
        push_i32(&mut output, 0);
        push_i32(&mut output, 0);
        push_i32(&mut output, 1);
        output.extend_from_slice(&0.4_f32.to_le_bytes());
        push_i32(&mut output, 1);
        output.extend_from_slice(&0.5_f32.to_le_bytes());
        output.extend_from_slice(&1.0_f32.to_le_bytes());
        output
    }

    fn mono_behaviour_prefix(game_object: (i32, i64), script: (i32, i64), name: &str) -> Vec<u8> {
        let mut output = Vec::new();
        push_pptr(&mut output, game_object);
        output.push(1);
        align(&mut output, 4);
        push_pptr(&mut output, script);
        push_aligned_string(&mut output, name);
        output
    }

    fn mono_script(class_name: &str) -> Vec<u8> {
        let mut output = Vec::new();
        push_aligned_string(&mut output, "Cubism script");
        push_i32(&mut output, 0);
        output.extend_from_slice(&[0x55; 16]);
        push_aligned_string(&mut output, class_name);
        push_aligned_string(&mut output, "Live2D.Cubism.Core");
        push_aligned_string(&mut output, "Live2D.Cubism.dll");
        output
    }

    fn texture_object(name: &str) -> Vec<u8> {
        let mut output = Vec::new();
        push_aligned_string(&mut output, name);
        push_i32(&mut output, 0);
        output.extend_from_slice(&[0, 0]);
        align(&mut output, 4);
        push_i32(&mut output, 1);
        push_i32(&mut output, 1);
        output.extend_from_slice(&4_u32.to_le_bytes());
        push_i32(&mut output, 0);
        push_i32(&mut output, 4);
        push_i32(&mut output, 1);
        output.extend_from_slice(&[0, 0, 0]);
        align(&mut output, 4);
        push_aligned_string(&mut output, "");
        output.push(0);
        align(&mut output, 4);
        push_i32(&mut output, 0);
        push_i32(&mut output, 1);
        push_i32(&mut output, 2);
        output.extend_from_slice(&[0_u8; 24]);
        push_i32(&mut output, 0);
        push_i32(&mut output, 0);
        push_i32(&mut output, 0);
        push_i32(&mut output, 4);
        output.extend_from_slice(&[255, 128, 64, 255]);
        output
    }

    fn synthetic_v22(types: &[TestType], objects: &[TestObject], externals: &[&str]) -> Vec<u8> {
        let mut metadata = Vec::new();
        metadata.extend_from_slice(b"2022.3.62f1\0");
        push_i32(&mut metadata, 13);
        metadata.push(1);
        push_i32(&mut metadata, i32::try_from(types.len()).unwrap());
        for test_type in types {
            push_type(&mut metadata, test_type);
        }

        let mut data = Vec::new();
        let mut records = Vec::new();
        for object in objects {
            align(&mut data, 4);
            records.push((
                object.path_id,
                data.len(),
                object.payload.len(),
                object.type_index,
            ));
            data.extend_from_slice(&object.payload);
        }
        push_i32(&mut metadata, i32::try_from(records.len()).unwrap());
        for (path_id, byte_start, byte_size, type_index) in records {
            align_with_base(&mut metadata, 48, 4);
            metadata.extend_from_slice(&path_id.to_le_bytes());
            metadata.extend_from_slice(&i64::try_from(byte_start).unwrap().to_le_bytes());
            metadata.extend_from_slice(&u32::try_from(byte_size).unwrap().to_le_bytes());
            push_i32(&mut metadata, i32::try_from(type_index).unwrap());
        }
        push_i32(&mut metadata, 0);
        push_i32(&mut metadata, i32::try_from(externals.len()).unwrap());
        for external in externals {
            metadata.push(0);
            metadata.extend_from_slice(&[0_u8; 16]);
            push_i32(&mut metadata, 0);
            metadata.extend_from_slice(external.as_bytes());
            metadata.push(0);
        }
        push_i32(&mut metadata, 0);
        metadata.push(0);

        let metadata_size = u32::try_from(metadata.len()).unwrap();
        let data_offset = (48_u64 + u64::from(metadata_size)).next_multiple_of(16);
        let file_size = data_offset + u64::try_from(data.len()).unwrap();
        let mut output = vec![0_u8; 48];
        output[8..12].copy_from_slice(&22_u32.to_be_bytes());
        output[20..24].copy_from_slice(&metadata_size.to_be_bytes());
        output[24..32].copy_from_slice(&i64::try_from(file_size).unwrap().to_be_bytes());
        output[32..40].copy_from_slice(&i64::try_from(data_offset).unwrap().to_be_bytes());
        output.extend_from_slice(&metadata);
        output.resize(usize::try_from(data_offset).unwrap(), 0);
        output.extend_from_slice(&data);
        output
    }

    fn push_type(output: &mut Vec<u8>, test_type: &TestType) {
        push_i32(output, test_type.class_id);
        output.push(0);
        output.extend_from_slice(&(-1_i16).to_le_bytes());
        if let Some(script_hash) = test_type.script_hash {
            output.extend_from_slice(&[script_hash; 16]);
        }
        output.extend_from_slice(&[0x42; 16]);
        if let Some(nodes) = &test_type.nodes {
            push_blob_tree(output, nodes);
        } else {
            push_i32(output, 0);
            push_i32(output, 0);
        }
        push_i32(output, 0);
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
            push_i32(output, i32::try_from(index).unwrap());
            push_i32(output, if node.align { 0x4000 } else { 0 });
            output.extend_from_slice(&0_u64.to_le_bytes());
        }
        output.extend_from_slice(&strings);
    }

    fn push_pptr(output: &mut Vec<u8>, reference: (i32, i64)) {
        push_i32(output, reference.0);
        output.extend_from_slice(&reference.1.to_le_bytes());
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
