# Private real-asset corpus gate

This directory defines the opt-in differential gate for real Unity game data.
Game files and generated snapshots are deliberately not committed: they may be
large, proprietary, or locally licensed. Only the manifest schema and runner
belong in the repository.

1. Copy `manifest.example.json` to a private location.
2. Point each enabled case at an asset file, bundle, web container, split-file
   directory, or game data directory.
3. Generate the expected snapshot with the checked managed oracle:

   ```shell
   dotnet build rust/oracle/AssetStudioOracle.csproj \
     --configuration Release --framework net10.0 --nologo --verbosity quiet
   dotnet rust/oracle/bin/Release/net10.0/AssetStudioOracle.dll \
     /path/to/game_Data > /private/corpus/snapshots/game.json
   ```

4. Run the same input through Rust and compare the complete manifest:

   ```shell
   cd rust
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
synthetic TypeTree dump class used by the checked oracle test. Other classes
still participate through their object metadata and raw bytes, with a `null`
parsed payload.

`maximum_object_bytes` is a per-object materialization ceiling for the trusted
gate. Raise it explicitly for a known large sample instead of disabling bounds
globally. A manifest must have at least one enabled case when the ignored test
is executed.
