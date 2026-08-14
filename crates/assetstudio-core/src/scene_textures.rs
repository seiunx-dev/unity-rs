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

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::image_export::{ImageFormat, ImageRowOrder, write_rgba_image};
use crate::loader::AssetCollection;
use crate::model_ir::ModelIr;
use crate::scene::resolve_object_reference;
use crate::scene_hierarchy::SceneObjectKey;
use crate::texture::{TEXTURE_2D_CLASS_ID, TextureReadLimits, read_texture2d};
use crate::{Error, Result};

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
    pub maximum_textures: usize,
    /// Total encoded bytes across every texture in the set.
    pub maximum_total_encoded_bytes: u64,
    pub texture: TextureReadLimits,
}

impl Default for SceneTextureLimits {
    fn default() -> Self {
        Self {
            maximum_textures: 4_096,
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
    bindings: BTreeMap<SceneObjectKey, Vec<SceneTextureBinding>>,
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
    /// Exceeding a limit is still an error, because that is a bound the caller
    /// chose.
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
        let mut by_object: BTreeMap<SceneObjectKey, usize> = BTreeMap::new();
        let mut total_encoded = 0_u64;

        for material in &model.materials {
            let mut bindings = Vec::new();
            for property in &material.material.saved_properties.texture_environments {
                let environment = &property.value;
                if environment.texture.is_null() {
                    continue;
                }
                let resolved = match resolve_object_reference(
                    collection,
                    material.object.file_index,
                    environment.texture,
                ) {
                    Ok(Some(resolved)) => resolved,
                    Ok(None) => continue,
                    Err(error) => {
                        set.skipped.push(SceneTextureSkip {
                            material: material.object,
                            property: property.name.clone(),
                            reason: error.to_string(),
                        });
                        continue;
                    }
                };
                if resolved.object.class_id != TEXTURE_2D_CLASS_ID {
                    set.skipped.push(SceneTextureSkip {
                        material: material.object,
                        property: property.name.clone(),
                        reason: format!("class ID {} is not Texture2D", resolved.object.class_id),
                    });
                    continue;
                }
                let key = SceneObjectKey {
                    file_index: resolved.file_index,
                    path_id: resolved.object.path_id,
                };
                let texture = match by_object.get(&key) {
                    Some(index) => *index,
                    None => match encode_texture(collection, key, format, limits) {
                        Ok((name, encoded)) => {
                            if set.textures.len() == limits.maximum_textures {
                                return Err(Error::invalid_data(format!(
                                    "model references more than {} textures",
                                    limits.maximum_textures
                                )));
                            }
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
                            let file_name = names.allocate(key, &name, format);
                            let index = set.textures.len();
                            set.textures.push(SceneTexture {
                                file_name,
                                object: key,
                                encoded,
                            });
                            by_object.insert(key, index);
                            index
                        }
                        Err(error) => {
                            set.skipped.push(SceneTextureSkip {
                                material: material.object,
                                property: property.name.clone(),
                                reason: error.to_string(),
                            });
                            continue;
                        }
                    },
                };
                bindings.push(SceneTextureBinding {
                    property: property.name.clone(),
                    texture,
                    slot: TextureSlot::from_property_name(&property.name),
                    offset: environment.offset,
                    scale: environment.scale,
                });
            }
            if !bindings.is_empty() {
                set.bindings.insert(material.object, bindings);
            }
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
    /// For callers that decoded the image themselves; [`Self::from_model`]
    /// covers resolving them from the collection. The file name is taken as
    /// given, so a caller supplying an asset-derived name must reduce it to a
    /// single path component first.
    pub fn push_texture(&mut self, texture: SceneTexture) -> usize {
        let index = self.textures.len();
        self.textures.push(texture);
        index
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
        self.bindings.entry(material).or_default().push(binding);
        Ok(())
    }

    /// Writes every texture into `directory`, returning the paths written.
    ///
    /// Existing files are left alone rather than overwritten, so exporting two
    /// models that share a texture into the same directory does not rewrite it
    /// and cannot clobber an unrelated file that happens to share the name.
    pub fn write_to_directory(&self, directory: &Path) -> Result<Vec<PathBuf>> {
        let mut written = Vec::new();
        for texture in &self.textures {
            let path = directory.join(&texture.file_name);
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut file) => {
                    use std::io::Write;
                    file.write_all(&texture.encoded)?;
                    file.flush()?;
                    written.push(path);
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(written)
    }
}

fn encode_texture(
    collection: &AssetCollection,
    key: SceneObjectKey,
    format: ImageFormat,
    limits: SceneTextureLimits,
) -> Result<(String, Vec<u8>)> {
    let loaded = collection
        .serialized_files
        .get(key.file_index)
        .ok_or_else(|| Error::invalid_data("texture file index is outside the collection"))?;
    let object_index = loaded
        .file
        .objects
        .iter()
        .position(|object| object.path_id == key.path_id)
        .ok_or_else(|| Error::invalid_data("texture path ID is absent from its file"))?;
    let texture = read_texture2d(collection, &loaded.file, object_index, limits.texture)?;
    let image = texture.decode_mip_rgba8(0, limits.texture)?;
    let mut encoded = Vec::new();
    write_rgba_image(
        &image,
        format,
        ImageRowOrder::UnityDecoded,
        limits.texture.maximum_output_bytes,
        &mut encoded,
    )?;
    Ok((texture.name, encoded))
}

/// Assigns each texture object one stable file name.
///
/// Held separately from a [`SceneTextureSet`] so a batch export can share one
/// allocator across every model it writes into a directory.
#[derive(Debug, Clone, Default)]
pub struct SceneTextureNames {
    by_object: BTreeMap<SceneObjectKey, String>,
    /// Claimed file names. The value is where the next ` (n)` search for that
    /// name should start, so a name repeated many times stays linear.
    used: BTreeMap<String, usize>,
}

impl SceneTextureNames {
    /// Returns this object's file name, allocating one on first sight.
    ///
    /// The managed reader appends ` (n)` on a collision; that is kept so
    /// exports line up. Uniqueness is over the final file name, so two textures
    /// whose names differ only in characters sanitising strips still get
    /// separate files.
    fn allocate(&mut self, object: SceneObjectKey, name: &str, format: ImageFormat) -> String {
        if let Some(existing) = self.by_object.get(&object) {
            return existing.clone();
        }
        let stem = sanitize_file_stem(name);
        let extension = format.extension();
        let candidate = format!("{stem}{extension}");
        let allocated = if self.used.contains_key(&candidate) {
            let mut next = self.used.get(&candidate).copied().unwrap_or(1);
            loop {
                let attempt = format!("{stem} ({next}){extension}");
                next += 1;
                if !self.used.contains_key(&attempt) {
                    self.used.insert(candidate, next);
                    break attempt;
                }
            }
        } else {
            candidate
        };
        self.used.insert(allocated.clone(), 1);
        self.by_object.insert(object, allocated.clone());
        allocated
    }
}

/// Keeps a name usable as a single file-name component.
///
/// Separators, the reserved characters Windows rejects, control characters and
/// a leading run of dots are replaced, and the result is truncated to a length
/// every common file system accepts. An empty result becomes `Texture`.
fn sanitize_file_stem(name: &str) -> String {
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
        stem.push(safe);
    }
    let trimmed =
        stem.trim_matches(|character: char| character == '.' || character.is_whitespace());
    if trimmed.is_empty() {
        "Texture".to_owned()
    } else {
        trimmed.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AssetCollection, ImageFormat, ModelIr, SceneObjectKey, SceneTextureLimits,
        SceneTextureNames, SceneTextureSet, TEXTURE_2D_CLASS_ID, TextureSlot, sanitize_file_stem,
    };

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
            let stem = sanitize_file_stem(hostile);
            assert!(
                !stem.contains('/') && !stem.contains('\\'),
                "{stem:?} still contains a separator"
            );
            assert!(stem != "." && stem != "..", "{stem:?} is a directory link");
            assert!(!stem.starts_with('.'), "{stem:?} starts with a dot");
        }
        assert_eq!(
            sanitize_file_stem("C:\\Windows\\system32"),
            "C__Windows_system32"
        );
        assert_eq!(sanitize_file_stem("   "), "Texture");
        assert_eq!(sanitize_file_stem("..."), "Texture");
        assert_eq!(sanitize_file_stem("a\u{0}b"), "a_b");
        assert!(sanitize_file_stem(&"x".repeat(400)).len() <= 120);
    }

    #[test]
    fn gives_colliding_names_distinct_files() {
        let mut names = SceneTextureNames::default();
        assert_eq!(
            names.allocate(object(1), "Body", ImageFormat::Png),
            "Body.png"
        );
        assert_eq!(
            names.allocate(object(2), "Body", ImageFormat::Png),
            "Body (1).png"
        );
        assert_eq!(
            names.allocate(object(3), "Body", ImageFormat::Png),
            "Body (2).png"
        );
        // A name that only collides after sanitising still gets its own file.
        assert_eq!(
            names.allocate(object(4), "Body/", ImageFormat::Png),
            "Body_.png"
        );
    }

    #[test]
    fn keeps_one_object_on_one_file_name_across_models() {
        // A batch export shares the allocator, so the same texture reached from
        // a second model must not claim a second file.
        let mut names = SceneTextureNames::default();
        assert_eq!(
            names.allocate(object(7), "Body", ImageFormat::Png),
            "Body.png"
        );
        assert_eq!(
            names.allocate(object(7), "Body", ImageFormat::Png),
            "Body.png"
        );
        // A different object with the same Unity name still gets its own file.
        assert_eq!(
            names.allocate(object(8), "Body", ImageFormat::Png),
            "Body (1).png"
        );
    }
}
