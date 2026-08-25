//! Builds valid BC6H blocks for the texture differential.
//!
//! BC6H was held out of the comparison for the same reason ASTC was: random
//! bytes land on reserved mode encodings that no encoder emits, and the two
//! implementations part ways there by design, so a disagreement would say
//! nothing about either decoder. ASTC was solved by borrowing a real encoder;
//! there is no BC6H encoder to borrow here.
//!
//! What the comparison needs is not an optimal encoder, only well-defined
//! blocks. This emits the one-subset mode -- the five mode bits `00011`, three
//! pairs of ten-bit endpoints, and sixteen four-bit indices -- which has no
//! reserved encodings to fall into: every bit pattern this writes is a block
//! both decoders are required to interpret the same way. A better encoder would
//! choose better endpoints; it would not make the comparison stronger.

/// Packs one BC6H block in the one-subset, ten-bit-endpoint mode.
///
/// `endpoints` are the two RGB triples, each channel a ten-bit value, and
/// `indices` the sixteen interpolation weights. The first index carries one
/// fewer bit -- the anchor's high bit is implied -- so it is masked to three.
fn one_subset_block(endpoints: [[u16; 3]; 2], indices: [u8; 16]) -> [u8; 16] {
    let mut block = [0_u8; 16];
    let mut cursor = 0_usize;
    let mut push = |value: u32, bits: usize, block: &mut [u8; 16]| {
        for offset in 0..bits {
            if value >> offset & 1 == 1 {
                block[(cursor + offset) / 8] |= 1 << ((cursor + offset) % 8);
            }
        }
        cursor += bits;
    };

    // Mode 00011: one subset, endpoints stored outright rather than as deltas.
    push(0b0_0011, 5, &mut block);
    // The two endpoints, red then green then blue.
    for channel in 0..3 {
        push(u32::from(endpoints[0][channel]), 10, &mut block);
    }
    for channel in 0..3 {
        push(u32::from(endpoints[1][channel]), 10, &mut block);
    }
    push(u32::from(indices[0]) & 0b111, 3, &mut block);
    for index in &indices[1..] {
        push(u32::from(*index), 4, &mut block);
    }
    // Not `debug_assert_eq!`. Release profiles drop those, and the committed
    // payload beside these fixtures is produced by this function, so at the
    // moment it is regenerated this is the only check that the packing fills a
    // block exactly -- a short block would be written to the file and then
    // compared against itself forever. Over-filling is caught by the array
    // bounds above; under-filling is caught only here, which is verified: a
    // ten-bit endpoint written as nine fails with 125 against 128 under
    // `--release`, where a `debug_assert` reported nothing.
    assert_eq!(cursor, 128, "a BC6H block is exactly 128 bits");
    block
}

/// Four blocks covering an 8x8 surface, varied enough that a decoder that
/// ignored the endpoints or the weights would produce something else.
pub(crate) fn bc6h_payload() -> Vec<u8> {
    let blocks = [
        // A dark-to-bright ramp across the block.
        (
            [[0x040, 0x080, 0x0c0], [0x300, 0x280, 0x200]],
            [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
        ),
        // Both endpoints equal: every weight has to land on the same colour.
        (
            [[0x1ff, 0x1ff, 0x1ff], [0x1ff, 0x1ff, 0x1ff]],
            [0, 15, 7, 8, 3, 12, 1, 14, 5, 10, 2, 13, 6, 9, 4, 11],
        ),
        // The extremes of the ten-bit range, which unquantize to the ends.
        (
            [[0x000, 0x000, 0x000], [0x3ff, 0x3ff, 0x3ff]],
            [0, 0, 0, 0, 5, 5, 5, 5, 10, 10, 10, 10, 15, 15, 15, 15],
        ),
        // One channel held while the others move.
        (
            [[0x200, 0x000, 0x3ff], [0x200, 0x3ff, 0x000]],
            [7, 7, 8, 8, 7, 7, 8, 8, 1, 1, 14, 14, 1, 1, 14, 14],
        ),
    ];

    let mut payload = Vec::new();
    for (endpoints, indices) in blocks {
        payload.extend_from_slice(&one_subset_block(endpoints, indices));
    }
    payload
}

#[cfg(test)]
mod tests {
    use super::bc6h_payload;

    /// The committed payload is what this generator produces, so the file
    /// beside the fixtures is explained rather than opaque.
    #[test]
    fn payload_matches_the_committed_fixture() {
        let committed = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/bc6h/one-subset.bin"),
        )
        .expect("the committed BC6H payload is readable");
        assert_eq!(bc6h_payload(), committed);
    }
}
