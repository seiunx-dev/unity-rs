//! Differential fixture gate against the managed implementation.
//!
//! The managed reader lives in the separate `Team-Haruki/AssetStudio`
//! repository and is a compatibility oracle only; nothing shipped from this
//! repository depends on it. Point `ASSETSTUDIO_REPO` at a checkout, or keep
//! that repository as a sibling directory of this one.
//!
//! The matrix deliberately covers v13 big-endian metadata/object payloads and
//! v22 little-endian assets with external `Texture2D`, `AudioClip`, and
//! `VideoClip` resource ranges plus resident Material, Mesh, and Sprite objects. It
//! compares stable order, path IDs, classes, names, raw object hashes, parsed
//! payload hashes, Material property bits, Mesh vertex/normal/UV/index bits,
//! settings fields, and `TypeTree` dumps.
//!
//! It also runs the same comparison across every serialized format version from
//! 13 through 22, and through the containers a game ships: `UnityFS` v6 with
//! inline and tail blocks-info, `UnityFS` v7 with its mandatory alignment,
//! LZ4/LZ4HC/Zstd-compressed block data and blocks-info tables, legacy
//! `UnityRaw` v6, and a gzip stream.
//!
//! Run with:
//! `cargo test -p assetstudio-core --test dotnet_oracle -- --ignored`

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use assetstudio_core::studio::Studio;
use serde_json::{Value, json};

#[path = "support/containers.rs"]
mod containers;
#[path = "support/oracle_manifest.rs"]
mod oracle_manifest;

use containers::{BlocksInfo, BundleEntry, BundleLayout, Compression};
use oracle_manifest::rust_manifest;

type ContainerCase = (&'static str, Vec<u8>, &'static [&'static str]);

#[test]
#[ignore = "requires the .NET 10 SDK and a Team-Haruki/AssetStudio checkout"]
fn managed_and_rust_manifests_match_for_shared_fixture() {
    let executable = build_managed_oracle().unwrap();
    for fixture in &object_fixtures() {
        let managed = managed_manifest(&executable, fixture.input_path()).unwrap();
        let rust = rust_manifest(fixture.input_path(), 1024 * 1024).unwrap();
        assert_eq!(managed, rust, "fixture {}", fixture.path.display());
        assert_converted_shader(fixture, &managed);
        assert_animation_curves(fixture, &managed);
    }

    assert_version_matrix(&executable);
    assert_compressed_texture_formats(&executable);
    assert_crunched_textures(&executable);
    assert_switch_textures(&executable);
    assert_container_fixtures(&executable);
    assert_split_group_fixture(&executable);
    assert_truncated_fixture(&executable);
}

/// Compares a serialized file delivered as a Unity split group.
///
/// Each reader gets its own copy of the directory. The managed reader merges a
/// split group by writing the joined file back into the directory it found the
/// parts in, so a shared fixture would leave the second reader looking at both
/// the parts and the merged file and reporting the objects twice.
fn assert_split_group_fixture(executable: &Path) {
    // The mesh file is resident-only, so the split group needs no sibling .resS.
    let bytes = synthetic_single_v22(43, 43, "2022.3.62f1", &mesh());
    let managed_fixture = TemporaryFixture::with_split_parts("oracle-split.assets", &bytes, 3)
        .expect("the managed split fixture is writable");
    let rust_fixture = TemporaryFixture::with_split_parts("oracle-split.assets", &bytes, 3)
        .expect("the Rust split fixture is writable");

    let managed = managed_manifest(executable, managed_fixture.input_path()).unwrap();
    let rust = rust_manifest(rust_fixture.input_path(), 1024 * 1024).unwrap();
    assert_eq!(managed, rust, "split group fixture");
    assert_eq!(
        managed["Files"][0]["Path"], "oracle-split.assets",
        "the merged file should carry the name without the split suffix: {managed}"
    );
    assert!(
        managed["Files"][0]["Objects"]
            .as_array()
            .is_some_and(|objects| !objects.is_empty()),
        "the split group produced no objects: {managed}"
    );
}

/// One fixture per object-level reader the gate compares.
fn object_fixtures() -> Vec<TemporaryFixture> {
    let mut fixtures = mesh_fixtures();
    fixtures.extend(animation_fixtures());
    fixtures.extend(vec![
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
                &mesh_payload(Some((7, 120, "oracle-mesh.resS")), None, false),
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
            "oracle-sprite-tight-2022.assets",
            &synthetic_tight_sprite_v22(),
        )
        .unwrap(),
        TemporaryFixture::new(
            "oracle-shader-5.2.assets",
            &synthetic_single_v22(48, 48, "5.2.4f1", &shader()),
        )
        .unwrap(),
        TemporaryFixture::new(
            "oracle-shader-5.3.assets",
            &synthetic_single_v22(48, 48, "5.3.8f2", &subprogram_shader()),
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
        TemporaryFixture::with_resource(
            "oracle-v22.assets",
            &synthetic_v22(),
            "oracle.resS",
            &oracle_resource(),
        )
        .unwrap(),
    ]);
    fixtures
}

/// The `AnimationClip` fixtures, including one carrying real keyframes.
fn animation_fixtures() -> Vec<TemporaryFixture> {
    vec![
        TemporaryFixture::new(
            "oracle-animation-curves.assets",
            &synthetic_single_v22(74, 74, "6000.2.0f1", &animation_clip_with_curves(true)),
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
    ]
}

/// The Mesh fixtures, including both shapes of packed geometry.
fn mesh_fixtures() -> Vec<TemporaryFixture> {
    vec![
        TemporaryFixture::new(
            "oracle-mesh-compressed.assets",
            &synthetic_single_v22(43, 43, "2022.3.62f1", &compressed_mesh()),
        )
        .unwrap(),
        TemporaryFixture::new(
            "oracle-mesh-compressed-only.assets",
            &synthetic_single_v22(43, 43, "2022.3.62f1", &compressed_only_mesh()),
        )
        .unwrap(),
    ]
}

/// Confirms the curve fixture really carried keyframes.
///
/// Both readers report an empty hash when a clip has no curves, so a fixture
/// whose curve block failed to parse would still make them agree while proving
/// nothing about a single keyframe.
fn assert_animation_curves(fixture: &TemporaryFixture, manifest: &Value) {
    if !fixture.path.to_string_lossy().contains("animation-curves") {
        return;
    }
    let payload = &manifest["Files"][0]["Objects"][0]["Payload"];
    for row in [
        "RotationCurves",
        "EulerCurves",
        "PositionCurves",
        "ScaleCurves",
        "FloatCurves",
    ] {
        let count = payload[row]["Count"].as_i64().unwrap_or(0);
        assert!(
            count > 0,
            "{row} carried no values, so the comparison proved nothing: {payload}"
        );
    }
}

/// Confirms the 5.3 fixture really took the converted-text path.
///
/// Both readers reach the direct script when the subprogram blob is missing, so
/// a fixture that lost its blob would still make the two agree while proving
/// nothing about the conversion. The converted text is far longer than the
/// header plus the source script, which the direct path would produce exactly.
fn assert_converted_shader(fixture: &TemporaryFixture, manifest: &Value) {
    const HEADER_BYTES: i64 = 122;
    const SCRIPT_BYTES: i64 = 52;

    if !fixture.path.to_string_lossy().contains("shader-5.3") {
        return;
    }
    let size = manifest["Files"][0]["Objects"][0]["Payload"]["Data"]["Size"]
        .as_i64()
        .expect("the shader payload reports a size");
    assert!(
        size > HEADER_BYTES + SCRIPT_BYTES,
        "the 5.3 shader fixture produced {size} bytes, which is the direct \
         script rather than converted text"
    );
}

/// Compares Switch GOB-deswizzled textures.
///
/// A Switch payload is stored in the console's block-linear layout, so the
/// reader has to undo the GOB swizzle before decoding and then crop the padded
/// surface back to the declared size. That reordering was only ever checked
/// against this crate's own expectations. The platform blob carries the block
/// height exponent the layout depends on.
fn assert_switch_textures(executable: &Path) {
    const SWITCH_TARGET_PLATFORM: i32 = 38;
    const REVISION: &str = "2022.3.62f1";
    // (name, format code, bytes per texel block, block height exponent)
    // Uncompressed at two block heights plus a block-compressed format, which
    // is what the deswizzle itself needs: the texel size and the block-height
    // exponent are the only inputs to the GOB layout. DXT5 and ASTC are left
    // out for the same reasons they are absent from the block-format matrix --
    // the recorded s3tc divergence and reserved encodings random bytes reach --
    // neither of which says anything about the swizzle.
    const CASES: &[(&str, i32, usize, i32)] = &[
        ("rgba32", 4, 4, 0),
        ("rgba32-tall", 4, 4, 2),
        ("bc7", 25, 16, 1),
    ];
    // 64x64 keeps the padded surface a whole number of GOBs at every block
    // height these cases use, so nothing depends on the crop being lenient.
    const SIZE: i32 = 64;

    for (index, (name, format, texel_bytes, block_height_log2)) in CASES.iter().enumerate() {
        let blocks = if *texel_bytes == 4 {
            (SIZE * SIZE) as usize
        } else {
            ((SIZE / 4) * (SIZE / 4)) as usize
        };
        let payload = block_payload(blocks * texel_bytes, 0x5851_F42D_4C95_7F2D ^ index as u64);
        let mut blob = vec![0_u8; 12];
        blob[8..12].copy_from_slice(&block_height_log2.to_le_bytes());
        let object = texture2d_inline(
            &format!("oracle-switch-{name}"),
            SIZE,
            SIZE,
            *format,
            1,
            REVISION,
            &blob,
            &payload,
        );
        let file =
            synthetic_single_v22_on_platform(28, 28, REVISION, SWITCH_TARGET_PLATFORM, &object);
        let fixture = TemporaryFixture::new(&format!("oracle-switch-{name}.assets"), &file)
            .expect("the Switch fixture is writable");
        let managed = managed_manifest(executable, fixture.input_path()).unwrap();
        let rust = rust_manifest(fixture.input_path(), 8 * 1024 * 1024).unwrap();
        assert_eq!(managed, rust, "Switch texture {name}");
        assert_eq!(
            managed["Files"][0]["Objects"][0]["Payload"]["Decoded"]["Size"],
            i64::from(SIZE) * i64::from(SIZE) * 4,
            "Switch texture {name} did not decode a full surface: {managed}"
        );
    }
}

/// Compares decoded pixels for classic Crunch and `UnityCrunch` payloads.
///
/// The unit tests already check these against hashes taken from the bundled
/// C++ decoder, but only the decoder in isolation. This runs the whole path the
/// way a caller reaches it -- `Texture2D` parse, header sniff, transcode,
/// mip-zero decode -- against the managed reader doing the same, so the version
/// gate that chooses between the two Crunch dialects is compared too rather
/// than assumed.
fn assert_crunched_textures(executable: &Path) {
    // Real CRN payloads; see tests/fixtures/crunch/README.md for provenance.
    // A classic payload needs a pre-2017.3 revision and a UnityCrunch one needs
    // 2017.3 or newer, except for the ETC formats which are always UnityCrunch.
    const CASES: &[(&str, &str, i32, &str)] = &[
        ("classic-dxt1", "classic_dxt1.crn", 28, "2017.2.0f3"),
        ("classic-dxt5", "classic_dxt5.crn", 29, "2017.2.0f3"),
        ("unity-dxt1", "unity_dxt1.crn", 28, "2022.3.62f1"),
        ("unity-dxt5", "unity_dxt5.crn", 29, "2022.3.62f1"),
        ("unity-etc1", "unity_etc1.crn", 64, "2022.3.62f1"),
        ("unity-etc2a", "unity_etc2a.crn", 65, "2022.3.62f1"),
    ];
    // 512x512 RGBA is a megabyte decoded, so the manifest cap has to clear it.
    const MAXIMUM_BYTES: u64 = 8 * 1024 * 1024;

    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/crunch");
    for (name, file_name, format, revision) in CASES {
        let payload = fs::read(directory.join(file_name))
            .unwrap_or_else(|error| panic!("cannot read {file_name}: {error}"));
        let object = crunched_texture2d(&format!("oracle-{name}"), *format, revision, &payload);
        let file = synthetic_single_v22(28, 28, revision, &object);
        let fixture = TemporaryFixture::new(&format!("oracle-crunch-{name}.assets"), &file)
            .expect("the Crunch fixture is writable");
        let managed = managed_manifest(executable, fixture.input_path()).unwrap();
        let rust = rust_manifest(fixture.input_path(), MAXIMUM_BYTES).unwrap();
        assert_eq!(managed, rust, "Crunch fixture {name}");
        assert_eq!(
            managed["Files"][0]["Objects"][0]["Payload"]["Decoded"]["Size"],
            512 * 512 * 4,
            "Crunch fixture {name} did not decode a full 512x512 surface: {managed}"
        );
    }
}

/// A 512x512 `Texture2D` whose payload is a CRN stream.
///
/// Ten mip levels, which is what a 512-pixel chain has and what these CRN
/// streams carry; only level zero is decoded and compared.
fn crunched_texture2d(name: &str, format: i32, revision: &str, payload: &[u8]) -> Vec<u8> {
    texture2d_inline(name, 512, 512, format, 10, revision, &[], payload)
}

/// The major and minor components of a Unity revision like `2017.2.0f3`.
fn unity_minor_version(revision: &str) -> (u32, u32) {
    let mut parts = revision.split('.');
    let major = parts.next().unwrap_or("0").parse().unwrap_or(0);
    let minor = parts
        .next()
        .unwrap_or("0")
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .unwrap_or(0);
    (major, minor)
}

/// Compares decoded pixels across the block-compressed texture formats.
///
/// Only the stored payload was ever compared here, and that comes straight off
/// disk, so every block decoder was verified by this crate's own round trips
/// alone. The managed decoder is the original C++ implementation and the Rust
/// one is an independent port, which makes this a real cross-check rather than
/// two views of the same code.
///
/// Three formats are deliberately absent, each for a different reason.
///
/// `DXT1` and `DXT5` share the recorded s3tc divergence: the managed decoder's
/// colour palette reproduces NV4x-era hardware where this reader follows the
/// specification. `DXT1`'s punch-through mode is the visible half of it -- index
/// 3 under `q0 <= q1` gives transparent black here and opaque black there -- and
/// `DXT5` carries the same colour block, which is why its alpha half, `BC4`,
/// agrees while the whole format does not.
///
/// `BC6H` and `ASTC` are excluded because random bytes are the wrong input for
/// them, not because a divergence was chosen. Both formats reserve encodings
/// that no encoder emits, and the two implementations handle those differently:
/// `ASTC` decodes to an error colour in the managed decoder and fails outright
/// here. Settling those two needs fixtures from a real encoder rather than a
/// byte generator.
fn assert_compressed_texture_formats(executable: &Path) {
    // (name, format code, bytes per 4x4 block)
    const FORMATS: &[(&str, i32, usize)] = &[
        ("bc4", 26, 8),
        ("bc5", 27, 16),
        ("bc7", 25, 16),
        ("etc-rgb4", 34, 8),
        ("etc2-rgb", 45, 8),
        ("etc2-rgba1", 46, 8),
        ("etc2-rgba8", 47, 16),
        ("eac-r", 41, 8),
        ("eac-r-signed", 42, 8),
        ("eac-rg", 43, 16),
        ("eac-rg-signed", 44, 16),
    ];
    // Eight by eight is four blocks, so a fixture exercises block-to-block
    // placement as well as the decode of one block.
    const SIZE: i32 = 8;
    const BLOCKS: usize = 4;
    const REVISION: &str = "2022.3.62f1";

    let mut disagreed = Vec::new();
    for (index, (name, format, block_bytes)) in FORMATS.iter().enumerate() {
        let payload = block_payload(BLOCKS * block_bytes, 0x9E37_79B9_7F4A_7C15 ^ index as u64);
        let object = texture2d_inline(
            &format!("oracle-{name}"),
            SIZE,
            SIZE,
            *format,
            1,
            REVISION,
            &[],
            &payload,
        );
        let file = synthetic_single_v22(28, 28, REVISION, &object);
        let fixture = TemporaryFixture::new(&format!("oracle-texture-{name}.assets"), &file)
            .expect("the texture fixture is writable");
        let managed = managed_manifest(executable, fixture.input_path()).unwrap();
        let rust = rust_manifest(fixture.input_path(), 1024 * 1024).unwrap();
        let managed_decoded = &managed["Files"][0]["Objects"][0]["Payload"]["Decoded"];
        let rust_decoded = &rust["Files"][0]["Objects"][0]["Payload"]["Decoded"];
        if managed_decoded != rust_decoded {
            disagreed.push(format!(
                "{name} ({format}): managed {managed_decoded} vs Rust {rust_decoded}"
            ));
            continue;
        }
        assert_eq!(managed, rust, "texture format {name} ({format})");
        assert!(
            !managed_decoded.is_null(),
            "texture format {name} decoded to nothing on both sides, so the \
             comparison proved nothing: {managed}"
        );
    }
    assert!(
        disagreed.is_empty(),
        "decoded pixels disagree for {} format(s):\n{}",
        disagreed.len(),
        disagreed.join("\n")
    );
}

/// Compares one file per serialized format version, 5 through 21.
///
/// Formats 5 through 12 always carry a `TypeTree`, so they need real trees rather
/// than the tree-less shortcut 13 and later allow; those come from the
/// committed TPK-derived fixture. Both halves cover the same claim: every
/// version gate this reader implements is compared against the managed reader
/// rather than against the Rust writer's own assumptions.
fn assert_version_matrix(executable: &Path) {
    let trees = text_asset_type_trees();
    for version in 5..22 {
        let name = format!("oracle-format-v{version}.assets");
        let bytes = if version < 13 {
            tree_bearing_text_asset(version, &trees)
        } else {
            synthetic_versioned_text_asset(version)
        };
        let fixture = TemporaryFixture::new(&name, &bytes).unwrap();
        let managed = managed_manifest(executable, fixture.input_path()).unwrap();
        let rust = rust_manifest(fixture.input_path(), 1024 * 1024).unwrap();
        assert_eq!(managed, rust, "format version {version}");
        assert!(
            managed["Files"][0]["Objects"]
                .as_array()
                .is_some_and(|objects| objects.len() == 1),
            "format version {version} produced no object: {managed}"
        );
        // The name only resolves if the reader walked the tree correctly, so an
        // agreed empty string would hide a tree both readers mis-parse.
        assert_eq!(
            managed["Files"][0]["Objects"][0]["Name"], "oracle.txt",
            "format version {version} did not resolve the TextAsset name"
        );
    }
}

/// Runs the same comparison through the container stack.
///
/// Every container row in the compatibility matrix used to rest on Rust's own
/// round trips, because the gate only ever fed the managed oracle bare
/// `.assets` files. These wrap one in the containers a game actually ships, so
/// the header dispatch, blocks-info placement, directory table and entry
/// extraction are compared against the managed reader rather than against the
/// Rust writer's own assumptions.
fn assert_container_fixtures(executable: &Path) {
    const REVISION: &str = "2022.3.62f1";
    // Resident objects only: a bundled fixture has nowhere to put a sibling
    // `.resS`. Two entries so the directory table itself is compared, not just
    // the single-entry degenerate case.
    let mesh_file = synthetic_single_v22(43, 43, "2022.3.62f1", &mesh());
    let material_file = synthetic_single_v22(21, 21, "2022.3.62f1", &material());
    let inner = mesh_file.clone();
    let entries = [
        BundleEntry {
            path: "CAB-oracle-mesh",
            bytes: mesh_file.as_slice(),
        },
        BundleEntry {
            path: "CAB-oracle-material",
            bytes: material_file.as_slice(),
        },
    ];
    let uncompressed: [ContainerCase; 3] = [
        (
            "oracle-bundle-v6-inline.unity3d",
            containers::unity_fs(&BundleLayout::v6(REVISION), &entries),
            &[],
        ),
        (
            "oracle-bundle-v6-tail.unity3d",
            containers::unity_fs(
                &BundleLayout {
                    info: BlocksInfo::AtEnd,
                    ..BundleLayout::v6(REVISION)
                },
                &entries,
            ),
            &[],
        ),
        (
            "oracle-bundle-v7-aligned.unity3d",
            containers::unity_fs(
                &BundleLayout {
                    version: 7,
                    info: BlocksInfo::InlineAligned,
                    ..BundleLayout::v6(REVISION)
                },
                &entries,
            ),
            &[],
        ),
    ];
    let tail: [ContainerCase; 5] = [
        (
            "oracle-bundle-raw-v6.unity3d",
            containers::unity_raw_v6(REVISION, &entries),
            &[],
        ),
        ("oracle-gzip.assets.gz", containers::gzip(&inner), &[]),
        // The WebGL player's container, and the Tuanjie fork of it. Both were
        // implemented against the format description alone.
        (
            "oracle-webdata.unityweb",
            containers::unity_web_data("UnityWebData1.0", &entries),
            &[],
        ),
        (
            "oracle-webdata-tuanjie.unityweb",
            containers::unity_web_data("TuanjieWebData1.0", &entries),
            &[],
        ),
        ("oracle-archive.zip", containers::zip_archive(&entries), &[]),
    ];
    let cases = uncompressed
        .into_iter()
        .chain(lzma_bundle_cases(&entries))
        .chain(compressed_bundle_cases(&entries))
        .chain(tail);
    for (name, bytes, allowed) in cases {
        let fixture = TemporaryFixture::new(name, &bytes).unwrap();
        let managed = managed_manifest_allowing(executable, fixture.input_path(), allowed).unwrap();
        let rust = rust_manifest(fixture.input_path(), 1024 * 1024).unwrap();
        assert_eq!(managed, rust, "container fixture {name}");
        assert!(
            managed["Files"]
                .as_array()
                .is_some_and(|files| !files.is_empty()),
            "container fixture {name} produced no serialized files: {managed}"
        );
    }
}

/// LZMA-compressed bundle cases.
///
/// Unity frames an LZMA block as the five-byte property header followed by the
/// raw stream, without the `.lzma` container's size field, so the framing
/// itself is worth comparing and not only the decoded bytes.
fn lzma_bundle_cases(entries: &[BundleEntry<'_>]) -> [ContainerCase; 3] {
    const REVISION: &str = "2022.3.62f1";

    [
        (
            "oracle-bundle-lzma-blocks.unity3d",
            containers::unity_fs(
                &BundleLayout {
                    blocks: Compression::Lzma,
                    ..BundleLayout::v6(REVISION)
                },
                entries,
            ),
            &[],
        ),
        (
            "oracle-bundle-lzma-directory.unity3d",
            containers::unity_fs(
                &BundleLayout {
                    directory: Compression::Lzma,
                    ..BundleLayout::v6(REVISION)
                },
                entries,
            ),
            &[],
        ),
        (
            "oracle-bundle-lzma-both-tail.unity3d",
            containers::unity_fs(
                &BundleLayout {
                    info: BlocksInfo::AtEnd,
                    blocks: Compression::Lzma,
                    directory: Compression::Lzma,
                    ..BundleLayout::v6(REVISION)
                },
                entries,
            ),
            &[],
        ),
    ]
}

fn compressed_bundle_cases(entries: &[BundleEntry<'_>]) -> [ContainerCase; 7] {
    const REVISION: &str = "2022.3.62f1";
    // The managed reader calls Zstd "non-standard" and then decodes it anyway.
    // Listing the exact wording keeps the harness strict about every other
    // diagnostic while documenting this one.
    const ZSTD_BLOCK_WARNING: &str = "Non-standard block compression type: 5";
    const ZSTD_INFO_WARNING: &str = "Non-standard blockInfo compression type: 5";

    [
        (
            "oracle-bundle-lz4-blocks.unity3d",
            containers::unity_fs(
                &BundleLayout {
                    blocks: Compression::Lz4,
                    ..BundleLayout::v6(REVISION)
                },
                entries,
            ),
            &[],
        ),
        (
            "oracle-bundle-lz4-directory.unity3d",
            containers::unity_fs(
                &BundleLayout {
                    directory: Compression::Lz4,
                    ..BundleLayout::v6(REVISION)
                },
                entries,
            ),
            &[],
        ),
        (
            "oracle-bundle-lz4hc-blocks.unity3d",
            containers::unity_fs(
                &BundleLayout {
                    blocks: Compression::Lz4Hc,
                    ..BundleLayout::v6(REVISION)
                },
                entries,
            ),
            &[],
        ),
        (
            "oracle-bundle-lz4hc-both-v7.unity3d",
            containers::unity_fs(
                &BundleLayout {
                    version: 7,
                    info: BlocksInfo::AtEnd,
                    blocks: Compression::Lz4Hc,
                    directory: Compression::Lz4Hc,
                    ..BundleLayout::v6(REVISION)
                },
                entries,
            ),
            &[],
        ),
        // Compressed blocks and a compressed directory: the decompression path
        // and the block mapping that depends on compressed-versus-uncompressed
        // sizes differing.
        (
            "oracle-bundle-zstd-blocks.unity3d",
            containers::unity_fs(
                &BundleLayout {
                    blocks: Compression::Zstd,
                    ..BundleLayout::v6(REVISION)
                },
                entries,
            ),
            &[ZSTD_BLOCK_WARNING],
        ),
        (
            "oracle-bundle-zstd-directory.unity3d",
            containers::unity_fs(
                &BundleLayout {
                    directory: Compression::Zstd,
                    ..BundleLayout::v6(REVISION)
                },
                entries,
            ),
            &[ZSTD_INFO_WARNING],
        ),
        (
            "oracle-bundle-zstd-both-v7.unity3d",
            containers::unity_fs(
                &BundleLayout {
                    version: 7,
                    info: BlocksInfo::AtEnd,
                    blocks: Compression::Zstd,
                    directory: Compression::Zstd,
                    ..BundleLayout::v6(REVISION)
                },
                entries,
            ),
            &[ZSTD_BLOCK_WARNING, ZSTD_INFO_WARNING],
        ),
    ]
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

/// Environment variable pointing at a `Team-Haruki/AssetStudio` checkout.
///
/// The managed reader lives in a separate repository and is the compatibility
/// oracle only, so it is not vendored here. Without this variable the project
/// falls back to a sibling directory of this repository.
const ASSETSTUDIO_REPO_ENV: &str = "ASSETSTUDIO_REPO";

fn build_managed_oracle() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let project =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../oracle/AssetStudioOracle.csproj");
    let mut arguments = vec![
        "build".to_owned(),
        project
            .to_str()
            .ok_or("oracle project path is not UTF-8")?
            .to_owned(),
        "--configuration".to_owned(),
        "Release".to_owned(),
        "--framework".to_owned(),
        "net10.0".to_owned(),
        "--no-restore".to_owned(),
        "--nologo".to_owned(),
        "--verbosity".to_owned(),
        "quiet".to_owned(),
        "-p:NuGetAudit=false".to_owned(),
    ];
    if let Some(repository) = std::env::var_os(ASSETSTUDIO_REPO_ENV) {
        let repository = repository
            .to_str()
            .ok_or("ASSETSTUDIO_REPO is not UTF-8")?
            .to_owned();
        arguments.push(format!("-p:AssetStudioRepo={repository}"));
    }
    let build = Command::new("dotnet").args(&arguments).output()?;
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
    managed_manifest_allowing(executable, path, &[])
}

/// Runs the managed oracle, permitting only the diagnostics named in `allowed`.
///
/// Any managed diagnostic is a failure by default, because a warning usually
/// means the managed reader fell back to something the Rust side does not know
/// about. A few inputs legitimately make it log and continue -- it calls Zstd a
/// "non-standard" block compression even though it decodes it -- so those are
/// listed per fixture rather than ignored wholesale.
fn managed_manifest_allowing(
    executable: &Path,
    path: &Path,
    allowed: &[&str],
) -> Result<Value, Box<dyn std::error::Error>> {
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
    let diagnostics = String::from_utf8_lossy(&output.stderr);
    let unexpected: Vec<&str> = diagnostics
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter(|line| !allowed.iter().any(|permitted| line.contains(permitted)))
        .collect();
    if !unexpected.is_empty() {
        return Err(format!(
            "managed oracle emitted unexpected diagnostics:\n{}",
            unexpected.join("\n")
        )
        .into());
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

struct TemporaryFixture {
    directory: PathBuf,
    path: PathBuf,
    resource_path: Option<PathBuf>,
    extra_paths: Vec<PathBuf>,
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
            extra_paths: Vec::new(),
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

    /// Writes `bytes` as `name.split0`, `name.split1`, ... in `chunks` pieces.
    ///
    /// A Unity split group is a plain byte-wise cut, so a reader has to
    /// concatenate the parts in index order before parsing. Splitting at an
    /// arbitrary offset -- not on any structure boundary -- is the point: a
    /// reader that parsed the parts individually would fail outright.
    fn with_split_parts(name: &str, bytes: &[u8], chunks: usize) -> std::io::Result<Self> {
        assert!(chunks > 1, "a split group needs at least two parts");
        let part_length = bytes.len().div_ceil(chunks);
        let mut fixture = Self::new(&format!("{name}.split0"), &bytes[..part_length])?;
        for (index, part) in bytes[part_length..].chunks(part_length).enumerate() {
            let path = fixture.directory.join(format!("{name}.split{}", index + 1));
            fs::write(&path, part)?;
            fixture.extra_paths.push(path);
        }
        Ok(fixture)
    }

    fn input_path(&self) -> &Path {
        &self.directory
    }
}

impl Drop for TemporaryFixture {
    fn drop(&mut self) {
        // Whole-directory removal rather than per-file: the managed reader
        // writes the merged file into a split fixture's directory itself, so
        // the harness does not know every name it has to clean up.
        let _ = fs::remove_dir_all(&self.directory);
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
    // 13 is the standalone Windows player.
    synthetic_single_v22_on_platform(class_id, path_id, version, 13, data)
}

/// The same file with a caller-chosen build target, which some readers branch
/// on: Switch textures are GOB-swizzled and Xbox 360 payloads are byte-swapped.
fn synthetic_single_v22_on_platform(
    class_id: i32,
    path_id: i64,
    version: &str,
    target_platform: i32,
    data: &[u8],
) -> Vec<u8> {
    let mut metadata = Vec::new();
    metadata.extend_from_slice(version.as_bytes());
    metadata.push(0);
    push_i32(&mut metadata, target_platform);
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

/// A tight-packed sprite whose mesh masks part of its texture rect.
///
/// The rectangle sprite above never reaches the masking path: a tight sprite
/// rasterizes its own triangles and clears every texel outside them, which is
/// where an off-by-one in the raster or a wrong winding shows up.
fn synthetic_tight_sprite_v22() -> Vec<u8> {
    synthetic_plain_v22(
        "2022.3.62f1",
        &[
            (213, 213, tight_sprite()),
            (28, 214, tight_sprite_texture()),
        ],
    )
}

/// An 8x8 RGBA32 texture whose pixels all differ, so a mis-masked texel shows.
fn tight_sprite_texture() -> Vec<u8> {
    let mut pixels = Vec::new();
    for y in 0..8_u8 {
        for x in 0..8_u8 {
            pixels.extend_from_slice(&[x * 8 + 4, y * 8 + 4, 128, 255]);
        }
    }
    texture2d_inline(
        "oracle-tight-sprite-texture",
        8,
        8,
        4,
        1,
        "2022.3.62f1",
        &[],
        &pixels,
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

/// The real `TextAsset` type trees the pre-13 fixtures embed.
///
/// Serialized formats below 13 always carry a tree; the flag that turns it off
/// arrives at 13. A fixture for those formats therefore has to embed one, and
/// inventing a shape would make the differential compare two readers against
/// something Unity never wrote. These come from
/// `tools/generate_typetree_fixtures.py`, which extracts them from `UnityPy`'s
/// bundled TPK; see that script for the derivation chain.
fn text_asset_type_trees() -> Value {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/typetree/text_asset.json");
    let bytes =
        fs::read(&path).unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
    serde_json::from_slice(&bytes).expect("the type tree fixture is valid JSON")
}

/// Builds a little-endian `TextAsset` file at a format that carries a tree.
///
/// Formats 5 through 12 differ from each other in more than the tree: the
/// header moves to the end of the file below 9, the Unity version string only
/// appears from 7, the target platform from 8, the per-object destroyed field
/// disappears at 11 while the script type index appears, and the tree itself
/// switches from the recursive encoding to the blob at 10 and again from 12.
/// Every one of those gates was previously unexercised by the differential.
fn tree_bearing_text_asset(version: u32, trees: &Value) -> Vec<u8> {
    assert!(
        (5..13).contains(&version),
        "tree-bearing fixtures cover formats 5 through 12"
    );
    let tree = &trees["trees"][version.to_string()];
    let nodes = tree["nodes"].as_array().expect("the tree has nodes");
    let object = text_asset();

    let mut metadata = Vec::new();
    if version >= 7 {
        let unity_version = tree["unity_version"].as_str().expect("a Unity version");
        metadata.extend_from_slice(unity_version.as_bytes());
        metadata.push(0);
    }
    if version >= 8 {
        // 13 is the standalone Windows player, the same target the tree-less
        // fixtures declare.
        push_i32(&mut metadata, 13);
    }

    // One type record: the class ID, then the tree, with no stripped flag,
    // script type index or hashes before format 13.
    push_i32(&mut metadata, 1);
    push_i32(&mut metadata, 49);
    if version >= 12 || version == 10 {
        push_blob_type_tree(&mut metadata, nodes);
    } else {
        push_recursive_type_tree(&mut metadata, nodes);
    }

    if (7..14).contains(&version) {
        push_i32(&mut metadata, 0); // big ID disabled
    }

    push_i32(&mut metadata, 1);
    push_i32(&mut metadata, 0x1020_3040);
    push_u32(&mut metadata, 0);
    push_u32(&mut metadata, u32::try_from(object.len()).unwrap());
    push_i32(&mut metadata, 49); // type ID, matched to the record by class ID
    metadata.extend_from_slice(&49_u16.to_le_bytes());
    if version < 11 {
        metadata.extend_from_slice(&0_u16.to_le_bytes()); // not destroyed
    } else {
        metadata.extend_from_slice(&(-1_i16).to_le_bytes()); // no script type
    }

    if version >= 11 {
        push_i32(&mut metadata, 0); // script types
    }
    push_i32(&mut metadata, 0); // externals
    metadata.push(0); // user information

    finish_tree_bearing_header(version, &metadata, &object)
}

/// Reads one integer field of a type-tree node from the committed fixture.
fn node_field(node: &Value, field: &str) -> i32 {
    let value = node[field]
        .as_i64()
        .unwrap_or_else(|| panic!("type tree node field {field} is not an integer: {node}"));
    i32::try_from(value)
        .unwrap_or_else(|_| panic!("type tree node field {field} does not fit in i32: {value}"))
}

/// Writes the pre-blob tree encoding: one variable-length record per node.
fn push_recursive_type_tree(output: &mut Vec<u8>, nodes: &[Value]) {
    for node in nodes {
        output.extend_from_slice(node["type"].as_str().unwrap().as_bytes());
        output.push(0);
        output.extend_from_slice(node["name"].as_str().unwrap().as_bytes());
        output.push(0);
        push_i32(output, node_field(node, "byte_size"));
        push_i32(output, node_field(node, "index"));
        push_i32(output, node_field(node, "is_array"));
        push_i32(output, node_field(node, "version"));
        push_i32(output, node_field(node, "meta_flags"));
        push_i32(output, node_field(node, "children"));
    }
}

/// Writes the flat tree encoding: fixed 24-byte records plus a string buffer.
///
/// Names are written into the buffer rather than taken from the common-string
/// table, so the fixture exercises the offset path instead of the shortcut.
fn push_blob_type_tree(output: &mut Vec<u8>, nodes: &[Value]) {
    let mut buffer = Vec::new();
    let mut offsets: Vec<(u32, u32)> = Vec::new();
    let intern = |value: &str, buffer: &mut Vec<u8>| -> u32 {
        let offset = u32::try_from(buffer.len()).unwrap();
        buffer.extend_from_slice(value.as_bytes());
        buffer.push(0);
        offset
    };
    for node in nodes {
        let type_offset = intern(node["type"].as_str().unwrap(), &mut buffer);
        let name_offset = intern(node["name"].as_str().unwrap(), &mut buffer);
        offsets.push((type_offset, name_offset));
    }

    push_i32(output, i32::try_from(nodes.len()).unwrap());
    push_i32(output, i32::try_from(buffer.len()).unwrap());
    for (node, (type_offset, name_offset)) in nodes.iter().zip(&offsets) {
        let node_version = u16::try_from(node_field(node, "version")).unwrap();
        output.extend_from_slice(&node_version.to_le_bytes());
        output.push(u8::try_from(node_field(node, "level")).unwrap());
        output.push(u8::try_from(node_field(node, "is_array")).unwrap());
        push_u32(output, *type_offset);
        push_u32(output, *name_offset);
        push_i32(output, node_field(node, "byte_size"));
        push_i32(output, node_field(node, "index"));
        push_i32(output, node_field(node, "meta_flags"));
    }
    output.extend_from_slice(&buffer);
}

/// Assembles the file around the metadata.
///
/// From format 9 the header leads the file and carries the endianness byte.
/// Before that the header is 16 bytes, and a reader seeks to
/// `file_size - metadata_size` to find the endianness byte followed by the
/// metadata, so the metadata sits at the end and the object data comes first.
fn finish_tree_bearing_header(version: u32, metadata: &[u8], data: &[u8]) -> Vec<u8> {
    if version >= 9 {
        return finish_legacy_header(version, metadata, data, 0);
    }
    let metadata_size = 1 + metadata.len();
    let data_offset = 16;
    let file_size = data_offset + data.len() + metadata_size;
    let mut output = vec![0_u8; 16];
    output[0..4].copy_from_slice(&u32::try_from(metadata_size).unwrap().to_be_bytes());
    output[4..8].copy_from_slice(&u32::try_from(file_size).unwrap().to_be_bytes());
    output[8..12].copy_from_slice(&version.to_be_bytes());
    output[12..16].copy_from_slice(&u32::try_from(data_offset).unwrap().to_be_bytes());
    output.extend_from_slice(data);
    output.push(0); // little-endian
    output.extend_from_slice(metadata);
    output
}

/// Builds a little-endian `TextAsset` file at one tree-less format version.
///
/// The gate compared only versions 13 and 22, so every other version gate --
/// aligned 64-bit path IDs at 14, the per-object stripped byte at 15 and 16,
/// the type record's stripped flag at 16, the script type index moving into the
/// type record at 17, and reference types at 20 -- rested on the Rust writer's
/// own assumptions. Type trees are disabled, which the format allows from 13
/// on, so these stay minimal. [`tree_bearing_text_asset`] covers 5 through 12,
/// where a tree is mandatory.
fn synthetic_versioned_text_asset(version: u32) -> Vec<u8> {
    assert!(
        (13..22).contains(&version),
        "tree-less fixtures cover formats 13 through 21"
    );
    let object = text_asset();
    let mut metadata = Vec::new();
    metadata.extend_from_slice(b"2019.4.40f1\0");
    push_i32(&mut metadata, 13);
    metadata.push(0);

    push_i32(&mut metadata, 1);
    push_i32(&mut metadata, 49);
    if version >= 16 {
        metadata.push(0);
    }
    if version >= 17 {
        metadata.extend_from_slice(&(-1_i16).to_le_bytes());
    }
    metadata.extend_from_slice(&[0x5a; 16]);

    if (7..14).contains(&version) {
        push_i32(&mut metadata, 0);
    }
    push_i32(&mut metadata, 1);
    if version < 14 {
        push_i32(&mut metadata, 0x1020_3040);
    } else {
        align(&mut metadata, 4);
        metadata.extend_from_slice(&0x0102_0304_0506_0708_i64.to_le_bytes());
    }
    push_u32(&mut metadata, 0);
    push_u32(&mut metadata, u32::try_from(object.len()).unwrap());
    push_i32(&mut metadata, if version < 16 { 49 } else { 0 });
    if version < 16 {
        metadata.extend_from_slice(&49_u16.to_le_bytes());
    }
    if version < 11 {
        metadata.extend_from_slice(&0_u16.to_le_bytes());
    }
    if (11..17).contains(&version) {
        metadata.extend_from_slice(&(-1_i16).to_le_bytes());
    }
    if matches!(version, 15 | 16) {
        metadata.push(0);
    }

    if version >= 11 {
        push_i32(&mut metadata, 0);
    }
    push_i32(&mut metadata, 0);
    if version >= 20 {
        push_i32(&mut metadata, 0);
    }
    metadata.push(0);
    finish_legacy_header(version, &metadata, &object, 0)
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

/// The modern (5.6 and up) tight sprite layout: submeshes, a vertex stream and
/// a `u16` index buffer, with `packingMode` left at Tight.
fn tight_sprite() -> Vec<u8> {
    let mut output = Vec::new();
    push_string(&mut output, "oracle-tight-sprite");
    push_floats(&mut output, &[0.0, 0.0, 8.0, 8.0]);
    push_floats(&mut output, &[0.0, 0.0]);
    push_floats(&mut output, &[0.0; 4]);
    push_floats(&mut output, &[1.0, 0.5, 0.5]);
    push_u32(&mut output, 0);
    output.push(0);
    align(&mut output, 4);
    output.extend_from_slice(&[0; 16]);
    output.extend_from_slice(&0_i64.to_le_bytes());
    push_i32(&mut output, 0);
    push_pptr(&mut output); // no atlas

    push_i32(&mut output, 0);
    output.extend_from_slice(&214_i64.to_le_bytes()); // texture
    push_pptr(&mut output); // no alpha texture
    push_i32(&mut output, 0); // secondary textures

    push_i32(&mut output, 1); // one submesh
    push_u32(&mut output, 0); // first byte
    push_u32(&mut output, 3); // index count
    push_i32(&mut output, 0); // topology
    push_u32(&mut output, 0); // base vertex
    push_u32(&mut output, 0); // first vertex
    push_u32(&mut output, 3); // vertex count
    push_floats(&mut output, &[0.0; 6]); // local AABB: centre and extent

    push_i32(&mut output, 6); // index buffer
    for index in [0_u16, 1, 2] {
        output.extend_from_slice(&index.to_le_bytes());
    }
    align(&mut output, 4);

    push_u32(&mut output, 3); // vertex count
    push_i32(&mut output, 1); // one channel
    output.extend_from_slice(&[0, 0, 0, 3]); // stream 0, offset 0, float3
    push_i32(&mut output, 36); // vertex data
    // A right triangle covering the lower-left half. The hypotenuse runs
    // x + y = 7.5 in texture pixels, half a pixel clear of every centre, so no
    // texel sits on the boundary and a disagreement means a real difference
    // rather than an edge rule.
    push_floats(
        &mut output,
        &[-4.0, -4.0, 0.0, 3.5, -4.0, 0.0, -4.0, 3.5, 0.0],
    );
    align(&mut output, 4);
    push_i32(&mut output, 0); // no bind pose

    push_floats(&mut output, &[0.0, 0.0, 8.0, 8.0]); // texture rect
    push_floats(&mut output, &[0.0, 0.0, 0.0, 0.0]); // rect and atlas offsets
    push_u32(&mut output, 0); // settings: unpacked, Tight
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

/// A Unity 5.3 Shader carrying an LZ4 subprogram blob.
///
/// The oracle used to refuse anything but the legacy direct script, so both
/// converted-text paths -- the 5.3-5.4 subprogram blob and the 5.5+ serialized
/// shader -- were compared against nothing. This covers the first: the blob is
/// decompressed, its program table walked, and the resulting text hashed by
/// both readers.
fn subprogram_shader() -> Vec<u8> {
    let record = shader_sub_program_record(201_509_030, 1, &["DIRECTIONAL"], b"void main() {}");
    let segment = shader_program_segment(&[(12, record.len())], &[&record]);
    let compressed = lz4_flex::block::compress(&segment);

    let mut output = Vec::new();
    push_string(&mut output, "oracle-subprogram-shader");
    let script = b"Shader \"Oracle/SubProgram\" { SubShader { Pass { } } }";
    push_i32(&mut output, i32::try_from(script.len()).unwrap());
    output.extend_from_slice(script);
    align(&mut output, 4);
    push_string(&mut output, "Oracle/SubProgram.shader");
    push_u32(&mut output, u32::try_from(segment.len()).unwrap());
    push_i32(&mut output, i32::try_from(compressed.len()).unwrap());
    output.extend_from_slice(&compressed);
    output
}

/// The blob's offset/length table followed by the records it points at.
fn shader_program_segment(entries: &[(usize, usize)], records: &[&[u8]]) -> Vec<u8> {
    let mut output = Vec::new();
    push_i32(&mut output, i32::try_from(entries.len()).unwrap());
    for (offset, length) in entries {
        push_i32(&mut output, i32::try_from(*offset).unwrap());
        push_i32(&mut output, i32::try_from(*length).unwrap());
    }
    for record in records {
        output.extend_from_slice(record);
    }
    output
}

/// One sub-program: version, program type, reserved counters, keywords, code.
fn shader_sub_program_record(
    version: i32,
    program_type: i32,
    keywords: &[&str],
    code: &[u8],
) -> Vec<u8> {
    let mut output = Vec::new();
    push_i32(&mut output, version);
    push_i32(&mut output, program_type);
    output.extend_from_slice(&[0; 12]);
    if version >= 201_608_170 {
        output.extend_from_slice(&[0; 4]);
    }
    push_i32(&mut output, i32::try_from(keywords.len()).unwrap());
    for keyword in keywords {
        push_string(&mut output, keyword);
    }
    push_i32(&mut output, i32::try_from(code.len()).unwrap());
    output.extend_from_slice(code);
    align(&mut output, 4);
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
    animation_clip_with_curves(false)
}

/// The clip body, optionally carrying one curve of each explicit kind.
///
/// The differential compared curve counts and never a keyframe, so the times,
/// values and tangents each reader produced were checked only against its own
/// expectations.
fn animation_clip_with_curves(curves: bool) -> Vec<u8> {
    let mut output = Vec::new();
    push_string(&mut output, "oracle-animation");
    output.extend_from_slice(&[0, 0, 0]);
    align(&mut output, 4);
    if curves {
        // Rotation, then an empty compressed list, then Euler, position, scale,
        // float and an empty PPtr list.
        push_i32(&mut output, 1);
        push_quaternion_curve(&mut output, "Root/Bone");
        push_i32(&mut output, 0);
        push_i32(&mut output, 1);
        push_vector3_curve(&mut output, "Root/Bone", 1.0);
        push_i32(&mut output, 1);
        push_vector3_curve(&mut output, "Root", 2.0);
        push_i32(&mut output, 1);
        push_vector3_curve(&mut output, "Root/Bone/Tip", 3.0);
        push_i32(&mut output, 1);
        push_float_curve(&mut output, "Root", "m_LocalScale.x");
        push_i32(&mut output, 0);
    } else {
        for _ in 0..7 {
            push_i32(&mut output, 0);
        }
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

/// A two-keyframe quaternion curve. Values are chosen so every component
/// differs, which makes a swapped axis or tangent visible in the hash.
fn push_quaternion_curve(output: &mut Vec<u8>, path: &str) {
    push_i32(output, 2);
    for (base, time) in [(0.0_f32, 0.0_f32), (1.0, 0.5)] {
        push_f32(output, time);
        push_floats(output, &[base, base + 0.25, base + 0.5, 1.0]);
        push_floats(output, &[0.125, 0.25, 0.375, 0.5]);
        push_floats(output, &[0.625, 0.75, 0.875, 1.0]);
        push_i32(output, 0); // weighted mode
        push_floats(output, &[0.33, 0.33, 0.33, 0.33]);
        push_floats(output, &[0.66, 0.66, 0.66, 0.66]);
    }
    push_i32(output, 0); // pre-infinity
    push_i32(output, 0); // post-infinity
    push_i32(output, 4); // rotation order
    push_string(output, path);
}

/// A two-keyframe vector curve, offset by `bias` so each curve differs.
fn push_vector3_curve(output: &mut Vec<u8>, path: &str, bias: f32) {
    push_i32(output, 2);
    for (step, time) in [(0.0_f32, 0.0_f32), (1.0, 1.0)] {
        let base = bias + step;
        push_f32(output, time);
        push_floats(output, &[base, base + 0.5, base + 1.5]);
        push_floats(output, &[0.1, 0.2, 0.3]);
        push_floats(output, &[0.4, 0.5, 0.6]);
        push_i32(output, 0);
        push_floats(output, &[0.33, 0.33, 0.33]);
        push_floats(output, &[0.66, 0.66, 0.66]);
    }
    push_i32(output, 0);
    push_i32(output, 0);
    push_i32(output, 4);
    push_string(output, path);
}

/// A two-keyframe scalar curve plus the binding fields that follow it.
fn push_float_curve(output: &mut Vec<u8>, path: &str, attribute: &str) {
    push_i32(output, 2);
    for (step, time) in [(0.0_f32, 0.0_f32), (1.0, 0.25)] {
        push_f32(output, time);
        push_f32(output, 1.5 + step);
        push_f32(output, 0.75);
        push_f32(output, 0.875);
        push_i32(output, 0);
        push_f32(output, 0.33);
        push_f32(output, 0.66);
    }
    push_i32(output, 0);
    push_i32(output, 0);
    push_i32(output, 4);
    push_string(output, attribute);
    push_string(output, path);
    push_i32(output, 4); // Transform
    push_pptr(output);
    push_i32(output, 0); // flags, 2022.2 and up
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
    mesh_payload(None, None, false)
}

/// A mesh whose geometry lives in the packed `CompressedMesh` vectors.
///
/// Unity writes this form whenever mesh compression is on in the import
/// settings. The decode was implemented against the managed reader's source and
/// verified only by this crate's own expectations, which is the pattern that
/// produced every silent defect found so far.
fn compressed_mesh() -> Vec<u8> {
    mesh_payload(None, None, true)
}

/// The shape Unity actually writes: packed geometry and an empty vertex stream.
///
/// The fixture above keeps both sources populated, which exercises the overlay
/// rule; this one is the realistic file, where the vertex data contributes
/// nothing at all.
fn compressed_only_mesh() -> Vec<u8> {
    let mut output = mesh_payload(None, None, true);
    // Blank the vertex-data vertex count in place: it is the u32 that precedes
    // the channel table, and the managed reader skips the whole block when it
    // is zero.
    let marker = VERTEX_DATA_MARKER;
    let position = output
        .windows(marker.len())
        .position(|window| window == marker)
        .expect("the fixture carries the vertex-data header");
    output[position..position + 4].copy_from_slice(&0_u32.to_le_bytes());
    output
}

/// The vertex count and channel count that open the fixture's vertex data.
const VERTEX_DATA_MARKER: [u8; 8] = [3, 0, 0, 0, 5, 0, 0, 0];

#[allow(clippy::too_many_lines)]
fn tuanjie_mesh() -> Vec<u8> {
    mesh_payload(None, Some(3), false)
}

#[allow(clippy::too_many_lines)]
fn mesh_payload(
    stream: Option<(u64, u32, &str)>,
    tuanjie_revision: Option<u8>,
    compressed: bool,
) -> Vec<u8> {
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

    output.extend_from_slice(&[u8::from(compressed), 1, 0, 0]);
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

    if compressed {
        // A range of 255 at eight bits makes every packed value decode to
        // itself, so the expected geometry is readable in the fixture.
        push_packed_float_data(&mut output, 9, 255.0, 0.0, &[1, 0, 0, 0, 2, 0, 0, 0, 3], 8);
        push_empty_packed_float(&mut output); // UVs
        push_empty_packed_float(&mut output); // normals
        push_empty_packed_float(&mut output); // tangents
        push_empty_packed_int(&mut output); // weights
        push_empty_packed_int(&mut output); // normal signs
        push_empty_packed_int(&mut output); // tangent signs
        push_empty_packed_float(&mut output); // float colours
        push_empty_packed_int(&mut output); // bone indices
        push_packed_int_data(&mut output, &[0, 1, 2], 8);
        push_u32(&mut output, 0); // UV info
    } else {
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
    }

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

/// Packs `values` little-endian at `bit_size` bits each.
fn pack_bits(values: &[u32], bit_size: u8) -> Vec<u8> {
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

/// Builds a `Texture2D` with inline pixel data at a caller-chosen format.
///
/// Inline rather than streamed so one fixture file is self-contained, which
/// lets the differential carry a texture per compressed format without a
/// sibling `.resS` for each.
#[allow(clippy::too_many_arguments)]
fn texture2d_inline(
    name: &str,
    width: i32,
    height: i32,
    format: i32,
    mip_count: i32,
    revision: &str,
    platform_blob: &[u8],
    data: &[u8],
) -> Vec<u8> {
    let mut output = Vec::new();
    push_string(&mut output, name);
    // The fallback-format block arrives at 2017.3, and its alpha-channel byte
    // at 2020.2. Before that the name is followed straight by the dimensions.
    let version = unity_minor_version(revision);
    if version >= (2017, 3) {
        push_i32(&mut output, 0);
        output.push(0);
        if version >= (2020, 2) {
            output.push(0);
        }
        align(&mut output, 4);
    }
    push_i32(&mut output, width);
    push_i32(&mut output, height);
    output.extend_from_slice(&u32::try_from(data.len()).unwrap().to_le_bytes());
    // The stripped-mip count arrives in 2020.
    if version.0 >= 2020 {
        push_i32(&mut output, 0);
    }
    push_i32(&mut output, format);
    push_i32(&mut output, mip_count);

    // Import settings. The readable flag is the only one every supported
    // revision has; preprocessing arrives in 2020, the mip limit in 2019.3 and
    // its group name in 2022.2, and the streaming-mipmap pair in 2018.2.
    output.push(1);
    if version.0 >= 2020 {
        output.push(0);
    }
    if version >= (2019, 3) {
        output.push(0);
        if version >= (2022, 2) {
            align(&mut output, 4);
        }
    }
    if version >= (2022, 2) {
        push_string(&mut output, "");
    }
    if version >= (2018, 2) {
        output.push(0);
    }
    align(&mut output, 4);
    if version >= (2018, 2) {
        push_i32(&mut output, 0);
    }

    push_i32(&mut output, 1); // image count
    push_i32(&mut output, 2); // dimension
    output.extend_from_slice(&[0; 24]); // GL texture settings
    push_i32(&mut output, 0); // lightmap format
    push_i32(&mut output, 0); // colour space
    if version >= (2020, 2) {
        push_i32(&mut output, i32::try_from(platform_blob.len()).unwrap());
        output.extend_from_slice(platform_blob);
        align(&mut output, 4);
    }
    push_i32(&mut output, i32::try_from(data.len()).unwrap());
    output.extend_from_slice(data);
    // The stream offset widens to 64 bits in 2020.
    if version.0 >= 2020 {
        output.extend_from_slice(&0_i64.to_le_bytes());
    } else {
        output.extend_from_slice(&0_u32.to_le_bytes());
    }
    output.extend_from_slice(&0_u32.to_le_bytes());
    push_string(&mut output, "");
    output
}

/// Deterministic pseudo-random bytes for a block-compressed payload.
///
/// Any byte pattern is a valid block to a decoder, so arbitrary content is the
/// point: it walks the interpolation modes, partition tables and endpoint
/// encodings a hand-written block would never reach, and both implementations
/// have to agree on whatever it means. The generator is a plain LCG so the
/// fixture is identical on every run and platform.
fn block_payload(byte_count: usize, seed: u64) -> Vec<u8> {
    let mut state = seed;
    let mut output = Vec::with_capacity(byte_count);
    for _ in 0..byte_count {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        #[allow(clippy::cast_possible_truncation)]
        output.push((state >> 33) as u8);
    }
    output
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
    finish_legacy_header(13, metadata, data, endianness)
}

/// Wraps metadata in the 20-byte header every format below 22 uses.
fn finish_legacy_header(version: u32, metadata: &[u8], data: &[u8], endianness: u8) -> Vec<u8> {
    let data_offset = (20 + metadata.len()).next_multiple_of(16);
    let file_size = data_offset + data.len();
    let mut output = vec![0; 20];
    output[0..4].copy_from_slice(&u32::try_from(metadata.len()).unwrap().to_be_bytes());
    output[4..8].copy_from_slice(&u32::try_from(file_size).unwrap().to_be_bytes());
    output[8..12].copy_from_slice(&version.to_be_bytes());
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
