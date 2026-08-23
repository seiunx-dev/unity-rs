#!/usr/bin/env python3
"""Regenerates the encoded audio the differential fixtures embed.

The audio oracle compares this crate's decoders against `vgmstream-cli`. That
only means something if the fixture carries real audio: an all-zero payload
makes two decoders agree no matter what either does with the bits, which is how
the Opus comparison passed for months while decoding incorrectly.

Each fixture here is a short tone produced by a named external encoder. The
encoded bytes are committed because the tests must not depend on a local
`ffmpeg` or `lame`; this script exists so those bytes have a recorded origin and
can be regenerated rather than being opaque blobs.

The Opus fixtures come in two flavours deliberately. Opus switches between two
internal codecs, and this crate's decoder (`ruopus`) handles them differently:

    CELT-only    matches libopus to within one unit
    SILK/hybrid  arrives two samples early and differs by ~3% afterwards

One fixture of each keeps both facts under test, so the exact path stays exact
and the divergent path cannot drift further without a failure.

Usage, from the repository root, with `ffmpeg` (built with libopus) and `lame`
on PATH:

    python3 tools/generate_audio_fixtures.py
"""

from __future__ import annotations

import subprocess
import sys
import tempfile
from pathlib import Path

FIXTURES = (
    Path(__file__).resolve().parent.parent
    / "crates"
    / "assetstudio-core"
    / "tests"
    / "fixtures"
    / "audio"
)

# 0.12 s is six 20 ms Opus packets, the shortest run that exercises inter-packet
# decoder state rather than just the first frame.
OPUS_DURATION = "0.12"
OPUS_TONE = f"sine=frequency=1000:sample_rate=48000:duration={OPUS_DURATION}"

# libopus picks its internal codec from the bitrate: 24 kbps lands in
# SILK/hybrid, 64 kbps in CELT-only. Both stay on `-application audio` because
# `lowdelay` also shortens the encoder lookahead to a 120-sample pre-skip, and
# FSB5's framing assumes 312. Fixed bitrate keeps the packet sizes from drifting
# with the rate controller.
OPUS_VARIANTS = {
    "opus-tone-packets.bin": ["-b:a", "24k", "-application", "audio"],
    "opus-tone-celt-packets.bin": ["-b:a", "64k", "-application", "audio"],
}

MPEG_NAME = "mpeg-layer3-tone.mp3"
MPEG_TONE = "sine=frequency=440:sample_rate=44100:duration=0.3"
MPEG_MULTISTREAM_NAME = "fsb5-mpeg-layer3-6ch.fsb"
MPEG_MULTISTREAM_PAIRS = ((330, 440), (550, 660), (770, 880))


def run(command: list[str]) -> None:
    subprocess.run(command, check=True, capture_output=True)


def ogg_packets(path: Path) -> list[bytes]:
    """Returns the packets of an Ogg stream, headers included."""
    data = path.read_bytes()
    packets: list[bytes] = []
    partial = b""
    offset = 0
    while offset < len(data):
        if data[offset : offset + 4] != b"OggS":
            raise ValueError(f"not an Ogg page at offset {offset}")
        segment_count = data[offset + 26]
        segments = data[offset + 27 : offset + 27 + segment_count]
        body = offset + 27 + segment_count
        for size in segments:
            partial += data[body : body + size]
            body += size
            # A segment shorter than 255 bytes terminates its packet.
            if size < 255:
                packets.append(partial)
                partial = b""
        offset = body
    return packets


def generate_opus(name: str, options: list[str], work: Path) -> None:
    ogg = work / "tone.opus"
    run(
        [
            "ffmpeg", "-v", "error", "-y",
            "-f", "lavfi", "-i", OPUS_TONE,
            "-c:a", "libopus", *options,
            "-vbr", "off", "-frame_duration", "20",
            str(ogg),
        ]
    )
    packets = ogg_packets(ogg)
    head, audio = packets[0], packets[2:]
    if not head.startswith(b"OpusHead"):
        raise ValueError("first packet is not an OpusHead")
    pre_skip = int.from_bytes(head[10:12], "little")

    # FSB5 hardcodes a 312-sample encoder delay, so a stream whose own pre-skip
    # differs would be trimmed wrongly by the fixture builder rather than by any
    # code under test.
    if pre_skip != 312:
        raise ValueError(f"{name}: pre-skip is {pre_skip}, but FSB5 assumes 312")

    # FSB5 packet framing: each packet carries a little-endian u16 length, and a
    # zero length terminates the stream. The terminator is appended by the test,
    # which is what makes it part of what the test verifies.
    blob = b"".join(len(p).to_bytes(2, "little") + p for p in audio)
    (FIXTURES / name).write_bytes(blob)
    configs = {packet[0] >> 3 for packet in audio}
    mode = "CELT" if min(configs) >= 16 else "SILK/hybrid"
    print(f"wrote {name}: {len(audio)} {mode} packets, {len(blob)} bytes")


def generate_mpeg(work: Path) -> None:
    raw = work / "tone.wav"
    run(["ffmpeg", "-v", "error", "-y", "-f", "lavfi", "-i", MPEG_TONE, str(raw)])
    # No bit reservoir and no Xing/Info tag, so every frame decodes standalone:
    # the fixture builder repacks the frames into FSB5's four-byte-aligned
    # framing and a reservoir would make a frame depend on its predecessor's
    # bytes, which that repacking does not preserve.
    run(
        [
            "lame", "--quiet", "-b", "128", "--cbr", "-m", "m",
            "--nores", "-t", str(raw), str(FIXTURES / MPEG_NAME),
        ]
    )
    print(f"wrote {MPEG_NAME}: {(FIXTURES / MPEG_NAME).stat().st_size} bytes")


def mpeg_layer3_frames(data: bytes) -> list[bytes]:
    """Splits a headerless MPEG Layer III stream into complete frames."""
    mpeg1_layer3_bitrates = (
        0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 0
    )
    mpeg1_rates = (44_100, 48_000, 32_000)
    frames: list[bytes] = []
    offset = 0
    while offset < len(data):
        if offset + 4 > len(data):
            raise ValueError("MPEG fixture ends inside a frame header")
        word = int.from_bytes(data[offset : offset + 4], "big")
        if word >> 21 != 0x7FF or (word >> 19) & 0x03 != 3:
            raise ValueError(f"MPEG fixture has a non-MPEG-1 sync at {offset}")
        if (word >> 17) & 0x03 != 1:
            raise ValueError(f"MPEG fixture has a non-Layer-III frame at {offset}")
        bitrate = mpeg1_layer3_bitrates[(word >> 12) & 0x0F]
        rate_index = (word >> 10) & 0x03
        if bitrate == 0 or rate_index >= len(mpeg1_rates):
            raise ValueError(f"MPEG fixture has a reserved header at {offset}")
        sample_rate = mpeg1_rates[rate_index]
        padding = (word >> 9) & 1
        length = 144 * bitrate * 1000 // sample_rate + padding
        end = offset + length
        if end > len(data):
            raise ValueError(f"MPEG fixture frame at {offset} is truncated")
        frames.append(data[offset:end])
        offset = end
    return frames


def generate_multistream_mpeg(work: Path) -> None:
    """Builds a six-channel FSB5 from three independently encoded stereo streams."""
    streams: list[list[bytes]] = []
    for index, (left, right) in enumerate(MPEG_MULTISTREAM_PAIRS):
        raw = work / f"mpeg-pair-{index}.wav"
        encoded = work / f"mpeg-pair-{index}.mp3"
        run(
            [
                "ffmpeg", "-v", "error", "-y",
                "-f", "lavfi", "-i",
                f"sine=frequency={left}:sample_rate=44100:duration=0.3",
                "-f", "lavfi", "-i",
                f"sine=frequency={right}:sample_rate=44100:duration=0.3",
                "-filter_complex", "[0:a][1:a]amerge=inputs=2[a]",
                "-map", "[a]", "-ac", "2", str(raw),
            ]
        )
        run(
            [
                "lame", "--quiet", "-b", "128", "--cbr", "-m", "s",
                "--nores", "-t", str(raw), str(encoded),
            ]
        )
        streams.append(mpeg_layer3_frames(encoded.read_bytes()))

    frame_count = len(streams[0])
    if frame_count == 0 or any(len(stream) != frame_count for stream in streams):
        raise ValueError("multistream MPEG encoders produced different frame counts")
    data = bytearray()
    for frames in zip(*streams):
        interleave = (len(frames[0]) + 15) & ~15
        for frame in frames:
            if (len(frame) + 15) & ~15 != interleave:
                raise ValueError("multistream MPEG frames have different padded spans")
            data.extend(frame)
            data.extend(b"\0" * (interleave - len(frame)))

    header = bytearray(0x3C)
    header[:4] = b"FSB5"
    header[4:8] = (1).to_bytes(4, "little")
    header[8:12] = (1).to_bytes(4, "little")
    header[12:16] = (8).to_bytes(4, "little")
    header[20:24] = len(data).to_bytes(4, "little")
    header[24:28] = (11).to_bytes(4, "little")
    # Compact channel code 2 is six channels; rate code 8 is 44.1 kHz.
    sample_mode = (frame_count * 1152 << 34) | (2 << 5) | (8 << 1)
    output = header + sample_mode.to_bytes(8, "little") + data
    (FIXTURES / MPEG_MULTISTREAM_NAME).write_bytes(output)
    print(
        f"wrote {MPEG_MULTISTREAM_NAME}: {frame_count} frames per stream, "
        f"{len(output)} bytes"
    )


def main() -> None:
    for tool in ("ffmpeg", "lame"):
        if subprocess.run(["which", tool], capture_output=True).returncode != 0:
            sys.exit(f"{tool} is required to regenerate the audio fixtures")
    with tempfile.TemporaryDirectory() as directory:
        work = Path(directory)
        for name, options in OPUS_VARIANTS.items():
            generate_opus(name, options, work)
        generate_mpeg(work)
        generate_multistream_mpeg(work)


if __name__ == "__main__":
    main()
