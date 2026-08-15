#!/usr/bin/env python3
"""Compares this project's export against an existing extraction of the same corpus.

A corpus that arrives with someone else's extraction beside it is a second
implementation for free, and a much broader one than a synthetic fixture can
be: it covers whatever the game actually ships, in whatever versions it ships
them, at a scale no hand-written oracle reaches.

    python3 tools/extracted_corpus_diff.py <bundles> <extracted> [limit] [unity-version]

`<extracted>` is a directory of per-bundle directories named after the bundle
they came from. Three kinds of file are compared, each on the terms it can
actually be held to:

* `.obj` -- compared as numbers, not as text. Both sides write the same
  geometry, but the extraction here spells floats with nine significant digits
  while this project reproduces the current managed writer's shortest
  round-trip form. Parsing both sides back to `f32` compares what the readers
  read rather than how two different tools chose to print it.
* `.txt` -- TextAsset bytes, compared exactly. There is nothing to normalize.
* `.png` -- compared as decoded pixels, since two PNG encoders agree about
  images and not about bytes, and compared by what a pixel contributes when it
  is drawn rather than by its raw channels. Sizes must match exactly, alpha
  may differ by at most two levels, and the composited value `rgb * alpha /
  255` by at most two -- which is what one level in each of colour and alpha
  compounds to, not a number chosen to make the corpus pass.

  The composited form is the point, not a convenience. Two correct decoders of
  the same block format land either side of a half -- one 768x1536 texture
  here differs in 4 bytes out of 4,718,592, each by exactly 1 -- and under a
  near-zero alpha they diverge much further while the drawn result does not:
  in one sprite the raw channels differ by up to 26 where alpha is 8 or less,
  and the composited value never differs by more than 1 anywhere in the image.
  Comparing raw channels would either fail on invisible colour or need a
  hand-picked alpha threshold, which is tuning until green. The two tools also
  disagree about what to leave under a masked texel -- this one zeroes the
  alpha and keeps the source colour, the extraction zeroes all four channels
  -- and the composited comparison says the right thing about that on its own.

  Alpha is still compared directly, so a texel that should be opaque and is
  not still fails, whatever colour sits under it.

A `Texture2D` and the `Sprite` cut from it usually carry the same asset name.
The extraction tells them apart with a `_sprite` suffix; this project's export
disambiguates by appending a path ID, and the two schemes do not line up. So
the export runs twice per bundle, once for the classes whose names the
extraction leaves bare and once for sprites, and each set is matched against
the half of the extraction it belongs to. Matching them in one pass silently
compares a sprite against its own source texture, which differs everywhere the
sprite was cropped.

Everything else in an extraction is ignored, and for two different reasons.
The game-specific JSON and `.bin` files come from a decoder for that game's
own table format, which this project does not implement and should not pretend
to. The `.shader` files are skipped on purpose even though both sides write
shader text: the extraction's writer emits less of it -- for one stencil
shader it writes `ZWrite Off` where this project writes `ColorMask`, `ZWrite`
and the whole `Stencil` block, and it spells a float property default `0.0`
where the managed writer spells it `0`. This project's shader text is held to
the managed writer by an exact differential, so comparing it against a weaker
writer would only invite matching the weaker one.

A file this project does not produce is counted and named rather than passed
over, because "we exported nothing" would otherwise look like agreement.

What is left over after all of that, on the corpus this was written against,
is a handful of sprites disagreeing on their tight-mesh boundary: 4 pixels out
of 1,143,000 in the worst one, going both ways -- one texel this project keeps
and the extraction masks, three the other way round. That is a rasterizer edge
rule, and this project's rasterizer is held to the managed one by an exact
differential, so the disagreement is the extraction's. Those cases are
reported rather than tolerated: widening the allowance until they pass would
also hide a real masking defect.
"""

from __future__ import annotations

import collections
import os
import re
import shutil
import struct
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import NamedTuple

ROOT = Path(__file__).resolve().parent.parent
# Export names collide, so this project appends the path ID. The extraction
# does not, so the suffix comes off before matching.
PATH_ID_SUFFIX = re.compile(r" @-?\d+$")


def stage(source: Path, target: Path) -> None:
    try:
        os.link(source, target)
    except OSError:
        shutil.copy2(source, target)


def obj_values(path: Path) -> list[list[object]]:
    """An OBJ as tokens, with every number narrowed to the f32 both sides read."""
    rows = []
    for line in path.read_text(errors="replace").splitlines():
        parts = line.split()
        if not parts:
            continue
        row: list[object] = [parts[0]]
        for token in parts[1:]:
            try:
                row.append(struct.unpack("<f", struct.pack("<f", float(token)))[0])
            except ValueError:
                row.append(token)
        rows.append(row)
    return rows


def image_pixels(path: Path):
    from PIL import Image  # noqa: PLC0415 -- optional, and only needed here

    with Image.open(path) as image:
        return image.convert("RGBA").tobytes(), image.size


# Derived rather than fitted. If each channel may differ by one level, then
# `|r1*a1 - r2*a2| <= |r1|*|a1-a2| + |a2|*|r1-r2| <= 255 + 255`, so the drawn
# value can differ by two. The alpha allowance is empirical: this corpus's
# extraction reaches two, which is one more than two decoders of the same
# format should, and is reported so a change in it is visible.
MAXIMUM_COMPOSITED_DIFFERENCE = 2.0
MAXIMUM_ALPHA_DIFFERENCE = 2


class ImageComparison(NamedTuple):
    agrees: bool
    worst_alpha: int
    worst_composited: float
    identical: bool
    reason: str


def image_difference(ours: Path, theirs: Path) -> ImageComparison:
    """Compares two images by what they draw, not by their raw channels."""
    mine, my_size = image_pixels(ours)
    yours, your_size = image_pixels(theirs)
    if my_size != your_size:
        return ImageComparison(False, 0, 0.0, False, f"{my_size} against {your_size}")
    worst_alpha = 0
    worst_composited = 0.0
    identical = True
    for offset in range(0, len(mine), 4):
        left = mine[offset : offset + 4]
        right = yours[offset : offset + 4]
        if left == right:
            continue
        identical = False
        worst_alpha = max(worst_alpha, abs(left[3] - right[3]))
        for channel in range(3):
            contribution = abs(left[channel] * left[3] - right[channel] * right[3]) / 255
            worst_composited = max(worst_composited, contribution)
    agrees = (
        worst_alpha <= MAXIMUM_ALPHA_DIFFERENCE
        and worst_composited <= MAXIMUM_COMPOSITED_DIFFERENCE
    )
    return ImageComparison(
        agrees,
        worst_alpha,
        worst_composited,
        identical,
        f"alpha differs by {worst_alpha}, drawn value by {worst_composited:.2f}",
    )


def main() -> int:
    if len(sys.argv) not in (3, 4, 5):
        print(__doc__)
        return 2
    bundles = Path(sys.argv[1])
    extracted = Path(sys.argv[2])
    limit = int(sys.argv[3]) if len(sys.argv) >= 4 else 0
    unity_version = sys.argv[4] if len(sys.argv) == 5 else None

    have_pillow = True
    try:
        image_pixels  # noqa: B018 -- the import happens inside, so probe it
        from PIL import Image  # noqa: F401, PLC0415
    except ImportError:
        have_pillow = False
        print("Pillow is not installed; PNG comparison is skipped", file=sys.stderr)

    cases = sorted(p for p in extracted.iterdir() if p.is_dir())
    if limit:
        cases = cases[:limit]

    totals = collections.Counter()
    problems: list[str] = []
    # Boxed so the comparison loop can raise the running maxima.
    worst_alpha = [0]
    worst_drawn = [0.0]

    with tempfile.TemporaryDirectory(prefix="assetstudio-extracted-") as work:
        root = Path(work).resolve()
        for index, case in enumerate(cases, start=1):
            bundle = bundles / case.name
            if not bundle.exists():
                totals["bundle missing"] += 1
                continue
            staged = root / "input"
            staged.mkdir()
            stage(bundle, staged / bundle.name)
            mine: dict[str, Path] = {}
            failed = False
            # Bare-named classes first -- Texture2D, Mesh, TextAsset -- then
            # sprites, whose extraction names carry a suffix.
            for suffix, classes in (("", ("28", "43", "49")), ("_sprite", ("213",))):
                output = root / f"output{suffix or '_plain'}"
                command = [
                    "cargo", "run", "--release", "--quiet",
                    "-p", "assetstudio-cli", "--locked", "--",
                ]
                if unity_version:
                    command += ["--unity-version", unity_version]
                command += ["export"]
                for class_id in classes:
                    command += ["--class", class_id]
                command += [str(staged), str(output)]
                result = subprocess.run(command, cwd=ROOT, capture_output=True, text=True)
                if result.returncode not in (0, 3):
                    problems.append(
                        f"{case.name}: export failed: {result.stderr.strip()[:160]}"
                    )
                    failed = True
                    break
                for path in output.rglob("*"):
                    if not path.is_file():
                        continue
                    stem = PATH_ID_SUFFIX.sub("", path.stem) + suffix
                    mine.setdefault(stem + path.suffix, path)
                    # A sprite atlas texture is named `sactx-0-1024x512-ASTC
                    # 4x4-...` by Unity, spaces and all. This project keeps the
                    # asset's own name; the extraction replaces the spaces.
                    # Without this the file reads as one this project never
                    # produced, which is a much more alarming thing to report.
                    mine.setdefault(stem.replace(" ", "_") + path.suffix, path)
            if failed:
                subprocess.run(["rm", "-rf", str(root)], check=True)
                root.mkdir()
                continue

            for theirs in sorted(case.iterdir()):
                if theirs.suffix not in (".obj", ".txt", ".png"):
                    continue
                if theirs.suffix == ".png" and not have_pillow:
                    continue
                totals[f"{theirs.suffix} compared"] += 1
                ours = mine.get(theirs.name)
                if ours is None:
                    totals[f"{theirs.suffix} not exported"] += 1
                    if len(problems) < 40:
                        problems.append(f"{case.name}/{theirs.name}: this project exported nothing")
                    continue
                if theirs.suffix == ".obj":
                    agree = obj_values(ours) == obj_values(theirs)
                elif theirs.suffix == ".txt":
                    agree = ours.read_bytes() == theirs.read_bytes()
                else:
                    try:
                        comparison = image_difference(ours, theirs)
                    except Exception as error:  # noqa: BLE001 -- corpus data
                        problems.append(f"{case.name}/{theirs.name}: {error}")
                        continue
                    if not comparison.agrees:
                        totals[".png differing"] += 1
                        if len(problems) < 40:
                            problems.append(
                                f"{case.name}/{theirs.name}: {comparison.reason}, "
                                "more than two correct decoders can"
                            )
                        continue
                    if comparison.identical:
                        totals[".png identical"] += 1
                    else:
                        totals[".png within decoder tolerance"] += 1
                        worst_alpha[0] = max(worst_alpha[0], comparison.worst_alpha)
                        worst_drawn[0] = max(worst_drawn[0], comparison.worst_composited)
                    continue
                if agree:
                    totals[f"{theirs.suffix} identical"] += 1
                else:
                    totals[f"{theirs.suffix} differing"] += 1
                    if len(problems) < 40:
                        problems.append(f"{case.name}/{theirs.name}: differs")

            subprocess.run(["rm", "-rf", str(root)], check=True)
            root.mkdir()
            if index % 25 == 0 or index == len(cases):
                print(f"  {index}/{len(cases)} bundles", file=sys.stderr)

    for label, count in sorted(totals.items()):
        print(f"{count:8}  {label}")
    if worst_alpha[0] or worst_drawn[0]:
        print(
            f"         worst alpha difference {worst_alpha[0]}, "
            f"worst drawn difference {worst_drawn[0]:.2f}"
        )
    if problems:
        print(f"\n{len(problems)} problem(s):", file=sys.stderr)
        for line in problems:
            print(f"  {line}", file=sys.stderr)
        return 1
    if not any(key.endswith("compared") for key in totals):
        print("nothing was compared, so nothing was checked", file=sys.stderr)
        return 1
    print("every compared file agrees with the existing extraction")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
