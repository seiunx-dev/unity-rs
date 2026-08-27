'use strict'

const assert = require('node:assert/strict')
const fs = require('node:fs')
const os = require('node:os')
const path = require('node:path')
const addon = require('../index.js')

assert.deepStrictEqual(Object.keys(addon).sort(), ['UnityRs'])

function u32(value) {
  const output = Buffer.alloc(4)
  output.writeUInt32LE(value)
  return output
}

function i32(value) {
  const output = Buffer.alloc(4)
  output.writeInt32LE(value)
  return output
}

function i64(value) {
  const output = Buffer.alloc(8)
  output.writeBigInt64LE(BigInt(value))
  return output
}

function f32(value) {
  const output = Buffer.alloc(4)
  output.writeFloatLE(value)
  return output
}

function align(buffer, alignment) {
  const padding = (alignment - (buffer.length % alignment)) % alignment
  return padding === 0 ? buffer : Buffer.concat([buffer, Buffer.alloc(padding)])
}

function alignedString(value) {
  const bytes = Buffer.from(value, 'utf8')
  return align(Buffer.concat([i32(bytes.length), bytes]), 4)
}

// Wraps one tree-less object payload in a v22 little-endian serialized file.
function finishV22Asset(classId, payload, version = '2022.3.62f1') {
  let metadata = Buffer.concat([
    Buffer.from(`${version}\0`, 'ascii'),
    i32(13),
    Buffer.from([0]),
    i32(1),
    i32(classId),
    Buffer.from([0]),
    Buffer.from([0xff, 0xff]),
    Buffer.alloc(16),
    i32(1),
  ])
  metadata = align(Buffer.concat([Buffer.alloc(48), metadata]), 4).subarray(48)
  metadata = Buffer.concat([
    metadata,
    i64(7),
    i64(0),
    u32(payload.length),
    i32(0),
    i32(0),
    i32(0),
    i32(0),
    Buffer.from([0]),
  ])
  const dataOffset = Math.ceil((48 + metadata.length) / 16) * 16
  const header = Buffer.alloc(48)
  header.writeUInt32BE(22, 8)
  header.writeUInt32BE(metadata.length, 20)
  header.writeBigInt64BE(BigInt(dataOffset + payload.length), 24)
  header.writeBigInt64BE(BigInt(dataOffset), 32)
  return Buffer.concat([
    header,
    metadata,
    Buffer.alloc(dataOffset - 48 - metadata.length),
    payload,
  ])
}

function syntheticTextAsset() {
  const payload = Buffer.concat([
    alignedString('node fixture'),
    i32(10),
    Buffer.from('hello node'),
  ])
  return finishV22Asset(49, payload)
}

function syntheticLegacyAnimation() {
  let payload = Buffer.concat([pptr(31), Buffer.from([1])])
  payload = align(payload, 4)
  payload = Buffer.concat([
    payload,
    pptr(70),
    i32(2),
    pptr(71),
    pptr(72),
    Buffer.from([0xaa, 0xbb]),
  ])
  return finishV22Asset(111, payload)
}

function syntheticAnimatorOverrideController() {
  return finishV22Asset(
    221,
    Buffer.concat([
      alignedString('node override controller'),
      pptr(90),
      i32(2),
      pptr(71),
      pptr(73),
      pptr(72),
      pptr(74),
      Buffer.from([0xcc]),
    ]),
  )
}

function syntheticContainerMetadataObjects() {
  const assetBundle = Buffer.concat([
    alignedString('root'),
    i32(2),
    pptr(11),
    pptr(12),
    i32(2),
    alignedString('bundle/first'),
    i32(0),
    i32(1),
    pptr(11),
    alignedString('bundle/second'),
    i32(1),
    i32(1),
    pptr(12),
    i32(0),
    i32(0),
    pptr(0),
    u32(0),
    alignedString('node-bundle'),
    i32(2),
    alignedString('shared-a'),
    alignedString('shared-b'),
    Buffer.from([0]),
  ])
  const resourceManager = Buffer.concat([
    i32(2),
    alignedString('resource/first'),
    pptr(21),
    alignedString('resource/second'),
    pptr(22),
  ])
  const preloadData = Buffer.concat([
    alignedString('node-preload'),
    i32(2),
    pptr(31),
    pptr(32),
  ])
  return finishV22Objects([
    { classId: 142, pathId: 7, payload: assetBundle },
    { classId: 147, pathId: 8, payload: resourceManager },
    { classId: 150, pathId: 9, payload: preloadData },
  ])
}

function syntheticGameObject() {
  return finishV22Asset(
    1,
    Buffer.concat([i32(0), i32(0), alignedString('Node Root')]),
  )
}

// A 2x2 RGBA32 Texture2D. Rows are stored bottom-up, so the first pixel in the
// payload is the BOTTOM-left one and a correct reader returns it last.
const TEXTURE_PIXELS = Buffer.from([
  255, 0, 0, 1, 0, 255, 0, 2, 0, 0, 255, 3, 255, 255, 255, 4,
])

// A Font whose payload is an OpenType blob, so the reader's extension guess has
// something to key on.
function syntheticFont() {
  const payload = Buffer.concat([
    alignedString('node-font'),
    Buffer.alloc(4), // line spacing
    Buffer.alloc(12), // default material PPtr
    Buffer.alloc(4), // font size
    Buffer.alloc(12), // texture PPtr
    Buffer.alloc(20),
    i32(0), // character rects
    i32(0), // kerning
    Buffer.alloc(4), // pixel scale
    i32(8),
    Buffer.from('OTTOfont'),
  ])
  return finishV22Asset(128, payload)
}

// A legacy MovieTexture, whose payload is an Ogg stream.
function syntheticMovieTexture() {
  const payload = Buffer.concat([
    alignedString('node-movie'),
    Buffer.alloc(8), // five colour-space bytes, aligned to four
    Buffer.from([1, 0, 0, 0]), // loop flag, aligned
    Buffer.alloc(12), // audio clip PPtr
    i32(4),
    Buffer.from('OggS'),
  ])
  return finishV22Asset(152, payload, '2018.4.36f1')
}

// Unity before 2.6 stores raw signed PCM16. Core can wrap it in a verified WAV
// without an external codec, which exercises a different path from simply
// copying an existing RIFF payload.
function syntheticLegacyPcm() {
  return finishV22Asset(
    83,
    Buffer.concat([
      alignedString('node-legacy-pcm'),
      i32(2), // one channel: the legacy field stores channels << 1
      f32(0),
      i32(22_050),
      i32(4),
      Buffer.from([1, 2, 3, 4]),
    ]),
    '2.5.0f1',
  )
}

function syntheticTexture2d() {
  let payload = Buffer.alloc(0)
  const push = (...parts) => {
    payload = Buffer.concat([payload, ...parts])
  }
  const pad4 = () => {
    payload = align(payload, 4)
  }
  const alignedStr = (value) => {
    const bytes = Buffer.from(value, 'utf8')
    push(i32(bytes.length), bytes)
    pad4()
  }

  alignedStr('image')
  push(i32(0), Buffer.from([0, 0]))
  pad4()
  push(i32(2), i32(2), u32(TEXTURE_PIXELS.length))
  push(i32(0), i32(4), i32(1))
  push(Buffer.from([0, 0, 0]))
  pad4()
  alignedStr('')
  push(Buffer.from([0]))
  pad4()
  push(i32(0), i32(1), i32(2))
  push(Buffer.alloc(24))
  push(i32(0), i32(0), i32(0))
  pad4()
  push(i32(TEXTURE_PIXELS.length), TEXTURE_PIXELS)
  return finishV22Asset(28, payload)
}

function syntheticTexture2dArray() {
  const layer0 = Buffer.from([1, 2, 3, 4, 5, 6, 7, 8])
  const layer1 = Buffer.from([11, 12, 13, 14, 15, 16, 17, 18])
  let payload = Buffer.concat([
    alignedString('array'),
    i32(0),
    Buffer.from([0]), // texture fallback settings
    Buffer.from([0]), // alpha-channel setting
  ])
  payload = align(payload, 4)
  payload = Buffer.concat([
    payload,
    i32(0), // color space
    i32(4), // GraphicsFormat R8G8B8A8_UNorm
    i32(1),
    i32(2),
    i32(2), // two layers
    i32(1), // one mip
    u32(layer0.length + layer1.length),
    Buffer.alloc(24),
    i32(7), // usage mode
    Buffer.from([1]), // readable
  ])
  payload = align(payload, 4)
  payload = Buffer.concat([
    payload,
    i32(layer0.length + layer1.length),
    layer0,
    layer1,
  ])
  return finishV22Asset(187, payload)
}

function pptr(pathId) {
  return Buffer.concat([i32(0), i64(pathId)])
}

function f32s(values) {
  return Buffer.concat(values.map(f32))
}

function texture2dPayload(name, width, height, pixels) {
  let payload = Buffer.concat([
    alignedString(name),
    i32(0),
    Buffer.from([0, 0]),
  ])
  payload = align(payload, 4)
  payload = Buffer.concat([
    payload,
    i32(width),
    i32(height),
    u32(pixels.length),
    i32(0),
    i32(4), // RGBA32
    i32(1), // one mip
    Buffer.from([0, 0, 0]),
  ])
  payload = align(payload, 4)
  payload = Buffer.concat([
    payload,
    alignedString(''),
    Buffer.from([0]),
  ])
  payload = align(payload, 4)
  return Buffer.concat([
    payload,
    i32(0),
    i32(1),
    i32(2),
    Buffer.alloc(24),
    i32(0),
    i32(0),
    i32(0), // empty platform blob
    i32(pixels.length),
    pixels,
  ])
}

function spriteAtlasRenderData(
  guidBytes,
  value,
  texturePathId,
  alphaTexturePathId,
  rectangle,
  textureOffset,
  atlasOffset,
  uvTransform,
  downscale,
  settings,
  secondaryTextures,
) {
  let payload = Buffer.concat([
    guidBytes,
    i64(value),
    pptr(texturePathId),
    pptr(alphaTexturePathId),
    f32s(rectangle),
    f32s(textureOffset),
    f32s(atlasOffset),
    f32s(uvTransform),
    f32(downscale),
    u32(settings),
    i32(secondaryTextures.length),
  ])
  for (const secondary of secondaryTextures) {
    payload = Buffer.concat([
      payload,
      pptr(secondary.pathId),
      alignedString(secondary.name),
    ])
  }
  return align(payload, 4)
}

function syntheticSpriteAtlas() {
  const higherKey = Buffer.concat([Buffer.from([1]), Buffer.alloc(15)])
  const lowerKey = Buffer.alloc(16)
  const payload = Buffer.concat([
    alignedString('node atlas'),
    i32(1),
    pptr(7),
    i32(1),
    alignedString('node sprite'),
    i32(2),
    // Deliberately serialize the higher key first. Core returns map entries in
    // deterministic raw-key order, and the binding must preserve that order.
    spriteAtlasRenderData(
      higherKey,
      9,
      11,
      0,
      [1, 2, 3, 4],
      [5, 6],
      [7, 8],
      [9, 10, 11, 12],
      0.5,
      79,
      [{ pathId: 99, name: 'mask' }],
    ),
    spriteAtlasRenderData(
      lowerKey,
      -5,
      10,
      12,
      [0, 0, 1, 1],
      [0, 0],
      [0, 0],
      [0, 0, 1, 1],
      1,
      2,
      [],
    ),
    alignedString('node-tag'),
    Buffer.from([1]),
  ])
  return finishV22Objects([
    { classId: 687_078_895, pathId: 9, payload: align(payload, 4) },
  ])
}

function syntheticSpriteMetadata() {
  let payload = Buffer.concat([
    alignedString('node sprite'),
    f32s([1, 2, 3, 4]),
    f32s([5, 6]),
    f32s([7, 8, 9, 10]),
    f32s([100, 0.25, 0.75]),
    u32(3),
    Buffer.from([1]),
  ])
  payload = align(payload, 4)
  payload = Buffer.concat([
    payload,
    Buffer.from(Array.from({ length: 16 }, (_, index) => index)),
    i64(-5),
    i32(1),
    alignedString('tag'),
    pptr(9),
    pptr(8),
    pptr(10),
    i32(1),
    pptr(12),
    alignedString('mask'),
    i32(0),
    i32(0),
  ])
  payload = align(payload, 4)
  payload = Buffer.concat([
    payload,
    u32(0),
    i32(1),
    Buffer.from([0, 0, 0, 3]),
    i32(0),
  ])
  payload = align(payload, 4)
  payload = Buffer.concat([
    payload,
    i32(0),
    f32s([11, 12, 13, 14]),
    f32s([15, 16, 17, 18]),
    u32(79),
    f32s([19, 20, 21, 22, 0.5]),
  ])
  return finishV22Asset(213, payload)
}

function syntheticTightSpriteMetadata() {
  let payload = Buffer.concat([
    alignedString('node tight sprite'),
    f32s([0, 0, 2, 2]),
    f32s([0, 0]),
    f32s([0, 0, 0, 0]),
    f32s([1, 0.5, 0.5]),
    u32(0),
    Buffer.from([0]),
  ])
  payload = align(payload, 4)
  payload = Buffer.concat([
    payload,
    Buffer.alloc(16),
    i64(0),
    i32(0),
    pptr(0),
    pptr(8),
    pptr(0),
    i32(0),
    i32(1),
    u32(0),
    u32(3),
    i32(0),
    u32(0),
    u32(0),
    u32(3),
    f32s([0, 0, 0, 0, 0, 0]),
    i32(6),
    Buffer.from([0, 0, 1, 0, 2, 0]),
  ])
  payload = align(payload, 4)
  payload = Buffer.concat([
    payload,
    u32(3),
    i32(1),
    Buffer.from([0, 0, 0, 3]),
    i32(36),
    f32s([-1, -1, 0, 1.1, -1, 0, -1, 1.1, 0]),
  ])
  payload = align(payload, 4)
  payload = Buffer.concat([
    payload,
    i32(0),
    f32s([0, 0, 2, 2]),
    f32s([0, 0, 0, 0]),
    u32(0),
    f32s([0, 0, 1, 1, 1]),
  ])
  return finishV22Asset(213, payload)
}

function syntheticTuanjieAvatar() {
  const emptySkeleton = () => Buffer.concat([i32(0), i32(0), i32(0)])
  const animationXform = () => Buffer.alloc(10 * 4)
  let payload = Buffer.concat([
    alignedString('node-tuanjie-avatar'),
    u32(0),
    emptySkeleton(),
    i32(0),
    i32(0),
    i32(0),
    animationXform(),
    emptySkeleton(),
    i32(0),
    i32(0),
    i32(0),
    i32(0),
    i32(0),
    f32s([1, 0.5, 0.5, 0.5, 0.5, 0.05, 0.05, 0]),
    Buffer.from([0, 0, 0]),
  ])
  payload = align(payload, 4)
  payload = Buffer.concat([
    payload,
    i32(0),
    i32(0),
    i32(-1),
    animationXform(),
    emptySkeleton(),
    i32(0),
    i32(0),
    i32(1),
    u32(0xfeedbeef),
    alignedString('Root/Hips'),
    i32(0),
    i32(0),
    f32s([0.5, 0.5, 0.5, 0.5, 0.05, 0.05, 0, 1]),
    alignedString('Hips'),
    Buffer.from([1, 0, 1]),
  ])
  return finishV22Asset(90, align(payload, 4), '2022.3.55t4')
}

function materialPayload(texturePathId) {
  return Buffer.concat([
    alignedString('node-material'),
    i32(1),
    i64(42),
    i32(2),
    alignedString('FOO'),
    alignedString('BAR'),
    i32(1),
    alignedString('OLD'),
    u32(3),
    Buffer.from([1, 0, 0, 0]),
    i32(2450),
    i32(2),
    alignedString('RenderType'),
    alignedString('Opaque'),
    alignedString('RenderType'),
    alignedString('Cutout'),
    i32(1),
    alignedString('ShadowCaster'),
    i32(1),
    alignedString('_MainTex'),
    pptr(texturePathId),
    f32s([2, 3, 0.25, 0.5]),
    i32(2),
    alignedString('_Mode'),
    i32(1),
    alignedString('_Mode'),
    i32(2),
    i32(1),
    alignedString('_Glossiness'),
    f32(0.75),
    i32(1),
    alignedString('_Color'),
    f32s([1, 0.5, 0.25, 1]),
    i32(0),
  ])
}

function modelGameObject() {
  return Buffer.concat([
    i32(3),
    pptr(11),
    pptr(21),
    pptr(31),
    i32(0),
    alignedString('node model'),
  ])
}

function modelTransform() {
  return Buffer.concat([
    pptr(1),
    f32s([0, 0, 0.38268343, 0.9238795]),
    f32s([2, 3, 4]),
    f32s([2, 3, 4]),
    i32(0),
    pptr(0),
  ])
}

function modelRenderer() {
  let payload = Buffer.concat([
    pptr(1),
    Buffer.from([1, 2, 1, 0, 0, 0, 0, 0, 0, 0]),
  ])
  payload = align(payload, 4)
  return align(Buffer.concat([
    payload,
    u32(0xffffffff),
    i32(0), // renderer priority
    Buffer.alloc(36), // lightmap indexes and two tiling/offset vectors
    i32(1),
    pptr(41),
    Buffer.alloc(4), // static batch info
    pptr(0),
    pptr(0),
    pptr(0),
    Buffer.alloc(8), // sorting layer/id/order
  ]), 4)
}

function emptyPackedFloat() {
  return Buffer.concat([u32(0), f32(0), f32(0), i32(0), Buffer.alloc(4)])
}

function emptyPackedInt() {
  return Buffer.concat([u32(0), i32(0), Buffer.alloc(4)])
}

function modelMesh() {
  let payload = Buffer.concat([
    alignedString('node triangle'),
    i32(1),
    ...[0, 3, 0, 0, 0, 3].map(u32),
    Buffer.alloc(24),
    i32(0),
    i32(0),
    i32(0),
    u32(0),
    i32(0),
    i32(0),
    i32(0),
    i32(0),
    i32(0),
    Buffer.from([0, 1, 0, 0]),
    i32(0),
    i32(6),
    Buffer.from([0, 0, 1, 0, 2, 0]),
  ])
  payload = align(payload, 4)
  payload = Buffer.concat([
    payload,
    u32(3),
    i32(5),
    Buffer.from([0, 0, 0, 3]),
    Buffer.alloc(16),
    i32(36),
  ])
  for (const vertex of [[0, 0, 0], [1, 0, 0], [0, 1, 0]]) {
    payload = Buffer.concat([payload, f32s(vertex)])
  }
  payload = align(payload, 4)
  payload = Buffer.concat([
    payload,
    ...Array.from({ length: 4 }, emptyPackedFloat),
    ...Array.from({ length: 3 }, emptyPackedInt),
    emptyPackedFloat(),
    ...Array.from({ length: 2 }, emptyPackedInt),
    u32(0),
    Buffer.alloc(24),
    i32(0),
    i32(0),
    i32(0),
  ])
  payload = align(payload, 4)
  payload = Buffer.concat([payload, i32(0)])
  payload = align(payload, 4)
  payload = Buffer.concat([payload, Buffer.alloc(8)])
  payload = align(payload, 4)
  return Buffer.concat([payload, i64(0), u32(0), alignedString('')])
}

function finishV22Objects(objects, version = '2022.3.62f1') {
  const classes = [...new Set(objects.map(({ classId }) => classId))]
    .sort((left, right) => left - right)
  let metadata = Buffer.concat([
    Buffer.from(`${version}\0`, 'ascii'),
    i32(13),
    Buffer.from([0]),
    i32(classes.length),
    ...classes.flatMap((classId) => [
      i32(classId),
      Buffer.from([0, 0xff, 0xff]),
      Buffer.alloc(16),
    ]),
  ])
  let data = Buffer.alloc(0)
  const records = []
  for (const object of objects) {
    data = align(data, 4)
    records.push({
      pathId: object.pathId,
      offset: data.length,
      size: object.payload.length,
      typeIndex: classes.indexOf(object.classId),
    })
    data = Buffer.concat([data, object.payload])
  }
  metadata = Buffer.concat([metadata, i32(records.length)])
  for (const record of records) {
    metadata = align(Buffer.concat([Buffer.alloc(48), metadata]), 4).subarray(48)
    metadata = Buffer.concat([
      metadata,
      i64(record.pathId),
      i64(record.offset),
      u32(record.size),
      i32(record.typeIndex),
    ])
  }
  metadata = Buffer.concat([
    metadata,
    i32(0),
    i32(0),
    i32(0),
    Buffer.from([0]),
  ])
  const dataOffset = Math.ceil((48 + metadata.length) / 16) * 16
  const header = Buffer.alloc(48)
  header.writeUInt32BE(22, 8)
  header.writeUInt32BE(metadata.length, 20)
  header.writeBigInt64BE(BigInt(dataOffset + data.length), 24)
  header.writeBigInt64BE(BigInt(dataOffset), 32)
  return Buffer.concat([
    header,
    metadata,
    Buffer.alloc(dataOffset - 48 - metadata.length),
    data,
  ])
}

function finishV22TypedObjects(types, objects, externals = [], version = '2022.3.62f1') {
  let metadata = Buffer.concat([
    Buffer.from(`${version}\0`, 'ascii'),
    i32(13),
    Buffer.from([1]), // type trees are enabled; individual types may be stripped
    i32(types.length),
    ...types.map(live2dTypeRecord),
  ])
  let data = Buffer.alloc(0)
  const records = []
  for (const object of objects) {
    data = align(data, 4)
    records.push({
      pathId: object.pathId,
      offset: data.length,
      size: object.payload.length,
      typeIndex: object.typeIndex,
    })
    data = Buffer.concat([data, object.payload])
  }
  metadata = Buffer.concat([metadata, i32(records.length)])
  for (const record of records) {
    metadata = align(Buffer.concat([Buffer.alloc(48), metadata]), 4).subarray(48)
    metadata = Buffer.concat([
      metadata,
      i64(record.pathId),
      i64(record.offset),
      u32(record.size),
      i32(record.typeIndex),
    ])
  }
  metadata = Buffer.concat([metadata, i32(0), i32(externals.length)])
  for (const external of externals) {
    metadata = Buffer.concat([
      metadata,
      Buffer.from([0]),
      Buffer.alloc(16),
      i32(0),
      Buffer.from(`${external}\0`, 'utf8'),
    ])
  }
  metadata = Buffer.concat([metadata, i32(0), Buffer.from([0])])
  const dataOffset = Math.ceil((48 + metadata.length) / 16) * 16
  const header = Buffer.alloc(48)
  header.writeUInt32BE(22, 8)
  header.writeUInt32BE(metadata.length, 20)
  header.writeBigInt64BE(BigInt(dataOffset + data.length), 24)
  header.writeBigInt64BE(BigInt(dataOffset), 32)
  return Buffer.concat([
    header,
    metadata,
    Buffer.alloc(dataOffset - 48 - metadata.length),
    data,
  ])
}

function live2dTypeRecord(type) {
  const chunks = [
    i32(type.classId),
    Buffer.from([0, 0xff, 0xff]),
  ]
  if (type.classId === 114) {
    chunks.push(Buffer.alloc(16, type.scriptHash ?? 0))
  }
  chunks.push(Buffer.alloc(16, 0x42))
  chunks.push(type.nodes == null ? Buffer.alloc(8) : live2dBlobTree(type.nodes))
  chunks.push(i32(0)) // v21+ type dependencies
  return Buffer.concat(chunks)
}

function live2dBlobTree(nodes) {
  const strings = []
  const records = []
  let stringBytes = 0
  for (let index = 0; index < nodes.length; index += 1) {
    const node = nodes[index]
    const typeBytes = Buffer.from(`${node.type}\0`, 'utf8')
    const nameBytes = Buffer.from(`${node.name}\0`, 'utf8')
    const typeOffset = stringBytes
    stringBytes += typeBytes.length
    const nameOffset = stringBytes
    stringBytes += nameBytes.length
    strings.push(typeBytes, nameBytes)
    records.push(Buffer.concat([
      Buffer.from([1, 0, node.level, 0]),
      u32(typeOffset),
      u32(nameOffset),
      i32(-1),
      i32(index),
      i32(node.align ? 0x4000 : 0),
      Buffer.alloc(8),
    ]))
  }
  return Buffer.concat([
    i32(nodes.length),
    i32(stringBytes),
    ...records,
    ...strings,
  ])
}

function live2dReferenceNodes(pointerType, pointerName) {
  return [
    ...cubismMonoBehaviourNodes(),
    ...cubismPptrNodes(pointerName, pointerType, 1),
  ]
}

function live2dPptr(fileId, pathId) {
  return Buffer.concat([i32(fileId), i64(pathId)])
}

function live2dGameObject(name, components) {
  return Buffer.concat([
    i32(components.length),
    ...components.map(([fileId, pathId]) => live2dPptr(fileId, pathId)),
    i32(0),
    Buffer.alloc(4), // Tuanjie editor-info flag and absolute alignment
    alignedString(name),
  ])
}

function live2dTransform(gameObject, children, father) {
  return Buffer.concat([
    live2dPptr(...gameObject),
    f32s([0, 0, 0, 1, 0, 0, 0, 1, 1, 1]),
    i32(children.length),
    ...children.map(([fileId, pathId]) => live2dPptr(fileId, pathId)),
    live2dPptr(...father),
  ])
}

function live2dMonoBehaviourPrefix(gameObject, script, name) {
  let payload = Buffer.concat([
    live2dPptr(...gameObject),
    Buffer.from([1]),
  ])
  payload = align(payload, 4)
  return Buffer.concat([
    payload,
    live2dPptr(...script),
    alignedString(name),
  ])
}

function live2dMonoBehaviour(gameObject, script, name, field) {
  return Buffer.concat([
    live2dMonoBehaviourPrefix(gameObject, script, name),
    live2dPptr(...field),
  ])
}

function live2dMocBehaviour(gameObject, script) {
  return Buffer.concat([
    live2dMonoBehaviourPrefix(gameObject, script, 'node-moc'),
    i32(5),
    Buffer.from('MOC3\x09', 'binary'),
  ])
}

function live2dMonoScript(className) {
  return Buffer.concat([
    alignedString('Cubism script'),
    i32(0),
    Buffer.alloc(16, 0x55),
    alignedString(className),
    alignedString('Live2D.Cubism.Core'),
    alignedString('Live2D.Cubism.dll'),
  ])
}

function live2dAnimator() {
  let payload = Buffer.concat([live2dPptr(0, 1), Buffer.from([1])])
  payload = align(payload, 4)
  return Buffer.concat([payload, live2dPptr(0, 0), live2dPptr(0, 31)])
}

function live2dAnimatorController() {
  let payload = Buffer.concat([
    alignedString('node live2d controller'),
    u32(0),
    ...Array.from({ length: 9 }, () => i32(0)),
    i32(1),
    u32(0xdeadbeef),
    alignedString('Hero'),
    i32(1),
    live2dPptr(0, 41),
    i32(0),
    i32(0),
    i32(0),
    Buffer.from([1]),
  ])
  return align(payload, 4)
}

function live2dSchemas() {
  const schema = (className, pointerType, pointerName) => ({
    assemblyName: 'Live2D.Cubism.dll',
    namespace: 'Live2D.Cubism.Core',
    className,
    nodes: live2dReferenceNodes(pointerType, pointerName).map((node) => ({
      typeName: node.type,
      fieldName: node.name,
      level: node.level,
      align: node.align ?? false,
    })),
  })
  return [
    schema('CubismModel', 'CubismMoc', '_moc'),
    schema('CubismRenderer', 'Texture2D', '_mainTexture'),
  ]
}

function syntheticStrippedAclLive2dPackage() {
  const modelTypes = [
    { classId: 1 },
    { classId: 4 },
    { classId: 114, scriptHash: 0x20 },
    { classId: 114, scriptHash: 0x21 },
    { classId: 114, scriptHash: 0x30 },
    { classId: 115 },
    { classId: 95 },
    { classId: 91 },
    { classId: 74 },
  ]
  const modelObjects = [
    {
      typeIndex: 0,
      pathId: 1,
      payload: live2dGameObject('Hero', [[0, 10], [0, 20], [0, 50]]),
    },
    {
      typeIndex: 1,
      pathId: 10,
      payload: live2dTransform([0, 1], [[0, 11]], [0, 0]),
    },
    {
      typeIndex: 2,
      pathId: 20,
      payload: live2dMonoBehaviour([0, 1], [0, 100], '', [0, 30]),
    },
    {
      typeIndex: 0,
      pathId: 2,
      payload: live2dGameObject('Drawables', [[0, 11], [0, 21]]),
    },
    {
      typeIndex: 1,
      pathId: 11,
      payload: live2dTransform([0, 2], [], [0, 10]),
    },
    {
      typeIndex: 3,
      pathId: 21,
      payload: live2dMonoBehaviour([0, 2], [0, 101], '', [1, 40]),
    },
    {
      typeIndex: 4,
      pathId: 30,
      payload: live2dMocBehaviour([0, 1], [0, 102]),
    },
    { typeIndex: 5, pathId: 100, payload: live2dMonoScript('CubismModel') },
    { typeIndex: 5, pathId: 101, payload: live2dMonoScript('CubismRenderer') },
    { typeIndex: 5, pathId: 102, payload: live2dMonoScript('CubismMoc') },
    { typeIndex: 5, pathId: 103, payload: live2dMonoScript('CubismRenderController') },
    { typeIndex: 6, pathId: 50, payload: live2dAnimator() },
    { typeIndex: 7, pathId: 31, payload: live2dAnimatorController() },
    {
      typeIndex: 8,
      pathId: 41,
      payload: cubismAclAnimationClipPayload({
        frameCount: 2,
        curveCount: 1,
        bindingScript: 103,
      }),
    },
  ]
  const model = finishV22TypedObjects(
    modelTypes,
    modelObjects,
    ['archive:/textures.assets'],
    '2022.3.55t4',
  )
  const texture = finishV22TypedObjects(
    [{ classId: 28 }],
    [{
      typeIndex: 0,
      pathId: 40,
      payload: texture2dPayload(
        'face',
        1,
        1,
        Buffer.from([9, 8, 7, 255]),
      ),
    }],
  )
  return [
    { name: 'model.assets', data: model },
    { name: 'textures.assets', data: texture },
  ]
}

function syntheticTexturedModel() {
  return finishV22Objects([
    { classId: 1, pathId: 1, payload: modelGameObject() },
    { classId: 4, pathId: 11, payload: modelTransform() },
    { classId: 33, pathId: 21, payload: Buffer.concat([pptr(1), pptr(51)]) },
    { classId: 23, pathId: 31, payload: modelRenderer() },
    { classId: 21, pathId: 41, payload: materialPayload(61) },
    { classId: 43, pathId: 51, payload: modelMesh() },
    {
      classId: 28,
      pathId: 61,
      payload: texture2dPayload(
        'node model texture',
        1,
        1,
        Buffer.from([9, 8, 7, 255]),
      ),
    },
  ])
}

function syntheticMonoScript() {
  // Name, execution order, the 16-byte properties hash, then the identity
  // triple a MonoBehaviour is resolved through.
  const payload = Buffer.concat([
    alignedString('NodeScript'),
    i32(-42),
    Buffer.alloc(16),
    alignedString('CubismMoc'),
    alignedString('Live2D.Cubism.Core'),
    alignedString('Live2D.Cubism.Core.dll'),
  ])
  return finishV22Asset(115, payload)
}

function syntheticPlayerSettings() {
  // The platform prefix a 2022.3 PlayerSettings carries before its names: the
  // product GUID, the Android profiler flag, the screen orientation and target
  // device, the on-demand-resources flag, and the accelerometer frequency.
  // Each flag is a single byte followed by padding to the next four.
  const payload = Buffer.concat([
    Buffer.alloc(16),
    Buffer.from([0, 0, 0, 0]),
    i32(0),
    i32(0),
    Buffer.from([0, 0, 0, 0]),
    i32(0),
    alignedString('Team Haruki'),
    alignedString('unity-rs fixture'),
  ])
  return finishV22Asset(129, payload)
}

// The same object with its Unity version stripped, as a shipped build can be.
function syntheticStrippedPlayerSettings() {
  const built = syntheticPlayerSettings()
  return finishV22Asset(129, built.subarray(Number(built.readBigInt64BE(32))), '0.0.0')
}

function u32be(value) {
  const buffer = Buffer.alloc(4)
  buffer.writeUInt32BE(value)
  return buffer
}

function i64be(value) {
  const buffer = Buffer.alloc(8)
  buffer.writeBigInt64BE(BigInt(value))
  return buffer
}

function cstring(value) {
  return Buffer.concat([Buffer.from(value, 'ascii'), Buffer.from([0])])
}

// A UnityFS v6 bundle whose single data block is marked Oodle-compressed.
//
// The stored bytes are the payload verbatim: Core cannot decompress Oodle
// itself, so what the block actually contains is whatever the injected decoder
// says it does. Marking it compression 6 is what forces that decoder to be
// called, which is the point of the fixture.
function syntheticOodleBundle(entryName, payload) {
  const COMBINED = 0x40
  const SERIALIZED_FILE_ENTRY = 4
  const OODLE = 6

  const entryTable = Buffer.concat([
    i64be(0),
    i64be(payload.length),
    u32be(SERIALIZED_FILE_ENTRY),
    cstring(entryName),
  ])
  const blocksInfo = Buffer.concat([
    Buffer.alloc(16), // hash
    u32be(1), // one block
    u32be(payload.length), // uncompressed size
    u32be(payload.length), // stored size
    Buffer.from([0, OODLE]), // block flags, big-endian u16
    u32be(1), // one directory entry
    entryTable,
  ])

  const header = Buffer.concat([
    cstring('UnityFS'),
    u32be(6),
    cstring('5.x.x'),
    cstring('2019.4.40f1'),
  ])
  const sizeOffset = header.length
  const rest = Buffer.concat([
    i64be(0), // total size, patched below
    u32be(blocksInfo.length), // stored blocks-info size
    u32be(blocksInfo.length), // uncompressed blocks-info size
    u32be(COMBINED), // uncompressed blocks-info, combined directory
    blocksInfo,
    payload,
  ])
  const bundle = Buffer.concat([header, rest])
  bundle.writeBigInt64BE(BigInt(bundle.length), sizeOffset)
  return bundle
}

// A MonoBehaviour carrying its own TypeTree, which is the only way a reader can
// know a Live2D SDK type's layout. Nodes are the format 19+ blob encoding: 32
// bytes each, then one shared string buffer.
function typeTreeAsset(classId, nodes, payload) {
  const strings = []
  let stringBytes = 0
  const intern = (value) => {
    const at = stringBytes
    strings.push(Buffer.from(`${value}\0`, 'ascii'))
    stringBytes += value.length + 1
    return at
  }
  const encoded = nodes.map((node, index) => {
    const typeAt = intern(node.type)
    const nameAt = intern(node.name)
    const head = Buffer.alloc(4)
    head.writeUInt16LE(1, 0) // node version
    head.writeUInt8(node.level, 2)
    head.writeUInt8(node.array ? 1 : 0, 3)
    return Buffer.concat([
      head,
      u32(typeAt),
      u32(nameAt),
      i32(node.size ?? -1),
      i32(index),
      i32(node.align ? 0x4000 : 0),
      Buffer.alloc(8), // reference type hash
    ])
  })
  const stringBuffer = Buffer.concat(strings)

  let metadata = Buffer.concat([
    Buffer.from('2022.3.62f1\0', 'ascii'),
    i32(13),
    Buffer.from([1]), // the tree is enabled
    i32(1), // one type
    i32(classId),
    Buffer.from([0]), // not stripped
    Buffer.from([0, 0]), // script type index
    // A MonoBehaviour record carries the script hash before the type hash.
    ...(classId === 114 ? [Buffer.alloc(16)] : []),
    Buffer.alloc(16),
    i32(nodes.length),
    i32(stringBuffer.length),
    ...encoded,
    stringBuffer,
    i32(0), // no type dependencies
    i32(1), // one object
  ])
  metadata = align(Buffer.concat([Buffer.alloc(48), metadata]), 4).subarray(48)
  metadata = Buffer.concat([
    metadata,
    i64(11),
    i64(0),
    u32(payload.length),
    i32(0), // type index
    i32(0),
    i32(0),
    i32(0),
    Buffer.from([0]),
  ])
  const dataOffset = Math.ceil((48 + metadata.length) / 16) * 16
  const header = Buffer.alloc(48)
  header.writeUInt32BE(22, 8)
  header.writeUInt32BE(metadata.length, 20)
  header.writeBigInt64BE(BigInt(dataOffset + payload.length), 24)
  header.writeBigInt64BE(BigInt(dataOffset), 32)
  return Buffer.concat([
    header,
    metadata,
    Buffer.alloc(dataOffset - 48 - metadata.length),
    payload,
  ])
}

// The tree and bytes of a CubismExpressionData, whose field names come from the
// managed CubismUnityClasses/CubismExpressionData.cs.
function syntheticCubismExpression() {
  const string = (name, level) => [
    { type: 'string', name, level, align: true },
    { type: 'Array', name: 'Array', level: level + 1, array: true, align: true },
    { type: 'int', name: 'size', size: 4, level: level + 2 },
    { type: 'char', name: 'data', size: 1, level: level + 2 },
  ]
  const pptr = (name, target, level) => [
    { type: `PPtr<${target}>`, name, size: 12, level },
    { type: 'int', name: 'm_FileID', size: 4, level: level + 1 },
    { type: 'SInt64', name: 'm_PathID', size: 8, level: level + 1 },
  ]
  const nodes = [
    { type: 'MonoBehaviour', name: 'Base', level: 0 },
    ...pptr('m_GameObject', 'GameObject', 1),
    { type: 'UInt8', name: 'm_Enabled', size: 1, level: 1, align: true },
    ...pptr('m_Script', 'MonoScript', 1),
    ...string('m_Name', 1),
    ...string('Type', 1),
    { type: 'float', name: 'FadeInTime', size: 4, level: 1 },
    { type: 'float', name: 'FadeOutTime', size: 4, level: 1 },
    { type: 'vector', name: 'Parameters', level: 1 },
    { type: 'Array', name: 'Array', level: 2, array: true, align: true },
    { type: 'int', name: 'size', size: 4, level: 3 },
    { type: 'SerializableExpressionParameter', name: 'data', level: 3 },
    ...string('Id', 4),
    { type: 'float', name: 'Value', size: 4, level: 4 },
    { type: 'int', name: 'Blend', size: 4, level: 4 },
  ]

  const f32 = (value) => {
    const buffer = Buffer.alloc(4)
    buffer.writeFloatLE(value)
    return buffer
  }
  const payload = Buffer.concat([
    Buffer.alloc(12), // m_GameObject
    Buffer.from([1, 0, 0, 0]), // m_Enabled, aligned
    Buffer.alloc(12), // m_Script
    alignedString('node-expression'),
    alignedString('Live2D Expression'),
    f32(0.5),
    f32(1.25),
    i32(2),
    alignedString('ParamAngleX'),
    f32(0.8),
    i32(0),
    alignedString('ParamAngleY'),
    f32(-0.25),
    i32(1),
  ])
  return typeTreeAsset(114, nodes, payload)
}

function cubismStringNodes(name, level) {
  return [
    { type: 'string', name, level, align: true },
    { type: 'Array', name: 'Array', level: level + 1, array: true, align: true },
    { type: 'int', name: 'size', size: 4, level: level + 2 },
    { type: 'char', name: 'data', size: 1, level: level + 2 },
  ]
}

function cubismPptrNodes(name, target, level) {
  return [
    { type: `PPtr<${target}>`, name, size: 12, level },
    { type: 'int', name: 'm_FileID', size: 4, level: level + 1 },
    { type: 'SInt64', name: 'm_PathID', size: 8, level: level + 1 },
  ]
}

function cubismMonoBehaviourNodes() {
  return [
    { type: 'MonoBehaviour', name: 'Base', level: 0 },
    ...cubismPptrNodes('m_GameObject', 'GameObject', 1),
    { type: 'UInt8', name: 'm_Enabled', size: 1, level: 1, align: true },
    ...cubismPptrNodes('m_Script', 'MonoScript', 1),
    ...cubismStringNodes('m_Name', 1),
  ]
}

function cubismMonoBehaviourPayload(name) {
  return Buffer.concat([
    Buffer.alloc(12),
    Buffer.from([1, 0, 0, 0]),
    Buffer.alloc(12),
    alignedString(name),
  ])
}

function syntheticCubismPosePart() {
  const nodes = [
    ...cubismMonoBehaviourNodes(),
    { type: 'int', name: 'GroupIndex', size: 4, level: 1 },
    { type: 'vector', name: 'Link', level: 1 },
    { type: 'Array', name: 'Array', level: 2, array: true, align: true },
    { type: 'int', name: 'size', size: 4, level: 3 },
    ...cubismStringNodes('data', 3),
  ]
  const payload = Buffer.concat([
    cubismMonoBehaviourPayload('pose-part'),
    i32(3),
    i32(2),
    alignedString('PartArmL'),
    alignedString('PartArmR'),
  ])
  return typeTreeAsset(114, nodes, payload)
}

function syntheticCubismDisplayInfo() {
  const nodes = [
    ...cubismMonoBehaviourNodes(),
    ...cubismStringNodes('Name', 1),
    ...cubismStringNodes('DisplayName', 1),
  ]
  const payload = Buffer.concat([
    cubismMonoBehaviourPayload('display-info'),
    alignedString('ParamAngleX'),
    alignedString('Head angle'),
  ])
  return typeTreeAsset(114, nodes, payload)
}

function emptyCubismHumanPose() {
  const zeroXform = () => Buffer.alloc(10 * 4)
  const emptyF32Array = () => i32(0)
  const handPose = () => Buffer.concat([
    zeroXform(),
    emptyF32Array(),
    Buffer.alloc(4 * 4),
  ])
  return Buffer.concat([
    zeroXform(),
    Buffer.alloc(7 * 4),
    i32(0),
    handPose(),
    handPose(),
    emptyF32Array(),
    i32(0),
  ])
}

function emptyCubismMuscle(acl = Buffer.alloc(0)) {
  const zeroXform = () => Buffer.alloc(10 * 4)
  const emptyF32Array = () => i32(0)
  return align(Buffer.concat([
    emptyCubismHumanPose(),
    zeroXform(),
    zeroXform(),
    zeroXform(),
    zeroXform(),
    Buffer.alloc(3 * 4),
    i32(0), // streamed words
    u32(0), // streamed curve count
    i32(0), // dense frame count
    u32(0), // dense curve count
    f32(30),
    f32(0),
    emptyF32Array(), // dense samples
    emptyF32Array(), // constant samples
    acl,
    f32(0), // start time
    f32(1), // stop time
    Buffer.alloc(4 * 4), // orientation, level, cycle, angular speed
    i32(0), // indexes
    i32(0), // value deltas
    emptyF32Array(), // reference pose
    Buffer.alloc(11), // muscle flags
  ]), 4)
}

// A standard Unity 2022.2 AnimationClip with a muscle clip but no curves.
// It is intentionally empty at the binding layer: the Node call still has to
// parse the complete object and produce a valid motion3 document.
function syntheticCubismAnimationClip() {
  const payload = align(Buffer.concat([
    alignedString('node-motion'),
    Buffer.from([0, 1, 1]),
    Buffer.alloc(1),
    Buffer.alloc(7 * 4), // all seven ordinary curve lists are empty
    f32(60),
    i32(2),
    Buffer.alloc(6 * 4), // AABB
    u32(0), // muscle clip size is advisory on the standard Unity path
    emptyCubismMuscle(),
    i32(0), // generic bindings
    i32(0), // PPtr curve mapping
    Buffer.from([1, 0, 0, 0]), // 2018.3+ root/motion flags, aligned
    i32(0), // events
  ]), 4)
  return finishV22Asset(74, payload, '2022.2.0f1')
}

function fnv1a32(bytes) {
  let hash = 2_166_136_261
  for (const byte of bytes) {
    hash = Math.imul((hash ^ byte) >>> 0, 16_777_619) >>> 0
  }
  return hash
}

// A structurally valid, empty ACL 2.x `compressed_tracks` container. The
// decoder payload is irrelevant to this bridge test; Core validates the size,
// tag, version, algorithm, track shape and hash before JavaScript sees it.
function syntheticAclTracks(numTracks = 0, numSamplesPerTrack = 0) {
  const tracks = Buffer.alloc(32)
  tracks.writeUInt32LE(tracks.length, 0)
  tracks.writeUInt32LE(0xac11ac11, 8)
  tracks.writeUInt16LE(10, 12)
  tracks.writeUInt8(0, 14) // uniformly sampled algorithm
  tracks.writeUInt8(0, 15) // float1f tracks
  tracks.writeUInt32LE(numTracks, 16)
  tracks.writeUInt32LE(numSamplesPerTrack, 20)
  tracks.writeFloatLE(30, 24)
  tracks.writeUInt32LE(0, 28)
  tracks.writeUInt32LE(fnv1a32(tracks.subarray(8)), 4)
  return tracks
}

// Tuanjie 2022.3.55t4 stores the muscle block in little-endian m_AnimData and
// adds ACL tracks, a declared curve count, and the fast-sample flag.
function syntheticCubismAclAnimationClip() {
  return finishV22Asset(74, cubismAclAnimationClipPayload(), '2022.3.55t4')
}

function cubismAclAnimationClipPayload(options = {}) {
  const frameCount = options.frameCount ?? 0
  const curveCount = options.curveCount ?? 0
  const bindingScript = options.bindingScript ?? 0
  const tracks = syntheticAclTracks(curveCount, frameCount)
  const acl = Buffer.concat([
    u32(frameCount),
    u32(0), // bone count
    f32(30),
    u32(curveCount),
    i32(tracks.length),
    tracks,
    i32(0), // decoder map
    Buffer.from([1]), // fast sample mode, not aligned before 2022.3.61
  ])
  const embedded = emptyCubismMuscle(acl)
  const genericBindings = bindingScript === 0
    ? i32(0)
    : Buffer.concat([
      i32(1),
      u32(0), // no transform-path hash; resolve through the script
      u32(0),
      live2dPptr(0, bindingScript),
      i32(114),
      Buffer.from([0, 0, 0, 0]),
    ])
  return align(Buffer.concat([
    alignedString('node-acl-motion'),
    Buffer.from([0, 1, 1]),
    Buffer.alloc(1),
    Buffer.alloc(4 * 4), // rotation, compressed, float and PPtr curve lists
    f32(60),
    i32(2),
    Buffer.alloc(6 * 4),
    u32(embedded.length),
    i32(embedded.length),
    embedded,
    i64(0),
    u32(0),
    alignedString(''), // StreamingInfo
    genericBindings,
    i32(0), // PPtr curve mapping
    Buffer.from([1, 0, 0, 0]),
    i32(0), // events
  ]), 4)
}

// One transform-only model whose Animator selects the ACL clip above. It is
// deliberately free of renderer-specific Tuanjie fields: FBX must still carry
// the hierarchy and take, and the callback invocation proves the clip reached
// the ACL projection path rather than merely exercising the method shell.
function syntheticAclFbxModel() {
  const gameObject = Buffer.concat([
    i32(2),
    pptr(11),
    pptr(21),
    i32(0),
    Buffer.alloc(4), // Tuanjie editor-info flag plus absolute alignment
    alignedString('node acl model'),
  ])
  let animator = Buffer.concat([pptr(1), Buffer.from([1])])
  animator = align(animator, 4)
  animator = Buffer.concat([animator, pptr(0), pptr(31)])

  let controller = Buffer.concat([
    alignedString('node acl controller'),
    u32(0),
    ...Array.from({ length: 9 }, () => i32(0)),
    i32(1),
    u32(0xdeadbeef),
    alignedString('node acl model'),
    i32(1),
    pptr(41),
    i32(0),
    i32(0),
    i32(0),
    Buffer.from([1]),
  ])
  controller = align(controller, 4)

  return finishV22Objects([
    { classId: 1, pathId: 1, payload: gameObject },
    { classId: 4, pathId: 11, payload: modelTransform() },
    { classId: 95, pathId: 21, payload: animator },
    { classId: 91, pathId: 31, payload: controller },
    { classId: 74, pathId: 41, payload: cubismAclAnimationClipPayload() },
  ], '2022.3.55t4')
}

function syntheticTypeTreeIntAsset() {
  const payload = i32(42)
  const strings = Buffer.from('int\0value\0', 'ascii')
  const node = Buffer.concat([
    Buffer.from([2, 0, 0, 0]),
    u32(0),
    u32(4),
    i32(4),
    i32(0),
    i32(0),
    Buffer.alloc(8),
  ])
  let metadata = Buffer.concat([
    Buffer.from('2022.3.62f1\0', 'ascii'),
    i32(13),
    Buffer.from([1]),
    i32(1),
    i32(123456),
    Buffer.from([0]),
    Buffer.from([0xff, 0xff]),
    Buffer.alloc(16),
    i32(1),
    i32(strings.length),
    node,
    strings,
    i32(0),
    i32(1),
  ])
  metadata = align(Buffer.concat([Buffer.alloc(48), metadata]), 4).subarray(48)
  metadata = Buffer.concat([
    metadata,
    i64(11),
    i64(0),
    u32(payload.length),
    i32(0),
    i32(0),
    i32(0),
    i32(0),
    Buffer.from([0]),
  ])
  const dataOffset = Math.ceil((48 + metadata.length) / 16) * 16
  const header = Buffer.alloc(48)
  header.writeUInt32BE(22, 8)
  header.writeUInt32BE(metadata.length, 20)
  header.writeBigInt64BE(BigInt(dataOffset + payload.length), 24)
  header.writeBigInt64BE(BigInt(dataOffset), 32)
  return Buffer.concat([
    header,
    metadata,
    Buffer.alloc(dataOffset - 48 - metadata.length),
    payload,
  ])
}

const input = syntheticTextAsset()
const studio = addon.UnityRs.fromBuffer(input, 'node.assets', input.length)

assert.equal(studio.fileCount, 1)
assert.equal(studio.objectCount, 1)
assert.equal(studio.resourceCount, 0)
assert.deepEqual(studio.filePage(), [
  {
    index: 0,
    path: 'node.assets',
    unityVersion: '2022.3.62f1',
    objectCount: 1,
  },
])
const objects = studio.objectPage(0)
assert.equal(objects.length, 1)
assert.equal(objects[0].pathId, 7n)
assert.equal(objects[0].classId, 49)
assert.equal(objects[0].name, 'node fixture')
assert.equal(objects[0].byteSize, BigInt(input.length - Number(input.readBigInt64BE(32))))
assert.deepEqual(studio.readText(0, 7n), Buffer.from('hello node'))
assert.equal(studio.readRaw(0, 7n).length, Number(objects[0].byteSize))
assert.throws(() => studio.readText(0, 7n, 9), /limit|exceed/i)
assert.throws(() => studio.readTypeTreeJson(0, 7n), /TypeTree|type tree/i)
assert.throws(() => studio.readTypeTreeDump(0, 7n), /TypeTree|type tree/i)
assert.throws(() => studio.readTextureArray(0, 7n), /Texture2DArray|class ID/i)
assert.throws(() => studio.readSprite(0, 7n), /Sprite|class ID/i)
assert.throws(() => studio.objectPage(0, 0, 1_000_001), /page limit/i)
assert.throws(() => addon.UnityRs.fromBuffer(input, 'node.assets', 1), /exceed/i)

// Texture2D rows leave the decoder bottom-up. Every consumer -- the Python
// binding, the CLI's encoded images and the managed exporter -- hands callers
// top-down rows, so this binding has to flip them too.
const DISPLAY_ORDER_PIXELS = Buffer.concat([
  TEXTURE_PIXELS.subarray(8),
  TEXTURE_PIXELS.subarray(0, 8),
])
const textureInput = syntheticTexture2d()
const textureStudio = addon.UnityRs.fromBuffer(
  textureInput,
  'texture.assets',
  textureInput.length,
)
const decodedTexture = textureStudio.readTexture(0, 7n)
assert.equal(decodedTexture.width, 2)
assert.equal(decodedTexture.height, 2)
assert.deepEqual(Buffer.from(decodedTexture.pixels), DISPLAY_ORDER_PIXELS)

// encodeImage hands single decoded images to Core's bounded encoders, so a
// caller no longer needs the on-disk export layout (or a JavaScript encoder)
// just to save one texture or sprite.
{
  const encodedPng = addon.UnityRs.encodeImage(decodedTexture)
  assert.deepEqual(
    encodedPng.subarray(0, 8),
    Buffer.from('\x89PNG\r\n\x1a\n', 'binary'),
  )
  // The IHDR dimensions prove the pixels went in as a 2x2 image rather than
  // the call merely succeeding on some buffer.
  assert.equal(encodedPng.readUInt32BE(16), 2)
  assert.equal(encodedPng.readUInt32BE(20), 2)
  const encodedRaw = addon.UnityRs.encodeImage(decodedTexture, {
    imageFormat: 'raw-rgba',
  })
  assert.equal(encodedRaw.subarray(0, 16).toString('ascii'), 'HARUKI_RGBAIR_V1')
  const encodedQoi = addon.UnityRs.encodeImage(decodedTexture, {
    imageFormat: 'qoi',
  })
  assert.equal(encodedQoi.subarray(0, 4).toString('ascii'), 'qoif')
  assert.equal(encodedQoi.readUInt32BE(4), 2)
  assert.equal(encodedQoi.readUInt32BE(8), 2)
  // The zlib effort and scanline filter change the compressed stream, never
  // the pixels: every choice stays a valid PNG with the same IHDR geometry,
  // and the compression option accepts an explicit numeric level.
  for (const options of [
    { compression: 'fast' },
    { compression: 0 },
    { compression: 9 },
    { pngFilter: 'auto' },
    { pngFilter: 'adaptive' },
  ]) {
    const variant = addon.UnityRs.encodeImage(decodedTexture, options)
    assert.deepEqual(variant.subarray(0, 8), encodedPng.subarray(0, 8))
    assert.deepEqual(variant.subarray(16, 24), encodedPng.subarray(16, 24))
  }
  // Each JPEG knob produces a stream that differs from the baseline while
  // remaining a JPEG, and the background composite takes an RGB triple.
  const encodedJpeg = addon.UnityRs.encodeImage(decodedTexture, {
    imageFormat: 'jpeg',
  })
  assert.deepEqual(encodedJpeg.subarray(0, 2), Buffer.from([0xff, 0xd8]))
  for (const options of [
    { jpegSampling: '4:4:4' },
    { jpegProgressive: true },
    { jpegOptimizedHuffman: true },
    { jpegBackground: [255, 255, 255] },
  ]) {
    const variant = addon.UnityRs.encodeImage(decodedTexture, {
      imageFormat: 'jpeg',
      ...options,
    })
    assert.deepEqual(variant.subarray(0, 2), Buffer.from([0xff, 0xd8]))
    assert.notDeepEqual(variant, encodedJpeg)
  }
  assert.throws(
    () => addon.UnityRs.encodeImage(decodedTexture, { compression: 'turbo' }),
    /unsupported PNG compression/i,
  )
  assert.throws(
    () => addon.UnityRs.encodeImage(decodedTexture, { compression: 10 }),
    /range 0 through 9/i,
  )
  assert.throws(
    () => addon.UnityRs.encodeImage(decodedTexture, { pngFilter: 'paeth' }),
    /unsupported PNG filter/i,
  )
  assert.throws(
    () =>
      addon.UnityRs.encodeImage(decodedTexture, {
        imageFormat: 'jpeg',
        jpegSampling: '4:1:1',
      }),
    /unsupported JPEG sampling/i,
  )
  assert.throws(
    () =>
      addon.UnityRs.encodeImage(decodedTexture, {
        imageFormat: 'jpeg',
        jpegBackground: [255, 255],
      }),
    /three RGB channel values/i,
  )
  assert.throws(
    () => addon.UnityRs.encodeImage(decodedTexture, { imageFormat: 'gif' }),
    /unsupported image format/i,
  )
  assert.throws(
    () => addon.UnityRs.encodeImage(decodedTexture, { maximumBytes: 8 }),
    /exceed/i,
  )
  assert.throws(
    () =>
      addon.UnityRs.encodeImage(decodedTexture, {
        imageFormat: 'jpeg',
        jpegQuality: 0,
      }),
    /range 1 through 100/i,
  )
  // A pixel buffer that disagrees with the declared dimensions is rejected
  // before any encoder sees it.
  assert.throws(
    () =>
      addon.UnityRs.encodeImage({ width: 2, height: 2, pixels: Buffer.alloc(3) }),
    /requires 16/i,
  )
}

// Model texture encoding is a public Node option, not a PNG-only wrapper over
// Core. This fixture has a real Renderer -> Material -> Texture2D chain so the
// assertion cannot pass merely because the method accepted another argument.
{
  const modelInput = syntheticTexturedModel()
  const modelStudio = addon.UnityRs.fromBuffer(
    modelInput,
    'textured-model.assets',
    modelInput.length,
  )
  const modelScene = modelStudio.scene()
  assert.equal(modelScene.length, 1)
  assert.equal(modelScene[0].hasMeshRenderer, true)
  assert.match(modelStudio.readMeshObj(0, 51n).toString('utf8'), /^g node triangle/m)
  const rawModel = modelStudio.readModelObj(undefined, 128 * 1024, 'raw-rgba')
  assert.equal(rawModel.textures.length, 1)
  assert.match(rawModel.textures[0].fileName, /\.rgba$/)
  assert.equal(
    Buffer.from(rawModel.textures[0].data).subarray(0, 16).toString('ascii'),
    'HARUKI_RGBAIR_V1',
  )
  assert.throws(
    () => modelStudio.readModelObj(
      undefined,
      128 * 1024,
      'raw-rgba',
      { maximumTextures: 0 },
    ),
    /more than 0 textures/i,
  )
  assert.throws(
    () => modelStudio.readFbxWithTextures(
      128 * 1024,
      'raw-rgba',
      { maximumTotalEncodedBytes: rawModel.textures[0].data.length - 1 },
    ),
    /byte budget/i,
  )
  assert.throws(
    () => modelStudio.readModelObj(
      undefined,
      128 * 1024,
      'raw-rgba',
      { maximumMetadataBytes: 7 },
    ),
    /metadata requires 8 UTF-8 bytes/i,
  )
  const textureNameIndexBytes = Buffer.byteLength(rawModel.textures[0].fileName) * 2
  assert.throws(
    () => modelStudio.readModelObj(
      undefined,
      128 * 1024,
      'raw-rgba',
      { maximumNameIndexBytes: textureNameIndexBytes - 1 },
    ),
    new RegExp(`name indexes require ${textureNameIndexBytes} UTF-8 bytes`, 'i'),
  )
  const limitedModel = modelStudio.readModelObj(
    undefined,
    128 * 1024,
    'raw-rgba',
    { maximumSingleTextureBytes: rawModel.textures[0].data.length - 1 },
  )
  assert.equal(limitedModel.textures.length, 0)
  assert.equal(limitedModel.skipped.length, 1)
  assert.match(limitedModel.skipped[0], /limit/i)
  assert.throws(
    () => modelStudio.readModelObj(
      undefined,
      128 * 1024,
      'png',
      { maximumTotalEncodedBytes: -1 },
    ),
    /maximumTotalEncodedBytes must be non-negative/i,
  )
  assert.throws(
    () => modelStudio.readModelObj(
      undefined,
      128 * 1024,
      'png',
      { maximumNameIndexBytes: -1 },
    ),
    /maximumNameIndexBytes must be non-negative/i,
  )
  assert.throws(
    () => modelStudio.readModelObj(
      undefined,
      128 * 1024,
      'png',
      { maximumMetadataBytes: -1 },
    ),
    /maximumMetadataBytes must be non-negative/i,
  )

  const texturedFbx = modelStudio.readFbxWithTextures(128 * 1024, 'TGA')
  assert.equal(texturedFbx.textures.length, 1)
  assert.match(texturedFbx.textures[0].fileName, /\.tga$/)
  const tga = Buffer.from(texturedFbx.textures[0].data)
  assert.equal(tga[2], 2)
  assert.equal(tga.readUInt16LE(12), 1)
  assert.equal(tga.readUInt16LE(14), 1)
  assert.equal(tga[16], 32)
  assert.ok(
    Buffer.from(texturedFbx.fbx).includes(
      Buffer.from(texturedFbx.textures[0].fileName),
    ),
  )

  const defaultModel = modelStudio.readModelObj(undefined, 128 * 1024)
  assert.match(defaultModel.textures[0].fileName, /\.png$/)
  assert.deepEqual(
    Buffer.from(defaultModel.textures[0].data).subarray(0, 8),
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
  )
}

async function testAsyncWorkers() {
  const asyncStudio = await addon.UnityRs.fromBufferAsync(
    input,
    'async-node.assets',
    input.length,
  )
  assert.equal(asyncStudio.fileCount, 1)
  assert.deepEqual(await asyncStudio.readTextAsync(0, 7n), Buffer.from('hello node'))
  assert.deepEqual(await asyncStudio.readRawAsync(0, 7n), studio.readRaw(0, 7n))
  const configuredAsyncStudio = await addon.UnityRs.fromBufferAsync(
    input,
    'async-options.assets',
    input.length,
    { unityVersion: '2022.3.62f1', maximumPathBytes: 1024 * 1024 },
  )
  assert.equal(configuredAsyncStudio.fileCount, 1)
  await assert.rejects(
    addon.UnityRs.fromBufferAsync(
      input,
      'async-options.assets',
      input.length,
      { maximumPathBytes: 1 },
    ),
    /asset path/i,
  )
  await assert.rejects(asyncStudio.readTextAsync(0, 7n, 9), /limit|exceed/i)
  await assert.rejects(asyncStudio.readShaderAsync(0, 7n), /Shader|class ID/i)
  await assert.rejects(asyncStudio.readMeshObjAsync(0, 7n), /Mesh|class ID/i)
  await assert.rejects(asyncStudio.readTextureAsync(0, 7n), /Texture2D|class ID/i)
  await assert.rejects(asyncStudio.readTypeTreeJsonAsync(0, 7n), /TypeTree|type tree/i)
  await assert.rejects(asyncStudio.readTypeTreeDumpAsync(0, 7n), /TypeTree|type tree/i)
  await assert.rejects(asyncStudio.readTextureArrayAsync(0, 7n), /Texture2DArray|class ID/i)
  await assert.rejects(asyncStudio.readSpriteAsync(0, 7n), /Sprite|class ID/i)

  // The worker path has to agree with the synchronous one on row order.
  const asyncTextureStudio = await addon.UnityRs.fromBufferAsync(
    textureInput,
    'async-texture.assets',
    textureInput.length,
  )
  const asyncTexture = await asyncTextureStudio.readTextureAsync(0, 7n)
  assert.deepEqual(Buffer.from(asyncTexture.pixels), DISPLAY_ORDER_PIXELS)

  // The worker-thread encoder must produce exactly the synchronous bytes and
  // keep its input validation.
  assert.deepEqual(
    await addon.UnityRs.encodeImageAsync(asyncTexture, { imageFormat: 'png' }),
    addon.UnityRs.encodeImage(asyncTexture),
  )
  await assert.rejects(
    addon.UnityRs.encodeImageAsync(asyncTexture, { maximumBytes: 8 }),
    /exceed/i,
  )

  const textureArrayInput = syntheticTexture2dArray()
  const asyncTextureArrayStudio = await addon.UnityRs.fromBufferAsync(
    textureArrayInput,
    'async-texture-array.assets',
    textureArrayInput.length,
  )
  const syncTextureArray = asyncTextureArrayStudio.readTextureArray(0, 7n)
  const asyncTextureArray = await asyncTextureArrayStudio.readTextureArrayAsync(0, 7n)
  assert.equal(asyncTextureArray.length, 2)
  assert.deepEqual(asyncTextureArray, syncTextureArray)
  assert.deepEqual(
    Buffer.from(asyncTextureArray[0].pixels),
    Buffer.from([5, 6, 7, 8, 1, 2, 3, 4]),
  )
  assert.deepEqual(
    Buffer.from(asyncTextureArray[1].pixels),
    Buffer.from([15, 16, 17, 18, 11, 12, 13, 14]),
  )

  const treeInput = syntheticTypeTreeIntAsset()
  const treeStudio = await addon.UnityRs.fromBufferAsync(
    treeInput,
    'tree.assets',
    treeInput.length,
  )
  assert.equal(treeStudio.readTypeTreeJson(0, 11n).toString('utf8'), '42')
  assert.match(treeStudio.readTypeTreeDump(0, 11n).toString('utf8'), /42/)
  assert.equal(
    (await treeStudio.readTypeTreeJsonAsync(0, 11n)).toString('utf8'),
    '42',
  )
  assert.match(
    (await treeStudio.readTypeTreeDumpAsync(0, 11n)).toString('utf8'),
    /42/,
  )

  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'unity-rs-node-'))
  const fixturePath = path.join(directory, 'path.assets')
  try {
    fs.writeFileSync(fixturePath, input)
    const pathStudio = await addon.UnityRs.openAsync(fixturePath)
    assert.equal(pathStudio.fileCount, 1)
    assert.deepEqual(await pathStudio.readTextAsync(0, 7n), Buffer.from('hello node'))
    const configuredPathStudio = await addon.UnityRs.openAsync(fixturePath, {
      unityVersion: '2022.3.62f1',
      maximumPathBytes: 1024 * 1024,
    })
    assert.equal(configuredPathStudio.fileCount, 1)
    await assert.rejects(
      addon.UnityRs.openAsync(fixturePath, { maximumPathBytes: 1 }),
      /asset path/i,
    )
  } finally {
    fs.rmSync(directory, { recursive: true, force: true })
  }
}

testAsyncWorkers().catch((error) => {
  console.error(error)
  process.exitCode = 1
})

// MonoScript identity, which is how a MonoBehaviour's type is resolved.
{
  const scriptDirectory = fs.mkdtempSync(path.join(os.tmpdir(), 'unity-rs-node-script-'))
  try {
  const scriptPath = path.join(scriptDirectory, 'script.assets')
  fs.writeFileSync(scriptPath, syntheticMonoScript())
  const scriptStudio = new addon.UnityRs(scriptPath)
  const objects = scriptStudio.objectPage(0)
  assert.equal(objects.length, 1)
  assert.equal(objects[0].classId, 115)
  const script = scriptStudio.readMonoScript(0, objects[0].pathId)
  assert.equal(script.name, 'NodeScript')
  assert.equal(script.className, 'CubismMoc')
  assert.equal(script.namespace, 'Live2D.Cubism.Core')
  assert.equal(script.assemblyName, 'Live2D.Cubism.Core.dll')
  assert.equal(script.executionOrder, -42)
  // A limit below the payload has to be refused rather than truncated.
  assert.throws(() => scriptStudio.readMonoScript(0, objects[0].pathId, 1))
  } finally {
    fs.rmSync(scriptDirectory, { recursive: true, force: true })
  }
}

// Font, MovieTexture and VideoClip had no Node binding at all, while AudioClip
// originally exposed only the source container. These checks use what the
// payload declares rather than bytes this project produced.
{
  const fontStudio = addon.UnityRs.fromBuffers([
    { name: 'font.assets', data: syntheticFont() },
  ])
  const font = fontStudio.readFont(0, 7n)
  assert.strictEqual(font.name, 'node-font')
  assert.strictEqual(font.extension, '.otf')
  assert.ok(font.data.subarray(0, 4).equals(Buffer.from('OTTO')))

  const movieStudio = addon.UnityRs.fromBuffers([
    { name: 'movie.assets', data: syntheticMovieTexture() },
  ])
  const movie = movieStudio.readMovieTexture(0, 7n)
  assert.strictEqual(movie.name, 'node-movie')
  assert.ok(movie.data.equals(Buffer.from('OggS')))

  const audioStudio = addon.UnityRs.fromBuffers([
    { name: 'legacy-pcm.assets', data: syntheticLegacyPcm() },
  ])
  const compatibilityRaw = audioStudio.readAudio(0, 7n)
  assert.strictEqual(compatibilityRaw.name, 'node-legacy-pcm')
  assert.strictEqual(compatibilityRaw.extension, '.AudioClip')
  assert.strictEqual(compatibilityRaw.payloadKind, 'audio_raw')
  assert.strictEqual(compatibilityRaw.isDirectWav, true)
  assert.deepStrictEqual(compatibilityRaw.data, Buffer.from([1, 2, 3, 4]))

  const wav = audioStudio.readAudioClip(0, 7n)
  assert.strictEqual(wav.name, 'node-legacy-pcm')
  assert.strictEqual(wav.extension, '.wav')
  assert.strictEqual(wav.payloadKind, 'audio_wav')
  assert.strictEqual(wav.isDirectWav, true)
  assert.strictEqual(wav.data.length, 48)
  assert.deepStrictEqual(wav.data.subarray(0, 12), Buffer.from('RIFF(\0\0\0WAVE', 'binary'))
  assert.deepStrictEqual(wav.data.subarray(20, 24), Buffer.from([1, 0, 1, 0]))
  assert.strictEqual(wav.data.readUInt32LE(24), 22_050)
  assert.deepStrictEqual(wav.data.subarray(36, 44), Buffer.from('data\x04\0\0\0', 'binary'))
  assert.deepStrictEqual(wav.data.subarray(44), Buffer.from([1, 2, 3, 4]))

  const explicitRaw = audioStudio.readAudioClip(0, 7n, 'raw')
  assert.strictEqual(explicitRaw.payloadKind, 'audio_raw')
  assert.strictEqual(explicitRaw.extension, '.AudioClip')
  assert.deepStrictEqual(explicitRaw.data, Buffer.from([1, 2, 3, 4]))
  assert.throws(() => audioStudio.readAudioClip(0, 7n, 'flac'), /unsupported audio format/i)
  assert.throws(() => audioStudio.readAudioClip(0, 7n, 'wav', 47), /exceeding maximumBytes/i)

  // Reading one kind as another must fail rather than return something.
  assert.throws(() => fontStudio.readMovieTexture(0, 7n))
  assert.throws(() => fontStudio.readAudioClip(0, 7n))
}

// The Cubism document readers, which had no Node binding: a caller could
// materialize a whole package but not read one behaviour's document.
{
  const cubismStudio = addon.UnityRs.fromBuffers([
    { name: 'expression.assets', data: syntheticCubismExpression() },
  ])
  const expression = cubismStudio.readCubismExpression(0, 11n)
  assert.strictEqual(expression.name, 'node-expression')
  assert.strictEqual(expression.entryCount, 2)
  const document = JSON.parse(expression.json.toString('utf8'))
  assert.strictEqual(document.Type, 'Live2D Expression')
  // The managed extractor writes exp3.json through Newtonsoft's default float
  // format, so an integral value keeps its decimal point and a fraction keeps
  // its shortest form.
  assert.strictEqual(document.FadeInTime, 0.5)
  assert.strictEqual(document.FadeOutTime, 1.25)
  assert.deepStrictEqual(document.Parameters, [
    { Id: 'ParamAngleX', Value: 0.8, Blend: 0 },
    { Id: 'ParamAngleY', Value: -0.25, Blend: 1 },
  ])
  // A behaviour that is not an expression must fail rather than return an
  // empty document.
  assert.throws(() => cubismStudio.readCubismPhysics(0, 11n))
  assert.throws(() => cubismStudio.readCubismFadeMotion(0, 11n))
}

// The remaining direct Cubism readers. Whole-package materialization already
// carried pose/display documents, but without these methods Node callers could
// not inspect one component, and clip motion had no Node entry point at all.
{
  const cubismStudio = addon.UnityRs.fromBuffers([
    { name: 'pose.assets', data: syntheticCubismPosePart() },
    { name: 'display.assets', data: syntheticCubismDisplayInfo() },
    { name: 'motion.assets', data: syntheticCubismAnimationClip() },
  ])

  const pose = cubismStudio.readCubismPosePart(0, 11n)
  assert.strictEqual(pose.pathId, 11n)
  assert.strictEqual(pose.groupIndex, 3)
  assert.deepStrictEqual(pose.links, ['PartArmL', 'PartArmR'])

  const display = cubismStudio.readCubismDisplayInfo(1, 11n)
  assert.strictEqual(display.pathId, 11n)
  assert.strictEqual(display.name, 'ParamAngleX')
  assert.strictEqual(display.displayName, 'Head angle')
  assert.strictEqual(display.effectiveName, 'Head angle')

  const clip = cubismStudio.readAnimationClipInfo(2, 7n)
  assert.strictEqual(clip.pathId, 7n)
  assert.strictEqual(clip.name, 'node-motion')
  assert.strictEqual(clip.sampleRate, 60)
  assert.strictEqual(clip.wrapMode, 2)
  assert.strictEqual(clip.legacy, false)
  assert.strictEqual(clip.compressed, true)
  assert.strictEqual(clip.useHighQualityCurve, true)
  assert.strictEqual(clip.rotationCurveCount, 0)
  assert.strictEqual(clip.eulerCurveCount, 0)
  assert.strictEqual(clip.positionCurveCount, 0)
  assert.strictEqual(clip.scaleCurveCount, 0)
  assert.strictEqual(clip.floatCurveCount, 0)
  assert.strictEqual(clip.pptrCurveCount, 0)
  assert.strictEqual(clip.muscleClipSize, 0)
  assert.strictEqual(clip.hasMuscleClip, true)
  assert.strictEqual(clip.streamedCurveCount, 0)
  assert.strictEqual(clip.denseCurveCount, 0)
  assert.strictEqual(clip.constantValueCount, 0)
  assert.strictEqual(clip.hasAcl, false)
  assert.strictEqual(clip.aclFrameCount, undefined)
  assert.strictEqual(clip.hasStreamingInfo, false)
  assert.strictEqual(clip.streamingPath, undefined)

  const motion = cubismStudio.readCubismClipMotion(
    2,
    7n,
    { parameters: [], parts: [] },
    false,
  )
  assert.strictEqual(motion.fileIndex, 2)
  assert.strictEqual(motion.pathId, 7n)
  assert.strictEqual(motion.name, 'node-motion')
  assert.strictEqual(motion.duration, 1)
  assert.strictEqual(motion.fps, 60)
  assert.strictEqual(motion.curveCount, 0)
  assert.strictEqual(motion.keyframeCount, 0)
  assert.strictEqual(motion.eventCount, 0)
  const motionDocument = JSON.parse(motion.json.toString('utf8'))
  assert.strictEqual(motionDocument.Version, 3)
  assert.strictEqual(motionDocument.Meta.Duration, 1)
  assert.strictEqual(motionDocument.Meta.Fps, 60)
  assert.deepStrictEqual(motionDocument.Curves, [])

  const tooManyTargets = new Array(1_000_001)
  let targetElementRead = false
  Object.defineProperty(tooManyTargets, 0, {
    get() {
      targetElementRead = true
      throw new Error('Cubism target read before target count check')
    },
  })
  assert.throws(
    () =>
      cubismStudio.readCubismClipMotion(
        2,
        7n,
        { parameters: tooManyTargets, parts: [] },
        false,
      ),
    /motion targets.*exceeding limit/i,
  )
  assert.equal(targetElementRead, false)

  assert.throws(() => cubismStudio.readCubismPosePart(1, 11n))
  assert.throws(() => cubismStudio.readCubismDisplayInfo(0, 11n))
  assert.throws(() => cubismStudio.readCubismClipMotion(0, 11n))
  assert.throws(() => cubismStudio.readCubismClipMotion(2, 7n, undefined, false, 1))
  assert.throws(() => cubismStudio.readAnimationClipInfo(2, 7n, 1))
}

console.log('node api: additional readers ok')

// PlayerSettings identity and the Unity version override.
{
  const settingsDirectory = fs.mkdtempSync(
    path.join(os.tmpdir(), 'unity-rs-node-settings-'),
  )
  try {
    const settingsPath = path.join(settingsDirectory, 'settings.assets')
    fs.writeFileSync(settingsPath, syntheticPlayerSettings())
    const settingsStudio = new addon.UnityRs(settingsPath)
    const objects = settingsStudio.objectPage(0)
    assert.equal(objects[0].classId, 129)
    const settings = settingsStudio.readPlayerSettings(0, objects[0].pathId)
    assert.equal(settings.companyName, 'Team Haruki')
    assert.equal(settings.productName, 'unity-rs fixture')

    // A stripped build carries no version, so its layout cannot be decided
    // without one. That is what the override is for.
    const strippedPath = path.join(settingsDirectory, 'stripped.assets')
    fs.writeFileSync(strippedPath, syntheticStrippedPlayerSettings())
    const stripped = new addon.UnityRs(strippedPath)
    const strippedObjects = stripped.objectPage(0)
    assert.throws(
      () => stripped.readPlayerSettings(0, strippedObjects[0].pathId),
      /version/i,
    )

    const overridden = addon.UnityRs.openWithVersion(strippedPath, '2022.3.62f1')
    const overriddenSettings = overridden.readPlayerSettings(
      0,
      overridden.objectPage(0)[0].pathId,
    )
    assert.equal(overriddenSettings.companyName, 'Team Haruki')
    assert.throws(() => addon.UnityRs.openWithVersion(strippedPath, 'not a version'))
  } finally {
    fs.rmSync(settingsDirectory, { recursive: true, force: true })
  }
}

console.log('node api: settings and version override ok')

// Multi-buffer loading and checked resource ranges.
{
  const inputs = [
    { name: 'text.assets', data: syntheticTextAsset() },
    { name: 'script.assets', data: syntheticMonoScript() },
  ]
  const multi = addon.UnityRs.fromBuffers(inputs)
  assert.equal(multi.fileCount, 2)
  // Each file keeps its own object table.
  assert.equal(multi.objectPage(0).length, 1)
  assert.equal(multi.objectPage(1).length, 1)
  assert.equal(multi.objectPage(1)[0].classId, 115)
  // The total budget is enforced across the inputs, not per input.
  assert.throws(() => addon.UnityRs.fromBuffers(inputs, 16), /exceeding limit/i)
  assert.throws(
    () => addon.UnityRs.fromBuffers(
      inputs,
      undefined,
      { maximumInputFiles: 1 },
    ),
    /files.*exceeding limit/i,
  )

  // Reject the table length before napi-rs can walk or materialize its
  // elements. A sparse array makes the order observable without allocating a
  // million input objects.
  const tooManyInputs = new Array(1_000_001)
  let inputElementRead = false
  Object.defineProperty(tooManyInputs, 0, {
    get() {
      inputElementRead = true
      throw new Error('fromBuffers read an element before checking its count')
    },
  })
  assert.throws(
    () => addon.UnityRs.fromBuffers(tooManyInputs),
    /files.*exceeding limit/i,
  )
  assert.equal(inputElementRead, false)

  // The strict-unity-versions opt-out is accepted and does not disturb loads
  // that stay inside the verified ranges.
  const strict = addon.UnityRs.fromBuffers(inputs, undefined, {
    strictUnityVersions: true,
  })
  assert.equal(strict.fileCount, 2)
}

// Scene assembly across the loaded files.
{
  // The text fixture has no GameObject, so the hierarchy is legitimately empty
  // rather than an error.
  const studioForScene = addon.UnityRs.fromBuffers([
    { name: 'text.assets', data: syntheticTextAsset() },
  ])
  assert.deepEqual(studioForScene.scene(), [])

  const populatedScene = addon.UnityRs.fromBuffers([
    { name: 'scene.assets', data: syntheticGameObject() },
  ])
  const legacyNodes = populatedScene.scene(1)
  const limitedNodes = populatedScene.sceneWithLimits({
    maximumGameObjects: 1,
    maximumTotalComponents: 0,
    maximumTotalTransformChildReferences: 0,
    maximumTotalMaterialReferences: 0,
    maximumTotalBoneReferences: 0,
    maximumHierarchyEdges: 0,
    maximumIndexBytes: 1024,
  })
  assert.deepEqual(limitedNodes, legacyNodes)
  assert.equal(limitedNodes.length, 1)
  assert.equal(limitedNodes[0].name, 'Node Root')
  assert.throws(
    () => populatedScene.sceneWithLimits({ maximumGameObjects: 0 }),
    /GameObject|limit/i,
  )
  assert.throws(
    () => populatedScene.sceneWithLimits({ maximumIndexBytes: 0 }),
    /index bytes|limit/i,
  )
}

console.log('node api: multi-buffer, resource range and scene ok')

// Export and extraction, both of which write to disk.
{
  const outputDirectory = fs.mkdtempSync(path.join(os.tmpdir(), 'unity-rs-node-export-'))
  try {
    const exportStudio = addon.UnityRs.fromBuffers([
      { name: 'text.assets', data: syntheticTextAsset() },
    ])
    const report = exportStudio.export(outputDirectory)
    assert.equal(report.failures.length, 0)
    assert.equal(report.exported.length, 1)
    assert.equal(report.exported[0].classId, 49)
    assert.ok(fs.existsSync(report.exported[0].outputPath))

    // A second run without overwrite must not clobber what the first wrote.
    const again = exportStudio.export(outputDirectory)
    assert.equal(again.exported.length, 0)

    const configuredRoot = path.join(outputDirectory, 'configured')
    const configured = exportStudio.exportWithOptions(configuredRoot, {
      mode: ' RaW ',
      filenameFormat: ' PaTh_Id ',
      imageFormat: ' BmP ',
      jpegQuality: 100,
      audioFormat: ' RaW ',
      overwriteExisting: false,
      restoreTextAssetExtension: false,
      prettyJson: false,
      maximumObjects: 1,
      maximumTotalOutputBytes: 1024,
      maximumMetadataBytes: 1024,
      maximumRawObjectBytes: 1024,
      maximumTypeTreeJsonBytes: 1024,
      maximumTypeTreeDumpBytes: 1024,
      maximumTextAssetBytes: 1024,
      maximumSimpleAssetBytes: 1024,
      maximumAudioOutputBytes: 1024,
      maximumTextureOutputBytes: 1024,
      maximumTextureArrayOutputBytes: 1024,
      maximumTextureArrayBundleBytes: 1024,
      maximumSpriteOutputBytes: 1024,
      maximumShaderOutputBytes: 1024,
      maximumMonobehaviourJsonBytes: 1024,
      maximumMeshObjectBytes: 1024,
      maximumMeshOutputBytes: 1024,
    })
    assert.equal(configured.failures.length, 0)
    assert.equal(configured.exported.length, 1)
    assert.equal(configured.exported[0].payloadKind, 'raw')
    assert.equal(path.basename(configured.exported[0].outputPath), '7.dat')


    const metadataLimitedRoot = path.join(outputDirectory, 'metadata-limited')
    assert.throws(
      () => exportStudio.exportWithOptions(metadataLimitedRoot, {
        maximumMetadataBytes: 0,
      }),
      /export metadata exceeds/i,
    )
    const relativeExportPath = path.relative(outputDirectory, report.exported[0].outputPath)
    assert.equal(fs.existsSync(path.join(metadataLimitedRoot, relativeExportPath)), false)

    for (const field of [
      'maximumTotalOutputBytes',
      'maximumMetadataBytes',
      'maximumRawObjectBytes',
      'maximumTypeTreeJsonBytes',
      'maximumTypeTreeDumpBytes',
      'maximumTextAssetBytes',
      'maximumSimpleAssetBytes',
      'maximumAudioOutputBytes',
      'maximumTextureOutputBytes',
      'maximumTextureArrayOutputBytes',
      'maximumTextureArrayBundleBytes',
      'maximumSpriteOutputBytes',
      'maximumShaderOutputBytes',
      'maximumMonobehaviourJsonBytes',
      'maximumMeshObjectBytes',
      'maximumMeshOutputBytes',
    ]) {
      assert.throws(
        () => exportStudio.exportWithOptions(path.join(outputDirectory, 'invalid'), {
          [field]: -1,
        }),
        new RegExp(`${field} must be non-negative`, 'i'),
      )
    }
    assert.throws(
      () => exportStudio.exportWithOptions(path.join(outputDirectory, 'bad-mode'), {
        mode: 'mystery',
      }),
      /unsupported export mode/i,
    )
    assert.throws(
      () => exportStudio.exportWithOptions(path.join(outputDirectory, 'bad-name'), {
        filenameFormat: 'mystery',
      }),
      /unsupported filename format/i,
    )
    assert.throws(
      () => exportStudio.exportWithOptions(path.join(outputDirectory, 'bad-image'), {
        imageFormat: 'mystery',
      }),
      /unsupported image format/i,
    )
    assert.throws(
      () => exportStudio.exportWithOptions(path.join(outputDirectory, 'bad-audio'), {
        audioFormat: 'mystery',
      }),
      /unsupported audio format/i,
    )
    const oversizedChoice = 'é'.repeat(2048)
    for (const [field, option] of [
      ['export mode', { mode: oversizedChoice }],
      ['filename format', { filenameFormat: oversizedChoice }],
      ['image format', { imageFormat: oversizedChoice }],
      ['audio format', { audioFormat: oversizedChoice }],
    ]) {
      assert.throws(
        () => exportStudio.exportWithOptions(
          path.join(outputDirectory, `oversized-${field.replace(' ', '-')}`),
          option,
        ),
        (error) => {
          assert.match(
            error.message,
            new RegExp(`unsupported ${field} value of 4096 UTF-8 bytes`, 'i'),
          )
          assert.equal(error.message.includes(oversizedChoice), false)
          return true
        },
      )
    }
    assert.throws(
      () => exportStudio.exportWithOptions(path.join(outputDirectory, 'bad-quality'), {
        jpegQuality: 0,
      }),
      /jpegQuality.*1 through 100/i,
    )

    const textureStudio = addon.UnityRs.fromBuffers([
      { name: 'texture.assets', data: syntheticTexture2d() },
    ])
    const webp = textureStudio.exportWithOptions(
      path.join(outputDirectory, 'webp'),
      { imageFormat: 'webp' },
    )
    assert.equal(webp.failures.length, 0)
    assert.equal(webp.exported.length, 1)
    assert.match(webp.exported[0].outputPath, /\.webp$/)
    const webpBytes = fs.readFileSync(webp.exported[0].outputPath)
    assert.deepStrictEqual(webpBytes.subarray(0, 4), Buffer.from('RIFF'))
    assert.deepStrictEqual(webpBytes.subarray(8, 12), Buffer.from('WEBP'))

    // Batch export reaches the same PNG effort/filter knobs as encodeImage:
    // fast produces a different, still-valid PNG stream than the default.
    const pngDefault = textureStudio.exportWithOptions(
      path.join(outputDirectory, 'png-default'),
      {},
    )
    const pngFast = textureStudio.exportWithOptions(
      path.join(outputDirectory, 'png-fast'),
      { compression: 'fast', pngFilter: 'auto' },
    )
    assert.equal(pngDefault.failures.length, 0)
    assert.equal(pngFast.failures.length, 0)
    const pngDefaultBytes = fs.readFileSync(pngDefault.exported[0].outputPath)
    const pngFastBytes = fs.readFileSync(pngFast.exported[0].outputPath)
    const pngSignature = Buffer.from('\x89PNG\r\n\x1a\n', 'binary')
    assert.deepEqual(pngDefaultBytes.subarray(0, 8), pngSignature)
    assert.deepEqual(pngFastBytes.subarray(0, 8), pngSignature)
    assert.notDeepEqual(pngDefaultBytes, pngFastBytes)
    assert.throws(
      () => textureStudio.exportWithOptions(path.join(outputDirectory, 'png-bad'), {
        compression: 'turbo',
      }),
      /unsupported PNG compression/i,
    )

    const audioStudio = addon.UnityRs.fromBuffers([
      { name: 'legacy-pcm.assets', data: syntheticLegacyPcm() },
    ])
    const rawAudio = audioStudio.exportWithOptions(
      path.join(outputDirectory, 'raw-audio'),
      { audioFormat: 'raw' },
    )
    assert.equal(rawAudio.failures.length, 0)
    assert.equal(rawAudio.exported.length, 1)
    assert.match(rawAudio.exported[0].outputPath, /\.AudioClip$/)
    assert.deepStrictEqual(
      fs.readFileSync(rawAudio.exported[0].outputPath),
      Buffer.from([1, 2, 3, 4]),
    )
    const limitedWav = audioStudio.exportWithOptions(
      path.join(outputDirectory, 'limited-wav'),
      { audioFormat: 'wav', maximumAudioOutputBytes: 47 },
    )
    assert.equal(limitedWav.exported.length, 0)
    assert.equal(limitedWav.failures.length, 1)
    assert.match(limitedWav.failures[0].error, /exceeding limit/i)

    // Extraction of a plain file copies it through unchanged.
    const source = path.join(outputDirectory, 'input.assets')
    fs.writeFileSync(source, syntheticTextAsset())
    const extractRoot = path.join(outputDirectory, 'extracted')
    const extraction = addon.UnityRs.extract(source, extractRoot)
    assert.equal(extraction.failureCount, 0)
    assert.ok(extraction.outputBytes > 0n)

    const mixed = path.join(outputDirectory, 'mixed-load')
    fs.mkdirSync(mixed)
    fs.writeFileSync(path.join(mixed, 'a-good.assets'), syntheticTextAsset())
    const archive = Buffer.concat([
      Buffer.from('UnityArchive\0'),
      Buffer.from([0, 0, 0, 5]),
      Buffer.from('5.x.x\0'),
      Buffer.from('5.0.0f4\0'),
    ])
    fs.writeFileSync(path.join(mixed, 'b-archive.unity3d'), archive)
    const tolerant = addon.UnityRs.openWith(mixed, {
      skipUnreadableInputs: true,
      maximumDiagnosticBytes: 256 * 1024 * 1024,
    })
    assert.equal(tolerant.loadDiagnosticCount, 1)
    const loadDiagnostic = tolerant.loadDiagnosticPage(0, 1)[0]
    assert.match(loadDiagnostic.path, /b-archive/)
    assert.match(loadDiagnostic.message, /UnityArchive/)
    assert.deepStrictEqual(tolerant.loadDiagnosticPage(1), [])
    assert.throws(
      () => addon.UnityRs.openWith(mixed, {
        skipUnreadableInputs: true,
        maximumDiagnosticBytes: 0,
      }),
      /load diagnostics require/i,
    )
  } finally {
    fs.rmSync(outputDirectory, { recursive: true, force: true })
  }
}

// Static FBX for a collection with no renderable geometry is refused rather
// than emitting an empty scene.
{
  const fbxStudio = addon.UnityRs.fromBuffers([
    { name: 'text.assets', data: syntheticTextAsset() },
  ])
  assert.throws(() => fbxStudio.readStaticFbx())

  // The binary encoding had no binding at all until the writer was wired up.
  // This suite has no model fixture, so the bytes are checked by the Python
  // and CLI tests; what is checked here is that the methods exist and behave
  // like their text counterparts. Asserting they are functions first
  // distinguishes "not bound" from "bound and raised", which assert.throws
  // alone would not.
  assert.strictEqual(typeof fbxStudio.readStaticFbxBinary, 'function')
  assert.strictEqual(typeof fbxStudio.readFbxBinary, 'function')
  assert.throws(() => fbxStudio.readStaticFbxBinary())
  assert.throws(() => fbxStudio.readFbxBinary())
}

// Per-GameObject FBX planning, which had no Node binding: a caller could
// export the whole collection but not one branch of it.
//
// This suite has no model fixture, so the bytes are covered by the CLI and
// Python tests. What is checked here is that a collection with no models
// yields an empty plan rather than an error -- which is the answer, not a
// failure -- and that selecting an object that does not exist is an error
// rather than an empty file.
{
  const planStudio = addon.UnityRs.fromBuffers([
    { name: 'text.assets', data: syntheticTextAsset() },
  ])
  assert.deepStrictEqual(planStudio.splitObjectFbxCandidates(), [])
  assert.deepStrictEqual(planStudio.animatorFbxCandidates(), [])
  assert.throws(() => planStudio.readGameObjectFbx(0, 999n))
}

console.log('node api: export, extract and fbx ok')

// Live2D discovery on a collection that has none, which is a fact rather than
// an error.
{
  const emptyStudio = addon.UnityRs.fromBuffers([
    { name: 'text.assets', data: syntheticTextAsset() },
  ])
  assert.deepEqual(emptyStudio.live2DPackages(), [])
  // A non-clip object must be refused rather than misread as one.
  const objects = emptyStudio.objectPage(0)
  assert.throws(() => emptyStudio.readAnimationClipInfo(0, objects[0].pathId))
  assert.throws(() => emptyStudio.readAnimatorController(0, objects[0].pathId))
}

// Stable component references and container tables use real object layouts,
// not method-existence smoke. This catches field order, nested PPtr mapping,
// BigInt width and caller budgets in the generated Node-API marshalling path.
{
  const legacyStudio = addon.UnityRs.fromBuffers([
    { name: 'legacy-animation.assets', data: syntheticLegacyAnimation() },
  ])
  assert.deepStrictEqual(legacyStudio.readLegacyAnimation(0, 7n), {
    pathId: 7n,
    gameObject: { fileId: 0, pathId: 31n },
    enabled: 1,
    defaultClip: { fileId: 0, pathId: 70n },
    clips: [
      { fileId: 0, pathId: 71n },
      { fileId: 0, pathId: 72n },
    ],
    trailingBytes: 2n,
  })
  assert.throws(() => legacyStudio.readLegacyAnimation(0, 7n, 1))

  const overrideStudio = addon.UnityRs.fromBuffers([
    {
      name: 'animator-override.assets',
      data: syntheticAnimatorOverrideController(),
    },
  ])
  assert.deepStrictEqual(
    overrideStudio.readAnimatorOverrideController(0, 7n),
    {
      pathId: 7n,
      name: 'node override controller',
      controller: { fileId: 0, pathId: 90n },
      clipOverrides: [
        {
          originalClip: { fileId: 0, pathId: 71n },
          overrideClip: { fileId: 0, pathId: 73n },
        },
        {
          originalClip: { fileId: 0, pathId: 72n },
          overrideClip: { fileId: 0, pathId: 74n },
        },
      ],
      trailingBytes: 1n,
    },
  )
  assert.throws(() =>
    overrideStudio.readAnimatorOverrideController(0, 7n, 1),
  )

  const containerStudio = addon.UnityRs.fromBuffers([
    { name: 'container-metadata.assets', data: syntheticContainerMetadataObjects() },
  ])
  assert.deepStrictEqual(containerStudio.readAssetBundle(0, 7n), {
    pathId: 7n,
    name: 'node-bundle',
    objectName: 'root',
    assetBundleName: 'node-bundle',
    preloadTable: [
      { fileId: 0, pathId: 11n },
      { fileId: 0, pathId: 12n },
    ],
    container: [
      {
        key: 'bundle/first',
        preloadIndex: 0,
        preloadSize: 1,
        asset: { fileId: 0, pathId: 11n },
      },
      {
        key: 'bundle/second',
        preloadIndex: 1,
        preloadSize: 1,
        asset: { fileId: 0, pathId: 12n },
      },
    ],
    dependencies: ['shared-a', 'shared-b'],
    isStreamedSceneAssetBundle: false,
  })
  assert.deepStrictEqual(containerStudio.readResourceManager(0, 8n), {
    pathId: 8n,
    container: [
      { key: 'resource/first', asset: { fileId: 0, pathId: 21n } },
      { key: 'resource/second', asset: { fileId: 0, pathId: 22n } },
    ],
  })
  assert.deepStrictEqual(containerStudio.readPreloadData(0, 9n), {
    pathId: 9n,
    name: 'node-preload',
    assets: [
      { fileId: 0, pathId: 31n },
      { fileId: 0, pathId: 32n },
    ],
  })
  assert.throws(() => containerStudio.readAssetBundle(0, 7n, 1))
  assert.throws(() => containerStudio.readAssetBundle(0, 7n, undefined, undefined, 1))
  assert.throws(() => containerStudio.readResourceManager(0, 8n, 1))
  assert.throws(() => containerStudio.readPreloadData(0, 9n, 1))
}

{
  const atlasStudio = addon.UnityRs.fromBuffers([
    { name: 'sprite-atlas.assets', data: syntheticSpriteAtlas() },
  ])
  assert.deepStrictEqual(atlasStudio.readSpriteAtlas(0, 9n), {
    pathId: 9n,
    name: 'node atlas',
    packedSprites: [{ fileId: 0, pathId: 7n }],
    packedSpriteNames: ['node sprite'],
    renderDataEntries: [
      {
        key: { guidBytes: Buffer.alloc(16), value: -5n },
        texture: { fileId: 0, pathId: 10n },
        alphaTexture: { fileId: 0, pathId: 12n },
        textureRect: { x: 0, y: 0, width: 1, height: 1 },
        textureRectOffset: { x: 0, y: 0 },
        atlasRectOffset: { x: 0, y: 0 },
        uvTransform: { x: 0, y: 0, z: 1, w: 1 },
        downscaleMultiplier: 1,
        settings: {
          raw: 2,
          packed: false,
          packingMode: 1,
          packingRotation: 0,
          meshType: 0,
        },
        secondaryTextures: [],
      },
      {
        key: {
          guidBytes: Buffer.concat([Buffer.from([1]), Buffer.alloc(15)]),
          value: 9n,
        },
        texture: { fileId: 0, pathId: 11n },
        alphaTexture: { fileId: 0, pathId: 0n },
        textureRect: { x: 1, y: 2, width: 3, height: 4 },
        textureRectOffset: { x: 5, y: 6 },
        atlasRectOffset: { x: 7, y: 8 },
        uvTransform: { x: 9, y: 10, z: 11, w: 12 },
        downscaleMultiplier: 0.5,
        settings: {
          raw: 79,
          packed: true,
          packingMode: 1,
          packingRotation: 3,
          meshType: 1,
        },
        secondaryTextures: [
          {
            texture: { fileId: 0, pathId: 99n },
            name: 'mask',
          },
        ],
      },
    ],
    tag: 'node-tag',
    isVariant: true,
  })
  assert.throws(() => atlasStudio.readSpriteAtlas(0, 9n, 0))
  assert.throws(() => atlasStudio.readSpriteAtlas(0, 9n, undefined, 5))
  assert.throws(() =>
    atlasStudio.readSpriteAtlas(0, 9n, undefined, undefined, 12),
  )
}

{
  const spriteStudio = addon.UnityRs.fromBuffers([
    { name: 'sprite.assets', data: syntheticSpriteMetadata() },
  ])
  // The sprite-page cache counters marshal as BigInt and start at zero on a
  // fresh collection; the hit/miss semantics are pinned by core and Python
  // decode tests.
  assert.deepStrictEqual(spriteStudio.spritePageCacheStats(), {
    hits: 0n,
    misses: 0n,
  })
  assert.deepStrictEqual(spriteStudio.readSpriteMetadata(0, 7n), {
    objectIndex: 0,
    pathId: 7n,
    name: 'node sprite',
    rect: { x: 1, y: 2, width: 3, height: 4 },
    offset: { x: 5, y: 6 },
    border: { x: 7, y: 8, z: 9, w: 10 },
    pixelsToUnits: 100,
    pivot: { x: 0.25, y: 0.75 },
    extrude: 3,
    isPolygon: true,
    renderDataKey: {
      guidBytes: Buffer.from(Array.from({ length: 16 }, (_, index) => index)),
      value: -5n,
    },
    atlasTags: ['tag'],
    spriteAtlas: { fileId: 0, pathId: 9n },
    renderData: {
      texture: { fileId: 0, pathId: 8n },
      alphaTexture: { fileId: 0, pathId: 10n },
      secondaryTextures: [
        { texture: { fileId: 0, pathId: 12n }, name: 'mask' },
      ],
      textureRect: { x: 11, y: 12, width: 13, height: 14 },
      textureRectOffset: { x: 15, y: 16 },
      atlasRectOffset: { x: 17, y: 18 },
      settings: {
        raw: 79,
        packed: true,
        packingMode: 'rectangle',
        packingRotation: 3,
        meshType: 'tight',
      },
      uvTransform: { x: 19, y: 20, z: 21, w: 22 },
      downscaleMultiplier: 0.5,
      meshTriangles: [],
    },
  })
  for (const limits of [
    { maximumEntries: 0 },
    { maximumStringBytes: 5 },
    { maximumTotalStringBytes: 5 },
    { maximumMeshBytes: 0 },
  ]) {
    assert.throws(() => spriteStudio.readSpriteMetadata(0, 7n, limits))
  }

  const tightStudio = addon.UnityRs.fromBuffers([
    { name: 'tight-sprite.assets', data: syntheticTightSpriteMetadata() },
  ])
  const tight = tightStudio.readSpriteMetadata(0, 7n)
  assert.strictEqual(tight.renderData.settings.packingMode, 'tight')
  assert.strictEqual(tight.renderData.settings.meshType, 'full_rect')
  assert.strictEqual(tight.renderData.meshTriangles.length, 1)
  assert.deepStrictEqual(Object.keys(tight.renderData.meshTriangles[0]), [
    'first',
    'second',
    'third',
  ])
}

{
  const avatarStudio = addon.UnityRs.fromBuffers([
    { name: 'tuanjie-avatar.assets', data: syntheticTuanjieAvatar() },
  ])
  assert.deepStrictEqual(avatarStudio.readAvatar(0, 7n), {
    pathId: 7n,
    name: 'node-tuanjie-avatar',
    declaredSize: 0,
    declaredAvatarSize: 0,
    skeletonNodeCount: 0,
    humanSkeletonNodeCount: 0,
    pathCount: 1,
    paths: [{ hash: 0xfeedbeef, path: 'Root/Hips' }],
    hasHumanDescription: true,
    humanBoneCount: 0,
    skeletonBoneCount: 0,
    rootMotionBoneName: 'Hips',
  })
  assert.throws(() => avatarStudio.readAvatar(0, 7n, 1))
}

console.log('node api: animation and live2d discovery ok')

// Animated FBX and Live2D materialization on a collection that has neither.
{
  const barren = addon.UnityRs.fromBuffers([
    { name: 'text.assets', data: syntheticTextAsset() },
  ])
  // No renderable geometry is refused rather than written as an empty scene.
  assert.throws(() => barren.readFbx())
  // No Live2D model is an empty result rather than an error, and the result
  // carries its diagnostics: a package that could not include something has to
  // be able to say so, or a short package reads as a complete one.
  assert.deepEqual(barren.readLive2DPackages(), { packages: [], diagnostics: [] })
}

// The load options, which have to combine: a UnityCN-encrypted archive whose
// header version was also stripped is one file with two facts about it, and a
// factory per option can only state one of them.
{
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'unity-rs-node-options-'))
  const fixture = path.join(directory, 'options.assets')
  try {
    fs.writeFileSync(fixture, syntheticTextAsset())
    const opened = addon.UnityRs.openWith(fixture, {
      unityVersion: '2022.3.62f1',
      skipUnreadableInputs: true,
      maximumInputFiles: 8,
      maximumPathBytes: 1024 * 1024,
      maximumTotalPathBytes: 64 * 1024 * 1024,
    })
    assert.equal(opened.fileCount, 1)
    assert.deepEqual(opened.readText(0, 7n), Buffer.from('hello node'))
    // Omitting the options entirely is the same as opening plainly.
    assert.equal(addon.UnityRs.openWith(fixture).fileCount, 1)
    // Memory inputs carry the same Core load options. The arguments are
    // appended after the established byte limit so older calls are unchanged.
    const memoryOpened = addon.UnityRs.fromBuffer(
      syntheticTextAsset(),
      'memory-options.assets',
      undefined,
      {
        unityVersion: '2022.3.62f1',
        maximumPathBytes: 1024 * 1024,
      },
    )
    assert.equal(memoryOpened.fileCount, 1)
    assert.throws(
      () => addon.UnityRs.fromBuffer(
        syntheticTextAsset(),
        'memory-options.assets',
        undefined,
        { maximumPathBytes: 1 },
      ),
      /asset path/i,
    )
    // A key is 16 bytes, and a wrong length is refused rather than padded.
    assert.throws(
      () => addon.UnityRs.openWith(fixture, { unityCnKey: Buffer.alloc(8) }),
      /exactly 16 bytes/,
    )
    assert.throws(
      () => addon.UnityRs.openWith(fixture, { unityVersion: 'not-a-version' }),
      /unsupported Unity version/,
    )
    assert.throws(
      () => addon.UnityRs.openWith(fixture, { maximumPathBytes: 1 }),
      /asset path/i,
    )
    assert.throws(
      () => addon.UnityRs.openWith(fixture, { maximumTotalPathBytes: 1 }),
      /traversal paths total/i,
    )
  } finally {
    fs.rmSync(directory, { recursive: true, force: true })
  }
}

console.log('node api: load options ok')

console.log('node api: animated fbx and live2d materialization ok')

// Textured FBX and ACL inspection on inputs that have neither.
{
  const barren = addon.UnityRs.fromBuffers([
    { name: 'text.assets', data: syntheticTextAsset() },
  ])
  assert.throws(() => barren.readFbxWithTextures())
  assert.throws(
    () => barren.readFbxWithTextures(undefined, 'not-an-image-format'),
    /image format/i,
  )
  // The scene OBJ, which existed only inside the CLI. No renderable geometry
  // is refused rather than written as an empty scene, same as the FBX.
  assert.throws(() => barren.readModelObj())
  assert.throws(
    () => barren.readModelObj(undefined, undefined, 'not-an-image-format'),
    /image format/i,
  )
  // A TextAsset is not an AnimationClip, so asking for its ACL blob is
  // refused rather than answered with zeroes.
  const objects = barren.objectPage(0)
  assert.throws(() => barren.readAclTracks(0, objects[0].pathId))
}

console.log('node api: textured fbx and acl inspection ok')

// MonoBehaviour schemas, which is how a stripped managed layout is read.
{
  // A schema whose class never matches leaves the object at its engine-owned
  // prefix rather than guessing, and an empty node list is refused outright.
  const schemaStudio = addon.UnityRs.fromBuffers([
    { name: 'text.assets', data: syntheticTextAsset() },
  ])
  const objects = schemaStudio.objectPage(0)
  assert.throws(
    () =>
      schemaStudio.readMonoBehaviourJsonWithSchemas(0, objects[0].pathId, [
        { assemblyName: 'A.dll', className: 'A', nodes: [] },
      ]),
    /root node/i,
  )
  const tooManySchemas = new Array(100_001)
  let schemaElementRead = false
  Object.defineProperty(tooManySchemas, 0, {
    get() {
      schemaElementRead = true
      throw new Error('schema element read before collection count check')
    },
  })
  assert.throws(
    () =>
      schemaStudio.readMonoBehaviourJsonWithSchemas(
        0,
        objects[0].pathId,
        tooManySchemas,
      ),
    /schema collection.*exceeding limit/i,
  )
  assert.equal(schemaElementRead, false)

  const tooManyNodes = new Array(100_001)
  let nodeElementRead = false
  Object.defineProperty(tooManyNodes, 0, {
    get() {
      nodeElementRead = true
      throw new Error('schema node read before node count check')
    },
  })
  assert.throws(
    () =>
      schemaStudio.readMonoBehaviourJsonWithSchemas(0, objects[0].pathId, [
        {
          assemblyName: 'A.dll',
          className: 'A',
          nodes: tooManyNodes,
        },
      ]),
    /nodes.*maximum/i,
  )
  assert.equal(nodeElementRead, false)
  assert.throws(
    () =>
      schemaStudio.readMonoBehaviourJsonWithSchemas(0, objects[0].pathId, [
        {
          assemblyName: 'A.dll',
          className: 'A',
          unityVersion: 'not-a-unity-version',
          nodes: [{ typeName: 'A', fieldName: 'Base', level: 0, align: false }],
        },
      ]),
    /invalid Unity version/i,
  )
  // A TextAsset is not a MonoBehaviour, so the read is refused rather than
  // producing an object shaped by the schema.
  assert.throws(() =>
    schemaStudio.readMonoBehaviourJsonWithSchemas(0, objects[0].pathId, [
      {
        assemblyName: 'A.dll',
        className: 'A',
        nodes: [{ typeName: 'A', fieldName: 'Base', level: 0, align: false }],
      },
    ]),
  )

  // A file that carries its own tree is read through that tree, and the read
  // says so: the caller cannot otherwise tell whether a value came from Unity
  // or from a schema they supplied.
  const embedded = addon.UnityRs.fromBuffers([
    { name: 'expression.assets', data: syntheticCubismExpression() },
  ])
  const behaviour = embedded.objectPage(0)[0]
  const direct = embedded.readMonoBehaviourJson(0, behaviour.pathId)
  assert.strictEqual(direct.source, 'embedded')
  assert.strictEqual(JSON.parse(direct.json.toString('utf8')).m_Name, 'node-expression')
  assert.throws(
    () =>
      embedded.readMonoBehaviourJson(
        0,
        behaviour.pathId,
        false,
        direct.json.length - 1,
      ),
    /limit|exceed/i,
  )
  const read = embedded.readMonoBehaviourJsonWithSchemas(0, behaviour.pathId, [
    {
      assemblyName: 'Nothing.dll',
      className: 'Nothing',
      nodes: [{ typeName: 'Nothing', fieldName: 'Base', level: 0, align: false }],
    },
  ])
  assert.strictEqual(read.source, 'embedded')
  assert.strictEqual(JSON.parse(read.json.toString('utf8')).m_Name, 'node-expression')
  assert.throws(() =>
    schemaStudio.readMonoBehaviourJson(0, objects[0].pathId),
  )
}

console.log('node api: monobehaviour schemas ok')

// Complete Live2D package adapters. The model and renderer trees are stripped
// from this fixture, and its only motion is ACL-compressed, so neither half can
// be faked by a method-existence check: schemas must recover the PPtrs and the
// callback must produce the motion document.
async function testLive2dPackageAdapters() {
  const embedded = addon.UnityRs.fromBuffers([
    { name: 'expression.assets', data: syntheticCubismExpression() },
  ])
  const embeddedBehaviour = embedded.objectPage(0)[0]
  const embeddedRead = await embedded.readMonoBehaviourJsonAsync(
    0,
    embeddedBehaviour.pathId,
  )
  assert.strictEqual(embeddedRead.source, 'embedded')
  assert.strictEqual(
    JSON.parse(embeddedRead.json.toString('utf8')).m_Name,
    'node-expression',
  )
  await assert.rejects(
    embedded.readMonoBehaviourJsonAsync(
      0,
      embeddedBehaviour.pathId,
      false,
      embeddedRead.json.length - 1,
    ),
    /limit|exceed/i,
  )

  const studio = addon.UnityRs.fromBuffers(syntheticStrippedAclLive2dPackage())
  const strippedBehaviour = studio
    .objectPage(0)
    .find(({ classId }) => classId === 114)
  assert.ok(strippedBehaviour)
  assert.throws(
    () => studio.readMonoBehaviourJson(0, strippedBehaviour.pathId),
    /type tree|schema/i,
  )
  await assert.rejects(
    studio.readMonoBehaviourJsonAsync(0, strippedBehaviour.pathId),
    /type tree|schema/i,
  )
  const withoutSchemas = studio.readLive2DPackages()
  assert.deepStrictEqual(withoutSchemas.packages, [])
  assert.ok(
    withoutSchemas.diagnostics.some(
      ({ kind }) => kind === 'MissingEmbeddedTypeTree',
    ),
  )

  const schemas = live2dSchemas()
  const schemaRead = await studio.readMonoBehaviourJsonWithSchemasAsync(
    0,
    strippedBehaviour.pathId,
    schemas,
  )
  assert.strictEqual(schemaRead.source, 'schema')
  assert.ok(JSON.parse(schemaRead.json.toString('utf8'))._moc)

  // JavaScript values must be copied before this call returns, but complete
  // Core schema validation and registry indexing belong to the worker. A bad
  // Unity version therefore rejects the Promise instead of synchronously
  // throwing while the event loop is still trying to queue the task.
  const invalidSchemas = [
    { ...schemas[0], unityVersion: 'not-a-unity-version' },
  ]
  let invalidSchemaRead
  assert.doesNotThrow(() => {
    invalidSchemaRead = studio.readMonoBehaviourJsonWithSchemasAsync(
      0,
      strippedBehaviour.pathId,
      invalidSchemas,
    )
  })
  await assert.rejects(invalidSchemaRead, /invalid Unity version/i)

  const schemaOnly = studio.readLive2DPackagesWithSchemas(
    schemas,
    1024 * 1024,
    4 * 1024 * 1024,
  )
  assert.strictEqual(schemaOnly.packages.length, 1)
  assert.strictEqual(schemaOnly.packages[0].name, 'Hero')
  const schemaFiles = new Map(
    schemaOnly.packages[0].files.map(({ fileName, data }) => [fileName, data]),
  )
  assert.deepStrictEqual(schemaFiles.get('Hero.moc3'), Buffer.from('MOC3\x09', 'binary'))
  assert.ok(schemaFiles.get('Hero.model3.json').includes(Buffer.from('textures/face.png')))
  assert.deepStrictEqual(
    schemaFiles.get('textures/face.png').subarray(0, 8),
    Buffer.from('\x89PNG\r\n\x1a\n', 'binary'),
  )
  // Without an ACL decoder the package survives but the animation says why it
  // was omitted instead of silently pretending to be complete.
  assert.ok(schemaOnly.diagnostics.some(({ kind }) => kind === 'MotionReadFailed'))
  assert.ok(!schemaFiles.has('motions/node-acl-motion.motion3.json'))

  assert.throws(
    () => studio.readLive2DPackagesWithSchemas(schemas, 4, 4 * 1024 * 1024),
    /limit|exceed/i,
  )
  assert.throws(
    () => studio.readLive2DPackagesWithSchemas(schemas, -1, 1024),
    /maximumFileBytes must be non-negative/i,
  )
  assert.throws(
    () => studio.readLive2DPackagesWithSchemas(schemas, 1024, -1),
    /maximumTotalBytes must be non-negative/i,
  )

  let invalidPackageRead
  assert.doesNotThrow(() => {
    invalidPackageRead = studio.readLive2DPackagesWithAclDecoder(
      () => {
        throw new Error('invalid schema must fail before ACL decoding')
      },
      invalidSchemas,
      1024 * 1024,
      4 * 1024 * 1024,
    )
  })
  await assert.rejects(invalidPackageRead, /invalid Unity version/i)

  let calls = 0
  const decoded = await studio.readLive2DPackagesWithAclDecoder(
    (request) => {
      calls += 1
      assert.strictEqual(request.frameCount, 2)
      assert.strictEqual(request.boneCount, 0)
      assert.strictEqual(request.sampleRate, 30)
      assert.strictEqual(request.declaredCurveCount, 1)
      assert.strictEqual(request.useFastSampleMode, true)
      assert.strictEqual(request.compressedTracks.length, 32)
      assert.strictEqual(request.compressedTracks.readUInt32LE(8), 0xac11ac11)
      assert.deepStrictEqual(request.decoderMap, [])
      return {
        times: [0, 1 / 30],
        bindingIndices: [0],
        values: [0.25, 0.75],
        followingCurveOffset: 1,
      }
    },
    schemas,
    1024 * 1024,
    4 * 1024 * 1024,
  )
  assert.strictEqual(calls, 1)
  assert.strictEqual(decoded.packages.length, 1)
  assert.deepStrictEqual(decoded.diagnostics, [])
  const decodedFiles = new Map(
    decoded.packages[0].files.map(({ fileName, data }) => [fileName, data]),
  )
  const motion = decodedFiles.get('motions/node-acl-motion.motion3.json')
  assert.ok(motion)
  assert.strictEqual(JSON.parse(motion.toString('utf8')).Meta.Fps, 60)
  const manifest = JSON.parse(decodedFiles.get('Hero.model3.json').toString('utf8'))
  assert.deepStrictEqual(
    manifest.FileReferences.Motions['node-acl-motion'],
    [{ File: 'motions/node-acl-motion.motion3.json' }],
  )

  let invalidCalls = 0
  const malformed = await studio.readLive2DPackagesWithAclDecoder(
      () => {
        invalidCalls += 1
        return {
        times: [0],
        bindingIndices: [0],
        values: [0.25],
        followingCurveOffset: 1,
        }
      },
      schemas,
      1024 * 1024,
      4 * 1024 * 1024,
  )
  assert.strictEqual(invalidCalls, 1)
  assert.strictEqual(malformed.packages.length, 1)
  assert.ok(
    malformed.diagnostics.some(
      ({ kind, detail }) =>
        kind === 'MotionReadFailed'
        && /returned 1 times|2 declared frames/i.test(detail),
    ),
  )
  assert.ok(
    !malformed.packages[0].files.some(
      ({ fileName }) => fileName.endsWith('.motion3.json'),
    ),
  )

  let unresolvedCalls = 0
  const unresolved = await studio.readLive2DPackagesWithAclDecoder(() => {
    unresolvedCalls += 1
    return {
      times: [],
      bindingIndices: [],
      values: [],
      followingCurveOffset: 0,
    }
  })
  assert.strictEqual(unresolvedCalls, 0)
  assert.deepStrictEqual(unresolved.packages, [])
  assert.ok(unresolved.diagnostics.length > 0)
  console.log('node api: Live2D schema and ACL package adapters ok')
}

testLive2dPackageAdapters().catch((error) => {
  console.error(error)
  process.exitCode = 1
})

// Oodle decoder injection. Core ships no Oodle decoder, so a bundle marked
// with it is unreadable until the caller supplies one.
async function testOodleInjection() {
  const oodleDirectory = fs.mkdtempSync(path.join(os.tmpdir(), 'unity-rs-node-oodle-'))
  try {
    const inner = syntheticTextAsset()
    const bundlePath = path.join(oodleDirectory, 'oodle.unity3d')
    fs.writeFileSync(bundlePath, syntheticOodleBundle('CAB-oodle', inner))

    // Without a decoder the bundle is refused rather than silently skipped.
    assert.throws(() => new addon.UnityRs(bundlePath))

    // With one, it loads. This fixture's block is stored verbatim, so the
    // decoder is the identity -- what matters is that it is called at all and
    // that its bytes are what Core goes on to parse.
    let calls = 0
    const studio = await addon.UnityRs.openWithOodle(bundlePath, (input, expected) => {
      calls += 1
      assert.equal(input.length, expected)
      return input
    })
    assert.equal(calls, 1)
    assert.equal(studio.fileCount, 1)
    assert.deepEqual(studio.readText(0, 7n), Buffer.from('hello node'))

    // A decoder returning the wrong length is an error, not a short read.
    await assert.rejects(
      addon.UnityRs.openWithOodle(bundlePath, (input) => input.subarray(0, 4)),
      /expected/i,
    )
  } finally {
    fs.rmSync(oodleDirectory, { recursive: true, force: true })
  }
  console.log('node api: oodle injection ok')
}

testOodleInjection().catch((error) => {
  console.error(error)
  process.exitCode = 1
})

// ACL decoder injection. Core ships no ACL decoder, so a caller supplies one
// and Core validates whatever it returns.
async function testAclInjection() {
  const aclInput = syntheticCubismAclAnimationClip()
  const aclStudio = addon.UnityRs.fromBuffer(
    aclInput,
    'tuanjie-acl.assets',
    aclInput.length,
  )
  const clip = aclStudio.readAnimationClipInfo(0, 7n)
  assert.strictEqual(clip.pathId, 7n)
  assert.strictEqual(clip.name, 'node-acl-motion')
  assert.strictEqual(clip.hasMuscleClip, true)
  assert.ok(clip.muscleClipSize > 0)
  assert.strictEqual(clip.streamedCurveCount, 0)
  assert.strictEqual(clip.denseCurveCount, 0)
  assert.strictEqual(clip.constantValueCount, 0)
  assert.strictEqual(clip.hasAcl, true)
  assert.strictEqual(clip.aclFrameCount, 0)
  assert.strictEqual(clip.aclBoneCount, 0)
  assert.strictEqual(clip.aclSampleRate, 30)
  assert.strictEqual(clip.aclCurveCount, 0)
  assert.strictEqual(clip.aclTrackByteCount, 32n)
  assert.strictEqual(clip.aclDecoderCount, 0)
  assert.strictEqual(clip.aclUseFastSampleMode, true)
  assert.strictEqual(clip.hasStreamingInfo, true)
  assert.strictEqual(clip.streamingOffset, 0n)
  assert.strictEqual(clip.streamingSize, 0)
  assert.strictEqual(clip.streamingPath, '')
  let motionCalls = 0
  const motion = await aclStudio.readCubismClipMotionWithAclDecoder(
    0,
    7n,
    (request) => {
      motionCalls += 1
      assert.strictEqual(request.frameCount, 0)
      assert.strictEqual(request.boneCount, 0)
      assert.strictEqual(request.sampleRate, 30)
      assert.strictEqual(request.declaredCurveCount, 0)
      assert.strictEqual(request.useFastSampleMode, true)
      assert.strictEqual(request.compressedTracks.length, 32)
      assert.strictEqual(request.compressedTracks.readUInt32LE(8), 0xac11ac11)
      assert.deepStrictEqual(request.decoderMap, [])
      return {
        times: [],
        bindingIndices: [],
        values: [],
        followingCurveOffset: 0,
      }
    },
    { parameters: [], parts: [] },
    false,
  )
  assert.strictEqual(motionCalls, 1)
  assert.strictEqual(motion.name, 'node-acl-motion')
  assert.strictEqual(motion.duration, 1)
  assert.strictEqual(motion.curveCount, 0)
  assert.strictEqual(JSON.parse(motion.json.toString('utf8')).Meta.Fps, 60)

  // The bridge does not trust the callback: one time for a zero-frame clip is
  // structurally wrong and Core must reject it before it reaches the writer.
  await assert.rejects(
    aclStudio.readCubismClipMotionWithAclDecoder(0, 7n, () => ({
      times: [0],
      bindingIndices: [],
      values: [],
      followingCurveOffset: 0,
    })),
    /returned 1 times|0 declared frames/i,
  )
  let aclLengthGetterTouched = false
  const invalidTimes = []
  Object.defineProperty(invalidTimes, 0, {
    get() {
      aclLengthGetterTouched = true
      throw new Error('ACL time element was converted before its length')
    },
  })
  await assert.rejects(
    aclStudio.readCubismClipMotionWithAclDecoder(0, 7n, () => ({
      times: invalidTimes,
      bindingIndices: [],
      values: [],
      followingCurveOffset: 0,
    })),
    /returned 1 times|0 declared frames/i,
  )
  assert.strictEqual(aclLengthGetterTouched, false)

  const aclFbxInput = syntheticAclFbxModel()
  const aclFbxStudio = addon.UnityRs.fromBuffer(
    aclFbxInput,
    'acl-fbx.assets',
    aclFbxInput.length,
  )
  let fbxCalls = 0
  const aclFbx = await aclFbxStudio.readFbxWithAclDecoder((request) => {
    fbxCalls += 1
    assert.strictEqual(request.frameCount, 0)
    assert.strictEqual(request.declaredCurveCount, 0)
    assert.strictEqual(request.compressedTracks.length, 32)
    return {
      times: [],
      bindingIndices: [],
      values: [],
      followingCurveOffset: 0,
    }
  }, 1024 * 1024)
  assert.strictEqual(fbxCalls, 1)
  assert.ok(aclFbx.includes(Buffer.from('node acl model')))
  assert.ok(aclFbx.includes(Buffer.from('node-acl-motion')))

  let binaryCalls = 0
  const binaryFbx = await aclFbxStudio.readFbxBinaryWithAclDecoder((request) => {
    binaryCalls += 1
    assert.strictEqual(request.compressedTracks.length, 32)
    return {
      times: [],
      bindingIndices: [],
      values: [],
      followingCurveOffset: 0,
    }
  }, 1024 * 1024)
  assert.strictEqual(binaryCalls, 1)
  assert.deepStrictEqual(
    binaryFbx.subarray(0, 23),
    Buffer.from('Kaydara FBX Binary  \0\x1a\0', 'binary'),
  )
  assert.ok(binaryFbx.includes(Buffer.from('node acl model')))
  assert.ok(binaryFbx.includes(Buffer.from('node-acl-motion')))

  let selectedCalls = 0
  const selectedFbx = await aclFbxStudio.readGameObjectFbxWithAclDecoder(
    0,
    1n,
    (request) => {
      selectedCalls += 1
      assert.strictEqual(request.declaredCurveCount, 0)
      return {
        times: [],
        bindingIndices: [],
        values: [],
        followingCurveOffset: 0,
      }
    },
    true,
    1024 * 1024,
  )
  assert.strictEqual(selectedCalls, 1)
  assert.ok(selectedFbx.includes(Buffer.from('node acl model')))
  assert.ok(selectedFbx.includes(Buffer.from('node-acl-motion')))

  let disabledCalls = 0
  const staticSelected = await aclFbxStudio.readGameObjectFbxWithAclDecoder(
    0,
    1n,
    () => {
      disabledCalls += 1
      return {
        times: [],
        bindingIndices: [],
        values: [],
        followingCurveOffset: 0,
      }
    },
    false,
    1024 * 1024,
  )
  assert.strictEqual(disabledCalls, 0)
  assert.ok(staticSelected.includes(Buffer.from('node acl model')))
  assert.ok(!staticSelected.includes(Buffer.from('node-acl-motion')))
  await assert.rejects(
    aclFbxStudio.readFbxBinaryWithAclDecoder(() => ({
      times: [],
      bindingIndices: [],
      values: [],
      followingCurveOffset: 0,
    }), 1),
    /limit|exceed/i,
  )

  const barren = addon.UnityRs.fromBuffers([
    { name: 'text.assets', data: syntheticTextAsset() },
  ])
  // No renderable geometry, so the FBX is refused whether or not a decoder is
  // supplied -- the decoder must not turn an empty scene into a file.
  let calls = 0
  await assert.rejects(
    barren.readFbxWithAclDecoder(() => {
      calls += 1
      return { times: [], bindingIndices: [], values: [], followingCurveOffset: 0 }
    }),
  )
  // The clip path is never reached for this input, so the decoder stays unused
  // rather than being called speculatively.
  assert.equal(calls, 0)
  console.log('node api: acl injection ok')
}

testAclInjection().catch((error) => {
  console.error(error)
  process.exitCode = 1
})
