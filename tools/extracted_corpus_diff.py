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


def pillow_is_available() -> bool:
    try:
        image_pixels  # noqa: B018
        from PIL import Image  # noqa: F401, PLC0415
    except ImportError:
        print("Pillow is not installed; PNG comparison is skipped", file=sys.stderr)
        return False
    return True


def export_case(
    staged: Path,
    root: Path,
    case_name: str,
    unity_version: str | None,
    problems: dict[str, list[str]],
) -> tuple[dict[str, Path], set[str], bool]:
    mine: dict[str, Path] = {}
    ambiguous: set[str] = set()
    for suffix, classes in (("", ("28", "43", "49")), ("_sprite", ("213",))):
        output = root / f"output{suffix or '_plain'}"
        command = export_command(staged, output, classes, unity_version)
        result = subprocess.run(command, cwd=ROOT, capture_output=True, text=True)
        if result.returncode not in (0, 3):
            problems["export"].append(
                f"{case_name}: export failed: {result.stderr.strip()[:160]}"
            )
            return mine, ambiguous, True
        for path in output.rglob("*"):
            if path.is_file():
                register_exported_path(path, suffix, mine, ambiguous)
    return mine, ambiguous, False


def export_command(
    staged: Path,
    output: Path,
    classes: tuple[str, ...],
    unity_version: str | None,
) -> list[str]:
    command = [
        "cargo", "run", "--release", "--quiet",
        "-p", "unity-rs-cli", "--locked", "--",
    ]
    if unity_version:
        command += ["--unity-version", unity_version]
    command += ["export"]
    for class_id in classes:
        command += ["--class", class_id]
    return [*command, str(staged), str(output)]


def register_exported_path(
    path: Path, suffix: str, mine: dict[str, Path], ambiguous: set[str]
) -> None:
    stem = PATH_ID_SUFFIX.sub("", path.stem) + suffix
    primary = stem + path.suffix
    if primary in mine and mine[primary] != path:
        ambiguous.add(primary)
    mine.setdefault(primary, path)
    mine.setdefault(stem.replace(" ", "_") + path.suffix, path)
    mine.setdefault(sanitized_name(stem) + path.suffix, path)
    if path.suffix not in (".obj", ".png", ".txt"):
        for spelling in (stem, sanitized_name(stem)):
            mine.setdefault(spelling + path.suffix + ".txt", path)


def compare_case_files(
    case: Path,
    mine: dict[str, Path],
    ambiguous: set[str],
    have_pillow: bool,
    totals: collections.Counter,
    problems: dict[str, list[str]],
    worst_alpha: list[int],
    worst_drawn: list[float],
) -> None:
    for theirs in sorted(case.iterdir()):
        if theirs.suffix not in (".obj", ".txt", ".png"):
            continue
        if theirs.suffix == ".png" and not have_pillow:
            continue
        compare_file(
            case, theirs, mine, ambiguous, totals, problems, worst_alpha, worst_drawn
        )


def compare_file(
    case: Path,
    theirs: Path,
    mine: dict[str, Path],
    ambiguous: set[str],
    totals: collections.Counter,
    problems: dict[str, list[str]],
    worst_alpha: list[int],
    worst_drawn: list[float],
) -> None:
    suffix = theirs.suffix
    totals[f"{suffix} compared"] += 1
    if theirs.name in ambiguous:
        totals[f"{suffix} ambiguous name"] += 1
        return
    ours = mine.get(theirs.name)
    if ours is None:
        record_missing(case, theirs, totals, problems)
        return
    if suffix == ".png":
        compare_png(case, ours, theirs, totals, problems, worst_alpha, worst_drawn)
        return
    agree = compare_obj_or_text(ours, theirs, totals)
    if agree is None:
        return
    totals[f"{suffix} {'identical' if agree else 'differing'}"] += 1
    if not agree and len(problems[suffix]) < 20:
        problems[suffix].append(f"{case.name}/{theirs.name}: differs")


def record_missing(
    case: Path,
    theirs: Path,
    totals: collections.Counter,
    problems: dict[str, list[str]],
) -> None:
    key = f"{theirs.suffix} missing"
    totals[f"{theirs.suffix} not exported"] += 1
    if len(problems[key]) < 20:
        problems[key].append(f"{case.name}/{theirs.name}: this project exported nothing")


def compare_obj_or_text(
    ours: Path, theirs: Path, totals: collections.Counter
) -> bool | None:
    if theirs.suffix == ".obj":
        return obj_values(ours) == obj_values(theirs)
    my_bytes = ours.read_bytes()
    their_bytes = theirs.read_bytes()
    if my_bytes == their_bytes:
        return True
    if lossy_utf8_reencode(my_bytes) == their_bytes:
        totals[".txt oracle re-encoded"] += 1
        return None
    return False


def compare_png(
    case: Path,
    ours: Path,
    theirs: Path,
    totals: collections.Counter,
    problems: dict[str, list[str]],
    worst_alpha: list[int],
    worst_drawn: list[float],
) -> None:
    try:
        comparison = image_difference(ours, theirs)
    except Exception as error:  # noqa: BLE001 -- corpus data
        problems[".png unreadable"].append(f"{case.name}/{theirs.name}: {error}")
        return
    if not comparison.agrees:
        totals[".png differing"] += 1
        if len(problems[".png"]) < 20:
            problems[".png"].append(
                f"{case.name}/{theirs.name}: {comparison.reason}, "
                "more than two correct decoders can"
            )
        return
    if comparison.identical:
        totals[".png identical"] += 1
    else:
        totals[".png within decoder tolerance"] += 1
        worst_alpha[0] = max(worst_alpha[0], comparison.worst_alpha)
        worst_drawn[0] = max(worst_drawn[0], comparison.worst_composited)


def report_results(
    totals: collections.Counter,
    problems: dict[str, list[str]],
    worst_alpha: int,
    worst_drawn: float,
) -> int:
    for label, count in sorted(totals.items()):
        print(f"{count:8}  {label}")
    if worst_alpha or worst_drawn:
        print(
            f"         worst alpha difference {worst_alpha}, "
            f"worst drawn difference {worst_drawn:.2f}"
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

    have_pillow = pillow_is_available()
    cases = sorted(p for p in extracted.iterdir() if p.is_dir())
    if limit:
        cases = cases[:limit]
    totals = collections.Counter()
    problems: dict[str, list[str]] = collections.defaultdict(list)
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
            mine, ambiguous, failed = export_case(
                staged, root, case.name, unity_version, problems
            )
            if failed:
                subprocess.run(["rm", "-rf", str(root)], check=True)
                root.mkdir()
                continue
            compare_case_files(
                case, mine, ambiguous, have_pillow, totals, problems,
                worst_alpha, worst_drawn,
            )
            subprocess.run(["rm", "-rf", str(root)], check=True)
            root.mkdir()
            if index % 25 == 0 or index == len(cases):
                print(f"  {index}/{len(cases)} bundles", file=sys.stderr)
    return report_results(totals, problems, worst_alpha[0], worst_drawn[0])


if __name__ == "__main__":
    raise SystemExit(main())
