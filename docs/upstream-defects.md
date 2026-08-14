# Upstream defects

Two dependencies produce output this project can show is wrong, and neither can
be fixed from here without taking over the code. Both are recorded with the
measurement that found them, a reproduction, and the change that fixes them, so
that filing either one upstream is a copy rather than a re-investigation.

The tests that pin these divergences name the file they would need to be
deleted from if the fix lands upstream, so nothing here goes stale silently.

---

## 1. `texture2ddecoder` 0.1.2 — `f32_to_u8` truncates where the reference rounds

**Affects** six ASTC HDR formats and BC6H — every HDR texture the decoder
handles, since the conversion runs on every pixel.

**Severity** each affected channel comes out one step low, never high. Between
4% and 14% of bytes in the project's fixtures.

### What is wrong

The C++ this crate was ported from converts a decoded half-float to a byte by
rounding:

```c
static inline uint8_t f32_to_u8(const float f) {
    float c = roundf(f * 255);
    if (c < 0) return 0;
    else if (c > 255) return 255;
    else return c;
}
```

Two ports of that function both lost the rounding, in different ways.

`src/astc.rs`, in `select_color_hdr`:

```rust
(floor(f * 255.0) as i32).clamp(0, 255) as u8
```

`src/bcn/bc6.rs`, in `f32_to_u8`:

```rust
(f * 255.0).clamp(0.0, 255.0) as u8
```

`as u8` truncates, so the second is `floor` written another way. The LDR ASTC
path uses a separate integer conversion and is unaffected, which is why twelve
LDR formats match the reference exactly while all six HDR ones do not.

### The fix

```diff
--- a/src/astc.rs
+++ b/src/astc.rs
     let f: f32 = fp16_ieee_to_fp32_value((c >> 1 & 0x7c00) | m >> 3);
     if f32::is_finite(f) {
-        (floor(f * 255.0) as i32).clamp(0, 255) as u8
+        (floor(f * 255.0 + 0.5) as i32).clamp(0, 255) as u8
     } else {
         255
     }

--- a/src/bcn/bc6.rs
+++ b/src/bcn/bc6.rs
 fn f32_to_u8(f: f32) -> u8 {
-    (f * 255.0).clamp(0.0, 255.0) as u8
+    let scaled = f * 255.0;
+    if scaled <= 0.0 {
+        0
+    } else if scaled >= 255.0 {
+        255
+    } else {
+        (scaled + 0.5) as u8
+    }
 }
```

Adding a half before flooring is `roundf` for the non-negative values that
survive the clamp, and avoids `f32::round`, which the crate cannot use in
`no_std`.

### Reproduction

Decode any HDR payload with both this crate and the reference C++ decoder and
compare. In this repository:

* `cargo test -p assetstudio-core --lib bc6h_differs_from_the_managed_decoder_only_by_truncation`
* `cargo test -p assetstudio-core --lib hdr_astc_differs_from_the_managed_decoder_only_by_truncation`

Both compare against blobs of reference output committed beside the fixtures
(`tests/fixtures/bc6h/`, `tests/fixtures/astc/`), and both assert that every
difference is exactly one and in the same direction. Applying the diff above to
a local copy of the crate makes the output hash-identical to the reference for
all seven formats, which is also how the synthetic BC6H blocks were confirmed
valid: two independent decoders agreeing on all 256 bytes once the conversion
matches is not something a malformed block produces.

### Status

Present on `master` as of 2026-08-15; 0.1.2 is the latest release. Not filed.

---

## 2. `ruopus` 0.1.2 — SILK output is early and inexact

**Affects** FSB5 Opus decoding wherever the stream uses SILK or hybrid packets,
which is what libopus selects at lower bitrates. CELT-only packets are correct.

**Severity** the output arrives two samples early at wideband, four at
narrowband, and differs by roughly 3% of peak once aligned.

### What is wrong

Two independent libopus-based decoders — `ffmpeg` and `vgmstream` — agree with
each other to within one unit on the same packets and disagree with this crate.
Measured per packet mode, against a peak near 4200:

| packet mode     | offset | worst difference |
|-----------------|--------|------------------|
| CELT-only       |      0 |                1 |
| SILK/hybrid     |     -2 |              103 |
| SILK wideband   |     -2 |              135 |
| SILK narrowband |     -4 |              115 |

The offset tracks SILK's internal sample rate — four output samples at 8 kHz,
two at 16 kHz, the same fraction of an internal sample either way — which is
what a resampler delay compensated slightly differently looks like. CELT being
exact points at the SILK path rather than the shared framing.

Opus conformance is defined by a similarity metric rather than bit equality, so
some divergence is expected; a 3% amplitude error with a fixed sample offset is
larger than that explains.

### Reproduction

No code from this project is needed. Encode a tone with libopus at a bitrate
that selects SILK, feed the raw packets to `ruopus`, apply the stream's own
pre-skip, and compare against `ffmpeg -i` on the same file. Repeat at a bitrate
that selects CELT to see the difference disappear.

In this repository, `fsb5_opus_silk_tone_divergence_from_libopus_is_bounded`
pins the measurement and `fsb5_opus_celt_tone_matches_vgmstream` guards the half
that is correct; both need `vgmstream-cli` and run under `--ignored`.

### Status

Not filed. No workaround short of a different decoder, and the alternatives are
libopus bindings, which would end this crate's pure-Rust property.

---

## Why neither is fixed here

Both fixes are one expression. Applying either one in this repository means
vendoring the dependency — roughly 2,600 lines for the two texture decoders and
their helpers — which moves permanent maintenance of third-party code into this
project to correct, in the texture case, an error of at most 1/255 per channel.

That trade is a dependency-posture decision rather than a technical one, so it
is recorded here instead of taken. The tests hold the current behaviour in
place and will fail if either divergence changes shape or disappears.
