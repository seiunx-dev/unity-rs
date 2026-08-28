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
* `.txt` -- TextAsset bytes, compared exactly, with one attribution. On the
  corpus this was written against, 571 of 577 differ, and none of them is a
  disagreement: the extraction decoded each asset as text and wrote the decode
  back, so every byte that is not valid UTF-8 became `?`. Most of these assets
  are not text at all -- this project's copies begin `1f 8b 08` and unpack
  through Python's `gzip`, while the extraction's copies are rejected as not
  gzip and carry 1,044 `?` bytes against this project's 8. Reproducing that
  transform exactly -- decode UTF-8, one `?` per undecodable byte, re-encode --
  identifies those files as an oracle that destroyed its own payload rather
  than as a difference of opinion. Nothing is loosened: a difference the
  transform does not reproduce still fails. The blind spot is that a difference
  confined to bytes which are themselves undecodable collapses to `?` on both
  sides and cannot be seen.
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

What is left over after all of that, across all 2,778 bundles, is sprites
disagreeing on their tight-mesh boundary and nothing else. Every one is tiny
and the counts say so: 214 of 2,609 images disagree at all, and among those
reported the worst holds 62 bad pixels out of 4,161,596, with 578 bad pixels
in 113,262,530 across the set -- five ten-thousandths of one percent. On one
4,171,800-pixel sprite, 324,432 of the 373,063 pixels that differ have
identical alpha and 48,603 differ by one; 21 exceed the allowance. That is a
rasterizer edge rule, and this project's rasterizer is held to the managed one
by an exact differential, so the disagreement is the extraction's. Those cases
are reported rather than tolerated: widening the allowance until they pass
would also hide a real masking defect.

Counts matter as much as extremes here. Reporting only the worst pixel made a
21-pixel edge case read as "alpha differs by 255" -- indistinguishable from a
texture that decoded wrongly from end to end. For the same reason the problem
list is capped per kind rather than globally: a single flat cap filled with
image rows and hid 39 meshes reported as never exported, which were visible
only in the totals.

Those 39 are worth recording, because 34 of them were this tool's fault. The
name normalization handled spaces and stopped there, so a Spine mesh -- named
`Skeleton Prefab Mesh [Spine GameObject (x)]` in the asset and
`Skeleton_Prefab_Mesh__Spine_GameObject__x__` in the extraction -- looked like
a file this project never wrote. It wrote all 34, and with the names matched
they compare value for value: 632 of 637 OBJs compared, 632 identical, none
differing. The remaining 5 are Unity's vertex-less meshes, which the export
path declines as unsupported and the extraction writes as empty files. A
mismatched name masquerading as a missing export is precisely the alarm this
arm exists to raise, raised by the arm itself.

Where all of that lands, over the whole 2,778-bundle corpus: 637 OBJs
compared with 632 identical and 5 declined as vertex-less; 2,609 images with
248 identical, 2,147 inside the decoder allowance, 212 disagreeing, 2 refused
as ambiguous names and none missing; 577 text assets with 6 identical, 571
attributed to the extraction's re-encoding, none missing and none differing.

Of those 212, exactly one is a colour disagreement rather than a mask edge --
a 4096x4096 Alpha8 font atlas, where this project matches the managed
converter's opaque white and the extraction writes black. That format is now
covered by the managed differential directly, so this arm is no longer the
only thing watching it. The other 211 are 4,496 mask-edge pixels in total.
"""

from __future__ import annotations

import collections
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
MAXIMUM_UNITY_VERSION_LENGTH = 64
UNITY_VERSION_CHARACTERS = frozenset(".-_+")


def stage(source: Path, target: Path) -> None:
    shutil.copy2(source, target)


def validated_unity_version(value: str) -> str:
    """Bound a CLI version passed as one literal argument to the native CLI."""
    if not value or len(value) > MAXIMUM_UNITY_VERSION_LENGTH:
        raise ValueError("Unity version must contain between 1 and 64 characters")
    if not value.isascii() or not all(
        character.isalnum() or character in UNITY_VERSION_CHARACTERS
        for character in value
    ):
        raise ValueError(f"Unity version contains unsupported characters: {value!r}")
    return value


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


def sanitized_name(stem: str) -> str:
    """The extraction's spelling of an asset name: one `_` per unsafe character."""
    return "".join(
        character if character.isalnum() or character in "_.-" else "_"
        for character in stem
    )


def lossy_utf8_reencode(raw: bytes) -> bytes:
    """What a UTF-8 decode with a one-character `?` fallback would produce.

    Valid sequences pass through; every undecodable byte becomes one `?`, which
    is why the length is preserved.
    """
    out = bytearray()
    index = 0
    while index < len(raw):
        for width in (1, 2, 3, 4):
            chunk = raw[index : index + width]
            try:
                chunk.decode("utf-8")
            except UnicodeDecodeError:
                continue
            out += chunk
            index += width
            break
        else:
            out += b"?"
            index += 1
    return bytes(out)


def image_pixels(path: Path):
    from PIL import Image  # noqa: PLC0415

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
    """Compares two images by what they draw, not by their raw channels.

    The reason string carries how many pixels exceeded the allowance, not only
    the worst one. Without the count a sprite whose tight-mesh edge disagrees
    on twenty-one pixels out of four million reads exactly like a texture that
    decoded wrongly from end to end -- both say "alpha differs by 255" -- and
    the first is a known rasterization edge rule while the second would be a
    defect. Reporting only the extreme made them indistinguishable.
    """
    mine, my_size = image_pixels(ours)
    yours, your_size = image_pixels(theirs)
    if my_size != your_size:
        return ImageComparison(False, 0, 0.0, False, f"{my_size} against {your_size}")
    worst_alpha = 0
    worst_composited = 0.0
    over_allowance = 0
    total_pixels = len(mine) // 4
    identical = True
    for offset in range(0, len(mine), 4):
        left = mine[offset : offset + 4]
        right = yours[offset : offset + 4]
        if left == right:
            continue
        identical = False
        pixel_alpha = abs(left[3] - right[3])
        pixel_composited = 0.0
        for channel in range(3):
            contribution = abs(left[channel] * left[3] - right[channel] * right[3]) / 255
            pixel_composited = max(pixel_composited, contribution)
        worst_alpha = max(worst_alpha, pixel_alpha)
        worst_composited = max(worst_composited, pixel_composited)
        if (
            pixel_alpha > MAXIMUM_ALPHA_DIFFERENCE
            or pixel_composited > MAXIMUM_COMPOSITED_DIFFERENCE
        ):
            over_allowance += 1
    agrees = over_allowance == 0
    return ImageComparison(
        agrees,
        worst_alpha,
        worst_composited,
        identical,
        f"{over_allowance} of {total_pixels} pixel(s) over the allowance; "
        f"worst alpha differs by {worst_alpha}, drawn value by {worst_composited:.2f}",
    )


def main() -> int:
    if len(sys.argv) not in (3, 4, 5):
        print(__doc__)
        return 2
    bundles = Path(sys.argv[1])
    extracted = Path(sys.argv[2])
    limit = int(sys.argv[3]) if len(sys.argv) >= 4 else 0
    try:
        unity_version = validated_unity_version(sys.argv[4]) if len(sys.argv) == 5 else None
    except ValueError as error:
        print(error, file=sys.stderr)
        return 2

    have_pillow = True
    try:
        image_pixels  # noqa: B018
        from PIL import Image  # noqa: F401, PLC0415
    except ImportError:
        have_pillow = False
        print("Pillow is not installed; PNG comparison is skipped", file=sys.stderr)

    cases = sorted(p for p in extracted.iterdir() if p.is_dir())
    if limit:
        cases = cases[:limit]

    totals = collections.Counter()
    # Keyed by file kind rather than one flat list. A single global cap fills
    # with whichever kind is alphabetically first and noisiest -- the PNG rows,
    # in practice -- so 39 meshes this project exported nothing for never
    # appeared at all and were visible only in the totals. A category that has
    # something to say should not be silenced by a category that has more.
    problems: dict[str, list[str]] = collections.defaultdict(list)
    # Boxed so the comparison loop can raise the running maxima.
    worst_alpha = [0]
    worst_drawn = [0.0]

    with tempfile.TemporaryDirectory(prefix="unity-rs-extracted-") as work:
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
            # A name that more than one asset claims cannot be compared. This
            # bundle holds sixty-odd textures all called `item_icon`; the export
            # keeps them apart with path IDs, which are stripped here, and the
            # extraction numbers its own copies in an order that is not ours.
            # Comparing them anyway pairs two unrelated icons and reports a
            # handful of channels differing by three to five -- small enough to
            # read as a decoder disagreement, which is what it looked like.
            ambiguous: set[str] = set()
            failed = False
            # Bare-named classes first -- Texture2D, Mesh, TextAsset -- then
            # sprites, whose extraction names carry a suffix.
            for suffix, classes in (("", ("28", "43", "49")), ("_sprite", ("213",))):
                output = root / f"output{suffix or '_plain'}"
                command = [
                    "cargo", "run", "--release", "--quiet",
                    "-p", "unity-rs-cli", "--locked", "--",
                ]
                if unity_version:
                    command += ["--unity-version", unity_version]
                command += ["export"]
                for class_id in classes:
                    command += ["--class", class_id]
                command += [str(staged), str(output)]
                result = subprocess.run(
                    command,
                    cwd=ROOT,
                    capture_output=True,
                    text=True,
                    shell=False,
                )
                if result.returncode not in (0, 3):
                    problems["export"].append(
                        f"{case.name}: export failed: {result.stderr.strip()[:160]}"
                    )
                    failed = True
                    break
                for path in output.rglob("*"):
                    if not path.is_file():
                        continue
                    stem = PATH_ID_SUFFIX.sub("", path.stem) + suffix
                    primary = stem + path.suffix
                    if primary in mine and mine[primary] != path:
                        ambiguous.add(primary)
                    mine.setdefault(primary, path)
                    # A sprite atlas texture is named `sactx-0-1024x512-ASTC
                    # 4x4-...` by Unity, spaces and all. This project keeps the
                    # asset's own name; the extraction replaces the spaces.
                    # Without this the file reads as one this project never
                    # produced, which is a much more alarming thing to report.
                    mine.setdefault(stem.replace(" ", "_") + path.suffix, path)
                    # Spaces are not the only thing it replaces. A Spine mesh is
                    # `Skeleton Prefab Mesh [Spine GameObject (name)]` in the
                    # asset and `Skeleton_Prefab_Mesh__Spine_GameObject__name__`
                    # in the extraction, so brackets and parentheses go the same
                    # way. Matching only on spaces reported 34 of these as never
                    # exported when all 34 were written and correct -- the alarm
                    # this arm exists to prevent, raised by the arm itself.
                    mine.setdefault(sanitized_name(stem) + path.suffix, path)
                    # A `TextAsset` whose name already carries an extension is
                    # written under that name here -- a Spine atlas stays
                    # `x.atlas` -- while the extraction appends `.txt` to every
                    # text asset regardless, giving `x.atlas.txt`. Only the
                    # extensions this comparison does not otherwise handle are
                    # offered, so an image is never matched to `<image>.txt`.
                    if path.suffix not in (".obj", ".png", ".txt"):
                        for spelling in (stem, sanitized_name(stem)):
                            mine.setdefault(spelling + path.suffix + ".txt", path)
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
                if theirs.name in ambiguous:
                    totals[f"{theirs.suffix} ambiguous name"] += 1
                    continue
                ours = mine.get(theirs.name)
                if ours is None:
                    totals[f"{theirs.suffix} not exported"] += 1
                    if len(problems[f"{theirs.suffix} missing"]) < 20:
                        problems[f"{theirs.suffix} missing"].append(
                            f"{case.name}/{theirs.name}: this project exported nothing"
                        )
                    continue
                if theirs.suffix == ".obj":
                    agree = obj_values(ours) == obj_values(theirs)
                elif theirs.suffix == ".txt":
                    my_bytes = ours.read_bytes()
                    their_bytes = theirs.read_bytes()
                    agree = my_bytes == their_bytes
                    if not agree and lossy_utf8_reencode(my_bytes) == their_bytes:
                        # Not a disagreement: the extraction decoded the asset
                        # as text and wrote the decode back, so every byte that
                        # is not valid UTF-8 became `?`. A `TextAsset` is
                        # frequently not text -- most of these are gzip streams,
                        # and the extraction's copies do not decompress -- so
                        # the oracle has destroyed the payload rather than
                        # disagreeing about it. Attributing this exactly, by
                        # reproducing the transform, keeps the row honest: any
                        # other difference still fails. What it cannot see is a
                        # difference confined to bytes that are themselves
                        # undecodable, since those collapse to `?` on both
                        # sides.
                        totals[".txt oracle re-encoded"] += 1
                        continue
                else:
                    try:
                        comparison = image_difference(ours, theirs)
                    except Exception as error:  # noqa: BLE001 -- corpus data
                        problems[".png unreadable"].append(f"{case.name}/{theirs.name}: {error}")
                        continue
                    if not comparison.agrees:
                        totals[".png differing"] += 1
                        if len(problems[".png"]) < 20:
                            problems[".png"].append(
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
                    if len(problems[theirs.suffix]) < 20:
                        problems[theirs.suffix].append(f"{case.name}/{theirs.name}: differs")

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
        total = sum(len(lines) for lines in problems.values())
        print(f"\n{total} problem(s) in {len(problems)} categor(y/ies):", file=sys.stderr)
        for category in sorted(problems):
            lines = problems[category]
            print(f"  {category}: {len(lines)} shown", file=sys.stderr)
            for line in lines:
                print(f"    {line}", file=sys.stderr)
        return 1
    if not any(key.endswith("compared") for key in totals):
        print("nothing was compared, so nothing was checked", file=sys.stderr)
        return 1
    print("every compared file agrees with the existing extraction")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
