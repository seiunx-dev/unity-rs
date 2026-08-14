use std::io::{self, Write};
use std::path::Path;

use assetstudio_core::animation_clip::{AnimationClipReadLimits, read_animation_clip};
use assetstudio_core::animator_controller::{
    AnimatorControllerReadLimits, read_animator_controller,
};
use assetstudio_core::avatar::{AvatarReadLimits, read_avatar};
use assetstudio_core::live2d_motion::{
    CubismFadeMotionReadLimits, CubismMotionTargetNames, project_cubism_fade_motion,
};
use assetstudio_core::live2d_physics::{CubismPhysicsReadLimits, project_cubism_physics};
use assetstudio_core::live2d_schema::{CubismExpressionReadLimits, project_cubism_expression};
use assetstudio_core::material::{MaterialReadLimits, read_material};
use assetstudio_core::mesh::{MeshReadLimits, read_mesh_with_collection};
use assetstudio_core::project_settings::{
    ProjectSettingsReadLimits, read_build_settings, read_player_settings,
};
use assetstudio_core::shader::{ShaderReadLimits, read_shader};
use assetstudio_core::simple_assets::{
    SimpleAssetReadLimits, read_audio_clip_asset, read_font, read_movie_texture, read_video_clip,
};
use assetstudio_core::sprite::{SpriteReadLimits, decode_sprite_rgba8, read_sprite};
use assetstudio_core::studio::Studio;
use assetstudio_core::texture::{TextureReadLimits, read_texture2d};
use assetstudio_core::type_tree::TypeValue;
use assetstudio_core::type_tree_dump::write_type_tree_dump;
use serde_json::{Value, json};

pub fn rust_manifest(
    path: &Path,
    maximum_object_bytes: u64,
) -> Result<Value, Box<dyn std::error::Error>> {
    let studio = Studio::open(path)?;
    let mut files = Vec::new();
    for file in studio.files() {
        let mut objects = Vec::new();
        for object in studio
            .objects()
            .filter(|object| object.file_index() == file.index())
        {
            let payload = rust_payload(
                &studio,
                file.index(),
                object.object_index(),
                object.class_id(),
                maximum_object_bytes,
            )?;
            let raw = object.read_raw(maximum_object_bytes)?;
            objects.push(json!({
                "PathId": object.path_id(),
                "ClassId": object.class_id(),
                "ByteSize": object.byte_size(),
                "Name": object.name(),
                "Raw": bytes_manifest(&raw),
                "Payload": payload,
            }));
        }
        files.push(json!({
            // Core labels a bundled file `<container>::<entry>` for provenance,
            // while the managed reader keeps only the entry name. Compare the
            // identity both agree on -- and the one cross-file resolution
            // actually matches against -- rather than the label.
            "Path": portable_file_name(file.path()),
            "UnityVersion": file.unity_version(),
            "Objects": objects,
        }));
    }
    let mut resources = studio.resources().collect::<Vec<_>>();
    resources.sort_by(|left, right| {
        portable_file_name(left.path())
            .to_ascii_lowercase()
            .cmp(&portable_file_name(right.path()).to_ascii_lowercase())
            .then_with(|| portable_file_name(left.path()).cmp(portable_file_name(right.path())))
            .then_with(|| left.path().cmp(right.path()))
    });
    let mut resource_manifest = Vec::new();
    resource_manifest.try_reserve_exact(resources.len())?;
    for resource in resources {
        let mut bytes = StreamingBytesManifest::new();
        resource.write(&mut bytes)?;
        resource_manifest.push(json!({
            "Path": portable_file_name(resource.path()),
            "Data": bytes.finish(),
        }));
    }
    Ok(json!({ "Files": files, "Resources": resource_manifest }))
}

struct StreamingBytesManifest {
    size: u64,
    hash: u64,
}

impl StreamingBytesManifest {
    const fn new() -> Self {
        Self {
            size: 0,
            hash: 0xcbf2_9ce4_8422_2325,
        }
    }

    fn finish(self) -> Value {
        json!({
            "Size": self.size,
            "Fnv64": format!("{:016x}", self.hash),
        })
    }
}

impl Write for StreamingBytesManifest {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        self.size = self
            .size
            .checked_add(u64::try_from(input.len()).map_err(io::Error::other)?)
            .ok_or_else(|| io::Error::other("resource byte count overflowed"))?;
        self.hash = input.iter().fold(self.hash, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
        });
        Ok(input.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn portable_file_name(path: &str) -> &str {
    path.rsplit_once("::")
        .map_or(path, |(_, name)| name)
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(path)
}

fn rust_payload(
    studio: &Studio,
    file_index: usize,
    object_index: usize,
    class_id: i32,
    maximum_bytes: u64,
) -> Result<Value, Box<dyn std::error::Error>> {
    match class_id {
        21 | 28 | 43 | 48 | 49 | 74 | 83 | 90 | 91 | 128 | 152 | 213 | 329 => {
            rust_binary_payload(studio, file_index, object_index, class_id, maximum_bytes)
        }
        114 | 129 | 141 | 123_456 => {
            rust_metadata_payload(studio, file_index, object_index, class_id, maximum_bytes)
        }
        _ => Ok(Value::Null),
    }
}

fn rust_binary_payload(
    studio: &Studio,
    file_index: usize,
    object_index: usize,
    class_id: i32,
    maximum_bytes: u64,
) -> Result<Value, Box<dyn std::error::Error>> {
    let loaded = &studio.collection().serialized_files[file_index].file;
    let simple_limits = SimpleAssetReadLimits {
        maximum_payload_bytes: maximum_bytes,
        ..SimpleAssetReadLimits::default()
    };
    Ok(match class_id {
        21 => material_manifest(studio, file_index, object_index, maximum_bytes)?,
        28 => texture_manifest(studio, file_index, object_index, maximum_bytes)?,
        43 => mesh_manifest(studio, file_index, object_index, maximum_bytes)?,
        48 => shader_manifest(studio, file_index, object_index, maximum_bytes)?,
        49 => {
            let maximum = usize::try_from(maximum_bytes)?;
            let text = loaded.read_text_asset(object_index, maximum)?;
            json!({ "Name": text.name, "Script": bytes_manifest(&text.script) })
        }
        74 => animation_clip_manifest(studio, file_index, object_index, maximum_bytes)?,
        83 => {
            let audio =
                read_audio_clip_asset(studio.collection(), loaded, object_index, simple_limits)?;
            json!({
                "Name": audio.name,
                "Extension": audio.raw_extension,
                "Data": bytes_manifest(&audio.payload.read_to_vec(maximum_bytes)?),
            })
        }
        90 => avatar_manifest(studio, file_index, object_index, maximum_bytes)?,
        91 => animator_controller_manifest(studio, file_index, object_index, maximum_bytes)?,
        128 => simple_binary_manifest(
            read_font(loaded, object_index, simple_limits)?,
            maximum_bytes,
        )?,
        152 => simple_binary_manifest(
            read_movie_texture(loaded, object_index, simple_limits)?,
            maximum_bytes,
        )?,
        213 => sprite_manifest(studio, file_index, object_index, maximum_bytes)?,
        329 => simple_binary_manifest(
            read_video_clip(studio.collection(), loaded, object_index, simple_limits)?,
            maximum_bytes,
        )?,
        _ => unreachable!("rust_payload selects binary fixture classes"),
    })
}

/// Curve keyframes hashed as a flat little-endian stream: the path, then every
/// keyframe's time, value and both tangents as raw float bits. Bit patterns
/// rather than decimal text so a rounding difference cannot hide.
fn quaternion_curves_manifest(
    curves: &[assetstudio_core::animation_clip::QuaternionCurve],
) -> Result<Value, Box<dyn std::error::Error>> {
    let mut values = Vec::new();
    for curve in curves {
        append_path(&mut values, &curve.path);
        for key in &curve.curve.keyframes {
            values.push(key.time.to_bits());
            append_quaternion(&mut values, key.value);
            append_quaternion(&mut values, key.in_slope);
            append_quaternion(&mut values, key.out_slope);
        }
    }
    u32_values_manifest(values.into_iter())
}

fn vector3_curves_manifest(
    curves: &[assetstudio_core::animation_clip::Vector3Curve],
) -> Result<Value, Box<dyn std::error::Error>> {
    let mut values = Vec::new();
    for curve in curves {
        append_path(&mut values, &curve.path);
        for key in &curve.curve.keyframes {
            values.push(key.time.to_bits());
            append_vector3(&mut values, key.value);
            append_vector3(&mut values, key.in_slope);
            append_vector3(&mut values, key.out_slope);
        }
    }
    u32_values_manifest(values.into_iter())
}

fn float_curves_manifest(
    curves: &[assetstudio_core::animation_clip::FloatCurve],
) -> Result<Value, Box<dyn std::error::Error>> {
    let mut values = Vec::new();
    for curve in curves {
        append_path(&mut values, &curve.path);
        append_path(&mut values, &curve.attribute);
        values.push(curve.class_id.cast_unsigned());
        for key in &curve.curve.keyframes {
            values.push(key.time.to_bits());
            values.push(key.value.to_bits());
            values.push(key.in_slope.to_bits());
            values.push(key.out_slope.to_bits());
        }
    }
    u32_values_manifest(values.into_iter())
}

fn append_path(values: &mut Vec<u32>, path: &str) {
    values.push(u32::try_from(path.chars().count()).expect("a path length fits in u32"));
    values.extend(path.chars().map(u32::from));
}

fn append_vector3(values: &mut Vec<u32>, value: assetstudio_core::animation_clip::Vector3) {
    values.push(value.x.to_bits());
    values.push(value.y.to_bits());
    values.push(value.z.to_bits());
}

fn append_quaternion(values: &mut Vec<u32>, value: assetstudio_core::animation_clip::Vector4) {
    values.push(value.x.to_bits());
    values.push(value.y.to_bits());
    values.push(value.z.to_bits());
    values.push(value.w.to_bits());
}

fn animation_clip_manifest(
    studio: &Studio,
    file_index: usize,
    object_index: usize,
    maximum_bytes: u64,
) -> Result<Value, Box<dyn std::error::Error>> {
    let loaded = &studio.collection().serialized_files[file_index].file;
    let clip = read_animation_clip(
        loaded,
        object_index,
        AnimationClipReadLimits {
            maximum_object_bytes: maximum_bytes,
            ..AnimationClipReadLimits::default()
        },
    )?;
    let acl = if let Some(acl) = clip
        .muscle_clip
        .as_ref()
        .and_then(|muscle| muscle.clip.acl.as_ref())
    {
        Some(json!({
            "FrameCount": acl.frame_count,
            "BoneCount": acl.bone_count,
            "SampleRateBits": acl.sample_rate_bits,
            "CurveCount": acl.curve_count.unwrap_or(0),
            "Tracks": bytes_manifest(&acl.tracks.read(maximum_bytes)?),
            "DecoderMap": acl.decoder_map.read_values(acl.decoder_map.count)?,
            "UseFastSampleMode": acl.use_fast_sample_mode.unwrap_or(false),
        }))
    } else {
        None
    };
    let streaming = clip.streaming_info.as_ref().map(|value| {
        json!({
            "Offset": value.offset,
            "Size": value.size,
            "Path": value.path,
        })
    });
    Ok(json!({
        "Name": clip.name,
        "SampleRateBits": clip.sample_rate.to_bits(),
        "WrapMode": clip.wrap_mode,
        "EulerCurveCount": clip.euler_curves.len(),
        // Keyframe values, not just curve counts. Everything below used to be
        // compared by shape alone, so the times, values and tangents each
        // reader produced were only ever checked against its own expectations.
        "RotationCurves": quaternion_curves_manifest(&clip.rotation_curves)?,
        "EulerCurves": vector3_curves_manifest(&clip.euler_curves)?,
        "PositionCurves": vector3_curves_manifest(&clip.position_curves)?,
        "ScaleCurves": vector3_curves_manifest(&clip.scale_curves)?,
        "FloatCurves": float_curves_manifest(&clip.float_curves)?,
        "MusclePresent": clip.muscle_clip.is_some(),
        "StreamedCurveCount": clip
            .muscle_clip
            .as_ref()
            .map(|muscle| muscle.clip.streamed.curve_count),
        "Acl": acl,
        "Streaming": streaming,
    }))
}

fn avatar_manifest(
    studio: &Studio,
    file_index: usize,
    object_index: usize,
    maximum_bytes: u64,
) -> Result<Value, Box<dyn std::error::Error>> {
    let loaded = &studio.collection().serialized_files[file_index].file;
    let avatar = read_avatar(
        loaded,
        object_index,
        AvatarReadLimits {
            maximum_object_bytes: maximum_bytes,
            ..AvatarReadLimits::default()
        },
    )?;
    let tos = avatar
        .paths
        .iter()
        .map(|entry| json!({ "Key": entry.hash, "Value": entry.path }))
        .collect::<Vec<_>>();
    Ok(json!({ "Name": avatar.name, "Tos": tos }))
}

fn animator_controller_manifest(
    studio: &Studio,
    file_index: usize,
    object_index: usize,
    maximum_bytes: u64,
) -> Result<Value, Box<dyn std::error::Error>> {
    let loaded = &studio.collection().serialized_files[file_index].file;
    let controller = read_animator_controller(
        loaded,
        object_index,
        AnimatorControllerReadLimits {
            maximum_object_bytes: maximum_bytes,
            ..AnimatorControllerReadLimits::default()
        },
    )?;
    let tos = controller
        .tos
        .iter()
        .map(|entry| json!({ "Key": entry.key, "Value": entry.value }))
        .collect::<Vec<_>>();
    let animation_clips = controller
        .animation_clips
        .iter()
        .map(|entry| json!({ "FileId": entry.file_id, "PathId": entry.path_id }))
        .collect::<Vec<_>>();
    Ok(json!({
        "Name": controller.name,
        "Tos": tos,
        "AnimationClips": animation_clips,
    }))
}

fn shader_manifest(
    studio: &Studio,
    file_index: usize,
    object_index: usize,
    maximum_bytes: u64,
) -> Result<Value, Box<dyn std::error::Error>> {
    let loaded = &studio.collection().serialized_files[file_index].file;
    let shader = read_shader(
        loaded,
        object_index,
        ShaderReadLimits {
            maximum_script_bytes: maximum_bytes,
            maximum_output_bytes: maximum_bytes,
            ..ShaderReadLimits::default()
        },
    )?;
    Ok(json!({
        "Name": shader.name,
        "Data": bytes_manifest(&shader.read_to_vec(maximum_bytes)?),
    }))
}

fn sprite_manifest(
    studio: &Studio,
    file_index: usize,
    object_index: usize,
    maximum_bytes: u64,
) -> Result<Value, Box<dyn std::error::Error>> {
    let loaded = &studio.collection().serialized_files[file_index].file;
    let limits = SpriteReadLimits {
        maximum_mesh_bytes: maximum_bytes,
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
    let sprite = read_sprite(loaded, object_index, limits)?;
    let image = decode_sprite_rgba8(studio.collection(), loaded, &sprite, limits, texture_limits)?;
    Ok(json!({
        "Name": sprite.name,
        "Width": image.width,
        "Height": image.height,
        "Pixels": bytes_manifest(&image.pixels),
    }))
}

fn material_manifest(
    studio: &Studio,
    file_index: usize,
    object_index: usize,
    maximum_bytes: u64,
) -> Result<Value, Box<dyn std::error::Error>> {
    let loaded = &studio.collection().serialized_files[file_index].file;
    let material = read_material(
        loaded,
        object_index,
        MaterialReadLimits {
            maximum_object_bytes: maximum_bytes,
            ..MaterialReadLimits::default()
        },
    )?;
    let texture_environments = material
        .saved_properties
        .texture_environments
        .iter()
        .map(|entry| {
            json!({
                "Name": entry.name,
                "Texture": {
                    "FileId": entry.value.texture.file_id,
                    "PathId": entry.value.texture.path_id,
                },
                "ScaleBits": entry.value.scale.map(f32::to_bits),
                "OffsetBits": entry.value.offset.map(f32::to_bits),
            })
        })
        .collect::<Vec<_>>();
    let integers = material
        .saved_properties
        .integers
        .iter()
        .map(|entry| json!({ "Name": entry.name, "Value": entry.value }))
        .collect::<Vec<_>>();
    let floats = material
        .saved_properties
        .floats
        .iter()
        .map(|entry| json!({ "Name": entry.name, "ValueBits": entry.value.to_bits() }))
        .collect::<Vec<_>>();
    let colors = material
        .saved_properties
        .colors
        .iter()
        .map(|entry| {
            json!({
                "Name": entry.name,
                "ValueBits": entry.value.map(f32::to_bits),
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "Name": material.name,
        "Shader": {
            "FileId": material.shader.file_id,
            "PathId": material.shader.path_id,
        },
        "TextureEnvironments": texture_environments,
        "Integers": integers,
        "Floats": floats,
        "Colors": colors,
    }))
}

fn mesh_manifest(
    studio: &Studio,
    file_index: usize,
    object_index: usize,
    maximum_bytes: u64,
) -> Result<Value, Box<dyn std::error::Error>> {
    let loaded = &studio.collection().serialized_files[file_index].file;
    let mesh = read_mesh_with_collection(
        studio.collection(),
        loaded,
        object_index,
        MeshReadLimits {
            maximum_object_bytes: maximum_bytes,
            ..MeshReadLimits::default()
        },
    )?;
    let indices = mesh
        .sub_meshes
        .iter()
        .flat_map(|sub_mesh| sub_mesh.indices.iter().copied());
    Ok(json!({
        "Name": mesh.name,
        "VertexCount": mesh.vertices.len(),
        "Vertices": f32_values_manifest(mesh.vertices.iter().flatten().copied())?,
        // An empty channel is reported as absent. The managed reader allocates a
        // zero-length array where this one leaves the option unset, and both
        // mean the mesh has no such channel; encoding that allocator detail
        // into the comparison would say nothing about either reader.
        "Normals": mesh.normals.as_ref().filter(|values| !values.is_empty()).map(|values| {
            f32_values_manifest(values.iter().flatten().copied())
        }).transpose()?,
        "Uv0": mesh.uv0.as_ref().filter(|values| !values.is_empty()).map(|values| {
            f32_values_manifest(values.iter().flatten().copied())
        }).transpose()?,
        "Indices": u32_values_manifest(indices)?,
    }))
}

fn texture_manifest(
    studio: &Studio,
    file_index: usize,
    object_index: usize,
    maximum_bytes: u64,
) -> Result<Value, Box<dyn std::error::Error>> {
    let limits = TextureReadLimits {
        maximum_payload_bytes: maximum_bytes,
        maximum_output_bytes: maximum_bytes,
        maximum_decoder_working_bytes: maximum_bytes,
        ..TextureReadLimits::default()
    };
    let loaded = &studio.collection().serialized_files[file_index].file;
    let texture = read_texture2d(studio.collection(), loaded, object_index, limits)?;
    // Mip zero only: the managed decoder exposes one decoded surface, and a
    // format it cannot handle decodes to null on both sides rather than failing
    // the comparison.
    let decoded = match texture.decode_mip_rgba8(0, limits) {
        Ok(image) => bytes_manifest(&image.pixels),
        Err(_) => Value::Null,
    };
    Ok(json!({
        "Name": texture.name,
        "Width": texture.width,
        "Height": texture.height,
        "TextureFormat": texture.format.0,
        "MipCount": texture.mip_count,
        "Data": bytes_manifest(&texture.data.read_to_vec(maximum_bytes)?),
        "Decoded": decoded,
    }))
}

fn simple_binary_manifest(
    asset: assetstudio_core::simple_assets::SimpleBinaryAsset,
    maximum_bytes: u64,
) -> Result<Value, Box<dyn std::error::Error>> {
    let assetstudio_core::simple_assets::SimpleBinaryAsset {
        name,
        payload,
        suggested_extension,
        ..
    } = asset;
    Ok(json!({
        "Name": name,
        "Extension": suggested_extension,
        "Data": bytes_manifest(&payload.read_to_vec(maximum_bytes)?),
    }))
}

fn rust_metadata_payload(
    studio: &Studio,
    file_index: usize,
    object_index: usize,
    class_id: i32,
    maximum_bytes: u64,
) -> Result<Value, Box<dyn std::error::Error>> {
    let loaded = &studio.collection().serialized_files[file_index].file;
    Ok(match class_id {
        141 => {
            let build =
                read_build_settings(loaded, object_index, ProjectSettingsReadLimits::default())?;
            json!({ "Levels": build.levels, "Scenes": build.scenes })
        }
        129 => {
            let player =
                read_player_settings(loaded, object_index, ProjectSettingsReadLimits::default())?;
            json!({
                "CompanyName": player.company_name,
                "ProductName": player.product_name,
            })
        }
        114 => mono_behaviour_manifest(loaded, object_index)?,
        123_456 => {
            let value = loaded.read_type_tree_value(object_index)?;
            let tree = loaded.object_type_tree(object_index)?;
            let mut dump = Vec::new();
            write_type_tree_dump(tree, &value, &mut dump, maximum_bytes)?;
            json!({ "Dump": String::from_utf8(dump)? })
        }
        _ => unreachable!("rust_payload selects metadata fixture classes"),
    })
}

/// A `MonoBehaviour`, plus physics3.json when the behaviour is a Cubism rig.
///
/// The managed side runs the `Live2D` converter and hands back the same
/// document, parsed rather than as text so that whitespace and key order do not
/// enter into it. The `Live2D` SDK's own types define the layout, so both
/// readers have to take it from the file's `TypeTree` rather than from anything
/// built in.
fn mono_behaviour_manifest(
    loaded: &assetstudio_core::serialized::SerializedFile,
    object_index: usize,
) -> Result<Value, Box<dyn std::error::Error>> {
    let value = loaded.read_type_tree_value(object_index)?;
    let name = match &value {
        TypeValue::Object(fields) => fields
            .iter()
            .find(|field| field.name == "m_Name")
            .and_then(|field| match &field.value {
                TypeValue::String(name) => Some(name.clone()),
                _ => None,
            })
            .unwrap_or_default(),
        _ => String::new(),
    };

    let expression =
        match project_cubism_expression(0, &value, CubismExpressionReadLimits::default()) {
            Ok(expression) => {
                let mut document = Vec::new();
                expression.write_exp3_json(&mut document, 1024 * 1024)?;
                Some(serde_json::from_slice::<Value>(&document)?)
            }
            Err(_) => None,
        };
    let motion = match project_cubism_fade_motion(0, &value, CubismFadeMotionReadLimits::default())
    {
        Ok(fade) => {
            let mut document = Vec::new();
            // The same names the managed side is given; in the real flow the
            // model supplies them, and without them every curve falls to the
            // unbound branch and the target logic goes untested.
            let names = CubismMotionTargetNames {
                parameters: vec!["ParamAngleX".to_owned()],
                parts: vec!["PartArmA".to_owned()],
            };
            fade.write_motion3_json(&names, false, &mut document, 1024 * 1024)?;
            Some(serde_json::from_slice::<Value>(&document)?)
        }
        Err(_) => None,
    };
    let physics = match project_cubism_physics(0, &value, CubismPhysicsReadLimits::default()) {
        Ok(rig) => {
            let mut document = Vec::new();
            // 30 is the fallback the managed converter passes in.
            rig.write_physics3_json(30.0, &mut document, 1024 * 1024)?;
            Some(serde_json::from_slice::<Value>(&document)?)
        }
        // A behaviour that is not a physics rig is not an error here; the
        // managed side reports the same absence by leaving the field null.
        Err(_) => None,
    };
    Ok(json!({ "Name": name, "Physics": physics, "Motion": motion, "Expression": expression }))
}

fn bytes_manifest(input: &[u8]) -> Value {
    json!({
        "Size": input.len(),
        "Fnv64": format!("{:016x}", fnv1a64(input)),
    })
}

fn f32_values_manifest(
    values: impl Iterator<Item = f32>,
) -> Result<Value, Box<dyn std::error::Error>> {
    scalar_values_manifest(values.map(f32::to_bits))
}

fn u32_values_manifest(
    values: impl Iterator<Item = u32>,
) -> Result<Value, Box<dyn std::error::Error>> {
    scalar_values_manifest(values)
}

fn scalar_values_manifest(
    values: impl Iterator<Item = u32>,
) -> Result<Value, Box<dyn std::error::Error>> {
    let mut bytes = StreamingBytesManifest::new();
    let mut count = 0_u64;
    for value in values {
        bytes.write_all(&value.to_le_bytes())?;
        count = count
            .checked_add(1)
            .ok_or("scalar value count overflowed")?;
    }
    Ok(json!({ "Count": count, "Data": bytes.finish() }))
}

pub(crate) fn fnv1a64(input: &[u8]) -> u64 {
    input.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}
