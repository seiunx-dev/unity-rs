# FSB5 Vorbis differential fixture

`fsb5-vorbis-stereo.fsb` contains 4,800 stereo frames at 48 kHz. The source
signal is a deterministic non-silent ramp/saw pattern encoded with the system
`libvorbis 1.3.7` encoder at quality 0.4, then repacked from Ogg packets into
FSB5 packet framing. Its setup-header CRC is `0x87c121d5`.

The fixture is used only for output comparison with a separately installed
`vgmstream-cli`; the Rust implementation neither links nor redistributes
libvorbis or vgmstream. The setup packet used to reconstruct the Vorbis stream
comes from the compact, mechanically generated setup table described in
`THIRD_PARTY_NOTICES.md`.

`fsb5-vorbis-stereo-silence.fsb` is a minimal silent development fixture kept
for parser diagnostics; semantic output tests use the non-silent fixture.

# MPEG Layer III differential fixture

`mpeg-layer3-tone.mp3` is a 0.3-second 440 Hz tone generated with `ffmpeg`'s
`sine` source and encoded by the system `lame` at 128 kbps CBR, mono, 44.1 kHz,
with the bit reservoir and the Xing/Info tag disabled so every frame stands
alone. It is thirteen MPEG-1 Layer III frames and nothing else.

The test repacks those frames into FSB5 framing, padding each to the four-byte
boundary FSB5 requires. It replaced an all-zero fixture that compared framing
only: two readers agree on silence whatever their decoders do, so a
sample-level defect had nowhere to show.

Layer III output is not specified bit-exactly, so the tone is compared with a
one-unit tolerance; the silent cases alongside it still require exact equality.
