"use strict";

const assert = require("node:assert/strict");
const { execFileSync } = require("node:child_process");
const { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } = require("node:fs");
const { tmpdir } = require("node:os");
const { join, resolve } = require("node:path");

const packageRoot = resolve(__dirname, "..");
const temporaryRoot = mkdtempSync(join(tmpdir(), "assetstudio-node-packed-"));
const functionOwnProperties = new Set([
  "arguments",
  "caller",
  "length",
  "name",
  "prototype",
]);

function declaredAssetStudioSurface(source) {
  const lines = source.split(/\r?\n/u);
  const start = lines.indexOf("export declare class AssetStudio {");
  assert.notEqual(start, -1, "installed index.d.ts does not declare AssetStudio");
  const staticMethods = [];
  const instanceMethods = [];
  const getters = [];
  for (const line of lines.slice(start + 1)) {
    if (line === "}") break;
    const match = /^  (?:(static|get) )?([A-Za-z_$][A-Za-z0-9_$]*|constructor)\(/u.exec(line);
    if (match === null) continue;
    const [, kind, name] = match;
    if (kind === "static") staticMethods.push(name);
    else if (kind === "get") getters.push(name);
    else instanceMethods.push(name);
  }
  return {
    staticMethods: staticMethods.sort(),
    instanceMethods: instanceMethods.sort(),
    getters: getters.sort(),
  };
}

function runtimeAssetStudioSurface(assetStudio) {
  const staticMethods = Object.getOwnPropertyNames(assetStudio)
    .filter((name) => !functionOwnProperties.has(name))
    .sort();
  const instanceMethods = [];
  const getters = [];
  for (const name of Object.getOwnPropertyNames(assetStudio.prototype)) {
    const descriptor = Object.getOwnPropertyDescriptor(assetStudio.prototype, name);
    assert.ok(descriptor, `missing runtime descriptor for AssetStudio.${name}`);
    if (typeof descriptor.get === "function") getters.push(name);
    else if (typeof descriptor.value === "function") instanceMethods.push(name);
    else assert.fail(`AssetStudio.${name} is neither a method nor a getter`);
  }
  return {
    staticMethods,
    instanceMethods: instanceMethods.sort(),
    getters: getters.sort(),
  };
}

function assertAssetStudioSurface(assetStudio, declaration, checkCounts = true) {
  const declared = declaredAssetStudioSurface(declaration);
  const runtime = runtimeAssetStudioSurface(assetStudio);
  assert.deepEqual(runtime.staticMethods, declared.staticMethods);
  assert.deepEqual(runtime.instanceMethods, declared.instanceMethods);
  assert.deepEqual(runtime.getters, declared.getters);
  if (checkCounts) {
    assert.equal(declared.staticMethods.length + declared.instanceMethods.length, 84);
    assert.equal(declared.getters.length, 3);
  }
}

try {
  const tarballDirectory = join(temporaryRoot, "tarball");
  const consumerDirectory = join(temporaryRoot, "consumer");
  mkdirSync(tarballDirectory);
  mkdirSync(consumerDirectory);

  const packed = JSON.parse(
    execFileSync(
      "npm",
      ["pack", "--json", "--pack-destination", tarballDirectory],
      { cwd: packageRoot, encoding: "utf8" },
    ),
  );
  assert.equal(packed.length, 1, packed);
  const tarball = join(tarballDirectory, packed[0].filename);

  writeFileSync(
    join(consumerDirectory, "package.json"),
    JSON.stringify({ name: "assetstudio-packed-consumer", private: true }),
  );
  execFileSync(
    "npm",
    [
      "install",
      "--offline",
      "--ignore-scripts",
      "--no-audit",
      "--no-fund",
      "--no-package-lock",
      tarball,
    ],
    { cwd: consumerDirectory, stdio: "pipe" },
  );

  const installedRoot = join(
    consumerDirectory,
    "node_modules",
    "assetstudio-rs-node",
  );
  const installedPackage = JSON.parse(
    readFileSync(join(installedRoot, "package.json"), "utf8"),
  );
  assert.equal(installedPackage.name, "assetstudio-rs-node");
  assert.equal(installedPackage.version, packed[0].version);

  const addon = require(installedRoot);
  assert.deepEqual(Object.keys(addon), ["AssetStudio"]);
  assert.equal(typeof addon.AssetStudio, "function");
  const declaration = readFileSync(join(installedRoot, "index.d.ts"), "utf8");
  assertAssetStudioSurface(addon.AssetStudio, declaration);

  // Prove this is a two-way check, not a count-only smoke. Renaming one method
  // in an otherwise complete declaration must disagree with the loaded class.
  const alteredDeclaration = declaration.replace(
    "  readRaw(fileIndex:",
    "  removedRaw(fileIndex:",
  );
  assert.notEqual(alteredDeclaration, declaration);
  assert.throws(
    () => assertAssetStudioSurface(addon.AssetStudio, alteredDeclaration, false),
    assert.AssertionError,
  );
} finally {
  rmSync(temporaryRoot, { recursive: true, force: true });
}

console.log("installed Node tarball loads the exact AssetStudio runtime surface");
