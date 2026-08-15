//! High-level, binding-friendly API for the native Rust implementation.
//!
//! Low-level parsers remain public for callers which need format-specific
//! control. [`Studio`] provides the stable entry point used by Rust, Python,
//! and future language bindings without routing through the legacy C ABI.

use std::io::{self, Write};
use std::path::Path;

use crate::Result;
use crate::acl::AclDecoder;
use crate::animation_clip::{AnimationClip, AnimationClipReadLimits, read_animation_clip};
use crate::animation_graph::{AnimationGraphLimits, build_animation_graph};
use crate::animator_controller::{
    AnimatorController, AnimatorControllerReadLimits, read_animator_controller,
};
use crate::avatar::{Avatar, AvatarReadLimits, read_avatar};
use crate::export::{ExportOptions, ExportReport, export_collection};
use crate::extraction::{ExtractionOptions, ExtractionReport, extract_path};
use crate::fbx_ascii::{write_model_ir_fbx_ascii, write_model_ir_fbx_ascii_with_animations};
use crate::live2d_clip_motion::{
    CubismClipMotion, CubismClipMotionReadLimits, read_cubism_clip_motion,
    read_cubism_clip_motion_with_acl_decoder,
};
use crate::live2d_motion::{CubismFadeMotion, CubismFadeMotionReadLimits, read_cubism_fade_motion};
use crate::live2d_package::{
    Live2dPackageBytesSet, Live2dPackageLimits, Live2dPackageMaterializeLimits, Live2dPackageSet,
    build_live2d_packages, build_live2d_packages_with_acl_decoder,
    build_live2d_packages_with_schema_provider,
    build_live2d_packages_with_schema_provider_and_acl_decoder, materialize_live2d_packages,
};
use crate::live2d_physics::{CubismPhysicsReadLimits, CubismPhysicsRig, read_cubism_physics};
use crate::live2d_schema::{
    CubismAuxiliaryReadLimits, CubismDisplayInfo, CubismExpression, CubismExpressionReadLimits,
    CubismPosePart, read_cubism_display_info, read_cubism_expression, read_cubism_pose_part,
};
use crate::loader::{AssetCollection, AssetLoadOptions, LoadedResource, LoadedSerializedFile};
use crate::material::{Material, MaterialReadLimits, read_material};
use crate::mesh::{MeshReadLimits, write_mesh_object_obj_with_collection};
use crate::model_animation::{ModelAnimationLimits, build_model_animations_with_acl_decoder};
use crate::model_export::{
    ModelExportCandidate, ModelExportPlanLimits, plan_animator_exports, plan_split_object_exports,
};
use crate::model_ir::{ModelIrLimits, build_model_ir, build_model_ir_for_game_object};
use crate::mono_schema::{
    MonoBehaviourSchemaProvider, ResolvedMonoBehaviourJson, read_mono_behaviour_json_with_provider,
};
use crate::monobehaviour::{MonoBehaviourReadLimits, MonoScript, read_mono_script};
use crate::project_settings::{
    BuildSettings, PlayerSettings, ProjectSettingsReadLimits, read_build_settings,
    read_player_settings,
};
use crate::scene_hierarchy::{
    SceneHierarchy, SceneHierarchyLimits, SceneObjectKey, build_scene_hierarchy,
};
use crate::serialized::ObjectInfo;
use crate::shader::{ShaderReadLimits, read_shader};
use crate::simple_assets::{
    AudioClipAsset, SimpleAssetReadLimits, SimpleBinaryAsset, read_audio_clip_asset, read_font,
    read_movie_texture, read_video_clip,
};
use crate::source::Region;
use crate::sprite::{SpriteReadLimits, decode_sprite_rgba8, read_sprite};
use crate::texture::{RgbaImage, TextureReadLimits, read_texture2d};
use crate::texture_array::{TextureArrayReadLimits, read_texture2d_array};

/// An opened collection of Unity serialized files and external resources.
#[derive(Debug)]
pub struct Studio {
    collection: AssetCollection,
}

impl Studio {
    /// Opens one asset file, bundle, web container, or directory with bounded
    /// defaults.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_options(path, AssetLoadOptions::default())
    }

    /// Opens an input with explicit loader options.
    pub fn open_with_options(path: impl AsRef<Path>, options: AssetLoadOptions) -> Result<Self> {
        AssetCollection::load_path_with_options(path.as_ref(), options).map(Self::from_collection)
    }

    /// Opens one already-bounded in-memory or custom-source region.
    pub fn open_region(path: impl Into<String>, region: Region) -> Result<Self> {
        Self::open_region_with_options(path, region, AssetLoadOptions::default())
    }

    /// Opens a source region with explicit loader options.
    pub fn open_region_with_options(
        path: impl Into<String>,
        region: Region,
        options: AssetLoadOptions,
    ) -> Result<Self> {
        AssetCollection::load_with_options(path, region, options).map(Self::from_collection)
    }

    /// Opens multiple named source regions as one collection.
    pub fn open_regions(inputs: impl IntoIterator<Item = (String, Region)>) -> Result<Self> {
        Self::open_regions_with_options(inputs, AssetLoadOptions::default())
    }

    /// Opens multiple named source regions with one shared loader budget.
    pub fn open_regions_with_options(
        inputs: impl IntoIterator<Item = (String, Region)>,
        options: AssetLoadOptions,
    ) -> Result<Self> {
        AssetCollection::load_regions_with_options(inputs, options).map(Self::from_collection)
    }

    /// Wraps an already-loaded collection.
    #[must_use]
    pub const fn from_collection(collection: AssetCollection) -> Self {
        Self { collection }
    }

    /// Returns the low-level collection for advanced format-specific work.
    #[must_use]
    pub const fn collection(&self) -> &AssetCollection {
        &self.collection
    }

    /// Consumes the high-level handle and returns its low-level collection.
    #[must_use]
    pub fn into_collection(self) -> AssetCollection {
        self.collection
    }

    /// Returns the number of discovered serialized files.
    #[must_use]
    pub fn file_count(&self) -> usize {
        self.collection.serialized_files.len()
    }

    /// Returns the total number of real serialized objects.
    #[must_use]
    pub fn object_count(&self) -> usize {
        self.collection
            .serialized_files
            .iter()
            .map(|loaded| loaded.file.objects.len())
            .sum()
    }

    /// Returns the number of discovered external resource files.
    #[must_use]
    pub fn resource_count(&self) -> usize {
        self.collection.resources.len()
    }

    /// Iterates files in deterministic collection order.
    #[must_use]
    pub fn files(&self) -> impl ExactSizeIterator<Item = StudioFile<'_>> {
        self.collection
            .serialized_files
            .iter()
            .enumerate()
            .map(|(index, loaded)| StudioFile { index, loaded })
    }

    /// Gets one discovered serialized file by collection index.
    #[must_use]
    pub fn file(&self, index: usize) -> Option<StudioFile<'_>> {
        self.collection
            .serialized_files
            .get(index)
            .map(|loaded| StudioFile { index, loaded })
    }

    /// Iterates external resources in deterministic discovery order.
    #[must_use]
    pub fn resources(&self) -> impl ExactSizeIterator<Item = StudioResource<'_>> {
        self.collection
            .resources
            .iter()
            .enumerate()
            .map(|(index, loaded)| StudioResource { index, loaded })
    }

    /// Gets one external resource by stable collection index.
    #[must_use]
    pub fn resource(&self, index: usize) -> Option<StudioResource<'_>> {
        self.collection
            .resources
            .get(index)
            .map(|loaded| StudioResource { index, loaded })
    }

    /// Resolves the first resource using `AssetStudio`'s portable,
    /// ASCII-insensitive resource-path semantics.
    #[must_use]
    pub fn resource_by_path(&self, requested_path: &str) -> Option<StudioResource<'_>> {
        let loaded = self.collection.resource(requested_path)?;
        let index = self
            .collection
            .resources
            .iter()
            .position(|candidate| std::ptr::eq(candidate, loaded))?;
        Some(StudioResource { index, loaded })
    }

    /// Iterates objects in file order and then serialized object-table order.
    pub fn objects(&self) -> impl Iterator<Item = StudioObject<'_>> {
        self.collection
            .serialized_files
            .iter()
            .enumerate()
            .flat_map(move |(file_index, loaded)| {
                loaded
                    .file
                    .objects
                    .iter()
                    .enumerate()
                    .map(move |(object_index, _)| StudioObject {
                        studio: self,
                        file_index,
                        object_index,
                    })
            })
    }

    /// Finds the first object with `path_id` in one serialized file.
    #[must_use]
    pub fn object(&self, file_index: usize, path_id: i64) -> Option<StudioObject<'_>> {
        let object_index = self
            .collection
            .object_index_by_path_id(file_index, path_id)?;
        self.object_by_index(file_index, object_index)
            .filter(|object| object.path_id() == path_id)
    }

    /// Gets an object by its real serialized table index.
    #[must_use]
    pub fn object_by_index(
        &self,
        file_index: usize,
        object_index: usize,
    ) -> Option<StudioObject<'_>> {
        self.collection
            .serialized_files
            .get(file_index)?
            .file
            .objects
            .get(object_index)?;
        Some(StudioObject {
            studio: self,
            file_index,
            object_index,
        })
    }

    /// Builds the bounded collection-wide `GameObject` hierarchy.
    pub fn scene_hierarchy(&self, limits: SceneHierarchyLimits) -> Result<SceneHierarchy> {
        build_scene_hierarchy(&self.collection, limits)
    }

    /// Exports supported objects through the bounded, atomic Core exporter.
    pub fn export(
        &self,
        output_root: impl AsRef<Path>,
        options: ExportOptions,
    ) -> Result<ExportReport> {
        export_collection(&self.collection, output_root.as_ref(), options)
    }

    /// Safely and recursively extracts one file or directory tree without
    /// requiring a loaded collection or a CLI subprocess.
    pub fn extract(
        input: impl AsRef<Path>,
        output_root: impl AsRef<Path>,
        options: ExtractionOptions,
    ) -> Result<ExtractionReport> {
        extract_path(input.as_ref(), output_root.as_ref(), options)
    }

    /// Builds the collection-wide static model and streams ASCII FBX 7.4.
    ///
    /// This supports ordinary and skinned renderer geometry, including direct
    /// bones, bone-name-hash recovery, and static blend shapes. Animation and
    /// textures are separate concerns.
    pub fn write_static_fbx(
        &self,
        output: &mut impl Write,
        maximum_output_bytes: u64,
    ) -> Result<u64> {
        let hierarchy = self.scene_hierarchy(SceneHierarchyLimits::default())?;
        let model = build_model_ir(&self.collection, &hierarchy, ModelIrLimits::default())?;
        write_model_ir_fbx_ascii(&model, output, maximum_output_bytes)
    }

    /// Materializes bounded static ASCII FBX 7.4 bytes.
    pub fn read_static_fbx(&self, maximum_output_bytes: u64) -> Result<Vec<u8>> {
        let maximum = usize::try_from(maximum_output_bytes).map_err(|_| {
            crate::Error::invalid_data("FBX output limit does not fit this platform")
        })?;
        let mut output = LimitedBuffer::new(maximum, "ASCII FBX");
        let write_result = self.write_static_fbx(&mut output, maximum_output_bytes);
        output.finish(write_result)?;
        Ok(output.bytes)
    }

    /// Builds the model plus selected animation graph and streams ASCII FBX 7.4.
    ///
    /// Explicit and packed legacy curves plus standard streamed, dense, and
    /// constant Transform and blend-shape samples are emitted.
    pub fn write_fbx(&self, output: &mut impl Write, maximum_output_bytes: u64) -> Result<u64> {
        self.write_fbx_with_acl_decoder(output, maximum_output_bytes, None)
    }

    /// Builds and writes ASCII FBX, optionally using an injected ACL decoder.
    pub fn write_fbx_with_acl_decoder(
        &self,
        output: &mut impl Write,
        maximum_output_bytes: u64,
        acl_decoder: Option<&dyn AclDecoder>,
    ) -> Result<u64> {
        let hierarchy = self.scene_hierarchy(SceneHierarchyLimits::default())?;
        let model = build_model_ir(&self.collection, &hierarchy, ModelIrLimits::default())?;
        let graph = build_animation_graph(
            &self.collection,
            &hierarchy,
            AnimationGraphLimits::default(),
        )?;
        let animations = build_model_animations_with_acl_decoder(
            &self.collection,
            &model,
            &graph,
            ModelAnimationLimits::default(),
            acl_decoder,
        )?;
        write_model_ir_fbx_ascii_with_animations(&model, &animations, output, maximum_output_bytes)
    }

    /// Streams the same static scene in FBX 7.4's binary encoding.
    ///
    /// The scene is identical to [`Self::write_static_fbx`]; only the encoding
    /// differs. Binary is smaller and parses faster, and some importers accept
    /// only it.
    pub fn write_static_fbx_binary(
        &self,
        output: &mut impl Write,
        maximum_output_bytes: u64,
    ) -> Result<u64> {
        let hierarchy = self.scene_hierarchy(SceneHierarchyLimits::default())?;
        let model = build_model_ir(&self.collection, &hierarchy, ModelIrLimits::default())?;
        crate::fbx_binary_scene::write_model_ir_fbx_binary(&model, output, maximum_output_bytes)
    }

    /// Materializes bounded static binary FBX 7.4 bytes.
    pub fn read_static_fbx_binary(&self, maximum_output_bytes: u64) -> Result<Vec<u8>> {
        let maximum = usize::try_from(maximum_output_bytes).map_err(|_| {
            crate::Error::invalid_data("FBX output limit does not fit this platform")
        })?;
        let mut output = LimitedBuffer::new(maximum, "binary FBX");
        let write_result = self.write_static_fbx_binary(&mut output, maximum_output_bytes);
        output.finish(write_result)?;
        Ok(output.bytes)
    }

    /// Streams binary FBX 7.4 with the animation tracks [`Self::write_fbx`]
    /// emits.
    pub fn write_fbx_binary(
        &self,
        output: &mut impl Write,
        maximum_output_bytes: u64,
    ) -> Result<u64> {
        self.write_fbx_binary_with_acl_decoder(output, maximum_output_bytes, None)
    }

    /// Builds and writes binary FBX, optionally using an injected ACL decoder.
    pub fn write_fbx_binary_with_acl_decoder(
        &self,
        output: &mut impl Write,
        maximum_output_bytes: u64,
        acl_decoder: Option<&dyn AclDecoder>,
    ) -> Result<u64> {
        let hierarchy = self.scene_hierarchy(SceneHierarchyLimits::default())?;
        let model = build_model_ir(&self.collection, &hierarchy, ModelIrLimits::default())?;
        let graph = build_animation_graph(
            &self.collection,
            &hierarchy,
            AnimationGraphLimits::default(),
        )?;
        let animations = build_model_animations_with_acl_decoder(
            &self.collection,
            &model,
            &graph,
            ModelAnimationLimits::default(),
            acl_decoder,
        )?;
        crate::fbx_binary_scene::write_model_ir_fbx_binary_full(
            &model,
            Some(&animations),
            None,
            output,
            maximum_output_bytes,
        )
    }

    /// Materializes bounded binary FBX 7.4 with supported animation tracks.
    pub fn read_fbx_binary(&self, maximum_output_bytes: u64) -> Result<Vec<u8>> {
        self.read_fbx_binary_with_acl_decoder(maximum_output_bytes, None)
    }

    /// Materializes binary FBX, optionally using an injected ACL decoder.
    pub fn read_fbx_binary_with_acl_decoder(
        &self,
        maximum_output_bytes: u64,
        acl_decoder: Option<&dyn AclDecoder>,
    ) -> Result<Vec<u8>> {
        let maximum = usize::try_from(maximum_output_bytes).map_err(|_| {
            crate::Error::invalid_data("FBX output limit does not fit this platform")
        })?;
        let mut output = LimitedBuffer::new(maximum, "binary FBX");
        let write_result =
            self.write_fbx_binary_with_acl_decoder(&mut output, maximum_output_bytes, acl_decoder);
        output.finish(write_result)?;
        Ok(output.bytes)
    }

    /// Builds the model with its animations and its material textures, streams
    /// the FBX, and returns the texture set alongside the byte count.
    ///
    /// The FBX references each texture by file name, so a caller has to write
    /// the returned set beside the model for those references to resolve. That
    /// is why the set comes back rather than being written here: this method
    /// owns one output stream and has no directory to write siblings into.
    pub fn write_fbx_with_textures(
        &self,
        output: &mut impl Write,
        maximum_output_bytes: u64,
        format: crate::image_export::ImageFormat,
        texture_limits: crate::scene_textures::SceneTextureLimits,
    ) -> Result<(u64, crate::scene_textures::SceneTextureSet)> {
        let hierarchy = self.scene_hierarchy(SceneHierarchyLimits::default())?;
        let model = build_model_ir(&self.collection, &hierarchy, ModelIrLimits::default())?;
        let graph = build_animation_graph(
            &self.collection,
            &hierarchy,
            AnimationGraphLimits::default(),
        )?;
        let animations = build_model_animations_with_acl_decoder(
            &self.collection,
            &model,
            &graph,
            ModelAnimationLimits::default(),
            None,
        )?;
        let textures = crate::scene_textures::SceneTextureSet::from_model(
            &self.collection,
            &model,
            format,
            texture_limits,
        )?;
        let written = crate::fbx_ascii::write_model_ir_fbx_ascii_with_textures(
            &model,
            &animations,
            &textures,
            output,
            maximum_output_bytes,
        )?;
        Ok((written, textures))
    }

    /// Materializes bounded ASCII FBX 7.4 with supported animation tracks.
    pub fn read_fbx(&self, maximum_output_bytes: u64) -> Result<Vec<u8>> {
        self.read_fbx_with_acl_decoder(maximum_output_bytes, None)
    }

    /// Materializes ASCII FBX, optionally using an injected ACL decoder.
    pub fn read_fbx_with_acl_decoder(
        &self,
        maximum_output_bytes: u64,
        acl_decoder: Option<&dyn AclDecoder>,
    ) -> Result<Vec<u8>> {
        let maximum = usize::try_from(maximum_output_bytes).map_err(|_| {
            crate::Error::invalid_data("FBX output limit does not fit this platform")
        })?;
        let mut output = LimitedBuffer::new(maximum, "ASCII FBX");
        let write_result =
            self.write_fbx_with_acl_decoder(&mut output, maximum_output_bytes, acl_decoder);
        output.finish(write_result)?;
        Ok(output.bytes)
    }

    /// Plans independently exported `SplitObjects` model branches.
    pub fn split_object_fbx_candidates(
        &self,
        limits: ModelExportPlanLimits,
    ) -> Result<Vec<ModelExportCandidate>> {
        let hierarchy = self.scene_hierarchy(SceneHierarchyLimits::default())?;
        plan_split_object_exports(&hierarchy, limits)
    }

    /// Plans independently exported Animator-owned model branches.
    pub fn animator_fbx_candidates(
        &self,
        limits: ModelExportPlanLimits,
    ) -> Result<Vec<ModelExportCandidate>> {
        let hierarchy = self.scene_hierarchy(SceneHierarchyLimits::default())?;
        plan_animator_exports(&hierarchy, limits)
    }

    /// Streams one selected `GameObject` branch as bounded ASCII FBX 7.4.
    ///
    /// The complete descendant subtree and transform-only ancestor chain are
    /// retained. When requested, only Animator or legacy Animation bindings
    /// owned by nodes in that selected model contribute clips.
    pub fn write_game_object_fbx(
        &self,
        game_object: SceneObjectKey,
        include_animations: bool,
        output: &mut impl Write,
        maximum_output_bytes: u64,
    ) -> Result<u64> {
        self.write_game_object_fbx_with_acl_decoder(
            game_object,
            include_animations,
            output,
            maximum_output_bytes,
            None,
        )
    }

    /// Streams one selected `GameObject` branch and optionally decodes
    /// Tuanjie ACL-backed animations through a safe injected decoder.
    pub fn write_game_object_fbx_with_acl_decoder(
        &self,
        game_object: SceneObjectKey,
        include_animations: bool,
        output: &mut impl Write,
        maximum_output_bytes: u64,
        acl_decoder: Option<&dyn AclDecoder>,
    ) -> Result<u64> {
        let hierarchy = self.scene_hierarchy(SceneHierarchyLimits::default())?;
        let model = build_model_ir_for_game_object(
            &self.collection,
            &hierarchy,
            game_object,
            ModelIrLimits::default(),
        )?;
        if !include_animations {
            return write_model_ir_fbx_ascii(&model, output, maximum_output_bytes);
        }
        let graph = build_animation_graph(
            &self.collection,
            &hierarchy,
            AnimationGraphLimits::default(),
        )?;
        let animations = build_model_animations_with_acl_decoder(
            &self.collection,
            &model,
            &graph,
            ModelAnimationLimits::default(),
            acl_decoder,
        )?;
        write_model_ir_fbx_ascii_with_animations(&model, &animations, output, maximum_output_bytes)
    }

    /// Materializes one selected, bounded `GameObject` FBX branch.
    pub fn read_game_object_fbx(
        &self,
        game_object: SceneObjectKey,
        include_animations: bool,
        maximum_output_bytes: u64,
    ) -> Result<Vec<u8>> {
        self.read_game_object_fbx_with_acl_decoder(
            game_object,
            include_animations,
            maximum_output_bytes,
            None,
        )
    }

    /// Materializes one selected FBX branch with an optional ACL decoder.
    pub fn read_game_object_fbx_with_acl_decoder(
        &self,
        game_object: SceneObjectKey,
        include_animations: bool,
        maximum_output_bytes: u64,
        acl_decoder: Option<&dyn AclDecoder>,
    ) -> Result<Vec<u8>> {
        let maximum = usize::try_from(maximum_output_bytes).map_err(|_| {
            crate::Error::invalid_data("FBX output limit does not fit this platform")
        })?;
        let mut output = LimitedBuffer::new(maximum, "selected ASCII FBX");
        let write_result = self.write_game_object_fbx_with_acl_decoder(
            game_object,
            include_animations,
            &mut output,
            maximum_output_bytes,
            acl_decoder,
        );
        output.finish(write_result)?;
        Ok(output.bytes)
    }

    /// Builds source-bound `Live2D` package plans and diagnostics.
    pub fn live2d_packages(&self, limits: Live2dPackageLimits) -> Result<Live2dPackageSet> {
        build_live2d_packages(&self.collection, limits)
    }

    /// Builds `Live2D` plans with external schemas for stripped managed types.
    pub fn live2d_packages_with_schema_provider(
        &self,
        limits: Live2dPackageLimits,
        provider: &dyn MonoBehaviourSchemaProvider,
    ) -> Result<Live2dPackageSet> {
        build_live2d_packages_with_schema_provider(&self.collection, limits, provider)
    }

    /// Builds `Live2D` plans with optional managed schemas and optional ACL
    /// sample decompression, without introducing an ABI boundary.
    pub fn live2d_packages_with_adapters(
        &self,
        limits: Live2dPackageLimits,
        provider: Option<&dyn MonoBehaviourSchemaProvider>,
        acl_decoder: Option<&dyn AclDecoder>,
    ) -> Result<Live2dPackageSet> {
        match (provider, acl_decoder) {
            (Some(provider), Some(decoder)) => {
                build_live2d_packages_with_schema_provider_and_acl_decoder(
                    &self.collection,
                    limits,
                    provider,
                    decoder,
                )
            }
            (Some(provider), None) => {
                build_live2d_packages_with_schema_provider(&self.collection, limits, provider)
            }
            (None, Some(decoder)) => {
                build_live2d_packages_with_acl_decoder(&self.collection, limits, decoder)
            }
            (None, None) => build_live2d_packages(&self.collection, limits),
        }
    }

    /// Materializes verified MOC, model3 JSON, and mip-zero texture PNG files.
    ///
    /// Embedded-schema expressions, motion, physics, pose, and display-info
    /// are included when their verified fields are present.
    pub fn read_live2d_packages(
        &self,
        planning_limits: Live2dPackageLimits,
        materialize_limits: Live2dPackageMaterializeLimits,
    ) -> Result<Live2dPackageBytesSet> {
        let packages = self.live2d_packages(planning_limits)?;
        materialize_live2d_packages(packages, materialize_limits)
    }

    /// Materializes packages after resolving stripped managed fields through
    /// a trusted, non-executing external schema provider.
    pub fn read_live2d_packages_with_schema_provider(
        &self,
        planning_limits: Live2dPackageLimits,
        materialize_limits: Live2dPackageMaterializeLimits,
        provider: &dyn MonoBehaviourSchemaProvider,
    ) -> Result<Live2dPackageBytesSet> {
        let packages = self.live2d_packages_with_schema_provider(planning_limits, provider)?;
        materialize_live2d_packages(packages, materialize_limits)
    }

    /// Materializes `Live2D` packages with optional managed schemas and an
    /// optional caller-supplied Tuanjie ACL decoder.
    pub fn read_live2d_packages_with_adapters(
        &self,
        planning_limits: Live2dPackageLimits,
        materialize_limits: Live2dPackageMaterializeLimits,
        provider: Option<&dyn MonoBehaviourSchemaProvider>,
        acl_decoder: Option<&dyn AclDecoder>,
    ) -> Result<Live2dPackageBytesSet> {
        let packages =
            self.live2d_packages_with_adapters(planning_limits, provider, acl_decoder)?;
        materialize_live2d_packages(packages, materialize_limits)
    }
}

/// Borrowed metadata for one discovered serialized file.
#[derive(Debug, Clone, Copy)]
pub struct StudioFile<'a> {
    index: usize,
    loaded: &'a LoadedSerializedFile,
}

impl StudioFile<'_> {
    #[must_use]
    pub const fn index(&self) -> usize {
        self.index
    }

    #[must_use]
    pub fn path(&self) -> &str {
        &self.loaded.path
    }

    #[must_use]
    pub fn unity_version(&self) -> &str {
        &self.loaded.file.unity_version_string
    }

    #[must_use]
    pub fn object_count(&self) -> usize {
        self.loaded.file.objects.len()
    }
}

/// Borrowed, source-bound access to one discovered external resource.
#[derive(Debug, Clone, Copy)]
pub struct StudioResource<'a> {
    index: usize,
    loaded: &'a LoadedResource,
}

impl StudioResource<'_> {
    #[must_use]
    pub const fn index(&self) -> usize {
        self.index
    }

    #[must_use]
    pub fn path(&self) -> &str {
        &self.loaded.path
    }

    #[must_use]
    pub fn byte_size(&self) -> u64 {
        self.loaded.region.len()
    }

    /// Streams the complete resource without first materializing it.
    pub fn write(&self, output: &mut impl Write) -> Result<u64> {
        self.write_range(0, self.loaded.region.len(), output)
    }

    /// Streams one checked resource range without materializing it.
    pub fn write_range(&self, offset: u64, length: u64, output: &mut impl Write) -> Result<u64> {
        self.loaded.region.copy_range(offset, length, output)
    }

    /// Materializes the resource under an explicit byte limit.
    pub fn read(&self, maximum_bytes: u64) -> Result<Vec<u8>> {
        self.loaded.region.read_to_vec(maximum_bytes)
    }

    /// Materializes one checked resource range under an explicit byte limit.
    pub fn read_range(&self, offset: u64, length: u64, maximum_bytes: u64) -> Result<Vec<u8>> {
        self.loaded
            .region
            .subregion(offset, length)?
            .read_to_vec(maximum_bytes)
    }
}

/// Borrowed handle to one real serialized object.
#[derive(Debug, Clone, Copy)]
pub struct StudioObject<'a> {
    studio: &'a Studio,
    file_index: usize,
    object_index: usize,
}

impl StudioObject<'_> {
    fn loaded(&self) -> &LoadedSerializedFile {
        &self.studio.collection.serialized_files[self.file_index]
    }

    fn info(&self) -> &ObjectInfo {
        &self.loaded().file.objects[self.object_index]
    }

    #[must_use]
    pub const fn file_index(&self) -> usize {
        self.file_index
    }

    #[must_use]
    pub const fn object_index(&self) -> usize {
        self.object_index
    }

    #[must_use]
    pub fn source_path(&self) -> &str {
        &self.loaded().path
    }

    #[must_use]
    pub fn path_id(&self) -> i64 {
        self.info().path_id
    }

    #[must_use]
    pub fn class_id(&self) -> i32 {
        self.info().class_id
    }

    #[must_use]
    pub fn byte_size(&self) -> u64 {
        self.info().byte_size
    }

    /// Returns a resolved name when the loader could safely parse one.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.studio
            .collection
            .object_metadata(self.file_index, self.path_id())
            .and_then(|metadata| metadata.name.as_deref())
    }

    /// Returns the resolved AssetBundle/ResourceManager container path.
    #[must_use]
    pub fn container(&self) -> Option<&str> {
        self.studio
            .collection
            .object_metadata(self.file_index, self.path_id())
            .and_then(|metadata| metadata.container.as_deref())
    }

    /// Streams the exact serialized object bytes without materializing them.
    pub fn write_raw(&self, output: &mut impl Write) -> Result<u64> {
        self.loaded()
            .file
            .object_region(self.object_index)?
            .copy_range(0, self.byte_size(), output)
    }

    /// Materializes the exact serialized object bytes under a caller limit.
    pub fn read_raw(&self, maximum_bytes: u64) -> Result<Vec<u8>> {
        self.loaded()
            .file
            .read_object_bytes(self.object_index, maximum_bytes)
    }

    /// Reads `TextAsset` contents under a caller limit.
    pub fn read_text_bytes(&self, maximum_bytes: usize) -> Result<Vec<u8>> {
        self.loaded()
            .file
            .read_text_asset(self.object_index, maximum_bytes)
            .map(|asset| asset.script)
    }

    /// Converts one Unity `Shader` to the bounded textual payload produced by
    /// `AssetStudio`'s managed shader converter.
    pub fn read_shader_text(&self, maximum_output_bytes: u64) -> Result<Vec<u8>> {
        let limits = ShaderReadLimits {
            maximum_output_bytes,
            ..ShaderReadLimits::default()
        };
        read_shader(&self.loaded().file, self.object_index, limits)?
            .read_to_vec(maximum_output_bytes)
    }

    /// Streams one resident or collection-backed Unity `Mesh` using the managed-compatible OBJ
    /// coordinate, winding, numeric, and CRLF text contract.
    pub fn write_mesh_obj(&self, output: &mut impl Write, limits: MeshReadLimits) -> Result<u64> {
        write_mesh_object_obj_with_collection(
            &self.studio.collection,
            &self.loaded().file,
            self.object_index,
            output,
            limits,
        )
    }

    /// Materializes one resident Unity `Mesh` as bounded OBJ bytes.
    pub fn read_mesh_obj(&self, limits: MeshReadLimits) -> Result<Vec<u8>> {
        let maximum = usize::try_from(limits.maximum_output_bytes)
            .map_err(|_| crate::Error::invalid_data("Mesh OBJ limit does not fit this platform"))?;
        let mut output = LimitedBuffer::new(maximum, "Mesh OBJ");
        let write_result = self.write_mesh_obj(&mut output, limits);
        output.finish(write_result)?;
        Ok(output.bytes)
    }

    /// Reads one bounded Unity or Tuanjie `AnimationClip` object.
    pub fn read_animation_clip(&self, limits: AnimationClipReadLimits) -> Result<AnimationClip> {
        read_animation_clip(&self.loaded().file, self.object_index, limits)
    }

    /// Reads one complete, bounded Unity or Tuanjie `AnimatorController`.
    pub fn read_animator_controller(
        &self,
        limits: AnimatorControllerReadLimits,
    ) -> Result<AnimatorController> {
        read_animator_controller(&self.loaded().file, self.object_index, limits)
    }

    /// Reads one complete, bounded Unity or Tuanjie `Avatar`.
    pub fn read_avatar(&self, limits: AvatarReadLimits) -> Result<Avatar> {
        read_avatar(&self.loaded().file, self.object_index, limits)
    }

    /// Reads source-bound `AudioClip` bytes plus verified direct-WAV metadata.
    pub fn read_audio_clip(&self, limits: SimpleAssetReadLimits) -> Result<AudioClipAsset> {
        read_audio_clip_asset(
            &self.studio.collection,
            &self.loaded().file,
            self.object_index,
            limits,
        )
    }

    /// Reads the embedded font program from one Unity `Font`.
    pub fn read_font(&self, limits: SimpleAssetReadLimits) -> Result<SimpleBinaryAsset> {
        read_font(&self.loaded().file, self.object_index, limits)
    }

    /// Reads the resident Ogg payload from one legacy `MovieTexture`.
    pub fn read_movie_texture(&self, limits: SimpleAssetReadLimits) -> Result<SimpleBinaryAsset> {
        read_movie_texture(&self.loaded().file, self.object_index, limits)
    }

    /// Reads one `VideoClip`, resolving inline or external payload bytes.
    pub fn read_video_clip(&self, limits: SimpleAssetReadLimits) -> Result<SimpleBinaryAsset> {
        read_video_clip(
            &self.studio.collection,
            &self.loaded().file,
            self.object_index,
            limits,
        )
    }

    /// Reads one bounded Unity `Material` property sheet without resolving its references.
    pub fn read_material(&self, limits: MaterialReadLimits) -> Result<Material> {
        read_material(&self.loaded().file, self.object_index, limits)
    }

    /// Reads the managed type identity stored in one Unity `MonoScript`
    /// without opening or executing its named assembly.
    pub fn read_mono_script(&self, limits: MonoBehaviourReadLimits) -> Result<MonoScript> {
        read_mono_script(&self.loaded().file, self.object_index, limits)
    }

    /// Converts an embedded `TypeTree` to deterministic JSON under an output
    /// byte limit.
    pub fn read_type_tree_json(&self, pretty: bool, maximum_bytes: usize) -> Result<Vec<u8>> {
        let value = self.loaded().file.read_type_tree_value(self.object_index)?;
        let mut output = LimitedBuffer::new(maximum_bytes, "TypeTree JSON");
        let write_result = crate::json::write_type_value_json(&value, &mut output, pretty);
        output.finish(write_result)?;
        Ok(output.bytes)
    }

    /// Streams the managed-compatible, tab-indented `TypeTree` text dump.
    pub fn write_type_tree_dump(
        &self,
        output: &mut impl Write,
        maximum_output_bytes: u64,
    ) -> Result<u64> {
        let tree = self.loaded().file.object_type_tree(self.object_index)?;
        let value = self.loaded().file.read_type_tree_value(self.object_index)?;
        crate::type_tree_dump::write_type_tree_dump(tree, &value, output, maximum_output_bytes)
    }

    /// Materializes the managed-compatible `TypeTree` text dump under a
    /// caller-provided byte limit.
    pub fn read_type_tree_dump(&self, maximum_output_bytes: u64) -> Result<Vec<u8>> {
        let maximum = usize::try_from(maximum_output_bytes).map_err(|_| {
            crate::Error::invalid_data("TypeTree dump output limit does not fit this platform")
        })?;
        let mut output = LimitedBuffer::new(maximum, "TypeTree dump");
        let write_result = self.write_type_tree_dump(&mut output, maximum_output_bytes);
        output.finish(write_result)?;
        Ok(output.bytes)
    }

    /// Reads a `MonoBehaviour` using its embedded tree or a trusted external
    /// full-object schema, then returns bounded deterministic JSON.
    pub fn read_mono_behaviour_json(
        &self,
        provider: &dyn MonoBehaviourSchemaProvider,
        pretty: bool,
        limits: MonoBehaviourReadLimits,
    ) -> Result<ResolvedMonoBehaviourJson> {
        read_mono_behaviour_json_with_provider(
            &self.studio.collection,
            self.file_index,
            self.object_index,
            provider,
            pretty,
            limits,
        )
    }

    /// Decodes one `Texture2D` mip to RGBA8 pixels in Unity's decoded row
    /// order. Format-specific writers can flip it to display order.
    pub fn decode_texture_mip(
        &self,
        mip_level: u32,
        limits: TextureReadLimits,
    ) -> Result<RgbaImage> {
        let texture = read_texture2d(
            &self.studio.collection,
            &self.loaded().file,
            self.object_index,
            limits,
        )?;
        texture.decode_mip_rgba8(mip_level, limits)
    }

    /// Decodes mip zero for every layer of one `Texture2DArray` in numeric
    /// layer order. Pixels retain Unity's decoded row order; display-oriented
    /// bindings can flip each image without changing the format parser.
    pub fn decode_texture_array_mip0(
        &self,
        limits: TextureArrayReadLimits,
    ) -> Result<Vec<RgbaImage>> {
        let texture = read_texture2d_array(
            &self.studio.collection,
            &self.loaded().file,
            self.object_index,
            limits,
        )?;
        texture.decode_mip0_layers_rgba8(limits)
    }

    /// Resolves and decodes one `Sprite` to display-order RGBA8 pixels.
    ///
    /// Explicit atlases, collection-level `packedSprites` backfill, variant
    /// replacement, color/alpha textures, packing transforms, and supported
    /// cross-file references all use the same path as the atomic exporter.
    pub fn decode_sprite(
        &self,
        sprite_limits: SpriteReadLimits,
        texture_limits: TextureReadLimits,
    ) -> Result<RgbaImage> {
        let sprite = read_sprite(&self.loaded().file, self.object_index, sprite_limits)?;
        decode_sprite_rgba8(
            &self.studio.collection,
            &self.loaded().file,
            &sprite,
            sprite_limits,
            texture_limits,
        )
    }

    /// Reads class 141 scene/level paths with exact Unity version gates.
    pub fn read_build_settings(&self, limits: ProjectSettingsReadLimits) -> Result<BuildSettings> {
        read_build_settings(&self.loaded().file, self.object_index, limits)
    }

    /// Reads the stable company/product identity fields from class 129.
    pub fn read_player_settings(
        &self,
        limits: ProjectSettingsReadLimits,
    ) -> Result<PlayerSettings> {
        read_player_settings(&self.loaded().file, self.object_index, limits)
    }

    /// Projects a `CubismExpressionData` embedded `TypeTree` into exp3 fields.
    pub fn read_cubism_expression(
        &self,
        limits: CubismExpressionReadLimits,
    ) -> Result<CubismExpression> {
        read_cubism_expression(&self.loaded().file, self.object_index, limits)
    }

    /// Projects an embedded-schema `CubismPosePart` component.
    pub fn read_cubism_pose_part(
        &self,
        limits: CubismAuxiliaryReadLimits,
    ) -> Result<CubismPosePart> {
        read_cubism_pose_part(&self.loaded().file, self.object_index, limits)
    }

    /// Projects an embedded-schema Cubism display-info component.
    pub fn read_cubism_display_info(
        &self,
        limits: CubismAuxiliaryReadLimits,
    ) -> Result<CubismDisplayInfo> {
        read_cubism_display_info(&self.loaded().file, self.object_index, limits)
    }

    /// Projects an embedded-schema `CubismPhysicsController` rig.
    pub fn read_cubism_physics(&self, limits: CubismPhysicsReadLimits) -> Result<CubismPhysicsRig> {
        read_cubism_physics(&self.loaded().file, self.object_index, limits)
    }

    /// Projects an embedded-schema `CubismFadeMotionData` object.
    pub fn read_cubism_fade_motion(
        &self,
        limits: CubismFadeMotionReadLimits,
    ) -> Result<CubismFadeMotion> {
        read_cubism_fade_motion(&self.loaded().file, self.object_index, limits)
    }

    /// Projects this `AnimationClip` through Cubism's V2 parameter/part bindings.
    pub fn read_cubism_clip_motion(
        &self,
        targets: &crate::live2d_motion::CubismMotionTargetNames,
        limits: CubismClipMotionReadLimits,
    ) -> Result<CubismClipMotion> {
        read_cubism_clip_motion(
            &self.studio.collection,
            self.file_index,
            self.object_index,
            targets,
            limits,
        )
    }

    /// Projects this clip through Cubism bindings with an injected ACL decoder.
    pub fn read_cubism_clip_motion_with_acl_decoder(
        &self,
        targets: &crate::live2d_motion::CubismMotionTargetNames,
        limits: CubismClipMotionReadLimits,
        acl_decoder: &dyn AclDecoder,
    ) -> Result<CubismClipMotion> {
        read_cubism_clip_motion_with_acl_decoder(
            &self.studio.collection,
            self.file_index,
            self.object_index,
            targets,
            limits,
            Some(acl_decoder),
        )
    }
}

struct LimitedBuffer {
    bytes: Vec<u8>,
    maximum: usize,
    field: &'static str,
    limit_exceeded: bool,
}

impl LimitedBuffer {
    const fn new(maximum: usize, field: &'static str) -> Self {
        Self {
            bytes: Vec::new(),
            maximum,
            field,
            limit_exceeded: false,
        }
    }

    fn finish<T>(&self, result: Result<T>) -> Result<()> {
        if self.limit_exceeded {
            return Err(crate::Error::invalid_data(format!(
                "{} exceeds {} bytes",
                self.field, self.maximum
            )));
        }
        result.map(|_| ())
    }
}

impl Write for LimitedBuffer {
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

#[cfg(test)]
mod tests {
    use crate::loader::{AssetCollection, LoadedResource};
    use crate::source::Region;

    use super::Studio;

    #[test]
    fn high_level_api_lists_finds_and_reads_without_an_abi_layer() {
        let memory_studio = Studio::open_region(
            "memory.assets",
            Region::from_bytes(synthetic_v22_text_asset()),
        )
        .unwrap();
        assert_eq!(
            memory_studio
                .object(0, 7)
                .unwrap()
                .read_text_bytes(16)
                .unwrap(),
            b"payload"
        );
        let memory_collection = Studio::open_regions([
            (
                "memory.assets".to_owned(),
                Region::from_bytes(synthetic_v22_text_asset()),
            ),
            (
                "memory.resS".to_owned(),
                Region::from_bytes(b"memory resource".as_slice()),
            ),
        ])
        .unwrap();
        assert_eq!(memory_collection.file_count(), 1);
        assert_eq!(memory_collection.resource_count(), 1);
        assert_eq!(
            memory_collection
                .resource_by_path("MEMORY.RESS")
                .unwrap()
                .read(15)
                .unwrap(),
            b"memory resource"
        );

        let mut collection = AssetCollection::load(
            "high-level.assets",
            Region::from_bytes(synthetic_v22_text_asset()),
        )
        .unwrap();
        collection.resources.push(LoadedResource {
            path: "archive:/high-level/high-level.resS".to_owned(),
            region: Region::from_bytes(b"external payload".as_slice()),
        });
        let studio = Studio::from_collection(collection);

        assert_eq!(studio.file_count(), 1);
        assert_eq!(studio.object_count(), 1);
        assert_eq!(studio.resource_count(), 1);
        let file = studio.files().next().unwrap();
        assert_eq!(file.index(), 0);
        assert_eq!(file.path(), "high-level.assets");
        assert_eq!(file.unity_version(), "2022.3.62f1");
        assert_eq!(file.object_count(), 1);
        assert_eq!(studio.file(0).unwrap().path(), "high-level.assets");
        assert!(studio.file(1).is_none());

        let object = studio.objects().next().unwrap();
        assert_eq!(object.file_index(), 0);
        assert_eq!(object.object_index(), 0);
        assert_eq!(object.path_id(), 7);
        assert_eq!(object.class_id(), 49);
        assert_eq!(object.name(), Some("hello"));
        assert_eq!(object.read_text_bytes(16).unwrap(), b"payload");
        assert_eq!(
            object.read_type_tree_dump(1024).unwrap(),
            b"TextAsset Base\r\n\
\tstring m_Name = \"hello\"\r\n\
\tTypelessData m_Script\r\n\
\tint size = 7\r\n"
        );
        assert!(object.read_type_tree_dump(8).is_err());
        let mut streamed_dump = Vec::new();
        assert_eq!(
            object
                .write_type_tree_dump(&mut streamed_dump, 1024)
                .unwrap(),
            u64::try_from(streamed_dump.len()).unwrap()
        );
        assert!(object.read_raw(4).is_err());
        assert_eq!(studio.object(0, 7).unwrap().path_id(), 7);
        assert!(studio.object(0, 8).is_none());

        let resource = studio.resources().next().unwrap();
        assert_eq!(resource.index(), 0);
        assert_eq!(resource.path(), "archive:/high-level/high-level.resS");
        assert_eq!(resource.byte_size(), 16);
        assert_eq!(resource.read(16).unwrap(), b"external payload");
        assert!(resource.read(15).is_err());
        assert_eq!(resource.read_range(9, 7, 7).unwrap(), b"payload");
        assert!(resource.read_range(9, 7, 6).is_err());
        assert!(resource.read_range(15, 2, 2).is_err());
        let mut streamed_resource = Vec::new();
        assert_eq!(resource.write(&mut streamed_resource).unwrap(), 16);
        assert_eq!(streamed_resource, b"external payload");
        let mut streamed_range = Vec::new();
        assert_eq!(resource.write_range(0, 8, &mut streamed_range).unwrap(), 8);
        assert_eq!(streamed_range, b"external");
        assert_eq!(
            studio.resource_by_path("HIGH-LEVEL.RESS").unwrap().index(),
            0
        );
        assert!(studio.resource(1).is_none());
    }

    fn synthetic_v22_text_asset() -> Vec<u8> {
        let mut object = Vec::new();
        push_aligned_string(&mut object, "hello");
        push_i32(&mut object, 7);
        object.extend_from_slice(b"payload");

        let mut metadata = Vec::new();
        metadata.extend_from_slice(b"2022.3.62f1\0");
        push_i32(&mut metadata, 13);
        metadata.push(1);
        push_i32(&mut metadata, 1);
        push_i32(&mut metadata, 49);
        metadata.push(0);
        metadata.extend_from_slice(&(-1_i16).to_le_bytes());
        metadata.extend_from_slice(&[0_u8; 16]);
        push_blob_tree(
            &mut metadata,
            &[
                ("TextAsset", "Base", 0),
                ("string", "m_Name", 1),
                ("Array", "Array", 2),
                ("int", "size", 3),
                ("char", "data", 3),
                ("TypelessData", "m_Script", 1),
                ("int", "size", 2),
                ("UInt8", "data", 2),
            ],
        );
        push_i32(&mut metadata, 0);
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
        bytes[20..24].copy_from_slice(&metadata_size.to_be_bytes());
        bytes[24..32].copy_from_slice(&i64::try_from(file_size).unwrap().to_be_bytes());
        bytes[32..40].copy_from_slice(&i64::try_from(data_offset).unwrap().to_be_bytes());
        bytes.extend_from_slice(&metadata);
        bytes.resize(usize::try_from(data_offset).unwrap(), 0);
        bytes.extend_from_slice(&object);
        bytes
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

    fn push_blob_tree(output: &mut Vec<u8>, nodes: &[(&str, &str, u8)]) {
        let mut strings = Vec::new();
        let mut offsets = Vec::new();
        for &(type_name, field_name, _) in nodes {
            let type_offset = u32::try_from(strings.len()).unwrap();
            strings.extend_from_slice(type_name.as_bytes());
            strings.push(0);
            let field_offset = u32::try_from(strings.len()).unwrap();
            strings.extend_from_slice(field_name.as_bytes());
            strings.push(0);
            offsets.push((type_offset, field_offset));
        }
        push_i32(output, i32::try_from(nodes.len()).unwrap());
        push_i32(output, i32::try_from(strings.len()).unwrap());
        for (index, (&(_, _, level), (type_offset, field_offset))) in
            nodes.iter().zip(offsets).enumerate()
        {
            output.extend_from_slice(&1_u16.to_le_bytes());
            output.push(level);
            output.push(0);
            output.extend_from_slice(&type_offset.to_le_bytes());
            output.extend_from_slice(&field_offset.to_le_bytes());
            output.extend_from_slice(&(-1_i32).to_le_bytes());
            output.extend_from_slice(&i32::try_from(index).unwrap().to_le_bytes());
            output.extend_from_slice(&0_i32.to_le_bytes());
            output.extend_from_slice(&0_u64.to_le_bytes());
        }
        output.extend_from_slice(&strings);
    }

    fn align_with_base(output: &mut Vec<u8>, base: usize, alignment: usize) {
        while !(base + output.len()).is_multiple_of(alignment) {
            output.push(0);
        }
    }
}
