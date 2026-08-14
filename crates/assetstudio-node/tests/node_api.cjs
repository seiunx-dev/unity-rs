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

// A 2x2 RGBA32 Texture2D. Rows are stored bottom-up, so the first pixel in the
// payload is the BOTTOM-left one and a correct reader returns it last.
const TEXTURE_PIXELS = Buffer.from([
  255, 0, 0, 1, 0, 255, 0, 2, 0, 0, 255, 3, 255, 255, 255, 4,
])

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
}

console.log('node api: multi-buffer, resource range and scene ok')
