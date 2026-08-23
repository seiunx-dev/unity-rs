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
            "assetstudio-live2d-{label}-{}-{timestamp}-{sequence}",
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
fn live2d_exports_exact_verified_models_with_stable_clean_collision_names() {
    let root = TestDirectory::new("verified");
    let input = root.path().join("models.assets");
    let output = root.path().join("output");
    let first_model = b"MOC3\x63first-payload";
    let second_model = b"MOC3\x63second-payload";
    fs::write(
        &input,
        synthetic_models_file(first_model, second_model, true),
    )
    .unwrap();

    let result = cli(
        root.path(),
        [
            "live2d".into(),
            input.as_os_str().into(),
            output.as_os_str().into(),
        ],
    );
    assert_success(&result);

    assert_eq!(
        fs::read(output.join("_Same_Model.moc3")).unwrap(),
        first_model
    );
    assert_eq!(
        fs::read(output.join("_Same_Model @9.moc3")).unwrap(),
        second_model
    );
    let stdout = String::from_utf8_lossy(&result.stdout);
    let first_position = stdout.find("models.assets::7").unwrap();
    let second_position = stdout.find("models.assets::9").unwrap();
    assert!(first_position < second_position, "stdout: {stdout}");
    assert!(!stdout.contains("models.assets::10"), "stdout: {stdout}");
    assert!(
        stdout.contains("live2d summary: 2 exported, 0 failed"),
        "stdout: {stdout}"
    );
    assert_no_temporary_files(&output);
}

#[test]
fn live2d_ignores_an_unrelated_monobehaviour_and_reports_no_models() {
    let root = TestDirectory::new("unrelated");
    let input = root.path().join("ordinary.assets");
    let output = root.path().join("output");
    fs::write(&input, synthetic_models_file(b"unused", b"unused", false)).unwrap();

    let result = cli(
        root.path(),
        [
            "live2d".into(),
            input.as_os_str().into(),
            output.as_os_str().into(),
        ],
    );
    assert_success(&result);
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(
        stdout.contains("no CubismMoc models found"),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains("live2d summary: 0 exported, 0 failed, 0 bytes"),
        "stdout: {stdout}"
    );
    assert!(!output.exists());
}

#[test]
fn live2d_is_atomic_no_clobber_and_existing_outputs_are_partial_failures() {
    let root = TestDirectory::new("no-clobber");
    let input = root.path().join("models.assets");
    let output = root.path().join("output");
    let first_model = b"MOC3\x63first-payload";
    let second_model = b"MOC3\x63second-payload";
    fs::write(
        &input,
        synthetic_models_file(first_model, second_model, true),
    )
    .unwrap();

    let arguments = [
        "live2d".into(),
        input.as_os_str().into(),
        output.as_os_str().into(),
    ];
    assert_success(&cli(root.path(), arguments.clone()));
    let repeated = cli(root.path(), arguments);

    assert_eq!(repeated.status.code(), Some(3));
    assert_eq!(
        fs::read(output.join("_Same_Model.moc3")).unwrap(),
        first_model
    );
    assert_eq!(
        fs::read(output.join("_Same_Model @9.moc3")).unwrap(),
        second_model
    );
    let stdout = String::from_utf8_lossy(&repeated.stdout);
    let stderr = String::from_utf8_lossy(&repeated.stderr);
    assert!(
        stdout.contains("live2d summary: 0 exported, 2 failed, 0 bytes"),
        "stdout: {stdout}"
    );
    assert!(
        stderr.contains("live2d completed with 2 failure(s)"),
        "stderr: {stderr}"
    );
    assert_no_temporary_files(&output);
}

#[test]
fn live2d_usage_and_runtime_errors_have_stable_exit_codes() {
    let root = TestDirectory::new("errors");
    let output = root.path().join("output");

    let missing_output = cli(root.path(), ["live2d".into(), "input.assets".into()]);
    assert_eq!(missing_output.status.code(), Some(2));
    let unknown_option = cli(
        root.path(),
        [
            "live2d".into(),
            "--overwrite".into(),
            "input.assets".into(),
            output.as_os_str().into(),
        ],
    );
    assert_eq!(unknown_option.status.code(), Some(2));

    let missing_input = cli(
        root.path(),
        [
            "live2d".into(),
            root.path().join("missing.assets").into_os_string(),
            output.as_os_str().into(),
        ],
    );
    assert_eq!(missing_input.status.code(), Some(1));
    assert!(!output.exists());
}

#[cfg(unix)]
#[test]
fn live2d_rejects_a_symbolic_link_output_root() {
    use std::os::unix::fs::symlink;

    let root = TestDirectory::new("symlink");
    let input = root.path().join("models.assets");
    let real_output = root.path().join("real-output");
    let linked_output = root.path().join("linked-output");
    fs::create_dir_all(real_output.join("existing")).unwrap();
    symlink(&real_output, &linked_output).unwrap();
    fs::write(
        &input,
        synthetic_models_file(b"MOC3\x63first", b"MOC3\x63second", true),
    )
    .unwrap();

    let result = cli(
        root.path(),
        [
            "live2d".into(),
            input.as_os_str().into(),
            linked_output.join("existing/output").into_os_string(),
        ],
    );

    assert_eq!(result.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("symbolic-link"),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(!real_output.join("existing/output").exists());
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

fn assert_no_temporary_files(directory: &Path) {
    assert!(fs::read_dir(directory).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".assetstudio-live2d-")
    }));
}

fn synthetic_models_file(first_model: &[u8], second_model: &[u8], include_cubism: bool) -> Vec<u8> {
    let shared_name = "../Same:Model.moc3";
    let mut objects = Vec::new();
    if include_cubism {
        objects.push((9, 114, cubism_object(shared_name, second_model, 8)));
        objects.push((8, 115, mono_script("CubismMoc")));
        objects.push((7, 114, cubism_object(shared_name, first_model, 8)));
    }
    objects.push((
        10,
        114,
        cubism_object("ordinary", b"not a MOC3 payload", 11),
    ));
    objects.push((11, 115, mono_script("UnrelatedBehaviour")));
    synthetic_v22_objects(&objects)
}

fn cubism_object(name: &str, model: &[u8], script_path_id: i64) -> Vec<u8> {
    let mut object = Vec::new();
    push_pptr(&mut object, 0, 1);
    object.push(1);
    align(&mut object, 4);
    push_pptr(&mut object, 0, script_path_id);
    push_aligned_string(&mut object, name);
    object.extend_from_slice(&u32::try_from(model.len()).unwrap().to_le_bytes());
    object.extend_from_slice(model);
    object
}

fn mono_script(class_name: &str) -> Vec<u8> {
    let mut script = Vec::new();
    push_aligned_string(&mut script, "script");
    script.extend_from_slice(&0_i32.to_le_bytes());
    script.extend_from_slice(&[0x55; 16]);
    push_aligned_string(&mut script, class_name);
    push_aligned_string(&mut script, "Live2D.Cubism.Core");
    push_aligned_string(&mut script, "Live2D.Cubism.dll");
    script
}

fn synthetic_v22_objects(objects: &[(i64, i32, Vec<u8>)]) -> Vec<u8> {
    let mut metadata = Vec::new();
    metadata.extend_from_slice(b"2022.3.62f1\0");
    metadata.extend_from_slice(&13_i32.to_le_bytes());
    metadata.push(0);
    metadata.extend_from_slice(&2_i32.to_le_bytes());
    push_serialized_type(&mut metadata, 114);
    push_serialized_type(&mut metadata, 115);
    metadata.extend_from_slice(&i32::try_from(objects.len()).unwrap().to_le_bytes());

    let mut object_offsets = Vec::with_capacity(objects.len());
    let mut payload = Vec::new();
    for (_, _, object) in objects {
        align(&mut payload, 8);
        object_offsets.push(payload.len());
        payload.extend_from_slice(object);
    }
    for ((path_id, class_id, object), relative_start) in objects.iter().zip(object_offsets) {
        let type_index = i32::from(*class_id != 114);
        push_object_info(
            &mut metadata,
            *path_id,
            relative_start,
            object.len(),
            type_index,
        );
    }
    metadata.extend_from_slice(&0_i32.to_le_bytes());
    metadata.extend_from_slice(&0_i32.to_le_bytes());
    metadata.extend_from_slice(&0_i32.to_le_bytes());
    metadata.push(0);

    let metadata_size = u32::try_from(metadata.len()).unwrap();
    let metadata_end = 48_u64 + u64::from(metadata_size);
    let data_offset = metadata_end.div_ceil(16) * 16;
    let file_size = data_offset + u64::try_from(payload.len()).unwrap();
    let mut bytes = vec![0_u8; 48];
    bytes[8..12].copy_from_slice(&22_u32.to_be_bytes());
    bytes[16] = 0;
    bytes[20..24].copy_from_slice(&metadata_size.to_be_bytes());
    bytes[24..32].copy_from_slice(&i64::try_from(file_size).unwrap().to_be_bytes());
    bytes[32..40].copy_from_slice(&i64::try_from(data_offset).unwrap().to_be_bytes());
    bytes.extend_from_slice(&metadata);
    bytes.resize(usize::try_from(data_offset).unwrap(), 0);
    bytes.extend_from_slice(&payload);
    bytes
}

fn push_serialized_type(output: &mut Vec<u8>, class_id: i32) {
    output.extend_from_slice(&class_id.to_le_bytes());
    output.push(0);
    output.extend_from_slice(&(-1_i16).to_le_bytes());
    if class_id == 114 {
        output.extend_from_slice(&[0x11; 16]);
    }
    output.extend_from_slice(&[0x22; 16]);
}

fn push_object_info(
    output: &mut Vec<u8>,
    path_id: i64,
    relative_start: usize,
    byte_size: usize,
    type_index: i32,
) {
    align_with_base(output, 48, 4);
    output.extend_from_slice(&path_id.to_le_bytes());
    output.extend_from_slice(&i64::try_from(relative_start).unwrap().to_le_bytes());
    output.extend_from_slice(&u32::try_from(byte_size).unwrap().to_le_bytes());
    output.extend_from_slice(&type_index.to_le_bytes());
}

fn push_pptr(output: &mut Vec<u8>, file_id: i32, path_id: i64) {
    output.extend_from_slice(&file_id.to_le_bytes());
    output.extend_from_slice(&path_id.to_le_bytes());
}

fn push_aligned_string(output: &mut Vec<u8>, value: &str) {
    output.extend_from_slice(&i32::try_from(value.len()).unwrap().to_le_bytes());
    output.extend_from_slice(value.as_bytes());
    align(output, 4);
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
