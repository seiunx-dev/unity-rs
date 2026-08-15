# Reading `MonoBehaviour` out of a release build

A `MonoBehaviour` is the one asset whose layout Unity does not have to write
down. For engine classes the serialized file carries a type tree describing
every field; for a script the tree is generated from the compiled assembly, and
a player build normally ships without it. What arrives instead is the engine
prefix — `m_GameObject`, `m_Enabled`, `m_Script`, `m_Name` — followed by a run
of bytes with no stated shape.

Without the shape those bytes cannot be read, and this reader declines them
rather than guessing. On one Unity 2022.3 title that is 172,192 objects, all
for this one reason. Getting them back means supplying the layout from
somewhere else.

## Where the layout comes from

The assembly the script was compiled into. Reading a game's managed assemblies
is a different trust decision from reading its data files, so nothing in the
Rust crates opens or executes one: the layout arrives as data, in a JSON
document, and `tools/monoschema` is a separate program that produces it.

```
dotnet build tools/monoschema/MonoSchemaGenerator.csproj
./tools/monoschema/bin/Debug/net10.0/monoschema <dll-directory> <unity-version> schema.json
```

The directory is anything holding the game's managed assemblies: a Mono build's
`Managed` directory, or the dummy DLL set an IL2CPP dump produces. The Unity
version decides the engine prefix — `m_PathID` is 32-bit before Unity 5 — so it
has to match the build. `--assembly <name>` narrows the walk, repeatable;
without it every assembly is converted, which for a large game is tens of
megabytes of JSON. Every generated entry is restricted to that exact Unity
version by default. `--unversioned` deliberately emits a cross-version fallback
document and should only be used after the same layout has been verified on
every version that will consume it.

Then hand the document to any command that opens a collection:

```
assetstudio --mono-schema schema.json export <input> <output>
```

`--class 114` narrows an export to `MonoBehaviour` alone, which is usually what
a schema was generated for.

`--mono-schema` is repeatable and the first document holding a class wins. An
object read through a schema is reported as `typetree_json_schema` rather than
`typetree_json`, because it is a weaker claim: it is only as good as the schema
it came from.

## The document

```json
{
  "version": 1,
  "generated_for": "6000.3.0f1",
  "entries": [
    {
      "assembly": "Assembly-CSharp",
      "namespace": "Game",
      "class": "Stats",
      "unity_version": "6000.3.0f1",
      "nodes": [
        { "level": 0, "type": "MonoBehaviour", "name": "Base" },
        { "level": 1, "type": "UInt8", "name": "m_Enabled", "meta_flags": 16384 },
        { "level": 1, "type": "SInt32", "name": "score" }
      ]
    }
  ]
}
```

Anything that can name a class and lay out its serialized fields can write
this; the generator is one producer, not the format's owner. `assembly` matches
with or without a `.dll` suffix, because a `MonoScript` in a shipped file
spells it `Fwk` while a generator walking a directory spells it `Fwk.dll`.
`unity_version` is optional in the document format and an entry without one
applies to every version. The generator writes it by default because the Unity
version changes both the engine prefix and user-field layout; accepting that
tree for another release would be a silent corruption risk. `--unversioned`
is the explicit opt-out for a verified shared layout. `generated_for` remains
an informational summary and the reader ignores it for matching. A node needs
`level`, `type` and `name`, and `meta_flags` carries Unity's align bit
(`0x4000`). A present `unity_version` must itself be a valid Unity version;
malformed or non-string values are rejected rather than becoming an accidental
global fallback.

## How a schema is checked

A schema is unverifiable against the build it was made for: the reader has
nothing to disagree with, so a wrong layout produces confident nonsense. The
check is to run it against a build that *does* still carry type trees — Unity
wrote those, the schema came from walking a DLL, and reading the same object
both ways is a real differential.

```
python3 tools/mono_schema_diff.py <bundle-directory> schema.json
```

`--mono-schema-override` is what makes this possible: it reads through the
schema even where the file carries its own tree. It exists for this check and
extraction should not use it — Unity's tree is the authority on Unity's file.

The tool holds the two readings to different standards on purpose. The
**values** must match exactly, in order; that is the real claim, and a schema
missing one four-byte field shifts everything after it. The **field names** may
differ and are only reported. A reconstructed tree names fields as the C#
source does and Unity does not always agree: `UnityEngine.Rect` serializes as
`x, y, width, height` while its fields are `m_XMin, m_YMin, m_Width,
m_Height`. Making that an error would only invite papering over the value
check.

Against all 2,777 Unity 6000.3 Addressables bundles of one game, with a schema
built from that game's dummy DLL set, 94,713 objects were read through a schema
and every one held the values Unity's own tree gives. 53,350 of them produced
byte-identical JSON; the other 41,363 differ only in field names, for the
reason above.

## What a dummy DLL set costs

An IL2CPP dump is not a faithful assembly, and two of its infidelities are
silent enough to be worth naming.

**Enums lose their base type.** A dummy DLL writes an enum deriving from its
underlying type — `System.Int32` — rather than from `System.Enum`. Cecil then
does not see an enum, Unity's serialization logic asks whether the field type
is a serializable value type, gets no, and drops the field. Nothing reports it:
the schema is four bytes short and the only symptom is a read running off the
end of the object much later. `UnityEngine.UI.ScrollRect` alone loses three
fields this way. The generator repairs it — an enum is still recognizable by
shape, one instance field named `value__` of an integral type — and on the
corpus above that was 1,680 enums. Measured on a 13-bundle slice: before the
repair 154 objects read through a schema and 14 hit a hard read error; after
it, 735 read through a schema and none failed.

**A missing assembly silently shortens a class.** If a field's type lives in an
assembly the directory does not contain, `Resolve()` fails and the field is
dropped with the same silence. The generator names those classes and fields on
stderr — nothing downstream can work out why a schema was short — and the Rust
reader still refuses the object, because its tree will not account for every
byte of it. That refusal is the backstop, and it caught all 35 remaining cases
on the corpus above.

## Limits

* Classes whose conversion throws — a field referencing a type no assembly in
  the directory defines — are skipped, counted, and named on stderr.
* Generic definitions, interfaces and abstract classes are not emitted: a
  generic definition has no single layout, and Unity does not serialize the
  other two as assets.
* An enum is emitted as `SInt32` regardless of its underlying type, matching
  the managed converter.
* A `SerializeReference` field is read through the file's own reference types
  rather than through the schema, so a generated schema does not have to
  describe one. What the schema must get right is where the registry sits in
  the object, and the converter places it as Unity does.
