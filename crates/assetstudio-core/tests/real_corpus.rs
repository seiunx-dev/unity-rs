//! Opt-in differential gate for private, versioned Unity asset corpora.
//!
//! The manifest points at local game data and checked managed-oracle JSON
//! snapshots; no proprietary asset bytes need to be committed to this repo.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

#[path = "support/oracle_manifest.rs"]
mod oracle_manifest;

use oracle_manifest::rust_manifest_with_version;

const CORPUS_MANIFEST_ENV: &str = "ASSETSTUDIO_CORPUS_MANIFEST";
const DEFAULT_MAXIMUM_OBJECT_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug)]
struct CorpusCase {
    name: String,
    input: PathBuf,
    /// The managed snapshot to compare against, when there is one.
    ///
    /// Optional because generating it needs the .NET SDK and an `AssetStudio`
    /// checkout, and requiring all three -- game files, a toolchain and a
    /// snapshot -- kept this gate unused. A case without one still reads every
    /// object and still fails on an error; it just cannot say the values are
    /// right, only that nothing broke.
    expected: Option<PathBuf>,
    maximum_object_bytes: u64,
    /// The version to read the input under when it does not carry its own.
    ///
    /// Shipping bundles often have the header version stripped -- two of the
    /// four real ones checked here say `5.x.x` -- and without this a case
    /// pointed at one fails on the first version-dependent object rather than
    /// on anything about the reader.
    unity_version: Option<String>,
    enabled: bool,
}

#[test]
fn example_corpus_manifest_is_valid() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/manifest.example.json");
    let cases = read_cases(&path).unwrap();
    assert_eq!(cases.len(), 3);
    assert!(cases.iter().all(|case| !case.enabled));
    // One case of each shape, so the example shows that the snapshot is
    // optional rather than leaving it to the prose.
    assert!(cases[0].expected.is_some());
    assert!(cases[1].expected.is_none());
    // And that a stripped-header bundle is pointed at a version rather than
    // being unusable, which is the shape a downloaded bundle actually has.
    assert!(cases[0].unity_version.is_none());
    assert_eq!(cases[2].unity_version.as_deref(), Some("2022.3.62f2"));
}

#[test]
#[ignore = "requires ASSETSTUDIO_CORPUS_MANIFEST and private Unity game assets"]
fn private_real_corpus_matches_managed_snapshots() {
    let path = std::env::var_os(CORPUS_MANIFEST_ENV)
        .map(PathBuf::from)
        .expect("ASSETSTUDIO_CORPUS_MANIFEST is not set");
    let cases = read_cases(&path).unwrap();
    let mut compared = 0_usize;
    let mut read_only = Vec::new();
    for case in cases.iter().filter(|case| case.enabled) {
        // Reading is the same work either way, and an error here fails the case
        // whether or not a snapshot exists.
        let actual = rust_manifest_with_version(
            &case.input,
            case.maximum_object_bytes,
            case.unity_version.as_deref(),
        )
        .unwrap_or_else(|error| panic!("corpus case {:?} could not be read: {error}", case.name));
        if let Some(expected) = &case.expected {
            let expected: Value = serde_json::from_slice(&fs::read(expected).unwrap()).unwrap();
            assert_eq!(actual, expected, "private corpus case {:?}", case.name);
            compared += 1;
        } else {
            // Without a snapshot there is nothing to compare values against,
            // so the case has to at least prove it read something: an input
            // the reader does not recognise parses as a resource file with no
            // objects and would otherwise pass while checking nothing.
            let (files, objects, _) = counts(&actual);
            assert!(
                files > 0 && objects > 0,
                "corpus case {:?} produced {files} file(s) and {objects} object(s); \
                 a case without a snapshot must at least parse to something",
                case.name
            );
            read_only.push((case.name.clone(), summarize(&actual)));
        }
    }
    assert!(
        compared + read_only.len() > 0,
        "private corpus manifest has no enabled cases"
    );
    for (name, summary) in &read_only {
        // Printed rather than asserted: without a snapshot there is nothing to
        // assert the values against, and saying so is more useful than a
        // silent pass that looks like a comparison.
        println!("corpus case {name:?}: read only, no snapshot -- {summary}");
    }
    println!(
        "corpus: {compared} case(s) compared against managed snapshots, \
         {} read without one",
        read_only.len()
    );
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
        let expected = entry
            .get("expected")
            .map(|value| {
                value
                    .as_str()
                    .map(|value| resolve_path(base, value))
                    .ok_or_else(|| format!("corpus case {index} expected is not a string"))
            })
            .transpose()?;
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
        let unity_version = entry
            .get("unity_version")
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| format!("corpus case {index} unity_version is not a string"))
            })
            .transpose()?;
        cases.push(CorpusCase {
            name,
            input,
            expected,
            maximum_object_bytes,
            unity_version,
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

/// Files, objects, and objects with a parsed payload.
fn counts(manifest: &Value) -> (usize, usize, usize) {
    let Some(files) = manifest["Files"].as_array() else {
        return (0, 0, 0);
    };
    let objects: Vec<&Value> = files
        .iter()
        .flat_map(|file| file["Objects"].as_array().map_or(&[][..], Vec::as_slice))
        .collect();
    let parsed = objects
        .iter()
        .filter(|object| !object["Payload"].is_null())
        .count();
    (files.len(), objects.len(), parsed)
}

/// What a manifest contains, so a read-only case reports something checkable
/// rather than just "no error".
fn summarize(manifest: &Value) -> String {
    let (files, objects, parsed) = counts(manifest);
    format!("{files} file(s), {objects} object(s), {parsed} with a parsed payload")
}
