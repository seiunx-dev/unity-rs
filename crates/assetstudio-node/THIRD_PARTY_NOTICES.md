# Third-party notices for the Rust workspace

This document records the dependency set resolved by `Cargo.lock`. It is an
attribution aid, not a replacement for the license text distributed by each
package. Binary release archives carry the applicable texts in
`THIRD_PARTY_LICENSES.txt`, generated from the locked dependency graph and the
resolved crate sources.

| Packages | Resolved license expression |
| --- | --- |
| `adler2` | 0BSD OR MIT OR Apache-2.0 |
| `alloc-no-stdlib`, `alloc-stdlib` | BSD-3-Clause |
| `brotli` | BSD-3-Clause AND MIT |
| `brotli-decompressor` | BSD-3-Clause OR MIT |
| `autocfg`, `bitflags`, `cc`, `cfg-if`, `crc32fast`, `equivalent`, `find-msvc-tools`, `flate2`, `getrandom`, `hashbrown`, `image-webp`, `indexmap`, `itoa`, `jobserver`, `lazy_static`, `lewton`, `libc`, `log`, `num-complex`, `num-traits`, `paste`, `pkg-config`, `proc-macro2`, `quick-error`, `quote`, `regex-lite`, `serde`, `serde_core`, `serde_derive`, `serde_json`, `shlex`, `smallvec`, `syn`, `texture2ddecoder`, `tinyvec_macros`, `typed-path`, `zstd-safe`, `zstd-sys` | MIT OR Apache-2.0 |
| `bytemuck` | Zlib OR Apache-2.0 OR MIT |
| `lz4_flex`, `simd-adler32`, `zip`, `zmij`, `zstd` | MIT |
| `lzma-rust2` | Apache-2.0 |
| `jpeg-encoder` | (MIT OR Apache-2.0) AND IJG |
| `byteorder`, `byteorder-lite`, `memchr` | Unlicense OR MIT |
| `miniz_oxide` | MIT OR Zlib OR Apache-2.0 |
| `r-efi` | MIT OR Apache-2.0 OR LGPL-2.1-or-later |
| `unicode-ident` | (MIT OR Apache-2.0) AND Unicode-3.0 |
| `zune-core`, `zune-jpeg` (tests only) | MIT OR Apache-2.0 OR Zlib |
| `symphonia`, `symphonia-core`, `symphonia-bundle-mp3`, `symphonia-metadata` | MPL-2.0 |
| `ruopus` | MIT |
| `tinyvec` | Zlib OR Apache-2.0 OR MIT |

## Codec-specific provenance

- **Vendored copy.** `crates/assetstudio-core/src/vendor/texture2ddecoder/`
  carries that crate's ASTC and BC6H decoders, their helper modules, the MIT
  licence text and the upstream copyright notice. Two expressions are changed
  and marked `VENDOR FIX` in place; one unused `paste`-generated block is
  dropped and marked `VENDOR DELTA`. Everything else is the published source,
  so the copy diffs cleanly against it. The reason, the patch and the
  conditions for deleting the copy are in `docs/upstream-defects.md`. Every
  other format still comes from the crate itself.
- `texture2ddecoder 0.1.2` is a pure-Rust, `no_std` decoder distributed under
  MIT OR Apache-2.0. Its upstream notice attributes ATC and BCn code to
  AssetStudio's MIT native decoder, ASTC/ETC/PVRTC code to MIT sources, FP16
  code to MIT FP16, classic Crunch to the public domain, and Unity Crunch to
  Zlib. This workspace currently calls its ATC, ASTC, BC6/BC7, ETC/EAC, and
  PVRTC1 decoders, plus its classic Crunch and Unity Crunch entry points. CRN
  inputs are independently checked for exact size, header/data checksums,
  source-contained regions, dimensions, format, faces, mip offsets, and bounded
  palette/decoder allocations before entering those allocation-backed decoders.
- The Crunch differential fixtures are CRN encodings of the CC0 Ferris artwork
  obtained from the exact upstream source commit recorded by
  `texture2ddecoder 0.1.2`. See the fixture README for provenance. They are
  retained only for complete-output comparisons with AssetStudio's bundled C++
  decoder.
- `zstd-sys` builds the upstream Zstandard C implementation through the
  `zstd` crate. The bundled upstream implementation is BSD-3-Clause; its
  license is shipped separately inside the crate in addition to the wrapper's
  MIT/Apache-2.0 terms. It remains outside the workspace's
  `unsafe_code = "forbid"` boundary and must stay pinned and audited as an FFI
  dependency.
- The workspace selects pure-Rust backends for gzip/deflate, Brotli, LZ4, and
  LZMA. Lossless WebP encoding uses the pure-Rust, unsafe-forbidden
  `image-webp` encoder. JPEG encoding uses `jpeg-encoder` with its optional SIMD
  feature disabled, which makes that crate forbid unsafe code; its IJG-derived
  transform carries the additional IJG license shipped in the crate. JPEG
  semantic tests use `zune-jpeg` with its architecture-specific unsafe features
  disabled. Oodle remains an injected interface; proprietary Oodle libraries
  are neither linked nor redistributed by this Rust workspace.
- FSB5 FMOD/Xbox IMA-ADPCM, Nintendo DSP/GC-ADPCM, Sony VAG/PS-ADPCM and
  HEVAG, and FMOD FADPCM decoding is implemented directly in this workspace.
  Mono/stereo MPEG Layer II/III decoding uses Symphonia 0.6.1, whose facade,
  core, MPEG bundle, and metadata crates are pure Rust, forbid unsafe code, and
  are distributed under MPL-2.0. AssetStudio validates and removes FMOD's
  per-frame padding before handing each bounded MPEG frame to Symphonia.
  FSB Opus decoding uses `ruopus 0.1.2`, a pure-Rust RFC 6716 implementation
  distributed under MIT; its default optional acceleration and Python features
  are disabled. AssetStudio validates the FSB packet framing, channel/rate
  contract, fixed 312-frame encoder delay, output size, and trailing padding
  before decoding.
  FSB Vorbis decoding uses the pure-Rust `lewton 0.10.2` decoder. FSB stores a
  CRC reference instead of the Vorbis setup packet, so the workspace embeds a
  compact table of 161 setup packets mechanically generated by
  `tools/build_fsb_vorbis_table.rs` from `fsbex 0.3.0`, which is distributed
  under MIT OR Apache-2.0. The workspace selects its MIT option; the upstream
  copyright and complete permission notice ship beside the Core crate in
  `FSB_VORBIS_NOTICE.md`. The generated table is bounds checked before use.
  A system `libvorbis 1.3.7` encoder was used only to create the non-silent
  differential fixture documented alongside it; libvorbis is not a build or
  runtime dependency and is not redistributed here.
  Ignored differential tests can invoke a separately installed
  `vgmstream-cli` as an output oracle; vgmstream code or binaries are not
  linked, vendored, or redistributed by the workspace.

## Reference implementations

Nothing in this section is a dependency of the shipped crates. These projects
are consulted for format knowledge and executed as compatibility oracles.

- [`Team-Haruki/AssetStudio`](https://github.com/Team-Haruki/AssetStudio) (MIT)
  is the primary oracle. The differential gate builds its managed reader and
  compares complete manifests; see `oracle/` and
  `crates/assetstudio-core/tests/dotnet_oracle.rs`.
- [`K0lb3/UnityPy`](https://github.com/K0lb3/UnityPy) (MIT) is a second,
  independent implementation used the same way. Two caveats govern how much its
  agreement is worth. Its pixel decoding calls the same upstream
  `texture2ddecoder` this workspace already links, so a texture comparison
  tests this crate's mip, swizzle and format dispatch and says nothing about
  the shared decoder underneath. Its `helpers/MeshHelper.py` and
  `export/ShaderConverter.py` are transliterations of AssetStudio, so they are
  not independent evidence for mesh or shader behaviour either.
- UnityPy's `helpers/ArchiveStorageManager.py` credits
  [`Razmoth/PGRStudio`](https://github.com/Razmoth/PGRStudio) for the UnityCN
  archive-storage scheme. `crates/assetstudio-core/src/unity_cn.rs` implements
  that scheme from the behavioural description rather than by transliteration,
  including its own AES-128 encryption path so no cryptography dependency is
  taken, and this workspace ships no key material. The scheme itself is
  reverse-engineered format behaviour.
- UnityPy's `resources/lzma.tpk` is not its own work: it is generated by
  AssetRipper/Tpk from TypeTreeDumps and redistributed. It is not vendored
  here, and its terms would need clearing before any redistribution.

## Maintenance notes

The published `alloc-stdlib 0.2.4` crate and the split napi-rs crates used by
the Node binding do not carry a top-level license file in their Cargo source
archives. Reviewed copies from their official upstream repositories are pinned
under `tools/legal-fallbacks/`; the generator maps them only to the exact
versions in `Cargo.lock` and fails closed when one of those versions changes.

`paste 1.0.15` is a build-time procedural-macro dependency of
`texture2ddecoder`. RustSec RUSTSEC-2024-0436 classifies it as unmaintained; the
advisory is informational and reports no vulnerability. It is retained only as
a transitive build dependency until `texture2ddecoder` migrates to a maintained
compatible macro crate or removes the generated decoder wrappers.

Regenerate and review this file whenever `Cargo.lock` changes. In particular,
do not assume that a semver-compatible update preserves licenses, native-code
content, safety properties, or redistribution terms.
