//! Differential fixture gate against the checked-in managed implementation.
//!
//! The matrix deliberately covers v13 big-endian metadata/object payloads and
//! v22 little-endian assets with external `Texture2D`, `AudioClip`, and
//! `VideoClip` resource ranges plus resident Material, Mesh, and Sprite objects. It
//! compares stable order, path IDs, classes, names, raw object hashes, parsed
//! payload hashes, Material property bits, Mesh vertex/normal/UV/index bits,
//! settings fields, and `TypeTree` dumps.
//!
//! Run with:
//! `cargo test -p assetstudio-core --test dotnet_oracle -- --ignored`

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use assetstudio_core::studio::Studio;
use serde_json::{Value, json};

#[path = "support/oracle_manifest.rs"]
mod oracle_manifest;

use oracle_manifest::rust_manifest;

#[test]
#[ignore = "requires the .NET 10 SDK and the managed AssetStudio oracle"]
fn managed_and_rust_manifests_match_for_shared_fixture() {
    let executable = build_managed_oracle().unwrap();
    let fixtures = [
        TemporaryFixture::new("oracle-v13-big-endian.assets", &synthetic_v13_big_endian()).unwrap(),
        TemporaryFixture::new(
            "oracle-movie-2018.assets",
            &synthetic_single_v22(152, -20, "2018.4.36f1", &movie_texture()),
        )
        .unwrap(),
        TemporaryFixture::new(
            "oracle-mesh-2022.assets",
            &synthetic_single_v22(43, 43, "2022.3.62f1", &mesh()),
        )
        .unwrap(),
        TemporaryFixture::new(
            "oracle-mesh-tuanjie.assets",
            &synthetic_single_v22(43, 43, "2022.3.61t2", &tuanjie_mesh()),
        )
        .unwrap(),
        TemporaryFixture::new(
            "oracle-mesh-6000.1.assets",
            &synthetic_single_v22(43, 43, "6000.1.0f1", &mesh()),
        )
        .unwrap(),
        TemporaryFixture::with_resource(
            "oracle-mesh-streamed-6000.1.assets",
            &synthetic_single_v22(
                43,
                43,
                "6000.1.0f1",
                &mesh_payload(Some((7, 120, "oracle-mesh.resS")), None),
            ),
            "oracle-mesh.resS",
            &streamed_mesh_resource(),
        )
        .unwrap(),
        TemporaryFixture::new(
            "oracle-material-2022.assets",
            &synthetic_single_v22(21, 21, "2022.3.62f1", &material()),
        )
        .unwrap(),
        TemporaryFixture::new("oracle-sprite-2022.assets", &synthetic_sprite_v22()).unwrap(),
        TemporaryFixture::new(
            "oracle-shader-5.2.assets",
            &synthetic_single_v22(48, 48, "5.2.4f1", &shader()),
        )
        .unwrap(),
        TemporaryFixture::new(
            "oracle-controller-6000.2.assets",
            &synthetic_single_v22(91, 91, "6000.2.0f1", &animator_controller()),
        )
        .unwrap(),
        TemporaryFixture::new(
            "oracle-controller-tuanjie.assets",
            &synthetic_single_v22(91, 91, "2022.3.55t4", &tuanjie_animator_controller()),
        )
        .unwrap(),
        TemporaryFixture::new(
            "oracle-avatar-6000.2.assets",
            &synthetic_single_v22(90, 90, "6000.2.0f1", &avatar()),
        )
        .unwrap(),
        TemporaryFixture::new(
            "oracle-avatar-tuanjie.assets",
            &synthetic_single_v22(90, 90, "2022.3.55t4", &avatar()),
        )
        .unwrap(),
        TemporaryFixture::new(
            "oracle-animation-6000.2.assets",
            &synthetic_single_v22(74, 74, "6000.2.0f1", &animation_clip()),
        )
        .unwrap(),
        TemporaryFixture::new(
            "oracle-animation-tuanjie-1.6.assets",
            &synthetic_single_v22(74, 74, "2022.3.61t1", &tuanjie_animation_clip()),
        )
        .unwrap(),
        TemporaryFixture::new(
            "oracle-animation-tuanjie-1.5.assets",
            &synthetic_single_v22(74, 74, "2022.3.55t4", &tuanjie_embedded_animation_clip()),
        )
        .unwrap(),
        TemporaryFixture::with_resource(
            "oracle-v22.assets",
            &synthetic_v22(),
            "oracle.resS",
            &oracle_resource(),
        )
        .unwrap(),
    ];
    for fixture in &fixtures {
        let managed = managed_manifest(&executable, fixture.input_path()).unwrap();
        let rust = rust_manifest(fixture.input_path(), 1024 * 1024).unwrap();
        assert_eq!(managed, rust, "fixture {}", fixture.path.display());
    }

    assert_truncated_fixture(&executable);
}

fn assert_truncated_fixture(executable: &Path) {
    let mut bytes = synthetic_v22();
    bytes.truncate(bytes.len() - 3);
    let fixture = TemporaryFixture::new("oracle-truncated.assets", &bytes).unwrap();
    let managed = managed_manifest(executable, fixture.input_path()).unwrap();
    assert_eq!(managed, json!({ "Files": [], "Resources": [] }));
    let rust = Studio::open(fixture.input_path()).unwrap();
    assert_eq!(rust.file_count(), 0);
}

fn build_managed_oracle() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let project =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../oracle/AssetStudioOracle.csproj");
    let build = Command::new("dotnet")
        .args([
            "build",
            project.to_str().ok_or("oracle project path is not UTF-8")?,
            "--configuration",
            "Release",
            "--framework",
            "net10.0",
            "--no-restore",
            "--nologo",
            "--verbosity",
            "quiet",
            "-p:NuGetAudit=false",
        ])
        .output()?;
    if !build.status.success() {
        return Err(format!(
            "managed oracle build failed with {}:\n{}\n{}",
            build.status,
            String::from_utf8_lossy(&build.stdout),
            String::from_utf8_lossy(&build.stderr)
        )
        .into());
    }
    Ok(project
        .parent()
        .ok_or("oracle project has no parent")?
        .join("bin/Release/net10.0/AssetStudioOracle.dll"))
}

fn managed_manifest(executable: &Path, path: &Path) -> Result<Value, Box<dyn std::error::Error>> {
    let output = Command::new("dotnet")
        .args([
            executable
                .to_str()
                .ok_or("oracle executable path is not UTF-8")?,
            path.to_str().ok_or("fixture path is not UTF-8")?,
        ])
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "managed oracle failed with {}:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    if !output.stderr.is_empty() {
        return Err(format!(
            "managed oracle emitted diagnostics:\n{}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

struct TemporaryFixture {
    directory: PathBuf,
    path: PathBuf,
    resource_path: Option<PathBuf>,
}

impl TemporaryFixture {
    fn new(name: &str, bytes: &[u8]) -> std::io::Result<Self> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "assetstudio-dotnet-oracle-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&directory)?;
        let path = directory.join(name);
        fs::write(&path, bytes)?;
        Ok(Self {
            directory,
            path,
            resource_path: None,
        })
    }

    fn with_resource(
        name: &str,
        bytes: &[u8],
        resource_name: &str,
        resource: &[u8],
    ) -> std::io::Result<Self> {
        let mut fixture = Self::new(name, bytes)?;
        let resource_path = fixture.directory.join(resource_name);
        fs::write(&resource_path, resource)?;
        fixture.resource_path = Some(resource_path);
        Ok(fixture)
    }

    fn input_path(&self) -> &Path {
        &self.directory
    }
}

impl Drop for TemporaryFixture {
    fn drop(&mut self) {
        if let Some(resource_path) = &self.resource_path {
            let _ = fs::remove_file(resource_path);
        }
        let _ = fs::remove_file(&self.path);
        let _ = fs::remove_dir(&self.directory);
    }
}

fn synthetic_v22() -> Vec<u8> {
    let objects = [
        (49, 91_i64, text_asset()),
        (128, -4_i64, font()),
        (28, 7_i64, texture2d()),
        (83, 42_i64, audio_clip()),
        (329, 5_i64, video_clip()),
        (141, 8_i64, build_settings()),
        (129, 9_i64, player_settings()),
        (123_456, 10_i64, dump_object()),
    ];
    let classes = [28, 49, 83, 128, 129, 141, 329, 123_456];
    let mut metadata = Vec::new();
    metadata.extend_from_slice(b"2022.3.62f1\0");
    push_i32(&mut metadata, 13);
    metadata.push(1);
    push_i32(&mut metadata, i32::try_from(classes.len()).unwrap());
    for class_id in classes {
        push_i32(&mut metadata, class_id);
        metadata.push(0);
        metadata.extend_from_slice(&(-1_i16).to_le_bytes());
        metadata.extend_from_slice(&[0; 16]);
        if class_id == 123_456 {
            push_blob_tree(
                &mut metadata,
                &[
                    TestNode::new("Root", "Base", 0, false),
                    TestNode::new("string", "m_Name", 1, false),
                    TestNode::new("Array", "Array", 2, true),
                    TestNode::new("int", "size", 3, false),
                    TestNode::new("char", "data", 3, false),
                    TestNode::new("vector", "values", 1, false),
                    TestNode::new("Array", "Array", 2, false),
                    TestNode::new("int", "size", 3, false),
                    TestNode::new("SInt32", "data", 3, false),
                    TestNode::new("bool", "enabled", 1, false),
                    TestNode::new("float", "weight", 1, false),
                ],
            );
        } else {
            push_i32(&mut metadata, 0);
            push_i32(&mut metadata, 0);
        }
        push_i32(&mut metadata, 0);
    }

    let mut data = Vec::new();
    let mut records = Vec::new();
    for (class_id, path_id, payload) in objects {
        align(&mut data, 4);
        records.push((
            path_id,
            i64::try_from(data.len()).unwrap(),
            u32::try_from(payload.len()).unwrap(),
            i32::try_from(classes.iter().position(|value| *value == class_id).unwrap()).unwrap(),
        ));
        data.extend_from_slice(&payload);
    }
    push_i32(&mut metadata, i32::try_from(records.len()).unwrap());
    for (path_id, offset, size, type_index) in records {
        align_with_base(&mut metadata, 48, 4);
        metadata.extend_from_slice(&path_id.to_le_bytes());
        metadata.extend_from_slice(&offset.to_le_bytes());
        metadata.extend_from_slice(&size.to_le_bytes());
        push_i32(&mut metadata, type_index);
    }
    for _ in 0..3 {
        push_i32(&mut metadata, 0);
    }
    metadata.push(0);
    finish_v22(&metadata, &data)
}

fn synthetic_single_v22(class_id: i32, path_id: i64, version: &str, data: &[u8]) -> Vec<u8> {
    let mut metadata = Vec::new();
    metadata.extend_from_slice(version.as_bytes());
    metadata.push(0);
    push_i32(&mut metadata, 13);
    metadata.push(0);
    push_i32(&mut metadata, 1);
    push_i32(&mut metadata, class_id);
    metadata.push(0);
    metadata.extend_from_slice(&(-1_i16).to_le_bytes());
    metadata.extend_from_slice(&[0; 16]);
    push_i32(&mut metadata, 1);
    align_with_base(&mut metadata, 48, 4);
    metadata.extend_from_slice(&path_id.to_le_bytes());
    metadata.extend_from_slice(&0_i64.to_le_bytes());
    metadata.extend_from_slice(&u32::try_from(data.len()).unwrap().to_le_bytes());
    push_i32(&mut metadata, 0);
    for _ in 0..3 {
        push_i32(&mut metadata, 0);
    }
    metadata.push(0);
    finish_v22(&metadata, data)
}

fn synthetic_sprite_v22() -> Vec<u8> {
    synthetic_plain_v22(
        "2022.3.62f1",
        &[(213, 213, sprite()), (28, 214, sprite_texture())],
    )
}

fn synthetic_plain_v22(version: &str, objects: &[(i32, i64, Vec<u8>)]) -> Vec<u8> {
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
        metadata.extend_from_slice(&[0; 16]);
    }

    let mut data = Vec::new();
    let mut records = Vec::new();
    for (class_id, path_id, payload) in objects {
        align(&mut data, 4);
        records.push((
            *path_id,
            i64::try_from(data.len()).unwrap(),
            u32::try_from(payload.len()).unwrap(),
            i32::try_from(classes.iter().position(|value| value == class_id).unwrap()).unwrap(),
        ));
        data.extend_from_slice(payload);
    }
    push_i32(&mut metadata, i32::try_from(records.len()).unwrap());
    for (path_id, offset, size, type_index) in records {
        align_with_base(&mut metadata, 48, 4);
        metadata.extend_from_slice(&path_id.to_le_bytes());
        metadata.extend_from_slice(&offset.to_le_bytes());
        metadata.extend_from_slice(&size.to_le_bytes());
        push_i32(&mut metadata, type_index);
    }
    for _ in 0..3 {
        push_i32(&mut metadata, 0);
    }
    metadata.push(0);
    finish_v22(&metadata, &data)
}

fn synthetic_v13_big_endian() -> Vec<u8> {
    let object = text_asset_be();
    let mut metadata = Vec::new();
    metadata.extend_from_slice(b"2018.4.36f1\0");
    push_i32_be(&mut metadata, 13);
    metadata.push(0);
    push_i32_be(&mut metadata, 1);
    push_i32_be(&mut metadata, 49);
    metadata.extend_from_slice(&[0x5a; 16]);
    push_i32_be(&mut metadata, 0);
    push_i32_be(&mut metadata, 1);
    push_i32_be(&mut metadata, 0x1020_3040);
    push_u32_be(&mut metadata, 0);
    push_u32_be(&mut metadata, u32::try_from(object.len()).unwrap());
    push_i32_be(&mut metadata, 49);
    metadata.extend_from_slice(&49_u16.to_be_bytes());
    metadata.extend_from_slice(&(-1_i16).to_be_bytes());
    push_i32_be(&mut metadata, 0);
    push_i32_be(&mut metadata, 0);
    metadata.push(0);
    finish_v13(&metadata, &object, 1)
}

fn text_asset() -> Vec<u8> {
    let mut output = Vec::new();
    push_string(&mut output, "oracle.txt");
    push_i32(&mut output, 13);
    output.extend_from_slice(b"oracle payload");
    output
}

fn text_asset_be() -> Vec<u8> {
    let mut output = Vec::new();
    push_i32_be(&mut output, 9);
    output.extend_from_slice(b"legacy.be");
    align(&mut output, 4);
    push_i32_be(&mut output, 6);
    output.extend_from_slice(b"bigend");
    output
}

fn font() -> Vec<u8> {
    let mut output = Vec::new();
    push_string(&mut output, "oracle-font");
    output.extend_from_slice(&0_f32.to_le_bytes());
    push_pptr(&mut output);
    output.extend_from_slice(&12_f32.to_le_bytes());
    push_pptr(&mut output);
    output.extend_from_slice(&[0; 20]);
    push_i32(&mut output, 0);
    push_i32(&mut output, 0);
    output.extend_from_slice(&1_f32.to_le_bytes());
    push_i32(&mut output, 8);
    output.extend_from_slice(b"OTTOfont");
    output
}

fn movie_texture() -> Vec<u8> {
    let mut output = Vec::new();
    push_string(&mut output, "oracle-movie");
    output.extend_from_slice(&[0; 5]);
    align(&mut output, 4);
    output.push(1);
    align(&mut output, 4);
    push_pptr(&mut output);
    push_i32(&mut output, 4);
    output.extend_from_slice(b"OggS");
    output
}

fn material() -> Vec<u8> {
    let mut output = Vec::new();
    push_string(&mut output, "oracle-material");
    push_i32(&mut output, 1);
    output.extend_from_slice(&42_i64.to_le_bytes());
    push_string_array(&mut output, &["FOO", "BAR"]);
    push_string_array(&mut output, &["OLD"]);
    push_u32(&mut output, 3);
    output.push(1);
    align(&mut output, 4);
    push_i32(&mut output, 2_450);
    push_i32(&mut output, 2);
    push_string(&mut output, "RenderType");
    push_string(&mut output, "Opaque");
    push_string(&mut output, "RenderType");
    push_string(&mut output, "Cutout");
    push_string_array(&mut output, &["ShadowCaster"]);

    push_i32(&mut output, 1);
    push_string(&mut output, "_MainTex");
    push_i32(&mut output, 0);
    output.extend_from_slice(&9_i64.to_le_bytes());
    for value in [2.0_f32, 3.0, 0.25, 0.5] {
        output.extend_from_slice(&value.to_le_bytes());
    }
    push_i32(&mut output, 2);
    push_named_i32(&mut output, "_Mode", 1);
    push_named_i32(&mut output, "_Mode", 2);
    push_i32(&mut output, 1);
    push_named_f32(&mut output, "_Glossiness", 0.75);
    push_i32(&mut output, 1);
    push_string(&mut output, "_Color");
    for value in [1.0_f32, 0.5, 0.25, 1.0] {
        output.extend_from_slice(&value.to_le_bytes());
    }
    // The managed reader deliberately leaves this empty
    // m_BuildTextureStacks vector in the object tail.
    push_i32(&mut output, 0);
    output
}

fn sprite() -> Vec<u8> {
    let mut output = Vec::new();
    push_string(&mut output, "oracle-sprite");
    push_floats(&mut output, &[0.0, 0.0, 2.0, 2.0]);
    push_floats(&mut output, &[0.0, 0.0]);
    push_floats(&mut output, &[0.0; 4]);
    push_floats(&mut output, &[100.0, 0.5, 0.5]);
    push_u32(&mut output, 0);
    output.push(0);
    align(&mut output, 4);
    output.extend_from_slice(&[0; 16]);
    output.extend_from_slice(&0_i64.to_le_bytes());
    push_i32(&mut output, 0);
    push_pptr(&mut output);

    push_i32(&mut output, 0);
    output.extend_from_slice(&214_i64.to_le_bytes());
    push_pptr(&mut output);
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
    push_floats(&mut output, &[1.0, 0.0, 2.0, 2.0]);
    push_floats(&mut output, &[0.0, 0.0]);
    push_floats(&mut output, &[0.0, 0.0]);
    push_u32(&mut output, 2);
    push_floats(&mut output, &[0.0, 0.0, 1.0, 1.0, 1.0]);
    output
}

fn sprite_texture() -> Vec<u8> {
    let mut pixels = Vec::new();
    for y in 0..3_u8 {
        for x in 0..4_u8 {
            pixels.extend_from_slice(&[x + y * 10, 40, 80, 255]);
        }
    }
    let mut output = Vec::new();
    push_string(&mut output, "oracle-sprite-texture");
    push_i32(&mut output, 0);
    output.extend_from_slice(&[0, 0]);
    align(&mut output, 4);
    push_i32(&mut output, 4);
    push_i32(&mut output, 3);
    push_u32(&mut output, u32::try_from(pixels.len()).unwrap());
    push_i32(&mut output, 0);
    push_i32(&mut output, 4);
    push_i32(&mut output, 1);
    output.extend_from_slice(&[0, 0, 0]);
    align(&mut output, 4);
    push_string(&mut output, "");
    output.push(0);
    align(&mut output, 4);
    push_i32(&mut output, 0);
    push_i32(&mut output, 1);
    push_i32(&mut output, 2);
    output.extend_from_slice(&[0; 24]);
    for _ in 0..3 {
        push_i32(&mut output, 0);
    }
    align(&mut output, 4);
    push_i32(&mut output, i32::try_from(pixels.len()).unwrap());
    output.extend_from_slice(&pixels);
    output
}

fn shader() -> Vec<u8> {
    let script = b"Shader \"Oracle/Legacy\" { SubShader { Pass {} } }\n";
    let mut output = Vec::new();
    push_string(&mut output, "oracle-legacy-shader");
    push_i32(&mut output, i32::try_from(script.len()).unwrap());
    output.extend_from_slice(script);
    align(&mut output, 4);
    push_string(&mut output, "Oracle/Legacy.shader");
    output
}

fn animator_controller() -> Vec<u8> {
    let mut output = Vec::new();
    push_string(&mut output, "oracle-controller");
    push_u32(&mut output, 0);
    for _ in 0..9 {
        push_i32(&mut output, 0);
    }
    push_i32(&mut output, 2);
    push_i32(&mut output, 17);
    push_i32(&mut output, -9);
    push_i32(&mut output, 1);
    push_u32(&mut output, 0xdead_beef);
    push_string(&mut output, "Root/Body");
    push_i32(&mut output, 1);
    push_i32(&mut output, 0);
    output.extend_from_slice(&92_i64.to_le_bytes());
    output
}

fn tuanjie_animator_controller() -> Vec<u8> {
    let mut output = Vec::new();
    push_string(&mut output, "oracle-tuanjie-controller");
    push_u32(&mut output, 0);
    for _ in 0..9 {
        push_i32(&mut output, 0);
    }
    push_i32(&mut output, 1);
    push_u32(&mut output, 0xdead_beef);
    push_string(&mut output, "Root/Body");
    push_i32(&mut output, 1);
    push_i32(&mut output, 0);
    output.extend_from_slice(&92_i64.to_le_bytes());
    output
}

fn animation_clip() -> Vec<u8> {
    let mut output = Vec::new();
    push_string(&mut output, "oracle-animation");
    output.extend_from_slice(&[0, 0, 0]);
    align(&mut output, 4);
    for _ in 0..7 {
        push_i32(&mut output, 0);
    }
    push_f32(&mut output, 60.0);
    push_i32(&mut output, 2);
    push_zero_f32s(&mut output, 6);
    push_u32(&mut output, 0);

    push_animation_xform(&mut output);
    push_zero_f32s(&mut output, 7);
    push_i32(&mut output, 0);
    for _ in 0..2 {
        push_animation_xform(&mut output);
        push_i32(&mut output, 0);
        push_zero_f32s(&mut output, 4);
    }
    push_i32(&mut output, 0);
    push_i32(&mut output, 0);

    for _ in 0..4 {
        push_animation_xform(&mut output);
    }
    push_zero_f32s(&mut output, 3);
    push_i32(&mut output, 0);
    output.extend_from_slice(&2_u16.to_le_bytes());
    output.extend_from_slice(&1_u16.to_le_bytes());
    push_i32(&mut output, 0);
    push_u32(&mut output, 0);
    push_f32(&mut output, 30.0);
    push_f32(&mut output, 0.0);
    push_i32(&mut output, 0);
    push_i32(&mut output, 0);
    push_zero_f32s(&mut output, 6);
    push_i32(&mut output, 0);
    push_i32(&mut output, 0);
    push_i32(&mut output, 0);
    output.extend_from_slice(&[0; 11]);
    align(&mut output, 4);

    push_i32(&mut output, 0);
    push_i32(&mut output, 0);
    output.extend_from_slice(&[0, 0]);
    align(&mut output, 4);
    push_i32(&mut output, 0);
    align(&mut output, 4);
    output
}

fn tuanjie_animation_clip() -> Vec<u8> {
    let mut output = Vec::new();
    push_string(&mut output, "oracle-tuanjie-animation");
    output.extend_from_slice(&[0, 0, 0]);
    align(&mut output, 4);
    for _ in 0..4 {
        push_i32(&mut output, 0);
    }
    push_f32(&mut output, 60.0);
    push_i32(&mut output, 2);
    push_zero_f32s(&mut output, 6);
    for _ in 0..3 {
        push_i32(&mut output, 0);
    }
    push_u32(&mut output, 0);

    push_animation_xform(&mut output);
    push_zero_f32s(&mut output, 7);
    push_i32(&mut output, 0);
    for _ in 0..2 {
        push_animation_xform(&mut output);
        push_i32(&mut output, 0);
        push_zero_f32s(&mut output, 4);
    }
    push_i32(&mut output, 0);
    push_i32(&mut output, 0);
    for _ in 0..4 {
        push_animation_xform(&mut output);
    }
    push_zero_f32s(&mut output, 3);
    push_i32(&mut output, 0);
    output.extend_from_slice(&2_u16.to_le_bytes());
    output.extend_from_slice(&1_u16.to_le_bytes());
    push_i32(&mut output, 0);
    push_u32(&mut output, 0);
    push_f32(&mut output, 30.0);
    push_f32(&mut output, 0.0);
    push_i32(&mut output, 0);
    push_i32(&mut output, 0);
    push_u32(&mut output, 12);
    push_u32(&mut output, 3);
    push_f32(&mut output, 30.0);
    push_u32(&mut output, 7);
    push_i32(&mut output, 3);
    output.extend_from_slice(&[0xaa, 0xbb, 0xcc]);
    align(&mut output, 4);
    push_i32(&mut output, 2);
    push_u32(&mut output, 0x10);
    push_u32(&mut output, 0x20);
    output.push(1);
    align(&mut output, 4);
    push_zero_f32s(&mut output, 6);
    push_i32(&mut output, 0);
    push_i32(&mut output, 0);
    push_i32(&mut output, 0);
    output.extend_from_slice(&[0; 11]);
    align(&mut output, 4);

    output.extend_from_slice(&0x1020_3040_5060_7080_i64.to_le_bytes());
    push_u32(&mut output, 0x1234);
    push_string(&mut output, "archive:/animation.resS");
    push_i32(&mut output, 0);
    push_i32(&mut output, 0);
    output.extend_from_slice(&[0, 0]);
    align(&mut output, 4);
    push_i32(&mut output, 0);
    align(&mut output, 4);
    output
}

fn tuanjie_embedded_animation_clip() -> Vec<u8> {
    let mut output = Vec::new();
    push_string(&mut output, "oracle-tuanjie-embedded-animation");
    output.extend_from_slice(&[0, 0, 0]);
    align(&mut output, 4);
    for _ in 0..4 {
        push_i32(&mut output, 0);
    }
    push_f32(&mut output, 60.0);
    push_i32(&mut output, 2);
    push_zero_f32s(&mut output, 6);

    let mut embedded = Vec::new();
    push_animation_xform(&mut embedded);
    push_zero_f32s(&mut embedded, 7);
    push_i32(&mut embedded, 0);
    for _ in 0..2 {
        push_animation_xform(&mut embedded);
        push_i32(&mut embedded, 0);
        push_zero_f32s(&mut embedded, 4);
    }
    push_i32(&mut embedded, 0);
    push_i32(&mut embedded, 0);
    for _ in 0..4 {
        push_animation_xform(&mut embedded);
    }
    push_zero_f32s(&mut embedded, 3);
    push_i32(&mut embedded, 0);
    embedded.extend_from_slice(&2_u16.to_le_bytes());
    embedded.extend_from_slice(&1_u16.to_le_bytes());
    push_i32(&mut embedded, 0);
    push_u32(&mut embedded, 0);
    push_f32(&mut embedded, 30.0);
    push_f32(&mut embedded, 0.0);
    push_i32(&mut embedded, 0);
    push_i32(&mut embedded, 0);
    push_u32(&mut embedded, 12);
    push_u32(&mut embedded, 3);
    push_f32(&mut embedded, 30.0);
    push_u32(&mut embedded, 7);
    push_i32(&mut embedded, 3);
    embedded.extend_from_slice(&[0xaa, 0xbb, 0xcc]);
    push_i32(&mut embedded, 2);
    push_u32(&mut embedded, 0x10);
    push_u32(&mut embedded, 0x20);
    embedded.push(1);
    push_zero_f32s(&mut embedded, 6);
    push_i32(&mut embedded, 0);
    push_i32(&mut embedded, 0);
    push_i32(&mut embedded, 0);
    embedded.extend_from_slice(&[0; 11]);
    align(&mut embedded, 4);

    push_u32(&mut output, u32::try_from(embedded.len()).unwrap());
    push_i32(&mut output, i32::try_from(embedded.len()).unwrap());
    output.extend_from_slice(&embedded);
    output.extend_from_slice(&0x1020_3040_5060_7080_i64.to_le_bytes());
    push_u32(&mut output, 0x1234);
    push_string(&mut output, "archive:/animation.resS");
    push_i32(&mut output, 0);
    push_i32(&mut output, 0);
    output.extend_from_slice(&[0, 0]);
    align(&mut output, 4);
    push_i32(&mut output, 0);
    align(&mut output, 4);
    output
}

fn push_animation_xform(output: &mut Vec<u8>) {
    push_zero_f32s(output, 10);
}

fn push_zero_f32s(output: &mut Vec<u8>, count: usize) {
    for _ in 0..count {
        push_f32(output, 0.0);
    }
}

fn avatar() -> Vec<u8> {
    let mut output = Vec::new();
    push_string(&mut output, "oracle-avatar");
    push_u32(&mut output, 0);
    push_empty_skeleton(&mut output);
    push_i32(&mut output, 0);
    push_i32(&mut output, 0);
    push_i32(&mut output, 0);
    push_avatar_xform(&mut output);
    push_empty_skeleton(&mut output);
    for _ in 0..5 {
        push_i32(&mut output, 0);
    }
    for value in [1.0_f32, 0.5, 0.5, 0.5, 0.5, 0.05, 0.05, 0.0] {
        output.extend_from_slice(&value.to_le_bytes());
    }
    output.extend_from_slice(&[0, 0, 0]);
    align(&mut output, 4);
    push_i32(&mut output, 0);
    push_i32(&mut output, 0);
    push_i32(&mut output, -1);
    push_avatar_xform(&mut output);
    push_empty_skeleton(&mut output);
    push_i32(&mut output, 0);
    push_i32(&mut output, 0);
    push_i32(&mut output, 1);
    push_u32(&mut output, 0xfeed_beef);
    push_string(&mut output, "Root/Hips");
    push_i32(&mut output, 0);
    push_i32(&mut output, 0);
    for value in [0.5_f32, 0.5, 0.5, 0.5, 0.05, 0.05, 0.0, 1.0] {
        output.extend_from_slice(&value.to_le_bytes());
    }
    push_string(&mut output, "Hips");
    output.extend_from_slice(&[1, 0, 1]);
    align(&mut output, 4);
    output
}

fn push_empty_skeleton(output: &mut Vec<u8>) {
    push_i32(output, 0);
    push_i32(output, 0);
    push_i32(output, 0);
}

fn push_avatar_xform(output: &mut Vec<u8>) {
    for value in [0.0_f32, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0] {
        output.extend_from_slice(&value.to_le_bytes());
    }
}

fn push_string_array(output: &mut Vec<u8>, values: &[&str]) {
    push_i32(output, i32::try_from(values.len()).unwrap());
    for value in values {
        push_string(output, value);
    }
}

fn push_named_i32(output: &mut Vec<u8>, name: &str, value: i32) {
    push_string(output, name);
    push_i32(output, value);
}

fn push_named_f32(output: &mut Vec<u8>, name: &str, value: f32) {
    push_string(output, name);
    output.extend_from_slice(&value.to_le_bytes());
}

fn push_floats(output: &mut Vec<u8>, values: &[f32]) {
    for value in values {
        output.extend_from_slice(&value.to_le_bytes());
    }
}

#[allow(clippy::too_many_lines)]
fn mesh() -> Vec<u8> {
    mesh_payload(None, None)
}

#[allow(clippy::too_many_lines)]
fn tuanjie_mesh() -> Vec<u8> {
    mesh_payload(None, Some(3))
}

#[allow(clippy::too_many_lines)]
fn mesh_payload(stream: Option<(u64, u32, &str)>, tuanjie_revision: Option<u8>) -> Vec<u8> {
    let mut output = Vec::new();
    push_string(&mut output, "oracle-mesh");
    push_i32(&mut output, 1);
    push_u32(&mut output, 0);
    push_u32(&mut output, 3);
    push_i32(&mut output, 0);
    push_u32(&mut output, 0);
    push_u32(&mut output, 0);
    push_u32(&mut output, 3);
    output.extend_from_slice(&[0; 24]);

    for _ in 0..4 {
        push_i32(&mut output, 0);
    }
    push_i32(&mut output, 0);
    push_i32(&mut output, 0);
    push_u32(&mut output, 0);
    push_i32(&mut output, 0);
    push_i32(&mut output, 0);

    output.extend_from_slice(&[0, 1, 0, 0]);
    if let Some(revision) = tuanjie_revision {
        if revision == 3 {
            align(&mut output, 4);
        }
        push_tuanjie_shared_cluster(&mut output, revision);
        align(&mut output, 4);
    } else {
        align(&mut output, 4);
    }
    push_i32(&mut output, 0);
    push_i32(&mut output, 6);
    for index in 0..3_u16 {
        output.extend_from_slice(&index.to_le_bytes());
    }
    align(&mut output, 4);

    push_u32(&mut output, 3);
    push_i32(&mut output, 5);
    output.extend_from_slice(&[0, 0, 0, 3]);
    output.extend_from_slice(&[1, 0, 0, 3]);
    output.extend_from_slice(&[0, 0, 0, 0]);
    output.extend_from_slice(&[0, 0, 0, 0]);
    output.extend_from_slice(&[2, 0, 0, 2]);
    let vertex_data = mesh_vertex_data();
    if stream.is_some() {
        push_i32(&mut output, 0);
    } else {
        push_i32(&mut output, i32::try_from(vertex_data.len()).unwrap());
        output.extend_from_slice(&vertex_data);
    }
    align(&mut output, 4);

    for _ in 0..4 {
        push_empty_packed_float(&mut output);
    }
    for _ in 0..3 {
        push_empty_packed_int(&mut output);
    }
    push_empty_packed_float(&mut output);
    for _ in 0..2 {
        push_empty_packed_int(&mut output);
    }
    push_u32(&mut output, 0);

    output.extend_from_slice(&[0; 24]);
    for _ in 0..4 {
        push_i32(&mut output, 0);
    }
    output.extend_from_slice(&[0; 8]);
    let (stream_offset, stream_size, stream_path) = stream.unwrap_or((0, 0, ""));
    output.extend_from_slice(&i64::try_from(stream_offset).unwrap().to_le_bytes());
    push_u32(&mut output, stream_size);
    push_string(&mut output, stream_path);
    if tuanjie_revision.is_some() {
        output.extend_from_slice(&[1, 0]);
    }
    output
}

fn push_tuanjie_shared_cluster(output: &mut Vec<u8>, revision: u8) {
    assert!((1..=3).contains(&revision));
    push_i32(output, 0);
    output.extend_from_slice(&1.0_f32.to_le_bytes());
    if revision == 1 {
        output.extend_from_slice(&[0_u8; 16]);
    }
    push_i32(output, 3);
    output.extend_from_slice(&[0xa5, 0x5a, 0xc3]);
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

fn mesh_vertex_data() -> Vec<u8> {
    let vertices = [
        ([1.5_f32, 0.0, 0.0], [1.0_f32, 0.0, 0.0], [0.0_f32, 0.0]),
        ([0.0, 2.0, 0.0], [0.0, 1.0, 0.0], [1.0, 0.0]),
        ([0.0, 0.0, 3.0], [0.0, 0.0, 1.0], [0.0, 1.0]),
    ];
    let mut vertex_data = Vec::new();
    for (position, _, _) in vertices {
        for value in position {
            vertex_data.extend_from_slice(&value.to_le_bytes());
        }
    }
    vertex_data.resize(48, 0);
    for (_, normal, _) in vertices {
        for value in normal {
            vertex_data.extend_from_slice(&value.to_le_bytes());
        }
    }
    vertex_data.resize(96, 0);
    for (_, _, uv) in vertices {
        for value in uv {
            vertex_data.extend_from_slice(&value.to_le_bytes());
        }
    }
    vertex_data
}

fn streamed_mesh_resource() -> Vec<u8> {
    let mut resource = vec![0xa5; 7];
    resource.extend_from_slice(&mesh_vertex_data());
    resource
}

fn push_empty_packed_float(output: &mut Vec<u8>) {
    push_u32(output, 0);
    output.extend_from_slice(&[0; 8]);
    push_i32(output, 0);
    align(output, 4);
    output.push(0);
    align(output, 4);
}

fn push_empty_packed_int(output: &mut Vec<u8>) {
    push_u32(output, 0);
    push_i32(output, 0);
    align(output, 4);
    output.push(0);
    align(output, 4);
}

fn texture2d() -> Vec<u8> {
    let mut output = Vec::new();
    push_string(&mut output, "oracle-texture");
    push_i32(&mut output, 0);
    output.extend_from_slice(&[0, 0]);
    align(&mut output, 4);
    push_i32(&mut output, 2);
    push_i32(&mut output, 2);
    output.extend_from_slice(&16_u32.to_le_bytes());
    push_i32(&mut output, 0);
    push_i32(&mut output, 4);
    push_i32(&mut output, 1);
    output.extend_from_slice(&[1, 0, 0]);
    align(&mut output, 4);
    push_string(&mut output, "");
    output.push(0);
    align(&mut output, 4);
    push_i32(&mut output, 0);
    push_i32(&mut output, 1);
    push_i32(&mut output, 2);
    output.extend_from_slice(&[0; 24]);
    push_i32(&mut output, 0);
    push_i32(&mut output, 0);
    push_i32(&mut output, 0);
    push_i32(&mut output, 0);
    output.extend_from_slice(&4_i64.to_le_bytes());
    output.extend_from_slice(&16_u32.to_le_bytes());
    push_string(&mut output, "oracle.resS");
    output
}

fn audio_clip() -> Vec<u8> {
    let mut output = Vec::new();
    push_string(&mut output, "oracle-audio");
    push_i32(&mut output, 1);
    push_i32(&mut output, 2);
    push_i32(&mut output, 48_000);
    push_i32(&mut output, 16);
    output.extend_from_slice(&0.5_f32.to_le_bytes());
    output.push(0);
    align(&mut output, 4);
    push_i32(&mut output, 0);
    output.extend_from_slice(&[1, 0, 1]);
    align(&mut output, 4);
    push_string(&mut output, "oracle.resS");
    output.extend_from_slice(&24_i64.to_le_bytes());
    output.extend_from_slice(&8_i64.to_le_bytes());
    push_i32(&mut output, 1);
    output
}

fn video_clip() -> Vec<u8> {
    let mut output = Vec::new();
    push_string(&mut output, "oracle-video");
    push_string(&mut output, "movies/oracle.mp4");
    for value in [0_u32, 0, 1_920, 1_080, 1, 1] {
        output.extend_from_slice(&value.to_le_bytes());
    }
    output.extend_from_slice(&30_f64.to_le_bytes());
    output.extend_from_slice(&300_u64.to_le_bytes());
    push_i32(&mut output, 0);
    for _ in 0..4 {
        push_i32(&mut output, 0);
    }
    push_string(&mut output, "oracle.resS");
    output.extend_from_slice(&40_i64.to_le_bytes());
    output.extend_from_slice(&9_i64.to_le_bytes());
    output.extend_from_slice(&[0, 1]);
    output
}

fn oracle_resource() -> Vec<u8> {
    let mut output = vec![0xcc; 64];
    output[4..20].copy_from_slice(&[255, 0, 0, 1, 0, 255, 0, 2, 0, 0, 255, 3, 255, 255, 255, 4]);
    output[24..32].copy_from_slice(b"OggSdata");
    output[40..49].copy_from_slice(b"video-bin");
    output
}

fn build_settings() -> Vec<u8> {
    let mut output = Vec::new();
    push_i32(&mut output, 2);
    push_string(&mut output, "Assets/Intro.unity");
    push_string(&mut output, "Assets/Game.unity");
    output
}

fn player_settings() -> Vec<u8> {
    let mut output = vec![0; 16];
    output.push(1);
    align(&mut output, 4);
    push_i32(&mut output, 1);
    push_i32(&mut output, 2);
    output.push(0);
    align(&mut output, 4);
    push_i32(&mut output, 60);
    push_string(&mut output, "Haruki");
    push_string(&mut output, "Asset Studio Rust");
    output
}

fn dump_object() -> Vec<u8> {
    let mut output = Vec::new();
    push_string(&mut output, "Oracle Dump");
    push_i32(&mut output, 2);
    push_i32(&mut output, -4);
    push_i32(&mut output, 7);
    output.push(1);
    output.extend_from_slice(&1.25_f32.to_le_bytes());
    output
}

#[derive(Clone, Copy)]
struct TestNode {
    type_name: &'static str,
    field_name: &'static str,
    level: u8,
    align: bool,
}

impl TestNode {
    const fn new(
        type_name: &'static str,
        field_name: &'static str,
        level: u8,
        align: bool,
    ) -> Self {
        Self {
            type_name,
            field_name,
            level,
            align,
        }
    }
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
        output.extend_from_slice(&i32::try_from(index).unwrap().to_le_bytes());
        output.extend_from_slice(&(if node.align { 0x4000_i32 } else { 0 }).to_le_bytes());
        output.extend_from_slice(&0_u64.to_le_bytes());
    }
    output.extend_from_slice(&strings);
}

fn finish_v22(metadata: &[u8], data: &[u8]) -> Vec<u8> {
    let data_offset = (48 + metadata.len()).next_multiple_of(16);
    let file_size = data_offset + data.len();
    let mut output = vec![0; 48];
    output[8..12].copy_from_slice(&22_u32.to_be_bytes());
    output[20..24].copy_from_slice(&u32::try_from(metadata.len()).unwrap().to_be_bytes());
    output[24..32].copy_from_slice(&i64::try_from(file_size).unwrap().to_be_bytes());
    output[32..40].copy_from_slice(&i64::try_from(data_offset).unwrap().to_be_bytes());
    output.extend_from_slice(metadata);
    output.resize(data_offset, 0);
    output.extend_from_slice(data);
    output
}

fn finish_v13(metadata: &[u8], data: &[u8], endianness: u8) -> Vec<u8> {
    let data_offset = (20 + metadata.len()).next_multiple_of(16);
    let file_size = data_offset + data.len();
    let mut output = vec![0; 20];
    output[0..4].copy_from_slice(&u32::try_from(metadata.len()).unwrap().to_be_bytes());
    output[4..8].copy_from_slice(&u32::try_from(file_size).unwrap().to_be_bytes());
    output[8..12].copy_from_slice(&13_u32.to_be_bytes());
    output[12..16].copy_from_slice(&u32::try_from(data_offset).unwrap().to_be_bytes());
    output[16] = endianness;
    output.extend_from_slice(metadata);
    output.resize(data_offset, 0);
    output.extend_from_slice(data);
    output
}

fn push_string(output: &mut Vec<u8>, value: &str) {
    push_i32(output, i32::try_from(value.len()).unwrap());
    output.extend_from_slice(value.as_bytes());
    align(output, 4);
}

fn push_i32(output: &mut Vec<u8>, value: i32) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn push_u32(output: &mut Vec<u8>, value: u32) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn push_f32(output: &mut Vec<u8>, value: f32) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn push_i32_be(output: &mut Vec<u8>, value: i32) {
    output.extend_from_slice(&value.to_be_bytes());
}

fn push_u32_be(output: &mut Vec<u8>, value: u32) {
    output.extend_from_slice(&value.to_be_bytes());
}

fn push_pptr(output: &mut Vec<u8>) {
    push_i32(output, 0);
    output.extend_from_slice(&0_i64.to_le_bytes());
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
