use std::ffi::OsString;
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
            "assetstudio-fbx-{label}-{}-{timestamp}-{sequence}",
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
fn fbx_exports_a_real_static_model_with_the_ascii_contract() {
    let root = TestDirectory::new("success");
    let input = root.path().join("model.assets");
    let destination = root.path().join("nested").join("model.fbx");
    let fixture = synthetic_model([0.0, 0.0, 0.0, 1.0]);
    fs::write(&input, &fixture).unwrap();

    let result = cli(
        root.path(),
        [
            "fbx".into(),
            input.as_os_str().into(),
            destination.as_os_str().into(),
        ],
    );
    assert_success(&result);
    let bytes = fs::read(&destination).unwrap();
    let text = std::str::from_utf8(&bytes).unwrap();
    assert!(text.starts_with("; FBX 7.4.0 project file\n"));
    assert!(text.contains("FBXVersion: 7400"));
    assert!(text.contains("P: \"Lcl Translation\", \"Lcl Translation\", \"\", \"A\",-2,3,4"));
    assert!(text.contains("a: 0,0,0,-1,0,0,0,1,0"));
    assert!(text.contains("PolygonVertexIndex: *3"));
    assert!(text.contains("a: 2,1,-1"));
    assert!(String::from_utf8_lossy(&result.stdout).contains("ASCII FBX 7.4"));
    assert!(String::from_utf8_lossy(&result.stdout).contains("0 animation clips"));
    assert_eq!(fs::read(&input).unwrap(), fixture);
    assert_no_fbx_temporary_files(destination.parent().unwrap());
    assert!(!root.path().join("ASExport").exists());
    assert!(!root.path().join("ASExtract").exists());
}

#[test]
fn obj_exports_the_model_with_a_companion_material_library() {
    let root = TestDirectory::new("obj-success");
    let input = root.path().join("model.assets");
    let destination = root.path().join("nested").join("model.obj");
    let fixture = synthetic_model([0.0, 0.0, 0.0, 1.0]);
    fs::write(&input, &fixture).unwrap();

    let result = cli(
        root.path(),
        [
            "obj".into(),
            input.as_os_str().into(),
            destination.as_os_str().into(),
        ],
    );
    assert_success(&result);

    let text = fs::read_to_string(&destination).unwrap();
    assert!(text.starts_with("mtllib model.mtl\r\n"), "{text}");
    // The node sits at Unity X=2, and OBJ mirrors X.
    assert!(text.contains("v -2 3 4\r\n"), "{text}");
    assert!(text.contains("usemtl "), "{text}");
    assert!(text.contains("f "), "{text}");

    // The library lands beside the OBJ under the same stem.
    let library = destination.parent().unwrap().join("model.mtl");
    let mtl = fs::read_to_string(&library).unwrap();
    assert!(mtl.contains("newmtl "), "{mtl}");
    assert!(mtl.contains("illum 2\r\n"), "{mtl}");

    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(stdout.contains("Wavefront OBJ"));
    assert!(stdout.contains("material library"));
    assert_eq!(fs::read(&input).unwrap(), fixture);
    assert_no_fbx_temporary_files(destination.parent().unwrap());
}

#[test]
fn obj_never_clobbers_an_existing_destination() {
    let root = TestDirectory::new("obj-clobber");
    let input = root.path().join("model.assets");
    let destination = root.path().join("model.obj");
    fs::write(&input, synthetic_model([0.0, 0.0, 0.0, 1.0])).unwrap();
    fs::write(&destination, b"keep me").unwrap();

    let result = cli(
        root.path(),
        [
            "obj".into(),
            input.as_os_str().into(),
            destination.as_os_str().into(),
        ],
    );
    assert!(!result.status.success());
    assert_eq!(fs::read(&destination).unwrap(), b"keep me");
    assert_no_fbx_temporary_files(root.path());
}

#[test]
fn fbx_budget_invalid_input_and_general_rotation_are_handled_atomically() {
    let root = TestDirectory::new("failures");
    let input = root.path().join("model.assets");
    let destination = root.path().join("budget.fbx");
    fs::write(&input, synthetic_model([0.0, 0.0, 0.0, 1.0])).unwrap();

    let budget = cli(
        root.path(),
        [
            "fbx".into(),
            "--maximum-output-bytes".into(),
            "64".into(),
            input.as_os_str().into(),
            destination.as_os_str().into(),
        ],
    );
    assert_eq!(budget.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&budget.stderr).contains("output limit"));
    assert!(!destination.exists());
    assert_no_fbx_temporary_files(root.path());

    let rotated_input = root.path().join("rotated.assets");
    let rotated_output = root.path().join("rotated.fbx");
    fs::write(&rotated_input, synthetic_model([0.0, 0.0, 0.5, 0.5])).unwrap();
    let rotated = cli(
        root.path(),
        [
            "fbx".into(),
            rotated_input.as_os_str().into(),
            rotated_output.as_os_str().into(),
        ],
    );
    assert_success(&rotated);
    assert!(
        std::str::from_utf8(&fs::read(&rotated_output).unwrap())
            .unwrap()
            .contains("P: \"Lcl Rotation\", \"Lcl Rotation\", \"\", \"A\",0,0,-90")
    );

    let invalid_input = root.path().join("invalid.assets");
    let invalid_output = root.path().join("invalid.fbx");
    fs::write(&invalid_input, synthetic_model([0.0, 0.0, 0.0, 0.0])).unwrap();
    let invalid = cli(
        root.path(),
        [
            "fbx".into(),
            invalid_input.as_os_str().into(),
            invalid_output.as_os_str().into(),
        ],
    );
    assert_eq!(invalid.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("zero-length quaternion"));
    assert!(!invalid_output.exists());
    assert_no_fbx_temporary_files(root.path());
}

#[test]
fn fbx_never_clobbers_an_existing_destination() {
    let root = TestDirectory::new("no-clobber");
    let input = root.path().join("model.assets");
    let destination = root.path().join("model.fbx");
    fs::write(&input, synthetic_model([0.0, 0.0, 0.0, 1.0])).unwrap();
    fs::write(&destination, b"sentinel").unwrap();

    let result = cli(
        root.path(),
        [
            "fbx".into(),
            input.as_os_str().into(),
            destination.as_os_str().into(),
        ],
    );
    assert_eq!(result.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&result.stderr).contains("refusing to overwrite"));
    assert_eq!(fs::read(&destination).unwrap(), b"sentinel");
    assert_no_fbx_temporary_files(root.path());
}

#[test]
fn split_objects_and_legacy_mode_export_independent_model_files() {
    let root = TestDirectory::new("split-objects");
    let input = root.path().join("model.assets");
    let output = root.path().join("split");
    fs::write(&input, synthetic_model([0.0, 0.0, 0.0, 1.0])).unwrap();

    let result = cli(
        root.path(),
        [
            "split-objects".into(),
            input.as_os_str().into(),
            output.as_os_str().into(),
        ],
    );
    assert_success(&result);
    let fbx = output.join("root.fbx");
    let original = fs::read(&fbx).unwrap();
    assert!(
        std::str::from_utf8(&original)
            .unwrap()
            .contains("FBXVersion: 7400")
    );
    assert!(String::from_utf8_lossy(&result.stdout).contains("1 exported"));

    let legacy_output = root.path().join("legacy");
    let legacy = cli(
        root.path(),
        [
            input.as_os_str().into(),
            "-m".into(),
            "splitObjects".into(),
            "-o".into(),
            legacy_output.as_os_str().into(),
        ],
    );
    assert_success(&legacy);
    assert!(legacy_output.join("root.fbx").is_file());

    let repeated = cli(
        root.path(),
        [
            "split-objects".into(),
            input.as_os_str().into(),
            output.as_os_str().into(),
        ],
    );
    assert_eq!(repeated.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&repeated.stdout).contains("refusing to overwrite"));
    assert_eq!(fs::read(&fbx).unwrap(), original);
    assert_no_fbx_temporary_files(&output);
}

#[test]
fn animator_batch_reports_no_candidates_without_creating_output() {
    let root = TestDirectory::new("animator-empty");
    let input = root.path().join("model.assets");
    let output = root.path().join("animators");
    fs::write(&input, synthetic_model([0.0, 0.0, 0.0, 1.0])).unwrap();

    let result = cli(
        root.path(),
        [
            "animator".into(),
            "--no-animations".into(),
            input.as_os_str().into(),
            output.as_os_str().into(),
        ],
    );
    assert_success(&result);
    assert!(String::from_utf8_lossy(&result.stdout).contains("no matching FBX models"));
    assert!(!output.exists());
}

#[test]
fn animator_batch_exports_the_owning_game_object_branch() {
    let root = TestDirectory::new("animator-success");
    let input = root.path().join("animator.assets");
    let output = root.path().join("animators");
    fs::write(&input, synthetic_animator_model()).unwrap();

    let result = cli(
        root.path(),
        [
            "animator".into(),
            input.as_os_str().into(),
            output.as_os_str().into(),
        ],
    );
    assert_success(&result);
    let fbx = fs::read_to_string(output.join("root.fbx")).unwrap();
    assert!(fbx.contains("FBXVersion: 7400"));
    assert!(fbx.contains("Geometry::tri"));
    assert!(String::from_utf8_lossy(&result.stdout).contains("1 exported"));
}

#[test]
fn fbx_usage_and_runtime_errors_are_distinct_and_have_no_implicit_outputs() {
    let root = TestDirectory::new("errors");
    for arguments in [
        vec!["fbx".into(), "input.assets".into()],
        vec!["fbx".into(), "input.assets".into(), "output.txt".into()],
        vec![
            "fbx".into(),
            "--maximum-output-bytes".into(),
            "0".into(),
            "input.assets".into(),
            "output.fbx".into(),
        ],
    ] {
        assert_eq!(cli(root.path(), arguments).status.code(), Some(2));
    }

    let destination = root.path().join("missing.fbx");
    let missing = cli(
        root.path(),
        [
            "fbx".into(),
            root.path().join("missing.assets").into_os_string(),
            destination.as_os_str().into(),
        ],
    );
    assert_eq!(missing.status.code(), Some(1));
    assert!(!destination.exists());
    assert_no_fbx_temporary_files(root.path());
    assert!(!root.path().join("ASExport").exists());
    assert!(!root.path().join("ASExtract").exists());
}

#[cfg(unix)]
#[test]
fn fbx_rejects_symbolic_link_output_ancestors() {
    use std::os::unix::fs::symlink;

    let root = TestDirectory::new("symlink");
    let input = root.path().join("model.assets");
    let real = root.path().join("real");
    let linked = root.path().join("linked");
    fs::write(&input, synthetic_model([0.0, 0.0, 0.0, 1.0])).unwrap();
    fs::create_dir(&real).unwrap();
    symlink(&real, &linked).unwrap();

    let result = cli(
        root.path(),
        [
            "fbx".into(),
            input.as_os_str().into(),
            linked.join("model.fbx").into_os_string(),
        ],
    );
    assert_eq!(result.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&result.stderr).contains("symbolic link"));
    assert!(!real.join("model.fbx").exists());
    assert_no_fbx_temporary_files(&real);
}

fn cli<I>(current_directory: &Path, arguments: I) -> Output
where
    I: IntoIterator<Item = OsString>,
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

fn assert_no_fbx_temporary_files(directory: &Path) {
    assert!(fs::read_dir(directory).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".assetstudio-fbx-")
    }));
}

fn synthetic_model(rotation: [f32; 4]) -> Vec<u8> {
    let objects = [
        (1, 1, game_object(&[11, 21, 31])),
        (4, 11, transform_object(rotation)),
        (33, 21, mesh_filter_object()),
        (23, 31, renderer_object()),
        (43, 51, mesh_object()),
    ];
    synthetic_v22(&objects)
}

fn synthetic_animator_model() -> Vec<u8> {
    let objects = [
        (1, 1, game_object(&[11, 21, 31, 41])),
        (4, 11, transform_object([0.0, 0.0, 0.0, 1.0])),
        (33, 21, mesh_filter_object()),
        (23, 31, renderer_object()),
        (43, 51, mesh_object()),
        (95, 41, animator_object()),
    ];
    synthetic_v22(&objects)
}

fn animator_object() -> Vec<u8> {
    let mut output = Vec::new();
    push_pptr(&mut output, 1);
    output.push(1);
    align(&mut output, 4);
    push_pptr(&mut output, 0);
    push_pptr(&mut output, 0);
    output
}

fn game_object(components: &[i64]) -> Vec<u8> {
    let mut output = Vec::new();
    push_i32(&mut output, i32::try_from(components.len()).unwrap());
    for component in components {
        push_pptr(&mut output, *component);
    }
    push_i32(&mut output, 0);
    push_aligned_string(&mut output, "root");
    output
}

fn transform_object(rotation: [f32; 4]) -> Vec<u8> {
    let mut output = Vec::new();
    push_pptr(&mut output, 1);
    push_f32s(&mut output, &rotation);
    push_f32s(&mut output, &[2.0, 3.0, 4.0]);
    push_f32s(&mut output, &[1.0, 1.0, 1.0]);
    push_i32(&mut output, 0);
    push_pptr(&mut output, 0);
    output
}

fn mesh_filter_object() -> Vec<u8> {
    let mut output = Vec::new();
    push_pptr(&mut output, 1);
    push_pptr(&mut output, 51);
    output
}

fn renderer_object() -> Vec<u8> {
    let mut output = Vec::new();
    push_pptr(&mut output, 1);
    output.extend_from_slice(&[1, 2, 1, 0, 0, 0, 0, 0, 0, 0]);
    align(&mut output, 4);
    output.extend_from_slice(&u32::MAX.to_le_bytes());
    push_i32(&mut output, 0);
    output.extend_from_slice(&[0_u8; 36]);
    push_i32(&mut output, 0);
    output.extend_from_slice(&[0_u8; 4]);
    for _ in 0..3 {
        push_pptr(&mut output, 0);
    }
    output.extend_from_slice(&[0_u8; 8]);
    align(&mut output, 4);
    output
}

fn mesh_object() -> Vec<u8> {
    let mut output = Vec::new();
    push_aligned_string(&mut output, "tri");
    push_i32(&mut output, 1);
    for value in [0_u32, 3, 0, 0, 0, 3] {
        push_u32(&mut output, value);
    }
    output.extend_from_slice(&[0_u8; 24]);
    for _ in 0..3 {
        push_i32(&mut output, 0);
    }
    push_u32(&mut output, 0);
    for _ in 0..5 {
        push_i32(&mut output, 0);
    }
    output.extend_from_slice(&[0, 1, 0, 0]);
    align(&mut output, 4);
    push_i32(&mut output, 0);
    push_i32(&mut output, 6);
    for index in 0..3_u16 {
        output.extend_from_slice(&index.to_le_bytes());
    }
    align(&mut output, 4);
    push_mesh_vertex_data(&mut output);
    push_mesh_tail(&mut output);
    output
}

fn push_mesh_vertex_data(output: &mut Vec<u8>) {
    push_u32(output, 3);
    push_i32(output, 5);
    output.extend_from_slice(&[0, 0, 0, 3]);
    for _ in 0..4 {
        output.extend_from_slice(&[0_u8; 4]);
    }
    push_i32(output, 36);
    for vertex in [[0.0_f32, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]] {
        push_f32s(output, &vertex);
    }
    align(output, 4);
}

fn push_mesh_tail(output: &mut Vec<u8>) {
    for _ in 0..4 {
        push_empty_packed_float(output);
    }
    for _ in 0..3 {
        push_empty_packed_int(output);
    }
    push_empty_packed_float(output);
    for _ in 0..2 {
        push_empty_packed_int(output);
    }
    push_u32(output, 0);
    output.extend_from_slice(&[0_u8; 24]);
    for _ in 0..3 {
        push_i32(output, 0);
    }
    align(output, 4);
    push_i32(output, 0);
    align(output, 4);
    output.extend_from_slice(&[0_u8; 8]);
    align(output, 4);
    output.extend_from_slice(&0_i64.to_le_bytes());
    push_u32(output, 0);
    push_aligned_string(output, "");
}

fn push_empty_packed_float(output: &mut Vec<u8>) {
    push_u32(output, 0);
    push_f32s(output, &[0.0, 0.0]);
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

fn synthetic_v22(objects: &[(i32, i64, Vec<u8>)]) -> Vec<u8> {
    let mut classes: Vec<i32> = objects.iter().map(|object| object.0).collect();
    classes.sort_unstable();
    classes.dedup();
    let mut metadata = Vec::new();
    metadata.extend_from_slice(b"2022.3.62f1\0");
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

fn finish_v22(metadata: &[u8], data: &[u8]) -> Vec<u8> {
    let metadata_size = u32::try_from(metadata.len()).unwrap();
    let data_offset = (48_u64 + u64::from(metadata_size)).next_multiple_of(16);
    let file_size = data_offset + u64::try_from(data.len()).unwrap();
    let mut output = vec![0_u8; 48];
    output[8..12].copy_from_slice(&22_u32.to_be_bytes());
    output[20..24].copy_from_slice(&metadata_size.to_be_bytes());
    output[24..32].copy_from_slice(&i64::try_from(file_size).unwrap().to_be_bytes());
    output[32..40].copy_from_slice(&i64::try_from(data_offset).unwrap().to_be_bytes());
    output.extend_from_slice(metadata);
    output.resize(usize::try_from(data_offset).unwrap(), 0);
    output.extend_from_slice(data);
    output
}

fn push_pptr(output: &mut Vec<u8>, path_id: i64) {
    push_i32(output, 0);
    output.extend_from_slice(&path_id.to_le_bytes());
}

fn push_i32(output: &mut Vec<u8>, value: i32) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn push_u32(output: &mut Vec<u8>, value: u32) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn push_f32s(output: &mut Vec<u8>, values: &[f32]) {
    for value in values {
        output.extend_from_slice(&value.to_le_bytes());
    }
}

fn push_aligned_string(output: &mut Vec<u8>, value: &str) {
    push_i32(output, i32::try_from(value.len()).unwrap());
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
