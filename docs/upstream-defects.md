# Upstream defects

Two dependencies produce output this project can show is wrong. Neither can be
corrected from outside the dependency, so one is fixed by vendoring the code
that carries it and the other is recorded and left alone. Both are written up
with the measurement that found them and a reproduction that does not depend on
this project, so filing either upstream is a copy rather than a
re-investigation. The texture defect also has its patch here; the Opus one does
not, because it has been characterised from the outside but not traced to a
line.

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

* `cargo test -p assetstudio-core --lib bc6h_decodes_exactly_like_the_managed_decoder`
* `cargo test -p assetstudio-core --lib hdr_astc_decodes_exactly_like_the_managed_decoder`

Both compare against blobs of reference output committed beside the fixtures
(`tests/fixtures/bc6h/`, `tests/fixtures/astc/`). Before the fix they asserted
that every difference was exactly one and in the same direction; they now assert
there is none. Applying the diff above to an unmodified copy of the crate makes
its output hash-identical to the reference for all seven formats, which is also
how the synthetic BC6H blocks were confirmed valid: two independent decoders
agreeing on all 256 bytes once the conversion matches is not something a
malformed block produces.

### Status

Present on `master` as of 2026-08-15; 0.1.2 is the latest release. Not filed.

**Fixed here by vendoring.** `crates/assetstudio-core/src/vendor/texture2ddecoder/`
carries the ASTC and BC6H decoders with the two expressions above corrected
and nothing else changed, so the copy diffs cleanly against the published
source. Every other format still comes from the crate. All eighteen ASTC
formats and BC6H now compare exactly against the managed decoder, so the
copy is held to the same standard as the rest of the texture path rather
than trusted because it was copied. Drop the directory and restore the two
call sites in `texture.rs` when upstream releases the fix.

### Scope, and a correction

This defect is in the HDR path. `select_color_hdr` is reached by the six ASTC
HDR formats, and `f32_to_u8` by BC6H, which is HDR-only. LDR ASTC does not go
through either, so nothing below is evidence for this fix -- the evidence for
it is the managed differential, where all eighteen ASTC formats and BC6H are
compared against the reference decoder and required to agree exactly.

An earlier version of this section claimed otherwise. It measured an
`ASTC_RGB_6x6` sprite atlas from a shipping game, found this crate agreeing
with the managed native decoder byte for byte (1,382,400 bytes, FNV-1a
`c6687283ffa9acde`) while UnityPy differed by one level, and attributed
UnityPy's difference to the unpatched crate. That attribution was wrong twice
over: the texture is LDR, so this fix cannot touch it, and UnityPy does not
decode ASTC with `texture2ddecoder` at all -- it uses `astc_encoder`, a
binding to ARM's reference codec, with `USE_DECODE_UNORM8`.

What those measurements do show is worth keeping, stated correctly. On two
shipping games this crate's LDR ASTC decode is byte-identical to the managed
native decoder, and within one level per channel of ARM's reference decoder --
including alpha, since an ASTC block decodes all four channels through the
same endpoint arithmetic and an `ASTC_RGB_*` texture can still carry a varying
alpha. `tools/unitypy_texture_diff.py` checks exactly that and reports
anything outside it.

---

## 2. `texture2ddecoder` 0.1.2 — ATC's alternate-mode palette entry wraps

### What is wrong

`decode_atc_rgb4_block` builds the palette for the alternate interpolation
mode -- the one selected when bit 15 of the first colour is set -- like this:

```rust
colors[4] = max(
    0,
    ((colors[8] as u16).overflowing_sub(colors[12] as u16).0 / 4) as u8,
);
```

The entry is meant to be `max(0, c0 - c1 / 4)`. Three things go wrong in one
expression:

* the divide is applied to the difference rather than to `c1`;
* the subtraction is `overflowing_sub`, so `c0 < c1` wraps to about 65530
  instead of clamping;
* `max(0, _)` is applied to an unsigned value, so it can never clamp anything.

Whenever `c0 < c1` in any channel -- which is half the blocks that use this
mode -- the entry becomes whatever the low byte of `(65536 + c0 - c1) / 4`
happens to be.

### Measurement

The differential fixture is four 8-byte blocks of pseudo-random bytes, two of
which land in the alternate mode. Every mode-0 block agrees with the managed
decoder exactly. Both mode-1 blocks disagree, at the three texels that select
palette index 1:

| block | c0 | c1 | managed | this crate |
|---|---|---|---|---|
| 0 | (132,132,74) | (41,36,165) | (122,123,33) | (22,24,233) |
| 1 | (66,214,173) | (8,28,148) | (64,207,136) | (14,46,6) |

The managed values are `max(0, c0 - c1/4)` to the byte. The crate's are
`((c0 - c1) mod 65536) / 4` truncated to eight bits, to the byte. Channels
move by up to 200 of 255, so this is not a rounding step -- an ATC texture
using the mode decodes to the wrong colours.

### The fix

```diff
-        colors[4] = max(
-            0,
-            ((colors[8] as u16).overflowing_sub(colors[12] as u16).0 / 4) as u8,
-        );
+        colors[4] = colors[8].saturating_sub(colors[12] / 4);
```

and the same for `colors[5]` and `colors[6]`.

### Status

Present on `master` as of 2026-08-15; 0.1.2 is the latest release. Not filed.

**Fixed here by vendoring**, alongside the two rounding defects above:
`crates/assetstudio-core/src/vendor/texture2ddecoder/atc.rs` carries the
decoder with that one expression corrected. `ATC_RGB4` and `ATC_RGBA8` are now
in the managed differential and agree exactly, which is also how the defect
was found -- adding the two formats to the comparison was an audit's
suggestion, because until then neither had a test of any kind.

---

## 3. `ruopus` 0.1.2 — SILK output is early and inexact

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
libopus bindings, which would add a second native dependency to a crate whose
audio decoding is otherwise pure Rust.

---

## Why the texture defects are fixed here and the Opus one is not

None can be corrected from outside the dependency. For the two rounding
defects the reason is precise: the conversion is the decoder's last step, so
nothing downstream can recover what it discarded. The ATC one is a palette
entry, equally interior. For the Opus defect it is that the divergence has
been characterised from the outside but not located in the source -- the
measurements above say what `ruopus` does and where it does it by packet mode,
not which line is responsible.

The texture defects are fixed by vendoring the three decoders that carry them,
a change of one expression each. That was worth roughly 3,000 lines because it
makes nine texture formats byte-exact against the managed implementation, and
because the managed differential proves the copy correct rather than the copy
being taken on trust.

The Opus defect is not, for two reasons. The equivalent step would be vendoring
or replacing an Opus decoder, and the alternatives are libopus bindings: a
second native dependency, for a codec whose CELT path is already correct. (The
first is `zstd`, whose C sources this workspace already builds, so the point is
the cost of adding another rather than a pure-Rust property to protect.) And there is no one-line fix to apply: finding it means working through
`ruopus`'s SILK resampler, which is upstream's work to do with the measurements
above rather than a patch waiting to be written. Its tests hold the current behaviour in place and will fail if the
divergence changes shape or disappears.
