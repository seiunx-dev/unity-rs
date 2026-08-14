# Crunch differential fixtures

These CRN files come from commit
`2b1f1f2f45595058f9523578c8924c2cb14d2127` of
[`UniversalGameExtraction/texture2ddecoder`](https://github.com/UniversalGameExtraction/texture2ddecoder).
That is the exact source revision recorded by the packaged `texture2ddecoder
0.1.2` dependency. The upstream test README says the textures are encodings of
its repositioned `ferris_512.png`, derived from the original Ferris artwork at
[`rustacean.net`](https://rustacean.net/). The original Ferris artwork is
dedicated to the public domain under CC0 1.0.

The Rust tests compare complete decoded RGBA output against hashes produced by
this repository's bundled arm64 `Texture2DDecoderNative` C++ oracle. The
fixtures cover classic Crunch DXT1/DXT5 and UnityCrunch DXT1/DXT5/ETC1/ETC2A.
