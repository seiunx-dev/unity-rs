//! Vendored ASTC, BC6H and ATC decoders from `texture2ddecoder` 0.1.2.
//!
//! The workspace still depends on that crate for every other format. These two
//! decoders are copied here because both of their ports of the reference
//! `f32_to_u8` dropped its rounding, and the result is wrong output that cannot
//! be corrected from outside: the conversion is the decoder's last step and
//! nothing downstream can recover the value it discarded.
//!
//! The delta from upstream is two expressions, each marked `VENDOR FIX` in
//! place. Everything else is byte-for-byte the published source, so a diff
//! against the crate shows exactly what changed:
//!
//! * `astc.rs`, `select_color_hdr`: `floor(f * 255.0)` becomes
//!   `floor(f * 255.0 + 0.5)`.
//! * `bc6.rs`, `f32_to_u8`: the truncating `as u8` becomes an explicit
//!   clamp-then-round.
//! * `atc.rs`, the alternate-mode palette: `(c0 - c1) / 4` computed with
//!   `overflowing_sub` becomes `c0.saturating_sub(c1 / 4)`, which is what the
//!   format calls for. Upstream divides the difference rather than the
//!   subtrahend and then clamps an unsigned value with `max(0, _)`, which
//!   cannot clamp; whenever c0 < c1 the subtraction wraps and the entry
//!   becomes a byte of noise. This one is not a rounding step -- it moves
//!   whole channels by up to 200 of 255 on ordinary blocks.
//!
//! Both restore `roundf(f * 255)` from the C++ these were ported from. Without
//! them every HDR channel landing at or above a half comes out a step low --
//! six ASTC HDR formats and all of BC6H, since BC6H is HDR-only. The managed
//! differential compares both against the reference decoder and now requires
//! exact agreement, so this copy is held to the same standard as the rest of
//! the texture path rather than trusted because it was copied.
//!
//! Upstream is unfixed as of 2026-08-15 and 0.1.2 is the latest release;
//! `docs/upstream-defects.md` carries the patch in a form that can be filed.
//!
//! `texture2ddecoder` is MIT OR Apache-2.0. The MIT text and the upstream
//! copyright notice sit beside this file, and `THIRD_PARTY_NOTICES.md` records
//! the arrangement.

mod atc;
mod bc3_alpha;
mod bc6;
mod bitreader;
mod color;
mod consts;
// The first-party ASTC decoder in `crate::astc` (forked from the vendored
// copy for performance) shares this half-float conversion with BC6H.
pub(crate) mod f16;
pub(crate) use atc::{decode_atc_rgb4, decode_atc_rgba8};

/// Decodes a whole BC6H (unsigned) image.
///
/// Upstream generates this from a `block_decoder!` macro that needs the `paste`
/// crate; written out here instead so the vendored set stays to the files that
/// carry the fix. The body is that macro's expansion for BC6H: 4x4 blocks, 16
/// bytes each, both bounds checked before any block is touched.
pub(crate) fn decode_bc6_unsigned(
    data: &[u8],
    width: usize,
    height: usize,
    image: &mut [u32],
) -> Result<(), &'static str> {
    const BLOCK_WIDTH: usize = 4;
    const BLOCK_HEIGHT: usize = 4;
    const BLOCK_SIZE: usize = BLOCK_WIDTH * BLOCK_HEIGHT;
    const RAW_BLOCK_SIZE: usize = 16;

    let num_blocks_x = width.div_ceil(BLOCK_WIDTH);
    let num_blocks_y = height.div_ceil(BLOCK_HEIGHT);
    let mut buffer: [u32; BLOCK_SIZE] = [color::color(0, 0, 0, 255); BLOCK_SIZE];

    if data.len() < num_blocks_x * num_blocks_y * RAW_BLOCK_SIZE {
        return Err("Not enough data to decode image!");
    }
    if image.len() < width * height {
        return Err("Image buffer is too small!");
    }

    let mut data_offset = 0;
    for block_y in 0..num_blocks_y {
        for block_x in 0..num_blocks_x {
            bc6::decode_bc6_block_unsigned(&data[data_offset..], &mut buffer);
            color::copy_block_buffer(
                block_x,
                block_y,
                width,
                height,
                BLOCK_WIDTH,
                BLOCK_HEIGHT,
                &buffer,
                image,
            );
            data_offset += RAW_BLOCK_SIZE;
        }
    }
    Ok(())
}
