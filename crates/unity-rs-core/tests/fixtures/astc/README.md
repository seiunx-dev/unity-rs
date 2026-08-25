# ASTC differential fixtures

`astc-<variant>-<N>x<N>.bin` are ASTC payloads produced by ARM's `astcenc`
through the `astc-encoder-py` binding, at each of Unity's six block footprints
in three variants: opaque LDR, LDR with a diagonal alpha ramp, and HDR. Each
covers a surface two blocks wide and two blocks tall, so the comparison sees
block-to-block placement and not just one block's decode. The source image is a
gradient computed by `tools/generate_astc_fixtures.py`, which regenerates these
files; the formula lives in the script rather than in a committed PNG.

ASTC was the last block format outside the managed differential, and the reason
was that the other formats there are fed pseudorandom bytes. That works for
them because every bit pattern decodes to something. ASTC is not like that:
random data lands on reserved block encodings no encoder emits, and the two
implementations part ways on those by design -- the managed decoder substitutes
an error colour where this crate rejects the block -- so a random-payload
comparison would report a disagreement that says nothing about either decoder.
Real encoder output contains no reserved blocks, which is what makes the
comparison mean something.

## The HDR blobs

The twelve LDR formats decode identically to the managed decoder. The six HDR
formats do not, and `astc-hdr-<N>x<N>-managed.rgba` holds what the managed
decoder produces for them.

The cause is one word. `texture2ddecoder` 0.1.2 ports the reference
`select_color_hdr` with `floor(f * 255)` where the C++ it came from has
`roundf(f * 255)`, so an HDR channel landing on a fractional value at or above
one half comes out a step low. Between 8% and 14% of the bytes in these
fixtures are affected; every difference is exactly one, and always in the same
direction. Correcting that word in a local copy of the crate reproduces all six
managed hashes exactly, which is where these blobs come from.

Two tests hold the ends together.
`hdr_astc_differs_from_the_managed_decoder_only_by_truncation` in `texture.rs`
checks this crate's output against the blobs byte by byte, so a divergence of
any other shape or size fails. The managed differential checks the blobs
themselves against the live managed decoder on every run, so calling them
managed output stays earned rather than remembered, and fails if the formats
ever start matching -- which is the signal to move HDR into the exact set.
