'use strict'

const assert = require('node:assert/strict')
const fs = require('node:fs')
const os = require('node:os')
const path = require('node:path')
const addon = require('../index.js')

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
function syntheticAclTracks() {
  const tracks = Buffer.alloc(32)
  tracks.writeUInt32LE(tracks.length, 0)
  tracks.writeUInt32LE(0xac11ac11, 8)
  tracks.writeUInt16LE(10, 12)
  tracks.writeUInt8(0, 14) // uniformly sampled algorithm
  tracks.writeUInt8(0, 15) // float1f tracks
  tracks.writeUInt32LE(0, 16)
  tracks.writeUInt32LE(0, 20)
  tracks.writeFloatLE(30, 24)
  tracks.writeUInt32LE(0, 28)
  tracks.writeUInt32LE(fnv1a32(tracks.subarray(8)), 4)
  return tracks
}

// Tuanjie 2022.3.55t4 stores the muscle block in little-endian m_AnimData and
// adds ACL tracks, a declared curve count, and the fast-sample flag.
function syntheticCubismAclAnimationClip() {
  const tracks = syntheticAclTracks()
  const acl = Buffer.concat([
    u32(0), // frame count
    u32(0), // bone count
    f32(30),
    u32(0), // declared curve count
    i32(tracks.length),
    tracks,
    i32(0), // decoder map
    Buffer.from([1]), // fast sample mode, not aligned before 2022.3.61
  ])
  const embedded = emptyCubismMuscle(acl)
  const payload = align(Buffer.concat([
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
    i32(0), // generic bindings
    i32(0), // PPtr curve mapping
    Buffer.from([1, 0, 0, 0]),
    i32(0), // events
  ]), 4)
  return finishV22Asset(74, payload, '2022.3.55t4')
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
const studio = addon.AssetStudio.fromBuffer(input, 'node.assets', input.length)

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
assert.throws(() => addon.AssetStudio.fromBuffer(input, 'node.assets', 1), /exceed/i)

// Texture2D rows leave the decoder bottom-up. Every consumer -- the Python
// binding, the CLI's encoded images and the managed exporter -- hands callers
// top-down rows, so this binding has to flip them too.
const DISPLAY_ORDER_PIXELS = Buffer.concat([
  TEXTURE_PIXELS.subarray(8),
  TEXTURE_PIXELS.subarray(0, 8),
])
const textureInput = syntheticTexture2d()
const textureStudio = addon.AssetStudio.fromBuffer(
  textureInput,
  'texture.assets',
  textureInput.length,
)
const decodedTexture = textureStudio.readTexture(0, 7n)
assert.equal(decodedTexture.width, 2)
assert.equal(decodedTexture.height, 2)
assert.deepEqual(Buffer.from(decodedTexture.pixels), DISPLAY_ORDER_PIXELS)

// Model texture encoding is a public Node option, not a PNG-only wrapper over
// Core. This fixture has a real Renderer -> Material -> Texture2D chain so the
// assertion cannot pass merely because the method accepted another argument.
{
  const modelInput = syntheticTexturedModel()
  const modelStudio = addon.AssetStudio.fromBuffer(
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
  const asyncStudio = await addon.AssetStudio.fromBufferAsync(
    input,
    'async-node.assets',
    input.length,
  )
  assert.equal(asyncStudio.fileCount, 1)
  assert.deepEqual(await asyncStudio.readTextAsync(0, 7n), Buffer.from('hello node'))
  assert.deepEqual(await asyncStudio.readRawAsync(0, 7n), studio.readRaw(0, 7n))
  await assert.rejects(asyncStudio.readTextAsync(0, 7n, 9), /limit|exceed/i)
  await assert.rejects(asyncStudio.readShaderAsync(0, 7n), /Shader|class ID/i)
  await assert.rejects(asyncStudio.readMeshObjAsync(0, 7n), /Mesh|class ID/i)
  await assert.rejects(asyncStudio.readTextureAsync(0, 7n), /Texture2D|class ID/i)
  await assert.rejects(asyncStudio.readTypeTreeJsonAsync(0, 7n), /TypeTree|type tree/i)
  await assert.rejects(asyncStudio.readTypeTreeDumpAsync(0, 7n), /TypeTree|type tree/i)
  await assert.rejects(asyncStudio.readTextureArrayAsync(0, 7n), /Texture2DArray|class ID/i)
  await assert.rejects(asyncStudio.readSpriteAsync(0, 7n), /Sprite|class ID/i)

  // The worker path has to agree with the synchronous one on row order.
  const asyncTextureStudio = await addon.AssetStudio.fromBufferAsync(
    textureInput,
    'async-texture.assets',
    textureInput.length,
  )
  const asyncTexture = await asyncTextureStudio.readTextureAsync(0, 7n)
  assert.deepEqual(Buffer.from(asyncTexture.pixels), DISPLAY_ORDER_PIXELS)

  const treeInput = syntheticTypeTreeIntAsset()
  const treeStudio = await addon.AssetStudio.fromBufferAsync(
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

  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'assetstudio-node-'))
  const fixturePath = path.join(directory, 'path.assets')
  try {
    fs.writeFileSync(fixturePath, input)
    const pathStudio = await addon.AssetStudio.openAsync(fixturePath)
    assert.equal(pathStudio.fileCount, 1)
    assert.deepEqual(await pathStudio.readTextAsync(0, 7n), Buffer.from('hello node'))
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
  const scriptDirectory = fs.mkdtempSync(path.join(os.tmpdir(), 'assetstudio-node-script-'))
  try {
  const scriptPath = path.join(scriptDirectory, 'script.assets')
  fs.writeFileSync(scriptPath, syntheticMonoScript())
  const scriptStudio = new addon.AssetStudio(scriptPath)
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

// Font, MovieTexture and VideoClip had no Node binding at all: a caller could
// only reach them through `export`. Checked against what the payload declares
// rather than against bytes this project produced.
{
  const fontStudio = addon.AssetStudio.fromBuffers([
    { name: 'font.assets', data: syntheticFont() },
  ])
  const font = fontStudio.readFont(0, 7n)
  assert.strictEqual(font.name, 'node-font')
  assert.strictEqual(font.extension, '.otf')
  assert.ok(font.data.subarray(0, 4).equals(Buffer.from('OTTO')))

  const movieStudio = addon.AssetStudio.fromBuffers([
    { name: 'movie.assets', data: syntheticMovieTexture() },
  ])
  const movie = movieStudio.readMovieTexture(0, 7n)
  assert.strictEqual(movie.name, 'node-movie')
  assert.ok(movie.data.equals(Buffer.from('OggS')))

  // Reading one kind as another must fail rather than return something.
  assert.throws(() => fontStudio.readMovieTexture(0, 7n))
}

// The Cubism document readers, which had no Node binding: a caller could
// materialize a whole package but not read one behaviour's document.
{
  const cubismStudio = addon.AssetStudio.fromBuffers([
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
  const cubismStudio = addon.AssetStudio.fromBuffers([
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

  assert.throws(() => cubismStudio.readCubismPosePart(1, 11n))
  assert.throws(() => cubismStudio.readCubismDisplayInfo(0, 11n))
  assert.throws(() => cubismStudio.readCubismClipMotion(0, 11n))
  assert.throws(() => cubismStudio.readCubismClipMotion(2, 7n, undefined, false, 1))
}

console.log('node api: additional readers ok')

// PlayerSettings identity and the Unity version override.
{
  const settingsDirectory = fs.mkdtempSync(
    path.join(os.tmpdir(), 'assetstudio-node-settings-'),
  )
  try {
    const settingsPath = path.join(settingsDirectory, 'settings.assets')
    fs.writeFileSync(settingsPath, syntheticPlayerSettings())
    const settingsStudio = new addon.AssetStudio(settingsPath)
    const objects = settingsStudio.objectPage(0)
    assert.equal(objects[0].classId, 129)
    const settings = settingsStudio.readPlayerSettings(0, objects[0].pathId)
    assert.equal(settings.companyName, 'Team Haruki')
    assert.equal(settings.productName, 'unity-rs fixture')

    // A stripped build carries no version, so its layout cannot be decided
    // without one. That is what the override is for.
    const strippedPath = path.join(settingsDirectory, 'stripped.assets')
    fs.writeFileSync(strippedPath, syntheticStrippedPlayerSettings())
    const stripped = new addon.AssetStudio(strippedPath)
    const strippedObjects = stripped.objectPage(0)
    assert.throws(
      () => stripped.readPlayerSettings(0, strippedObjects[0].pathId),
      /version/i,
    )

    const overridden = addon.AssetStudio.openWithVersion(strippedPath, '2022.3.62f1')
    const overriddenSettings = overridden.readPlayerSettings(
      0,
      overridden.objectPage(0)[0].pathId,
    )
    assert.equal(overriddenSettings.companyName, 'Team Haruki')
    assert.throws(() => addon.AssetStudio.openWithVersion(strippedPath, 'not a version'))
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
  const multi = addon.AssetStudio.fromBuffers(inputs)
  assert.equal(multi.fileCount, 2)
  // Each file keeps its own object table.
  assert.equal(multi.objectPage(0).length, 1)
  assert.equal(multi.objectPage(1).length, 1)
  assert.equal(multi.objectPage(1)[0].classId, 115)
  // The total budget is enforced across the inputs, not per input.
  assert.throws(() => addon.AssetStudio.fromBuffers(inputs, 16), /exceeding limit/i)
}

// Scene assembly across the loaded files.
{
  // The text fixture has no GameObject, so the hierarchy is legitimately empty
  // rather than an error.
  const studioForScene = addon.AssetStudio.fromBuffers([
    { name: 'text.assets', data: syntheticTextAsset() },
  ])
  assert.deepEqual(studioForScene.scene(), [])

  const populatedScene = addon.AssetStudio.fromBuffers([
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
  })
  assert.deepEqual(limitedNodes, legacyNodes)
  assert.equal(limitedNodes.length, 1)
  assert.equal(limitedNodes[0].name, 'Node Root')
  assert.throws(
    () => populatedScene.sceneWithLimits({ maximumGameObjects: 0 }),
    /GameObject|limit/i,
  )
}

console.log('node api: multi-buffer, resource range and scene ok')

// Export and extraction, both of which write to disk.
{
  const outputDirectory = fs.mkdtempSync(path.join(os.tmpdir(), 'assetstudio-node-export-'))
  try {
    const exportStudio = addon.AssetStudio.fromBuffers([
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

    // Extraction of a plain file copies it through unchanged.
    const source = path.join(outputDirectory, 'input.assets')
    fs.writeFileSync(source, syntheticTextAsset())
    const extractRoot = path.join(outputDirectory, 'extracted')
    const extraction = addon.AssetStudio.extract(source, extractRoot)
    assert.equal(extraction.failureCount, 0)
    assert.ok(extraction.outputBytes > 0n)
  } finally {
    fs.rmSync(outputDirectory, { recursive: true, force: true })
  }
}

// Static FBX for a collection with no renderable geometry is refused rather
// than emitting an empty scene.
{
  const fbxStudio = addon.AssetStudio.fromBuffers([
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
  const planStudio = addon.AssetStudio.fromBuffers([
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
  const emptyStudio = addon.AssetStudio.fromBuffers([
    { name: 'text.assets', data: syntheticTextAsset() },
  ])
  assert.deepEqual(emptyStudio.live2DPackages(), [])
  // A non-clip object must be refused rather than misread as one.
  const objects = emptyStudio.objectPage(0)
  assert.throws(() => emptyStudio.readAnimationClipInfo(0, objects[0].pathId))
  assert.throws(() => emptyStudio.readAnimatorController(0, objects[0].pathId))
}

console.log('node api: animation and live2d discovery ok')

// Animated FBX and Live2D materialization on a collection that has neither.
{
  const barren = addon.AssetStudio.fromBuffers([
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
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'assetstudio-node-options-'))
  const fixture = path.join(directory, 'options.assets')
  try {
    fs.writeFileSync(fixture, syntheticTextAsset())
    const opened = addon.AssetStudio.openWith(fixture, {
      unityVersion: '2022.3.62f1',
      skipUnreadableInputs: true,
      maximumInputFiles: 8,
    })
    assert.equal(opened.fileCount, 1)
    assert.deepEqual(opened.readText(0, 7n), Buffer.from('hello node'))
    // Omitting the options entirely is the same as opening plainly.
    assert.equal(addon.AssetStudio.openWith(fixture).fileCount, 1)
    // A key is 16 bytes, and a wrong length is refused rather than padded.
    assert.throws(
      () => addon.AssetStudio.openWith(fixture, { unityCnKey: Buffer.alloc(8) }),
      /exactly 16 bytes/,
    )
    assert.throws(
      () => addon.AssetStudio.openWith(fixture, { unityVersion: 'not-a-version' }),
      /unsupported Unity version/,
    )
  } finally {
    fs.rmSync(directory, { recursive: true, force: true })
  }
}

console.log('node api: load options ok')

console.log('node api: animated fbx and live2d materialization ok')

// Textured FBX and ACL inspection on inputs that have neither.
{
  const barren = addon.AssetStudio.fromBuffers([
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
  const schemaStudio = addon.AssetStudio.fromBuffers([
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
  const embedded = addon.AssetStudio.fromBuffers([
    { name: 'expression.assets', data: syntheticCubismExpression() },
  ])
  const behaviour = embedded.objectPage(0)[0]
  const read = embedded.readMonoBehaviourJsonWithSchemas(0, behaviour.pathId, [
    {
      assemblyName: 'Nothing.dll',
      className: 'Nothing',
      nodes: [{ typeName: 'Nothing', fieldName: 'Base', level: 0, align: false }],
    },
  ])
  assert.strictEqual(read.source, 'embedded')
  assert.strictEqual(JSON.parse(read.json.toString('utf8')).m_Name, 'node-expression')
}

console.log('node api: monobehaviour schemas ok')

// Oodle decoder injection. Core ships no Oodle decoder, so a bundle marked
// with it is unreadable until the caller supplies one.
async function testOodleInjection() {
  const oodleDirectory = fs.mkdtempSync(path.join(os.tmpdir(), 'assetstudio-node-oodle-'))
  try {
    const inner = syntheticTextAsset()
    const bundlePath = path.join(oodleDirectory, 'oodle.unity3d')
    fs.writeFileSync(bundlePath, syntheticOodleBundle('CAB-oodle', inner))

    // Without a decoder the bundle is refused rather than silently skipped.
    assert.throws(() => new addon.AssetStudio(bundlePath))

    // With one, it loads. This fixture's block is stored verbatim, so the
    // decoder is the identity -- what matters is that it is called at all and
    // that its bytes are what Core goes on to parse.
    let calls = 0
    const studio = await addon.AssetStudio.openWithOodle(bundlePath, (input, expected) => {
      calls += 1
      assert.equal(input.length, expected)
      return input
    })
    assert.equal(calls, 1)
    assert.equal(studio.fileCount, 1)
    assert.deepEqual(studio.readText(0, 7n), Buffer.from('hello node'))

    // A decoder returning the wrong length is an error, not a short read.
    await assert.rejects(
      addon.AssetStudio.openWithOodle(bundlePath, (input) => input.subarray(0, 4)),
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
  const aclStudio = addon.AssetStudio.fromBuffer(
    aclInput,
    'tuanjie-acl.assets',
    aclInput.length,
  )
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

  const barren = addon.AssetStudio.fromBuffers([
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
