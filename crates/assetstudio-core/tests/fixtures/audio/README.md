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
