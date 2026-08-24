//! Resolves the textures a model's materials reference and encodes them for
//! output beside the model file.
//!
//! A `Material` stores its textures as `PPtr`s inside `m_SavedProperties`, so a
//! model exporter that only writes geometry and material colours drops every
//! image the model actually uses. This module resolves those pointers against
//! the loaded collection, decodes each `Texture2D` once, and assigns the stable
//! file names the exporters reference.
//!
//! Names come from the asset and are therefore untrusted. Every name is reduced
//! to a single path component here, so a texture called `../../etc/passwd`
//! cannot escape the directory the caller chose.

use std::collections::HashMap;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::image_export::{ImageFormat, ImageRowOrder, write_rgba_image};
use crate::loader::AssetCollection;
use crate::model_ir::ModelIr;
use crate::scene::resolve_object_reference;
use crate::scene_hierarchy::SceneObjectKey;
use crate::texture::{TEXTURE_2D_CLASS_ID, TextureReadLimits, read_texture2d};
use crate::{Error, Result};

const MAXIMUM_TEXTURE_TEMPORARY_ATTEMPTS: u64 = 1_024;
const MAXIMUM_SCENE_TEXTURE_COMPONENT_BYTES: usize = 240;
static TEXTURE_TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// The material channel a texture is bound to.
///
/// Unity's property names are shader-defined, so the managed reader maps the
/// four it recognises onto fixed channels and leaves everything else unbound.
/// An unbound texture is still resolved and written; it simply has no material
/// property to connect to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextureSlot {
    Diffuse,
    NormalMap,
    Specular,
    Bump,
}

impl TextureSlot {
    /// The FBX material property this channel connects to.
    #[must_use]
    pub const fn fbx_property(self) -> &'static str {
        match self {
            Self::Diffuse => "DiffuseColor",
            Self::NormalMap => "NormalMap",
            Self::Specular => "SpecularColor",
            Self::Bump => "Bump",
        }
    }

    /// The Wavefront MTL directive this channel maps to.
    #[must_use]
    pub const fn mtl_directive(self) -> &'static str {
        match self {
            Self::Diffuse => "map_Kd",
            // MTL has one bump channel; both of Unity's normal-ish properties
            // land on it, which is what every consumer expects to find.
            Self::NormalMap | Self::Bump => "map_Bump",
            Self::Specular => "map_Ks",
        }
    }

    /// Classifies a Unity shader property name.
    ///
    /// The exact-match cases are checked before the substring cases because
    /// `_BumpMap` also contains neither, and a shader is free to name a
    /// property so that several substrings apply.
    #[must_use]
    pub fn from_property_name(name: &str) -> Option<Self> {
        match name {
            "_MainTex" => Some(Self::Diffuse),
            "_BumpMap" => Some(Self::Bump),
            _ if name.contains("Specular") => Some(Self::Specular),
            _ if name.contains("Normal") => Some(Self::NormalMap),
            _ => None,
        }
    }
}

/// Bounds on how much texture data a model may pull in.
#[derive(Debug, Clone, Copy)]
pub struct SceneTextureLimits {
    /// Non-null material texture references, including references that are
    /// unresolved, unsupported or repeat an already decoded texture.
    pub maximum_texture_references: usize,
    pub maximum_textures: usize,
    /// UTF-8 bytes retained by the shared object-to-file-name and
    /// case-folded collision indexes.
    ///
    /// A batch export can reuse one [`SceneTextureNames`] across many models,
    /// so this is deliberately independent of the per-model texture count.
    pub maximum_name_index_bytes: u64,
    /// Total encoded bytes across every texture in the set.
    pub maximum_total_encoded_bytes: u64,
    pub texture: TextureReadLimits,
}

impl Default for SceneTextureLimits {
    fn default() -> Self {
        Self {
            maximum_texture_references: 1_000_000,
            maximum_textures: 4_096,
            maximum_name_index_bytes: 64 * 1024 * 1024,
            maximum_total_encoded_bytes: 2 * 1024 * 1024 * 1024,
            texture: TextureReadLimits::default(),
        }
    }
}

/// One decoded, encoded texture and the file name the exporters reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SceneTexture {
    /// A single path component, including the image extension.
    pub file_name: String,
    pub object: SceneObjectKey,
    pub encoded: Vec<u8>,
}

/// One material property bound to a texture in the set.
#[derive(Debug, Clone, PartialEq)]
pub struct SceneTextureBinding {
    /// The Unity shader property, for example `_MainTex`.
    pub property: String,
    /// Index into [`SceneTextureSet::textures`].
    pub texture: usize,
    /// The material channel, or `None` when the property is unrecognised.
    pub slot: Option<TextureSlot>,
    pub offset: [f32; 2],
    pub scale: [f32; 2],
}

/// Why a texture reference produced no texture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SceneTextureSkip {
    pub material: SceneObjectKey,
    pub property: String,
    pub reason: String,
}

/// Every texture a model's materials reference, plus the per-material bindings.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SceneTextureSet {
    pub textures: Vec<SceneTexture>,
    bindings: HashMap<SceneObjectKey, Vec<SceneTextureBinding>>,
    /// References that resolved to something this exporter cannot use.
    ///
    /// Reported rather than dropped: a `RenderTexture` or a `Cubemap` in a
    /// texture slot is a real limitation, and silently writing an untextured
    /// model would hide it.
    pub skipped: Vec<SceneTextureSkip>,
}

impl SceneTextureSet {
    /// Resolves and encodes every texture the model's materials reference.
    ///
    /// A reference that resolves to a class other than `Texture2D`, or that
    /// fails to decode, is recorded in [`Self::skipped`] rather than failing
    /// the export: one unreadable texture should not cost the whole model.
    /// Exceeding the set-wide count or encoded-byte limit is still an error,
    /// because those bounds apply to the operation as a whole. A per-texture
    /// decode limit is reported in [`Self::skipped`] like any other failure of
    /// that texture, so the remaining material images can still be returned.
    pub fn from_model(
        collection: &AssetCollection,
        model: &ModelIr,
        format: ImageFormat,
        limits: SceneTextureLimits,
    ) -> Result<Self> {
        Self::from_model_with_names(
            collection,
            model,
            format,
            limits,
            &mut SceneTextureNames::default(),
        )
    }

    /// Resolves textures against a name allocator shared with earlier models.
    ///
    /// A batch export writes many models into one directory. Each model gets
    /// its own set, so without a shared allocator two different textures that
    /// happen to share a Unity name would claim the same file and the second
    /// would be silently dropped on write. Sharing `names` keeps one texture
    /// object on one file name across the whole batch and pushes a genuine
    /// collision onto a distinct ` (n)` name.
    pub fn from_model_with_names(
        collection: &AssetCollection,
        model: &ModelIr,
        format: ImageFormat,
        limits: SceneTextureLimits,
        names: &mut SceneTextureNames,
    ) -> Result<Self> {
        let mut set = Self::default();
        let mut by_object: HashMap<SceneObjectKey, usize> = HashMap::new();
        let (mut total_encoded, mut texture_references) = (0_u64, 0_usize);

        for material in &model.materials {
            let mut bindings = Vec::new();
            for property in &material.material.saved_properties.texture_environments {
                let environment = &property.value;
                if environment.texture.is_null() {
                    continue;
                }
                charge_scene_texture_reference(
                    &mut texture_references,
                    limits.maximum_texture_references,
                )?;
                let resolved = match resolve_object_reference(
                    collection,
                    material.object.file_index,
                    environment.texture,
                ) {
                    Ok(Some(resolved)) => resolved,
                    Ok(None) => continue,
                    Err(error) => {
                        record_texture_skip(&mut set, material.object, &property.name, error)?;
                        continue;
                    }
                };
                if resolved.object.class_id != TEXTURE_2D_CLASS_ID {
                    record_texture_skip(
                        &mut set,
                        material.object,
                        &property.name,
                        format_args!("class ID {} is not Texture2D", resolved.object.class_id),
                    )?;
                    continue;
                }
                let key = SceneObjectKey {
                    file_index: resolved.file_index,
                    path_id: resolved.object.path_id,
                };
                let texture = if let Some(index) = by_object.get(&key) {
                    *index
                } else {
                    // Charge the reference before decoding it. Otherwise a
                    // caller choosing zero (or an already exhausted count)
                    // could still make us read, decode and encode one more
                    // potentially large texture before the limit fired.
                    if set.textures.len() == limits.maximum_textures {
                        return Err(Error::invalid_data(format!(
                            "model references more than {} textures",
                            limits.maximum_textures
                        )));
                    }
                    let remaining_encoded = limits
                        .maximum_total_encoded_bytes
                        .checked_sub(total_encoded)
                        .ok_or_else(|| {
                            Error::invalid_data("model texture byte accounting underflowed")
                        })?;
                    if remaining_encoded == 0 {
                        return Err(total_texture_budget_error(limits));
                    }
                    match encode_texture(collection, key, format, limits, remaining_encoded) {
                        Ok((name, encoded)) => {
                            total_encoded =
                                total_encoded.checked_add(encoded.len() as u64).ok_or_else(
                                    || Error::invalid_data("encoded texture size overflowed"),
                                )?;
                            if total_encoded > limits.maximum_total_encoded_bytes {
                                return Err(Error::invalid_data(format!(
                                    "model textures exceed the {} byte budget",
                                    limits.maximum_total_encoded_bytes
                                )));
                            }
                            let texture = ResolvedSceneTexture {
                                key,
                                name: &name,
                                encoded,
                                format,
                                maximum_name_index_bytes: limits.maximum_name_index_bytes,
                            };
                            insert_resolved_scene_texture(&mut set, &mut by_object, names, texture)?
                        }
                        Err(TextureEncodeFailure::TotalBudgetExceeded) => {
                            return Err(total_texture_budget_error(limits));
                        }
                        Err(TextureEncodeFailure::Recoverable(error)) => {
                            record_texture_skip(&mut set, material.object, &property.name, error)?;
                            continue;
                        }
                    }
                };
                append_scene_texture_binding(
                    &mut bindings,
                    &property.name,
                    texture,
                    environment.offset,
                    environment.scale,
                )?;
            }
            store_scene_texture_bindings(&mut set, material.object, bindings)?;
        }
        Ok(set)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.textures.is_empty()
    }

    /// The bindings for one material, in serialized property order.
    #[must_use]
    pub fn bindings_for(&self, material: SceneObjectKey) -> &[SceneTextureBinding] {
        self.bindings
            .get(&material)
            .map_or(&[][..], |bindings| bindings.as_slice())
    }

    /// Adds an already-encoded texture, returning its index.
    ///
    /// Allocation failure leaves the collection unchanged and is returned to
    /// the caller instead of aborting inside `Vec::push`.
    ///
    /// For callers that decoded the image themselves; [`Self::from_model`]
    /// covers resolving them from the collection. [`Self::write_to_directory`]
    /// revalidates the supplied file name as one portable relative component
    /// before it creates a temporary file, so a manually constructed texture
    /// cannot escape the selected output directory.
    pub fn push_texture(&mut self, texture: SceneTexture) -> Result<usize> {
        reserve_scene_textures(&mut self.textures, 1)?;
        let index = self.textures.len();
        self.textures.push(texture);
        Ok(index)
    }

    /// Binds a material property to a texture already in this set.
    pub fn bind(&mut self, material: SceneObjectKey, binding: SceneTextureBinding) -> Result<()> {
        if binding.texture >= self.textures.len() {
            return Err(Error::invalid_data(format!(
                "texture binding index {} is outside the {} texture(s) in the set",
                binding.texture,
                self.textures.len()
            )));
        }
        self.bindings.try_reserve(1).map_err(|error| {
            Error::invalid_data(format!("cannot grow material texture bindings: {error}"))
        })?;
        let bindings = self.bindings.entry(material).or_default();
        bindings.try_reserve(1).map_err(|error| {
            Error::invalid_data(format!("cannot grow model texture bindings: {error}"))
        })?;
        bindings.push(binding);
        Ok(())
    }

    /// Writes every texture into `directory`, returning the paths written.
    ///
    /// Existing files are left alone rather than overwritten, so exporting two
    /// models that share a texture into the same directory does not rewrite it
    /// and cannot clobber an unrelated file that happens to share the name.
    /// Each new file is completely written and synced under a temporary name
    /// in the same directory before an atomic no-clobber publish. If any later
    /// texture fails validation, writing or publication, every file newly
    /// published by this call is removed; pre-existing files are never part of
    /// that rollback.
    pub fn write_to_directory(&self, directory: &Path) -> Result<Vec<PathBuf>> {
        let mut written = Vec::new();
        written
            .try_reserve_exact(self.textures.len())
            .map_err(|error| {
                Error::invalid_data(format!("cannot allocate written texture paths: {error}"))
            })?;
        let result = (|| {
            for texture in &self.textures {
                validate_texture_file_name(&texture.file_name)?;
                let path = join_scene_texture_path_fallibly(
                    directory,
                    Path::new(&texture.file_name),
                    "model texture output path",
                )?;
                let mut temporary = TextureTemporaryFile::create(directory)?;
                temporary.file_mut().write_all(&texture.encoded)?;
                temporary.file_mut().flush()?;
                temporary.file_mut().sync_all()?;
                temporary.close()?;
                if temporary.persist_no_clobber(&path)? {
                    written.push(path);
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            if let Err(cleanup) = remove_scene_texture_outputs(&written) {
                return Err(Error::invalid_data(format!(
                    "{error}; additionally failed to roll back model textures: {cleanup}"
                )));
            }
            return Err(error);
        }
        Ok(written)
    }
}

fn charge_scene_texture_reference(count: &mut usize, maximum: usize) -> Result<()> {
    if *count >= maximum {
        return Err(Error::invalid_data(format!(
            "model has more than {maximum} non-null texture references"
        )));
    }
    *count += 1;
    Ok(())
}

fn store_scene_texture_bindings(
    set: &mut SceneTextureSet,
    material: SceneObjectKey,
    bindings: Vec<SceneTextureBinding>,
) -> Result<()> {
    if bindings.is_empty() {
        return Ok(());
    }
    set.bindings.try_reserve(1).map_err(|error| {
        Error::invalid_data(format!("cannot grow material texture bindings: {error}"))
    })?;
    set.bindings.insert(material, bindings);
    Ok(())
}

fn remove_scene_texture_outputs(paths: &[PathBuf]) -> Result<()> {
    let mut first_error = None;
    for path in paths.iter().rev() {
        if let Err(error) = fs::remove_file(path)
            && error.kind() != io::ErrorKind::NotFound
            && first_error.is_none()
        {
            first_error = Some(error);
        }
    }
    first_error.map_or(Ok(()), |error| Err(error.into()))
}

struct ResolvedSceneTexture<'a> {
    key: SceneObjectKey,
    name: &'a str,
    encoded: Vec<u8>,
    format: ImageFormat,
    maximum_name_index_bytes: u64,
}

fn insert_resolved_scene_texture(
    set: &mut SceneTextureSet,
    by_object: &mut HashMap<SceneObjectKey, usize>,
    names: &mut SceneTextureNames,
    texture: ResolvedSceneTexture<'_>,
) -> Result<usize> {
    reserve_scene_textures(&mut set.textures, 1)?;
    by_object.try_reserve(1).map_err(|error| {
        Error::invalid_data(format!("cannot grow resolved texture index: {error}"))
    })?;
    let file_name = names.allocate(
        texture.key,
        texture.name,
        texture.format,
        texture.maximum_name_index_bytes,
    )?;
    let index = set.textures.len();
    set.textures.push(SceneTexture {
        file_name,
        object: texture.key,
        encoded: texture.encoded,
    });
    by_object.insert(texture.key, index);
    Ok(index)
}

fn reserve_scene_textures(textures: &mut Vec<SceneTexture>, additional: usize) -> Result<()> {
    textures.try_reserve(additional).map_err(|error| {
        Error::invalid_data(format!("cannot grow model texture collection: {error}"))
    })
}

fn append_scene_texture_binding(
    bindings: &mut Vec<SceneTextureBinding>,
    property: &str,
    texture: usize,
    offset: [f32; 2],
    scale: [f32; 2],
) -> Result<()> {
    bindings.try_reserve(1).map_err(|error| {
        Error::invalid_data(format!("cannot grow model texture bindings: {error}"))
    })?;
    let property = copy_scene_texture_string(property, "model texture property")?;
    bindings.push(SceneTextureBinding {
        slot: TextureSlot::from_property_name(&property),
        property,
        texture,
        offset,
        scale,
    });
    Ok(())
}

struct TextureTemporaryFile {
    path: PathBuf,
    file: Option<File>,
    persisted: bool,
}

impl TextureTemporaryFile {
    fn create(directory: &Path) -> Result<Self> {
        for _ in 0..MAXIMUM_TEXTURE_TEMPORARY_ATTEMPTS {
            let sequence = TEXTURE_TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let mut name = FallibleSceneTextureString::default();
            fmt::write(
                &mut name,
                format_args!(".assetstudio-texture-{}-{sequence}.tmp", std::process::id()),
            )
            .map_err(|_| Error::invalid_data("cannot allocate texture temporary file name"))?;
            let path = join_scene_texture_path_fallibly(
                directory,
                Path::new(&name.value),
                "model texture temporary path",
            )?;
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => {
                    return Ok(Self {
                        path,
                        file: Some(file),
                        persisted: false,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
        }
        Err(Error::invalid_data(format!(
            "cannot allocate a texture temporary file after {MAXIMUM_TEXTURE_TEMPORARY_ATTEMPTS} attempts"
        )))
    }

    fn file_mut(&mut self) -> &mut File {
        self.file.as_mut().expect("texture temporary file is open")
    }

    fn close(&mut self) -> Result<()> {
        self.file
            .take()
            .ok_or_else(|| Error::invalid_data("texture temporary file was already closed"))?;
        Ok(())
    }

    fn persist_no_clobber(&mut self, destination: &Path) -> Result<bool> {
        match fs::hard_link(&self.path, destination) {
            Ok(()) => {
                // The destination is committed once the hard-link exists.
                // Keep Drop responsible for retrying a failed temporary-link
                // cleanup instead of reporting a false texture-write failure.
                self.persisted = fs::remove_file(&self.path).is_ok();
                Ok(true)
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(false),
            Err(error) => Err(error.into()),
        }
    }
}

impl Drop for TextureTemporaryFile {
    fn drop(&mut self) {
        if !self.persisted {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn record_texture_skip(
    set: &mut SceneTextureSet,
    material: SceneObjectKey,
    property: &str,
    reason: impl std::fmt::Display,
) -> Result<()> {
    let property = copy_scene_texture_string(property, "skipped texture property")?;
    let mut formatted_reason = FallibleSceneTextureString::default();
    fmt::write(&mut formatted_reason, format_args!("{reason}"))
        .map_err(|_| Error::invalid_data("cannot allocate skipped texture reason"))?;
    set.skipped.try_reserve(1).map_err(|error| {
        Error::invalid_data(format!("cannot grow skipped model textures: {error}"))
    })?;
    set.skipped.push(SceneTextureSkip {
        material,
        property,
        reason: formatted_reason.value,
    });
    Ok(())
}

fn encode_texture(
    collection: &AssetCollection,
    key: SceneObjectKey,
    format: ImageFormat,
    limits: SceneTextureLimits,
    remaining_encoded_bytes: u64,
) -> std::result::Result<(String, Vec<u8>), TextureEncodeFailure> {
    let loaded = collection
        .serialized_files
        .get(key.file_index)
        .ok_or_else(|| Error::invalid_data("texture file index is outside the collection"))
        .map_err(TextureEncodeFailure::Recoverable)?;
    let object_index = collection
        .object_index_by_path_id(key.file_index, key.path_id)
        .ok_or_else(|| Error::invalid_data("texture path ID is absent from its file"))
        .map_err(TextureEncodeFailure::Recoverable)?;
    let texture = read_texture2d(collection, &loaded.file, object_index, limits.texture)
        .map_err(TextureEncodeFailure::Recoverable)?;
    let image = texture
        .decode_mip_rgba8(0, limits.texture)
        .map_err(TextureEncodeFailure::Recoverable)?;
    let maximum_buffer_bytes = limits
        .texture
        .maximum_output_bytes
        .min(remaining_encoded_bytes);
    let maximum_buffer_bytes = usize::try_from(maximum_buffer_bytes).unwrap_or(usize::MAX);
    let mut encoded = FallibleEncodedTexture::new(maximum_buffer_bytes);
    let write_result = write_rgba_image(
        &image,
        format,
        ImageRowOrder::UnityDecoded,
        limits.texture.maximum_output_bytes,
        &mut encoded,
    );
    if encoded.limit_exceeded && remaining_encoded_bytes < limits.texture.maximum_output_bytes {
        return Err(TextureEncodeFailure::TotalBudgetExceeded);
    }
    write_result.map_err(TextureEncodeFailure::Recoverable)?;
    Ok((texture.name, encoded.bytes))
}

fn total_texture_budget_error(limits: SceneTextureLimits) -> Error {
    Error::invalid_data(format!(
        "model textures exceed the {} byte budget",
        limits.maximum_total_encoded_bytes
    ))
}

enum TextureEncodeFailure {
    Recoverable(Error),
    TotalBudgetExceeded,
}

struct FallibleEncodedTexture {
    bytes: Vec<u8>,
    maximum: usize,
    limit_exceeded: bool,
}

impl FallibleEncodedTexture {
    const fn new(maximum: usize) -> Self {
        Self {
            bytes: Vec::new(),
            maximum,
            limit_exceeded: false,
        }
    }
}

impl Write for FallibleEncodedTexture {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        let next = self
            .bytes
            .len()
            .checked_add(input.len())
            .ok_or_else(|| io::Error::other("encoded model texture length overflowed"))?;
        if next > self.maximum {
            self.limit_exceeded = true;
            return Err(io::Error::other(
                "encoded model texture exceeds its output budget",
            ));
        }
        self.bytes
            .try_reserve(input.len())
            .map_err(|_| io::Error::other("cannot allocate encoded model texture"))?;
        self.bytes.extend_from_slice(input);
        Ok(input.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Assigns each texture object one stable file name.
///
/// Held separately from a [`SceneTextureSet`] so a batch export can share one
/// allocator across every model it writes into a directory.
#[derive(Debug, Clone, Default)]
pub struct SceneTextureNames {
    by_object: HashMap<SceneObjectKey, String>,
    /// Unicode-lowercased claimed file names. The value is where the next
    /// ` (n)` search for that name should start, so a name repeated many times
    /// stays linear and names that collide on common case-insensitive file
    /// systems cannot silently overwrite one another.
    used: HashMap<String, usize>,
    retained_name_bytes: u64,
}

impl SceneTextureNames {
    /// Returns this object's file name, allocating one on first sight.
    ///
    /// The managed reader appends ` (n)` on a collision; that is kept so
    /// exports line up. Uniqueness is over the final file name, so two textures
    /// whose names differ only in characters sanitising strips still get
    /// separate files.
    fn allocate(
        &mut self,
        object: SceneObjectKey,
        name: &str,
        format: ImageFormat,
        maximum_name_index_bytes: u64,
    ) -> Result<String> {
        if self.retained_name_bytes > maximum_name_index_bytes {
            return Err(scene_texture_name_budget_error(
                self.retained_name_bytes,
                maximum_name_index_bytes,
            ));
        }
        if let Some(existing) = self.by_object.get(&object) {
            return copy_scene_texture_string(existing, "existing model texture name");
        }
        let stem = sanitize_file_stem(name)?;
        let extension = format.extension();
        let candidate = join_scene_texture_strings(
            &[&stem, extension],
            MAXIMUM_SCENE_TEXTURE_COMPONENT_BYTES,
            "model texture file name",
        )?;
        let candidate_key = portable_scene_texture_key(&candidate)?;
        let allocated = if self.used.contains_key(&candidate_key) {
            let mut next = self.used.get(&candidate_key).copied().unwrap_or(1);
            loop {
                let mut suffix = FallibleSceneTextureString::default();
                fmt::write(&mut suffix, format_args!(" ({next})"))
                    .map_err(|_| Error::invalid_data("cannot allocate texture collision suffix"))?;
                let attempt = join_scene_texture_strings(
                    &[&stem, &suffix.value, extension],
                    MAXIMUM_SCENE_TEXTURE_COMPONENT_BYTES,
                    "colliding model texture file name",
                )?;
                let attempt_key = portable_scene_texture_key(&attempt)?;
                next = next
                    .checked_add(1)
                    .ok_or_else(|| Error::invalid_data("texture collision counter overflowed"))?;
                if !self.used.contains_key(&attempt_key) {
                    let retained_name_bytes = charge_scene_texture_name_bytes(
                        self.retained_name_bytes,
                        attempt_key.len(),
                        attempt.len(),
                        maximum_name_index_bytes,
                    )?;
                    let object_name =
                        copy_scene_texture_string(&attempt, "model texture object name")?;
                    self.used.try_reserve(1).map_err(|error| {
                        Error::invalid_data(format!(
                            "cannot grow used model texture names: {error}"
                        ))
                    })?;
                    self.by_object.try_reserve(1).map_err(|error| {
                        Error::invalid_data(format!(
                            "cannot grow model texture object names: {error}"
                        ))
                    })?;
                    let base_cursor = self.used.get_mut(&candidate_key).ok_or_else(|| {
                        Error::invalid_data("model texture collision base disappeared")
                    })?;
                    *base_cursor = next;
                    self.used.insert(attempt_key, 1);
                    self.by_object.insert(object, object_name);
                    self.retained_name_bytes = retained_name_bytes;
                    break attempt;
                }
            }
        } else {
            let retained_name_bytes = charge_scene_texture_name_bytes(
                self.retained_name_bytes,
                candidate_key.len(),
                candidate.len(),
                maximum_name_index_bytes,
            )?;
            let object_name = copy_scene_texture_string(&candidate, "model texture object name")?;
            self.used.try_reserve(1).map_err(|error| {
                Error::invalid_data(format!("cannot grow used model texture names: {error}"))
            })?;
            self.by_object.try_reserve(1).map_err(|error| {
                Error::invalid_data(format!("cannot grow model texture object names: {error}"))
            })?;
            self.used.insert(candidate_key, 1);
            self.by_object.insert(object, object_name);
            self.retained_name_bytes = retained_name_bytes;
            candidate
        };
        Ok(allocated)
    }
}

fn portable_scene_texture_key(value: &str) -> Result<String> {
    let length = value.chars().try_fold(0_usize, |length, character| {
        character
            .to_lowercase()
            .try_fold(length, |length, lowercase| {
                length
                    .checked_add(lowercase.len_utf8())
                    .ok_or_else(|| Error::invalid_data("model texture name key length overflowed"))
            })
    })?;
    let mut key = String::new();
    key.try_reserve_exact(length).map_err(|error| {
        Error::invalid_data(format!(
            "cannot allocate case-folded model texture name: {error}"
        ))
    })?;
    for character in value.chars().flat_map(char::to_lowercase) {
        key.push(character);
    }
    debug_assert_eq!(key.len(), length);
    Ok(key)
}

fn charge_scene_texture_name_bytes(
    current: u64,
    key_bytes: usize,
    object_name_bytes: usize,
    maximum: u64,
) -> Result<u64> {
    let additional = key_bytes
        .checked_add(object_name_bytes)
        .ok_or_else(|| Error::invalid_data("model texture name byte count overflowed"))?;
    let additional = u64::try_from(additional)
        .map_err(|_| Error::invalid_data("model texture name bytes do not fit in u64"))?;
    let next = current
        .checked_add(additional)
        .ok_or_else(|| Error::invalid_data("model texture name byte budget overflowed"))?;
    if next > maximum {
        return Err(scene_texture_name_budget_error(next, maximum));
    }
    Ok(next)
}

fn scene_texture_name_budget_error(requested: u64, maximum: u64) -> Error {
    Error::invalid_data(format!(
        "model texture name indexes require {requested} UTF-8 bytes, exceeding limit {maximum}"
    ))
}

/// Keeps a name usable as a single file-name component.
///
/// Separators, the reserved characters Windows rejects, control characters and
/// a leading run of dots are replaced, and the result is truncated to a length
/// every common file system accepts. An empty result becomes `Texture`.
fn sanitize_file_stem(name: &str) -> Result<String> {
    const MAXIMUM_STEM_BYTES: usize = 120;

    let mut stem = String::new();
    for character in name.chars() {
        let safe = match character {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            character if character.is_control() => '_',
            character => character,
        };
        if stem.len() + safe.len_utf8() > MAXIMUM_STEM_BYTES {
            break;
        }
        stem.try_reserve_exact(safe.len_utf8()).map_err(|error| {
            Error::invalid_data(format!("cannot allocate model texture stem: {error}"))
        })?;
        stem.push(safe);
    }
    let trimmed =
        stem.trim_matches(|character: char| character == '.' || character.is_whitespace());
    let mut output = if trimmed.is_empty() {
        copy_scene_texture_string("Texture", "fallback model texture stem")?
    } else {
        copy_scene_texture_string(trimmed, "model texture stem")?
    };
    if is_windows_reserved_texture_name(&output) {
        while output.len() >= MAXIMUM_STEM_BYTES {
            output.pop();
        }
        let mut prefixed = String::new();
        prefixed
            .try_reserve_exact(output.len() + 1)
            .map_err(|error| {
                Error::invalid_data(format!(
                    "cannot allocate reserved model texture stem: {error}"
                ))
            })?;
        prefixed.push('_');
        prefixed.push_str(&output);
        output = prefixed;
    }
    Ok(output)
}

fn is_windows_reserved_texture_name(value: &str) -> bool {
    let base = value.split('.').next().unwrap_or(value).as_bytes();
    let equals = |expected: &[u8]| base.eq_ignore_ascii_case(expected);
    equals(b"CON")
        || equals(b"PRN")
        || equals(b"AUX")
        || equals(b"NUL")
        || (base.len() == 4
            && (base[..3].eq_ignore_ascii_case(b"COM") || base[..3].eq_ignore_ascii_case(b"LPT"))
            && matches!(base[3], b'1'..=b'9'))
}

fn validate_texture_file_name(value: &str) -> Result<()> {
    if value.is_empty() {
        return Err(Error::invalid_data("model texture file name is empty"));
    }
    if value.len() > MAXIMUM_SCENE_TEXTURE_COMPONENT_BYTES {
        return Err(Error::invalid_data(format!(
            "model texture file name is {} bytes, exceeding portable limit {MAXIMUM_SCENE_TEXTURE_COMPONENT_BYTES}",
            value.len()
        )));
    }
    if value.ends_with([' ', '.'])
        || value.chars().any(|character| {
            character.is_control()
                || matches!(
                    character,
                    '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'
                )
        })
        || is_windows_reserved_texture_name(value)
    {
        return Err(Error::invalid_data(
            "model texture file name is not a portable single component",
        ));
    }
    let mut components = Path::new(value).components();
    if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
        return Err(Error::invalid_data(
            "model texture file name is not a single relative component",
        ));
    }
    Ok(())
}

fn copy_scene_texture_string(value: &str, label: &str) -> Result<String> {
    let mut copy = String::new();
    copy.try_reserve_exact(value.len())
        .map_err(|error| Error::invalid_data(format!("cannot allocate {label}: {error}")))?;
    copy.push_str(value);
    Ok(copy)
}

fn join_scene_texture_strings(
    values: &[&str],
    maximum_bytes: usize,
    label: &str,
) -> Result<String> {
    let length = values.iter().try_fold(0_usize, |length, value| {
        length
            .checked_add(value.len())
            .ok_or_else(|| Error::invalid_data(format!("{label} length overflowed")))
    })?;
    if length > maximum_bytes {
        return Err(Error::invalid_data(format!(
            "{label} is {length} bytes, exceeding limit {maximum_bytes}"
        )));
    }
    let mut output = String::new();
    output
        .try_reserve_exact(length)
        .map_err(|error| Error::invalid_data(format!("cannot allocate {label}: {error}")))?;
    for value in values {
        output.push_str(value);
    }
    Ok(output)
}

fn join_scene_texture_path_fallibly(parent: &Path, child: &Path, label: &str) -> Result<PathBuf> {
    if child.is_absolute() {
        return Err(Error::invalid_data(format!("{label} child is absolute")));
    }
    let separator = usize::from(!parent.as_os_str().is_empty() && !child.as_os_str().is_empty());
    let length = parent
        .as_os_str()
        .as_encoded_bytes()
        .len()
        .checked_add(separator)
        .and_then(|length| length.checked_add(child.as_os_str().as_encoded_bytes().len()))
        .ok_or_else(|| Error::invalid_data(format!("{label} length overflowed")))?;
    let mut path = PathBuf::new();
    path.try_reserve_exact(length)
        .map_err(|error| Error::invalid_data(format!("cannot allocate {label}: {error}")))?;
    path.push(parent);
    if !child.as_os_str().is_empty() {
        path.push(child);
    }
    if path.as_os_str().as_encoded_bytes().len() > length {
        return Err(Error::invalid_data(format!(
            "{label} exceeded its checked allocation"
        )));
    }
    Ok(path)
}

#[derive(Default)]
struct FallibleSceneTextureString {
    value: String,
}

impl fmt::Write for FallibleSceneTextureString {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        self.value
            .try_reserve(value.len())
            .map_err(|_| fmt::Error)?;
        self.value.push_str(value);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AssetCollection, ImageFormat, ModelIr, SceneObjectKey, SceneTexture, SceneTextureLimits,
        SceneTextureNames, SceneTextureSet, TEXTURE_2D_CLASS_ID, TextureSlot, TextureTemporaryFile,
        reserve_scene_textures, sanitize_file_stem,
    };
    use std::io::Write as _;

    const fn object(path_id: i64) -> SceneObjectKey {
        SceneObjectKey {
            file_index: 0,
            path_id,
        }
    }

    /// A collection holding one 1x1 RGBA `Texture2D`.
    fn texture_collection(name: &str) -> AssetCollection {
        use crate::loader::LoadedSerializedFile;
        use crate::serialized::SerializedFile;
        use crate::source::Region;

        let bytes = synthetic_v22(&[(
            TEXTURE_2D_CLASS_ID,
            81,
            texture_object(name, &[0x10, 0x20, 0x30, 0xFF]),
        )]);
        AssetCollection::from_loaded_parts(
            vec![LoadedSerializedFile {
                path: "textures.assets".to_owned(),
                file: SerializedFile::open(Region::from_bytes(bytes)).unwrap(),
            }],
            Vec::new(),
        )
    }

    /// A model with one material binding `property` to path ID 81.
    fn model_with_texture_property(property: &str, path_id: i64) -> ModelIr {
        use crate::material::{
            Material, MaterialPropertySheet, MaterialTextureEnvironment, NamedMaterialProperty,
        };
        use crate::model_ir::ModelMaterial;
        use crate::serialized::ObjectReference;

        let material = Material {
            path_id: 61,
            name: "mat".to_owned(),
            shader: ObjectReference {
                file_id: 0,
                path_id: 0,
            },
            legacy_shader_keywords: Vec::new(),
            valid_keywords: Vec::new(),
            invalid_keywords: Vec::new(),
            lightmap_flags: None,
            enable_instancing_variants: None,
            custom_render_queue: None,
            string_tags: Vec::new(),
            disabled_shader_passes: Vec::new(),
            saved_properties: MaterialPropertySheet {
                texture_environments: vec![NamedMaterialProperty {
                    name: property.to_owned(),
                    value: MaterialTextureEnvironment {
                        texture: ObjectReference {
                            file_id: 0,
                            path_id,
                        },
                        scale: [1.0, 2.0],
                        offset: [0.5, 0.25],
                    },
                }],
                ..MaterialPropertySheet::default()
            },
            trailing_bytes: 0,
        };
        ModelIr::from_test_parts(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            vec![ModelMaterial {
                object: object(61),
                material,
            }],
        )
    }

    #[test]
    fn resolves_encodes_and_binds_a_material_texture() {
        let collection = texture_collection("Body");
        let model = model_with_texture_property("_MainTex", 81);
        let set = SceneTextureSet::from_model(
            &collection,
            &model,
            ImageFormat::Png,
            SceneTextureLimits::default(),
        )
        .unwrap();

        assert_eq!(set.textures.len(), 1);
        assert_eq!(set.textures[0].file_name, "Body.png");
        assert_eq!(set.textures[0].object.path_id, 81);
        assert!(
            set.textures[0].encoded.starts_with(b"\x89PNG\r\n\x1a\n"),
            "the payload is not a PNG"
        );
        assert!(set.skipped.is_empty());

        let bindings = set.bindings_for(object(61));
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].property, "_MainTex");
        assert_eq!(bindings[0].slot, Some(TextureSlot::Diffuse));
        // The TexEnv offset and scale come straight through, so an exact
        // comparison is the right one: nothing arithmetic happens to them.
        #[allow(clippy::float_cmp)]
        {
            assert_eq!(bindings[0].offset, [0.5, 0.25]);
            assert_eq!(bindings[0].scale, [1.0, 2.0]);
        }
    }

    #[test]
    fn records_a_reference_it_cannot_resolve_rather_than_failing() {
        // Path ID 99 is not in the file. One bad reference must not cost the
        // model its other textures, so it is reported and the export goes on.
        let collection = texture_collection("Body");
        let model = model_with_texture_property("_MainTex", 99);
        let set = SceneTextureSet::from_model(
            &collection,
            &model,
            ImageFormat::Png,
            SceneTextureLimits::default(),
        )
        .unwrap();
        assert!(set.textures.is_empty());
        assert_eq!(set.skipped.len(), 1);
        assert_eq!(set.skipped[0].property, "_MainTex");
        assert!(set.bindings_for(object(61)).is_empty());
    }

    #[test]
    fn rejects_an_exhausted_texture_count_before_decoding() {
        let mut collection = texture_collection("Body");
        // If the count were charged after decoding, this deliberately corrupt
        // object would be recorded as a skipped texture and the call would
        // incorrectly succeed even though the caller allowed zero textures.
        collection.serialized_files[0].file.objects[0].byte_size = 1;
        let model = model_with_texture_property("_MainTex", 81);
        let error = SceneTextureSet::from_model(
            &collection,
            &model,
            ImageFormat::Png,
            SceneTextureLimits {
                maximum_textures: 0,
                ..SceneTextureLimits::default()
            },
        )
        .unwrap_err();

        assert!(error.to_string().contains("more than 0 textures"));
    }

    #[test]
    fn bounds_repeated_and_skipped_texture_references_independently() {
        const REPEATED: usize = 16_384;

        let collection = texture_collection("Body");
        for path_id in [81, 99] {
            let mut model = model_with_texture_property("_MainTex", path_id);
            let duplicate = model.materials[0]
                .material
                .saved_properties
                .texture_environments[0]
                .clone();
            let environments = &mut model.materials[0]
                .material
                .saved_properties
                .texture_environments;
            environments.try_reserve_exact(REPEATED - 1).unwrap();
            for _ in 1..REPEATED {
                environments.push(duplicate.clone());
            }

            let error = SceneTextureSet::from_model(
                &collection,
                &model,
                ImageFormat::Png,
                SceneTextureLimits {
                    maximum_texture_references: REPEATED - 1,
                    maximum_textures: 1,
                    ..SceneTextureLimits::default()
                },
            )
            .unwrap_err();

            assert!(
                error
                    .to_string()
                    .contains("more than 16383 non-null texture references"),
                "{error}"
            );
        }
    }

    #[test]
    fn rejects_an_exhausted_total_budget_before_decoding() {
        let mut collection = texture_collection("Body");
        collection.serialized_files[0].file.objects[0].byte_size = 1;
        let model = model_with_texture_property("_MainTex", 81);
        let error = SceneTextureSet::from_model(
            &collection,
            &model,
            ImageFormat::Png,
            SceneTextureLimits {
                maximum_total_encoded_bytes: 0,
                ..SceneTextureLimits::default()
            },
        )
        .unwrap_err();

        assert!(error.to_string().contains("0 byte budget"));
    }

    #[test]
    fn public_model_reader_applies_the_shared_name_index_budget() {
        let collection = texture_collection("Body");
        let model = model_with_texture_property("_MainTex", 81);
        let error = SceneTextureSet::from_model(
            &collection,
            &model,
            ImageFormat::RawRgba,
            SceneTextureLimits {
                maximum_name_index_bytes: 17,
                ..SceneTextureLimits::default()
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("require 18 UTF-8 bytes"));

        let set = SceneTextureSet::from_model(
            &collection,
            &model,
            ImageFormat::RawRgba,
            SceneTextureLimits {
                maximum_name_index_bytes: 18,
                ..SceneTextureLimits::default()
            },
        )
        .unwrap();
        assert_eq!(set.textures[0].file_name, "Body.rgba");
    }

    #[test]
    fn manual_texture_growth_reports_allocation_failure_transactionally() {
        let mut textures = Vec::new();
        let original_capacity = textures.capacity();
        let error = reserve_scene_textures(&mut textures, usize::MAX).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("cannot grow model texture collection")
        );
        assert!(textures.is_empty());
        assert_eq!(textures.capacity(), original_capacity);

        let mut set = SceneTextureSet::default();
        let index = set
            .push_texture(SceneTexture {
                file_name: "Body.png".to_owned(),
                object: object(81),
                encoded: Vec::new(),
            })
            .unwrap();
        assert_eq!(index, 0);
        assert_eq!(set.textures.len(), 1);
    }

    #[test]
    fn writes_files_without_clobbering_what_is_already_there() {
        let collection = texture_collection("Body");
        let model = model_with_texture_property("_MainTex", 81);
        let set = SceneTextureSet::from_model(
            &collection,
            &model,
            ImageFormat::Png,
            SceneTextureLimits::default(),
        )
        .unwrap();

        let directory =
            std::env::temp_dir().join(format!("assetstudio-scene-textures-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let written = set.write_to_directory(&directory).unwrap();
        assert_eq!(written.len(), 1);
        assert_eq!(written[0].file_name().unwrap(), "Body.png");
        assert_eq!(std::fs::read(&written[0]).unwrap(), set.textures[0].encoded);

        // A second export into the same directory leaves the file alone.
        assert!(set.write_to_directory(&directory).unwrap().is_empty());
        assert!(std::fs::read_dir(&directory).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".assetstudio-texture-")
        }));
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn removes_an_abandoned_texture_temporary_file() {
        let directory = std::env::temp_dir().join(format!(
            "assetstudio-scene-texture-abort-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir(&directory).unwrap();
        let temporary_path;
        {
            let mut temporary = TextureTemporaryFile::create(&directory).unwrap();
            temporary.file_mut().write_all(b"incomplete").unwrap();
            temporary_path = temporary.path.clone();
            assert!(temporary_path.exists());
        }
        assert!(!temporary_path.exists());
        std::fs::remove_dir(&directory).unwrap();
    }

    #[test]
    fn write_rejects_non_portable_names_before_creating_a_temporary_file() {
        let directory = std::env::temp_dir().join(format!(
            "assetstudio-scene-texture-traversal-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir(&directory).unwrap();
        let escaped = directory.parent().unwrap().join("escaped.png");
        let _ = std::fs::remove_file(&escaped);
        for file_name in [
            "../escaped.png".to_owned(),
            "/absolute.png".to_owned(),
            "nested/texture.png".to_owned(),
            "CON.png".to_owned(),
            "x".repeat(241),
        ] {
            let mut set = SceneTextureSet::default();
            set.push_texture(SceneTexture {
                file_name,
                object: object(1),
                encoded: b"not written".to_vec(),
            })
            .unwrap();

            assert!(set.write_to_directory(&directory).is_err());
            assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 0);
        }
        assert!(!escaped.exists());
        std::fs::remove_dir(&directory).unwrap();
    }

    #[test]
    fn texture_batch_failure_rolls_back_files_published_by_the_same_call() {
        let directory = std::env::temp_dir().join(format!(
            "assetstudio-scene-texture-rollback-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir(&directory).unwrap();
        let existing = directory.join("existing.png");
        std::fs::write(&existing, b"keep me").unwrap();

        let mut set = SceneTextureSet::default();
        set.push_texture(SceneTexture {
            file_name: "first.png".to_owned(),
            object: object(1),
            encoded: b"published before the later failure".to_vec(),
        })
        .unwrap();
        set.push_texture(SceneTexture {
            file_name: "existing.png".to_owned(),
            object: object(2),
            encoded: b"must not replace the existing file".to_vec(),
        })
        .unwrap();
        set.push_texture(SceneTexture {
            file_name: "../invalid.png".to_owned(),
            object: object(3),
            encoded: b"must not be written".to_vec(),
        })
        .unwrap();

        let error = set.write_to_directory(&directory).unwrap_err();
        assert!(error.to_string().contains("portable"), "{error}");
        assert!(!directory.join("first.png").exists());
        assert_eq!(std::fs::read(&existing).unwrap(), b"keep me");
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 1);
        std::fs::remove_dir_all(&directory).unwrap();
    }

    /// A 1x1 RGBA32 `Texture2D` with inline pixel data.
    fn texture_object(name: &str, data: &[u8]) -> Vec<u8> {
        let mut output = Vec::new();
        push_aligned_string(&mut output, name);
        output.extend_from_slice(&0_i32.to_le_bytes());
        output.push(0);
        output.push(0);
        align(&mut output, 4);
        output.extend_from_slice(&1_i32.to_le_bytes());
        output.extend_from_slice(&1_i32.to_le_bytes());
        output.extend_from_slice(&u32::try_from(data.len()).unwrap().to_le_bytes());
        output.extend_from_slice(&0_i32.to_le_bytes());
        output.extend_from_slice(&crate::texture::TextureFormat::RGBA32.0.to_le_bytes());
        output.extend_from_slice(&1_i32.to_le_bytes());
        output.extend_from_slice(&[0, 0, 0]);
        align(&mut output, 4);
        push_aligned_string(&mut output, "");
        output.push(0);
        align(&mut output, 4);
        for value in [0_i32, 1, 2] {
            output.extend_from_slice(&value.to_le_bytes());
        }
        output.extend_from_slice(&[0_u8; 24]);
        for _ in 0..3 {
            output.extend_from_slice(&0_i32.to_le_bytes());
        }
        align(&mut output, 4);
        output.extend_from_slice(&i32::try_from(data.len()).unwrap().to_le_bytes());
        output.extend_from_slice(data);
        output
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

    #[test]
    fn maps_the_shader_properties_the_managed_reader_recognises() {
        assert_eq!(
            TextureSlot::from_property_name("_MainTex"),
            Some(TextureSlot::Diffuse)
        );
        assert_eq!(
            TextureSlot::from_property_name("_BumpMap"),
            Some(TextureSlot::Bump)
        );
        assert_eq!(
            TextureSlot::from_property_name("_SpecularTex"),
            Some(TextureSlot::Specular)
        );
        assert_eq!(
            TextureSlot::from_property_name("_NormalTex"),
            Some(TextureSlot::NormalMap)
        );
        assert_eq!(TextureSlot::from_property_name("_EmissionMap"), None);
        // The exact matches win over the substring rules.
        assert_eq!(
            TextureSlot::from_property_name("_BumpMap").map(TextureSlot::fbx_property),
            Some("Bump")
        );
    }

    #[test]
    fn reduces_hostile_names_to_one_path_component() {
        // The traversal case only has to end up as one harmless component.
        for hostile in ["../../etc/passwd", "..\\..\\Windows", "/etc/passwd", "."] {
            let stem = sanitize_file_stem(hostile).unwrap();
            assert!(
                !stem.contains('/') && !stem.contains('\\'),
                "{stem:?} still contains a separator"
            );
            assert!(stem != "." && stem != "..", "{stem:?} is a directory link");
            assert!(!stem.starts_with('.'), "{stem:?} starts with a dot");
        }
        assert_eq!(
            sanitize_file_stem("C:\\Windows\\system32").unwrap(),
            "C__Windows_system32"
        );
        assert_eq!(sanitize_file_stem("   ").unwrap(), "Texture");
        assert_eq!(sanitize_file_stem("...").unwrap(), "Texture");
        assert_eq!(sanitize_file_stem("a\u{0}b").unwrap(), "a_b");
        assert_eq!(sanitize_file_stem("CON").unwrap(), "_CON");
        assert!(sanitize_file_stem(&"x".repeat(400)).unwrap().len() <= 120);
    }

    #[test]
    fn gives_colliding_names_distinct_files() {
        let mut names = SceneTextureNames::default();
        assert_eq!(
            names
                .allocate(object(1), "Body", ImageFormat::Png, u64::MAX)
                .unwrap(),
            "Body.png"
        );
        assert_eq!(
            names
                .allocate(object(2), "Body", ImageFormat::Png, u64::MAX)
                .unwrap(),
            "Body (1).png"
        );
        assert_eq!(
            names
                .allocate(object(3), "Body", ImageFormat::Png, u64::MAX)
                .unwrap(),
            "Body (2).png"
        );
        // A name that only collides after sanitising still gets its own file.
        assert_eq!(
            names
                .allocate(object(4), "Body/", ImageFormat::Png, u64::MAX)
                .unwrap(),
            "Body_.png"
        );
    }

    #[test]
    fn keeps_one_object_on_one_file_name_across_models() {
        // A batch export shares the allocator, so the same texture reached from
        // a second model must not claim a second file.
        let mut names = SceneTextureNames::default();
        assert_eq!(
            names
                .allocate(object(7), "Body", ImageFormat::Png, u64::MAX)
                .unwrap(),
            "Body.png"
        );
        assert_eq!(
            names
                .allocate(object(7), "Body", ImageFormat::Png, u64::MAX)
                .unwrap(),
            "Body.png"
        );
        // A different object with the same Unity name still gets its own file.
        assert_eq!(
            names
                .allocate(object(8), "Body", ImageFormat::Png, u64::MAX)
                .unwrap(),
            "Body (1).png"
        );
    }

    #[test]
    fn case_folds_and_bounds_shared_name_indexes_transactionally() {
        let mut names = SceneTextureNames::default();
        assert_eq!(
            names
                .allocate(object(1), "Body", ImageFormat::Png, 16)
                .unwrap(),
            "Body.png"
        );
        assert_eq!(names.retained_name_bytes, 16);

        // The second name differs only by case. It must get a distinct file on
        // Windows and default macOS volumes, and its folded key plus object
        // mapping need another 24 retained bytes.
        let error = names
            .allocate(object(2), "body", ImageFormat::Png, 39)
            .unwrap_err();
        assert!(error.to_string().contains("require 40 UTF-8 bytes"));
        assert_eq!(names.retained_name_bytes, 16);
        assert_eq!(names.used.len(), 1);
        assert_eq!(names.by_object.len(), 1);
        assert_eq!(names.used.get("body.png"), Some(&1));

        assert_eq!(
            names
                .allocate(object(2), "body", ImageFormat::Png, 40)
                .unwrap(),
            "body (1).png"
        );
        assert_eq!(names.retained_name_bytes, 40);
        assert_eq!(names.used.len(), 2);
        assert_eq!(names.by_object.len(), 2);

        // Unicode lowercasing can expand: `İ.png` is six source bytes but its
        // retained lowercase key is seven bytes. Reject it before insertion
        // when the exact two-copy index budget is one byte short.
        let mut unicode = SceneTextureNames::default();
        let error = unicode
            .allocate(object(3), "İ", ImageFormat::Png, 12)
            .unwrap_err();
        assert!(error.to_string().contains("require 13 UTF-8 bytes"));
        assert_eq!(unicode.retained_name_bytes, 0);
        assert!(unicode.used.is_empty());
        assert!(unicode.by_object.is_empty());
    }
}
