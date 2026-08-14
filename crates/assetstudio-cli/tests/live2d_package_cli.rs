use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const GAME_OBJECT: i32 = 1;
const TRANSFORM: i32 = 4;
const TEXTURE_2D: i32 = 28;
const MONO_BEHAVIOUR: i32 = 114;
const MONO_SCRIPT: i32 = 115;

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
            "assetstudio-live2d-package-{label}-{}-{timestamp}-{sequence}",
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
fn writes_exact_static_package_moc_manifest_and_unity_order_png() {
    let root = TestDirectory::new("exact");
    let input = root.path().join("input");
    let output = root.path().join("output");
    write_fixture(&input, Fixture::Complete);

    let result = cli(root.path(), package_arguments(&input, &output));

    assert_success(&result);
    let package = output.join("Hero");
    assert_eq!(fs::read(package.join("Hero.moc3")).unwrap(), b"MOC3\x09");
    assert_eq!(
        fs::read_to_string(package.join("Hero.model3.json")).unwrap(),
        concat!(
            "{\n",
            "  \"Version\": 3,\n",
            "  \"Name\": \"Hero\",\n",
            "  \"FileReferences\": {\n",
            "    \"Moc\": \"Hero.moc3\",\n",
            "    \"Textures\": [\n",
            "      \"textures/face.png\"\n",
            "    ],\n",
            // The managed document declares these five whether or not they
            // carry anything, so an absent reference is null and an empty
            // collection is empty rather than missing. This expectation used to
            // stop at Textures, which was this crate agreeing with itself.
            "    \"Physics\": null,\n",
            "    \"Pose\": null,\n",
            "    \"DisplayInfo\": null,\n",
            "    \"Motions\": {},\n",
            "    \"Expressions\": []\n",
            "  },\n",
            "  \"Groups\": [\n",
            // Every object and array is expanded and the file ends without a
            // trailing newline, because that is what Newtonsoft's
            // `Formatting.Indented` produces and the managed extractor writes
            // every one of these documents through it. The differential
            // compares these bytes now, not only what they parse to.
            "    {\n",
            "      \"Target\": \"Parameter\",\n",
            "      \"Name\": \"EyeBlink\",\n",
            "      \"Ids\": []\n",
            "    },\n",
            "    {\n",
            "      \"Target\": \"Parameter\",\n",
            "      \"Name\": \"LipSync\",\n",
            "      \"Ids\": []\n",
            "    }\n",
            "  ]\n",
            "}"
        )
    );
    let png = fs::read(package.join("textures/face.png")).unwrap();
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    // This exact deterministic PNG is a 1x2 image. Its display-order pixels are
    // top blue then bottom red, proving UnityDecoded row reversal was applied.
    assert_eq!(png, EXPECTED_FACE_PNG);
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(
        stdout.contains("live2d-package summary: 1 exported, 0 failed"),
        "stdout: {stdout}"
    );
    assert_no_work_files(&output);
}

#[test]
fn concurrent_publish_is_no_clobber_and_leaves_no_work_files() {
    let root = TestDirectory::new("concurrent");
    let input = root.path().join("input");
    let output = root.path().join("output");
    write_fixture(&input, Fixture::Complete);
    let arguments = package_arguments(&input, &output);

    let first = spawn_cli(root.path(), &arguments);
    let second = spawn_cli(root.path(), &arguments);
    let first = first.wait_with_output().unwrap();
    let second = second.wait_with_output().unwrap();

    let mut codes = [first.status.code(), second.status.code()];
    codes.sort_unstable();
    assert_eq!(codes, [Some(0), Some(3)]);
    assert_eq!(
        fs::read(output.join("Hero/Hero.moc3")).unwrap(),
        b"MOC3\x09"
    );
    assert_no_work_files(&output);
}

#[test]
fn existing_destination_and_decode_failure_do_not_publish_partial_packages() {
    let root = TestDirectory::new("atomic");
    let input = root.path().join("input");
    let output = root.path().join("output");
    write_fixture(&input, Fixture::Complete);
    fs::create_dir_all(output.join("Hero")).unwrap();
    fs::write(output.join("Hero/sentinel"), b"keep").unwrap();

    let existing = cli(root.path(), package_arguments(&input, &output));

    assert_eq!(existing.status.code(), Some(3));
    assert_eq!(fs::read(output.join("Hero/sentinel")).unwrap(), b"keep");
    assert!(!output.join("Hero/Hero.moc3").exists());
    assert_no_work_files(&output);

    fs::remove_dir_all(output.join("Hero")).unwrap();
    write_fixture(&input, Fixture::ShortTexture);
    let invalid = cli(root.path(), package_arguments(&input, &output));

    assert_eq!(invalid.status.code(), Some(3));
    assert!(!output.join("Hero").exists());
    assert_no_work_files(&output);
}

#[test]
fn schema_diagnostics_are_partial_but_no_package_is_zero_side_effect() {
    let root = TestDirectory::new("diagnostics");
    let input = root.path().join("input");
    let output = root.path().join("output");
    write_fixture(&input, Fixture::MissingModelTree);

    let diagnostic = cli(root.path(), package_arguments(&input, &output));

    assert_eq!(diagnostic.status.code(), Some(3));
    let stdout = String::from_utf8_lossy(&diagnostic.stdout);
    assert!(
        stdout.contains("MissingEmbeddedTypeTree"),
        "stdout: {stdout}"
    );
    assert!(stdout.contains("_moc"), "stdout: {stdout}");
    assert!(!output.exists());

    write_fixture(&input, Fixture::Empty);
    let empty = cli(root.path(), package_arguments(&input, &output));

    assert_success(&empty);
    assert!(String::from_utf8_lossy(&empty.stdout).contains("no verified Live2D packages found"));
    assert!(!output.exists());
}

#[test]
fn usage_and_runtime_errors_have_stable_exit_codes() {
    let root = TestDirectory::new("errors");
    let output = root.path().join("output");

    let usage = cli(
        root.path(),
        [OsString::from("live2d-package"), OsString::from("input")],
    );
    assert_eq!(usage.status.code(), Some(2));

    let runtime = cli(
        root.path(),
        package_arguments(&root.path().join("missing"), &output),
    );
    assert_eq!(runtime.status.code(), Some(1));
    assert!(!output.exists());
}

#[cfg(unix)]
#[test]
fn rejects_a_symbolic_link_in_the_output_path() {
    use std::os::unix::fs::symlink;

    let root = TestDirectory::new("symlink");
    let input = root.path().join("input");
    let real = root.path().join("real");
    let linked = root.path().join("linked");
    write_fixture(&input, Fixture::Complete);
    fs::create_dir(&real).unwrap();
    symlink(&real, &linked).unwrap();

    let result = cli(
        root.path(),
        package_arguments(&input, &linked.join("output")),
    );

    assert_eq!(result.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("symbolic-link"),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(!real.join("output").exists());
}

fn package_arguments(input: &Path, output: &Path) -> [OsString; 3] {
    [
        OsString::from("live2d-package"),
        input.as_os_str().to_owned(),
        output.as_os_str().to_owned(),
    ]
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

fn spawn_cli(current_directory: &Path, arguments: &[OsString]) -> Child {
    Command::new(env!("CARGO_BIN_EXE_assetstudio"))
        .current_dir(current_directory)
        .args(arguments)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
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

fn assert_no_work_files(directory: &Path) {
    assert!(fs::read_dir(directory).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".assetstudio-live2d-package-")
    }));
}

const EXPECTED_FACE_PNG: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 2, 8, 6, 0,
    0, 0, 153, 129, 182, 39, 0, 0, 0, 17, 73, 68, 65, 84, 120, 156, 99, 96, 96, 248, 255, 159, 225,
    63, 144, 4, 0, 17, 248, 3, 253, 233, 53, 210, 246, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96,
    130,
];

#[derive(Clone, Copy)]
enum Fixture {
    Complete,
    ShortTexture,
    MissingModelTree,
    Empty,
}

fn write_fixture(directory: &Path, fixture: Fixture) {
    if directory.exists() {
        fs::remove_dir_all(directory).unwrap();
    }
    fs::create_dir_all(directory).unwrap();
    if matches!(fixture, Fixture::Empty) {
        fs::write(directory.join("empty.assets"), synthetic_v22(&[], &[], &[])).unwrap();
        return;
    }

    let model_nodes = (!matches!(fixture, Fixture::MissingModelTree))
        .then(|| mono_behaviour_tree("CubismMoc", "_moc"));
    let types = vec![
        TestType::plain(GAME_OBJECT),
        TestType::plain(TRANSFORM),
        TestType::behaviour(0x20, model_nodes),
        TestType::behaviour(0x21, Some(mono_behaviour_tree("Texture2D", "_mainTexture"))),
        TestType::behaviour(0x30, None),
        TestType::plain(MONO_SCRIPT),
    ];
    let objects = vec![
        TestObject::new(1, 0, game_object("Hero", &[(0, 10), (0, 20)])),
        TestObject::new(10, 1, transform((0, 1), &[(0, 11)], (0, 0))),
        TestObject::new(20, 2, mono_behaviour((0, 1), (0, 100), "", (0, 30))),
        TestObject::new(2, 0, game_object("Drawables", &[(0, 11), (0, 21)])),
        TestObject::new(11, 1, transform((0, 2), &[], (0, 10))),
        TestObject::new(21, 3, mono_behaviour((0, 2), (0, 101), "", (1, 40))),
        TestObject::new(30, 4, cubism_moc_behaviour((0, 1), (0, 102))),
        TestObject::new(100, 5, mono_script("CubismModel")),
        TestObject::new(101, 5, mono_script("CubismRenderer")),
        TestObject::new(102, 5, mono_script("CubismMoc")),
    ];
    fs::write(
        directory.join("model.assets"),
        synthetic_v22(&types, &objects, &["archive:/textures.assets"]),
    )
    .unwrap();

    let texture_payload: &[u8] = if matches!(fixture, Fixture::ShortTexture) {
        &[255, 0, 0, 255, 0, 0, 255]
    } else {
        &[255, 0, 0, 255, 0, 0, 255, 255]
    };
    fs::write(
        directory.join("textures.assets"),
        synthetic_v22(
            &[TestType::plain(TEXTURE_2D)],
            &[TestObject::new(
                40,
                0,
                texture_object("face", texture_payload),
            )],
            &[],
        ),
    )
    .unwrap();
}

#[derive(Clone)]
struct TestType {
    class_id: i32,
    script_hash: Option<u8>,
    nodes: Option<Vec<TestNode>>,
}

impl TestType {
    fn plain(class_id: i32) -> Self {
        Self {
            class_id,
            script_hash: None,
            nodes: None,
        }
    }

    fn behaviour(script_hash: u8, nodes: Option<Vec<TestNode>>) -> Self {
        Self {
            class_id: MONO_BEHAVIOUR,
            script_hash: Some(script_hash),
            nodes,
        }
    }
}

struct TestObject {
    path_id: i64,
    type_index: usize,
    payload: Vec<u8>,
}

impl TestObject {
    fn new(path_id: i64, type_index: usize, payload: Vec<u8>) -> Self {
        Self {
            path_id,
            type_index,
            payload,
        }
    }
}

#[derive(Clone, Copy)]
struct TestNode {
    type_name: &'static str,
    field_name: &'static str,
    level: u8,
    align: bool,
}

const fn node(
    type_name: &'static str,
    field_name: &'static str,
    level: u8,
    align: bool,
) -> TestNode {
    TestNode {
        type_name,
        field_name,
        level,
        align,
    }
}

fn mono_behaviour_tree(pointer_type: &'static str, pointer_name: &'static str) -> Vec<TestNode> {
    vec![
        node("MonoBehaviour", "Base", 0, false),
        node("PPtr<GameObject>", "m_GameObject", 1, false),
        node("int", "m_FileID", 2, false),
        node("SInt64", "m_PathID", 2, false),
        node("UInt8", "m_Enabled", 1, true),
        node("PPtr<MonoScript>", "m_Script", 1, false),
        node("int", "m_FileID", 2, false),
        node("SInt64", "m_PathID", 2, false),
        node("string", "m_Name", 1, false),
        node("Array", "Array", 2, true),
        node("int", "size", 3, false),
        node("char", "data", 3, false),
        node(pointer_type, pointer_name, 1, false),
        node("int", "m_FileID", 2, false),
        node("SInt64", "m_PathID", 2, false),
    ]
}

fn game_object(name: &str, components: &[(i32, i64)]) -> Vec<u8> {
    let mut output = Vec::new();
    push_i32(&mut output, i32::try_from(components.len()).unwrap());
    for reference in components {
        push_pptr(&mut output, *reference);
    }
    push_i32(&mut output, 0);
    push_aligned_string(&mut output, name);
    output
}

fn transform(game_object: (i32, i64), children: &[(i32, i64)], father: (i32, i64)) -> Vec<u8> {
    let mut output = Vec::new();
    push_pptr(&mut output, game_object);
    for value in [0.0_f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0] {
        output.extend_from_slice(&value.to_le_bytes());
    }
    push_i32(&mut output, i32::try_from(children.len()).unwrap());
    for reference in children {
        push_pptr(&mut output, *reference);
    }
    push_pptr(&mut output, father);
    output
}

fn mono_behaviour(
    game_object: (i32, i64),
    script: (i32, i64),
    name: &str,
    field: (i32, i64),
) -> Vec<u8> {
    let mut output = mono_behaviour_prefix(game_object, script, name);
    push_pptr(&mut output, field);
    output
}

fn cubism_moc_behaviour(game_object: (i32, i64), script: (i32, i64)) -> Vec<u8> {
    let mut output = mono_behaviour_prefix(game_object, script, "moc");
    push_i32(&mut output, 5);
    output.extend_from_slice(b"MOC3\x09");
    output
}

fn mono_behaviour_prefix(game_object: (i32, i64), script: (i32, i64), name: &str) -> Vec<u8> {
    let mut output = Vec::new();
    push_pptr(&mut output, game_object);
    output.push(1);
    align(&mut output, 4);
    push_pptr(&mut output, script);
    push_aligned_string(&mut output, name);
    output
}

fn mono_script(class_name: &str) -> Vec<u8> {
    let mut output = Vec::new();
    push_aligned_string(&mut output, "Cubism script");
    push_i32(&mut output, 0);
    output.extend_from_slice(&[0x55; 16]);
    push_aligned_string(&mut output, class_name);
    push_aligned_string(&mut output, "Live2D.Cubism.Core");
    push_aligned_string(&mut output, "Live2D.Cubism.dll");
    output
}

fn texture_object(name: &str, pixels: &[u8]) -> Vec<u8> {
    let mut output = Vec::new();
    push_aligned_string(&mut output, name);
    push_i32(&mut output, 0);
    output.extend_from_slice(&[0, 0]);
    align(&mut output, 4);
    push_i32(&mut output, 1);
    push_i32(&mut output, 2);
    output.extend_from_slice(&u32::try_from(pixels.len()).unwrap().to_le_bytes());
    push_i32(&mut output, 0);
    push_i32(&mut output, 4);
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
    push_i32(&mut output, i32::try_from(pixels.len()).unwrap());
    output.extend_from_slice(pixels);
    output
}

fn synthetic_v22(types: &[TestType], objects: &[TestObject], externals: &[&str]) -> Vec<u8> {
    let mut metadata = Vec::new();
    metadata.extend_from_slice(b"2022.3.62f1\0");
    push_i32(&mut metadata, 13);
    metadata.push(1);
    push_i32(&mut metadata, i32::try_from(types.len()).unwrap());
    for test_type in types {
        push_type(&mut metadata, test_type);
    }

    let mut data = Vec::new();
    let mut records = Vec::new();
    for object in objects {
        align(&mut data, 4);
        records.push((
            object.path_id,
            data.len(),
            object.payload.len(),
            object.type_index,
        ));
        data.extend_from_slice(&object.payload);
    }
    push_i32(&mut metadata, i32::try_from(records.len()).unwrap());
    for (path_id, byte_start, byte_size, type_index) in records {
        align_with_base(&mut metadata, 48, 4);
        metadata.extend_from_slice(&path_id.to_le_bytes());
        metadata.extend_from_slice(&i64::try_from(byte_start).unwrap().to_le_bytes());
        metadata.extend_from_slice(&u32::try_from(byte_size).unwrap().to_le_bytes());
        push_i32(&mut metadata, i32::try_from(type_index).unwrap());
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
    let data_offset = (48_u64 + u64::from(metadata_size)).next_multiple_of(16);
    let file_size = data_offset + u64::try_from(data.len()).unwrap();
    let mut output = vec![0_u8; 48];
    output[8..12].copy_from_slice(&22_u32.to_be_bytes());
    output[20..24].copy_from_slice(&metadata_size.to_be_bytes());
    output[24..32].copy_from_slice(&i64::try_from(file_size).unwrap().to_be_bytes());
    output[32..40].copy_from_slice(&i64::try_from(data_offset).unwrap().to_be_bytes());
    output.extend_from_slice(&metadata);
    output.resize(usize::try_from(data_offset).unwrap(), 0);
    output.extend_from_slice(&data);
    output
}

fn push_type(output: &mut Vec<u8>, test_type: &TestType) {
    push_i32(output, test_type.class_id);
    output.push(0);
    output.extend_from_slice(&(-1_i16).to_le_bytes());
    if let Some(script_hash) = test_type.script_hash {
        output.extend_from_slice(&[script_hash; 16]);
    }
    output.extend_from_slice(&[0x42; 16]);
    if let Some(nodes) = &test_type.nodes {
        push_blob_tree(output, nodes);
    } else {
        push_i32(output, 0);
        push_i32(output, 0);
    }
    push_i32(output, 0);
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
        push_i32(output, i32::try_from(index).unwrap());
        push_i32(output, if node.align { 0x4000 } else { 0 });
        output.extend_from_slice(&0_u64.to_le_bytes());
    }
    output.extend_from_slice(&strings);
}

fn push_pptr(output: &mut Vec<u8>, reference: (i32, i64)) {
    push_i32(output, reference.0);
    output.extend_from_slice(&reference.1.to_le_bytes());
}

fn push_aligned_string(output: &mut Vec<u8>, value: &str) {
    push_i32(output, i32::try_from(value.len()).unwrap());
    output.extend_from_slice(value.as_bytes());
    if !value.is_empty() {
        align(output, 4);
    }
}

fn push_i32(output: &mut Vec<u8>, value: i32) {
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
