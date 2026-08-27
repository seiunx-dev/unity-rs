# unity-rs

[![CI](https://github.com/seiunx-dev/unity-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/seiunx-dev/unity-rs/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

`unity-rs` is a headless Unity asset reader, inspector, extractor, and exporter
implemented in Rust. It provides one bounded parsing core and exposes it through:

- a reusable Rust library;
- a native command-line application;
- Python 3.9+ bindings built with PyO3;
- an optional Node.js binding built with napi-rs.

The normal build, installation, and runtime paths do not require .NET. There is
no GUI, custom C ABI, numeric context handle, or context lifecycle layer in this
repository. Rust, Python, and Node call the same high-level Core API directly.

The project is currently **Beta**. Its supported paths are extensively bounded,
tested, and compared with independent readers, but `unity-rs` does not claim to
understand every Unity, Tuanjie, console, or proprietary codec variant. Version
handling is two-tier: containers, per-class version floors, Tuanjie builds, and
stripped versions are rejected explicitly instead of guessed, while a standard
Unity version **above** a class's verified ceiling is parsed with the newest
known layout by default — a mismatch surfaces as an `Unsupported` error naming
the attempt, never as silent partial output. Pass `--strict-unity-versions`
(CLI), `strict_unity_versions=True` (Python), or `strictUnityVersions: true`
(Node) to refuse above-ceiling versions outright instead.

## Project status

The headless Rust/Python migration is complete. Compatibility work now proceeds
from real, independently verifiable samples: documented verified ranges move
only with fixtures, while the default runtime leniently attempts the newest
known layout above those ranges rather than refusing new engine releases
outright.

| Surface | Current identifier | Status |
| --- | --- | --- |
| Rust library | `unity-rs-core` | Primary, implemented |
| Native CLI | `unity-rs` / Cargo package `unity-rs-cli` | Implemented |
| Python distribution | `unity-rs`, import `unity_rs` | Primary, implemented (`cp39-abi3`) |
| Node.js package | `unity-rs-node` | Optional Beta |

The project and all first-party public identifiers now use the `unity-rs`
family. In language syntax that cannot contain a hyphen, the corresponding
identifiers are `unity_rs_core`, `unity_rs`, and `UnityRs`.

For the maintained compatibility matrix, evidence, and deferred sample-backed
work, see [REWRITE_STATUS.md](REWRITE_STATUS.md). Binding coverage is tracked in
[the Python API audit](docs/python-api-audit.md) and
[the Node API audit](docs/node-api-audit.md).

## Quick start

### Rust

From a Cargo project, import the core crate under the `unity_rs_core`
name:

```toml
[dependencies]
unity_rs_core = { package = "unity-rs-core", git = "https://github.com/seiunx-dev/unity-rs" }
```

```rust,no_run
use unity_rs_core::studio::Studio;

fn main() -> Result<(), unity_rs_core::Error> {
    let studio = Studio::open("game_Data")?;

    for object in studio.objects() {
        println!(
            "file={} path_id={} class_id={} name={}",
            object.file_index(),
            object.path_id(),
            object.class_id(),
            object.name().unwrap_or("<unnamed>"),
        );
    }

    let payload = studio
        .object(0, 7)
        .expect("object exists")
        .read_raw(512 * 1024 * 1024)?;
    println!("{} bytes", payload.len());
    Ok(())
}
```

### CLI

Run directly from the checkout:

```shell
cargo run -p unity-rs-cli -- info game_Data
cargo run -p unity-rs-cli -- list bundle.ab
cargo run -p unity-rs-cli -- export bundle.ab exported
cargo run -p unity-rs-cli -- extract bundle.ab unpacked
```

Common commands:

| Command | Purpose |
| --- | --- |
| `inspect` | Inspect container/file structure and failures |
| `info` / `list` | Summarize files or list serialized objects |
| `scene` | Inspect the resolved GameObject/Transform hierarchy |
| `export` | Export supported assets with bounded, atomic writes |
| `extract` | Recursively unpack supported containers safely |
| `obj` | Export a resolved scene as OBJ/MTL plus textures |
| `fbx` | Export a resolved scene as FBX 7.4 |
| `split-objects` | Export one FBX per managed-compatible GameObject candidate |
| `animator` | Export one FBX per Animator candidate |
| `live2d` | Export verified `.moc3` payloads |
| `live2d-package` | Materialize verified Live2D packages |

Use `cargo run -p unity-rs-cli -- --help` or
`cargo run -p unity-rs-cli -- <command> --help` for complete options and
budgets.

### Python

Build the abi3 extension from the checkout:

```shell
cd crates/unity-rs-python
maturin develop --locked --offline
```

```python
from unity_rs import UnityRs, ExportLimits, SceneLimits

studio = UnityRs("game_Data")

for obj in studio.iter_objects():
    print(obj.file_index, obj.path_id, obj.class_id, obj.name)

image = studio.read_texture(0, 28)
png_bytes = image.encode("png", compression="fast")
scene = studio.scene(limits=SceneLimits(maximum_game_objects=100_000))
report = studio.export(
    "exported",
    image_format="png",
    limits=ExportLimits(
        maximum_total_output_bytes=4 * 1024 * 1024 * 1024,
        maximum_metadata_bytes=256 * 1024 * 1024,
    ),
)
```

The wheel targets Python 3.9+ through `cp39-abi3`. The runtime module and its
`.pyi` are checked in both directions, and a strict Python 3.9 consumer is
type-checked in CI.

### Node.js

Build the optional addon from the checkout:

```shell
cd crates/unity-rs-node
npm install
npm run build:debug
```

```javascript
const { UnityRs } = require("unity-rs-node")

const studio = new UnityRs("game_Data")
console.log(studio.fileCount, studio.objectCount, studio.resourceCount)

const firstPage = studio.objectPage(0, 0, 256)
for (const object of firstPage) {
  console.log(object.pathId, object.classId, object.name)
}
```

Large asynchronous operations use napi-rs workers. Work that can scale with
asset input is completed in `Task::compute`; `resolve` only performs the final
Node-facing handoff.

## What is implemented

### Containers and serialized data

- `SerializedFile` v5-v22, little- and big-endian, 32/64-bit PathIDs, TypeTrees,
  external references, and source-bound object regions;
- UnityFS v6-v8 with None, LZ4/LZ4HC, LZMA, and Zstd blocks;
- UnityWeb/UnityRaw v1-v6 and UnityWebData/TuanjieWebData;
- gzip, Brotli, ZIP Stored/Deflate, and numeric `.split0`...`.splitN` inputs;
- recursive container/resource discovery, companion resources, and cross-file
  PPtr resolution;
- caller-keyed UnityCN decoding and caller-injected Oodle decoding without
  shipping proprietary key material or libraries.

### Assets and export

- TextAsset, TypeTree dump/JSON, raw objects, Font, MovieTexture, AudioClip,
  VideoClip, Shader, Material, BuildSettings, and PlayerSettings;
- Texture2D, Texture2DArray, Sprite, and SpriteAtlas, with bounded
  JPEG/PNG/BMP/TGA/lossless-WebP/raw-RGBA output;
- BC/DXT, ETC/EAC, ASTC, PVRTC, ATC, Crunch, Xbox 360, and verified Nintendo
  Switch mip-zero texture paths;
- Mesh and scene graph parsing, OBJ/MTL, ASCII/binary FBX 7.4, materials,
  textures, skin clusters, static blend shapes, and supported animation tracks;
- AnimationClip, Animation, AnimatorController, AnimatorOverrideController,
  Avatar, Animator/SplitObjects planning, and caller-injected ACL decoding;
- MonoScript/MonoBehaviour metadata and JSON via embedded TypeTrees or trusted,
  non-executing external schema documents;
- verified Cubism MOC, expression, pose, display-info, physics, fade-motion,
  AnimationClip motion, texture, and `model3.json` package materialization.

### High-level operations

- stable file/object/resource enumeration and bounded paging;
- safe recursive extraction;
- atomic no-clobber or requested-overwrite export;
- collection-wide scene, model, FBX, OBJ, and Live2D materialization;
- the same load options, error families, and resource budgets across Rust,
  Python, and the corresponding Node surface, including the strict-version
  opt-out (`--strict-unity-versions` / `strict_unity_versions` /
  `strictUnityVersions`).

For exact Unity version gates, codec exceptions, and differential evidence, use
the [full status document](REWRITE_STATUS.md) rather than treating this summary
as a promise for every engine build.

## Not tested areas

The following remain **Not tested** until real samples and an independent
oracle are available. Standard Unity versions above a class's verified ceiling
are also **Not tested**: they are attempted with the newest known layout by
default, and the stable `Unsupported` contract still holds whenever that layout
does not fit or strict mode is enabled:

- SerializedFile formats 1-4;
- UnityArchive payload parsing;
- Tuanjie and Unity 6 virtual-geometry cluster decoding;
- lower/stripped Nintendo Switch mip paths not described by verified layouts;
- a bundled pure-Rust Tuanjie ACL 2.x decoder;
- remaining platform-native texture/audio codecs without redistributable,
  sample-verified decoders.

These are tracked by the opt-in [real corpus gate](corpus/README.md). A corpus
case can run without a snapshot and require every object to parse, or compare a
versioned manifest against the checked managed oracle when that oracle is
available.

## Safety model

Unity files are untrusted input. `unity-rs` therefore treats offsets, sizes,
counts, strings, path components, and decompression work as bounded values.
Important invariants include:

- parsers read immutable source-bound `Region` values rather than shared mutable
  cursors;
- arithmetic, offsets, alignment, and allocation growth are checked;
- large result tables use fallible reservation and caller-controlled limits;
- nested containers charge input, expansion, file-count, path, and depth
  budgets;
- exports reject traversal, unsafe path components, and symbolic-link targets;
- output is prepared beside its destination and published atomically;
- Core forbids unsafe Rust; proprietary native decoders stay behind narrow,
  caller-supplied interfaces;
- Python releases the GIL around scalable Rust work, and Node worker tasks keep
  scalable projection off the event loop.

Malformed-input tests verify stable errors instead of panics. **Not tested**
paths are rejected through the `Unsupported` error family, separately from
corrupted data and failed I/O. The one lenient path — standard Unity versions
above a class's verified ceiling — reports its failures through the same
`Unsupported` family, with the inner diagnostic preserved.

## Workspace layout

```text
crates/unity-rs-core    Rust parsing/export core
crates/unity-rs-cli     Native command-line frontend
crates/unity-rs-python  PyO3 abi3 package
crates/unity-rs-node    Optional napi-rs package
corpus/                    Private real-asset acceptance harness
oracle/                    Checked managed differential harness
tools/                     Quality, packaging, corpus, and API audits
```

The workspace directory names match their Cargo and package identifiers.
Delivery-scope tests enforce that the workspace contains only the four headless
members above and that each frontend depends directly on Core. GUI, managed
runtime, old FFI source, and public context handles are rejected from shipped
artifacts.

## Build and verification

Minimum supported Rust version: **1.88**.

```shell
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo doc --workspace --no-deps --locked
```

The local CI runner mirrors the GitHub workflow:

```shell
python3 tools/local_ci.py --list
python3 tools/local_ci.py --fail-on-skip quality rust python node typing oracle
```

Additional opt-in groups cover cross-compilation and Linux release execution:

```shell
python3 tools/local_ci.py cross
python3 tools/local_ci.py linux
```

CI verifies:

- Rust build/tests on Linux, Windows, and macOS;
- Python abi3 wheels for Linux/Windows/macOS on x86-64 and ARM64;
- Node addons on Linux, Windows, and macOS;
- Rust crate, Python wheel/sdist, npm tarball, and native CLI contents;
- strict Clippy, rustdoc, RustSec, licenses, and headless delivery scope;
- differential behavior against the checked managed reader, UnityPy, and
  vgmstream where those comparisons are independent.

The managed oracle is optional and is never loaded by the runtime. To run it
from a checkout, set `UNITY_RS_ORACLE_REPO` to the managed repository path or
keep that checkout beside this repository:

```shell
UNITY_RS_ORACLE_REPO=/path/to/managed/oracle \
  cargo test -p unity-rs-core --test dotnet_oracle --locked -- --ignored
```

## Credits

`unity-rs` is an independent Rust implementation built on the Unity format
research, behavior, and long-running community work established by:

- [AssetStudioMod by aelurum](https://github.com/aelurum/AssetStudioMod)
- [AssetStudio by Perfare](https://github.com/Perfare/AssetStudio)

Their implementations remain important historical references and compatibility
oracles. This repository also uses independent differential evidence from
[UnityPy](https://github.com/K0lb3/UnityPy) and
[vgmstream](https://github.com/vgmstream/vgmstream) where appropriate.

Please consult [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md),
[THIRD_PARTY_LICENSES.txt](THIRD_PARTY_LICENSES.txt), and
[docs/upstream-defects.md](docs/upstream-defects.md) for vendored code,
dependency licenses, and locally corrected upstream behavior.

## License

`unity-rs` is distributed under the [MIT License](LICENSE).
