#!/usr/bin/env python3
"""Reads the crate's exported WAV files with Python's own RIFF parser.

The WAV header is written by this project, and the audio differential parses it
back with this project's helper before comparing samples against vgmstream. So
the header itself has only ever been read by the code that wrote it: a wrong
block alignment or byte rate would survive, because both sides would make the
same mistake and the sample comparison would still line up.

`wave` is in Python's standard library, was not written for this project, and
refuses files whose fields disagree. Asking it for the channel count, sample
rate, width and frame count is a genuine second opinion on the container.

It is not a complete one. `wave` reads the three fields it needs and ignores
the byte rate and block alignment entirely, and both of those are redundant --
derived from the other three -- which is precisely why a wrong one survives
every reader that recomputes what it needs. The chunk walk here checks them,
along with the chunk sizes against the file's own length.

    python3 tools/validate_wav_output.py            # export through the CLI
    python3 tools/validate_wav_output.py sound.wav  # check an existing file
"""

from __future__ import annotations

import struct
import subprocess
import sys
import tempfile
import wave
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# (name, format, sample rate, bits, payload) for each clip written.
#
# Legacy pre-2.6 AudioClips carry raw PCM16, which is the one shape this can
# build without a codec. The first field is Unity's `format`, not a channel
# count: channels are `format >> 1`, which is what the reader derives. Writing
# 2 there and expecting stereo is the mistake this script made first, and the
# standard library's reader is what caught it.
CLIPS = [
    ("legacy-stereo", 4, 22_050, 16, b"\x01\x02\x03\x04"),
    ("legacy-mono", 2, 44_100, 16, b"\x10\x20\x30\x40\x50\x60"),
]


class Invalid(Exception):
    pass


def pad(value: bytes) -> bytes:
    return value + b"\x00" * (-len(value) % 4)


def text(value: str) -> bytes:
    return pad(struct.pack("<I", len(value)) + value.encode())


def audio_clip(name: str, audio_format: int, rate: int, payload: bytes) -> bytes:
    """A pre-2.6 AudioClip, whose payload is raw PCM16."""
    body = bytearray(text(name))
    body += struct.pack("<i", audio_format)
    body += struct.pack("<f", 0.0)
    body += struct.pack("<i", rate)
    body += struct.pack("<i", len(payload))
    body += payload
    return finish_v22(83, bytes(pad(body)), "2.5.0f1")


def finish_v22(class_id: int, payload: bytes, version: str) -> bytes:
    metadata = bytearray(version.encode() + b"\x00")
    metadata += struct.pack("<i", 13) + b"\x00"
    metadata += struct.pack("<i", 1) + struct.pack("<i", class_id)
    metadata += b"\x00" + struct.pack("<h", -1) + bytes(16)
    metadata += struct.pack("<i", 1)
    while (48 + len(metadata)) % 4:
        metadata += b"\x00"
    metadata += struct.pack("<q", 7) + struct.pack("<q", 0)
    metadata += struct.pack("<I", len(payload)) + struct.pack("<i", 0)
    metadata += struct.pack("<i", 0) * 3 + b"\x00"

    data_offset = -(-(48 + len(metadata)) // 16) * 16
    header = bytearray(48)
    header[8:12] = struct.pack(">I", 22)
    header[20:24] = struct.pack(">I", len(metadata))
    header[24:32] = struct.pack(">q", data_offset + len(payload))
    header[32:40] = struct.pack(">q", data_offset)
    return bytes(header + metadata + bytes(data_offset - 48 - len(metadata)) + payload)


def check(path: Path, channels: int, rate: int, bits: int, frames: int) -> list[str]:
    """Opens the file with the standard library and compares what it reports."""
    try:
        with wave.open(str(path), "rb") as reader:
            actual = (
                reader.getnchannels(),
                reader.getframerate(),
                reader.getsampwidth() * 8,
                reader.getnframes(),
            )
            data = reader.readframes(reader.getnframes())
    except wave.Error as error:
        raise Invalid(f"Python's wave module rejected the file: {error}") from error

    expected = (channels, rate, bits, frames)
    if actual != expected:
        raise Invalid(
            f"wave reports channels/rate/bits/frames {actual}, expected {expected}"
        )
    if len(data) != frames * channels * bits // 8:
        raise Invalid(
            f"wave read {len(data)} bytes of samples; "
            f"{frames * channels * bits // 8} are implied by the header"
        )
    return [f"{channels}ch {rate}Hz {bits}-bit, {frames} frame(s)", *derived(path)]


def derived(path: Path) -> list[str]:
    """Checks the fields `wave` computes nothing from and reports nowhere.

    The byte rate and block alignment are redundant -- both follow from the
    channel count, sample rate and sample width -- which is exactly why a wrong
    one survives. Python's reader takes the three it needs and ignores these
    two, so a file with a nonsense byte rate reads back perfectly above and
    still trips players that seek by it. The chunk sizes are redundant in the
    same way against the file's own length.
    """
    data = path.read_bytes()
    if len(data) < 44:
        raise Invalid(f"the file is {len(data)} bytes, shorter than a WAV header")
    riff, chunk_size, wave_tag = struct.unpack_from("<4sI4s", data, 0)
    if riff != b"RIFF" or wave_tag != b"WAVE":
        raise Invalid("the file is not RIFF/WAVE")
    if chunk_size != len(data) - 8:
        raise Invalid(f"RIFF size is {chunk_size}; the file implies {len(data) - 8}")

    at = 12
    seen = []
    while at + 8 <= len(data):
        identifier, size = struct.unpack_from("<4sI", data, at)
        seen.append(identifier.decode("ascii", "replace"))
        body = data[at + 8 : at + 8 + size]
        if len(body) != size:
            raise Invalid(
                f"chunk {seen[-1]} declares {size} bytes but only {len(body)} remain"
            )
        if identifier == b"fmt ":
            validate_format_chunk(body, size)
        elif identifier == b"data" and at + 8 + size != len(data):
            raise Invalid(
                f"the data chunk ends at {at + 8 + size} of {len(data)} bytes"
            )
        at += 8 + size + (size % 2)

    for required in ("fmt ", "data"):
        if required not in seen:
            raise Invalid(f"the file has no {required} chunk")
    return [f"chunks {'/'.join(seen)}, byte rate and block alignment consistent"]


def validate_format_chunk(body: bytes, size: int) -> None:
    if size < 16:
        raise Invalid(f"the fmt chunk is {size} bytes, shorter than PCM's 16")
    audio_format, channels, rate, byte_rate, block_align, bits = struct.unpack_from(
        "<HHIIHH", body, 0
    )
    if audio_format != 1:
        raise Invalid(f"fmt declares format {audio_format}, not PCM")
    expected_block = channels * bits // 8
    if block_align != expected_block:
        raise Invalid(
            f"block alignment is {block_align}; "
            f"{channels} channels at {bits} bits imply {expected_block}"
        )
    expected_rate = rate * expected_block
    if byte_rate != expected_rate:
        raise Invalid(
            f"byte rate is {byte_rate}; "
            f"{rate}Hz at {expected_block} bytes per frame imply {expected_rate}"
        )


def export_and_validate() -> int:
    checked = 0
    with tempfile.TemporaryDirectory(prefix="unity-rs-wav-") as directory:
        for name, audio_format, rate, bits, payload in CLIPS:
            channels = audio_format >> 1
            frames = len(payload) // (channels * bits // 8)
            work = Path(directory) / name
            work.mkdir()
            assets = work / "clip.assets"
            assets.write_bytes(audio_clip(name, audio_format, rate, payload))
            result = subprocess.run(
                ["cargo", "run", "--quiet", "-p", "unity-rs-cli", "--locked", "--",
                 "export", str(assets), str(work / "out")],
                cwd=ROOT, capture_output=True, text=True,
            )
            if result.returncode != 0:
                print(result.stderr.strip(), file=sys.stderr)
                return 1
            wavs = list((work / "out").rglob("*.wav"))
            if len(wavs) != 1:
                print(f"{name}: expected one WAV, found {wavs}", file=sys.stderr)
                return 1
            try:
                for note in check(wavs[0], channels, rate, bits, frames):
                    print(f"  {name}: {note}")
            except Invalid as error:
                print(f"{name}: {error}", file=sys.stderr)
                return 1
            checked += 1
    print(f"{checked} exported WAV file(s) accepted by the standard library's reader")
    return 0


def main() -> int:
    if len(sys.argv) == 1:
        return export_and_validate()
    if len(sys.argv) != 2:
        print(__doc__)
        return 2
    path = Path(sys.argv[1])
    try:
        with wave.open(str(path), "rb") as reader:
            print(
                f"{path}: {reader.getnchannels()}ch {reader.getframerate()}Hz "
                f"{reader.getsampwidth() * 8}-bit, {reader.getnframes()} frame(s)"
            )
    except wave.Error as error:
        print(f"{path}: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
