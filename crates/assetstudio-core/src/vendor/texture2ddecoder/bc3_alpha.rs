//! `decode_bc3_alpha` from `texture2ddecoder` 0.1.2, unchanged.
//!
//! `ATC_RGBA8` is an ATC colour block with a BC3 alpha block, so the vendored
//! ATC decoder needs this one function. The crate does not export it, and the
//! copy carries no fix: it is here only because its caller moved.

/// Writes the BC3 alpha block in `data` into `channel` of `outbuf`.
pub(super) fn decode_bc3_alpha(data: &[u8], outbuf: &mut [u32], channel: usize) {
    // use u16 to avoid overflow and replicate equivalent behavior to C++ code
    let mut a: [u16; 8] = [data[0] as u16, data[1] as u16, 0, 0, 0, 0, 0, 0];
    if a[0] > a[1] {
        a[2] = (a[0] * 6 + a[1]) / 7;
        a[3] = (a[0] * 5 + a[1] * 2) / 7;
        a[4] = (a[0] * 4 + a[1] * 3) / 7;
        a[5] = (a[0] * 3 + a[1] * 4) / 7;
        a[6] = (a[0] * 2 + a[1] * 5) / 7;
        a[7] = (a[0] + a[1] * 6) / 7;
    } else {
        a[2] = (a[0] * 4 + a[1]) / 5;
        a[3] = (a[0] * 3 + a[1] * 2) / 5;
        a[4] = (a[0] * 2 + a[1] * 3) / 5;
        a[5] = (a[0] + a[1] * 4) / 5;
        a[6] = 0;
        a[7] = 255;
    }

    let mut d: usize = (u64::from_le_bytes(data[..8].try_into().unwrap()) >> 16) as usize;

    let channel_shift = channel * 8;
    let channel_mask = 0xFFFFFFFF ^ (0xFF << channel_shift);
    outbuf.iter_mut().for_each(|p| {
        *p = (*p & channel_mask) | (a[d & 7] as u32) << channel_shift;
        d >>= 3;
    });
}
