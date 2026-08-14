use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(label: &str) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sequence = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "assetstudio-cli-{label}-{}-{timestamp}-{sequence}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn info_and_legacy_info_are_read_only_summaries() {
    let root = TestDirectory::new("info-read-only");
    let input = root.path().join("fixture.assets");
    let original = synthetic_v22_text_asset();
    fs::write(&input, &original).unwrap();

    let before = directory_entries(root.path());
    let result = cli(root.path(), ["info".into(), input.as_os_str().into()]);
    assert_success(&result);
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(stdout.contains("serialized files: 1"), "stdout: {stdout}");
    assert!(stdout.contains("objects: 1"), "stdout: {stdout}");
    assert!(stdout.contains("49 (TextAsset): 1"), "stdout: {stdout}");
    assert!(
        stdout.contains("2022.3.62f1: 1 file(s)"),
        "stdout: {stdout}"
    );
    assert!(!stdout.contains("path ID 7"), "stdout: {stdout}");

    let legacy = cli(
        root.path(),
        [input.as_os_str().into(), "-m".into(), "info".into()],
    );
    assert_success(&legacy);
    assert_eq!(directory_entries(root.path()), before);
    assert_eq!(fs::read(&input).unwrap(), original);
    assert!(!root.path().join("ASExport").exists());
    assert!(!root.path().join("ASExtract").exists());
}

#[test]
fn list_reports_objects_without_creating_output() {
    let root = TestDirectory::new("list-read-only");
    let input = root.path().join("fixture.assets");
    fs::write(&input, synthetic_v22_text_asset()).unwrap();

    let result = cli(root.path(), ["list".into(), input.as_os_str().into()]);
    assert_success(&result);
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(
        stdout.contains("path ID 7: class 49 (TextAsset), type 0"),
        "stdout: {stdout}"
    );
    assert_eq!(directory_entries(root.path()), vec!["fixture.assets"]);
}

#[test]
fn inspect_directory_continues_and_returns_partial_failure() {
    let root = TestDirectory::new("inspect-partial");
    let input = root.path().join("input");
    fs::create_dir(&input).unwrap();
    fs::write(input.join("a.assets"), synthetic_v22_text_asset()).unwrap();
    fs::write(
        input.join("z-truncated.gz"),
        [0x1f, 0x8b, 0x08, 0, 0, 0, 0, 0, 0, 0],
    )
    .unwrap();

    let result = cli(root.path(), ["inspect".into(), input.as_os_str().into()]);
    assert_eq!(result.status.code(), Some(3));
    let stdout = String::from_utf8_lossy(&result.stdout);
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(stdout.contains("a.assets"), "stdout: {stdout}");
    assert!(stdout.contains("49 (TextAsset): 1"), "stdout: {stdout}");
    assert!(
        stdout.contains("inspect summary: 1 succeeded, 1 failed"),
        "stdout: {stdout}"
    );
    assert!(
        stderr.contains("inspect completed with 1 failure(s)"),
        "stderr: {stderr}"
    );
    assert!(!root.path().join("ASExport").exists());
    assert!(!root.path().join("ASExtract").exists());
}

#[test]
fn usage_runtime_and_help_have_stable_exit_codes() {
    let root = TestDirectory::new("exit-codes");

    let no_arguments = cli(root.path(), std::iter::empty::<std::ffi::OsString>());
    assert_eq!(no_arguments.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&no_arguments.stderr).contains("input path or command"));

    let missing_inspect_path = cli(root.path(), ["inspect".into()]);
    assert_eq!(missing_inspect_path.status.code(), Some(2));

    let missing_input = cli(
        root.path(),
        ["info".into(), root.path().join("missing").into_os_string()],
    );
    assert_eq!(missing_input.status.code(), Some(1));

    let help = cli(root.path(), ["--help".into()]);
    assert_success(&help);
    assert!(String::from_utf8_lossy(&help.stdout).contains("Read-only commands:"));
}

#[test]
fn missing_extract_and_export_inputs_leave_no_output_directory() {
    let root = TestDirectory::new("no-output-side-effects");
    let input = root.path().join("missing.assets");
    let extract_output = root.path().join("extract-output");
    let extract = cli(
        root.path(),
        [
            input.as_os_str().into(),
            "-m".into(),
            "extract".into(),
            "-o".into(),
            extract_output.as_os_str().into(),
        ],
    );
    assert_eq!(extract.status.code(), Some(1));
    assert!(!extract_output.exists());

    let export_output = root.path().join("export-output");
    let export = cli(
        root.path(),
        [
            "export".into(),
            input.as_os_str().into(),
            export_output.as_os_str().into(),
        ],
    );
    assert_eq!(export.status.code(), Some(1));
    assert!(!export_output.exists());
    assert!(!root.path().join("ASExport").exists());
    assert!(!root.path().join("ASExtract").exists());
}

#[test]
fn one_unreadable_file_does_not_hide_the_rest_of_a_directory() {
    let root = TestDirectory::new("skip-unreadable");
    let inputs = root.path().join("game_Data");
    fs::create_dir_all(&inputs).unwrap();
    fs::write(inputs.join("a-good.assets"), synthetic_v22_text_asset()).unwrap();
    // Recognized, and explicitly refused because its layout is unverified.
    let mut archive = Vec::new();
    archive.extend_from_slice(b"UnityArchive\0");
    archive.extend_from_slice(&5_u32.to_be_bytes());
    archive.extend_from_slice(b"5.x.x\0");
    archive.extend_from_slice(b"5.0.0f4\0");
    fs::write(inputs.join("b-archive.unity3d"), &archive).unwrap();
    fs::write(inputs.join("c-good.assets"), synthetic_v22_text_asset()).unwrap();

    let result = cli(root.path(), ["list".into(), inputs.as_os_str().into()]);
    let stdout = String::from_utf8_lossy(&result.stdout);
    // The readable files still load, the skipped one is named, and the run
    // reports a partial failure instead of a silent success.
    assert!(stdout.contains("serialized files: 2"), "stdout: {stdout}");
    assert!(stdout.contains("skipped"), "stdout: {stdout}");
    assert!(stdout.contains("b-archive"), "stdout: {stdout}");
    assert_eq!(result.status.code(), Some(3), "stdout: {stdout}");

    // A single input that cannot be parsed is still a hard failure, not an
    // empty success.
    let only_bad = root.path().join("only-bad");
    fs::create_dir_all(&only_bad).unwrap();
    fs::write(only_bad.join("archive.unity3d"), &archive).unwrap();
    let result = cli(root.path(), ["list".into(), only_bad.as_os_str().into()]);
    assert_eq!(result.status.code(), Some(1));
}

#[test]
fn unity_version_overrides_a_stripped_serialized_file_version() {
    let root = TestDirectory::new("unity-version");
    let input = root.path().join("stripped.assets");
    fs::write(&input, synthetic_v22_text_asset_with_version("0.0.0")).unwrap();

    // Without the flag there is no version to parse against, so the declared
    // one stands and nothing reports an effective version.
    let bare = cli(root.path(), ["list".into(), input.as_os_str().into()]);
    assert_success(&bare);
    let stdout = String::from_utf8_lossy(&bare.stdout);
    assert!(stdout.contains("Unity version: 0.0.0"), "stdout: {stdout}");
    assert!(
        !stdout.contains("effective Unity version"),
        "stdout: {stdout}"
    );

    for flag in [
        vec!["--unity-version".into(), "2022.3.62f1".into()],
        vec!["--unity-version=2022.3.62f1".into()],
    ] {
        let mut arguments: Vec<std::ffi::OsString> = vec!["list".into()];
        arguments.extend(flag);
        arguments.push(input.as_os_str().into());
        let result = cli(root.path(), arguments);
        assert_success(&result);
        let stdout = String::from_utf8_lossy(&result.stdout);
        assert!(stdout.contains("Unity version: 0.0.0"), "stdout: {stdout}");
        assert!(
            stdout.contains("effective Unity version: 2022.3.62f1"),
            "stdout: {stdout}"
        );
    }

    // The flag applies to every command that opens a collection, including the
    // legacy `<input> -m <mode>` spellings.
    let scene = cli(
        root.path(),
        [
            "scene".into(),
            "--unity-version".into(),
            "2022.3.62f1".into(),
            input.as_os_str().into(),
        ],
    );
    assert_success(&scene);

    for bad in [
        vec!["--unity-version".into()],
        vec!["--unity-version".into(), "not-a-version".into()],
    ] {
        let mut arguments: Vec<std::ffi::OsString> = vec!["list".into()];
        arguments.extend(bad);
        arguments.push(input.as_os_str().into());
        let result = cli(root.path(), arguments);
        assert_eq!(result.status.code(), Some(2), "expected a usage error");
    }
}

fn cli<I>(current_directory: &Path, arguments: I) -> Output
where
    I: IntoIterator<Item = std::ffi::OsString>,
{
    Command::new(env!("CARGO_BIN_EXE_assetstudio"))
        .current_dir(current_directory)
        .args(arguments)
        .output()
        .unwrap()
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn directory_entries(path: &Path) -> Vec<String> {
    let mut entries = fs::read_dir(path)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    entries.sort_unstable();
    entries
}

fn synthetic_v22_text_asset() -> Vec<u8> {
    synthetic_v22_text_asset_with_version("2022.3.62f1")
}

fn synthetic_v22_text_asset_with_version(unity_version: &str) -> Vec<u8> {
    let mut object = Vec::new();
    push_i32_le(&mut object, 8);
    object.extend_from_slice(b"demo.lua");
    push_i32_le(&mut object, 7);
    object.extend_from_slice(b"payload");

    let mut metadata = Vec::new();
    metadata.extend_from_slice(unity_version.as_bytes());
    metadata.push(0);
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

fn push_i32_le(output: &mut Vec<u8>, value: i32) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn align_vec_with_base(output: &mut Vec<u8>, base: usize, alignment: usize) {
    while !(base + output.len()).is_multiple_of(alignment) {
        output.push(0);
    }
}
