//! Opt-in differential gate for private, versioned Unity asset corpora.
//!
//! The manifest points at local game data and checked managed-oracle JSON
//! snapshots; no proprietary asset bytes need to be committed to this repo.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

#[path = "support/oracle_manifest.rs"]
mod oracle_manifest;

use oracle_manifest::rust_manifest;

const CORPUS_MANIFEST_ENV: &str = "ASSETSTUDIO_CORPUS_MANIFEST";
const DEFAULT_MAXIMUM_OBJECT_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug)]
struct CorpusCase {
    name: String,
    input: PathBuf,
    expected: PathBuf,
    maximum_object_bytes: u64,
    enabled: bool,
}

#[test]
fn example_corpus_manifest_is_valid() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/manifest.example.json");
    let cases = read_cases(&path).unwrap();
    assert_eq!(cases.len(), 1);
    assert!(!cases[0].enabled);
}

#[test]
#[ignore = "requires ASSETSTUDIO_CORPUS_MANIFEST and private Unity game assets"]
fn private_real_corpus_matches_managed_snapshots() {
    let path = std::env::var_os(CORPUS_MANIFEST_ENV)
        .map(PathBuf::from)
        .expect("ASSETSTUDIO_CORPUS_MANIFEST is not set");
    let cases = read_cases(&path).unwrap();
    let mut executed = 0_usize;
    for case in cases.iter().filter(|case| case.enabled) {
        let expected: Value = serde_json::from_slice(&fs::read(&case.expected).unwrap()).unwrap();
        let actual = rust_manifest(&case.input, case.maximum_object_bytes).unwrap();
        assert_eq!(actual, expected, "private corpus case {:?}", case.name);
        executed += 1;
    }
    assert!(executed > 0, "private corpus manifest has no enabled cases");
}

fn read_cases(path: &Path) -> Result<Vec<CorpusCase>, Box<dyn std::error::Error>> {
    let document: Value = serde_json::from_slice(&fs::read(path)?)?;
    let root = document
        .as_object()
        .ok_or("corpus manifest root must be a JSON object")?;
    if required_u64(root, "schema")? != 1 {
        return Err("unsupported corpus manifest schema".into());
    }
    let entries = root
        .get("cases")
        .and_then(Value::as_array)
        .ok_or("corpus manifest cases must be an array")?;
    let base = path.parent().ok_or("corpus manifest has no parent")?;
    let mut cases = Vec::new();
    cases.try_reserve_exact(entries.len())?;
    for (index, entry) in entries.iter().enumerate() {
        let entry = entry
            .as_object()
            .ok_or_else(|| format!("corpus case {index} must be an object"))?;
        let name = required_string(entry, "name", index)?;
        let input = resolve_path(base, &required_string(entry, "input", index)?);
        let expected = resolve_path(base, &required_string(entry, "expected", index)?);
        let maximum_object_bytes = entry
            .get("maximum_object_bytes")
            .map(|value| {
                value
                    .as_u64()
                    .ok_or_else(|| format!("corpus case {index} maximum_object_bytes is not u64"))
            })
            .transpose()?
            .unwrap_or(DEFAULT_MAXIMUM_OBJECT_BYTES);
        let enabled = entry
            .get("enabled")
            .map(|value| {
                value
                    .as_bool()
                    .ok_or_else(|| format!("corpus case {index} enabled is not boolean"))
            })
            .transpose()?
            .unwrap_or(true);
        cases.push(CorpusCase {
            name,
            input,
            expected,
            maximum_object_bytes,
            enabled,
        });
    }
    Ok(cases)
}

fn required_u64(
    object: &Map<String, Value>,
    field: &str,
) -> Result<u64, Box<dyn std::error::Error>> {
    object
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("corpus manifest {field} must be u64").into())
}

fn required_string(
    object: &Map<String, Value>,
    field: &str,
    index: usize,
) -> Result<String, Box<dyn std::error::Error>> {
    object
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("corpus case {index} {field} must be a string").into())
}

fn resolve_path(base: &Path, value: &str) -> PathBuf {
    let path = Path::new(value);
    if path.is_absolute() {
        path.to_owned()
    } else {
        base.join(path)
    }
}
