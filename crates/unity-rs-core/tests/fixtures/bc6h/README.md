# BC6H differential fixtures

`one-subset.bin` is four BC6H blocks covering an 8x8 surface, emitted by the
generator in `tests/support/bc6h_fixture.rs`; a test there checks that the
generator still reproduces this file, so the bytes are explained rather than
opaque.

BC6H was held out of the texture differential for the same reason ASTC was: the
other formats there are fed pseudorandom bytes, and random data lands on
reserved BC6H mode encodings that no encoder emits, where the two
implementations part ways by design. ASTC was solved by borrowing ARM's
`astcenc`; there is no BC6H encoder to borrow here, so the blocks are built
instead. They use the one-subset mode -- five mode bits, three pairs of ten-bit
endpoints, sixteen four-bit indices -- which has no reserved encodings to fall
into. A better encoder would choose better endpoints; it would not make the
comparison stronger, because what the comparison needs is a well-defined block,
not an optimal one.

## The managed blob

`one-subset-managed.rgba` is what the managed decoder produces for that payload,
and the two do not agree.

`texture2ddecoder` 0.1.2 carries two ports of the reference `f32_to_u8`. The
ASTC one writes `floor(f * 255)` where the C++ has `roundf(f * 255)`; this one
writes `(f * 255.0) as u8`, which truncates. Both come out a step low wherever
the value lands at or above a half. BC6H is HDR-only, so every pixel goes
through it; 11 of this fixture's 256 bytes are affected, each by exactly one and
always in the same direction.

Restoring `roundf` in a local copy of the crate reproduces the managed hash
exactly. That is also what establishes the payload as a valid block: two
independent decoders agreeing on all 256 bytes, once the conversion matches, is
not something a malformed block would produce.

`bc6h_differs_from_the_managed_decoder_only_by_truncation` in `texture.rs`
checks this crate's output against the blob byte by byte, so a divergence of any
other shape fails. The managed differential re-checks the blob against the live
managed decoder on every run, and fails if the two ever start matching -- which
is the signal to compare BC6H exactly and drop the recorded divergence.
