# Private real-asset corpus gate

This directory defines the opt-in differential gate for real Unity game data.
Game files and generated snapshots are deliberately not committed: they may be
large, proprietary, or locally licensed. Only the manifest schema and runner
belong in the repository.

1. Copy `manifest.example.json` to a private location.
2. Point each enabled case at an asset file, bundle, web container, split-file
   directory, or game data directory.
3. Optionally generate the expected snapshot with the checked managed oracle.

   A case may omit `expected`, in which case the gate reads every object and
   fails on an error but has nothing to compare values against; it reports what
   it read and requires the case to parse to at least one object, so an input
   the reader does not recognise -- which parses as a resource file with no
   objects -- fails rather than passing quietly. That mode needs only the game
   files. Comparing values needs the snapshot below, and the snapshot needs the
   .NET SDK and an AssetStudio checkout.

   To generate one:

   ```shell
   dotnet build oracle/AssetStudioOracle.csproj \
     --configuration Release --framework net10.0 --nologo --verbosity quiet
   dotnet oracle/bin/Release/net10.0/AssetStudioOracle.dll \
     /path/to/game_Data > /private/corpus/snapshots/game.json
   ```

4. Run the input through Rust, comparing against the snapshot where a case has
   one:

   ```shell
   ASSETSTUDIO_CORPUS_MANIFEST=/private/corpus/manifest.json \
     cargo test -p assetstudio-core --test real_corpus --locked \
       -- --ignored --nocapture
   ```

The snapshot fixes serialized-file order, Unity versions, object-table order,
signed path IDs, class IDs, byte sizes, resolved names, raw object FNV-1a
hashes, discovered external-resource filenames and streaming byte hashes, and
supported parsed payload metadata/hashes. Current parsed probes cover
`Material` texture/scalar/color properties, `Texture2D`, resident `Mesh`
vertex/normal/UV/index data, resolved and cropped `Sprite` RGBA pixels,
legacy direct `Shader` output, `TextAsset`, `AudioClip`, `Font`, legacy
`MovieTexture`, `VideoClip`, Unity 6000.2 `AnimationClip` split streamed-count
layout, Unity 6000.1 resident and external-stream `Mesh` geometry, Unity 6000.2 `AnimatorController`
tail/reference alignment, the Unity
6000.2 `Avatar` prefix through TOS (the managed reader
does not consume its HumanDescription tail), `BuildSettings`, `PlayerSettings`, and the
synthetic TypeTree dump class used by the checked oracle test. `MonoBehaviour`
objects additionally carry their Cubism projections where the file's own
`TypeTree` describes one -- physics3.json, motion3.json, exp3.json -- and a
`CubismMoc` behaviour carries its MOC3 header fields. Other classes still
participate through their object metadata and raw bytes, with a `null` parsed
payload.

A snapshot also carries a `Live2D` section: every file a Live2D package would
be written as, keyed by relative path, with JSON documents compared as values
and other files by size and hash. On the managed side that comes from running
the real `Live2DExtractor`, so a corpus case containing a Cubism model compares
the whole package rather than one document at a time.

Regenerate snapshots after updating this repository. The manifest has grown
several times -- most recently with the Cubism and Live2D rows above -- and an
older snapshot will differ from a current run for that reason rather than
because anything is wrong.

`maximum_object_bytes` is a per-object materialization ceiling for the trusted
gate, and also bounds the Live2D section's file size and the total across
**every package in the case** -- not per package. A case covering thousands of
bundles therefore needs a budget in the gigabytes, and under-declaring it fails
partway through a texture rather than at the object that was too big. Raise it
explicitly for a known large sample instead of disabling bounds globally. A manifest must have at least one enabled case when the ignored test
is executed.
