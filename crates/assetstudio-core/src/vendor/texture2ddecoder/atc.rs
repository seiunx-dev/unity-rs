#![allow(clippy::identity_op)]
use super::bc3_alpha::decode_bc3_alpha;
use super::color::color;

#[inline]
const fn expand_quantized(v: u8, bits: u8) -> u8 {
    let v = v << (8 - bits);
    v | (v >> bits)
}

#[inline]
pub fn decode_atc_rgb4_block(data: &[u8], outbuf: &mut [u32]) {
    let mut colors: [u8; 16] = [0; 16];
    let c0: u32 = u16::from_le_bytes([data[0], data[1]]) as u32;
    let c1: u32 = u16::from_le_bytes([data[2], data[3]]) as u32;

    if 0 == (c0 & 0x8000) {
        colors[0] = expand_quantized(((c0 >> 0) & 0x1f) as u8, 5);
        colors[1] = expand_quantized(((c0 >> 5) & 0x1f) as u8, 5);
        colors[2] = expand_quantized(((c0 >> 10) & 0x1f) as u8, 5);

        colors[12] = expand_quantized(((c1 >> 0) & 0x1f) as u8, 5);
        colors[13] = expand_quantized(((c1 >> 5) & 0x3f) as u8, 6);
        colors[14] = expand_quantized(((c1 >> 11) & 0x1f) as u8, 5);

        #[inline]
        const fn interop_colors(c0: u8, c1: u8) -> u8 {
            ((5 * c0 as u16 + 3 * c1 as u16) / 8) as u8
        }
        // colors[4] = (5 * colors[0] + 3 * colors[12]) / 8;
        // colors[5] = (5 * colors[1] + 3 * colors[13]) / 8;
        // colors[6] = (5 * colors[2] + 3 * colors[14]) / 8;

        // colors[8] = (3 * colors[0] + 5 * colors[12]) / 8;
        // colors[9] = (3 * colors[1] + 5 * colors[13]) / 8;
        // colors[10] = (3 * colors[2] + 5 * colors[14]) / 8;

        colors[4] = interop_colors(colors[0], colors[12]);
        colors[5] = interop_colors(colors[1], colors[13]);
        colors[6] = interop_colors(colors[2], colors[14]);

        colors[8] = interop_colors(colors[12], colors[0]);
        colors[9] = interop_colors(colors[13], colors[1]);
        colors[10] = interop_colors(colors[14], colors[2]);
    } else {
        colors[0] = 0;
        colors[1] = 0;
        colors[2] = 0;

        colors[8] = expand_quantized(((c0 >> 0) & 0x1f) as u8, 5);
        colors[9] = expand_quantized(((c0 >> 5) & 0x1f) as u8, 5);
        colors[10] = expand_quantized(((c0 >> 10) & 0x1f) as u8, 5);

        colors[12] = expand_quantized(((c1 >> 0) & 0x1f) as u8, 5);
        colors[13] = expand_quantized(((c1 >> 5) & 0x3f) as u8, 6);
        colors[14] = expand_quantized(((c1 >> 11) & 0x1f) as u8, 5);

        // VENDOR FIX: the palette entry is `max(0, c0 - c1 / 4)`, saturating.
        // Upstream divides the difference instead of the subtrahend, subtracts
        // with `overflowing_sub` on `u16`, and then clamps with `max(0, _)` on
        // an unsigned value, which can never clamp anything: whenever c0 < c1
        // the difference wraps to about 65530, the divide leaves ~16382 and the
        // `as u8` truncates that to whatever the low byte happens to be.
        // Verified against the managed decoder, which produces this expression
        // exactly on both alternate-mode blocks of the differential fixture.
        colors[4] = colors[8].saturating_sub(colors[12] / 4);
        colors[5] = colors[9].saturating_sub(colors[13] / 4);
        colors[6] = colors[10].saturating_sub(colors[14] / 4);
    }

    let mut next = 8 * 4;
    (0..16).for_each(|i| {
        let idx = (((data[next >> 3] >> (next & 7)) & 3) * 4) as usize;
        outbuf[i] = color(colors[idx + 2], colors[idx + 1], colors[idx + 0], 255);
        next += 2;
    });
}

#[inline]
pub fn decode_atc_rgba8_block(data: &[u8], outbuf: &mut [u32]) {
    decode_atc_rgb4_block(&data[8..], outbuf);
    decode_bc3_alpha(data, outbuf, 3);
}

/// Decodes a whole `ATC_RGB4` image.
///
/// Upstream generates this from a `block_decoder!` macro that needs the `paste`
/// crate; written out here for the same reason `decode_bc6_unsigned` is. The
/// body is that macro's expansion: 4x4 blocks, 8 bytes each, both bounds
/// checked before any block is touched.
pub(crate) fn decode_atc_rgb4(
    data: &[u8],
    width: usize,
    height: usize,
    image: &mut [u32],
) -> Result<(), &'static str> {
    decode_atc(data, width, height, image, 8, decode_atc_rgb4_block)
}

/// Decodes a whole `ATC_RGBA8` image: the same colour blocks with BC3 alpha.
pub(crate) fn decode_atc_rgba8(
    data: &[u8],
    width: usize,
    height: usize,
    image: &mut [u32],
) -> Result<(), &'static str> {
    decode_atc(data, width, height, image, 16, decode_atc_rgba8_block)
}

fn decode_atc(
    data: &[u8],
    width: usize,
    height: usize,
    image: &mut [u32],
    block_bytes: usize,
    decode_block: fn(&[u8], &mut [u32]),
) -> Result<(), &'static str> {
    const BLOCK_WIDTH: usize = 4;
    const BLOCK_HEIGHT: usize = 4;
    let num_blocks_x = width.div_ceil(BLOCK_WIDTH);
    let num_blocks_y = height.div_ceil(BLOCK_HEIGHT);
    let mut buffer: [u32; BLOCK_WIDTH * BLOCK_HEIGHT] = [0; BLOCK_WIDTH * BLOCK_HEIGHT];

    if data.len() < num_blocks_x * num_blocks_y * block_bytes {
        return Err("Not enough data to decode image!");
    }
    if image.len() < width * height {
        return Err("Image buffer is too small!");
    }

    let mut data_offset = 0;
    for by in 0..num_blocks_y {
        for bx in 0..num_blocks_x {
            decode_block(&data[data_offset..], &mut buffer);
            super::color::copy_block_buffer(
                bx,
                by,
                width,
                height,
                BLOCK_WIDTH,
                BLOCK_HEIGHT,
                &buffer,
                image,
            );
            data_offset += block_bytes;
        }
    }
    Ok(())
}
