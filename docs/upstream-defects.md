# Upstream defects

Two dependencies produce output this project can show is wrong. Neither can be
corrected from outside the dependency, so the texture defects are fixed in
code this repository carries -- vendored or forked -- and the Opus one is
recorded and left alone. Each is written up with the measurement that found it
and a reproduction that does not depend on this project, so filing any of them
upstream is a copy rather than a re-investigation. The texture defects also
have their patches here; the Opus one does not, because it has been
characterised from the outside but not traced to a line.

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

* `cargo test -p unity-rs-core --lib bc6h_decodes_exactly_like_the_managed_decoder`
* `cargo test -p unity-rs-core --lib hdr_astc_decodes_exactly_like_the_managed_decoder`

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

**Fixed here by vendoring and forking.** BC6H comes from
`crates/unity-rs-core/src/vendor/texture2ddecoder/`, which carries that
decoder with `f32_to_u8` corrected and nothing else changed, so the copy
diffs cleanly against the published source. The ASTC decoder began the same
way and has since moved to the maintained first-party fork in
`crates/unity-rs-core/src/astc.rs`, which keeps the corrected
`select_color_hdr` rounding. Every other format still comes from the crate.
The six HDR ASTC formats and BC6H compare exactly against the managed
decoder, so the copies are held to the same standard as the rest of the
texture path rather than trusted because they were copied; the twelve LDR
ASTC formats are pinned against ARM's reference decoder instead, with the
managed relationship bounded separately (defect 3). Drop the vendored BC6H
copy and restore its call site in `texture.rs` when upstream releases the
fix.

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
shipping games this crate's LDR ASTC decode was, at the time, byte-identical
to the managed native decoder, and within one level per channel of ARM's
reference decoder -- including alpha, since an ASTC block decodes all four
channels through the same endpoint arithmetic and an `ASTC_RGB_*` texture can
still carry a varying alpha. Defect 3 below is that one level located and
resolved: the crate has since adopted the specification's conversion, so the
identity and the bound trade places -- byte-identical to ARM's reference
decoder, within one level of the managed one.
`tools/unitypy_texture_diff.py` checks the bound and reports anything outside
it.

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
`crates/unity-rs-core/src/vendor/texture2ddecoder/atc.rs` carries the
decoder with that one expression corrected. `ATC_RGB4` and `ATC_RGBA8` are now
in the managed differential and agree exactly, which is also how the defect
was found -- adding the two formats to the comparison was an audit's
suggestion, because until then neither had a test of any kind.

---

## 3. `texture2ddecoder` 0.1.2 — LDR interpolation rounds a rescale where the specification keeps the top byte

**Affects** the twelve LDR ASTC formats -- every LDR ASTC texture the decoder
handles, since the conversion runs on every texel.

**Severity** between 2.7% and 27% of bytes in the project's fixtures move by
exactly one level, in either direction, relative to the specification's
decode.

### What is wrong

The ASTC specification interpolates LDR endpoints at 16-bit precision and, in
its 8-bit `decode_unorm8` mode, keeps the top byte of the result:

```text
C = (C0 * (64 - w) + C1 * w + 32) >> 6    // 16-bit interpolation
out = C >> 8                              // the top byte is the 8-bit result
```

The AssetStudio lineage -- the managed decoder, the Texture2DDecoder C++, and
the `texture2ddecoder` port's `select_color` -- converts with a rounded
rescale instead:

```rust
((C * 255 + 32768) / 65536) as u8
```

The two functions agree on most inputs and differ by exactly one on the rest,
on both sides: `C = 0x00ff` decodes to 0 under the specification and 1 under
the rescale, `C = 0xff00` to 255 and 254. Every ASTC decoder that follows the
specification -- GPUs, ARM's `astcenc`, UnityPy through `astc_encoder` with
`USE_DECODE_UNORM8` -- produces the top-byte result, so the rescale makes the
lineage a one-level outlier on real textures rather than a different-but-equal
convention.

### Measurement

ARM's `astcenc` 5.7.0, the specification's reference codec, decompressed the
twelve committed LDR fixtures (`astcenc -dl` over the payloads wrapped in
`.astc` containers; `tools/decode_astc_references.py` reproduces this). This
crate's pre-change output disagreed with the reference on 2.7% to 27% of bytes
depending on footprint, every difference exactly one level; the managed
decoder carries the same disagreement, byte for byte. The same relationship
was measured on a shipping game's sprite atlases against UnityPy, which
matched the reference exactly while this crate and the managed decoder
differed from it by one level on the same texels.

### The fix

In the first-party fork `crates/unity-rs-core/src/astc.rs`, `select_color`
and the `LdrPartition` fast path now convert with `>> 8`.
`ldr_astc_decodes_exactly_like_the_khronos_reference` in `texture.rs` pins
the result against committed `astcenc` reference blobs byte for byte, and the
managed differential keeps the managed relationship as a declared divergence
bounded to one per byte; `tests/fixtures/astc/README.md` records both
contracts.

### Status

Present in `texture2ddecoder` 0.1.2 and on `master`, inherited from
AssetStudio's Texture2DDecoder. Not filed: in the upstream projects the
rescale is long-standing behaviour their consumers may depend on. This
project chose the specification's conversion because it is what GPUs and
every conformant decoder produce, and records the managed difference instead
of reproducing it.

---

## 4. `ruopus` 0.1.2 — SILK comparison has two measured profiles

**Affects** FSB5 Opus decoding wherever the stream uses SILK or hybrid packets,
which is what libopus selects at lower bitrates. CELT-only packets are correct.

**Severity** the output arrives two samples early at wideband, four at
narrowband, and differs by roughly 3% of peak once aligned.

The repository's formal Linux x86-64 gate is a notable exception: the pinned
`vgmstream` r2117 release and `ruopus` 0.1.2 produce identical PCM for the
checked SILK/hybrid fixture (`offset = 0`, `worst = 0`). That result was
independently reproduced in a Rust 1.88 Linux amd64 container. The earlier
measurements below came from a different local oracle/build environment. The
responsible build or platform difference has not yet been isolated, so the
test accepts exactly those two measured profiles rather than applying a broad
codec tolerance.

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
pins the exact Linux profile or the exact earlier `(-2, 276)` profile, while
`fsb5_opus_celt_tone_matches_vgmstream` independently guards the CELT path;
both need `vgmstream-cli` and run under `--ignored`.

### Status

Not filed. There is no downstream correction that can recover the samples the
decoder should have produced.

The independent pure-Rust `opus-rs` crate was evaluated before accepting a
native libopus dependency. Published releases 0.1.26 and 0.1.28 were each fed
the exact two packet fixtures used by this repository's `vgmstream` gate. The
comparison used a 48 kHz mono decoder, a 960-sample output buffer per packet,
the fixture's 312-sample pre-skip, and the crate's documented
`sample * 32768` conversion back to PCM16. Results against the same
`vgmstream-cli` output were:

| decoder | CELT offset | CELT worst | SILK/hybrid offset | SILK/hybrid worst |
|---------|------------:|-----------:|-------------------:|------------------:|
| `ruopus` 0.1.2 | 0 | 1 | -2 | 276 |
| `opus-rs` 0.1.26 | 0 | 36 | -5 | 164 |
| `opus-rs` 0.1.28 | 0 | 36 | -5 | 164 |

The lower SILK amplitude difference is not a fix: its timing moves three more
samples away from libopus, while the already-correct CELT path regresses from
one unit to 36. The 0.1.28 crates.io package and repository head
`a1d4c31f245ddeb007a219f3fed7f1e92a502304` produced the same measurements.
Consequently it is not a safe replacement either. The only known replacement
that passes the existing oracle remains a libopus binding, which would add a
second native dependency to a crate whose audio decoding is otherwise pure
Rust.

---

## Why the texture defects are fixed here and the Opus one is not

None can be corrected from outside the dependency. For the conversion defects
the reason is precise: the conversion is the decoder's last step, so nothing
downstream can recover what it discarded or misrounded. The ATC one is a
palette entry, equally interior. For the Opus defect it is that the divergence has
been characterised from the outside but not located in the source -- the
measurements above say what `ruopus` does and where it does it by packet mode,
not which line is responsible.

The texture defects are fixed in code this repository carries -- the vendored
BC6H and ATC decoders and the first-party ASTC fork -- a change of one
expression each. That was worth roughly 3,000 lines because it makes nine
texture formats byte-exact against the managed implementation and twelve more
byte-exact against ARM's reference decoder, and because the differentials
prove the copies correct rather than the copies being taken on trust.

The Opus defect is not, for two reasons. The equivalent step would be vendoring
or replacing an Opus decoder. The currently published independent pure-Rust
alternative was tested above and does not pass the oracle; the remaining known
alternative is a libopus binding, a second native dependency for a codec whose
CELT path is already correct. (The first is `zstd`, whose C sources this
workspace already builds, so the point is the cost of adding another rather
than a pure-Rust property to protect.) And there is no one-line fix to apply: finding it means working through
`ruopus`'s SILK resampler, which is upstream's work to do with the measurements
above rather than a patch waiting to be written. Its tests hold the current behaviour in place and will fail if the
divergence changes shape or disappears.
