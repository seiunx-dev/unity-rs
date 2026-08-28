//! First-party ASTC (LDR and HDR) decoder.
//!
//! Forked from the vendored `texture2ddecoder` 0.1.2 copy -- itself a Rust
//! port of AssetStudio's Texture2DDecoder C++ -- which this file replaces on
//! the decode path. The fork exists for performance and robustness rather
//! than a one-expression fix, so it lives here as maintained first-party code
//! instead of under `vendor`'s minimal-delta contract:
//!
//! * every bit field is extracted from the 16-byte block held in one `u128`,
//!   so the former slice-arithmetic reads -- which could panic on hostile
//!   blocks -- become guarded shifts that cannot leave the block;
//! * a hostile weight count is rejected with an error (surfacing as the same
//!   whole-image `InvalidData` the contained panic used to produce) instead
//!   of relying on `catch_unwind`.
//!
//! The decode arithmetic follows the vendored copy with one deliberate
//! departure: LDR interpolation converts its 16-bit result to 8 bits by
//! taking the top byte, as the ASTC specification's `unorm16` decode mode
//! and Khronos `astcenc` do, where the AssetStudio lineage rounds a
//! `* 255 / 65535` rescale instead. The two differ by at most one per byte;
//! `astcenc` reference blobs pin the LDR output exactly and the managed
//! differential carries the divergence as a declared bound (see
//! `tests/fixtures/astc/README.md`). HDR keeps the vendored arithmetic,
//! including its `VENDOR FIX` rounding restoration in `select_color_hdr`,
//! and still matches the managed decoder byte for byte.
//! `texture2ddecoder` is MIT OR Apache-2.0; the upstream notice sits beside
//! the remaining vendored copy in `vendor/texture2ddecoder`.
//!
//! The numeric-conversion lints are allowed as a set: the port keeps the
//! upstream decoder's `as` conversions verbatim so its arithmetic stays
//! diffable against the source it was forked from, and every conversion
//! operates on values the block geometry already bounds.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_lossless,
    clippy::too_many_lines,
    clippy::too_many_arguments,
    clippy::unreadable_literal,
    clippy::needless_range_loop,
    clippy::many_single_char_names,
    clippy::similar_names,
    clippy::doc_markdown,
    clippy::semicolon_if_nothing_returned,
    clippy::inline_always,
    clippy::float_cmp,
    clippy::verbose_bit_mask,
    clippy::match_bool
)]
use crate::vendor::texture2ddecoder::f16::fp16_ieee_to_fp32_value;

/// One 16-byte ASTC block, readable both as raw bytes and as one 128-bit
/// little-endian word for arbitrary bit-field extraction.
struct Block {
    bytes: [u8; 16],
    bits: u128,
}

impl Block {
    fn new(bytes: [u8; 16]) -> Self {
        Self {
            bytes,
            bits: u128::from_le_bytes(bytes),
        }
    }
}

/// Extracts `len` (< 32) bits starting at `offset`. Offsets at or past the
/// end of the block read as zero instead of leaving it; valid blocks never
/// take that branch.
#[inline]
fn getbits(bits: u128, offset: usize, len: usize) -> i32 {
    if offset >= 128 {
        return 0;
    }
    (((bits >> offset) as u64) & ((1u64 << len) - 1)) as i32
}

/// Extracts up to 64 bits starting at signed bit position `bit`, matching the
/// vendored reader's semantics: negative positions shift the low word left,
/// out-of-range positions read as zero.
#[inline]
fn getbits64(bits: u128, bit: isize, len: usize) -> u64 {
    if len == 0 {
        return 0;
    }
    let mask = if len >= 64 {
        u64::MAX
    } else {
        (1u64 << len) - 1
    };
    let window = if bit >= 0 {
        if bit >= 128 { 0 } else { (bits >> bit) as u64 }
    } else {
        let left = bit.unsigned_abs();
        if left >= 64 { 0 } else { (bits as u64) << left }
    };
    window & mask
}

#[inline]
const fn color(r: u8, g: u8, b: u8, a: u8) -> u32 {
    u32::from_le_bytes([b, g, r, a])
}

fn copy_block_buffer(
    bx: usize,
    by: usize,
    w: usize,
    h: usize,
    bw: usize,
    bh: usize,
    buffer: &[u32],
    image: &mut [u32],
) {
    let x = bw * bx;
    let copy_width = if bw * (bx + 1) > w { w - bw * bx } else { bw };
    let y_0 = by * bh;
    let copy_height = if bh * (by + 1) > h { h - y_0 } else { bh };
    let mut buffer_offset = 0;
    for y in y_0..y_0 + copy_height {
        let image_offset = y * w + x;
        image[image_offset..image_offset + copy_width]
            .copy_from_slice(&buffer[buffer_offset..buffer_offset + copy_width]);
        buffer_offset += bw;
    }
}

#[inline]
fn floor(x: f32) -> f32 {
    let mut i = x as i32;
    if x < 0.0 && x != i as f32 {
        i -= 1;
    }
    i as f32
}

static BIT_REVERSE_TABLE: [u8; 256] = [
    0x00, 0x80, 0x40, 0xC0, 0x20, 0xA0, 0x60, 0xE0, 0x10, 0x90, 0x50, 0xD0, 0x30, 0xB0, 0x70, 0xF0,
    0x08, 0x88, 0x48, 0xC8, 0x28, 0xA8, 0x68, 0xE8, 0x18, 0x98, 0x58, 0xD8, 0x38, 0xB8, 0x78, 0xF8,
    0x04, 0x84, 0x44, 0xC4, 0x24, 0xA4, 0x64, 0xE4, 0x14, 0x94, 0x54, 0xD4, 0x34, 0xB4, 0x74, 0xF4,
    0x0C, 0x8C, 0x4C, 0xCC, 0x2C, 0xAC, 0x6C, 0xEC, 0x1C, 0x9C, 0x5C, 0xDC, 0x3C, 0xBC, 0x7C, 0xFC,
    0x02, 0x82, 0x42, 0xC2, 0x22, 0xA2, 0x62, 0xE2, 0x12, 0x92, 0x52, 0xD2, 0x32, 0xB2, 0x72, 0xF2,
    0x0A, 0x8A, 0x4A, 0xCA, 0x2A, 0xAA, 0x6A, 0xEA, 0x1A, 0x9A, 0x5A, 0xDA, 0x3A, 0xBA, 0x7A, 0xFA,
    0x06, 0x86, 0x46, 0xC6, 0x26, 0xA6, 0x66, 0xE6, 0x16, 0x96, 0x56, 0xD6, 0x36, 0xB6, 0x76, 0xF6,
    0x0E, 0x8E, 0x4E, 0xCE, 0x2E, 0xAE, 0x6E, 0xEE, 0x1E, 0x9E, 0x5E, 0xDE, 0x3E, 0xBE, 0x7E, 0xFE,
    0x01, 0x81, 0x41, 0xC1, 0x21, 0xA1, 0x61, 0xE1, 0x11, 0x91, 0x51, 0xD1, 0x31, 0xB1, 0x71, 0xF1,
    0x09, 0x89, 0x49, 0xC9, 0x29, 0xA9, 0x69, 0xE9, 0x19, 0x99, 0x59, 0xD9, 0x39, 0xB9, 0x79, 0xF9,
    0x05, 0x85, 0x45, 0xC5, 0x25, 0xA5, 0x65, 0xE5, 0x15, 0x95, 0x55, 0xD5, 0x35, 0xB5, 0x75, 0xF5,
    0x0D, 0x8D, 0x4D, 0xCD, 0x2D, 0xAD, 0x6D, 0xED, 0x1D, 0x9D, 0x5D, 0xDD, 0x3D, 0xBD, 0x7D, 0xFD,
    0x03, 0x83, 0x43, 0xC3, 0x23, 0xA3, 0x63, 0xE3, 0x13, 0x93, 0x53, 0xD3, 0x33, 0xB3, 0x73, 0xF3,
    0x0B, 0x8B, 0x4B, 0xCB, 0x2B, 0xAB, 0x6B, 0xEB, 0x1B, 0x9B, 0x5B, 0xDB, 0x3B, 0xBB, 0x7B, 0xFB,
    0x07, 0x87, 0x47, 0xC7, 0x27, 0xA7, 0x67, 0xE7, 0x17, 0x97, 0x57, 0xD7, 0x37, 0xB7, 0x77, 0xF7,
    0x0F, 0x8F, 0x4F, 0xCF, 0x2F, 0xAF, 0x6F, 0xEF, 0x1F, 0x9F, 0x5F, 0xDF, 0x3F, 0xBF, 0x7F, 0xFF,
];

static WEIGHT_PREC_TABLE_A: [i32; 16] = [0, 0, 0, 3, 0, 5, 3, 0, 0, 0, 5, 3, 0, 5, 3, 0];
static WEIGHT_PREC_TABLE_B: [i32; 16] = [0, 0, 1, 0, 2, 0, 1, 3, 0, 0, 1, 2, 4, 2, 3, 5];

static CEM_TABLE_A: [usize; 19] = [0, 3, 5, 0, 3, 5, 0, 3, 5, 0, 3, 5, 0, 3, 5, 0, 3, 0, 0];
static CEM_TABLE_B: [usize; 19] = [8, 6, 5, 7, 5, 4, 6, 4, 3, 5, 3, 2, 4, 2, 1, 3, 1, 2, 1];

#[inline]
fn bit_reverse_u8(c: u8, bits: u8) -> u8 {
    let x = BIT_REVERSE_TABLE[c as usize].overflowing_shr(8 - bits as u32);
    match x.1 {
        false => x.0,
        true => 0,
    }
}

#[inline]
fn bit_reverse_u64(d: u64, bits: usize) -> u64 {
    let ret = (BIT_REVERSE_TABLE[(d & 0xff) as usize] as u64) << 56
        | (BIT_REVERSE_TABLE[(d >> 8 & 0xff) as usize] as u64) << 48
        | (BIT_REVERSE_TABLE[(d >> 16 & 0xff) as usize] as u64) << 40
        | (BIT_REVERSE_TABLE[(d >> 24 & 0xff) as usize] as u64) << 32
        | (BIT_REVERSE_TABLE[(d >> 32 & 0xff) as usize] as u64) << 24
        | (BIT_REVERSE_TABLE[(d >> 40 & 0xff) as usize] as u64) << 16
        | (BIT_REVERSE_TABLE[(d >> 48 & 0xff) as usize] as u64) << 8
        | (BIT_REVERSE_TABLE[(d >> 56 & 0xff) as usize] as u64);
    ret >> (64 - bits as u64)
}

#[inline]
const fn u8ptr_to_u16(ptr: &[u8]) -> u16 {
    u16::from_le_bytes([ptr[0], ptr[1]])
}

// #[inline]
// fn bit_transfer_signed(a: &mut i32, b: &mut i32) {
//     *b = (*b >> 1) | (*a & 0x80);
//     *a = (*a >> 1) & 0x3f;
//     if *a & 0x20 != 0 {
//         *a -= 0x40;
//     }
// }

#[inline]
fn bit_transfer_signed_alt(v: &mut [i32], a: usize, b: usize) {
    v[b] = (v[b] >> 1) | (v[a] & 0x80);
    v[a] = (v[a] >> 1) & 0x3f;
    if v[a] & 0x20 != 0 {
        v[a] -= 0x40;
    }
}

#[inline]
fn set_endpoint(
    endpoint: &mut [i32],
    r1: i32,
    g1: i32,
    b1: i32,
    a1: i32,
    r2: i32,
    g2: i32,
    b2: i32,
    a2: i32,
) {
    endpoint[0] = r1;
    endpoint[1] = g1;
    endpoint[2] = b1;
    endpoint[3] = a1;
    endpoint[4] = r2;
    endpoint[5] = g2;
    endpoint[6] = b2;
    endpoint[7] = a2;
}

#[inline]
fn set_endpoint_clamp(
    endpoint: &mut [i32],
    r1: i32,
    g1: i32,
    b1: i32,
    a1: i32,
    r2: i32,
    g2: i32,
    b2: i32,
    a2: i32,
) {
    endpoint[0] = r1.clamp(0, 255);
    endpoint[1] = g1.clamp(0, 255);
    endpoint[2] = b1.clamp(0, 255);
    endpoint[3] = a1.clamp(0, 255);
    endpoint[4] = r2.clamp(0, 255);
    endpoint[5] = g2.clamp(0, 255);
    endpoint[6] = b2.clamp(0, 255);
    endpoint[7] = a2.clamp(0, 255);
}

#[inline]
fn set_endpoint_blue(
    endpoint: &mut [i32],
    r1: i32,
    g1: i32,
    b1: i32,
    a1: i32,
    r2: i32,
    g2: i32,
    b2: i32,
    a2: i32,
) {
    endpoint[0] = (r1 + b1) >> 1;
    endpoint[1] = (g1 + b1) >> 1;
    endpoint[2] = b1;
    endpoint[3] = a1;
    endpoint[4] = (r2 + b2) >> 1;
    endpoint[5] = (g2 + b2) >> 1;
    endpoint[6] = b2;
    endpoint[7] = a2;
}

#[inline]
fn set_endpoint_blue_clamp(
    endpoint: &mut [i32],
    r1: i32,
    g1: i32,
    b1: i32,
    a1: i32,
    r2: i32,
    g2: i32,
    b2: i32,
    a2: i32,
) {
    endpoint[0] = ((r1 + b1) >> 1).clamp(0, 255);
    endpoint[1] = ((g1 + b1) >> 1).clamp(0, 255);
    endpoint[2] = b1.clamp(0, 255);
    endpoint[3] = a1.clamp(0, 255);
    endpoint[4] = ((r2 + b2) >> 1).clamp(0, 255);
    endpoint[5] = ((g2 + b2) >> 1).clamp(0, 255);
    endpoint[6] = b2.clamp(0, 255);
    endpoint[7] = a2.clamp(0, 255);
}

#[inline]
fn set_endpoint_hdr(
    endpoint: &mut [i32],
    r1: i32,
    g1: i32,
    b1: i32,
    a1: i32,
    r2: i32,
    g2: i32,
    b2: i32,
    a2: i32,
) {
    endpoint[0] = r1;
    endpoint[1] = g1;
    endpoint[2] = b1;
    endpoint[3] = a1;
    endpoint[4] = r2;
    endpoint[5] = g2;
    endpoint[6] = b2;
    endpoint[7] = a2;
}

#[inline]
fn set_endpoint_hdr_clamp(
    endpoint: &mut [i32],
    r1: i32,
    g1: i32,
    b1: i32,
    a1: i32,
    r2: i32,
    g2: i32,
    b2: i32,
    a2: i32,
) {
    endpoint[0] = r1.clamp(0, 0xfff);
    endpoint[1] = g1.clamp(0, 0xfff);
    endpoint[2] = b1.clamp(0, 0xfff);
    endpoint[3] = a1.clamp(0, 0xfff);
    endpoint[4] = r2.clamp(0, 0xfff);
    endpoint[5] = g2.clamp(0, 0xfff);
    endpoint[6] = b2.clamp(0, 0xfff);
    endpoint[7] = a2.clamp(0, 0xfff);
}

// typedef uint_fast8_t (*t_select_folor_func_ptr)(int, int, int);

// The `>> 8` is the ASTC specification's `unorm16` to 8-bit conversion: the
// interpolated 16-bit value keeps its top byte, matching Khronos `astcenc`.
// The AssetStudio-lineage decoders round `* 255 / 65535` here instead, which
// differs by one on some values; that divergence is pinned in the managed
// differential rather than reproduced.
#[inline]
const fn select_color(v0: i32, v1: i32, weight: i32) -> u8 {
    ((((v0 << 8 | v0) * (64 - weight) + (v1 << 8 | v1) * weight + 32) >> 6) >> 8) as u8
}

#[inline]
fn select_color_hdr(v0: i32, v1: i32, weight: i32) -> u8 {
    let c: u16 = (((v0 << 4) * (64 - weight) + (v1 << 4) * weight + 32) >> 6) as u16;
    let mut m: u16 = c & 0x7ff;
    if m < 512 {
        m *= 3;
    } else if m < 1536 {
        m = 4 * m - 512;
    } else {
        m = 5 * m - 2048;
    }
    let f: f32 = fp16_ieee_to_fp32_value((c >> 1 & 0x7c00) | m >> 3);
    if f32::is_finite(f) {
        // VENDOR FIX: the C++ this was ported from rounds here
        // (`roundf(f * 255)`); dropping it makes every HDR channel that
        // lands at or above a half come out a step low. Adding a half
        // before flooring is `roundf` for the values that survive the
        // clamp, and avoids `f32::round`, which is unavailable here.
        (floor(f * 255.0 + 0.5) as i32).clamp(0, 255) as u8
    } else {
        255
    }
}

#[inline]
fn f32_to_u8(f: f32) -> u8 {
    floor(f * 255.0).clamp(0.0, 255.0) as u8
}

#[inline]
fn f16ptr_to_u8(ptr: &[u8]) -> u8 {
    f32_to_u8(fp16_ieee_to_fp32_value(u16::from_le_bytes([
        ptr[0], ptr[1],
    ])))
}

struct BlockData {
    bw: usize,
    bh: usize,
    width: usize,
    height: usize,
    part_num: usize,
    dual_plane: bool,
    plane_selector: usize,
    weight_range: usize,
    weight_num: usize,
    cem: [usize; 4],
    cem_range: usize,
    endpoint_value_num: usize,
    endpoints: [[i32; 8]; 4],
    weights: [[i32; 2]; 144],
    partition: [usize; 144],
}

impl BlockData {
    const fn default() -> Self {
        Self {
            bw: 0,
            bh: 0,
            width: 0,
            height: 0,
            part_num: 0,
            dual_plane: false,
            plane_selector: 0,
            weight_range: 0,
            weight_num: 0,
            cem: [0; 4],
            cem_range: 0,
            endpoint_value_num: 0,
            endpoints: [[0; 8]; 4],
            weights: [[0; 2]; 144],
            partition: [0; 144],
        }
    }
}

#[derive(Clone, Copy, Default)]
struct IntSeqData {
    bits: u64,
    nonbits: u64,
}

/// Per-texel bilinear infill coefficients for one weight-grid geometry.
///
/// The infill that upsamples the encoded weight grid to the block's texels
/// depends only on the block footprint and the grid dimensions -- never on
/// block content -- yet the vendored decoder recomputed it for every block.
/// Profiling put that recomputation at ~42% of ASTC decode time.
#[derive(Clone, Copy, Default)]
struct InfillTexel {
    v: u16,
    w00: i16,
    w01: i16,
    w10: i16,
    w11: i16,
}

struct InfillTable {
    texels: [InfillTexel; 144],
}

/// Lazily built infill tables for every weight-grid geometry of one image.
///
/// Grid dimensions run 2 through 12 on each axis, so the cache is a fixed
/// 121-slot table; each slot is at most one bounded 1.4 KiB allocation per
/// image, filled the first time a block uses that geometry.
struct InfillCache {
    bw: usize,
    bh: usize,
    tables: Vec<Option<Box<InfillTable>>>,
}

impl InfillCache {
    fn new(bw: usize, bh: usize) -> Self {
        Self {
            bw,
            bh,
            tables: (0..121).map(|_| None).collect(),
        }
    }

    fn table(&mut self, width: usize, height: usize) -> &InfillTable {
        let index = (width - 2) * 11 + (height - 2);
        if self.tables[index].is_none() {
            self.tables[index] = Some(build_infill_table(self.bw, self.bh, width, height));
        }
        self.tables[index].as_deref().expect("slot was just filled")
    }
}

fn build_infill_table(bw: usize, bh: usize, width: usize, height: usize) -> Box<InfillTable> {
    let ds = (1024 + bw / 2) / (bw - 1);
    let dt = (1024 + bh / 2) / (bh - 1);
    let mut table = Box::new(InfillTable {
        texels: [InfillTexel::default(); 144],
    });
    let mut i = 0;
    for t in 0..bh {
        for s in 0..bw {
            let gs = (ds * s * (width - 1) + 32) >> 6;
            let gt = (dt * t * (height - 1) + 32) >> 6;
            let fs = gs & 0xf;
            let ft = gt & 0xf;
            let v = (gs >> 4) + (gt >> 4) * width;
            let w11 = ((fs * ft + 8) >> 4) as i32;
            table.texels[i] = InfillTexel {
                v: v as u16,
                w11: w11 as i16,
                w10: (ft as i32 - w11) as i16,
                w01: (fs as i32 - w11) as i16,
                w00: (16 - fs as i32 - ft as i32 + w11) as i16,
            };
            i += 1;
        }
    }
    table
}

/// Weight decode scratch reused across every block of one image, so the
/// 2.5 KiB of sequence and weight-value buffers are zeroed once instead of
/// once per block.
struct WeightScratch {
    seq: [IntSeqData; 128],
    wv: [i32; 128],
}

/// Reusable whole-image decode state. `decode_block_params` writes every
/// field it or the later stages read, so carrying one `BlockData` across
/// blocks never leaks one block's values into the next.
struct AstcState {
    data: BlockData,
    scratch: WeightScratch,
    cache: InfillCache,
}

impl AstcState {
    fn new(bw: usize, bh: usize) -> Self {
        Self {
            data: BlockData::default(),
            scratch: WeightScratch {
                seq: [IntSeqData::default(); 128],
                wv: [0; 128],
            },
            cache: InfillCache::new(bw, bh),
        }
    }
}

fn decode_intseq(
    block: &Block,
    offset: usize,
    a: usize,
    b: usize,
    count: usize,
    reverse: bool,
    out: &mut [IntSeqData],
) {
    static TRITS_TABLE: [[u64; 256]; 5] = [
        [
            0, 1, 2, 0, 0, 1, 2, 1, 0, 1, 2, 2, 0, 1, 2, 2, 0, 1, 2, 0, 0, 1, 2, 1, 0, 1, 2, 2, 0,
            1, 2, 0, 0, 1, 2, 0, 0, 1, 2, 1, 0, 1, 2, 2, 0, 1, 2, 2, 0, 1, 2, 0, 0, 1, 2, 1, 0, 1,
            2, 2, 0, 1, 2, 1, 0, 1, 2, 0, 0, 1, 2, 1, 0, 1, 2, 2, 0, 1, 2, 2, 0, 1, 2, 0, 0, 1, 2,
            1, 0, 1, 2, 2, 0, 1, 2, 2, 0, 1, 2, 0, 0, 1, 2, 1, 0, 1, 2, 2, 0, 1, 2, 2, 0, 1, 2, 0,
            0, 1, 2, 1, 0, 1, 2, 2, 0, 1, 2, 2, 0, 1, 2, 0, 0, 1, 2, 1, 0, 1, 2, 2, 0, 1, 2, 2, 0,
            1, 2, 0, 0, 1, 2, 1, 0, 1, 2, 2, 0, 1, 2, 0, 0, 1, 2, 0, 0, 1, 2, 1, 0, 1, 2, 2, 0, 1,
            2, 2, 0, 1, 2, 0, 0, 1, 2, 1, 0, 1, 2, 2, 0, 1, 2, 1, 0, 1, 2, 0, 0, 1, 2, 1, 0, 1, 2,
            2, 0, 1, 2, 2, 0, 1, 2, 0, 0, 1, 2, 1, 0, 1, 2, 2, 0, 1, 2, 2, 0, 1, 2, 0, 0, 1, 2, 1,
            0, 1, 2, 2, 0, 1, 2, 2, 0, 1, 2, 0, 0, 1, 2, 1, 0, 1, 2, 2, 0, 1, 2, 2,
        ],
        [
            0, 0, 0, 0, 1, 1, 1, 0, 2, 2, 2, 0, 2, 2, 2, 0, 0, 0, 0, 1, 1, 1, 1, 1, 2, 2, 2, 1, 0,
            0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 0, 2, 2, 2, 0, 2, 2, 2, 0, 0, 0, 0, 1, 1, 1, 1, 1, 2, 2,
            2, 1, 1, 1, 1, 0, 0, 0, 0, 0, 1, 1, 1, 0, 2, 2, 2, 0, 2, 2, 2, 0, 0, 0, 0, 1, 1, 1, 1,
            1, 2, 2, 2, 1, 2, 2, 2, 0, 0, 0, 0, 0, 1, 1, 1, 0, 2, 2, 2, 0, 2, 2, 2, 0, 0, 0, 0, 1,
            1, 1, 1, 1, 2, 2, 2, 1, 2, 2, 2, 0, 0, 0, 0, 0, 1, 1, 1, 0, 2, 2, 2, 0, 2, 2, 2, 0, 0,
            0, 0, 1, 1, 1, 1, 1, 2, 2, 2, 1, 0, 0, 0, 1, 0, 0, 0, 0, 1, 1, 1, 0, 2, 2, 2, 0, 2, 2,
            2, 0, 0, 0, 0, 1, 1, 1, 1, 1, 2, 2, 2, 1, 1, 1, 1, 1, 0, 0, 0, 0, 1, 1, 1, 0, 2, 2, 2,
            0, 2, 2, 2, 0, 0, 0, 0, 1, 1, 1, 1, 1, 2, 2, 2, 1, 2, 2, 2, 1, 0, 0, 0, 0, 1, 1, 1, 0,
            2, 2, 2, 0, 2, 2, 2, 0, 0, 0, 0, 1, 1, 1, 1, 1, 2, 2, 2, 1, 2, 2, 2, 1,
        ],
        [
            0, 0, 0, 2, 0, 0, 0, 2, 0, 0, 0, 2, 2, 2, 2, 2, 1, 1, 1, 2, 1, 1, 1, 2, 1, 1, 1, 2, 0,
            0, 0, 2, 0, 0, 0, 2, 0, 0, 0, 2, 0, 0, 0, 2, 2, 2, 2, 2, 1, 1, 1, 2, 1, 1, 1, 2, 1, 1,
            1, 2, 0, 0, 0, 2, 0, 0, 0, 2, 0, 0, 0, 2, 0, 0, 0, 2, 2, 2, 2, 2, 1, 1, 1, 2, 1, 1, 1,
            2, 1, 1, 1, 2, 0, 0, 0, 2, 0, 0, 0, 2, 0, 0, 0, 2, 0, 0, 0, 2, 2, 2, 2, 2, 1, 1, 1, 2,
            1, 1, 1, 2, 1, 1, 1, 2, 2, 2, 2, 2, 0, 0, 0, 2, 0, 0, 0, 2, 0, 0, 0, 2, 2, 2, 2, 2, 1,
            1, 1, 2, 1, 1, 1, 2, 1, 1, 1, 2, 1, 1, 1, 2, 0, 0, 0, 2, 0, 0, 0, 2, 0, 0, 0, 2, 2, 2,
            2, 2, 1, 1, 1, 2, 1, 1, 1, 2, 1, 1, 1, 2, 1, 1, 1, 2, 0, 0, 0, 2, 0, 0, 0, 2, 0, 0, 0,
            2, 2, 2, 2, 2, 1, 1, 1, 2, 1, 1, 1, 2, 1, 1, 1, 2, 1, 1, 1, 2, 0, 0, 0, 2, 0, 0, 0, 2,
            0, 0, 0, 2, 2, 2, 2, 2, 1, 1, 1, 2, 1, 1, 1, 2, 1, 1, 1, 2, 2, 2, 2, 2,
        ],
        [
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2,
            2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
            1, 1, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2,
            2, 2, 2, 2, 2, 2, 2, 2, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 2, 2, 2, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
            1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2,
            2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1,
            1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 2, 2, 2, 2,
        ],
        [
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2,
            2, 2, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 2, 2, 2, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2,
            2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
            1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
            1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
            1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2,
            2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2,
        ],
    ];
    static QUINTS_TABLE: [[u64; 128]; 3] = [
        [
            0, 1, 2, 3, 4, 0, 4, 4, 0, 1, 2, 3, 4, 1, 4, 4, 0, 1, 2, 3, 4, 2, 4, 4, 0, 1, 2, 3, 4,
            3, 4, 4, 0, 1, 2, 3, 4, 0, 4, 0, 0, 1, 2, 3, 4, 1, 4, 1, 0, 1, 2, 3, 4, 2, 4, 2, 0, 1,
            2, 3, 4, 3, 4, 3, 0, 1, 2, 3, 4, 0, 2, 3, 0, 1, 2, 3, 4, 1, 2, 3, 0, 1, 2, 3, 4, 2, 2,
            3, 0, 1, 2, 3, 4, 3, 2, 3, 0, 1, 2, 3, 4, 0, 0, 1, 0, 1, 2, 3, 4, 1, 0, 1, 0, 1, 2, 3,
            4, 2, 0, 1, 0, 1, 2, 3, 4, 3, 0, 1,
        ],
        [
            0, 0, 0, 0, 0, 4, 4, 4, 1, 1, 1, 1, 1, 4, 4, 4, 2, 2, 2, 2, 2, 4, 4, 4, 3, 3, 3, 3, 3,
            4, 4, 4, 0, 0, 0, 0, 0, 4, 0, 4, 1, 1, 1, 1, 1, 4, 1, 4, 2, 2, 2, 2, 2, 4, 2, 4, 3, 3,
            3, 3, 3, 4, 3, 4, 0, 0, 0, 0, 0, 4, 0, 0, 1, 1, 1, 1, 1, 4, 1, 1, 2, 2, 2, 2, 2, 4, 2,
            2, 3, 3, 3, 3, 3, 4, 3, 3, 0, 0, 0, 0, 0, 4, 0, 0, 1, 1, 1, 1, 1, 4, 1, 1, 2, 2, 2, 2,
            2, 4, 2, 2, 3, 3, 3, 3, 3, 4, 3, 3,
        ],
        [
            0, 0, 0, 0, 0, 0, 0, 4, 0, 0, 0, 0, 0, 0, 1, 4, 0, 0, 0, 0, 0, 0, 2, 4, 0, 0, 0, 0, 0,
            0, 3, 4, 1, 1, 1, 1, 1, 1, 4, 4, 1, 1, 1, 1, 1, 1, 4, 4, 1, 1, 1, 1, 1, 1, 4, 4, 1, 1,
            1, 1, 1, 1, 4, 4, 2, 2, 2, 2, 2, 2, 4, 4, 2, 2, 2, 2, 2, 2, 4, 4, 2, 2, 2, 2, 2, 2, 4,
            4, 2, 2, 2, 2, 2, 2, 4, 4, 3, 3, 3, 3, 3, 3, 4, 4, 3, 3, 3, 3, 3, 3, 4, 4, 3, 3, 3, 3,
            3, 3, 4, 4, 3, 3, 3, 3, 3, 3, 4, 4,
        ],
    ];

    if count == 0 {
        return;
    }

    match a {
        3 => decode_trit_sequence(block, offset, b, count, reverse, out, &TRITS_TABLE),
        5 => decode_quint_sequence(block, offset, b, count, reverse, out, &QUINTS_TABLE),
        _ => decode_plain_sequence(block, offset, b, count, reverse, out),
    }
}

#[allow(clippy::too_many_arguments)]
fn decode_trit_sequence(
    block: &Block,
    offset: usize,
    bit_count: usize,
    count: usize,
    reverse: bool,
    out: &mut [IntSeqData],
    table: &[[u64; 256]; 5],
) {
    const BIT_OFFSETS: [usize; 5] = [0, 2, 4, 5, 7];
    let mask = (1 << bit_count) - 1;
    let block_count = count.div_ceil(5);
    let block_size = 8 + 5 * bit_count;
    let last_count = (count + 4) % 5 + 1;
    let last_size = (block_size * last_count).div_ceil(5);
    let mut output_index = 0;
    let mut position = offset as isize;
    for block_index in 0..block_count {
        let size = sequence_block_size(block_index, block_count, block_size, last_size);
        let bits = read_sequence_block(block, &mut position, size, block_size, reverse);
        let table_index = ((bits >> bit_count & 3)
            | (bits >> (bit_count * 2) & 0xc)
            | (bits >> (bit_count * 3) & 0x10)
            | (bits >> (bit_count * 4) & 0x60)
            | (bits >> (bit_count * 5) & 0x80)) as usize;
        for item in 0..5 {
            if output_index == count {
                break;
            }
            out[output_index] = IntSeqData {
                bits: (bits >> (BIT_OFFSETS[item] + bit_count * item)) & mask,
                nonbits: table[item][table_index],
            };
            output_index += 1;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn decode_quint_sequence(
    block: &Block,
    offset: usize,
    bit_count: usize,
    count: usize,
    reverse: bool,
    out: &mut [IntSeqData],
    table: &[[u64; 128]; 3],
) {
    const BIT_OFFSETS: [usize; 3] = [0, 3, 5];
    let mask = (1 << bit_count) - 1;
    let block_count = count.div_ceil(3);
    let block_size = 7 + 3 * bit_count;
    let last_count = (count + 2) % 3 + 1;
    let last_size = (block_size * last_count).div_ceil(3);
    let mut output_index = 0;
    let mut position = offset as isize;
    for block_index in 0..block_count {
        let size = sequence_block_size(block_index, block_count, block_size, last_size);
        let bits = read_sequence_block(block, &mut position, size, block_size, reverse);
        let table_index = ((bits >> bit_count & 7)
            | (bits >> (bit_count * 2) & 0x18)
            | (bits >> (bit_count * 3) & 0x60)) as usize;
        for item in 0..3 {
            if output_index == count {
                break;
            }
            out[output_index] = IntSeqData {
                bits: (bits >> (BIT_OFFSETS[item] + bit_count * item)) & mask,
                nonbits: table[item][table_index],
            };
            output_index += 1;
        }
    }
}

fn sequence_block_size(index: usize, count: usize, size: usize, last_size: usize) -> usize {
    if index + 1 == count { last_size } else { size }
}

fn read_sequence_block(
    block: &Block,
    position: &mut isize,
    size: usize,
    stride: usize,
    reverse: bool,
) -> u64 {
    if reverse {
        let bits = bit_reverse_u64(getbits64(block.bits, *position - size as isize, size), size);
        *position -= stride as isize;
        bits
    } else {
        let bits = getbits64(block.bits, *position, size);
        *position += stride as isize;
        bits
    }
}

fn decode_plain_sequence(
    block: &Block,
    offset: usize,
    bit_count: usize,
    count: usize,
    reverse: bool,
    out: &mut [IntSeqData],
) {
    let mut position = offset as isize;
    if reverse {
        position -= bit_count as isize;
    }
    for value in out.iter_mut().take(count) {
        let bits = getbits(block.bits, position as usize, bit_count) as u8;
        value.bits = if reverse {
            u64::from(bit_reverse_u8(bits, bit_count as u8))
        } else {
            u64::from(bits)
        };
        value.nonbits = 0;
        position += if reverse {
            -(bit_count as isize)
        } else {
            bit_count as isize
        };
    }
}

fn decode_block_params(block: &Block, data: &mut BlockData) {
    decode_weight_grid(block, data);
    data.part_num = ((block.bytes[1] >> 3 & 3) + 1) as usize;
    data.weight_num = data.width * data.height * if data.dual_plane { 2 } else { 1 };
    let weight_bits = weight_bit_count(data);
    let (mut config_bits, cem_base) = decode_cem_config(block, data, weight_bits);
    if data.dual_plane {
        config_bits += 2;
        data.plane_selector = decode_plane_selector(block, data, weight_bits, cem_base);
    }
    let remain_bits = 128 - config_bits - weight_bits as usize;
    data.endpoint_value_num = data.cem[..data.part_num]
        .iter()
        .map(|cem| (cem >> 1 & 6) + 2)
        .sum();
    data.cem_range = select_cem_range(data.endpoint_value_num, remain_bits, data.cem_range);
}

fn decode_weight_grid(block: &Block, data: &mut BlockData) {
    data.dual_plane = (block.bytes[1] & 4) != 0;
    data.weight_range = ((block.bytes[0] >> 4 & 1) | (block.bytes[1] << 2 & 8)) as usize;
    if block.bytes[0] & 3 != 0 {
        decode_weight_grid_nonzero(block, data);
    } else {
        decode_weight_grid_zero(block, data);
    }
}

fn decode_weight_grid_nonzero(block: &Block, data: &mut BlockData) {
    data.weight_range |= (block.bytes[0] << 1 & 6) as usize;
    match block.bytes[0] & 0xc {
        0 => {
            data.width = ((u8ptr_to_u16(&block.bytes) >> 7 & 3) + 4) as usize;
            data.height = ((block.bytes[0] >> 5 & 3) + 2) as usize;
        }
        4 => {
            data.width = ((u8ptr_to_u16(&block.bytes) >> 7 & 3) + 8) as usize;
            data.height = ((block.bytes[0] >> 5 & 3) + 2) as usize;
        }
        8 => {
            data.width = ((block.bytes[0] >> 5 & 3) + 2) as usize;
            data.height = ((u8ptr_to_u16(&block.bytes) >> 7 & 3) + 8) as usize;
        }
        12 => decode_weight_grid_nonzero_tall(block, data),
        _ => {}
    }
}

fn decode_weight_grid_nonzero_tall(block: &Block, data: &mut BlockData) {
    if block.bytes[1] & 1 != 0 {
        data.width = ((block.bytes[0] >> 7 & 1) + 2) as usize;
        data.height = ((block.bytes[0] >> 5 & 3) + 2) as usize;
    } else {
        data.width = ((block.bytes[0] >> 5 & 3) + 2) as usize;
        data.height = ((block.bytes[0] >> 7 & 1) + 6) as usize;
    }
}

fn decode_weight_grid_zero(block: &Block, data: &mut BlockData) {
    data.weight_range |= (block.bytes[0] >> 1 & 6) as usize;
    match u8ptr_to_u16(&block.bytes) & 0x180 {
        0 => {
            data.width = 12;
            data.height = ((block.bytes[0] >> 5 & 3) + 2) as usize;
        }
        0x80 => {
            data.width = ((block.bytes[0] >> 5 & 3) + 2) as usize;
            data.height = 12;
        }
        0x100 => {
            data.width = ((block.bytes[0] >> 5 & 3) + 6) as usize;
            data.height = ((block.bytes[1] >> 1 & 3) + 6) as usize;
            data.dual_plane = false;
            data.weight_range &= 7;
        }
        0x180 => {
            let wide = block.bytes[0] & 0x20 != 0;
            data.width = if wide { 10 } else { 6 };
            data.height = if wide { 6 } else { 10 };
        }
        _ => {}
    }
}

fn weight_bit_count(data: &BlockData) -> i32 {
    let count = data.weight_num as i32;
    match WEIGHT_PREC_TABLE_A[data.weight_range] {
        3 => count * WEIGHT_PREC_TABLE_B[data.weight_range] + (count * 8 + 4) / 5,
        5 => count * WEIGHT_PREC_TABLE_B[data.weight_range] + (count * 7 + 2) / 3,
        _ => count * WEIGHT_PREC_TABLE_B[data.weight_range],
    }
}

fn decode_cem_config(block: &Block, data: &mut BlockData, weight_bits: i32) -> (usize, usize) {
    if data.part_num == 1 {
        data.cem[0] = (u8ptr_to_u16(&block.bytes[1..]) >> 5 & 0xf) as usize;
        return (17, 0);
    }
    let cem_base = (u8ptr_to_u16(&block.bytes[2..]) >> 7 & 3) as usize;
    if cem_base == 0 {
        let cem = (block.bytes[3] >> 1 & 0xf) as usize;
        data.cem[..data.part_num].fill(cem);
        return (29, 0);
    }
    for index in 0..data.part_num {
        data.cem[index] = ((block.bytes[3] >> (index + 1) & 1) as usize + cem_base - 1) << 2;
    }
    decode_cem_low_bits(block, data, weight_bits);
    (25 + data.part_num * 3, cem_base)
}

fn decode_cem_low_bits(block: &Block, data: &mut BlockData, weight_bits: i32) {
    match data.part_num {
        2 => {
            data.cem[0] |= (block.bytes[3] >> 3 & 3) as usize;
            data.cem[1] |= getbits(block.bits, 126 - weight_bits as usize, 2) as usize;
        }
        3 => {
            data.cem[0] |= (block.bytes[3] >> 4 & 1) as usize;
            data.cem[0] |= (getbits(block.bits, 122 - weight_bits as usize, 2) & 2) as usize;
            data.cem[1] |= getbits(block.bits, 124 - weight_bits as usize, 2) as usize;
            data.cem[2] |= getbits(block.bits, 126 - weight_bits as usize, 2) as usize;
        }
        4 => {
            for index in 0..4 {
                data.cem[index] |=
                    getbits(block.bits, 120 + index * 2 - weight_bits as usize, 2) as usize;
            }
        }
        _ => {}
    }
}

fn decode_plane_selector(
    block: &Block,
    data: &BlockData,
    weight_bits: i32,
    cem_base: usize,
) -> usize {
    let offset = if cem_base != 0 {
        130 - weight_bits as usize - data.part_num * 3
    } else {
        126 - weight_bits as usize
    };
    getbits(block.bits, offset, 2) as usize
}

fn select_cem_range(endpoint_count: usize, remain_bits: usize, fallback: usize) -> usize {
    for index in 0..CEM_TABLE_A.len() {
        let bits = match CEM_TABLE_A[index] {
            3 => endpoint_count * CEM_TABLE_B[index] + (endpoint_count * 8).div_ceil(5),
            5 => endpoint_count * CEM_TABLE_B[index] + (endpoint_count * 7).div_ceil(3),
            _ => endpoint_count * CEM_TABLE_B[index],
        };
        if bits <= remain_bits {
            return index;
        }
    }
    fallback
}

fn decode_endpoints_hdr7(endpoints: &mut [i32], v: &[i32]) {
    let modeval = (v[2] >> 4 & 0x8) | (v[1] >> 5 & 0x4) | (v[0] >> 6);
    let (major_component, mode) = {
        if (modeval & 0xc) != 0xc {
            (modeval >> 2, modeval & 3)
        } else if modeval != 0xf {
            (modeval & 3, 4)
        } else {
            (0, 5)
        }
    };
    let mut c: [i32; 4] = [v[0] & 0x3f, v[1] & 0x1f, v[2] & 0x1f, v[3] & 0x1f];

    match mode {
        0 => {
            c[3] |= v[3] & 0x60;
            c[0] |= v[3] >> 1 & 0x40;
            c[0] |= v[2] << 1 & 0x80;
            c[0] |= v[1] << 3 & 0x300;
            c[0] |= v[2] << 5 & 0x400;
            c[0] <<= 1;
            c[1] <<= 1;
            c[2] <<= 1;
            c[3] <<= 1;
        }
        1 => {
            c[1] |= v[1] & 0x20;
            c[2] |= v[2] & 0x20;
            c[0] |= v[3] >> 1 & 0x40;
            c[0] |= v[2] << 1 & 0x80;
            c[0] |= v[1] << 2 & 0x100;
            c[0] |= v[3] << 4 & 0x600;
            c[0] <<= 1;
            c[1] <<= 1;
            c[2] <<= 1;
            c[3] <<= 1;
        }
        2 => {
            c[3] |= v[3] & 0xe0;
            c[0] |= v[2] << 1 & 0xc0;
            c[0] |= v[1] << 3 & 0x300;
            c[0] <<= 2;
            c[1] <<= 2;
            c[2] <<= 2;
            c[3] <<= 2;
        }
        3 => {
            c[1] |= v[1] & 0x20;
            c[2] |= v[2] & 0x20;
            c[3] |= v[3] & 0x60;
            c[0] |= v[3] >> 1 & 0x40;
            c[0] |= v[2] << 1 & 0x80;
            c[0] |= v[1] << 2 & 0x100;
            c[0] <<= 3;
            c[1] <<= 3;
            c[2] <<= 3;
            c[3] <<= 3;
        }
        4 => {
            c[1] |= v[1] & 0x60;
            c[2] |= v[2] & 0x60;
            c[3] |= v[3] & 0x20;
            c[0] |= v[3] >> 1 & 0x40;
            c[0] |= v[3] << 1 & 0x80;
            c[0] <<= 4;
            c[1] <<= 4;
            c[2] <<= 4;
            c[3] <<= 4;
        }
        5 => {
            c[1] |= v[1] & 0x60;
            c[2] |= v[2] & 0x60;
            c[3] |= v[3] & 0x60;
            c[0] |= v[3] >> 1 & 0x40;
            c[0] <<= 5;
            c[1] <<= 5;
            c[2] <<= 5;
            c[3] <<= 5;
        }
        _ => {}
    }
    if mode != 5 {
        c[1] = c[0] - c[1];
        c[2] = c[0] - c[2];
    }
    match major_component {
        1 => {
            set_endpoint_hdr_clamp(
                endpoints,
                c[1] - c[3],
                c[0] - c[3],
                c[2] - c[3],
                0x780,
                c[1],
                c[0],
                c[2],
                0x780,
            );
        }
        2 => {
            set_endpoint_hdr_clamp(
                endpoints,
                c[2] - c[3],
                c[1] - c[3],
                c[0] - c[3],
                0x780,
                c[2],
                c[1],
                c[0],
                0x780,
            );
        }
        _ => {
            set_endpoint_hdr_clamp(
                endpoints,
                c[0] - c[3],
                c[1] - c[3],
                c[2] - c[3],
                0x780,
                c[0],
                c[1],
                c[2],
                0x780,
            );
        }
    }
}

fn decode_endpoints_hdr11(endpoints: &mut [i32], v: &[i32], alpha1: i32, alpha2: i32) {
    let major_component = (v[4] >> 7) | (v[5] >> 6 & 2);
    if major_component == 3 {
        set_endpoint_hdr(
            endpoints,
            v[0] << 4,
            v[2] << 4,
            v[4] << 5 & 0xfe0,
            alpha1,
            v[1] << 4,
            v[3] << 4,
            v[5] << 5 & 0xfe0,
            alpha2,
        );
        return;
    }
    let mode = (v[1] >> 7) | (v[2] >> 6 & 2) | (v[3] >> 5 & 4);
    let mut va = v[0] | (v[1] << 2 & 0x100);
    let mut vb0 = v[2] & 0x3f;
    let mut vb1 = v[3] & 0x3f;
    let mut vc = v[1] & 0x3f;
    let delta_bits = match mode {
        0 | 2 => 7,
        1 | 3 | 5 | 7 => 6,
        _ => 5,
    };
    let mut vd0 = sign_extend(v[4], delta_bits);
    let mut vd1 = sign_extend(v[5], delta_bits);

    match mode {
        0 => {
            vb0 |= v[2] & 0x40;
            vb1 |= v[3] & 0x40;
        }
        1 => {
            vb0 |= v[2] & 0x40;
            vb1 |= v[3] & 0x40;
            vb0 |= v[4] << 1 & 0x80;
            vb1 |= v[5] << 1 & 0x80;
        }
        2 => {
            va |= v[2] << 3 & 0x200;
            vc |= v[3] & 0x40;
        }
        3 => {
            va |= v[4] << 3 & 0x200;
            vc |= v[5] & 0x40;
            vb0 |= v[2] & 0x40;
            vb1 |= v[3] & 0x40;
        }
        4 => {
            va |= v[4] << 4 & 0x200;
            va |= v[5] << 5 & 0x400;
            vb0 |= v[2] & 0x40;
            vb1 |= v[3] & 0x40;
            vb0 |= v[4] << 1 & 0x80;
            vb1 |= v[5] << 1 & 0x80;
        }
        5 => {
            va |= v[2] << 3 & 0x200;
            va |= v[3] << 4 & 0x400;
            vc |= v[5] & 0x40;
            vc |= v[4] << 1 & 0x80;
        }
        6 => {
            va |= v[4] << 4 & 0x200;
            va |= v[5] << 5 & 0x400;
            va |= v[4] << 5 & 0x800;
            vc |= v[5] & 0x40;
            vb0 |= v[2] & 0x40;
            vb1 |= v[3] & 0x40;
        }
        7 => {
            va |= v[2] << 3 & 0x200;
            va |= v[3] << 4 & 0x400;
            va |= v[4] << 5 & 0x800;
            vc |= v[5] & 0x40;
        }
        _ => {}
    }

    let shamt = (mode >> 1) ^ 3;
    va <<= shamt;
    vb0 <<= shamt;
    vb1 <<= shamt;
    vc <<= shamt;
    let mult = 1 << shamt;
    vd0 *= mult;
    vd1 *= mult;

    match major_component {
        1 => {
            set_endpoint_hdr_clamp(
                endpoints,
                va - vb0 - vc - vd0,
                va - vc,
                va - vb1 - vc - vd1,
                alpha1,
                va - vb0,
                va,
                va - vb1,
                alpha2,
            );
        }
        2 => {
            set_endpoint_hdr_clamp(
                endpoints,
                va - vb1 - vc - vd1,
                va - vb0 - vc - vd0,
                va - vc,
                alpha1,
                va - vb1,
                va - vb0,
                va,
                alpha2,
            );
        }
        _ => {
            set_endpoint_hdr_clamp(
                endpoints,
                va - vc,
                va - vb0 - vc - vd0,
                va - vb1 - vc - vd1,
                alpha1,
                va,
                va - vb0,
                va - vb1,
                alpha2,
            );
        }
    }
}

fn sign_extend(value: i32, bits: u32) -> i32 {
    let shift = i32::BITS - bits;
    (value << shift) >> shift
}

fn decode_endpoints(block: &Block, data: &mut BlockData) {
    let mut seq: [IntSeqData; 32] = [IntSeqData::default(); 32];
    let mut ev: [i32; 32] = [0; 32];
    decode_intseq(
        block,
        if data.part_num == 1 { 17 } else { 29 },
        CEM_TABLE_A[data.cem_range],
        CEM_TABLE_B[data.cem_range],
        data.endpoint_value_num,
        false,
        &mut seq,
    );

    decode_endpoint_values(
        &seq,
        &mut ev,
        CEM_TABLE_A[data.cem_range],
        CEM_TABLE_B[data.cem_range],
        data.endpoint_value_num,
    );

    let mut v: &mut [i32] = &mut ev;
    for cem in 0..data.part_num {
        decode_endpoint_mode(&mut data.endpoints[cem], data.cem[cem], v);
        v = &mut v[(data.cem[cem] / 4 + 1) * 2..];
    }
}

fn decode_endpoint_values(
    sequence: &[IntSeqData],
    values: &mut [i32],
    encoding: usize,
    precision: usize,
    count: usize,
) {
    for index in 0..count {
        values[index] = match encoding {
            3 => expand_trit_endpoint(sequence[index], precision),
            5 => expand_quint_endpoint(sequence[index], precision),
            _ => expand_binary_endpoint(sequence[index].bits, precision),
        };
    }
}

fn expand_trit_endpoint(value: IntSeqData, precision: usize) -> i32 {
    const MULTIPLIERS: [usize; 7] = [0, 204, 93, 44, 22, 11, 5];
    let a = (value.bits & 1) * 0x1ff;
    let x = value.bits >> 1;
    let b = match precision {
        2 => 0b100010110 * x,
        3 => x << 7 | x << 2 | x,
        4 => x << 6 | x,
        5 => x << 5 | x >> 2,
        6 => x << 4 | x >> 4,
        _ => 0,
    };
    ((a & 0x80) | ((value.nonbits * MULTIPLIERS[precision] as u64 + b) ^ a) >> 2) as i32
}

fn expand_quint_endpoint(value: IntSeqData, precision: usize) -> i32 {
    const MULTIPLIERS: [usize; 6] = [0, 113, 54, 26, 13, 6];
    let a = (value.bits & 1) * 0x1ff;
    let x = value.bits >> 1;
    let b = match precision {
        2 => 0b100001100 * x,
        3 => x << 7 | x << 1 | x >> 1,
        4 => x << 6 | x >> 1,
        5 => x << 5 | x >> 3,
        _ => 0,
    };
    ((a & 0x80) | ((value.nonbits * MULTIPLIERS[precision] as u64 + b) ^ a) >> 2) as i32
}

fn expand_binary_endpoint(bits: u64, precision: usize) -> i32 {
    match precision {
        1 => (bits * 0xff) as i32,
        2 => (bits * 0x55) as i32,
        3 => (bits << 5 | bits << 2 | bits >> 1) as i32,
        4 => (bits << 4 | bits) as i32,
        5 => (bits << 3 | bits >> 2) as i32,
        6 => (bits << 2 | bits >> 4) as i32,
        7 => (bits << 1 | bits >> 6) as i32,
        8 => bits as i32,
        _ => 0,
    }
}

fn decode_endpoint_mode(endpoints: &mut [i32], mode: usize, v: &mut [i32]) {
    match mode {
        0 => {
            set_endpoint(endpoints, v[0], v[0], v[0], 255, v[1], v[1], v[1], 255);
        }
        1 => {
            let l0 = (v[0] >> 2) | (v[1] & 0xc0);
            let l1 = (l0 + (v[1] & 0x3f)).clamp(0, 255);
            set_endpoint(endpoints, l0, l0, l0, 255, l1, l1, l1, 255);
        }
        2 => {
            let y0;
            let y1;
            if v[0] <= v[1] {
                y0 = v[0] << 4;
                y1 = v[1] << 4;
            } else {
                y0 = (v[1] << 4) + 8;
                y1 = (v[0] << 4) - 8;
            }
            set_endpoint_hdr(endpoints, y0, y0, y0, 0x780, y1, y1, y1, 0x780);
        }
        3 => {
            let y0;
            let d;
            if v[0] & 0x80 != 0 {
                y0 = (v[1] & 0xe0) << 4 | (v[0] & 0x7f) << 2;
                d = (v[1] & 0x1f) << 2;
            } else {
                y0 = (v[1] & 0xf0) << 4 | (v[0] & 0x7f) << 1;
                d = (v[1] & 0x0f) << 1;
            }
            let y1 = (y0 + d).clamp(0, 0xfff);
            set_endpoint_hdr(endpoints, y0, y0, y0, 0x780, y1, y1, y1, 0x780);
        }
        4 => {
            set_endpoint(endpoints, v[0], v[0], v[0], v[2], v[1], v[1], v[1], v[3]);
        }
        5 => {
            bit_transfer_signed_alt(v, 1, 0);
            bit_transfer_signed_alt(v, 3, 2);
            v[1] += v[0];
            set_endpoint_clamp(
                endpoints,
                v[0],
                v[0],
                v[0],
                v[2],
                v[1],
                v[1],
                v[1],
                v[2] + v[3],
            );
        }
        6 => {
            set_endpoint(
                endpoints,
                (v[0] * v[3]) >> 8,
                (v[1] * v[3]) >> 8,
                (v[2] * v[3]) >> 8,
                255,
                v[0],
                v[1],
                v[2],
                255,
            );
        }
        7 => {
            decode_endpoints_hdr7(endpoints, v);
        }
        8 => {
            if v[0] + v[2] + v[4] <= v[1] + v[3] + v[5] {
                set_endpoint(endpoints, v[0], v[2], v[4], 255, v[1], v[3], v[5], 255);
            } else {
                set_endpoint_blue(endpoints, v[1], v[3], v[5], 255, v[0], v[2], v[4], 255);
            }
        }
        9 => {
            bit_transfer_signed_alt(v, 1, 0);
            bit_transfer_signed_alt(v, 3, 2);
            bit_transfer_signed_alt(v, 5, 4);
            if v[1] + v[3] + v[5] >= 0 {
                set_endpoint_clamp(
                    endpoints,
                    v[0],
                    v[2],
                    v[4],
                    255,
                    v[0] + v[1],
                    v[2] + v[3],
                    v[4] + v[5],
                    255,
                );
            } else {
                set_endpoint_blue_clamp(
                    endpoints,
                    v[0] + v[1],
                    v[2] + v[3],
                    v[4] + v[5],
                    255,
                    v[0],
                    v[2],
                    v[4],
                    255,
                );
            }
        }
        10 => {
            set_endpoint(
                endpoints,
                (v[0] * v[3]) >> 8,
                (v[1] * v[3]) >> 8,
                (v[2] * v[3]) >> 8,
                v[4],
                v[0],
                v[1],
                v[2],
                v[5],
            );
        }
        11 => {
            decode_endpoints_hdr11(endpoints, v, 0x780, 0x780);
        }
        12 => {
            if v[0] + v[2] + v[4] <= v[1] + v[3] + v[5] {
                set_endpoint(endpoints, v[0], v[2], v[4], v[6], v[1], v[3], v[5], v[7]);
            } else {
                set_endpoint_blue(endpoints, v[1], v[3], v[5], v[7], v[0], v[2], v[4], v[6]);
            }
        }
        13 => {
            bit_transfer_signed_alt(v, 1, 0);
            bit_transfer_signed_alt(v, 3, 2);
            bit_transfer_signed_alt(v, 5, 4);
            bit_transfer_signed_alt(v, 7, 6);

            if v[1] + v[3] + v[5] >= 0 {
                set_endpoint_clamp(
                    endpoints,
                    v[0],
                    v[2],
                    v[4],
                    v[6],
                    v[0] + v[1],
                    v[2] + v[3],
                    v[4] + v[5],
                    v[6] + v[7],
                );
            } else {
                set_endpoint_blue_clamp(
                    endpoints,
                    v[0] + v[1],
                    v[2] + v[3],
                    v[4] + v[5],
                    v[6] + v[7],
                    v[0],
                    v[2],
                    v[4],
                    v[6],
                );
            }
        }
        14 => {
            decode_endpoints_hdr11(endpoints, v, v[6], v[7]);
        }
        15 => {
            let mode = ((v[6] >> 7) & 1) | ((v[7] >> 6) & 2);
            v[6] &= 0x7f;
            v[7] &= 0x7f;
            if mode == 3 {
                decode_endpoints_hdr11(endpoints, v, v[6] << 5, v[7] << 5);
            } else {
                v[6] |= (v[7] << (mode + 1)) & 0x780;
                v[7] = ((v[7] & (0x3f >> mode)) ^ (0x20 >> mode)) - (0x20 >> mode);
                v[6] <<= 4 - mode;
                v[7] <<= 4 - mode;
                decode_endpoints_hdr11(endpoints, v, v[6], (v[6] + v[7]).clamp(0, 0xfff));
            }
        }
        _ => {
            panic!("Unsupported ASTC format");
        }
    }
}

fn decode_weights(
    block: &Block,
    data: &mut BlockData,
    scratch: &mut WeightScratch,
    cache: &mut InfillCache,
) {
    let a = WEIGHT_PREC_TABLE_A[data.weight_range] as usize;
    let b = WEIGHT_PREC_TABLE_B[data.weight_range] as usize;
    decode_intseq(block, 128, a, b, data.weight_num, true, &mut scratch.seq);
    unquantize_weights(
        &scratch.seq[..data.weight_num],
        &mut scratch.wv[..data.weight_num],
        a,
        b,
    );
    interpolate_weights(data, &scratch.wv, cache);
}

fn unquantize_weights(sequence: &[IntSeqData], values: &mut [i32], a: usize, b: usize) {
    if a == 0 {
        for (output, value) in values.iter_mut().zip(sequence) {
            *output = unquantize_binary_weight(value.bits, b);
            adjust_weight(output);
        }
        return;
    }
    if b == 0 {
        let scale = if a == 3 { 32 } else { 16 };
        for (output, value) in values.iter_mut().zip(sequence) {
            *output = (value.nonbits * scale) as i32;
        }
        return;
    }
    for (output, value) in values.iter_mut().zip(sequence) {
        *output = unquantize_mixed_weight(*value, a, b);
        let sign = (value.bits & 1) * 0x7f;
        *output = ((sign & 0x20) | ((*output as u64 ^ sign) >> 2)) as i32;
        adjust_weight(output);
    }
}

fn unquantize_binary_weight(bits: u64, precision: usize) -> i32 {
    match precision {
        1 => {
            if bits != 0 {
                63
            } else {
                0
            }
        }
        2 => (bits << 4 | bits << 2 | bits) as i32,
        3 => (bits << 3 | bits) as i32,
        4 => (bits << 2 | bits >> 2) as i32,
        5 => (bits << 1 | bits >> 4) as i32,
        _ => panic!("Unsupported ASTC format"),
    }
}

fn unquantize_mixed_weight(value: IntSeqData, a: usize, b: usize) -> i32 {
    match (a, b) {
        (3, 1) => (value.nonbits * 50) as i32,
        (3, 2) => (value.nonbits * 23) as i32 + if value.bits & 2 != 0 { 0b1000101 } else { 0 },
        (3, 3) => (value.nonbits * 11 + ((value.bits << 4 | value.bits >> 1) & 0b1100011)) as i32,
        (5, 1) => (value.nonbits * 28) as i32,
        (5, 2) => (value.nonbits * 13) as i32 + if value.bits & 2 != 0 { 0b1000010 } else { 0 },
        _ => panic!("Unsupported ASTC format"),
    }
}

fn adjust_weight(value: &mut i32) {
    if *value > 32 {
        *value += 1;
    }
}

fn interpolate_weights(data: &mut BlockData, values: &[i32], cache: &mut InfillCache) {
    let texel_count = data.bw * data.bh;
    let table = cache.table(data.width, data.height);
    let weights = &mut data.weights[..texel_count];
    let texels = &table.texels[..texel_count];
    if data.dual_plane {
        interpolate_dual_plane_weights(weights, texels, values, data.width);
    } else {
        interpolate_single_plane_weights(weights, texels, values, data.width);
    }
}

fn interpolate_dual_plane_weights(
    weights: &mut [[i32; 2]],
    texels: &[InfillTexel],
    values: &[i32],
    width: usize,
) {
    for (weight, texel) in weights.iter_mut().zip(texels) {
        let v = texel.v as usize * 2;
        let (w00, w01) = (i32::from(texel.w00), i32::from(texel.w01));
        let (w10, w11) = (i32::from(texel.w10), i32::from(texel.w11));
        for plane in 0..2 {
            let p00 = values[v + plane];
            let p01 = values[v + 2 + plane];
            let p10 = values[v + width * 2 + plane];
            let p11 = values[v + width * 2 + 2 + plane];
            weight[plane] = (p00 * w00 + p01 * w01 + p10 * w10 + p11 * w11 + 8) >> 4;
        }
    }
}

fn interpolate_single_plane_weights(
    weights: &mut [[i32; 2]],
    texels: &[InfillTexel],
    values: &[i32],
    width: usize,
) {
    for (weight, texel) in weights.iter_mut().zip(texels) {
        let v = texel.v as usize;
        weight[0] = (values[v] * i32::from(texel.w00)
            + values[v + 1] * i32::from(texel.w01)
            + values[v + width] * i32::from(texel.w10)
            + values[v + width + 1] * i32::from(texel.w11)
            + 8)
            >> 4;
    }
}

fn select_partition(block: &Block, data: &mut BlockData) {
    let seed = (i32::from_le_bytes(block.bytes[0..4].try_into().unwrap()) >> 13 & 0x3ff)
        | (data.part_num as i32 - 1) << 10;
    let random = partition_random(seed as u32);
    let seeds = partition_seeds(seed, random, data.part_num);
    let coordinate_scale = if data.bw * data.bh < 31 { 2 } else { 1 };
    let mut index = 0;
    for y in 0..data.bh {
        for x in 0..data.bw {
            data.partition[index] = partition_for_texel(
                x * coordinate_scale,
                y * coordinate_scale,
                &seeds,
                random,
                data.part_num,
            );
            index += 1;
        }
    }
}

fn partition_random(mut value: u32) -> u32 {
    value ^= value >> 15;
    value = value.overflowing_sub(value << 17).0;
    value = value.overflowing_add(value << 7).0;
    value = value.overflowing_add(value << 4).0;
    value ^= value >> 5;
    value = value.overflowing_add(value << 16).0;
    value ^= value >> 7;
    value ^= value >> 3;
    value ^= value << 6;
    value ^ (value >> 17)
}

fn partition_seeds(seed: i32, random: u32, partition_count: usize) -> [i32; 8] {
    let mut seeds = [0_i32; 8];
    for (index, value) in seeds.iter_mut().enumerate() {
        let nibble = random >> (index * 4) & 0xf;
        *value = (nibble * nibble) as i32;
    }
    let shifts = [
        if seed & 2 != 0 { 4 } else { 5 },
        if partition_count == 3 { 6 } else { 5 },
    ];
    for (index, value) in seeds.iter_mut().enumerate() {
        let shift_index = if seed & 1 != 0 {
            index % 2
        } else {
            1 - index % 2
        };
        *value >>= shifts[shift_index];
    }
    seeds
}

fn partition_for_texel(
    x: usize,
    y: usize,
    seeds: &[i32; 8],
    random: u32,
    partition_count: usize,
) -> usize {
    let x = x as i32;
    let y = y as i32;
    let scores = [
        (seeds[0] * x + seeds[1] * y + (random >> 14) as i32) & 0x3f,
        (seeds[2] * x + seeds[3] * y + (random >> 10) as i32) & 0x3f,
        if partition_count < 3 {
            0
        } else {
            (seeds[4] * x + seeds[5] * y + (random >> 6) as i32) & 0x3f
        },
        if partition_count < 4 {
            0
        } else {
            (seeds[6] * x + seeds[7] * y + (random >> 2) as i32) & 0x3f
        },
    ];
    if scores[0] >= scores[1] && scores[0] >= scores[2] && scores[0] >= scores[3] {
        0
    } else if scores[1] >= scores[2] && scores[1] >= scores[3] {
        1
    } else if scores[2] >= scores[3] {
        2
    } else {
        3
    }
}

/// Which colour endpoint modes decode a colour channel through the HDR path.
/// This is the `FUNC_TABLE_C` of the C++ original, reduced to the one bit it
/// actually carried: dispatching per texel through a function pointer cost four
/// indirect calls per texel and blocked inlining of `select_color`, which is a
/// handful of integer ops.
const CEM_HDR_C: [bool; 16] = [
    false, false, true, true, false, false, false, true, false, false, false, true, false, false,
    true, true,
];

/// The same for the alpha channel. It differs from `CEM_HDR_C` at index 14.
const CEM_HDR_A: [bool; 16] = [
    false, false, true, true, false, false, false, true, false, false, false, true, false, false,
    false, true,
];

#[inline(always)]
fn select_c(cem: usize, v0: i32, v1: i32, weight: i32) -> u8 {
    if CEM_HDR_C[cem] {
        select_color_hdr(v0, v1, weight)
    } else {
        select_color(v0, v1, weight)
    }
}

#[inline(always)]
fn select_a(cem: usize, v0: i32, v1: i32, weight: i32) -> u8 {
    if CEM_HDR_A[cem] {
        select_color_hdr(v0, v1, weight)
    } else {
        select_color(v0, v1, weight)
    }
}

/// Interpolation constants for one partition's four LDR channels.
///
/// `select_color` computes `((c0*(64-w) + c1*w + 32) >> 6) >> 8` per
/// channel; with `base = c0*64 + 32` and `delta = c1 - c0` the interpolated
/// value is exactly `base + delta*w` in `i32` (`c0,c1 <= 65535`, so every
/// term stays far inside the type and the sum is non-negative), which
/// trades two multiplies per channel for one.
#[derive(Clone, Copy, Default)]
struct LdrPartition {
    base: [i32; 4],
    delta: [i32; 4],
}

impl LdrPartition {
    fn new(endpoints: &[i32; 8]) -> Self {
        let mut base = [0_i32; 4];
        let mut delta = [0_i32; 4];
        for channel in 0..4 {
            let low = endpoints[channel] << 8 | endpoints[channel];
            let high = endpoints[channel + 4] << 8 | endpoints[channel + 4];
            base[channel] = low * 64 + 32;
            delta[channel] = high - low;
        }
        Self { base, delta }
    }

    #[inline]
    fn pixel(&self, weight: i32) -> u32 {
        let mut channels = [0_u8; 4];
        for index in 0..4 {
            channels[index] = (((self.base[index] + self.delta[index] * weight) >> 6) >> 8) as u8;
        }
        color(channels[0], channels[1], channels[2], channels[3])
    }
}

fn applicate_color(data: &BlockData, outbuf: &mut [u32]) {
    let count = data.bw * data.bh;
    let out = &mut outbuf[..count];
    let weights = &data.weights[..count];
    let ldr = (0..data.part_num).all(|p| !CEM_HDR_C[data.cem[p]] && !CEM_HDR_A[data.cem[p]]);
    if ldr && !data.dual_plane {
        applicate_ldr_color(data, out, weights);
    } else if data.dual_plane {
        applicate_dual_plane_color(data, out, weights);
    } else if data.part_num > 1 {
        applicate_partitioned_color(data, out, weights);
    } else {
        applicate_single_color(data, out, weights);
    }
}

fn applicate_ldr_color(data: &BlockData, out: &mut [u32], weights: &[[i32; 2]]) {
    if data.part_num == 1 {
        let partition = LdrPartition::new(&data.endpoints[0]);
        for (pixel, weight) in out.iter_mut().zip(weights) {
            *pixel = partition.pixel(weight[0]);
        }
        return;
    }
    let mut partitions = [LdrPartition::default(); 4];
    for (partition, endpoints) in partitions
        .iter_mut()
        .zip(&data.endpoints)
        .take(data.part_num)
    {
        *partition = LdrPartition::new(endpoints);
    }
    for ((pixel, weight), &partition) in out.iter_mut().zip(weights).zip(&data.partition) {
        *pixel = partitions[partition].pixel(weight[0]);
    }
}

fn applicate_dual_plane_color(data: &BlockData, out: &mut [u32], weights: &[[i32; 2]]) {
    let mut planes = [0_usize; 4];
    planes[data.plane_selector] = 1;
    if data.part_num == 1 {
        let endpoints = &data.endpoints[0];
        let cem = data.cem[0];
        for (pixel, weight) in out.iter_mut().zip(weights) {
            *pixel = interpolate_astc_color(endpoints, cem, *weight, &planes);
        }
        return;
    }
    for ((pixel, weight), &partition) in out.iter_mut().zip(weights).zip(&data.partition) {
        *pixel = interpolate_astc_color(
            &data.endpoints[partition],
            data.cem[partition],
            *weight,
            &planes,
        );
    }
}

fn interpolate_astc_color(
    endpoints: &[i32; 8],
    cem: usize,
    weights: [i32; 2],
    planes: &[usize; 4],
) -> u32 {
    color(
        select_c(cem, endpoints[0], endpoints[4], weights[planes[0]]),
        select_c(cem, endpoints[1], endpoints[5], weights[planes[1]]),
        select_c(cem, endpoints[2], endpoints[6], weights[planes[2]]),
        select_a(cem, endpoints[3], endpoints[7], weights[planes[3]]),
    )
}

fn applicate_partitioned_color(data: &BlockData, out: &mut [u32], weights: &[[i32; 2]]) {
    for ((pixel, weight), &partition) in out.iter_mut().zip(weights).zip(&data.partition) {
        let endpoints = &data.endpoints[partition];
        let cem = data.cem[partition];
        *pixel = interpolate_single_plane_color(endpoints, cem, weight[0]);
    }
}

fn applicate_single_color(data: &BlockData, out: &mut [u32], weights: &[[i32; 2]]) {
    let endpoints = &data.endpoints[0];
    let cem = data.cem[0];
    for (pixel, weight) in out.iter_mut().zip(weights) {
        *pixel = interpolate_single_plane_color(endpoints, cem, weight[0]);
    }
}

fn interpolate_single_plane_color(endpoints: &[i32; 8], cem: usize, weight: i32) -> u32 {
    color(
        select_c(cem, endpoints[0], endpoints[4], weight),
        select_c(cem, endpoints[1], endpoints[5], weight),
        select_c(cem, endpoints[2], endpoints[6], weight),
        select_a(cem, endpoints[3], endpoints[7], weight),
    )
}

/// Decodes one block into `outbuf`, or reports a hostile weight count.
///
/// The weight gate bounds every later `wv`/`seq` index: the bilinear infill
/// reads up to `(width * height + width) * planes + planes - 1`, so demanding
/// that stay inside the weight buffers rejects exactly the blocks whose
/// out-of-bounds reads the vendored decoder turned into contained panics.
/// Valid encoder output sits far below the gate.
fn decode_astc_block(
    block: &Block,
    block_width: usize,
    block_height: usize,
    state: &mut AstcState,
    outbuf: &mut [u32],
) -> Result<(), ()> {
    let pixel_count = block_width * block_height;
    if let Some(color) = astc_void_extent_color(block) {
        outbuf[..pixel_count].fill(color);
        return Ok(());
    }
    if is_invalid_astc_block(block) {
        outbuf[..pixel_count].fill(color(255, 0, 255, 255));
        return Ok(());
    }
    decode_regular_astc_block(block, block_width, block_height, state, outbuf)
}

fn astc_void_extent_color(block: &Block) -> Option<u32> {
    if block.bytes[0] != 0xfc || (block.bytes[1] & 1) != 1 {
        return None;
    }
    Some(if block.bytes[1] & 2 != 0 {
        color(
            f16ptr_to_u8(&block.bytes[8..]),
            f16ptr_to_u8(&block.bytes[10..]),
            f16ptr_to_u8(&block.bytes[12..]),
            f16ptr_to_u8(&block.bytes[14..]),
        )
    } else {
        color(
            block.bytes[9],
            block.bytes[11],
            block.bytes[13],
            block.bytes[15],
        )
    })
}

fn is_invalid_astc_block(block: &Block) -> bool {
    ((block.bytes[0] & 0xc3) == 0xc0 && (block.bytes[1] & 1) == 1) || (block.bytes[0] & 0xf) == 0
}

fn decode_regular_astc_block(
    block: &Block,
    block_width: usize,
    block_height: usize,
    state: &mut AstcState,
    outbuf: &mut [u32],
) -> Result<(), ()> {
    let AstcState {
        data: block_data,
        scratch,
        cache,
    } = state;
    block_data.bw = block_width;
    block_data.bh = block_height;
    decode_block_params(block, block_data);
    let planes = if block_data.dual_plane { 2 } else { 1 };
    let grid = block_data.width * block_data.height;
    if (grid + block_data.width) * planes + planes > 128 {
        return Err(());
    }
    decode_endpoints(block, block_data);
    decode_weights(block, block_data, scratch, cache);
    if block_data.part_num > 1 {
        select_partition(block, block_data);
    }
    applicate_color(block_data, outbuf);
    Ok(())
}

pub fn decode_astc(
    data: &[u8],
    width: usize,
    height: usize,
    block_width: usize,
    block_height: usize,
    image: &mut [u32],
) -> Result<(), &'static str> {
    let num_blocks_x = width.div_ceil(block_width);
    let num_blocks_y = height.div_ceil(block_height);
    let mut buffer: [u32; 144] = [0; 144];
    let mut state = AstcState::new(block_width, block_height);
    let mut data_offset = 0;

    if data.len() < num_blocks_x * num_blocks_y * 16 {
        return Err("Not enough data to decode image!");
    }
    if image.len() < width * height {
        return Err("Image buffer is too small!");
    }
    if block_width * block_height > 144 {
        return Err("Block size is too big!");
    }

    for by in 0..num_blocks_y {
        for bx in 0..num_blocks_x {
            let bytes: [u8; 16] = data[data_offset..data_offset + 16]
                .try_into()
                .expect("block slice is exactly 16 bytes");
            let block = Block::new(bytes);
            if decode_astc_block(&block, block_width, block_height, &mut state, &mut buffer)
                .is_err()
            {
                return Err("ASTC block weight count exceeds the format limit");
            }
            copy_block_buffer(
                bx,
                by,
                width,
                height,
                block_width,
                block_height,
                &buffer,
                image,
            );
            data_offset += 16;
        }
    }

    Ok(())
}
