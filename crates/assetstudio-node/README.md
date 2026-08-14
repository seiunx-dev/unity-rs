# assetstudio-rs-node

This package is a thin Node-API binding over `assetstudio-core`. It does not
define or route through AssetStudio's historical C ABI. Parsing, cross-file
resolution, limits, and format errors remain owned by the safe Rust core.

The first binding slice supports:

- path or in-memory loading;
- bounded file, object, and resource metadata pages;
- bounded resource, raw-object, `TextAsset`, TypeTree JSON/dump, `Shader`, and
  `Mesh` reads;
- bounded `Texture2D`, `Texture2DArray`, and `Sprite` decoding to tightly
  packed RGBA8 bytes.
- Promise-returning worker variants for path/buffer opening and the main
  raw/text/TypeTree/shader/mesh/image reads, backed by libuv's worker pool.

Build and test locally with Node.js 20.17 or newer:

```shell
npm install
npm run build:debug
npm test
```

Both synchronous methods and `*Async` worker variants are available. Prefer
the Promise-returning variants for large or untrusted inputs so parsing and
decoding do not block the JavaScript event loop; the same Rust core limits
apply to both paths.
