use std::fs;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn exports_a_text_asset_from_the_native_cli() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("assetstudio-cli-{unique}"));
    let input = root.join("fixture.assets");
    let output = root.join("output");
    fs::create_dir_all(&root).unwrap();
    fs::write(&input, synthetic_v22_text_asset()).unwrap();

    let result = Command::new(env!("CARGO_BIN_EXE_assetstudio"))
        .arg("export")
        .arg(&input)
        .arg(&output)
        .output()
        .unwrap();

    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        String::from_utf8_lossy(&result.stdout).contains("1 succeeded, 0 unsupported, 0 failed")
    );
    assert_eq!(
        fs::read(output.join("0000_fixture.assets").join("demo.lua")).unwrap(),
        b"payload"
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn exports_lossless_webp_from_the_native_cli() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("assetstudio-cli-webp-{unique}"));
    let input = root.join("texture.assets");
    let output = root.join("output");
    fs::create_dir_all(&root).unwrap();
    fs::write(&input, synthetic_v22_texture2d()).unwrap();

    let result = Command::new(env!("CARGO_BIN_EXE_assetstudio"))
        .arg("export")
        .arg(&input)
        .arg(&output)
        .args(["--image-format", "webp"])
        .output()
        .unwrap();

    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let webp = fs::read(output.join("0000_texture.assets").join("image.webp")).unwrap();
    assert_eq!(&webp[..4], b"RIFF");
    assert_eq!(&webp[8..12], b"WEBP");
    assert_eq!(&webp[12..16], b"VP8L");

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn exports_configurable_jpeg_from_the_native_cli() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("assetstudio-cli-jpeg-{unique}"));
    let input = root.join("texture.assets");
    let output = root.join("output");
    fs::create_dir_all(&root).unwrap();
    fs::write(&input, synthetic_v22_texture2d()).unwrap();

    let result = Command::new(env!("CARGO_BIN_EXE_assetstudio"))
        .arg("export")
        .arg(&input)
        .arg(&output)
        .args(["--image-format", "jpeg", "--jpeg-quality", "100"])
        .output()
        .unwrap();

    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let jpeg = fs::read(output.join("0000_texture.assets").join("image.jpg")).unwrap();
    assert_eq!(&jpeg[..2], &[0xff, 0xd8]);
    assert_eq!(&jpeg[jpeg.len() - 2..], &[0xff, 0xd9]);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn exports_switch_mip_chain_base_image_from_the_native_cli() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("assetstudio-cli-switch-{unique}"));
    let input = root.join("switch.assets");
    let output = root.join("output");
    fs::create_dir_all(&root).unwrap();
    fs::write(&input, synthetic_v22_switch_mip_chain()).unwrap();

    let result = Command::new(env!("CARGO_BIN_EXE_assetstudio"))
        .arg("export")
        .arg(&input)
        .arg(&output)
        .args(["--image-format", "raw-rgba"])
        .output()
        .unwrap();

    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let rgba = fs::read(output.join("0000_switch.assets").join("switch-chain.rgba")).unwrap();
    assert_eq!(&rgba[..16], b"HARUKI_RGBAIR_V1");
    assert_eq!(
        &rgba[16..36],
        &[1, 0, 0, 0, 1, 0, 0, 0, 4, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0]
    );
    assert_eq!(&rgba[36..], &[9, 8, 7, 6]);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn reports_a_unity6_shader_as_unsupported_rather_than_failing() {
    // This used to assert the CLI wrote Unity6Object.shader. Unity changed the
    // serialized shader in 2021 and neither implementation reads the new
    // layout, so what it actually wrote was a file parsed from a fixture no
    // Unity produces. Declining is the honest outcome -- and it is not a
    // failure: a 2022 game carries hundreds of these, and an export that
    // exits non-zero because of them cannot be told from one that broke.
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("assetstudio-cli-shader-{unique}"));
    let input = root.join("unity6-shader.assets");
    let output = root.join("output");
    fs::create_dir_all(&root).unwrap();
    fs::write(&input, synthetic_v22_unity6_shader()).unwrap();

    let result = Command::new(env!("CARGO_BIN_EXE_assetstudio"))
        .arg("export")
        .arg(&input)
        .arg(&output)
        .output()
        .unwrap();

    assert!(
        result.status.success(),
        "an unsupported object is not a failed export; stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(
        stdout.contains("unsupported ") && stdout.contains("2021"),
        "the run should say which object it declined and why: {stdout}"
    );
    assert!(
        stdout.contains("0 succeeded, 1 unsupported, 0 failed"),
        "summary should separate the three outcomes: {stdout}"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn exports_verified_legacy_pcm_as_wav_from_the_native_cli() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("assetstudio-cli-audio-{unique}"));
    let input = root.join("audio.assets");
    let output = root.join("output");
    fs::create_dir_all(&root).unwrap();
    fs::write(&input, synthetic_v22_legacy_pcm()).unwrap();

    let result = Command::new(env!("CARGO_BIN_EXE_assetstudio"))
        .arg("export")
        .arg(&input)
        .arg(&output)
        .args(["--audio-format", "wav"])
        .output()
        .unwrap();

    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let wav = fs::read(output.join("0000_audio.assets").join("legacy-pcm.wav")).unwrap();
    assert_eq!(&wav[..12], b"RIFF(\0\0\0WAVE");
    assert_eq!(&wav[36..44], b"data\x04\0\0\0");
    assert_eq!(&wav[44..], &[1, 2, 3, 4]);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn exports_decoder_free_fsb5_pcm_as_wav_from_the_native_cli() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("assetstudio-cli-fsb5-pcm-{unique}"));
    let input = root.join("audio.assets");
    let output = root.join("output");
    fs::create_dir_all(&root).unwrap();
    fs::write(&input, synthetic_v22_fsb5_pcm()).unwrap();

    let result = Command::new(env!("CARGO_BIN_EXE_assetstudio"))
        .arg("export")
        .arg(&input)
        .arg(&output)
        .args(["--audio-format", "wav"])
        .output()
        .unwrap();

    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let wav = fs::read(output.join("0000_audio.assets").join("fsb5-pcm.wav")).unwrap();
    assert_eq!(&wav[..12], b"RIFF(\0\0\0WAVE");
    assert_eq!(&wav[20..28], &[1, 0, 2, 0, 68, 172, 0, 0]);
    assert_eq!(&wav[44..], &[1, 2, 3, 4]);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn exports_fsb5_ima_as_wav_from_the_native_cli() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("assetstudio-cli-fsb5-ima-{unique}"));
    let input = root.join("audio.assets");
    let output = root.join("output");
    fs::create_dir_all(&root).unwrap();
    fs::write(&input, synthetic_v22_fsb5_ima()).unwrap();

    let result = Command::new(env!("CARGO_BIN_EXE_assetstudio"))
        .arg("export")
        .arg(&input)
        .arg(&output)
        .args(["--audio-format", "wav"])
        .output()
        .unwrap();

    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let wav = fs::read(output.join("0000_audio.assets").join("fsb5-ima.wav")).unwrap();
    assert_eq!(&wav[..4], b"RIFF");
    assert_eq!(&wav[20..28], &[1, 0, 1, 0, 68, 172, 0, 0]);
    assert_eq!(wav.len(), 44 + 64 * 2);
    assert_eq!(i16::from_le_bytes(wav[44..46].try_into().unwrap()), 1000);
    assert_eq!(i16::from_le_bytes(wav[46..48].try_into().unwrap()), 1002);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn exports_fsb5_dsp_as_wav_from_the_native_cli() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("assetstudio-cli-fsb5-dsp-{unique}"));
    let input = root.join("audio.assets");
    let output = root.join("output");
    fs::create_dir_all(&root).unwrap();
    fs::write(&input, synthetic_v22_fsb5_dsp()).unwrap();

    let result = Command::new(env!("CARGO_BIN_EXE_assetstudio"))
        .arg("export")
        .arg(&input)
        .arg(&output)
        .args(["--audio-format", "wav"])
        .output()
        .unwrap();

    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let wav = fs::read(output.join("0000_audio.assets").join("fsb5-dsp.wav")).unwrap();
    assert_eq!(&wav[..4], b"RIFF");
    assert_eq!(&wav[20..28], &[1, 0, 1, 0, 68, 172, 0, 0]);
    assert_eq!(wav.len(), 44 + 14 * 2);
    assert_eq!(i16::from_le_bytes(wav[44..46].try_into().unwrap()), 1);
    assert_eq!(i16::from_le_bytes(wav[46..48].try_into().unwrap()), 3);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn exports_fsb5_vag_as_wav_from_the_native_cli() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("assetstudio-cli-fsb5-vag-{unique}"));
    let input = root.join("audio.assets");
    let output = root.join("output");
    fs::create_dir_all(&root).unwrap();
    fs::write(&input, synthetic_v22_fsb5_vag()).unwrap();

    let result = Command::new(env!("CARGO_BIN_EXE_assetstudio"))
        .arg("export")
        .arg(&input)
        .arg(&output)
        .args(["--audio-format", "wav"])
        .output()
        .unwrap();

    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let wav = fs::read(output.join("0000_audio.assets").join("fsb5-vag.wav")).unwrap();
    assert_eq!(&wav[..4], b"RIFF");
    assert_eq!(&wav[20..28], &[1, 0, 1, 0, 68, 172, 0, 0]);
    assert_eq!(wav.len(), 44 + 56 * 2);
    assert_eq!(i16::from_le_bytes(wav[44..46].try_into().unwrap()), 1);
    assert_eq!(i16::from_le_bytes(wav[46..48].try_into().unwrap()), 2);
    assert_eq!(i16::from_le_bytes(wav[100..102].try_into().unwrap()), 2);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn exports_fsb5_hevag_as_wav_from_the_native_cli() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("assetstudio-cli-fsb5-hevag-{unique}"));
    let input = root.join("audio.assets");
    let output = root.join("output");
    fs::create_dir_all(&root).unwrap();
    fs::write(&input, synthetic_v22_fsb5_hevag()).unwrap();

    let result = Command::new(env!("CARGO_BIN_EXE_assetstudio"))
        .arg("export")
        .arg(&input)
        .arg(&output)
        .args(["--audio-format", "wav"])
        .output()
        .unwrap();

    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let wav = fs::read(output.join("0000_audio.assets").join("fsb5-hevag.wav")).unwrap();
    assert_eq!(&wav[..4], b"RIFF");
    assert_eq!(&wav[20..28], &[1, 0, 1, 0, 68, 172, 0, 0]);
    assert_eq!(wav.len(), 44 + 56 * 2);
    assert_eq!(i16::from_le_bytes(wav[44..46].try_into().unwrap()), 1);
    assert_eq!(i16::from_le_bytes(wav[46..48].try_into().unwrap()), 2);
    assert_eq!(i16::from_le_bytes(wav[100..102].try_into().unwrap()), 2);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn exports_fsb5_fadpcm_as_wav_from_the_native_cli() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("assetstudio-cli-fsb5-fadpcm-{unique}"));
    let input = root.join("audio.assets");
    let output = root.join("output");
    fs::create_dir_all(&root).unwrap();
    fs::write(&input, synthetic_v22_fsb5_fadpcm()).unwrap();

    let result = Command::new(env!("CARGO_BIN_EXE_assetstudio"))
        .arg("export")
        .arg(&input)
        .arg(&output)
        .args(["--audio-format", "wav"])
        .output()
        .unwrap();

    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let wav = fs::read(output.join("0000_audio.assets").join("fsb5-fadpcm.wav")).unwrap();
    assert_eq!(&wav[..4], b"RIFF");
    assert_eq!(&wav[20..28], &[1, 0, 1, 0, 68, 172, 0, 0]);
    assert_eq!(wav.len(), 44 + 512 * 2);
    assert_eq!(i16::from_le_bytes(wav[44..46].try_into().unwrap()), 1);
    assert_eq!(i16::from_le_bytes(wav[46..48].try_into().unwrap()), 2);
    assert_eq!(i16::from_le_bytes(wav[556..558].try_into().unwrap()), 2);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn exports_fsb5_mpeg_as_wav_from_the_native_cli() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("assetstudio-cli-fsb5-mpeg-{unique}"));
    let input = root.join("audio.assets");
    let output = root.join("output");
    fs::create_dir_all(&root).unwrap();
    fs::write(&input, synthetic_v22_fsb5_mpeg()).unwrap();

    let result = Command::new(env!("CARGO_BIN_EXE_assetstudio"))
        .arg("export")
        .arg(&input)
        .arg(&output)
        .args(["--audio-format", "wav"])
        .output()
        .unwrap();

    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let wav = fs::read(output.join("0000_audio.assets").join("fsb5-mpeg.wav")).unwrap();
    assert_eq!(&wav[..4], b"RIFF");
    assert_eq!(&wav[20..28], &[1, 0, 1, 0, 68, 172, 0, 0]);
    assert_eq!(wav.len(), 44 + 2304 * 2);
    assert!(wav[44..].iter().all(|byte| *byte == 0));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn exports_fsb5_opus_as_wav_from_the_native_cli() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("assetstudio-cli-fsb5-opus-{unique}"));
    let input = root.join("audio.assets");
    let output = root.join("output");
    fs::create_dir_all(&root).unwrap();
    fs::write(&input, synthetic_v22_fsb5_opus()).unwrap();

    let result = Command::new(env!("CARGO_BIN_EXE_assetstudio"))
        .arg("export")
        .arg(&input)
        .arg(&output)
        .args(["--audio-format", "wav"])
        .output()
        .unwrap();

    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let wav = fs::read(output.join("0000_audio.assets").join("fsb5-opus.wav")).unwrap();
    assert_eq!(&wav[..4], b"RIFF");
    assert_eq!(&wav[20..28], &[1, 0, 1, 0, 128, 187, 0, 0]);
    assert_eq!(wav.len(), 44 + 648 * 2);
    assert!(wav[44..].iter().all(|byte| *byte == 0));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn exports_fsb5_vorbis_as_wav_from_the_native_cli() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("assetstudio-cli-fsb5-vorbis-{unique}"));
    let input = root.join("audio.assets");
    let output = root.join("output");
    fs::create_dir_all(&root).unwrap();
    fs::write(&input, synthetic_v22_fsb5_vorbis()).unwrap();

    let result = Command::new(env!("CARGO_BIN_EXE_assetstudio"))
        .arg("export")
        .arg(&input)
        .arg(&output)
        .args(["--audio-format", "wav"])
        .output()
        .unwrap();

    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let wav = fs::read(output.join("0000_audio.assets").join("fsb5-vorbis.wav")).unwrap();
    assert_eq!(&wav[..4], b"RIFF");
    assert_eq!(&wav[20..28], &[1, 0, 2, 0, 128, 187, 0, 0]);
    assert_eq!(wav.len(), 44 + 4800 * 2 * 2);
    assert!(wav[44..].iter().any(|byte| *byte != 0));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn legacy_dump_writes_managed_type_tree_text_instead_of_json() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("assetstudio-cli-dump-{unique}"));
    let input = root.join("dump.assets");
    let output = root.join("output");
    fs::create_dir_all(&root).unwrap();
    fs::write(&input, synthetic_v22_dump_object()).unwrap();

    let result = Command::new(env!("CARGO_BIN_EXE_assetstudio"))
        .arg(&input)
        .args(["-m", "dump", "-o"])
        .arg(&output)
        .output()
        .unwrap();

    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        fs::read(output.join("0000_dump.assets").join("Hero.txt")).unwrap(),
        b"Root Base\r\n\tstring m_Name = \"Hero\"\r\n\tSInt32 score = 123\r\n"
    );

    fs::remove_dir_all(root).unwrap();
}

fn synthetic_v22_dump_object() -> Vec<u8> {
    let mut object = Vec::new();
    push_i32_le(&mut object, 4);
    object.extend_from_slice(b"Hero");
    push_i32_le(&mut object, 123);

    let nodes = [
        TestNode::new("Root", "Base", 0, false),
        TestNode::new("string", "m_Name", 1, false),
        TestNode::new("Array", "Array", 2, true),
        TestNode::new("int", "size", 3, false),
        TestNode::new("char", "data", 3, false),
        TestNode::new("SInt32", "score", 1, false),
    ];
    let mut metadata = Vec::new();
    metadata.extend_from_slice(b"2022.3.62f1\0");
    push_i32_le(&mut metadata, 13);
    metadata.push(1);
    push_i32_le(&mut metadata, 1);
    push_i32_le(&mut metadata, 123_456);
    metadata.push(0);
    metadata.extend_from_slice(&(-1_i16).to_le_bytes());
    metadata.extend_from_slice(&[0_u8; 16]);
    push_blob_tree(&mut metadata, &nodes);
    push_i32_le(&mut metadata, 0);
    push_i32_le(&mut metadata, 1);
    align_vec_with_base(&mut metadata, 48, 4);
    metadata.extend_from_slice(&7_i64.to_le_bytes());
    metadata.extend_from_slice(&0_i64.to_le_bytes());
    metadata.extend_from_slice(&u32::try_from(object.len()).unwrap().to_le_bytes());
    push_i32_le(&mut metadata, 0);
    for _ in 0..3 {
        push_i32_le(&mut metadata, 0);
    }
    metadata.push(0);
    finish_v22(&metadata, &object)
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
    push_i32_le(output, i32::try_from(nodes.len()).unwrap());
    push_i32_le(output, i32::try_from(strings.len()).unwrap());
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

fn synthetic_v22_text_asset() -> Vec<u8> {
    let mut object = Vec::new();
    push_i32_le(&mut object, 8);
    object.extend_from_slice(b"demo.lua");
    push_i32_le(&mut object, 7);
    object.extend_from_slice(b"payload");

    let mut metadata = Vec::new();
    metadata.extend_from_slice(b"2022.3.62f1\0");
    push_i32_le(&mut metadata, 13);
    metadata.push(0);
    push_i32_le(&mut metadata, 1);
    push_i32_le(&mut metadata, 49);
    metadata.push(0);
    metadata.extend_from_slice(&(-1_i16).to_le_bytes());
    metadata.extend_from_slice(&[0_u8; 16]);
    push_i32_le(&mut metadata, 1);
    align_vec_with_base(&mut metadata, 48, 4);
    metadata.extend_from_slice(&7_i64.to_le_bytes());
    metadata.extend_from_slice(&0_i64.to_le_bytes());
    metadata.extend_from_slice(&u32::try_from(object.len()).unwrap().to_le_bytes());
    push_i32_le(&mut metadata, 0);
    for _ in 0..3 {
        push_i32_le(&mut metadata, 0);
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

fn synthetic_v22_texture2d() -> Vec<u8> {
    let mut object = Vec::new();
    push_i32_le(&mut object, 5);
    object.extend_from_slice(b"image");
    align_vec(&mut object, 4);
    push_i32_le(&mut object, 0);
    object.extend_from_slice(&[0, 0]);
    align_vec(&mut object, 4);
    push_i32_le(&mut object, 2);
    push_i32_le(&mut object, 2);
    push_u32_le(&mut object, 16);
    push_i32_le(&mut object, 0);
    push_i32_le(&mut object, 4);
    push_i32_le(&mut object, 1);
    object.extend_from_slice(&[0, 0, 0]);
    align_vec(&mut object, 4);
    push_i32_le(&mut object, 0);
    object.push(0);
    align_vec(&mut object, 4);
    push_i32_le(&mut object, 0);
    push_i32_le(&mut object, 1);
    push_i32_le(&mut object, 2);
    object.extend_from_slice(&[0_u8; 24]);
    for value in [0, 0, 0, 16] {
        push_i32_le(&mut object, value);
    }
    object.extend_from_slice(&[
        255, 0, 0, 1, 0, 255, 0, 2, // decoded bottom row
        0, 0, 255, 3, 255, 255, 255, 4, // decoded top row
    ]);

    let mut metadata = Vec::new();
    metadata.extend_from_slice(b"2022.3.62f1\0");
    push_i32_le(&mut metadata, 13);
    metadata.push(0);
    push_i32_le(&mut metadata, 1);
    push_i32_le(&mut metadata, 28);
    metadata.push(0);
    metadata.extend_from_slice(&(-1_i16).to_le_bytes());
    metadata.extend_from_slice(&[0_u8; 16]);
    push_i32_le(&mut metadata, 1);
    align_vec_with_base(&mut metadata, 48, 4);
    metadata.extend_from_slice(&7_i64.to_le_bytes());
    metadata.extend_from_slice(&0_i64.to_le_bytes());
    metadata.extend_from_slice(&u32::try_from(object.len()).unwrap().to_le_bytes());
    push_i32_le(&mut metadata, 0);
    for _ in 0..3 {
        push_i32_le(&mut metadata, 0);
    }
    metadata.push(0);
    finish_v22(&metadata, &object)
}

fn synthetic_v22_switch_mip_chain() -> Vec<u8> {
    let mut object = Vec::new();
    push_i32_le(&mut object, 12);
    object.extend_from_slice(b"switch-chain");
    align_vec(&mut object, 4);
    push_i32_le(&mut object, 0);
    object.extend_from_slice(&[0, 0]);
    align_vec(&mut object, 4);
    push_i32_le(&mut object, 1);
    push_i32_le(&mut object, 1);
    push_u32_le(&mut object, 640);
    push_i32_le(&mut object, 0);
    push_i32_le(&mut object, 4);
    push_i32_le(&mut object, 2);
    object.extend_from_slice(&[0, 0, 0]);
    align_vec(&mut object, 4);
    push_i32_le(&mut object, 0);
    object.push(0);
    align_vec(&mut object, 4);
    push_i32_le(&mut object, 0);
    push_i32_le(&mut object, 1);
    push_i32_le(&mut object, 2);
    object.extend_from_slice(&[0_u8; 24]);
    push_i32_le(&mut object, 0);
    push_i32_le(&mut object, 0);
    push_i32_le(&mut object, 12);
    object.extend_from_slice(&[0_u8; 12]);
    push_i32_le(&mut object, 640);
    object.extend_from_slice(&[9, 8, 7, 6]);
    object.extend_from_slice(&[0_u8; 508]);
    object.extend_from_slice(&[0xa5; 128]);

    let mut metadata = Vec::new();
    metadata.extend_from_slice(b"2022.3.62f1\0");
    push_i32_le(&mut metadata, 38);
    metadata.push(0);
    push_i32_le(&mut metadata, 1);
    push_i32_le(&mut metadata, 28);
    metadata.push(0);
    metadata.extend_from_slice(&(-1_i16).to_le_bytes());
    metadata.extend_from_slice(&[0_u8; 16]);
    push_i32_le(&mut metadata, 1);
    align_vec_with_base(&mut metadata, 48, 4);
    metadata.extend_from_slice(&7_i64.to_le_bytes());
    metadata.extend_from_slice(&0_i64.to_le_bytes());
    metadata.extend_from_slice(&u32::try_from(object.len()).unwrap().to_le_bytes());
    push_i32_le(&mut metadata, 0);
    for _ in 0..3 {
        push_i32_le(&mut metadata, 0);
    }
    metadata.push(0);
    finish_v22(&metadata, &object)
}

fn synthetic_v22_unity6_shader() -> Vec<u8> {
    let mut object = Vec::new();
    push_aligned_string(&mut object, "Unity6Object");
    for _ in 0..4 {
        push_i32_le(&mut object, 0); // properties, subshaders, keywords, keyword flags
    }
    push_aligned_string(&mut object, "Parsed/Unity6");
    push_aligned_string(&mut object, "");
    push_aligned_string(&mut object, "");
    push_i32_le(&mut object, 0); // dependencies
    push_i32_le(&mut object, 0); // render-pipeline custom editors
    object.push(0); // disable no-subshaders message
    align_vec(&mut object, 4);
    for _ in 0..7 {
        push_i32_le(&mut object, 0); // platforms/tables/blob/object tail counts
    }
    object.push(0); // baked
    align_vec(&mut object, 4);

    let mut metadata = Vec::new();
    metadata.extend_from_slice(b"6000.2.0f1\0");
    push_i32_le(&mut metadata, 13);
    metadata.push(0);
    push_i32_le(&mut metadata, 1);
    push_i32_le(&mut metadata, 48);
    metadata.push(0);
    metadata.extend_from_slice(&(-1_i16).to_le_bytes());
    metadata.extend_from_slice(&[0_u8; 16]);
    push_i32_le(&mut metadata, 1);
    align_vec_with_base(&mut metadata, 48, 4);
    metadata.extend_from_slice(&7_i64.to_le_bytes());
    metadata.extend_from_slice(&0_i64.to_le_bytes());
    metadata.extend_from_slice(&u32::try_from(object.len()).unwrap().to_le_bytes());
    push_i32_le(&mut metadata, 0);
    for _ in 0..3 {
        push_i32_le(&mut metadata, 0);
    }
    metadata.push(0);
    finish_v22(&metadata, &object)
}

fn synthetic_v22_legacy_pcm() -> Vec<u8> {
    let mut object = Vec::new();
    push_i32_le(&mut object, 10);
    object.extend_from_slice(b"legacy-pcm");
    align_vec(&mut object, 4);
    push_i32_le(&mut object, 2);
    object.extend_from_slice(&0_f32.to_le_bytes());
    push_i32_le(&mut object, 22_050);
    push_i32_le(&mut object, 4);
    object.extend_from_slice(&[1, 2, 3, 4]);

    finish_single_v22(83, "2.5.0f1", &object)
}

fn synthetic_v22_fsb5_pcm() -> Vec<u8> {
    let pcm = [1_u8, 2, 3, 4];
    let mut fsb = vec![0_u8; 0x3c];
    fsb[..4].copy_from_slice(b"FSB5");
    fsb[4..8].copy_from_slice(&1_u32.to_le_bytes());
    fsb[8..12].copy_from_slice(&1_u32.to_le_bytes());
    fsb[12..16].copy_from_slice(&8_u32.to_le_bytes());
    fsb[20..24].copy_from_slice(&u32::try_from(pcm.len()).unwrap().to_le_bytes());
    fsb[24..28].copy_from_slice(&2_u32.to_le_bytes());
    let sample_mode = (1_u64 << 34) | (1 << 5) | (8 << 1);
    fsb.extend_from_slice(&sample_mode.to_le_bytes());
    fsb.extend_from_slice(&pcm);

    let mut object = Vec::new();
    push_aligned_string(&mut object, "fsb5-pcm");
    push_i32_le(&mut object, 0);
    push_i32_le(&mut object, 2);
    push_i32_le(&mut object, 44_100);
    push_i32_le(&mut object, 16);
    object.extend_from_slice(&0_f32.to_le_bytes());
    object.push(0);
    align_vec(&mut object, 4);
    push_i32_le(&mut object, 0);
    object.extend_from_slice(&[0_u8; 3]);
    align_vec(&mut object, 4);
    push_aligned_string(&mut object, "");
    object.extend_from_slice(&0_i64.to_le_bytes());
    object.extend_from_slice(&i64::try_from(fsb.len()).unwrap().to_le_bytes());
    push_i32_le(&mut object, 0);
    object.extend_from_slice(&fsb);
    finish_single_v22(83, "2022.3.62f1", &object)
}

fn synthetic_v22_fsb5_ima() -> Vec<u8> {
    let mut block = vec![0x10_u8; 36];
    block[..2].copy_from_slice(&1000_i16.to_le_bytes());
    block[2] = 10;
    block[3] = 0;
    let mut fsb = vec![0_u8; 0x3c];
    fsb[..4].copy_from_slice(b"FSB5");
    fsb[4..8].copy_from_slice(&1_u32.to_le_bytes());
    fsb[8..12].copy_from_slice(&1_u32.to_le_bytes());
    fsb[12..16].copy_from_slice(&8_u32.to_le_bytes());
    fsb[20..24].copy_from_slice(&u32::try_from(block.len()).unwrap().to_le_bytes());
    fsb[24..28].copy_from_slice(&7_u32.to_le_bytes());
    let sample_mode = (64_u64 << 34) | (8 << 1);
    fsb.extend_from_slice(&sample_mode.to_le_bytes());
    fsb.extend_from_slice(&block);

    let mut object = Vec::new();
    push_aligned_string(&mut object, "fsb5-ima");
    push_i32_le(&mut object, 0);
    push_i32_le(&mut object, 1);
    push_i32_le(&mut object, 44_100);
    push_i32_le(&mut object, 16);
    object.extend_from_slice(&0_f32.to_le_bytes());
    object.push(0);
    align_vec(&mut object, 4);
    push_i32_le(&mut object, 0);
    object.extend_from_slice(&[0_u8; 3]);
    align_vec(&mut object, 4);
    push_aligned_string(&mut object, "");
    object.extend_from_slice(&0_i64.to_le_bytes());
    object.extend_from_slice(&i64::try_from(fsb.len()).unwrap().to_le_bytes());
    push_i32_le(&mut object, 0);
    object.extend_from_slice(&fsb);
    finish_single_v22(83, "2022.3.62f1", &object)
}

fn synthetic_v22_fsb5_dsp() -> Vec<u8> {
    let mut coefficients = vec![0_u8; 0x2e];
    coefficients[..2].copy_from_slice(&2048_i16.to_be_bytes());
    let sample_mode = (14_u64 << 34) | (8 << 1) | 1;
    let chunk_header = (7_u32 << 25) | (u32::try_from(coefficients.len()).unwrap() << 1);
    let mut headers = Vec::new();
    headers.extend_from_slice(&sample_mode.to_le_bytes());
    headers.extend_from_slice(&chunk_header.to_le_bytes());
    headers.extend_from_slice(&coefficients);
    let data = [0_u8, 0x12, 0x12, 0x12, 0x12, 0x12, 0x12, 0x12];
    let mut fsb = vec![0_u8; 0x3c];
    fsb[..4].copy_from_slice(b"FSB5");
    fsb[4..8].copy_from_slice(&1_u32.to_le_bytes());
    fsb[8..12].copy_from_slice(&1_u32.to_le_bytes());
    fsb[12..16].copy_from_slice(&u32::try_from(headers.len()).unwrap().to_le_bytes());
    fsb[20..24].copy_from_slice(&u32::try_from(data.len()).unwrap().to_le_bytes());
    fsb[24..28].copy_from_slice(&6_u32.to_le_bytes());
    fsb.extend_from_slice(&headers);
    fsb.extend_from_slice(&data);

    let mut object = Vec::new();
    push_aligned_string(&mut object, "fsb5-dsp");
    push_i32_le(&mut object, 0);
    push_i32_le(&mut object, 1);
    push_i32_le(&mut object, 44_100);
    push_i32_le(&mut object, 16);
    object.extend_from_slice(&0_f32.to_le_bytes());
    object.push(0);
    align_vec(&mut object, 4);
    push_i32_le(&mut object, 0);
    object.extend_from_slice(&[0_u8; 3]);
    align_vec(&mut object, 4);
    push_aligned_string(&mut object, "");
    object.extend_from_slice(&0_i64.to_le_bytes());
    object.extend_from_slice(&i64::try_from(fsb.len()).unwrap().to_le_bytes());
    push_i32_le(&mut object, 0);
    object.extend_from_slice(&fsb);
    finish_single_v22(83, "2022.3.62f1", &object)
}

fn synthetic_v22_fsb5_vag() -> Vec<u8> {
    let mut first = [0x21_u8; 16];
    first[0] = 0x0c;
    first[1] = 0;
    let mut second = [0x32_u8; 16];
    second[0] = 0x0c;
    second[1] = 0;
    let sample_mode = (56_u64 << 34) | (8 << 1);
    let mut fsb = vec![0_u8; 0x3c];
    fsb[..4].copy_from_slice(b"FSB5");
    fsb[4..8].copy_from_slice(&1_u32.to_le_bytes());
    fsb[8..12].copy_from_slice(&1_u32.to_le_bytes());
    fsb[12..16].copy_from_slice(&8_u32.to_le_bytes());
    fsb[20..24].copy_from_slice(&32_u32.to_le_bytes());
    fsb[24..28].copy_from_slice(&8_u32.to_le_bytes());
    fsb.extend_from_slice(&sample_mode.to_le_bytes());
    fsb.extend_from_slice(&first);
    fsb.extend_from_slice(&second);

    let mut object = Vec::new();
    push_aligned_string(&mut object, "fsb5-vag");
    push_i32_le(&mut object, 0);
    push_i32_le(&mut object, 1);
    push_i32_le(&mut object, 44_100);
    push_i32_le(&mut object, 16);
    object.extend_from_slice(&0_f32.to_le_bytes());
    object.push(0);
    align_vec(&mut object, 4);
    push_i32_le(&mut object, 0);
    object.extend_from_slice(&[0_u8; 3]);
    align_vec(&mut object, 4);
    push_aligned_string(&mut object, "");
    object.extend_from_slice(&0_i64.to_le_bytes());
    object.extend_from_slice(&i64::try_from(fsb.len()).unwrap().to_le_bytes());
    push_i32_le(&mut object, 0);
    object.extend_from_slice(&fsb);
    finish_single_v22(83, "2022.3.62f1", &object)
}

fn synthetic_v22_fsb5_hevag() -> Vec<u8> {
    let mut first = [0x21_u8; 16];
    first[0] = 0x0c;
    first[1] = 0;
    let mut second = [0x32_u8; 16];
    second[0] = 0x0c;
    second[1] = 0;
    let mut fsb = vec![0_u8; 0x3c];
    fsb[..4].copy_from_slice(b"FSB5");
    fsb[4..8].copy_from_slice(&1_u32.to_le_bytes());
    fsb[8..12].copy_from_slice(&1_u32.to_le_bytes());
    fsb[12..16].copy_from_slice(&8_u32.to_le_bytes());
    fsb[20..24].copy_from_slice(&32_u32.to_le_bytes());
    fsb[24..28].copy_from_slice(&9_u32.to_le_bytes());
    let sample_mode = (56_u64 << 34) | (8 << 1);
    fsb.extend_from_slice(&sample_mode.to_le_bytes());
    fsb.extend_from_slice(&first);
    fsb.extend_from_slice(&second);

    let mut object = Vec::new();
    push_aligned_string(&mut object, "fsb5-hevag");
    push_i32_le(&mut object, 0);
    push_i32_le(&mut object, 1);
    push_i32_le(&mut object, 44_100);
    push_i32_le(&mut object, 16);
    object.extend_from_slice(&0_f32.to_le_bytes());
    object.push(0);
    align_vec(&mut object, 4);
    push_i32_le(&mut object, 0);
    object.extend_from_slice(&[0_u8; 3]);
    align_vec(&mut object, 4);
    push_aligned_string(&mut object, "");
    object.extend_from_slice(&0_i64.to_le_bytes());
    object.extend_from_slice(&i64::try_from(fsb.len()).unwrap().to_le_bytes());
    push_i32_le(&mut object, 0);
    object.extend_from_slice(&fsb);
    finish_single_v22(83, "2022.3.62f1", &object)
}

fn synthetic_v22_fsb5_fadpcm() -> Vec<u8> {
    let mut first = vec![0x21_u8; 0x8c];
    first[..12].fill(0);
    let mut second = vec![0x32_u8; 0x8c];
    second[..12].fill(0);
    let mut fsb = vec![0_u8; 0x3c];
    fsb[..4].copy_from_slice(b"FSB5");
    fsb[4..8].copy_from_slice(&1_u32.to_le_bytes());
    fsb[8..12].copy_from_slice(&1_u32.to_le_bytes());
    fsb[12..16].copy_from_slice(&8_u32.to_le_bytes());
    fsb[20..24].copy_from_slice(&0x118_u32.to_le_bytes());
    fsb[24..28].copy_from_slice(&16_u32.to_le_bytes());
    let sample_mode = (512_u64 << 34) | (8 << 1);
    fsb.extend_from_slice(&sample_mode.to_le_bytes());
    fsb.extend_from_slice(&first);
    fsb.extend_from_slice(&second);

    let mut object = Vec::new();
    push_aligned_string(&mut object, "fsb5-fadpcm");
    push_i32_le(&mut object, 0);
    push_i32_le(&mut object, 1);
    push_i32_le(&mut object, 44_100);
    push_i32_le(&mut object, 16);
    object.extend_from_slice(&0_f32.to_le_bytes());
    object.push(0);
    align_vec(&mut object, 4);
    push_i32_le(&mut object, 0);
    object.extend_from_slice(&[0_u8; 3]);
    align_vec(&mut object, 4);
    push_aligned_string(&mut object, "");
    object.extend_from_slice(&0_i64.to_le_bytes());
    object.extend_from_slice(&i64::try_from(fsb.len()).unwrap().to_le_bytes());
    push_i32_le(&mut object, 0);
    object.extend_from_slice(&fsb);
    finish_single_v22(83, "2022.3.62f1", &object)
}

fn synthetic_v22_fsb5_mpeg() -> Vec<u8> {
    let mut frames = vec![0_u8; 208];
    frames[..4].copy_from_slice(&[0xff, 0xfb, 0x10, 0xc0]);
    frames[104..108].copy_from_slice(&[0xff, 0xfb, 0x10, 0xc0]);
    let mut fsb = vec![0_u8; 0x3c];
    fsb[..4].copy_from_slice(b"FSB5");
    fsb[4..8].copy_from_slice(&1_u32.to_le_bytes());
    fsb[8..12].copy_from_slice(&1_u32.to_le_bytes());
    fsb[12..16].copy_from_slice(&8_u32.to_le_bytes());
    fsb[20..24].copy_from_slice(&208_u32.to_le_bytes());
    fsb[24..28].copy_from_slice(&11_u32.to_le_bytes());
    let sample_mode = (2304_u64 << 34) | (8 << 1);
    fsb.extend_from_slice(&sample_mode.to_le_bytes());
    fsb.extend_from_slice(&frames);

    let mut object = Vec::new();
    push_aligned_string(&mut object, "fsb5-mpeg");
    push_i32_le(&mut object, 0);
    push_i32_le(&mut object, 1);
    push_i32_le(&mut object, 44_100);
    push_i32_le(&mut object, 16);
    object.extend_from_slice(&0_f32.to_le_bytes());
    object.push(0);
    align_vec(&mut object, 4);
    push_i32_le(&mut object, 0);
    object.extend_from_slice(&[0_u8; 3]);
    align_vec(&mut object, 4);
    push_aligned_string(&mut object, "");
    object.extend_from_slice(&0_i64.to_le_bytes());
    object.extend_from_slice(&i64::try_from(fsb.len()).unwrap().to_le_bytes());
    push_i32_le(&mut object, 0);
    object.extend_from_slice(&fsb);
    finish_single_v22(83, "2022.3.62f1", &object)
}

fn synthetic_v22_fsb5_opus() -> Vec<u8> {
    let packet = [
        0xf8, 0x6f, 0xed, 0x8a, 0x58, 0xc6, 0x40, 0x44, 0x64, 0xd8, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0xad, 0x43, 0xa8, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00,
    ];
    let mut encoded = Vec::new();
    encoded.extend_from_slice(&64_u16.to_le_bytes());
    encoded.extend_from_slice(&packet);
    encoded.extend_from_slice(&0_u16.to_le_bytes());
    let mut fsb = vec![0_u8; 0x3c];
    fsb[..4].copy_from_slice(b"FSB5");
    fsb[4..8].copy_from_slice(&1_u32.to_le_bytes());
    fsb[8..12].copy_from_slice(&1_u32.to_le_bytes());
    fsb[12..16].copy_from_slice(&8_u32.to_le_bytes());
    fsb[20..24].copy_from_slice(&u32::try_from(encoded.len()).unwrap().to_le_bytes());
    fsb[24..28].copy_from_slice(&17_u32.to_le_bytes());
    let sample_mode = (648_u64 << 34) | (9 << 1);
    fsb.extend_from_slice(&sample_mode.to_le_bytes());
    fsb.extend_from_slice(&encoded);

    let mut object = Vec::new();
    push_aligned_string(&mut object, "fsb5-opus");
    push_i32_le(&mut object, 0);
    push_i32_le(&mut object, 1);
    push_i32_le(&mut object, 48_000);
    push_i32_le(&mut object, 16);
    object.extend_from_slice(&0_f32.to_le_bytes());
    object.push(0);
    align_vec(&mut object, 4);
    push_i32_le(&mut object, 0);
    object.extend_from_slice(&[0_u8; 3]);
    align_vec(&mut object, 4);
    push_aligned_string(&mut object, "");
    object.extend_from_slice(&0_i64.to_le_bytes());
    object.extend_from_slice(&i64::try_from(fsb.len()).unwrap().to_le_bytes());
    push_i32_le(&mut object, 0);
    object.extend_from_slice(&fsb);
    finish_single_v22(83, "2022.3.62f1", &object)
}

fn synthetic_v22_fsb5_vorbis() -> Vec<u8> {
    const FSB: &[u8] =
        include_bytes!("../../assetstudio-core/tests/fixtures/audio/fsb5-vorbis-stereo.fsb");
    let mut object = Vec::new();
    push_aligned_string(&mut object, "fsb5-vorbis");
    push_i32_le(&mut object, 0);
    push_i32_le(&mut object, 2);
    push_i32_le(&mut object, 48_000);
    push_i32_le(&mut object, 16);
    object.extend_from_slice(&0_f32.to_le_bytes());
    object.push(0);
    align_vec(&mut object, 4);
    push_i32_le(&mut object, 0);
    object.extend_from_slice(&[0_u8; 3]);
    align_vec(&mut object, 4);
    push_aligned_string(&mut object, "");
    object.extend_from_slice(&0_i64.to_le_bytes());
    object.extend_from_slice(&i64::try_from(FSB.len()).unwrap().to_le_bytes());
    push_i32_le(&mut object, 0);
    object.extend_from_slice(FSB);
    finish_single_v22(83, "2022.3.62f1", &object)
}

fn finish_single_v22(class_id: i32, version: &str, object: &[u8]) -> Vec<u8> {
    let mut metadata = Vec::new();
    metadata.extend_from_slice(version.as_bytes());
    metadata.push(0);
    push_i32_le(&mut metadata, 13);
    metadata.push(0);
    push_i32_le(&mut metadata, 1);
    push_i32_le(&mut metadata, class_id);
    metadata.push(0);
    metadata.extend_from_slice(&(-1_i16).to_le_bytes());
    metadata.extend_from_slice(&[0_u8; 16]);
    push_i32_le(&mut metadata, 1);
    align_vec_with_base(&mut metadata, 48, 4);
    metadata.extend_from_slice(&7_i64.to_le_bytes());
    metadata.extend_from_slice(&0_i64.to_le_bytes());
    metadata.extend_from_slice(&u32::try_from(object.len()).unwrap().to_le_bytes());
    push_i32_le(&mut metadata, 0);
    for _ in 0..3 {
        push_i32_le(&mut metadata, 0);
    }
    metadata.push(0);
    finish_v22(&metadata, object)
}

fn finish_v22(metadata: &[u8], object: &[u8]) -> Vec<u8> {
    let metadata_size = u32::try_from(metadata.len()).unwrap();
    let metadata_end = 48_u64 + u64::from(metadata_size);
    let data_offset = metadata_end.div_ceil(16) * 16;
    let file_size = data_offset + u64::try_from(object.len()).unwrap();
    let mut bytes = vec![0_u8; 48];
    bytes[8..12].copy_from_slice(&22_u32.to_be_bytes());
    bytes[20..24].copy_from_slice(&metadata_size.to_be_bytes());
    bytes[24..32].copy_from_slice(&i64::try_from(file_size).unwrap().to_be_bytes());
    bytes[32..40].copy_from_slice(&i64::try_from(data_offset).unwrap().to_be_bytes());
    bytes.extend_from_slice(metadata);
    bytes.resize(usize::try_from(data_offset).unwrap(), 0);
    bytes.extend_from_slice(object);
    bytes
}

fn push_i32_le(output: &mut Vec<u8>, value: i32) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn push_u32_le(output: &mut Vec<u8>, value: u32) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn push_aligned_string(output: &mut Vec<u8>, value: &str) {
    push_i32_le(output, i32::try_from(value.len()).unwrap());
    output.extend_from_slice(value.as_bytes());
    align_vec(output, 4);
}

fn align_vec(output: &mut Vec<u8>, alignment: usize) {
    while !output.len().is_multiple_of(alignment) {
        output.push(0);
    }
}

fn align_vec_with_base(output: &mut Vec<u8>, base: usize, alignment: usize) {
    while !(base + output.len()).is_multiple_of(alignment) {
        output.push(0);
    }
}
