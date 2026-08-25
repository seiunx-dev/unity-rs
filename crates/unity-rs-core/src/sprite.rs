use crate::endian::{Endian, EndianReader, checked_length};
use crate::loader::AssetCollection;
use crate::mesh::vertex_format_size;
use crate::serialized::SerializedFile;
use crate::source::{Region, RegionCursor};
use crate::sprite_atlas::{
    SPRITE_ATLAS_CLASS_ID, SpriteAtlasData, SpriteAtlasReadLimits, read_sprite_atlas,
};
use crate::texture::{RgbaImage, TEXTURE_2D_CLASS_ID, TextureReadLimits, read_texture2d};
use crate::{Error, Result};

pub const SPRITE_CLASS_ID: i32 = 213;

const NO_TARGET_PLATFORM: i32 = -2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpriteReadLimits {
    pub maximum_string_bytes: usize,
    pub maximum_total_string_bytes: usize,
    pub maximum_array_elements: usize,
    pub maximum_mesh_bytes: u64,
    pub maximum_output_pixels: u64,
    pub maximum_output_bytes: u64,
    pub maximum_working_bytes: u64,
    pub maximum_raster_operations: u64,
}

impl Default for SpriteReadLimits {
    fn default() -> Self {
        Self {
            maximum_string_bytes: 16 * 1024 * 1024,
            maximum_total_string_bytes: 32 * 1024 * 1024,
            maximum_array_elements: 1_000_000,
            maximum_mesh_bytes: 512 * 1024 * 1024,
            maximum_output_pixels: 128 * 1024 * 1024,
            maximum_output_bytes: 512 * 1024 * 1024,
            maximum_working_bytes: 512 * 1024 * 1024,
            // A tight sprite's cost is triangles times bounding-box area, and
            // ordinary content lands just the wrong side of half a billion: a
            // lane-skin sprite in a shipping Unity 6000.3 build needs
            // 536,921,219 tests against the 536,870,912 this used to allow,
            // 0.01% over. Four times that still bounds the work while leaving
            // room above what real atlases ask for.
            maximum_raster_operations: 2 * 1024 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vector2 {
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vector4 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub w: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RectF {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ObjectReference {
    pub file_id: i32,
    pub path_id: i64,
}

impl ObjectReference {
    #[must_use]
    pub const fn is_null(self) -> bool {
        self.path_id == 0 || self.file_id < 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpritePackingMode {
    Tight,
    Rectangle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpriteMeshType {
    FullRect,
    Tight,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpriteSettings {
    pub raw: u32,
    pub packed: bool,
    pub packing_mode: SpritePackingMode,
    pub packing_rotation: u8,
    pub mesh_type: SpriteMeshType,
}

impl SpriteSettings {
    fn from_raw(raw: u32) -> Self {
        Self {
            raw,
            packed: raw & 1 != 0,
            packing_mode: if (raw >> 1) & 1 == 0 {
                SpritePackingMode::Tight
            } else {
                SpritePackingMode::Rectangle
            },
            packing_rotation: u8::try_from((raw >> 2) & 0xf)
                .expect("four packing-rotation bits fit in u8"),
            mesh_type: if (raw >> 6) & 1 == 0 {
                SpriteMeshType::FullRect
            } else {
                SpriteMeshType::Tight
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecondarySpriteTexture {
    pub texture: ObjectReference,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SpriteRenderData {
    pub texture: ObjectReference,
    pub alpha_texture: ObjectReference,
    pub secondary_textures: Vec<SecondarySpriteTexture>,
    pub texture_rect: RectF,
    pub texture_rect_offset: Vector2,
    pub atlas_rect_offset: Vector2,
    pub settings: SpriteSettings,
    pub uv_transform: Vector4,
    pub downscale_multiplier: f32,
    /// Validated local-space triangles used by tight-mesh masking. Empty
    /// geometry deliberately falls back to the rectangular managed path.
    pub mesh_triangles: Vec<[Vector2; 3]>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Sprite {
    /// Index in the containing serialized object's table. This distinguishes
    /// duplicate path IDs when collection-level atlas metadata is resolved.
    pub object_index: usize,
    pub path_id: i64,
    pub name: String,
    pub rect: RectF,
    pub offset: Vector2,
    pub border: Vector4,
    pub pixels_to_units: f32,
    pub pivot: Vector2,
    pub extrude: u32,
    pub is_polygon: bool,
    pub render_data_key: Option<([u8; 16], i64)>,
    pub atlas_tags: Vec<String>,
    pub sprite_atlas: ObjectReference,
    pub render_data: SpriteRenderData,
}

/// Parses versioned Sprite metadata and validated tight-mesh triangles while
/// bounding every variable-length array and mesh payload.
pub fn read_sprite(
    file: &SerializedFile,
    object_index: usize,
    limits: SpriteReadLimits,
) -> Result<Sprite> {
    if file.unity_version.is_stripped() {
        return Err(Error::unsupported(
            "Sprite requires a Unity version because its layout is version-dependent",
        ));
    }
    let object = file.objects.get(object_index).ok_or_else(|| {
        Error::invalid_data(format!(
            "serialized object index {object_index} is out of range"
        ))
    })?;
    if object.class_id != SPRITE_CLASS_ID {
        return Err(Error::unsupported(format!(
            "object {} has class ID {}, not Sprite ({SPRITE_CLASS_ID})",
            object.path_id, object.class_id
        )));
    }

    let region = file.object_region(object_index)?;
    let endian = if file.header.endianness == 0 {
        Endian::Little
    } else {
        Endian::Big
    };
    let mut reader = SpriteObjectReader {
        reader: EndianReader::new(region.cursor(), endian),
        region,
        absolute_start: object.byte_start,
        target_platform: file.target_platform,
        format_version: file.header.version.0,
        version: file.unity_version.components(),
        limits,
        total_string_bytes: 0,
        mesh_bytes: 0,
    };

    let name = reader.read_named_object()?;
    let rect = reader.read_rect("Sprite rect")?;
    let offset = reader.read_vector2("Sprite offset")?;
    let border = if reader.version >= (4, 5, 0) {
        reader.read_vector4("Sprite border")?
    } else {
        zero_vector4()
    };
    let pixels_to_units = reader.read_finite_f32("Sprite pixels-to-units")?;
    let pivot = if has_serialized_pivot(file) {
        reader.read_vector2("Sprite pivot")?
    } else {
        Vector2 { x: 0.5, y: 0.5 }
    };
    let extrude = reader.reader.read_u32()?;
    let is_polygon = if reader.version >= (5, 3, 0) {
        let value = reader.reader.read_bool()?;
        reader.align(4)?;
        value
    } else {
        false
    };

    let (render_data_key, atlas_tags, sprite_atlas) = if reader.version.0 >= 2017 {
        let key_bytes = reader.reader.read_bytes(16)?;
        let key = <[u8; 16]>::try_from(key_bytes.as_slice())
            .map_err(|_| Error::invalid_data("Sprite render-data key is truncated"))?;
        let value = reader.reader.read_i64()?;
        let tags = reader.read_string_array("Sprite atlas tag")?;
        let atlas = reader.read_pptr()?;
        (Some((key, value)), tags, atlas)
    } else {
        (None, Vec::new(), ObjectReference::default())
    };

    let render_data = reader.read_render_data()?;
    Ok(Sprite {
        object_index,
        path_id: object.path_id,
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
    })
}

/// Resolves texture references and reproduces the rectangular crop and
/// optional RGB alpha mask used by C# SpriteHelper.GetImage.
///
/// Returned rows are already vertically flipped into the display order of the
/// C# Sprite image. Rectangular packed sprites apply the same flip/rotation
/// transforms as `SpriteHelper.CutImage`. An explicitly referenced `SpriteAtlas`
/// is resolved locally or across files before its composite render-data key is
/// queried. An atlas whose render-data map is present but does not contain the
/// Sprite key remains an error. A completely empty map falls back to the
/// Sprite's resident render data: shipping bundles use this shape for an atlas
/// that retains its packed-Sprite table while omitting platform atlas pages.
///
/// The collection-level `AssetsManager.ProcessAssets` pass is reproduced:
/// `packedSprites` backfills null or unresolved atlas references, and a master
/// atlas replaces a selected variant in stable collection order. Validated
/// legacy and modern triangle meshes use an output- and operation-bounded
/// tight mask. Texture downscaling and differently sized resident alpha crops
/// use ImageSharp-compatible bicubic resampling.
pub fn decode_sprite_rgba8(
    collection: &AssetCollection,
    file: &SerializedFile,
    sprite: &Sprite,
    limits: SpriteReadLimits,
    texture_limits: TextureReadLimits,
) -> Result<RgbaImage> {
    if let Some(atlas) = effective_sprite_atlas(collection, file, sprite, limits)? {
        return decode_sprite_from_atlas(
            collection,
            SpriteSource {
                file,
                file_index: None,
            },
            sprite,
            atlas,
            limits,
            texture_limits,
        );
    }

    decode_sprite_from_resident_data(collection, file, None, sprite, limits, texture_limits)
}

/// Resolves and decodes one Sprite whose source file is identified by its
/// stable collection index.
///
/// Collections opened through the loader use their prebuilt `SpriteAtlas`
/// assignment index here, so decoding many Sprites does not repeatedly scan
/// every atlas and every `packedSprites` array. Low-level collections that did
/// not rebuild derived indexes retain the compatibility scan.
pub fn decode_sprite_rgba8_by_file_index(
    collection: &AssetCollection,
    file_index: usize,
    sprite: &Sprite,
    limits: SpriteReadLimits,
    texture_limits: TextureReadLimits,
) -> Result<RgbaImage> {
    let file = collection
        .serialized_files()
        .get(file_index)
        .map(|loaded| &loaded.file)
        .ok_or_else(|| {
            Error::invalid_data(format!(
                "Sprite source serialized-file index {file_index} is out of range"
            ))
        })?;
    if let Some(atlas) =
        effective_sprite_atlas_by_file_index(collection, file_index, sprite, limits)?
    {
        return decode_sprite_from_atlas(
            collection,
            SpriteSource {
                file,
                file_index: Some(file_index),
            },
            sprite,
            atlas,
            limits,
            texture_limits,
        );
    }
    decode_sprite_from_resident_data(
        collection,
        file,
        Some(file_index),
        sprite,
        limits,
        texture_limits,
    )
}

#[derive(Clone, Copy)]
struct SpriteSource<'a> {
    file: &'a SerializedFile,
    file_index: Option<usize>,
}

#[derive(Clone, Copy)]
struct SelectedSpriteAtlas<'a> {
    file: &'a SerializedFile,
    file_index: Option<usize>,
    object_index: usize,
    is_variant: Option<bool>,
}

/// Reproduces `AssetsManager.ProcessAssets`: every successfully parsed atlas
/// may backfill a packed Sprite whose explicit atlas is null or unresolved,
/// and a master atlas replaces an already selected variant. A resolved master
/// remains authoritative. Atlas and object-table order stay deterministic.
fn effective_sprite_atlas<'a>(
    collection: &'a AssetCollection,
    sprite_file: &'a SerializedFile,
    sprite: &Sprite,
    limits: SpriteReadLimits,
) -> Result<Option<SelectedSpriteAtlas<'a>>> {
    let sprite_region = sprite_file.object_region(sprite.object_index)?;
    let mut selected = if sprite.sprite_atlas.is_null() {
        None
    } else {
        resolve_object_target(
            collection,
            sprite_file,
            None,
            sprite.sprite_atlas,
            SPRITE_ATLAS_CLASS_ID,
            "SpriteAtlas",
            "Sprite atlas",
        )
        .ok()
        .map(|(file, object_index)| SelectedSpriteAtlas {
            file,
            file_index: None,
            object_index,
            is_variant: read_sprite_atlas(file, object_index, atlas_read_limits(limits))
                .ok()
                .map(|atlas| atlas.is_variant),
        })
    };

    for (file_index, loaded) in collection.serialized_files().iter().enumerate() {
        for (object_index, object) in loaded.file.objects.iter().enumerate() {
            if object.class_id != SPRITE_ATLAS_CLASS_ID {
                continue;
            }
            let Ok(atlas) =
                read_sprite_atlas(&loaded.file, object_index, atlas_read_limits(limits))
            else {
                // Managed construction failures never enter AssetsFile.Objects
                // and therefore cannot participate in the backfill pass.
                continue;
            };
            let packs_sprite = atlas.packed_sprites.iter().any(|reference| {
                resolve_serialized_object_reference(
                    collection,
                    &loaded.file,
                    Some(file_index),
                    *reference,
                    SPRITE_CLASS_ID,
                )
                .is_some_and(|(file, index)| {
                    file.object_region(index)
                        .is_ok_and(|region| region == sprite_region)
                })
            });
            if !packs_sprite {
                continue;
            }

            let replace = selected.is_none_or(|current| current.is_variant != Some(false));
            if replace {
                selected = Some(SelectedSpriteAtlas {
                    file: &loaded.file,
                    file_index: Some(file_index),
                    object_index,
                    is_variant: Some(atlas.is_variant),
                });
            }
        }
    }
    Ok(selected)
}

fn effective_sprite_atlas_by_file_index<'a>(
    collection: &'a AssetCollection,
    sprite_file_index: usize,
    sprite: &Sprite,
    limits: SpriteReadLimits,
) -> Result<Option<SelectedSpriteAtlas<'a>>> {
    let Some(assignments) =
        collection.indexed_sprite_atlas_assignments(sprite_file_index, sprite.object_index)
    else {
        let file = &collection.serialized_files()[sprite_file_index].file;
        return effective_sprite_atlas(collection, file, sprite, limits);
    };

    let mut selected = if sprite.sprite_atlas.is_null() {
        None
    } else {
        crate::scene::resolve_typed_object_reference(
            collection,
            sprite_file_index,
            crate::serialized::ObjectReference {
                file_id: sprite.sprite_atlas.file_id,
                path_id: sprite.sprite_atlas.path_id,
            },
            SPRITE_ATLAS_CLASS_ID,
        )
        .ok()
        .flatten()
        .map(|resolved| SelectedSpriteAtlas {
            file: resolved.file,
            file_index: Some(resolved.file_index),
            object_index: resolved.object_index,
            is_variant: collection
                .indexed_sprite_atlas(resolved.file_index, resolved.object_index)
                .map(|atlas| atlas.is_variant),
        })
    };

    for assignment in assignments {
        let atlas = collection
            .indexed_sprite_atlas_by_index(assignment.atlas)
            .ok_or_else(|| Error::invalid_data("SpriteAtlas assignment index is inconsistent"))?;
        let replace = selected.is_none_or(|current| current.is_variant != Some(false));
        if replace {
            let loaded = collection
                .serialized_files()
                .get(atlas.file_index)
                .ok_or_else(|| Error::invalid_data("indexed SpriteAtlas file is out of range"))?;
            selected = Some(SelectedSpriteAtlas {
                file: &loaded.file,
                file_index: Some(atlas.file_index),
                object_index: atlas.object_index,
                is_variant: Some(atlas.is_variant),
            });
        }
    }
    Ok(selected)
}

fn resolve_serialized_object_reference<'a>(
    collection: &'a AssetCollection,
    file: &'a SerializedFile,
    file_index: Option<usize>,
    reference: crate::serialized::ObjectReference,
    expected_class_id: i32,
) -> Option<(&'a SerializedFile, usize)> {
    resolve_object_target(
        collection,
        file,
        file_index,
        ObjectReference {
            file_id: reference.file_id,
            path_id: reference.path_id,
        },
        expected_class_id,
        "serialized object",
        "SpriteAtlas packed Sprite",
    )
    .ok()
}

fn decode_sprite_from_resident_data(
    collection: &AssetCollection,
    file: &SerializedFile,
    file_index: Option<usize>,
    sprite: &Sprite,
    limits: SpriteReadLimits,
    texture_limits: TextureReadLimits,
) -> Result<RgbaImage> {
    validate_renderable(
        sprite.render_data.settings,
        sprite.render_data.downscale_multiplier,
    )?;
    let texture = resolve_texture_reference(
        collection,
        file,
        file_index,
        sprite.render_data.texture,
        texture_limits,
        "Sprite texture",
    )?;
    let decoded = texture.decode_mip_rgba8(0, texture_limits)?;
    let mut image = crop_and_flip(
        &decoded,
        sprite,
        sprite.render_data.texture_rect,
        sprite.render_data.texture_rect_offset,
        sprite.render_data.settings,
        sprite.render_data.downscale_multiplier,
        limits,
    )?;

    if !sprite.render_data.alpha_texture.is_null() {
        let alpha_texture = resolve_texture_reference(
            collection,
            file,
            file_index,
            sprite.render_data.alpha_texture,
            texture_limits,
            "Sprite alpha texture",
        )?;
        let alpha_decoded = alpha_texture.decode_mip_rgba8(0, texture_limits)?;
        let mut alpha = crop_and_flip(
            &alpha_decoded,
            sprite,
            sprite.render_data.texture_rect,
            sprite.render_data.texture_rect_offset,
            sprite.render_data.settings,
            sprite.render_data.downscale_multiplier,
            limits,
        )?;
        if (alpha.width, alpha.height) != (image.width, image.height) {
            let working_base = image_byte_length(&image, "Sprite color crop")?
                .checked_add(image_byte_length(&alpha, "Sprite alpha crop")?)
                .ok_or_else(|| Error::invalid_data("Sprite alpha-mask working size overflowed"))?;
            alpha = resize_bicubic_rgba8(
                &alpha,
                image.width,
                image.height,
                limits,
                working_base,
                "Sprite alpha mask",
            )?;
        }
        apply_rgb_alpha_mask(&mut image, &alpha)?;
    }

    Ok(image)
}

fn decode_sprite_from_atlas(
    collection: &AssetCollection,
    sprite_source: SpriteSource<'_>,
    sprite: &Sprite,
    selected_atlas: SelectedSpriteAtlas<'_>,
    limits: SpriteReadLimits,
    texture_limits: TextureReadLimits,
) -> Result<RgbaImage> {
    let atlas = read_sprite_atlas(
        selected_atlas.file,
        selected_atlas.object_index,
        atlas_read_limits(limits),
    )?;
    let (guid_bytes, value) = sprite.render_data_key.ok_or_else(|| {
        Error::invalid_data("Sprite has a resolved SpriteAtlas but no render-data key")
    })?;
    let data = match atlas.render_data_for_sprite_key(&guid_bytes, value) {
        Some(data) => data,
        None if atlas.render_data_entries.is_empty() => {
            return decode_sprite_from_resident_data(
                collection,
                sprite_source.file,
                sprite_source.file_index,
                sprite,
                limits,
                texture_limits,
            );
        }
        None => {
            return Err(Error::invalid_data(format!(
                "SpriteAtlas path ID {} has no render data for the Sprite key",
                atlas.path_id
            )));
        }
    };
    decode_sprite_atlas_data(
        collection,
        selected_atlas.file,
        selected_atlas.file_index,
        sprite,
        data,
        limits,
        texture_limits,
    )
}

fn decode_sprite_atlas_data(
    collection: &AssetCollection,
    atlas_file: &SerializedFile,
    atlas_file_index: Option<usize>,
    sprite: &Sprite,
    data: &SpriteAtlasData,
    limits: SpriteReadLimits,
    texture_limits: TextureReadLimits,
) -> Result<RgbaImage> {
    let settings = SpriteSettings::from_raw(data.settings.raw);
    validate_renderable(settings, data.downscale_multiplier)?;
    let texture = resolve_texture_reference(
        collection,
        atlas_file,
        atlas_file_index,
        ObjectReference {
            file_id: data.texture.file_id,
            path_id: data.texture.path_id,
        },
        texture_limits,
        "SpriteAtlas texture",
    )?;
    let decoded = texture.decode_mip_rgba8(0, texture_limits)?;
    crop_and_flip(
        &decoded,
        sprite,
        RectF {
            x: data.texture_rect.x,
            y: data.texture_rect.y,
            width: data.texture_rect.width,
            height: data.texture_rect.height,
        },
        Vector2 {
            x: data.texture_rect_offset.x,
            y: data.texture_rect_offset.y,
        },
        settings,
        data.downscale_multiplier,
        limits,
    )
}

fn atlas_read_limits(limits: SpriteReadLimits) -> SpriteAtlasReadLimits {
    SpriteAtlasReadLimits {
        maximum_string_bytes: limits.maximum_string_bytes,
        maximum_total_string_bytes: limits.maximum_total_string_bytes,
        maximum_packed_sprites: limits.maximum_array_elements,
        maximum_packed_sprite_names: limits.maximum_array_elements,
        maximum_render_data_entries: limits.maximum_array_elements,
        maximum_secondary_textures: limits.maximum_array_elements,
    }
}

fn validate_renderable(settings: SpriteSettings, downscale: f32) -> Result<()> {
    if settings.packed && settings.packing_rotation > 4 {
        return Err(Error::unsupported(format!(
            "unknown packed Sprite rotation (raw 0x{:08x}, rotation {})",
            settings.raw, settings.packing_rotation
        )));
    }
    if !downscale.is_finite() {
        return Err(Error::invalid_data(
            "Sprite downscale multiplier is not finite",
        ));
    }
    Ok(())
}

fn resolve_texture_reference(
    collection: &AssetCollection,
    file: &SerializedFile,
    file_index: Option<usize>,
    reference: ObjectReference,
    limits: TextureReadLimits,
    field: &str,
) -> Result<crate::texture::Texture2D> {
    let (target_file, index) = resolve_object_target(
        collection,
        file,
        file_index,
        reference,
        TEXTURE_2D_CLASS_ID,
        "Texture2D",
        field,
    )?;
    read_texture2d(collection, target_file, index, limits)
}

fn resolve_object_target<'a>(
    collection: &'a AssetCollection,
    file: &'a SerializedFile,
    file_index: Option<usize>,
    reference: ObjectReference,
    expected_class_id: i32,
    expected_class_name: &str,
    field: &str,
) -> Result<(&'a SerializedFile, usize)> {
    if reference.is_null() {
        // A null reference is a statement about the asset, not about its
        // bytes: a Sprite whose texture was stripped, or one whose atlas is
        // published in a bundle the caller did not open, is a shape this
        // reader declines rather than a file it failed to parse. A real
        // Addressables build splits sprites and their textures across bundles,
        // so reading one bundle at a time meets this constantly.
        return Err(Error::unsupported(format!("{field} reference is null")));
    }

    if let Some(file_index) = file_index {
        let resolved = crate::scene::resolve_typed_object_reference(
            collection,
            file_index,
            crate::serialized::ObjectReference {
                file_id: reference.file_id,
                path_id: reference.path_id,
            },
            expected_class_id,
        )?
        .ok_or_else(|| Error::unsupported(format!("{field} reference is null")))?;
        return Ok((resolved.file, resolved.object_index));
    }

    let target_file = if reference.file_id == 0 {
        file
    } else {
        let external_index = reference
            .file_id
            .checked_sub(1)
            .and_then(|index| usize::try_from(index).ok())
            .ok_or_else(|| {
                Error::invalid_data(format!(
                    "{field} PPtr file ID {} cannot be converted to an external index",
                    reference.file_id
                ))
            })?;
        let external = file.externals.get(external_index).ok_or_else(|| {
            Error::invalid_data(format!(
                "{field} PPtr file ID {} refers to external index {external_index}, but the serialized file has {} externals",
                reference.file_id,
                file.externals.len()
            ))
        })?;
        let target_name = portable_file_name(&external.path);
        collection
            .serialized_files
            .iter()
            .find(|candidate| {
                portable_file_name(&candidate.path).eq_ignore_ascii_case(target_name)
            })
            .map(|candidate| &candidate.file)
            .ok_or_else(|| {
                Error::invalid_data(format!(
                    "{field} external file {:?} (portable name {:?}) was not found in the asset collection",
                    external.path, target_name
                ))
            })?
    };

    let index = target_file
        .objects
        .iter()
        .position(|object| object.path_id == reference.path_id)
        .ok_or_else(|| {
            Error::invalid_data(format!(
                "{field} path ID {} was not found in the resolved serialized file",
                reference.path_id,
            ))
        })?;
    if target_file.objects[index].class_id != expected_class_id {
        return Err(Error::invalid_data(format!(
            "{field} path ID {} has class ID {}, not {expected_class_name} ({expected_class_id})",
            reference.path_id, target_file.objects[index].class_id,
        )));
    }
    Ok((target_file, index))
}

fn portable_file_name(path: &str) -> &str {
    let component = path.rsplit_once("::").map_or(path, |(_, name)| name);
    component.rsplit(['/', '\\']).next().unwrap_or(component)
}

fn crop_and_flip(
    source: &RgbaImage,
    sprite: &Sprite,
    rect: RectF,
    texture_rect_offset: Vector2,
    settings: SpriteSettings,
    downscale: f32,
    limits: SpriteReadLimits,
) -> Result<RgbaImage> {
    validate_rgba_image(source, "Sprite source texture")?;
    if source.width == 0 || source.height == 0 {
        return Err(Error::invalid_data(
            "Sprite source texture dimensions must be positive",
        ));
    }

    let resized;
    let (source, working_base) = if let Some((width, height)) =
        sprite_resize_dimensions(source.width, source.height, downscale)?
    {
        resized = resize_bicubic_rgba8(
            source,
            width,
            height,
            limits,
            0,
            "Sprite downscaled texture",
        )?;
        let working_base = image_byte_length(&resized, "Sprite downscaled texture")?;
        (&resized, working_base)
    } else {
        (source, 0)
    };

    let triangle_bytes = if settings.packing_mode == SpritePackingMode::Tight {
        u64::try_from(sprite.render_data.mesh_triangles.len())
            .ok()
            .and_then(|count| {
                count.checked_mul(u64::try_from(std::mem::size_of::<[Vector2; 3]>()).ok()?)
            })
            .ok_or_else(|| Error::invalid_data("Sprite triangle working size overflowed"))?
    } else {
        0
    };
    let (left, top, width, height, output_length) =
        resolve_crop_bounds(source, rect, settings, limits, working_base, triangle_bytes)?;
    let mut pixels = try_zeroed_sprite_pixels(output_length)?;

    let source_stride = usize::try_from(u64::from(source.width) * 4)
        .map_err(|_| Error::invalid_data("Sprite source row stride is too large"))?;
    let destination_stride = usize::try_from(u64::from(width) * 4)
        .map_err(|_| Error::invalid_data("Sprite crop row stride is too large"))?;
    let left_bytes = left
        .checked_mul(4)
        .ok_or_else(|| Error::invalid_data("Sprite crop x byte offset overflowed"))?;
    for destination_y in 0..usize::try_from(height)
        .map_err(|_| Error::invalid_data("Sprite crop height is too large"))?
    {
        let source_start = top
            .checked_add(destination_y)
            .and_then(|source_y| source_y.checked_mul(source_stride))
            .and_then(|offset| offset.checked_add(left_bytes))
            .ok_or_else(|| Error::invalid_data("Sprite source pixel offset overflowed"))?;
        let source_end = source_start
            .checked_add(destination_stride)
            .ok_or_else(|| Error::invalid_data("Sprite source row end overflowed"))?;
        let destination_start = destination_y
            .checked_mul(destination_stride)
            .ok_or_else(|| Error::invalid_data("Sprite destination row overflowed"))?;
        let destination_end = destination_start
            .checked_add(destination_stride)
            .ok_or_else(|| Error::invalid_data("Sprite destination row end overflowed"))?;
        let source_row = source
            .pixels
            .get(source_start..source_end)
            .ok_or_else(|| Error::invalid_data("Sprite crop source row exceeds decoded texture"))?;
        pixels[destination_start..destination_end].copy_from_slice(source_row);
    }

    let mut image = RgbaImage {
        width,
        height,
        pixels,
    };
    apply_packing_rotation(&mut image, settings)?;
    if settings.packing_mode == SpritePackingMode::Tight {
        apply_tight_mesh_mask(&mut image, sprite, texture_rect_offset, limits)?;
    }
    flip_vertical(&mut image)?;
    Ok(image)
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::float_cmp
)]
fn sprite_resize_dimensions(
    source_width: u32,
    source_height: u32,
    downscale: f32,
) -> Result<Option<(u32, u32)>> {
    if !downscale.is_finite() {
        return Err(Error::invalid_data(
            "Sprite downscale multiplier is not finite",
        ));
    }
    if downscale <= 0.0 || downscale == 1.0 {
        return Ok(None);
    }

    #[allow(clippy::cast_precision_loss)]
    let width = source_width as f32 / downscale;
    #[allow(clippy::cast_precision_loss)]
    let height = source_height as f32 / downscale;
    let checked_dimension = |value: f32, field: &str| {
        // ImageSharp accepts signed Int32 dimensions. C# casts the positive
        // quotient to Int32 by truncating toward zero before Resize is called.
        if !value.is_finite() || !(1.0..2_147_483_648.0).contains(&value) {
            return Err(Error::invalid_data(format!(
                "{field} {value} is outside the positive Int32 range"
            )));
        }
        Ok(value.trunc() as u32)
    };
    Ok(Some((
        checked_dimension(width, "Sprite resized width")?,
        checked_dimension(height, "Sprite resized height")?,
    )))
}

#[derive(Debug, Clone, Copy)]
struct BicubicAxisKernel {
    first: u32,
    last: u32,
    center: f32,
    sample_scale: f32,
    normalization: f32,
}

impl BicubicAxisKernel {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        clippy::cast_sign_loss
    )]
    fn new(source_length: u32, target_length: u32, target_index: u32) -> Result<Self> {
        let scale = target_length as f32 / source_length as f32;
        let sample_scale = scale.min(1.0);
        let center = ((target_index as f32 + 0.5) / scale) - 0.5;
        let radius = 2.0 / sample_scale;
        let lower = (center - radius).ceil();
        let upper = (center + radius).floor();
        let source_last = source_length - 1;
        let first = if lower <= 0.0 {
            0
        } else if lower >= source_length as f32 {
            source_length
        } else {
            lower as u32
        };
        let last = if upper >= source_last as f32 {
            source_last
        } else if upper < 0.0 {
            return Err(Error::invalid_data(
                "Sprite bicubic kernel does not intersect its source axis",
            ));
        } else {
            upper as u32
        };
        if first > last {
            return Err(Error::invalid_data(
                "Sprite bicubic kernel does not intersect its source axis",
            ));
        }

        let mut normalization = 0.0_f32;
        for source_index in first..=last {
            let distance = (source_index as f32 - center) * sample_scale;
            normalization += bicubic_weight(distance);
        }
        if !normalization.is_finite() || normalization == 0.0 {
            return Err(Error::invalid_data(
                "Sprite bicubic kernel normalization is invalid",
            ));
        }
        Ok(Self {
            first,
            last,
            center,
            sample_scale,
            normalization,
        })
    }

    #[allow(clippy::cast_precision_loss)]
    fn weight(self, source_index: u32) -> f32 {
        let distance = (source_index as f32 - self.center) * self.sample_scale;
        bicubic_weight(distance) / self.normalization
    }
}

fn bicubic_weight(value: f32) -> f32 {
    let value = value.abs();
    if value < 1.0 {
        ((1.5 * value - 2.5) * value) * value + 1.0
    } else if value < 2.0 {
        ((-0.5 * value + 2.5) * value - 4.0) * value + 2.0
    } else {
        0.0
    }
}

fn resize_bicubic_rgba8(
    source: &RgbaImage,
    target_width: u32,
    target_height: u32,
    limits: SpriteReadLimits,
    working_base: u64,
    field: &str,
) -> Result<RgbaImage> {
    validate_rgba_image(source, field)?;
    if source.width == 0 || source.height == 0 {
        return Err(Error::invalid_data(format!(
            "{field} source dimensions must be positive"
        )));
    }
    if target_width == 0 || target_height == 0 {
        return Err(Error::invalid_data(format!(
            "{field} target dimensions must be positive"
        )));
    }
    if target_width > i32::MAX as u32 || target_height > i32::MAX as u32 {
        return Err(Error::invalid_data(format!(
            "{field} target dimensions exceed the positive Int32 range"
        )));
    }

    let pixel_count = u64::from(target_width)
        .checked_mul(u64::from(target_height))
        .ok_or_else(|| Error::invalid_data(format!("{field} pixel count overflowed")))?;
    if pixel_count > limits.maximum_output_pixels {
        return Err(Error::invalid_data(format!(
            "{field} has {pixel_count} pixels, exceeding limit {}",
            limits.maximum_output_pixels
        )));
    }
    let output_length = pixel_count
        .checked_mul(4)
        .ok_or_else(|| Error::invalid_data(format!("{field} byte size overflowed")))?;
    if output_length > limits.maximum_output_bytes {
        return Err(Error::invalid_data(format!(
            "{field} is {output_length} bytes, exceeding limit {}",
            limits.maximum_output_bytes
        )));
    }
    let working_length = working_base
        .checked_add(output_length)
        .ok_or_else(|| Error::invalid_data(format!("{field} working size overflowed")))?;
    if working_length > limits.maximum_working_bytes {
        return Err(Error::invalid_data(format!(
            "{field} requires {working_length} working bytes, exceeding limit {}",
            limits.maximum_working_bytes
        )));
    }

    let output_length = usize::try_from(output_length)
        .map_err(|_| Error::invalid_data(format!("{field} is too large for this platform")))?;
    let mut pixels = try_zeroed_sprite_pixels(output_length)?;
    if (source.width, source.height) == (target_width, target_height) {
        pixels.copy_from_slice(&source.pixels);
        return Ok(RgbaImage {
            width: target_width,
            height: target_height,
            pixels,
        });
    }

    let source_width = usize::try_from(source.width)
        .map_err(|_| Error::invalid_data(format!("{field} width is too large")))?;
    let mut destination_offset = 0_usize;
    for destination_y in 0..target_height {
        let vertical = BicubicAxisKernel::new(source.height, target_height, destination_y)?;
        for destination_x in 0..target_width {
            let horizontal = BicubicAxisKernel::new(source.width, target_width, destination_x)?;
            let pixel = resize_bicubic_pixel(source, source_width, horizontal, vertical)?;
            let destination_end = destination_offset
                .checked_add(4)
                .ok_or_else(|| Error::invalid_data(format!("{field} offset overflowed")))?;
            pixels[destination_offset..destination_end].copy_from_slice(&pixel);
            destination_offset = destination_end;
        }
    }
    Ok(RgbaImage {
        width: target_width,
        height: target_height,
        pixels,
    })
}

fn resize_bicubic_pixel(
    source: &RgbaImage,
    source_width: usize,
    horizontal: BicubicAxisKernel,
    vertical: BicubicAxisKernel,
) -> Result<[u8; 4]> {
    let mut result = [0.0_f32; 4];
    for source_y in vertical.first..=vertical.last {
        let vertical_weight = vertical.weight(source_y);
        let source_y = usize::try_from(source_y)
            .map_err(|_| Error::invalid_data("Sprite bicubic y index is too large"))?;
        let mut row = [0.0_f32; 4];
        for source_x in horizontal.first..=horizontal.last {
            let horizontal_weight = horizontal.weight(source_x);
            let source_x = usize::try_from(source_x)
                .map_err(|_| Error::invalid_data("Sprite bicubic x index is too large"))?;
            let offset = source_y
                .checked_mul(source_width)
                .and_then(|value| value.checked_add(source_x))
                .and_then(|value| value.checked_mul(4))
                .ok_or_else(|| Error::invalid_data("Sprite bicubic source offset overflowed"))?;
            let pixel = source
                .pixels
                .get(offset..offset + 4)
                .ok_or_else(|| Error::invalid_data("Sprite bicubic source pixel is truncated"))?;
            let alpha = f32::from(pixel[3]) / 255.0;
            for channel in 0..3 {
                let mut value = f32::from(pixel[channel]) / 255.0;
                value *= alpha;
                row[channel] += value * horizontal_weight;
            }
            row[3] += alpha * horizontal_weight;
        }
        for channel in 0..4 {
            result[channel] += row[channel] * vertical_weight;
        }
    }
    if result[3] > 0.0 {
        for channel in 0..3 {
            result[channel] /= result[3];
        }
    }
    Ok(result.map(scaled_f32_to_u8))
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn scaled_f32_to_u8(value: f32) -> u8 {
    ((value.clamp(0.0, 1.0) * 255.0) + 0.5).floor() as u8
}

fn resolve_crop_bounds(
    source: &RgbaImage,
    rect: RectF,
    settings: SpriteSettings,
    limits: SpriteReadLimits,
    working_base: u64,
    triangle_bytes: u64,
) -> Result<(usize, usize, u32, u32, usize)> {
    let left = checked_floor(rect.x, "Sprite texture rect x")?;
    let top = checked_floor(rect.y, "Sprite texture rect y")?;
    let right = checked_ceil_sum(rect.x, rect.width, "Sprite texture rect right")?;
    let bottom = checked_ceil_sum(rect.y, rect.height, "Sprite texture rect bottom")?;
    if left < 0 || top < 0 {
        return Err(Error::invalid_data(format!(
            "Sprite texture rect begins outside the texture: ({left}, {top})"
        )));
    }
    let source_width = i64::from(source.width);
    let source_height = i64::from(source.height);
    let right = right.min(source_width);
    let bottom = bottom.min(source_height);
    if left >= right || top >= bottom {
        return Err(Error::invalid_data(format!(
            "Sprite texture rect {left},{top}..{right},{bottom} has no pixels inside {}x{} texture",
            source.width, source.height
        )));
    }

    let width = u32::try_from(right - left)
        .map_err(|_| Error::invalid_data("Sprite crop width does not fit in u32"))?;
    let height = u32::try_from(bottom - top)
        .map_err(|_| Error::invalid_data("Sprite crop height does not fit in u32"))?;
    let pixel_count = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or_else(|| Error::invalid_data("Sprite crop pixel count overflowed"))?;
    if pixel_count > limits.maximum_output_pixels {
        return Err(Error::invalid_data(format!(
            "Sprite crop has {pixel_count} pixels, exceeding limit {}",
            limits.maximum_output_pixels
        )));
    }
    let output_length = pixel_count
        .checked_mul(4)
        .ok_or_else(|| Error::invalid_data("Sprite crop byte size overflowed"))?;
    if output_length > limits.maximum_output_bytes {
        return Err(Error::invalid_data(format!(
            "Sprite crop is {output_length} bytes, exceeding limit {}",
            limits.maximum_output_bytes
        )));
    }
    let output_copies = if settings.packed && settings.packing_rotation == 4 {
        2
    } else {
        1
    };
    let working_length = output_length
        .checked_mul(output_copies)
        .and_then(|length| length.checked_add(working_base))
        .and_then(|length| length.checked_add(triangle_bytes))
        .and_then(|length| {
            if settings.packing_mode == SpritePackingMode::Tight && triangle_bytes != 0 {
                length.checked_add(pixel_count)
            } else {
                Some(length)
            }
        })
        .ok_or_else(|| Error::invalid_data("Sprite crop working byte size overflowed"))?;
    if working_length > limits.maximum_working_bytes {
        return Err(Error::invalid_data(format!(
            "Sprite crop requires {working_length} working bytes, exceeding limit {}",
            limits.maximum_working_bytes
        )));
    }
    let output_length = usize::try_from(output_length)
        .map_err(|_| Error::invalid_data("Sprite crop is too large for this platform"))?;
    let left = usize::try_from(left)
        .map_err(|_| Error::invalid_data("Sprite crop x cannot be negative"))?;
    let top = usize::try_from(top)
        .map_err(|_| Error::invalid_data("Sprite crop y cannot be negative"))?;
    Ok((left, top, width, height, output_length))
}

fn try_zeroed_sprite_pixels(output_length: usize) -> Result<Vec<u8>> {
    let mut pixels = Vec::new();
    pixels.try_reserve_exact(output_length).map_err(|error| {
        Error::invalid_data(format!("cannot allocate Sprite crop pixels: {error}"))
    })?;
    pixels.resize(output_length, 0);
    Ok(pixels)
}

fn apply_packing_rotation(image: &mut RgbaImage, settings: SpriteSettings) -> Result<()> {
    if !settings.packed {
        return Ok(());
    }
    match settings.packing_rotation {
        0 => Ok(()),
        1 => flip_horizontal(image),
        2 => flip_vertical(image),
        3 => {
            flip_horizontal(image)?;
            flip_vertical(image)
        }
        // AssetStudio's C# path maps SpritePackingRotation.Rotate90 to
        // ImageSharp Rotate(270), i.e. a 90-degree counter-clockwise turn.
        4 => rotate_counter_clockwise(image),
        rotation => Err(Error::unsupported(format!(
            "unknown packed Sprite rotation {rotation}"
        ))),
    }
}

fn apply_tight_mesh_mask(
    image: &mut RgbaImage,
    sprite: &Sprite,
    texture_rect_offset: Vector2,
    limits: SpriteReadLimits,
) -> Result<()> {
    // Pixel centers and disabled antialiasing match the managed fallback mask.
    // The current managed `< 1024` fast path clips against `path.Clip()` with
    // no clip operands and therefore returns the whole rectangular crop; Rust
    // deliberately applies the intended tight mask for small meshes as well.
    let triangles = &sprite.render_data.mesh_triangles;
    if triangles.is_empty() {
        // `SpriteHelper` catches unusable mesh geometry and returns the
        // rectangular crop, so semantic mesh failures are represented by an
        // empty validated triangle list.
        return Ok(());
    }
    validate_rgba_image(image, "tight Sprite crop")?;
    // Nearly every sprite is a two-triangle quad that covers its own crop, so
    // the mask keeps every pixel and the rasterisation is pure cost: across one
    // scenario, 6,657 of 6,680 masks were fully covered and 1.09 billion of
    // 1.09 billion pixel tests answered "inside". Proving that case up front
    // costs eight point-in-triangle tests instead of width*height of them.
    if mesh_covers_every_pixel(sprite, triangles, texture_rect_offset, image) {
        return Ok(());
    }
    let pixel_count = u64::from(image.width)
        .checked_mul(u64::from(image.height))
        .ok_or_else(|| Error::invalid_data("tight Sprite mask size overflowed"))?;
    let mask_length = usize::try_from(pixel_count)
        .map_err(|_| Error::invalid_data("tight Sprite mask is too large for this platform"))?;
    let mask_width = usize::try_from(image.width)
        .map_err(|_| Error::invalid_data("tight Sprite width is too large for this platform"))?;
    let mut mask = Vec::new();
    mask.try_reserve_exact(mask_length).map_err(|error| {
        Error::invalid_data(format!("cannot allocate tight Sprite mask: {error}"))
    })?;
    mask.resize(mask_length, 0_u8);

    let mut operations = 0_u64;
    for triangle in triangles {
        let Some(transformed) = transform_sprite_triangle(sprite, *triangle, texture_rect_offset)
        else {
            return Ok(());
        };
        let Some((left, top, right, bottom)) = tight_triangle_bounds(&transformed, image) else {
            continue;
        };
        let area = u64::from(right - left)
            .checked_mul(u64::from(bottom - top))
            .ok_or_else(|| Error::invalid_data("tight Sprite raster area overflowed"))?;
        operations = operations
            .checked_add(area)
            .ok_or_else(|| Error::invalid_data("tight Sprite raster operation count overflowed"))?;
        if operations > limits.maximum_raster_operations {
            return Err(Error::invalid_data(format!(
                "tight Sprite rasterization requires {operations} pixel tests, exceeding limit {}",
                limits.maximum_raster_operations
            )));
        }
        // `tight_triangle_bounds` clamps the box to the image and `mask` holds
        // width*height entries, so every index below is in range. Resolving the
        // row offset once per scanline keeps a fallible conversion -- and the
        // error path it carries -- out of the per-pixel loop.
        let edges = TriangleEdges::new(&transformed);
        for y in top..bottom {
            let row = usize::try_from(y)
                .ok()
                .and_then(|y| y.checked_mul(mask_width))
                .ok_or_else(|| Error::invalid_data("tight Sprite mask row overflowed"))?;
            let Some(scanline) = mask.get_mut(row..row + mask_width) else {
                return Err(Error::invalid_data("tight Sprite mask row is out of range"));
            };
            let row_terms = edges.row(f64::from(y) + 0.5);
            for x in left..right {
                if edges.contains(f64::from(x) + 0.5, &row_terms) {
                    #[allow(clippy::cast_possible_truncation)]
                    {
                        scanline[x as usize] = 1;
                    }
                }
            }
        }
    }

    for (pixel, keep) in image.pixels.chunks_exact_mut(4).zip(mask) {
        if keep == 0 {
            pixel.copy_from_slice(&[0, 0, 0, 0]);
        }
    }
    Ok(())
}

fn transform_sprite_triangle(
    sprite: &Sprite,
    triangle: [Vector2; 3],
    texture_rect_offset: Vector2,
) -> Option<[(f64, f64); 3]> {
    let scale = f64::from(sprite.pixels_to_units);
    let translation_x =
        f64::from(sprite.rect.width) * f64::from(sprite.pivot.x) - f64::from(texture_rect_offset.x);
    let translation_y = f64::from(sprite.rect.height) * f64::from(sprite.pivot.y)
        - f64::from(texture_rect_offset.y);
    let transformed = triangle.map(|vertex| {
        (
            f64::from(vertex.x) * scale + translation_x,
            f64::from(vertex.y) * scale + translation_y,
        )
    });
    transformed
        .iter()
        .all(|(x, y)| x.is_finite() && y.is_finite())
        .then_some(transformed)
}

fn tight_triangle_bounds(
    triangle: &[(f64, f64); 3],
    image: &RgbaImage,
) -> Option<(u32, u32, u32, u32)> {
    let minimum_x = triangle
        .iter()
        .map(|point| point.0)
        .fold(f64::INFINITY, f64::min);
    let minimum_y = triangle
        .iter()
        .map(|point| point.1)
        .fold(f64::INFINITY, f64::min);
    let maximum_x = triangle
        .iter()
        .map(|point| point.0)
        .fold(f64::NEG_INFINITY, f64::max);
    let maximum_y = triangle
        .iter()
        .map(|point| point.1)
        .fold(f64::NEG_INFINITY, f64::max);
    let clamp = |value: f64, maximum: u32| -> u32 {
        if value <= 0.0 {
            0
        } else if value >= f64::from(maximum) {
            maximum
        } else {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            {
                value as u32
            }
        }
    };
    let left = clamp(minimum_x.floor(), image.width);
    let top = clamp(minimum_y.floor(), image.height);
    let right = clamp(maximum_x.ceil(), image.width);
    let bottom = clamp(maximum_y.ceil(), image.height);
    (left < right && top < bottom).then_some((left, top, right, bottom))
}

/// True when the mesh provably keeps every pixel, making the mask a no-op.
///
/// Sufficient, never necessary: it only proves the shape that dominates real
/// sprites -- two triangles sharing a diagonal, forming a convex quadrilateral
/// that contains the whole pixel grid. Anything else falls through to the
/// rasteriser, so a `false` here can only cost time, never correctness.
fn mesh_covers_every_pixel(
    sprite: &Sprite,
    triangles: &[[Vector2; 3]],
    texture_rect_offset: Vector2,
    image: &RgbaImage,
) -> bool {
    let [first, second] = triangles else {
        return false;
    };
    let (Some(first), Some(second)) = (
        transform_sprite_triangle(sprite, *first, texture_rect_offset),
        transform_sprite_triangle(sprite, *second, texture_rect_offset),
    ) else {
        return false;
    };
    let Some(quad) = shared_edge_quad(&first, &second) else {
        return false;
    };
    if !is_convex_quad(&quad) {
        return false;
    }
    if image.width == 0 || image.height == 0 {
        return false;
    }
    // The extreme pixel centres; a convex region containing all four contains
    // every centre between them.
    let right = f64::from(image.width) - 0.5;
    let bottom = f64::from(image.height) - 0.5;
    [(0.5, 0.5), (right, 0.5), (0.5, bottom), (right, bottom)]
        .into_iter()
        .all(|(x, y)| point_in_triangle(x, y, &first) || point_in_triangle(x, y, &second))
}

/// Orders two triangles that share exactly one edge into the quadrilateral they
/// tile, as `shared, only-in-first, shared, only-in-second`.
fn shared_edge_quad(first: &[(f64, f64); 3], second: &[(f64, f64); 3]) -> Option<[(f64, f64); 4]> {
    let shared: Vec<(f64, f64)> = first
        .iter()
        .copied()
        .filter(|vertex| second.contains(vertex))
        .collect();
    if shared.len() != 2 {
        return None;
    }
    let unique_first = first.iter().copied().find(|v| !shared.contains(v))?;
    let unique_second = second.iter().copied().find(|v| !shared.contains(v))?;
    if unique_first == unique_second {
        return None;
    }
    Some([shared[0], unique_first, shared[1], unique_second])
}

fn is_convex_quad(quad: &[(f64, f64); 4]) -> bool {
    let mut negative = false;
    let mut positive = false;
    for index in 0..4 {
        let a = quad[index];
        let b = quad[(index + 1) % 4];
        let c = quad[(index + 2) % 4];
        let cross = (b.0 - a.0) * (c.1 - b.1) - (b.1 - a.1) * (c.0 - b.0);
        if cross < 0.0 {
            negative = true;
        } else if cross > 0.0 {
            positive = true;
        } else {
            // A straight or repeated vertex leaves the shape unproven.
            return false;
        }
    }
    negative != positive
}

fn point_in_triangle(x: f64, y: f64, triangle: &[(f64, f64); 3]) -> bool {
    let edges = TriangleEdges::new(triangle);
    edges.contains(x, &edges.row(y))
}

/// The three edge functions of a triangle, with everything that does not depend
/// on the sample point lifted out.
///
/// `point_in_triangle` recomputed each edge from the raw vertices for every
/// pixel of every triangle, so the vertex deltas and the whole `y` term were
/// re-derived millions of times. Hoisting them leaves one subtract, one
/// multiply and one subtract per edge per pixel. Every operation still has the
/// same operands as before, only evaluated once per triangle or per scanline
/// instead of per pixel, so the result is bit-for-bit what it was.
struct TriangleEdges {
    origin: [(f64, f64); 3],
    delta: [(f64, f64); 3],
}

impl TriangleEdges {
    fn new(triangle: &[(f64, f64); 3]) -> Self {
        let mut origin = [(0.0, 0.0); 3];
        let mut delta = [(0.0, 0.0); 3];
        for (index, pair) in [(0, 1), (1, 2), (2, 0)].into_iter().enumerate() {
            let (first, second) = (triangle[pair.0], triangle[pair.1]);
            origin[index] = first;
            delta[index] = (second.0 - first.0, second.1 - first.1);
        }
        Self { origin, delta }
    }

    /// The `y`-dependent term of each edge, constant along a scanline.
    fn row(&self, y: f64) -> [f64; 3] {
        [0, 1, 2].map(|index| (y - self.origin[index].1) * self.delta[index].0)
    }

    fn contains(&self, x: f64, row: &[f64; 3]) -> bool {
        let first = (x - self.origin[0].0) * self.delta[0].1 - row[0];
        let second = (x - self.origin[1].0) * self.delta[1].1 - row[1];
        let third = (x - self.origin[2].0) * self.delta[2].1 - row[2];
        if first == 0.0 && second == 0.0 && third == 0.0 {
            return false;
        }
        let has_negative = first < 0.0 || second < 0.0 || third < 0.0;
        let has_positive = first > 0.0 || second > 0.0 || third > 0.0;
        !(has_negative && has_positive)
    }
}

fn flip_horizontal(image: &mut RgbaImage) -> Result<()> {
    validate_rgba_image(image, "Sprite packed image")?;
    let width = usize::try_from(image.width)
        .map_err(|_| Error::invalid_data("Sprite width is too large for this platform"))?;
    let row_bytes = width
        .checked_mul(4)
        .ok_or_else(|| Error::invalid_data("Sprite row byte size overflowed"))?;
    for row in image.pixels.chunks_exact_mut(row_bytes) {
        for x in 0..width / 2 {
            let left = x
                .checked_mul(4)
                .ok_or_else(|| Error::invalid_data("Sprite pixel offset overflowed"))?;
            let right = (width - 1 - x)
                .checked_mul(4)
                .ok_or_else(|| Error::invalid_data("Sprite pixel offset overflowed"))?;
            for channel in 0..4 {
                row.swap(left + channel, right + channel);
            }
        }
    }
    Ok(())
}

fn flip_vertical(image: &mut RgbaImage) -> Result<()> {
    validate_rgba_image(image, "Sprite packed image")?;
    let row_bytes = usize::try_from(u64::from(image.width) * 4)
        .map_err(|_| Error::invalid_data("Sprite row byte size is too large"))?;
    let height = usize::try_from(image.height)
        .map_err(|_| Error::invalid_data("Sprite height is too large for this platform"))?;
    for y in 0..height / 2 {
        let opposite = height - 1 - y;
        let top = y
            .checked_mul(row_bytes)
            .ok_or_else(|| Error::invalid_data("Sprite row offset overflowed"))?;
        let bottom = opposite
            .checked_mul(row_bytes)
            .ok_or_else(|| Error::invalid_data("Sprite row offset overflowed"))?;
        for byte in 0..row_bytes {
            image.pixels.swap(top + byte, bottom + byte);
        }
    }
    Ok(())
}

fn rotate_counter_clockwise(image: &mut RgbaImage) -> Result<()> {
    validate_rgba_image(image, "Sprite packed image")?;
    let source_width = usize::try_from(image.width)
        .map_err(|_| Error::invalid_data("Sprite width is too large for this platform"))?;
    let source_height = usize::try_from(image.height)
        .map_err(|_| Error::invalid_data("Sprite height is too large for this platform"))?;
    let mut rotated = Vec::new();
    rotated
        .try_reserve_exact(image.pixels.len())
        .map_err(|error| Error::invalid_data(format!("cannot allocate rotated Sprite: {error}")))?;
    rotated.resize(image.pixels.len(), 0);
    for destination_y in 0..source_width {
        for destination_x in 0..source_height {
            let source_x = source_width - 1 - destination_y;
            let source_y = destination_x;
            let source = source_y
                .checked_mul(source_width)
                .and_then(|offset| offset.checked_add(source_x))
                .and_then(|offset| offset.checked_mul(4))
                .ok_or_else(|| Error::invalid_data("Sprite source pixel offset overflowed"))?;
            let destination = destination_y
                .checked_mul(source_height)
                .and_then(|offset| offset.checked_add(destination_x))
                .and_then(|offset| offset.checked_mul(4))
                .ok_or_else(|| Error::invalid_data("Sprite rotated pixel offset overflowed"))?;
            rotated[destination..destination + 4]
                .copy_from_slice(&image.pixels[source..source + 4]);
        }
    }
    image.width = u32::try_from(source_height)
        .map_err(|_| Error::invalid_data("rotated Sprite width does not fit in u32"))?;
    image.height = u32::try_from(source_width)
        .map_err(|_| Error::invalid_data("rotated Sprite height does not fit in u32"))?;
    image.pixels = rotated;
    Ok(())
}

fn apply_rgb_alpha_mask(image: &mut RgbaImage, alpha: &RgbaImage) -> Result<()> {
    validate_rgba_image(image, "Sprite color crop")?;
    validate_rgba_image(alpha, "Sprite alpha crop")?;
    if image.pixels.len() != alpha.pixels.len() {
        return Err(Error::invalid_data(
            "Sprite color and alpha pixel buffer lengths differ",
        ));
    }
    for (pixel, mask) in image
        .pixels
        .chunks_exact_mut(4)
        .zip(alpha.pixels.chunks_exact(4))
    {
        let grayscale = (u16::from(mask[0]) + u16::from(mask[1]) + u16::from(mask[2])) / 3;
        pixel[3] = u8::try_from(grayscale).expect("average of three u8 values fits in u8");
    }
    Ok(())
}

fn image_byte_length(image: &RgbaImage, field: &str) -> Result<u64> {
    validate_rgba_image(image, field)?;
    u64::try_from(image.pixels.len())
        .map_err(|_| Error::invalid_data(format!("{field} length does not fit in u64")))
}

fn validate_rgba_image(image: &RgbaImage, field: &str) -> Result<()> {
    let expected = u64::from(image.width)
        .checked_mul(u64::from(image.height))
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| Error::invalid_data(format!("{field} byte size overflowed")))?;
    let actual = u64::try_from(image.pixels.len())
        .map_err(|_| Error::invalid_data(format!("{field} length does not fit in u64")))?;
    if expected != actual {
        return Err(Error::invalid_data(format!(
            "{field} has {actual} bytes, expected {expected}"
        )));
    }
    Ok(())
}

fn checked_floor(value: f32, field: &str) -> Result<i64> {
    checked_rounded(value, f32::floor, field)
}

fn checked_ceil_sum(left: f32, size: f32, field: &str) -> Result<i64> {
    if size < 0.0 {
        return Err(Error::invalid_data(format!(
            "{field} uses negative size {size}"
        )));
    }
    checked_rounded(left + size, f32::ceil, field)
}

#[allow(clippy::cast_possible_truncation)]
fn checked_rounded(value: f32, operation: fn(f32) -> f32, field: &str) -> Result<i64> {
    if !value.is_finite() {
        return Err(Error::invalid_data(format!("{field} is not finite")));
    }
    let rounded = operation(value);
    if !(-2_147_483_648.0..2_147_483_648.0).contains(&rounded) {
        return Err(Error::invalid_data(format!(
            "{field} is outside the supported integer range"
        )));
    }
    Ok(i64::from(rounded as i32))
}

fn has_serialized_pivot(file: &SerializedFile) -> bool {
    let version = &file.unity_version;
    version.components() >= (5, 4, 2)
        || (version.components() == (5, 4, 1) && version.is_patch() && version.build >= 3)
}

fn zero_vector2() -> Vector2 {
    Vector2 { x: 0.0, y: 0.0 }
}

fn zero_vector4() -> Vector4 {
    Vector4 {
        x: 0.0,
        y: 0.0,
        z: 0.0,
        w: 0.0,
    }
}

enum PendingSpriteMesh {
    Legacy {
        vertices: Vec<Vector2>,
        indices: Vec<u16>,
    },
    Modern {
        submeshes: Vec<SpriteSubMesh>,
        index_buffer: Vec<u8>,
        vertex_data: SpriteVertexData,
    },
}

struct SpriteSubMesh {
    first_byte: u32,
    index_count: u32,
    first_vertex: u32,
    vertex_count: u32,
}

#[derive(Clone, Copy)]
struct SpriteVertexChannel {
    stream: u8,
    offset: u8,
    format: u8,
    dimension: u8,
}

struct SpriteVertexStream {
    offset: usize,
    stride: usize,
}

struct SpriteVertexData {
    vertex_count: usize,
    version: (u32, u32, u32),
    channels: Vec<SpriteVertexChannel>,
    data: Vec<u8>,
}

struct SpriteObjectReader {
    reader: EndianReader<RegionCursor>,
    region: Region,
    absolute_start: u64,
    target_platform: i32,
    format_version: u32,
    version: (u32, u32, u32),
    limits: SpriteReadLimits,
    total_string_bytes: usize,
    mesh_bytes: u64,
}

impl SpriteObjectReader {
    fn read_named_object(&mut self) -> Result<String> {
        if self.target_platform == NO_TARGET_PLATFORM {
            self.skip(4, "Object hide flags")?;
            self.read_pptr()?;
            self.read_pptr()?;
        }
        self.read_aligned_string("Sprite name")
    }

    fn read_render_data(&mut self) -> Result<SpriteRenderData> {
        let texture = self.read_pptr()?;
        let alpha_texture = if self.version >= (5, 2, 0) {
            self.read_pptr()?
        } else {
            ObjectReference::default()
        };
        let secondary_textures = if self.version.0 >= 2019 {
            let count = self.read_count("Sprite secondary texture")?;
            let mut textures = reserve_vec(count, "Sprite secondary textures")?;
            for _ in 0..count {
                textures.push(SecondarySpriteTexture {
                    texture: self.read_pptr()?,
                    name: self.read_aligned_string("Sprite secondary texture name")?,
                });
            }
            textures
        } else {
            Vec::new()
        };

        let pending_mesh = if self.version >= (5, 6, 0) {
            self.read_modern_sprite_mesh()?
        } else {
            self.read_legacy_sprite_mesh()?
        };

        if self.version.0 >= 2018 {
            self.skip_mesh_records(64, "Sprite bind pose")?;
            if self.version < (2018, 2, 0) {
                self.skip_mesh_records(32, "Sprite source skin")?;
            }
        }

        let texture_rect = self.read_rect("Sprite texture rect")?;
        let texture_rect_offset = self.read_vector2("Sprite texture rect offset")?;
        let atlas_rect_offset = if self.version >= (5, 6, 0) {
            self.read_vector2("Sprite atlas rect offset")?
        } else {
            zero_vector2()
        };
        let settings = SpriteSettings::from_raw(self.reader.read_u32()?);
        let uv_transform = if self.version >= (4, 5, 0) {
            self.read_vector4("Sprite UV transform")?
        } else {
            zero_vector4()
        };
        let downscale_multiplier = if self.version.0 >= 2017 {
            self.read_finite_f32("Sprite downscale multiplier")?
        } else {
            0.0
        };
        let mesh_triangles = if settings.packing_mode == SpritePackingMode::Tight {
            pending_mesh.into_triangles(self.limits)?
        } else {
            Vec::new()
        };

        Ok(SpriteRenderData {
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
        })
    }

    fn read_modern_sprite_mesh(&mut self) -> Result<PendingSpriteMesh> {
        let count = self.read_count("Sprite submesh")?;
        let mut record_size = 12_u64;
        if self.version < (4, 0, 0) {
            record_size = record_size
                .checked_add(4)
                .ok_or_else(|| Error::invalid_data("Sprite submesh size overflowed"))?;
        }
        if self.version >= (2017, 3, 0) {
            record_size = record_size
                .checked_add(4)
                .ok_or_else(|| Error::invalid_data("Sprite submesh size overflowed"))?;
        }
        if self.version.0 >= 3 {
            record_size = record_size
                .checked_add(32)
                .ok_or_else(|| Error::invalid_data("Sprite submesh size overflowed"))?;
        }
        self.charge_mesh_elements(count, record_size, "Sprite submesh")?;
        let mut submeshes = reserve_vec(count, "Sprite submeshes")?;
        for _ in 0..count {
            let first_byte = self.reader.read_u32()?;
            let index_count = self.reader.read_u32()?;
            self.reader.read_i32()?; // topology: SpriteHelper treats indices as triangles.
            if self.version < (4, 0, 0) {
                self.reader.read_u32()?;
            }
            if self.version >= (2017, 3, 0) {
                self.reader.read_u32()?; // baseVertex is ignored by managed SpriteHelper.
            }
            let (first_vertex, vertex_count) = if self.version.0 >= 3 {
                let first_vertex = self.reader.read_u32()?;
                let vertex_count = self.reader.read_u32()?;
                // Six floats: the AABB's centre and extent. The Mesh reader
                // gets this right in three places; this one skipped eight,
                // which shifted every field after the first submesh -- the
                // index buffer, the vertex data, the texture rect and the
                // packing settings. Only a sprite carrying a submesh reaches
                // it, which is every tight-packed sprite.
                self.skip(24, "Sprite submesh AABB")?;
                (first_vertex, vertex_count)
            } else {
                (0, 0)
            };
            submeshes.push(SpriteSubMesh {
                first_byte,
                index_count,
                first_vertex,
                vertex_count,
            });
        }
        let index_buffer = self.read_mesh_blob("Sprite index buffer")?;
        let vertex_data = self.read_vertex_data()?;
        Ok(PendingSpriteMesh::Modern {
            submeshes,
            index_buffer,
            vertex_data,
        })
    }

    fn read_legacy_sprite_mesh(&mut self) -> Result<PendingSpriteMesh> {
        let count = self.read_count("Sprite vertex")?;
        let vertex_size = if self.version <= (4, 3, u32::MAX) {
            20
        } else {
            12
        };
        self.charge_mesh_elements(count, vertex_size, "Sprite vertex")?;
        let mut vertices = reserve_vec(count, "Sprite vertices")?;
        for _ in 0..count {
            let vertex = Vector2 {
                x: self.read_finite_f32("Sprite vertex x")?,
                y: self.read_finite_f32("Sprite vertex y")?,
            };
            self.read_finite_f32("Sprite vertex z")?;
            if self.version <= (4, 3, u32::MAX) {
                self.read_finite_f32("Sprite vertex u")?;
                self.read_finite_f32("Sprite vertex v")?;
            }
            vertices.push(vertex);
        }

        let index_count = self.read_count("Sprite index")?;
        self.charge_mesh_elements(index_count, 2, "Sprite index")?;
        let mut indices = reserve_vec(index_count, "Sprite indices")?;
        for _ in 0..index_count {
            indices.push(self.reader.read_u16()?);
        }
        self.align(4)?;
        Ok(PendingSpriteMesh::Legacy { vertices, indices })
    }

    fn read_vertex_data(&mut self) -> Result<SpriteVertexData> {
        if self.version.0 < 2018 {
            self.charge_mesh(4, "Sprite vertex current channels")?;
            self.reader.read_u32()?;
        }
        self.charge_mesh(4, "Sprite vertex count")?;
        let vertex_count = usize::try_from(self.reader.read_u32()?)
            .map_err(|_| Error::invalid_data("Sprite vertex count does not fit in usize"))?;
        if vertex_count > self.limits.maximum_array_elements {
            return Err(Error::invalid_data(format!(
                "Sprite vertex count {vertex_count} exceeds limit {}",
                self.limits.maximum_array_elements
            )));
        }
        if self.version.0 >= 4 {
            let count = self.read_count("Sprite vertex channel")?;
            self.charge_mesh_elements(count, 4, "Sprite vertex channel")?;
            let mut channels = reserve_vec(count, "Sprite vertex channels")?;
            for _ in 0..count {
                channels.push(SpriteVertexChannel {
                    stream: self.reader.read_u8()?,
                    offset: self.reader.read_u8()?,
                    format: self.reader.read_u8()?,
                    dimension: self.reader.read_u8()? & 0xf,
                });
            }
            if self.version.0 < 5 {
                return Err(Error::unsupported(
                    "Sprite VertexData stream tables before Unity 5 are not implemented",
                ));
            }
            let data = self.read_mesh_blob("Sprite vertex data")?;
            return Ok(SpriteVertexData {
                vertex_count,
                version: self.version,
                channels,
                data,
            });
        }
        Err(Error::unsupported(
            "Sprite VertexData without channel descriptors is not implemented",
        ))
    }

    fn read_string_array(&mut self, field: &str) -> Result<Vec<String>> {
        let count = self.read_count(field)?;
        let mut values = reserve_vec(count, field)?;
        for _ in 0..count {
            values.push(self.read_aligned_string(field)?);
        }
        Ok(values)
    }

    fn read_rect(&mut self, field: &str) -> Result<RectF> {
        Ok(RectF {
            x: self.read_finite_f32(field)?,
            y: self.read_finite_f32(field)?,
            width: self.read_finite_f32(field)?,
            height: self.read_finite_f32(field)?,
        })
    }

    fn read_vector2(&mut self, field: &str) -> Result<Vector2> {
        Ok(Vector2 {
            x: self.read_finite_f32(field)?,
            y: self.read_finite_f32(field)?,
        })
    }

    fn read_vector4(&mut self, field: &str) -> Result<Vector4> {
        Ok(Vector4 {
            x: self.read_finite_f32(field)?,
            y: self.read_finite_f32(field)?,
            z: self.read_finite_f32(field)?,
            w: self.read_finite_f32(field)?,
        })
    }

    fn read_finite_f32(&mut self, field: &str) -> Result<f32> {
        let value = self.reader.read_f32()?;
        if !value.is_finite() {
            return Err(Error::invalid_data(format!("{field} is not finite")));
        }
        Ok(value)
    }

    fn read_pptr(&mut self) -> Result<ObjectReference> {
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
        self.total_string_bytes = self
            .total_string_bytes
            .checked_add(length)
            .ok_or_else(|| Error::invalid_data("Sprite string byte budget overflowed"))?;
        if self.total_string_bytes > self.limits.maximum_total_string_bytes {
            return Err(Error::invalid_data(format!(
                "Sprite strings total {} bytes, exceeding limit {}",
                self.total_string_bytes, self.limits.maximum_total_string_bytes
            )));
        }
        let value = self.reader.read_utf8(length)?;
        if length != 0 {
            self.align(4)?;
        }
        Ok(value)
    }

    fn read_count(&mut self, field: &str) -> Result<usize> {
        let count = checked_length(self.reader.read_i32()?, field)?;
        if count > self.limits.maximum_array_elements {
            return Err(Error::invalid_data(format!(
                "{field} count {count} exceeds limit {}",
                self.limits.maximum_array_elements
            )));
        }
        Ok(count)
    }

    fn read_mesh_blob(&mut self, field: &str) -> Result<Vec<u8>> {
        let length = checked_length(self.reader.read_i32()?, field)?;
        let length_u64 = u64::try_from(length)
            .map_err(|_| Error::invalid_data(format!("{field} length does not fit in u64")))?;
        self.charge_mesh(length_u64, field)?;
        let bytes = self.reader.read_bytes(length)?;
        self.align(4)?;
        Ok(bytes)
    }

    fn skip_mesh_records(&mut self, record_size: u64, field: &str) -> Result<()> {
        let count = self.read_count(field)?;
        self.skip_mesh_elements(count, record_size, field)
    }

    fn skip_mesh_elements(&mut self, count: usize, size: u64, field: &str) -> Result<()> {
        let count = u64::try_from(count)
            .map_err(|_| Error::invalid_data(format!("{field} count does not fit in u64")))?;
        let length = count
            .checked_mul(size)
            .ok_or_else(|| Error::invalid_data(format!("{field} byte size overflowed")))?;
        self.skip_mesh(length, field)
    }

    fn charge_mesh_elements(&mut self, count: usize, size: u64, field: &str) -> Result<()> {
        let count = u64::try_from(count)
            .map_err(|_| Error::invalid_data(format!("{field} count does not fit in u64")))?;
        let length = count
            .checked_mul(size)
            .ok_or_else(|| Error::invalid_data(format!("{field} byte size overflowed")))?;
        self.charge_mesh(length, field)
    }

    fn skip_mesh(&mut self, length: u64, field: &str) -> Result<()> {
        self.charge_mesh(length, field)?;
        self.skip(length, field)
    }

    fn charge_mesh(&mut self, length: u64, field: &str) -> Result<()> {
        self.mesh_bytes = self
            .mesh_bytes
            .checked_add(length)
            .ok_or_else(|| Error::invalid_data("Sprite mesh byte budget overflowed"))?;
        if self.mesh_bytes > self.limits.maximum_mesh_bytes {
            return Err(Error::invalid_data(format!(
                "{field} makes Sprite mesh data total {} bytes, exceeding limit {}",
                self.mesh_bytes, self.limits.maximum_mesh_bytes
            )));
        }
        Ok(())
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
            .ok_or_else(|| Error::invalid_data("Sprite alignment position overflowed"))?;
        let remainder = absolute % alignment;
        if remainder == 0 {
            return Ok(());
        }
        self.skip(alignment - remainder, "Sprite alignment")
    }
}

impl PendingSpriteMesh {
    fn into_triangles(self, limits: SpriteReadLimits) -> Result<Vec<[Vector2; 3]>> {
        match self {
            Self::Legacy { vertices, indices } => {
                let triangle_count = indices.len() / 3;
                let mut triangles = reserve_sprite_triangles(triangle_count, limits)?;
                for indices in indices.chunks_exact(3) {
                    let Some(first) = vertices.get(usize::from(indices[0])).copied() else {
                        return Ok(Vec::new());
                    };
                    let Some(second) = vertices.get(usize::from(indices[1])).copied() else {
                        return Ok(Vec::new());
                    };
                    let Some(third) = vertices.get(usize::from(indices[2])).copied() else {
                        return Ok(Vec::new());
                    };
                    triangles.push([first, second, third]);
                }
                Ok(triangles)
            }
            Self::Modern {
                submeshes,
                index_buffer,
                vertex_data,
            } => modern_sprite_triangles(submeshes, &index_buffer, &vertex_data, limits),
        }
    }
}

fn build_sprite_vertex_streams(
    channels: &[SpriteVertexChannel],
    vertex_count: usize,
    version: (u32, u32, u32),
) -> Result<Vec<SpriteVertexStream>> {
    let maximum_stream = channels
        .iter()
        .map(|channel| channel.stream)
        .max()
        .ok_or_else(|| Error::invalid_data("Sprite VertexData has no channels"))?;
    let stream_count = usize::from(maximum_stream) + 1;
    let mut streams = reserve_vec(stream_count, "Sprite vertex streams")?;
    let mut offset = 0_usize;
    for stream_index in 0..stream_count {
        let mut stride = 0_usize;
        for channel in channels
            .iter()
            .filter(|channel| usize::from(channel.stream) == stream_index && channel.dimension > 0)
        {
            let format_size = vertex_format_size(channel.format, version)?;
            stride = stride
                .checked_add(usize::from(channel.dimension) * format_size)
                .ok_or_else(|| Error::invalid_data("Sprite vertex stream stride overflowed"))?;
        }
        streams.push(SpriteVertexStream { offset, stride });
        offset = vertex_count
            .checked_mul(stride)
            .and_then(|length| offset.checked_add(length))
            .and_then(|end| end.checked_add(15))
            .map(|end| end & !15)
            .ok_or_else(|| Error::invalid_data("Sprite vertex stream range overflowed"))?;
    }
    Ok(streams)
}

fn modern_sprite_triangles(
    submeshes: Vec<SpriteSubMesh>,
    index_buffer: &[u8],
    vertex_data: &SpriteVertexData,
    limits: SpriteReadLimits,
) -> Result<Vec<[Vector2; 3]>> {
    let streams = build_sprite_vertex_streams(
        &vertex_data.channels,
        vertex_data.vertex_count,
        vertex_data.version,
    )?;
    let Some(position_channel) = vertex_data.channels.first().copied() else {
        return Ok(Vec::new());
    };
    let Some(position_stream) = streams.get(usize::from(position_channel.stream)) else {
        return Ok(Vec::new());
    };
    if position_stream.stride < 12
        || usize::from(position_channel.offset) + 12 > position_stream.stride
    {
        return Ok(Vec::new());
    }

    let total_triangles = submeshes.iter().try_fold(0_usize, |total, submesh| {
        total
            .checked_add(usize::try_from(submesh.index_count / 3).unwrap_or(usize::MAX))
            .ok_or_else(|| Error::invalid_data("Sprite triangle count overflowed"))
    })?;
    let mut triangles = reserve_sprite_triangles(total_triangles, limits)?;
    let mut total_vertices = 0_usize;
    for submesh in submeshes {
        let vertex_count = usize::try_from(submesh.vertex_count)
            .map_err(|_| Error::invalid_data("Sprite submesh vertex count is too large"))?;
        total_vertices = total_vertices
            .checked_add(vertex_count)
            .ok_or_else(|| Error::invalid_data("Sprite submesh vertex total overflowed"))?;
        if total_vertices > limits.maximum_array_elements {
            return Err(Error::invalid_data(format!(
                "Sprite submesh vertices total {total_vertices}, exceeding limit {}",
                limits.maximum_array_elements
            )));
        }
        let mut vertices = reserve_vec(vertex_count, "Sprite submesh vertices")?;
        let first_vertex = usize::try_from(submesh.first_vertex)
            .map_err(|_| Error::invalid_data("Sprite submesh first vertex is too large"))?;
        let start = first_vertex
            .checked_mul(position_stream.stride)
            .and_then(|offset| position_stream.offset.checked_add(offset))
            .and_then(|offset| offset.checked_add(usize::from(position_channel.offset)))
            .ok_or_else(|| Error::invalid_data("Sprite vertex data offset overflowed"))?;
        for vertex_index in 0..vertex_count {
            let offset = vertex_index
                .checked_mul(position_stream.stride)
                .and_then(|offset| start.checked_add(offset))
                .ok_or_else(|| Error::invalid_data("Sprite vertex position overflowed"))?;
            let Some(end) = offset.checked_add(12) else {
                return Ok(Vec::new());
            };
            let Some(bytes) = vertex_data.data.get(offset..end) else {
                return Ok(Vec::new());
            };
            let x = f32::from_le_bytes(bytes[0..4].try_into().expect("four-byte x slice"));
            let y = f32::from_le_bytes(bytes[4..8].try_into().expect("four-byte y slice"));
            let z = f32::from_le_bytes(bytes[8..12].try_into().expect("four-byte z slice"));
            if !x.is_finite() || !y.is_finite() || !z.is_finite() {
                return Ok(Vec::new());
            }
            vertices.push(Vector2 { x, y });
        }

        let first_index = usize::try_from(submesh.first_byte)
            .map_err(|_| Error::invalid_data("Sprite submesh first byte is too large"))?;
        let triangle_count = usize::try_from(submesh.index_count / 3)
            .map_err(|_| Error::invalid_data("Sprite submesh triangle count is too large"))?;
        for triangle_index in 0..triangle_count {
            let offset = triangle_index
                .checked_mul(6)
                .and_then(|offset| first_index.checked_add(offset))
                .ok_or_else(|| Error::invalid_data("Sprite index buffer offset overflowed"))?;
            let Some(end) = offset.checked_add(6) else {
                return Ok(Vec::new());
            };
            let Some(bytes) = index_buffer.get(offset..end) else {
                return Ok(Vec::new());
            };
            let mut triangle = [Vector2 { x: 0.0, y: 0.0 }; 3];
            for (slot, pair) in triangle.iter_mut().zip(bytes.chunks_exact(2)) {
                let index = u32::from(u16::from_le_bytes([pair[0], pair[1]]));
                let Some(local) = index.checked_sub(submesh.first_vertex) else {
                    return Ok(Vec::new());
                };
                let Some(vertex) = vertices.get(usize::try_from(local).unwrap_or(usize::MAX))
                else {
                    return Ok(Vec::new());
                };
                *slot = *vertex;
            }
            triangles.push(triangle);
        }
    }
    Ok(triangles)
}

fn reserve_sprite_triangles(
    triangle_count: usize,
    limits: SpriteReadLimits,
) -> Result<Vec<[Vector2; 3]>> {
    if triangle_count > limits.maximum_array_elements {
        return Err(Error::invalid_data(format!(
            "Sprite triangle count {triangle_count} exceeds limit {}",
            limits.maximum_array_elements
        )));
    }
    let bytes = triangle_count
        .checked_mul(std::mem::size_of::<[Vector2; 3]>())
        .and_then(|bytes| u64::try_from(bytes).ok())
        .ok_or_else(|| Error::invalid_data("Sprite triangle allocation size overflowed"))?;
    if bytes > limits.maximum_working_bytes {
        return Err(Error::invalid_data(format!(
            "Sprite triangles require {bytes} working bytes, exceeding limit {}",
            limits.maximum_working_bytes
        )));
    }
    reserve_vec(triangle_count, "Sprite triangles")
}

fn reserve_vec<T>(count: usize, field: &str) -> Result<Vec<T>> {
    let mut output = Vec::new();
    output.try_reserve_exact(count).map_err(|error| {
        Error::invalid_data(format!("cannot allocate {count} {field} entries: {error}"))
    })?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use crate::error::Error;
    use crate::loader::{AssetCollection, AssetLoadLimits, LoadedSerializedFile};
    use crate::serialized::SerializedFile;
    use crate::source::Region;
    use crate::sprite_atlas::SPRITE_ATLAS_CLASS_ID;
    use crate::texture::{RgbaImage, TextureFormat, TextureReadLimits};

    use super::{
        SPRITE_CLASS_ID, SpritePackingMode, SpriteReadLimits, decode_sprite_rgba8,
        decode_sprite_rgba8_by_file_index, is_convex_quad, read_sprite, resize_bicubic_rgba8,
        shared_edge_quad, sprite_resize_dimensions,
    };

    /// The coverage fast path may only skip masking for shapes it has proved
    /// convex. A bowtie -- two triangles on the same side of their shared edge
    /// -- must not qualify, or the mask it skips would have cut real pixels.
    #[test]
    fn only_a_convex_quad_can_skip_the_tight_mask() {
        let unit_square = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)];
        assert!(is_convex_quad(&unit_square));
        assert!(is_convex_quad(&[
            (0.0, 1.0),
            (1.0, 1.0),
            (1.0, 0.0),
            (0.0, 0.0)
        ]));

        // Self-intersecting: the diagonal's endpoints are not opposite corners.
        assert!(!is_convex_quad(&[
            (0.0, 0.0),
            (1.0, 1.0),
            (1.0, 0.0),
            (0.0, 1.0)
        ]));
        // Reflex vertex.
        assert!(!is_convex_quad(&[
            (0.0, 0.0),
            (2.0, 0.0),
            (0.5, 0.5),
            (0.0, 2.0)
        ]));
        // Collinear edge leaves the shape unproven rather than assumed convex.
        assert!(!is_convex_quad(&[
            (0.0, 0.0),
            (1.0, 0.0),
            (2.0, 0.0),
            (0.0, 1.0)
        ]));

        // Two triangles over the unit square's main diagonal.
        let lower = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0)];
        let upper = [(0.0, 0.0), (1.0, 1.0), (0.0, 1.0)];
        let quad = shared_edge_quad(&lower, &upper).expect("triangles share an edge");
        assert!(is_convex_quad(&quad));

        // Triangles that share only one vertex are not a quad at all.
        assert!(
            shared_edge_quad(&lower, &[(0.0, 0.0), (-1.0, 0.0), (-1.0, -1.0)]).is_none(),
            "one shared vertex is not a shared edge"
        );
    }

    #[test]
    fn parses_modern_metadata_crops_flips_and_applies_alpha_texture() {
        let color = rgba_grid(4, 3, |x, y| [x + y * 10, 40, 80, 255]);
        let alpha = rgba_grid(4, 3, |x, y| {
            let value = 3 * (x + y * 10);
            [value, value, value, 255]
        });
        let sprite_object = modern_sprite_object(
            2,
            3,
            [1.0, 0.0, 2.0, 2.0],
            2,
            ObjectReferenceFields::default(),
        );
        let file = parse_asset(
            "2022.3.62f1",
            &[
                (SPRITE_CLASS_ID, 1, sprite_object),
                (28, 2, texture_object(4, 3, &color)),
                (28, 3, texture_object(4, 3, &alpha)),
            ],
        );
        let collection = collection_with(file.clone());

        let sprite = read_sprite(&file, 0, SpriteReadLimits::default()).unwrap();
        let image = decode_sprite_rgba8(
            &collection,
            &file,
            &sprite,
            SpriteReadLimits::default(),
            TextureReadLimits::default(),
        )
        .unwrap();

        assert_eq!(sprite.name, "icon");
        assert_f32(sprite.rect.width, 2.0);
        assert_f32(sprite.pivot.x, 0.25);
        assert_f32(sprite.pivot.y, 0.75);
        assert_f32(sprite.border.x, 1.0);
        assert_f32(sprite.border.y, 2.0);
        assert_f32(sprite.border.z, 3.0);
        assert_f32(sprite.border.w, 4.0);
        assert_eq!(
            sprite.render_data.settings.packing_mode,
            SpritePackingMode::Rectangle
        );
        assert_eq!((image.width, image.height), (2, 2));
        assert_eq!(
            image.pixels,
            [
                11, 40, 80, 33, 12, 40, 80, 36, // source row 1 after final flip
                1, 40, 80, 3, 2, 40, 80, 6,
            ]
        );

        let count_limit = SpriteReadLimits {
            maximum_array_elements: 0,
            ..SpriteReadLimits::default()
        };
        assert!(read_sprite(&file, 0, count_limit).is_err());
        let mesh_limit = SpriteReadLimits {
            maximum_mesh_bytes: 7,
            ..SpriteReadLimits::default()
        };
        assert!(read_sprite(&file, 0, mesh_limit).is_err());
    }

    #[test]
    fn bicubic_resampling_matches_imagesharp_3_1_8_bytes() {
        let source = RgbaImage {
            width: 4,
            height: 4,
            pixels: imagesharp_oracle_grid(4, 4),
        };
        let image =
            resize_bicubic_rgba8(&source, 2, 2, SpriteReadLimits::default(), 0, "oracle").unwrap();

        // Generated with ImageSharp 3.1.8 Image<Bgra32>.Mutate(x =>
        // x.Resize(2, 2, KnownResamplers.Bicubic)). This also locks down
        // ImageSharp's default premultiplied-alpha interpolation.
        assert_eq!(
            image.pixels,
            [
                64, 39, 35, 73, 137, 68, 55, 115, // first output row
                164, 90, 58, 129, 160, 120, 78, 171,
            ]
        );
    }

    #[test]
    fn downscale_truncates_dimensions_before_crop_and_preserves_display_row_order() {
        let source = imagesharp_oracle_grid(5, 3);
        let sprite_object = modern_sprite_object(
            2,
            0,
            [0.0, 0.0, 5.0, 3.0],
            2,
            ObjectReferenceFields::default(),
        );
        let file = parse_asset(
            "2022.3.62f1",
            &[
                (SPRITE_CLASS_ID, 1, sprite_object),
                (28, 2, texture_object(5, 3, &source)),
            ],
        );
        let collection = collection_with(file.clone());
        let mut sprite = read_sprite(&file, 0, SpriteReadLimits::default()).unwrap();
        sprite.render_data.downscale_multiplier = 2.0;

        let image = decode_sprite_rgba8(
            &collection,
            &file,
            &sprite,
            SpriteReadLimits::default(),
            TextureReadLimits::default(),
        )
        .unwrap();

        // C# truncates 5 / 2 and 3 / 2 to 2x1. These bytes are the matching
        // ImageSharp 3.1.8 full-texture resize oracle.
        assert_eq!((image.width, image.height), (2, 1));
        assert_eq!(image.pixels, [108, 58, 45, 92, 145, 94, 69, 144]);

        let source = imagesharp_oracle_grid(4, 4);
        let sprite_object = modern_sprite_object(
            2,
            0,
            [0.0, 0.0, 4.0, 4.0],
            2,
            ObjectReferenceFields::default(),
        );
        let file = parse_asset(
            "2022.3.62f1",
            &[
                (SPRITE_CLASS_ID, 1, sprite_object),
                (28, 2, texture_object(4, 4, &source)),
            ],
        );
        let collection = collection_with(file.clone());
        let mut sprite = read_sprite(&file, 0, SpriteReadLimits::default()).unwrap();
        sprite.render_data.downscale_multiplier = 2.0;
        let image = decode_sprite_rgba8(
            &collection,
            &file,
            &sprite,
            SpriteReadLimits::default(),
            TextureReadLimits::default(),
        )
        .unwrap();
        assert_eq!(
            image.pixels,
            [
                164, 90, 58, 129, 160, 120, 78, 171, // vertically flipped
                64, 39, 35, 73, 137, 68, 55, 115,
            ]
        );
    }

    #[test]
    fn differently_sized_resident_alpha_crop_uses_export_bicubic_semantics() {
        let color = rgba_grid(2, 2, |x, y| [10 + x * 10 + y * 20, 1, 2, 7]);
        let alpha = rgba_grid(1, 3, |_, y| {
            let value = y * 120;
            [value, value, value, 255]
        });
        let sprite_object = modern_sprite_object(
            2,
            3,
            [0.0, 0.0, 2.0, 3.0],
            2,
            ObjectReferenceFields::default(),
        );
        let file = parse_asset(
            "2022.3.62f1",
            &[
                (SPRITE_CLASS_ID, 1, sprite_object),
                (28, 2, texture_object(2, 2, &color)),
                (28, 3, texture_object(1, 3, &alpha)),
            ],
        );
        let collection = collection_with(file.clone());
        let sprite = read_sprite(&file, 0, SpriteReadLimits::default()).unwrap();

        let image = decode_sprite_rgba8(
            &collection,
            &file,
            &sprite,
            SpriteReadLimits::default(),
            TextureReadLimits::default(),
        )
        .unwrap();
        assert_eq!((image.width, image.height), (2, 2));
        assert_eq!(
            image.pixels,
            [
                30, 1, 2, 203, 40, 1, 2, 203, // flipped color and mask row
                10, 1, 2, 37, 20, 1, 2, 37,
            ]
        );

        let working_limit = SpriteReadLimits {
            maximum_working_bytes: 43,
            ..SpriteReadLimits::default()
        };
        assert!(matches!(
            decode_sprite_rgba8(
                &collection,
                &file,
                &sprite,
                working_limit,
                TextureReadLimits::default(),
            ),
            Err(Error::InvalidData(message)) if message.contains("alpha mask")
                && message.contains("working bytes")
        ));
    }

    #[test]
    fn resampling_rejects_bad_dimensions_and_honors_pixel_and_working_budgets() {
        assert_eq!(sprite_resize_dimensions(4, 4, 0.0).unwrap(), None);
        assert_eq!(sprite_resize_dimensions(4, 4, -2.0).unwrap(), None);
        assert_eq!(sprite_resize_dimensions(4, 4, 1.0).unwrap(), None);
        assert!(sprite_resize_dimensions(1, 1, 2.0).is_err());
        assert!(sprite_resize_dimensions(4, 4, 1.0e-20).is_err());

        let source = imagesharp_oracle_grid(4, 4);
        let sprite_object = modern_sprite_object(
            2,
            0,
            [0.0, 0.0, 4.0, 4.0],
            2,
            ObjectReferenceFields::default(),
        );
        let file = parse_asset(
            "2022.3.62f1",
            &[
                (SPRITE_CLASS_ID, 1, sprite_object),
                (28, 2, texture_object(4, 4, &source)),
            ],
        );
        let collection = collection_with(file.clone());
        let mut sprite = read_sprite(&file, 0, SpriteReadLimits::default()).unwrap();
        sprite.render_data.downscale_multiplier = 2.0;

        let pixel_limit = SpriteReadLimits {
            maximum_output_pixels: 3,
            ..SpriteReadLimits::default()
        };
        assert!(matches!(
            decode_sprite_rgba8(
                &collection,
                &file,
                &sprite,
                pixel_limit,
                TextureReadLimits::default(),
            ),
            Err(Error::InvalidData(message)) if message.contains("4 pixels")
        ));

        let working_limit = SpriteReadLimits {
            maximum_working_bytes: 31,
            ..SpriteReadLimits::default()
        };
        assert!(matches!(
            decode_sprite_rgba8(
                &collection,
                &file,
                &sprite,
                working_limit,
                TextureReadLimits::default(),
            ),
            Err(Error::InvalidData(message)) if message.contains("32 working bytes")
        ));

        let malformed_target = resize_bicubic_rgba8(
            &RgbaImage {
                width: 1,
                height: 1,
                pixels: vec![0; 4],
            },
            0,
            1,
            SpriteReadLimits::default(),
            0,
            "bad target",
        )
        .unwrap_err();
        assert!(matches!(
            malformed_target,
            Error::InvalidData(message) if message.contains("must be positive")
        ));
    }

    #[test]
    fn clamps_edges_and_rejects_empty_or_oversized_crops() {
        let color = rgba_grid(4, 3, |x, y| [x, y, 0, 255]);
        let sprite_object = modern_sprite_object(
            2,
            0,
            [3.0, 2.0, 8.0, 9.0],
            2,
            ObjectReferenceFields::default(),
        );
        let file = parse_asset(
            "2022.3.62f1",
            &[
                (SPRITE_CLASS_ID, 1, sprite_object),
                (28, 2, texture_object(4, 3, &color)),
            ],
        );
        let collection = collection_with(file.clone());
        let sprite = read_sprite(&file, 0, SpriteReadLimits::default()).unwrap();

        let image = decode_sprite_rgba8(
            &collection,
            &file,
            &sprite,
            SpriteReadLimits::default(),
            TextureReadLimits::default(),
        )
        .unwrap();
        assert_eq!((image.width, image.height), (1, 1));
        assert_eq!(image.pixels, [3, 2, 0, 255]);

        let limits = SpriteReadLimits {
            maximum_output_bytes: 3,
            ..SpriteReadLimits::default()
        };
        assert!(
            decode_sprite_rgba8(
                &collection,
                &file,
                &sprite,
                limits,
                TextureReadLimits::default(),
            )
            .is_err()
        );

        let mut outside = sprite;
        outside.render_data.texture_rect.x = 4.0;
        assert!(
            decode_sprite_rgba8(
                &collection,
                &file,
                &outside,
                SpriteReadLimits::default(),
                TextureReadLimits::default(),
            )
            .is_err()
        );
    }

    #[test]
    fn follows_the_5_4_1_patch_pivot_gate_and_rejects_stripped_versions() {
        let without_pivot = legacy_sprite_object(false);
        let final_file = parse_asset("5.4.1f1", &[(SPRITE_CLASS_ID, 1, without_pivot)]);
        let final_sprite = read_sprite(&final_file, 0, SpriteReadLimits::default()).unwrap();
        assert_f32(final_sprite.pivot.x, 0.5);
        assert_f32(final_sprite.pivot.y, 0.5);

        let with_pivot = legacy_sprite_object(true);
        let patch_file = parse_asset("5.4.1p3", &[(SPRITE_CLASS_ID, 1, with_pivot)]);
        let patch_sprite = read_sprite(&patch_file, 0, SpriteReadLimits::default()).unwrap();
        assert_f32(patch_sprite.pivot.x, 0.25);
        assert_f32(patch_sprite.pivot.y, 0.75);

        let stripped = parse_asset("0.0.0", &[(SPRITE_CLASS_ID, 1, Vec::new())]);
        assert!(matches!(
            read_sprite(&stripped, 0, SpriteReadLimits::default()),
            Err(Error::Unsupported(message)) if message.contains("Unity version")
        ));
    }

    #[test]
    fn packed_rectangle_rotations_match_the_csharp_cut_image_order() {
        let texture = rgba_grid(3, 2, |x, y| [1 + x + y * 3, 0, 0, 255]);
        for (settings, dimensions, expected_red) in [
            (3, (3, 2), vec![4, 5, 6, 1, 2, 3]),
            (7, (3, 2), vec![6, 5, 4, 3, 2, 1]),
            (11, (3, 2), vec![1, 2, 3, 4, 5, 6]),
            (15, (3, 2), vec![3, 2, 1, 6, 5, 4]),
            (19, (2, 3), vec![1, 4, 2, 5, 3, 6]),
            // Rotation bits are ignored when the packed flag is clear.
            (6, (3, 2), vec![4, 5, 6, 1, 2, 3]),
        ] {
            let sprite_object = modern_sprite_object(
                2,
                0,
                [0.0, 0.0, 3.0, 2.0],
                settings,
                ObjectReferenceFields::default(),
            );
            let file = parse_asset(
                "2022.3.62f1",
                &[
                    (SPRITE_CLASS_ID, 1, sprite_object),
                    (28, 2, texture_object(3, 2, &texture)),
                ],
            );
            let collection = collection_with(file.clone());
            let sprite = read_sprite(&file, 0, SpriteReadLimits::default()).unwrap();
            let image = decode_sprite_rgba8(
                &collection,
                &file,
                &sprite,
                SpriteReadLimits::default(),
                TextureReadLimits::default(),
            )
            .unwrap();
            assert_eq!(
                (image.width, image.height),
                dimensions,
                "settings={settings}"
            );
            let red = image
                .pixels
                .chunks_exact(4)
                .map(|pixel| pixel[0])
                .collect::<Vec<_>>();
            assert_eq!(red, expected_red, "settings={settings}");
        }

        let sprite_object = modern_sprite_object(
            2,
            0,
            [0.0, 0.0, 3.0, 2.0],
            19,
            ObjectReferenceFields::default(),
        );
        let file = parse_asset(
            "2022.3.62f1",
            &[
                (SPRITE_CLASS_ID, 1, sprite_object),
                (28, 2, texture_object(3, 2, &texture)),
            ],
        );
        let collection = collection_with(file.clone());
        let sprite = read_sprite(&file, 0, SpriteReadLimits::default()).unwrap();
        let limits = SpriteReadLimits {
            maximum_working_bytes: 47,
            ..SpriteReadLimits::default()
        };
        assert!(matches!(
            decode_sprite_rgba8(
                &collection,
                &file,
                &sprite,
                limits,
                TextureReadLimits::default(),
            ),
            Err(Error::InvalidData(message)) if message.contains("working bytes")
        ));
    }

    #[test]
    fn rejects_unknown_rotation_and_rectangularly_falls_back_for_empty_tight_meshes() {
        let texture = texture_object(1, 1, &[1, 2, 3, 4]);
        let sprite_object = modern_sprite_object(
            2,
            0,
            [0.0, 0.0, 1.0, 1.0],
            23,
            ObjectReferenceFields::default(),
        );
        let file = parse_asset(
            "2022.3.62f1",
            &[
                (SPRITE_CLASS_ID, 1, sprite_object),
                (28, 2, texture.clone()),
            ],
        );
        let collection = collection_with(file.clone());
        let sprite = read_sprite(&file, 0, SpriteReadLimits::default()).unwrap();
        let error = decode_sprite_rgba8(
            &collection,
            &file,
            &sprite,
            SpriteReadLimits::default(),
            TextureReadLimits::default(),
        )
        .unwrap_err();
        assert!(
            matches!(error, Error::Unsupported(message) if message.contains("rotation")),
            "expected unsupported rotation error"
        );

        let file = parse_asset(
            "2022.3.62f1",
            &[
                (
                    SPRITE_CLASS_ID,
                    1,
                    modern_sprite_object(
                        2,
                        0,
                        [0.0, 0.0, 1.0, 1.0],
                        0,
                        ObjectReferenceFields::default(),
                    ),
                ),
                (28, 2, texture),
            ],
        );
        let collection = collection_with(file.clone());
        let sprite = read_sprite(&file, 0, SpriteReadLimits::default()).unwrap();
        let image = decode_sprite_rgba8(
            &collection,
            &file,
            &sprite,
            SpriteReadLimits::default(),
            TextureReadLimits::default(),
        )
        .unwrap();
        assert_eq!(image.pixels, [1, 2, 3, 4]);
    }

    #[test]
    fn masks_legacy_and_modern_tight_meshes_and_enforces_raster_budgets() {
        let pixels = [10, 1, 1, 255, 20, 2, 2, 255, 30, 3, 3, 255, 40, 4, 4, 255];
        for (version, sprite_object) in [
            ("5.5.0f1", legacy_tight_sprite_object([0, 1, 2])),
            ("2022.3.62f1", modern_tight_sprite_object([0, 1, 2])),
        ] {
            let texture = if version.starts_with("5.5") {
                legacy_texture_object_5_5(2, 2, &pixels)
            } else {
                texture_object(2, 2, &pixels)
            };
            let file = parse_asset(
                version,
                &[(SPRITE_CLASS_ID, 1, sprite_object), (28, 2, texture)],
            );
            let collection = collection_with(file.clone());
            let sprite = read_sprite(&file, 0, SpriteReadLimits::default()).unwrap();
            assert_eq!(sprite.render_data.mesh_triangles.len(), 1);
            let image = decode_sprite_rgba8(
                &collection,
                &file,
                &sprite,
                SpriteReadLimits::default(),
                TextureReadLimits::default(),
            )
            .unwrap();
            assert_eq!(
                image.pixels,
                [30, 3, 3, 255, 0, 0, 0, 0, 10, 1, 1, 255, 20, 2, 2, 255],
                "Unity version {version}"
            );

            let raster_limit = SpriteReadLimits {
                maximum_raster_operations: 3,
                ..SpriteReadLimits::default()
            };
            assert!(matches!(
                decode_sprite_rgba8(
                    &collection,
                    &file,
                    &sprite,
                    raster_limit,
                    TextureReadLimits::default(),
                ),
                Err(Error::InvalidData(message)) if message.contains("pixel tests")
            ));
        }

        let file = parse_asset(
            "5.5.0f1",
            &[
                (SPRITE_CLASS_ID, 1, legacy_tight_sprite_object([0, 1, 99])),
                (28, 2, legacy_texture_object_5_5(2, 2, &pixels)),
            ],
        );
        let collection = collection_with(file.clone());
        let sprite = read_sprite(&file, 0, SpriteReadLimits::default()).unwrap();
        assert!(sprite.render_data.mesh_triangles.is_empty());
        let image = decode_sprite_rgba8(
            &collection,
            &file,
            &sprite,
            SpriteReadLimits::default(),
            TextureReadLimits::default(),
        )
        .unwrap();
        assert_eq!(
            image.pixels,
            [30, 3, 3, 255, 40, 4, 4, 255, 10, 1, 1, 255, 20, 2, 2, 255]
        );
    }

    #[test]
    fn atlas_tight_mesh_uses_the_atlas_texture_rect_offset() {
        let pixels = [10, 1, 1, 255, 20, 2, 2, 255, 30, 3, 3, 255, 40, 4, 4, 255];
        let sprite_object = modern_tight_sprite_object_with_references(
            [0, 1, 2],
            ObjectReferenceFields {
                atlas_path: 9,
                ..ObjectReferenceFields::default()
            },
        );
        let atlas = sprite_atlas_object(AtlasFixtureFields {
            texture_path: 3,
            texture_rect: [0.0, 0.0, 2.0, 2.0],
            texture_rect_offset: [0.5, 0.0],
            settings: 0,
            ..AtlasFixtureFields::default()
        });
        let file = parse_asset(
            "2022.3.62f1",
            &[
                (SPRITE_CLASS_ID, 1, sprite_object),
                (28, 2, texture_object(1, 1, &[1, 2, 3, 255])),
                (28, 3, texture_object(2, 2, &pixels)),
                (SPRITE_ATLAS_CLASS_ID, 9, atlas),
            ],
        );
        let collection = collection_with(file.clone());
        let sprite = read_sprite(&file, 0, SpriteReadLimits::default()).unwrap();
        let image = decode_sprite_rgba8(
            &collection,
            &file,
            &sprite,
            SpriteReadLimits::default(),
            TextureReadLimits::default(),
        )
        .unwrap();
        assert_eq!(
            image.pixels,
            [0, 0, 0, 0, 0, 0, 0, 0, 10, 1, 1, 255, 0, 0, 0, 0]
        );
    }

    #[test]
    fn resolves_local_atlas_data_and_ignores_atlas_alpha_texture() {
        let resident = texture_object(1, 1, &[1, 2, 3, 255]);
        let atlas_pixels = rgba_grid(2, 2, |x, y| [10 + x * 10 + y * 20, 4, 5, 180 + x + y]);
        let alpha_pixels = rgba_grid(2, 2, |_, _| [0, 0, 0, 255]);
        let sprite = modern_sprite_object(
            2,
            0,
            [0.0, 0.0, 1.0, 1.0],
            2,
            ObjectReferenceFields {
                atlas_path: 9,
                ..ObjectReferenceFields::default()
            },
        );
        let atlas = sprite_atlas_object(AtlasFixtureFields {
            texture_path: 3,
            alpha_texture_path: 4,
            texture_rect: [1.0, 0.0, 1.0, 2.0],
            ..AtlasFixtureFields::default()
        });
        let file = parse_asset(
            "2022.3.62f1",
            &[
                (SPRITE_CLASS_ID, 1, sprite),
                (28, 2, resident),
                (28, 3, texture_object(2, 2, &atlas_pixels)),
                (28, 4, texture_object(2, 2, &alpha_pixels)),
                (SPRITE_ATLAS_CLASS_ID, 9, atlas),
            ],
        );
        let collection = collection_with(file.clone());
        let sprite = read_sprite(&file, 0, SpriteReadLimits::default()).unwrap();

        let image = decode_sprite_rgba8(
            &collection,
            &file,
            &sprite,
            SpriteReadLimits::default(),
            TextureReadLimits::default(),
        )
        .unwrap();

        assert_eq!((image.width, image.height), (1, 2));
        assert_eq!(image.pixels, [40, 4, 5, 182, 20, 4, 5, 181]);
    }

    #[test]
    fn backfills_packed_sprites_and_replaces_variants_but_not_masters() {
        let null_sprite = modern_sprite_object(
            2,
            0,
            [0.0, 0.0, 1.0, 1.0],
            2,
            ObjectReferenceFields::default(),
        );
        let variant = sprite_atlas_object(AtlasFixtureFields {
            texture_path: 3,
            is_variant: true,
            ..AtlasFixtureFields::default()
        });
        let master = sprite_atlas_object(AtlasFixtureFields {
            texture_path: 4,
            ..AtlasFixtureFields::default()
        });
        let file = parse_asset(
            "2022.3.62f1",
            &[
                (SPRITE_CLASS_ID, 1, null_sprite),
                (28, 2, texture_object(1, 1, &[1, 2, 3, 255])),
                (28, 3, texture_object(1, 1, &[30, 0, 0, 255])),
                (28, 4, texture_object(1, 1, &[40, 0, 0, 255])),
                (SPRITE_ATLAS_CLASS_ID, 9, variant),
                (SPRITE_ATLAS_CLASS_ID, 10, master),
            ],
        );
        let collection = collection_with(file.clone());
        let sprite = read_sprite(&file, 0, SpriteReadLimits::default()).unwrap();
        let image = decode_sprite_rgba8(
            &collection,
            &file,
            &sprite,
            SpriteReadLimits::default(),
            TextureReadLimits::default(),
        )
        .unwrap();
        assert_eq!(image.pixels, [40, 0, 0, 255]);

        let explicit_master = modern_sprite_object(
            2,
            0,
            [0.0, 0.0, 1.0, 1.0],
            2,
            ObjectReferenceFields {
                atlas_path: 9,
                ..ObjectReferenceFields::default()
            },
        );
        let first_master = sprite_atlas_object(AtlasFixtureFields {
            texture_path: 3,
            ..AtlasFixtureFields::default()
        });
        let later_master = sprite_atlas_object(AtlasFixtureFields {
            texture_path: 4,
            ..AtlasFixtureFields::default()
        });
        let file = parse_asset(
            "2022.3.62f1",
            &[
                (SPRITE_CLASS_ID, 1, explicit_master),
                (28, 2, texture_object(1, 1, &[1, 2, 3, 255])),
                (28, 3, texture_object(1, 1, &[30, 0, 0, 255])),
                (28, 4, texture_object(1, 1, &[40, 0, 0, 255])),
                (SPRITE_ATLAS_CLASS_ID, 9, first_master),
                (SPRITE_ATLAS_CLASS_ID, 10, later_master),
            ],
        );
        let collection = collection_with(file.clone());
        let sprite = read_sprite(&file, 0, SpriteReadLimits::default()).unwrap();
        let image = decode_sprite_rgba8(
            &collection,
            &file,
            &sprite,
            SpriteReadLimits::default(),
            TextureReadLimits::default(),
        )
        .unwrap();
        assert_eq!(image.pixels, [30, 0, 0, 255]);
    }

    #[test]
    fn indexed_backfill_preserves_master_selection_and_enforces_build_budget() {
        let sprite_object = modern_sprite_object(
            2,
            0,
            [0.0, 0.0, 1.0, 1.0],
            2,
            ObjectReferenceFields::default(),
        );
        let variant = sprite_atlas_object(AtlasFixtureFields {
            texture_path: 3,
            is_variant: true,
            ..AtlasFixtureFields::default()
        });
        let master = sprite_atlas_object(AtlasFixtureFields {
            texture_path: 4,
            ..AtlasFixtureFields::default()
        });
        let file = parse_asset(
            "2022.3.62f1",
            &[
                (SPRITE_CLASS_ID, 1, sprite_object),
                (28, 2, texture_object(1, 1, &[1, 2, 3, 255])),
                (28, 3, texture_object(1, 1, &[30, 0, 0, 255])),
                (28, 4, texture_object(1, 1, &[40, 0, 0, 255])),
                (SPRITE_ATLAS_CLASS_ID, 9, variant),
                (SPRITE_ATLAS_CLASS_ID, 10, master),
            ],
        );

        let mut collection = collection_with(file.clone());
        collection
            .resolve_object_metadata(AssetLoadLimits::default())
            .unwrap();
        let assignments = collection
            .indexed_sprite_atlas_assignments(0, 0)
            .expect("metadata resolution builds the atlas index");
        assert_eq!(assignments.len(), 2);
        let sprite = read_sprite(
            &collection.serialized_files()[0].file,
            0,
            SpriteReadLimits::default(),
        )
        .unwrap();
        let image = decode_sprite_rgba8_by_file_index(
            &collection,
            0,
            &sprite,
            SpriteReadLimits::default(),
            TextureReadLimits::default(),
        )
        .unwrap();
        assert_eq!(image.pixels, [40, 0, 0, 255]);

        let mut limited = collection_with(file);
        let error = limited
            .resolve_object_metadata(AssetLoadLimits {
                maximum_sprite_atlas_index_entries: 3,
                ..AssetLoadLimits::default()
            })
            .unwrap_err();
        assert!(error.to_string().contains("needs 4 entries"));
        assert!(limited.indexed_sprite_atlas_assignments(0, 0).is_none());
    }

    #[test]
    fn backfills_an_unresolved_explicit_atlas_from_a_cross_file_packed_sprite() {
        let source = parse_asset(
            "2022.3.62f1",
            &[(
                SPRITE_CLASS_ID,
                1,
                modern_sprite_object(
                    0,
                    0,
                    [0.0, 0.0, 1.0, 1.0],
                    2,
                    ObjectReferenceFields {
                        atlas_path: 99,
                        ..ObjectReferenceFields::default()
                    },
                ),
            )],
        );
        let atlas = parse_asset_with_externals(
            "2022.3.62f1",
            &[
                (
                    SPRITE_ATLAS_CLASS_ID,
                    9,
                    sprite_atlas_object(AtlasFixtureFields {
                        packed_sprite_file: 1,
                        packed_sprite_path: 1,
                        texture_path: 3,
                        ..AtlasFixtureFields::default()
                    }),
                ),
                (28, 3, texture_object(1, 1, &[70, 8, 9, 255])),
            ],
            &["archive:/SOURCE.AsSeTs"],
        );
        let collection = collection_with_files(vec![
            ("root::source.assets", source.clone()),
            ("bundle::atlas.assets", atlas),
        ]);
        let sprite = read_sprite(&source, 0, SpriteReadLimits::default()).unwrap();
        let image = decode_sprite_rgba8(
            &collection,
            &source,
            &sprite,
            SpriteReadLimits::default(),
            TextureReadLimits::default(),
        )
        .unwrap();
        assert_eq!(image.pixels, [70, 8, 9, 255]);
    }

    #[test]
    fn resolved_atlas_key_or_texture_failures_do_not_fall_back() {
        let resident = texture_object(1, 1, &[1, 2, 3, 255]);
        for (atlas, expected) in [
            (
                sprite_atlas_object(AtlasFixtureFields {
                    key_guid: [0x55; 16],
                    texture_path: 3,
                    ..AtlasFixtureFields::default()
                }),
                "no render data",
            ),
            (
                sprite_atlas_object(AtlasFixtureFields {
                    texture_path: 0,
                    ..AtlasFixtureFields::default()
                }),
                "SpriteAtlas texture reference is null",
            ),
            (
                sprite_atlas_object(AtlasFixtureFields {
                    texture_path: 99,
                    ..AtlasFixtureFields::default()
                }),
                "SpriteAtlas texture path ID 99",
            ),
        ] {
            let sprite = modern_sprite_object(
                2,
                0,
                [0.0, 0.0, 1.0, 1.0],
                2,
                ObjectReferenceFields {
                    atlas_path: 9,
                    ..ObjectReferenceFields::default()
                },
            );
            let file = parse_asset(
                "2022.3.62f1",
                &[
                    (SPRITE_CLASS_ID, 1, sprite),
                    (28, 2, resident.clone()),
                    (28, 3, texture_object(1, 1, &[9, 8, 7, 255])),
                    (SPRITE_ATLAS_CLASS_ID, 9, atlas),
                ],
            );
            let collection = collection_with(file.clone());
            let sprite = read_sprite(&file, 0, SpriteReadLimits::default()).unwrap();

            let error = decode_sprite_rgba8(
                &collection,
                &file,
                &sprite,
                SpriteReadLimits::default(),
                TextureReadLimits::default(),
            )
            .unwrap_err();
            // A null reference says the asset has no texture, which this
            // reader declines; the other cases say the bytes disagree with
            // themselves, which is a parse failure. Both must surface rather
            // than fall back to resident data, and the kind is part of what
            // callers act on -- an export counts one as unsupported and the
            // other as failed.
            let declined = expected.contains("is null");
            let matched = match (&error, declined) {
                (Error::Unsupported(message), true) | (Error::InvalidData(message), false) => {
                    message.contains(expected)
                }
                _ => false,
            };
            assert!(
                matched,
                "expected {} error containing {expected:?}, got {error:?}",
                if declined {
                    "unsupported"
                } else {
                    "invalid-data"
                }
            );
        }
    }

    #[test]
    fn empty_atlas_render_data_uses_resident_sprite_data() {
        let sprite = modern_sprite_object(
            2,
            0,
            [0.0, 0.0, 1.0, 1.0],
            2,
            ObjectReferenceFields {
                atlas_path: 9,
                ..ObjectReferenceFields::default()
            },
        );
        let atlas = sprite_atlas_object(AtlasFixtureFields {
            has_render_data: false,
            ..AtlasFixtureFields::default()
        });
        let file = parse_asset(
            "2022.3.62f1",
            &[
                (SPRITE_CLASS_ID, 1, sprite),
                (28, 2, texture_object(1, 1, &[1, 2, 3, 255])),
                (SPRITE_ATLAS_CLASS_ID, 9, atlas),
            ],
        );
        let collection = collection_with(file.clone());
        let sprite = read_sprite(&file, 0, SpriteReadLimits::default()).unwrap();
        let image = decode_sprite_rgba8(
            &collection,
            &file,
            &sprite,
            SpriteReadLimits::default(),
            TextureReadLimits::default(),
        )
        .unwrap();
        assert_eq!(image.pixels, [1, 2, 3, 255]);
    }

    #[test]
    fn unresolved_or_wrong_class_atlas_pptrs_use_resident_data() {
        for atlas_path in [99, 3] {
            let sprite = modern_sprite_object(
                2,
                0,
                [0.0, 0.0, 1.0, 1.0],
                2,
                ObjectReferenceFields {
                    atlas_path,
                    ..ObjectReferenceFields::default()
                },
            );
            let file = parse_asset(
                "2022.3.62f1",
                &[
                    (SPRITE_CLASS_ID, 1, sprite),
                    (28, 2, texture_object(1, 1, &[1, 2, 3, 255])),
                    (28, 3, texture_object(1, 1, &[9, 9, 9, 255])),
                ],
            );
            let collection = collection_with(file.clone());
            let sprite = read_sprite(&file, 0, SpriteReadLimits::default()).unwrap();

            let image = decode_sprite_rgba8(
                &collection,
                &file,
                &sprite,
                SpriteReadLimits::default(),
                TextureReadLimits::default(),
            )
            .unwrap();
            assert_eq!(image.pixels, [1, 2, 3, 255]);
        }
    }

    #[test]
    fn resolves_cross_file_atlas_and_texture_relative_to_the_atlas_file() {
        let source = parse_asset_with_externals(
            "2022.3.62f1",
            &[(
                SPRITE_CLASS_ID,
                1,
                modern_sprite_object(
                    0,
                    0,
                    [0.0, 0.0, 1.0, 1.0],
                    2,
                    ObjectReferenceFields {
                        atlas_file: 1,
                        atlas_path: 9,
                        ..ObjectReferenceFields::default()
                    },
                ),
            )],
            &["archive:/NESTED/ATLAS.AsSeTs"],
        );
        let atlas_file = parse_asset_with_externals(
            "2022.3.62f1",
            &[(
                SPRITE_ATLAS_CLASS_ID,
                9,
                sprite_atlas_object(AtlasFixtureFields {
                    texture_file: 1,
                    texture_path: 30,
                    texture_rect: [0.0, 0.0, 2.0, 1.0],
                    ..AtlasFixtureFields::default()
                }),
            )],
            &["CAB-abc/TEXTURES.assets"],
        );
        let textures = parse_asset(
            "2022.3.62f1",
            &[(
                28,
                30,
                texture_object(2, 1, &[10, 20, 30, 255, 40, 50, 60, 255]),
            )],
        );
        let collection = collection_with_files(vec![
            ("root::source.assets", source.clone()),
            ("bundle::nested/atlas.assets", atlas_file),
            ("bundle::textures.AsSeTs", textures),
        ]);
        let sprite = read_sprite(&source, 0, SpriteReadLimits::default()).unwrap();

        let image = decode_sprite_rgba8(
            &collection,
            &source,
            &sprite,
            SpriteReadLimits::default(),
            TextureReadLimits::default(),
        )
        .unwrap();
        assert_eq!(image.pixels, [10, 20, 30, 255, 40, 50, 60, 255]);
    }

    #[test]
    fn resolves_cross_file_color_and_alpha_textures_by_portable_name() {
        let color = [10, 20, 30, 255, 40, 50, 60, 255];
        let alpha = [9, 12, 15, 255, 30, 60, 90, 255];
        let source = parse_asset_with_externals(
            "2022.3.62f1",
            &[(
                SPRITE_CLASS_ID,
                1,
                modern_sprite_object(
                    20,
                    21,
                    [0.0, 0.0, 2.0, 1.0],
                    2,
                    ObjectReferenceFields {
                        texture_file: 1,
                        alpha_texture_file: 1,
                        ..ObjectReferenceFields::default()
                    },
                ),
            )],
            &["archive:/NESTED/TARGET.AsSeTs"],
        );
        let target = parse_asset(
            "2022.3.62f1",
            &[
                (28, 20, texture_object(2, 1, &color)),
                (28, 21, texture_object(2, 1, &alpha)),
            ],
        );
        let collection = collection_with_files(vec![
            ("bundle::SOURCE.assets", source.clone()),
            ("root::nested/target.assets", target),
        ]);

        let sprite = read_sprite(&source, 0, SpriteReadLimits::default()).unwrap();
        let image = decode_sprite_rgba8(
            &collection,
            &source,
            &sprite,
            SpriteReadLimits::default(),
            TextureReadLimits::default(),
        )
        .unwrap();

        assert_eq!((image.width, image.height), (2, 1));
        assert_eq!(image.pixels, [10, 20, 30, 12, 40, 50, 60, 60]);
    }

    #[test]
    fn reports_cross_file_texture_resolution_failures() {
        let source_with = |texture_file, texture_path_id, external: &[&str]| {
            parse_asset_with_externals(
                "2022.3.62f1",
                &[(
                    SPRITE_CLASS_ID,
                    1,
                    modern_sprite_object(
                        texture_path_id,
                        0,
                        [0.0, 0.0, 1.0, 1.0],
                        2,
                        ObjectReferenceFields {
                            texture_file,
                            ..ObjectReferenceFields::default()
                        },
                    ),
                )],
                external,
            )
        };

        let bad_file_id = source_with(2, 20, &["target.assets"]);
        assert_cross_file_error(
            &bad_file_id,
            &collection_with_files(vec![("source.assets", bad_file_id.clone())]),
            "external index 1",
        );

        let missing_target = source_with(1, 20, &["archive:/nested/missing.assets"]);
        assert_cross_file_error(
            &missing_target,
            &collection_with_files(vec![("source.assets", missing_target.clone())]),
            "not found in the asset collection",
        );

        let missing_path = source_with(1, 99, &["target.assets"]);
        let target = parse_asset(
            "2022.3.62f1",
            &[(28, 20, texture_object(1, 1, &[1, 2, 3, 4]))],
        );
        assert_cross_file_error(
            &missing_path,
            &collection_with_files(vec![
                ("source.assets", missing_path.clone()),
                ("target.assets", target),
            ]),
            "path ID 99",
        );

        let wrong_class = source_with(1, 20, &["target.assets"]);
        let target = parse_asset("2022.3.62f1", &[(49, 20, Vec::new())]);
        assert_cross_file_error(
            &wrong_class,
            &collection_with_files(vec![
                ("source.assets", wrong_class.clone()),
                ("target.assets", target),
            ]),
            "not Texture2D",
        );
    }

    fn assert_cross_file_error(
        source: &SerializedFile,
        collection: &AssetCollection,
        expected: &str,
    ) {
        let sprite = read_sprite(source, 0, SpriteReadLimits::default()).unwrap();
        let error = decode_sprite_rgba8(
            collection,
            source,
            &sprite,
            SpriteReadLimits::default(),
            TextureReadLimits::default(),
        )
        .unwrap_err();
        assert!(
            matches!(error, Error::InvalidData(message) if message.contains(expected)),
            "expected invalid-data error containing {expected:?}"
        );
    }

    #[derive(Clone, Copy, Default)]
    struct ObjectReferenceFields {
        texture_file: i32,
        alpha_texture_file: i32,
        atlas_file: i32,
        atlas_path: i64,
        render_key_guid: [u8; 16],
        render_key_value: i64,
    }

    #[derive(Clone, Copy)]
    struct AtlasFixtureFields {
        packed_sprite_file: i32,
        packed_sprite_path: i64,
        key_guid: [u8; 16],
        key_value: i64,
        texture_file: i32,
        texture_path: i64,
        alpha_texture_file: i32,
        alpha_texture_path: i64,
        texture_rect: [f32; 4],
        texture_rect_offset: [f32; 2],
        settings: u32,
        downscale_multiplier: f32,
        is_variant: bool,
        has_render_data: bool,
    }

    impl Default for AtlasFixtureFields {
        fn default() -> Self {
            Self {
                packed_sprite_file: 0,
                packed_sprite_path: 1,
                key_guid: [0; 16],
                key_value: 0,
                texture_file: 0,
                texture_path: 3,
                alpha_texture_file: 0,
                alpha_texture_path: 0,
                texture_rect: [0.0, 0.0, 1.0, 1.0],
                texture_rect_offset: [0.0, 0.0],
                settings: 2,
                downscale_multiplier: 1.0,
                is_variant: false,
                has_render_data: true,
            }
        }
    }

    fn modern_sprite_object(
        texture_path_id: i64,
        alpha_path_id: i64,
        texture_rect: [f32; 4],
        settings: u32,
        references: ObjectReferenceFields,
    ) -> Vec<u8> {
        let mut output = Vec::new();
        push_aligned_string(&mut output, "icon");
        push_floats(&mut output, &[0.0, 0.0, 2.0, 2.0]);
        push_floats(&mut output, &[0.0, 0.0]);
        push_floats(&mut output, &[1.0, 2.0, 3.0, 4.0]);
        push_floats(&mut output, &[100.0]);
        push_floats(&mut output, &[0.25, 0.75]);
        push_u32(&mut output, 0);
        output.push(0);
        align(&mut output, 4);
        output.extend_from_slice(&references.render_key_guid);
        output.extend_from_slice(&references.render_key_value.to_le_bytes());
        push_i32(&mut output, 0);
        push_pptr(&mut output, references.atlas_file, references.atlas_path);

        push_pptr(&mut output, references.texture_file, texture_path_id);
        push_pptr(&mut output, references.alpha_texture_file, alpha_path_id);
        push_i32(&mut output, 0);
        push_i32(&mut output, 0);
        push_i32(&mut output, 0);
        align(&mut output, 4);
        push_u32(&mut output, 0);
        push_i32(&mut output, 1);
        output.extend_from_slice(&[0, 0, 0, 3]);
        push_i32(&mut output, 0);
        align(&mut output, 4);
        push_i32(&mut output, 0);

        push_floats(&mut output, &texture_rect);
        push_floats(&mut output, &[0.0, 0.0]);
        push_floats(&mut output, &[0.0, 0.0]);
        push_u32(&mut output, settings);
        push_floats(&mut output, &[0.0, 0.0, 1.0, 1.0]);
        push_floats(&mut output, &[1.0]);
        output
    }

    fn legacy_tight_sprite_object(indices: [u16; 3]) -> Vec<u8> {
        let mut output = Vec::new();
        push_aligned_string(&mut output, "tight legacy");
        push_floats(&mut output, &[0.0, 0.0, 2.0, 2.0]);
        push_floats(&mut output, &[0.0, 0.0]);
        push_floats(&mut output, &[0.0; 4]);
        push_floats(&mut output, &[1.0, 0.5, 0.5]);
        push_u32(&mut output, 0);
        output.push(0);
        align(&mut output, 4);
        push_pptr(&mut output, 0, 2);
        push_pptr(&mut output, 0, 0);
        push_i32(&mut output, 3);
        push_floats(
            &mut output,
            &[-1.0, -1.0, 0.0, 1.1, -1.0, 0.0, -1.0, 1.1, 0.0],
        );
        push_i32(&mut output, 3);
        for index in indices {
            output.extend_from_slice(&index.to_le_bytes());
        }
        align(&mut output, 4);
        push_floats(&mut output, &[0.0, 0.0, 2.0, 2.0]);
        push_floats(&mut output, &[0.0, 0.0]);
        push_u32(&mut output, 0);
        push_floats(&mut output, &[0.0, 0.0, 1.0, 1.0]);
        output
    }

    fn modern_tight_sprite_object(indices: [u16; 3]) -> Vec<u8> {
        modern_tight_sprite_object_with_references(indices, ObjectReferenceFields::default())
    }

    fn modern_tight_sprite_object_with_references(
        indices: [u16; 3],
        references: ObjectReferenceFields,
    ) -> Vec<u8> {
        let mut output = Vec::new();
        push_aligned_string(&mut output, "tight modern");
        push_floats(&mut output, &[0.0, 0.0, 2.0, 2.0]);
        push_floats(&mut output, &[0.0, 0.0]);
        push_floats(&mut output, &[0.0; 4]);
        push_floats(&mut output, &[1.0, 0.5, 0.5]);
        push_u32(&mut output, 0);
        output.push(0);
        align(&mut output, 4);
        output.extend_from_slice(&references.render_key_guid);
        output.extend_from_slice(&references.render_key_value.to_le_bytes());
        push_i32(&mut output, 0);
        push_pptr(&mut output, references.atlas_file, references.atlas_path);
        push_pptr(&mut output, references.texture_file, 2);
        push_pptr(&mut output, 0, 0);
        push_i32(&mut output, 0);

        push_i32(&mut output, 1);
        push_u32(&mut output, 0);
        push_u32(&mut output, 3);
        push_i32(&mut output, 0);
        push_u32(&mut output, 0);
        push_u32(&mut output, 0);
        push_u32(&mut output, 3);
        push_floats(&mut output, &[0.0; 6]);
        push_i32(&mut output, 6);
        for index in indices {
            output.extend_from_slice(&index.to_le_bytes());
        }
        align(&mut output, 4);
        push_u32(&mut output, 3);
        push_i32(&mut output, 1);
        output.extend_from_slice(&[0, 0, 0, 3]);
        push_i32(&mut output, 36);
        push_floats(
            &mut output,
            &[-1.0, -1.0, 0.0, 1.1, -1.0, 0.0, -1.0, 1.1, 0.0],
        );
        align(&mut output, 4);
        push_i32(&mut output, 0);
        push_floats(&mut output, &[0.0, 0.0, 2.0, 2.0]);
        push_floats(&mut output, &[0.0, 0.0, 0.0, 0.0]);
        push_u32(&mut output, 0);
        push_floats(&mut output, &[0.0, 0.0, 1.0, 1.0, 1.0]);
        output
    }

    fn sprite_atlas_object(fields: AtlasFixtureFields) -> Vec<u8> {
        let mut output = Vec::new();
        push_aligned_string(&mut output, "atlas");
        push_i32(&mut output, 1);
        push_pptr(
            &mut output,
            fields.packed_sprite_file,
            fields.packed_sprite_path,
        );
        push_i32(&mut output, 1);
        push_aligned_string(&mut output, "icon");
        push_i32(&mut output, i32::from(fields.has_render_data));
        if fields.has_render_data {
            output.extend_from_slice(&fields.key_guid);
            output.extend_from_slice(&fields.key_value.to_le_bytes());
            push_pptr(&mut output, fields.texture_file, fields.texture_path);
            push_pptr(
                &mut output,
                fields.alpha_texture_file,
                fields.alpha_texture_path,
            );
            push_floats(&mut output, &fields.texture_rect);
            push_floats(&mut output, &fields.texture_rect_offset);
            push_floats(&mut output, &[0.0, 0.0]);
            push_floats(&mut output, &[0.0, 0.0, 1.0, 1.0]);
            push_floats(&mut output, &[fields.downscale_multiplier]);
            push_u32(&mut output, fields.settings);
            push_i32(&mut output, 0);
            align(&mut output, 4);
        }
        push_aligned_string(&mut output, "ui");
        output.push(u8::from(fields.is_variant));
        align(&mut output, 4);
        output
    }

    fn legacy_sprite_object(has_pivot: bool) -> Vec<u8> {
        let mut output = Vec::new();
        push_aligned_string(&mut output, "legacy");
        push_floats(&mut output, &[0.0, 0.0, 1.0, 1.0]);
        push_floats(&mut output, &[0.0, 0.0]);
        push_floats(&mut output, &[0.0; 4]);
        push_floats(&mut output, &[100.0]);
        if has_pivot {
            push_floats(&mut output, &[0.25, 0.75]);
        }
        push_u32(&mut output, 0);
        output.push(0);
        align(&mut output, 4);
        push_pptr(&mut output, 0, 2);
        push_pptr(&mut output, 0, 0);
        push_i32(&mut output, 0);
        push_i32(&mut output, 0);
        align(&mut output, 4);
        push_floats(&mut output, &[0.0, 0.0, 1.0, 1.0]);
        push_floats(&mut output, &[0.0, 0.0]);
        push_u32(&mut output, 2);
        push_floats(&mut output, &[0.0, 0.0, 1.0, 1.0]);
        output
    }

    fn texture_object(width: i32, height: i32, data: &[u8]) -> Vec<u8> {
        let mut output = Vec::new();
        push_aligned_string(&mut output, "texture");
        push_i32(&mut output, 0);
        output.push(0);
        output.push(0);
        align(&mut output, 4);
        push_i32(&mut output, width);
        push_i32(&mut output, height);
        push_u32(&mut output, u32::try_from(data.len()).unwrap());
        push_i32(&mut output, 0);
        push_i32(&mut output, TextureFormat::RGBA32.0);
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
        align(&mut output, 4);
        push_i32(&mut output, i32::try_from(data.len()).unwrap());
        output.extend_from_slice(data);
        output
    }

    fn legacy_texture_object_5_5(width: i32, height: i32, data: &[u8]) -> Vec<u8> {
        let mut output = Vec::new();
        push_aligned_string(&mut output, "legacy texture");
        push_i32(&mut output, width);
        push_i32(&mut output, height);
        push_u32(&mut output, u32::try_from(data.len()).unwrap());
        push_i32(&mut output, TextureFormat::RGBA32.0);
        push_i32(&mut output, 1);
        output.push(1);
        align(&mut output, 4);
        push_i32(&mut output, 1);
        push_i32(&mut output, 2);
        output.extend_from_slice(&[0; 16]);
        push_i32(&mut output, 0);
        push_i32(&mut output, 0);
        push_i32(&mut output, i32::try_from(data.len()).unwrap());
        output.extend_from_slice(data);
        output
    }

    fn rgba_grid(width: u8, height: u8, pixel: impl Fn(u8, u8) -> [u8; 4]) -> Vec<u8> {
        let mut output = Vec::new();
        for y in 0..height {
            for x in 0..width {
                output.extend_from_slice(&pixel(x, y));
            }
        }
        output
    }

    fn imagesharp_oracle_grid(width: u8, height: u8) -> Vec<u8> {
        rgba_grid(width, height, |x, y| {
            [
                x.wrapping_mul(37)
                    .wrapping_add(y.wrapping_mul(53))
                    .wrapping_add(3),
                x.wrapping_mul(17)
                    .wrapping_add(y.wrapping_mul(29))
                    .wrapping_add(7),
                x.wrapping_mul(11)
                    .wrapping_add(y.wrapping_mul(13))
                    .wrapping_add(19),
                x.wrapping_mul(23)
                    .wrapping_add(y.wrapping_mul(31))
                    .wrapping_add(41),
            ]
        })
    }

    fn collection_with(file: SerializedFile) -> AssetCollection {
        collection_with_files(vec![("fixture.assets", file)])
    }

    fn collection_with_files(files: Vec<(&str, SerializedFile)>) -> AssetCollection {
        AssetCollection::from_loaded_parts(
            files
                .into_iter()
                .map(|(path, file)| LoadedSerializedFile {
                    path: path.to_owned(),
                    file,
                })
                .collect(),
            Vec::new(),
        )
    }

    fn parse_asset(version: &str, objects: &[(i32, i64, Vec<u8>)]) -> SerializedFile {
        parse_asset_with_externals(version, objects, &[])
    }

    fn parse_asset_with_externals(
        version: &str,
        objects: &[(i32, i64, Vec<u8>)],
        externals: &[&str],
    ) -> SerializedFile {
        SerializedFile::open(Region::from_bytes(synthetic_v22_asset(
            version, objects, externals,
        )))
        .unwrap()
    }

    fn synthetic_v22_asset(
        version: &str,
        objects: &[(i32, i64, Vec<u8>)],
        externals: &[&str],
    ) -> Vec<u8> {
        let mut classes = Vec::new();
        for (class_id, _, _) in objects {
            if !classes.contains(class_id) {
                classes.push(*class_id);
            }
        }

        let mut metadata = Vec::new();
        metadata.extend_from_slice(version.as_bytes());
        metadata.push(0);
        push_i32(&mut metadata, 13);
        metadata.push(0);
        push_i32(&mut metadata, i32::try_from(classes.len()).unwrap());
        for class_id in &classes {
            push_i32(&mut metadata, *class_id);
            metadata.push(0);
            metadata.extend_from_slice(&(-1_i16).to_le_bytes());
            metadata.extend_from_slice(&[0_u8; 16]);
        }

        let mut data = Vec::new();
        let mut object_records = Vec::new();
        for (class_id, path_id, object) in objects {
            align(&mut data, 4);
            let offset = i64::try_from(data.len()).unwrap();
            let type_index = classes.iter().position(|value| value == class_id).unwrap();
            object_records.push((
                *path_id,
                offset,
                u32::try_from(object.len()).unwrap(),
                i32::try_from(type_index).unwrap(),
            ));
            data.extend_from_slice(object);
        }

        push_i32(&mut metadata, i32::try_from(object_records.len()).unwrap());
        for (path_id, offset, size, type_index) in object_records {
            align_with_base(&mut metadata, 48, 4);
            metadata.extend_from_slice(&path_id.to_le_bytes());
            metadata.extend_from_slice(&offset.to_le_bytes());
            metadata.extend_from_slice(&size.to_le_bytes());
            push_i32(&mut metadata, type_index);
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
        let metadata_end = 48_u64 + u64::from(metadata_size);
        let data_offset = metadata_end.div_ceil(16) * 16;
        let file_size = data_offset + u64::try_from(data.len()).unwrap();
        let mut bytes = vec![0_u8; 48];
        bytes[8..12].copy_from_slice(&22_u32.to_be_bytes());
        bytes[20..24].copy_from_slice(&metadata_size.to_be_bytes());
        bytes[24..32].copy_from_slice(&i64::try_from(file_size).unwrap().to_be_bytes());
        bytes[32..40].copy_from_slice(&i64::try_from(data_offset).unwrap().to_be_bytes());
        bytes.extend_from_slice(&metadata);
        bytes.resize(usize::try_from(data_offset).unwrap(), 0);
        bytes.extend_from_slice(&data);
        bytes
    }

    fn push_pptr(output: &mut Vec<u8>, file_id: i32, path_id: i64) {
        push_i32(output, file_id);
        output.extend_from_slice(&path_id.to_le_bytes());
    }

    fn push_aligned_string(output: &mut Vec<u8>, value: &str) {
        push_i32(output, i32::try_from(value.len()).unwrap());
        output.extend_from_slice(value.as_bytes());
        if !value.is_empty() {
            align(output, 4);
        }
    }

    fn push_floats(output: &mut Vec<u8>, values: &[f32]) {
        for value in values {
            output.extend_from_slice(&value.to_le_bytes());
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

    fn assert_f32(actual: f32, expected: f32) {
        assert_eq!(actual.to_bits(), expected.to_bits());
    }
}
