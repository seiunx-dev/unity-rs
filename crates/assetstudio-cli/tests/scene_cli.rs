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
            "assetstudio-scene-{label}-{}-{timestamp}-{sequence}",
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
fn scene_prints_stable_multi_game_object_tree_without_side_effects() {
    let root = TestDirectory::new("tree");
    let input = root.path().join("scene.assets");
    let original = synthetic_scene();
    fs::write(&input, &original).unwrap();
    let before = directory_entries(root.path());

    let result = cli(root.path(), ["scene".into(), input.as_os_str().into()]);
    assert_success(&result);
    let stdout = String::from_utf8(result.stdout).unwrap();
    let expected_tail = format!(
        concat!(
            "  serialized files: 1\n",
            "  nodes: 3\n",
            "  roots: 2\n",
            "    root f0:1 source=\"{}\" name=\"Root\"\n",
            "      transform component=f0:10 parent=null position=(1,2,3) rotation=(0,0,0,1) scale=(1,1,1)\n",
            "      mesh-filter component=f0:11 mesh=f0:30\n",
            "      animator component=f0:12 avatar=local(file=f0,path=40) controller=local(file=f0,path=41)\n",
            "      node f0:2 source=\"{}\" name=\"Child\"\n",
            "        transform component=f0:20 parent=f0:10 position=(4,5,6) rotation=(0,0,0,1) scale=(1,1,1)\n",
            "    root f0:3 source=\"{}\" name=\"Loose\"\n",
        ),
        input.display(),
        input.display(),
        input.display()
    );
    assert!(stdout.starts_with(&format!("scene {}\n", input.display())));
    assert!(stdout.ends_with(&expected_tail), "stdout:\n{stdout}");
    assert_eq!(directory_entries(root.path()), before);
    assert_eq!(fs::read(&input).unwrap(), original);
    assert!(!root.path().join("ASExport").exists());
    assert!(!root.path().join("ASExtract").exists());
}

#[test]
fn scene_usage_and_bad_input_have_stable_exit_codes() {
    let root = TestDirectory::new("errors");
    let missing_path = cli(root.path(), ["scene".into()]);
    assert_eq!(missing_path.status.code(), Some(2));

    let bad = root.path().join("bad.assets");
    let mut corrupt_object = Vec::new();
    push_i32(&mut corrupt_object, -1);
    let corrupt = synthetic_v22(&[(1, 1, corrupt_object)]);
    fs::write(&bad, corrupt).unwrap();
    let result = cli(root.path(), ["scene".into(), bad.as_os_str().into()]);
    assert_eq!(result.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&result.stderr).contains("assetstudio:"));
    assert_eq!(directory_entries(root.path()), vec!["bad.assets"]);
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

fn synthetic_scene() -> Vec<u8> {
    let objects = vec![
        (1, 1, game_object("Root", &[10, 11, 12])),
        (4, 10, transform(1, &[20], 0, [1.0, 2.0, 3.0])),
        (33, 11, mesh_filter(1, 30)),
        (95, 12, animator(1, 40, 41)),
        (1, 2, game_object("Child", &[20])),
        (4, 20, transform(2, &[], 10, [4.0, 5.0, 6.0])),
        (1, 3, game_object("Loose", &[])),
        (43, 30, named_object("Mesh")),
    ];
    synthetic_v22(&objects)
}

fn named_object(name: &str) -> Vec<u8> {
    let mut out = Vec::new();
    push_aligned_string(&mut out, name);
    out
}

fn game_object(name: &str, components: &[i64]) -> Vec<u8> {
    let mut out = Vec::new();
    push_i32(&mut out, i32::try_from(components.len()).unwrap());
    for component in components {
        push_pptr(&mut out, 0, *component);
    }
    push_i32(&mut out, 0);
    push_aligned_string(&mut out, name);
    out
}

fn transform(owner: i64, children: &[i64], father: i64, position: [f32; 3]) -> Vec<u8> {
    let mut out = Vec::new();
    push_pptr(&mut out, 0, owner);
    push_f32s(&mut out, &[0.0, 0.0, 0.0, 1.0]);
    push_f32s(&mut out, &position);
    push_f32s(&mut out, &[1.0, 1.0, 1.0]);
    push_i32(&mut out, i32::try_from(children.len()).unwrap());
    for child in children {
        push_pptr(&mut out, 0, *child);
    }
    push_pptr(&mut out, 0, father);
    out
}

fn mesh_filter(owner: i64, mesh: i64) -> Vec<u8> {
    let mut out = Vec::new();
    push_pptr(&mut out, 0, owner);
    push_pptr(&mut out, 0, mesh);
    out
}

fn animator(owner: i64, avatar: i64, controller: i64) -> Vec<u8> {
    let mut out = Vec::new();
    push_pptr(&mut out, 0, owner);
    out.push(1);
    align(&mut out, 4);
    push_pptr(&mut out, 0, avatar);
    push_pptr(&mut out, 0, controller);
    out
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
    for (class_id, path_id, object) in objects {
        align(&mut data, 4);
        let offset = i64::try_from(data.len()).unwrap();
        let type_index = classes.iter().position(|value| value == class_id).unwrap();
        records.push((*path_id, offset, object.len(), type_index));
        data.extend_from_slice(object);
    }
    push_i32(&mut metadata, i32::try_from(records.len()).unwrap());
    for (path_id, offset, length, type_index) in records {
        align_with_base(&mut metadata, 48, 4);
        metadata.extend_from_slice(&path_id.to_le_bytes());
        metadata.extend_from_slice(&offset.to_le_bytes());
        metadata.extend_from_slice(&u32::try_from(length).unwrap().to_le_bytes());
        push_i32(&mut metadata, i32::try_from(type_index).unwrap());
    }
    for _ in 0..3 {
        push_i32(&mut metadata, 0);
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

fn push_i32(out: &mut Vec<u8>, value: i32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_pptr(out: &mut Vec<u8>, file_id: i32, path_id: i64) {
    push_i32(out, file_id);
    out.extend_from_slice(&path_id.to_le_bytes());
}

fn push_f32s(out: &mut Vec<u8>, values: &[f32]) {
    for value in values {
        out.extend_from_slice(&value.to_le_bytes());
    }
}

fn push_aligned_string(out: &mut Vec<u8>, value: &str) {
    push_i32(out, i32::try_from(value.len()).unwrap());
    out.extend_from_slice(value.as_bytes());
    align(out, 4);
}

fn align(out: &mut Vec<u8>, alignment: usize) {
    while !out.len().is_multiple_of(alignment) {
        out.push(0);
    }
}

fn align_with_base(out: &mut Vec<u8>, base: usize, alignment: usize) {
    while !(base + out.len()).is_multiple_of(alignment) {
        out.push(0);
    }
}
