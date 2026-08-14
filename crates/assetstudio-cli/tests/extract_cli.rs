use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(label: &str) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "assetstudio-cli-extract-{label}-{}-{timestamp}-{sequence}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        Self(fs::canonicalize(path).unwrap())
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
fn extracts_webdata_recursively_and_supports_legacy_syntax() {
    let root = TestDirectory::new("webdata");
    let input = root.path().join("fixture.data");
    let output = root.path().join("output");
    fs::write(
        &input,
        web_file(&[("folder/item.bin", b"native extraction")]),
    )
    .unwrap();

    let result = cli(
        root.path(),
        [
            "extract".into(),
            input.as_os_str().into(),
            output.as_os_str().into(),
        ],
    );
    assert_success(&result);
    assert!(
        String::from_utf8_lossy(&result.stdout)
            .contains("extract summary: 1 succeeded, 0 skipped, 0 failed")
    );
    assert_eq!(
        fs::read(output.join("fixture.data_unpacked/folder/item.bin")).unwrap(),
        b"native extraction"
    );

    let legacy_output = root.path().join("legacy-output");
    let legacy = cli(
        root.path(),
        [
            input.as_os_str().into(),
            "-m".into(),
            "extract".into(),
            "-o".into(),
            legacy_output.as_os_str().into(),
        ],
    );
    assert_success(&legacy);
    assert_eq!(
        fs::read(legacy_output.join("fixture.data_unpacked/folder/item.bin")).unwrap(),
        b"native extraction"
    );
}

#[test]
fn directory_partial_failure_uses_exit_three_and_keeps_safe_outputs() {
    let root = TestDirectory::new("partial");
    let input = root.path().join("input");
    let output = root.path().join("output");
    fs::create_dir_all(&input).unwrap();
    fs::write(input.join("a.data"), web_file(&[("good.bin", b"good")])).unwrap();
    fs::write(input.join("b.data"), web_file(&[("../escape.bin", b"bad")])).unwrap();

    let result = cli(
        root.path(),
        [
            "extract".into(),
            input.as_os_str().into(),
            output.as_os_str().into(),
        ],
    );

    assert_eq!(result.status.code(), Some(3));
    assert_eq!(
        fs::read(output.join("a.data_unpacked/good.bin")).unwrap(),
        b"good"
    );
    assert!(!root.path().join("escape.bin").exists());
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("extract completed with 1 failure(s)")
    );
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

fn web_file(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let signature = b"UnityWebData1.0\0";
    let directory_size = entries
        .iter()
        .map(|(path, _)| 12 + path.len())
        .sum::<usize>();
    let header_size = signature.len() + 4 + directory_size;
    let mut next_offset = header_size;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(signature);
    bytes.extend_from_slice(&i32::try_from(header_size).unwrap().to_le_bytes());
    for (path, payload) in entries {
        bytes.extend_from_slice(&i32::try_from(next_offset).unwrap().to_le_bytes());
        bytes.extend_from_slice(&i32::try_from(payload.len()).unwrap().to_le_bytes());
        bytes.extend_from_slice(&i32::try_from(path.len()).unwrap().to_le_bytes());
        bytes.extend_from_slice(path.as_bytes());
        next_offset += payload.len();
    }
    for (_, payload) in entries {
        bytes.extend_from_slice(payload);
    }
    bytes
}
