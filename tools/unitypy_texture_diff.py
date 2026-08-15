#!/usr/bin/env python3
"""Compares decoded textures against UnityPy's, and bounds how far they may differ.

`crates/assetstudio-python/tests/unitypy_oracle.py` compares the
serialized-file layer and deliberately stops there, on the grounds that UnityPy
decodes textures with the same `texture2ddecoder` crate this crate depends on,
so agreement would prove nothing. That reasoning does not survive contact with
what UnityPy actually does: for ASTC it uses `astc_encoder`, a binding to ARM's
reference codec, with `USE_DECODE_UNORM8`. For those formats it is a genuinely
independent implementation, and the comparison is worth running.

What it checks:

* a format that needs no decoding -- `RGBA32` and friends -- must match byte
  for byte, since neither side is doing anything but moving pixels;
* ASTC may differ, but by at most one level per channel. Two correct
  implementations of the same block format can land either side of a half;
  more than that is a decode defect. Alpha is included: an ASTC block decodes
  all four channels through the same endpoint arithmetic, and an `ASTC_RGB_*`
  texture can still carry a varying alpha -- one in the corpus this was
  written against holds values 0, 1, 2 and 4 rather than a constant 255;
* everything else must match exactly, and a differing pixel count anywhere is
  reported.

This is a bound, not a proof of correctness, and it is not evidence for the
vendored decoder fixes in `docs/upstream-defects.md` -- those are held to exact
agreement against the managed decoder by the differential, which is a much
stronger statement than being within one level of anything.

Two shapes have to line up before the pixels can be compared, and getting
either wrong reports every pixel of every texture as different -- which is what
this script did on both of its first two runs. The raw-RGBA export carries a
36-byte IR header before the pixels, and it has already flipped Unity's
bottom-up rows into display order, which is the order UnityPy's `.image` is in.
So the header is skipped and no second flip is applied.

    python3 tools/unitypy_texture_diff.py <directory-of-bundles> [limit] [unity-version]

A shipping bundle often has its header version stripped, and then both sides
need to be told what it is: this crate through `--unity-version`, UnityPy
through `FALLBACK_UNITY_VERSION`.

Needs UnityPy in the interpreter running it -- the virtualenv `tools/local_ci.py`
builds has it -- and a corpus of real bundles, which is why this is a tool rather
than a test.
"""

from __future__ import annotations

import collections
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
RGBA_IR_HEADER_BYTES = 36

# The formats UnityPy hands to ARM's reference codec rather than to the same
# crate this one uses. Only these get the one-level allowance, because only
# these are two independent implementations of the same block format.
INDEPENDENTLY_DECODED = {
    48, 49, 50, 51, 52, 53,  # ASTC RGB 4x4 .. 12x12
    54, 55, 56, 57, 58, 59,  # ASTC RGBA 4x4 .. 12x12
    66, 67, 68, 69, 70, 71,  # ASTC HDR 4x4 .. 12x12
}


def decoded_by_this_crate(
    bundle: Path, output: Path, unity_version: str | None
) -> dict[int, bytes]:
    """Exports every texture as raw RGBA, keyed by path ID."""
    command = ["cargo", "run", "--release", "--quiet", "-p", "assetstudio-cli", "--locked", "--"]
    if unity_version:
        command += ["--unity-version", unity_version]
    command += ["export", "--filename", "path-id", "--image-format", "raw-rgba",
                str(bundle), str(output)]
    result = subprocess.run(command, cwd=ROOT, capture_output=True, text=True)
    if result.returncode != 0:
        print(f"  {bundle.name}: export failed: {result.stderr.strip()[:200]}", file=sys.stderr)
        return {}
    # The raw-RGBA IR starts with a 36-byte header: a 16-byte magic, then
    # width, height, stride and the pixel length.
    return {
        int(path.stem): path.read_bytes()[RGBA_IR_HEADER_BYTES:]
        for path in output.rglob("*.rgba")
        if path.stem.lstrip("-").isdigit()
    }


def main() -> int:
    if len(sys.argv) not in (2, 3, 4):
        print(__doc__)
        return 2
    directory = Path(sys.argv[1])
    limit = int(sys.argv[2]) if len(sys.argv) >= 3 else 0
    unity_version = sys.argv[3] if len(sys.argv) == 4 else None

    import UnityPy  # noqa: PLC0415  -- optional, and only needed here

    if unity_version:
        UnityPy.config.FALLBACK_UNITY_VERSION = unity_version

    bundles = sorted(directory.glob("*.bundle")) or sorted(
        p for p in directory.iterdir() if p.is_file()
    )
    if limit:
        bundles = bundles[:limit]

    stats: dict[int, collections.Counter] = collections.defaultdict(collections.Counter)
    problems: list[str] = []
    compared = 0

    with tempfile.TemporaryDirectory(prefix="assetstudio-texdiff-") as work:
        for bundle in bundles:
            output = Path(work) / bundle.stem
            ours = decoded_by_this_crate(bundle, output, unity_version)
            if not ours:
                continue
            try:
                env = UnityPy.load(str(bundle))
            except Exception as error:  # noqa: BLE001 -- a corpus file, not our input
                problems.append(f"{bundle.name}: UnityPy could not load it: {error}")
                continue
            for obj in env.objects:
                if obj.class_id != 28 or obj.path_id not in ours:
                    continue
                try:
                    image = obj.read().image.convert("RGBA")
                    fmt = int(obj.read_typetree().get("m_TextureFormat"))
                except Exception:  # noqa: BLE001, S112
                    continue
                theirs = image.tobytes()
                mine = ours[obj.path_id]
                if len(mine) != len(theirs):
                    problems.append(
                        f"{bundle.name}:{obj.path_id} format {fmt}: "
                        f"{len(mine)} bytes against UnityPy's {len(theirs)}"
                    )
                    continue
                compared += 1
                counter = stats[fmt]
                counter["textures"] += 1
                if mine == theirs:
                    counter["identical"] += 1
                    continue
                counter["differing"] += 1
                worst = max(abs(a - b) for a, b in zip(mine, theirs) if a != b)
                counter["worst"] = max(counter["worst"], worst)
                if fmt not in INDEPENDENTLY_DECODED:
                    problems.append(
                        f"{bundle.name}:{obj.path_id} format {fmt} differs by up to {worst}, "
                        "and both sides decode it the same way, so they should agree exactly"
                    )
                elif worst > 1:
                    problems.append(
                        f"{bundle.name}:{obj.path_id} format {fmt} differs by {worst}, "
                        "more than the one level two correct decoders can differ by"
                    )

    print(f"compared {compared} textures from {len(bundles)} bundle(s)")
    for fmt, counter in sorted(stats.items()):
        note = " (ARM reference decoder)" if fmt in INDEPENDENTLY_DECODED else ""
        print(
            f"  format {fmt}{note}: {counter['identical']}/{counter['textures']} identical"
            + (f", worst difference {counter['worst']}" if counter["differing"] else "")
        )
    if problems:
        print(f"\n{len(problems)} unexplained difference(s):", file=sys.stderr)
        for line in problems[:20]:
            print(f"  {line}", file=sys.stderr)
        return 1
    if compared == 0:
        print("no textures were compared, so nothing was checked", file=sys.stderr)
        return 1
    print("every difference is within one level on an independently decoded format")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
