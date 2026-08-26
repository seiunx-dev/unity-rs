"use strict";

const assert = require("node:assert/strict");
const { execFileSync } = require("node:child_process");
const { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } = require("node:fs");
const { tmpdir } = require("node:os");
const { join, resolve } = require("node:path");

const packageRoot = resolve(__dirname, "..");
const functionOwnProperties = new Set([
  "arguments",
  "caller",
  "length",
  "name",
  "prototype",
]);

function declaredUnityRsSurface(source) {
  const lines = source.split(/\r?\n/u);
  const start = lines.indexOf("export declare class UnityRs {");
  assert.notEqual(start, -1, "installed index.d.ts does not declare UnityRs");
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

function runtimeUnityRsSurface(unityRs) {
  const staticMethods = Object.getOwnPropertyNames(unityRs)
    .filter((name) => !functionOwnProperties.has(name))
    .sort();
  const instanceMethods = [];
  const getters = [];
  for (const name of Object.getOwnPropertyNames(unityRs.prototype)) {
    const descriptor = Object.getOwnPropertyDescriptor(unityRs.prototype, name);
    assert.ok(descriptor, `missing runtime descriptor for UnityRs.${name}`);
    if (typeof descriptor.get === "function") getters.push(name);
    else if (typeof descriptor.value === "function") instanceMethods.push(name);
    else assert.fail(`UnityRs.${name} is neither a method nor a getter`);
  }
  return {
    staticMethods,
    instanceMethods: instanceMethods.sort(),
    getters: getters.sort(),
  };
}

function assertUnityRsSurface(unityRs, declaration, checkCounts = true) {
  const declared = declaredUnityRsSurface(declaration);
  const runtime = runtimeUnityRsSurface(unityRs);
  assert.deepEqual(runtime.staticMethods, declared.staticMethods);
  assert.deepEqual(runtime.instanceMethods, declared.instanceMethods);
  assert.deepEqual(runtime.getters, declared.getters);
  if (checkCounts) {
    assert.equal(declared.staticMethods.length + declared.instanceMethods.length, 85);
    assert.equal(declared.getters.length, 4);
  }
}

function verifyInstalledPackage(installedRoot, expectedVersion) {
  const installedPackage = JSON.parse(
    readFileSync(join(installedRoot, "package.json"), "utf8"),
  );
  assert.equal(installedPackage.name, "unity-rs-node");
  assert.equal(installedPackage.version, expectedVersion);

  const addon = require(installedRoot);
  assert.deepEqual(Object.keys(addon), ["UnityRs"]);
  assert.equal(typeof addon.UnityRs, "function");
  const declaration = readFileSync(join(installedRoot, "index.d.ts"), "utf8");
  assertUnityRsSurface(addon.UnityRs, declaration);

  // Prove this is a two-way check, not a count-only smoke. Renaming one method
  // in an otherwise complete declaration must disagree with the loaded class.
  const alteredDeclaration = declaration.replace(
    "  readRaw(fileIndex:",
    "  removedRaw(fileIndex:",
  );
  assert.notEqual(alteredDeclaration, declaration);
  assert.throws(
    () => assertUnityRsSurface(addon.UnityRs, alteredDeclaration, false),
    assert.AssertionError,
  );
}

if (process.argv[2] === "--verify-installed") {
  verifyInstalledPackage(process.argv[3], process.argv[4]);
} else {
  const temporaryRoot = mkdtempSync(join(tmpdir(), "unity-rs-node-packed-"));
  const npmCli = process.env.npm_execpath;
  assert.ok(npmCli, "npm_execpath is missing; run this check through npm test:package");
  try {
    const tarballDirectory = join(temporaryRoot, "tarball");
    const consumerDirectory = join(temporaryRoot, "consumer");
    mkdirSync(tarballDirectory);
    mkdirSync(consumerDirectory);

    const packed = JSON.parse(
      execFileSync(
        process.execPath,
        [npmCli, "pack", "--json", "--pack-destination", tarballDirectory],
        { cwd: packageRoot, encoding: "utf8" },
      ),
    );
    assert.equal(packed.length, 1, packed);
    const tarball = join(tarballDirectory, packed[0].filename);

    writeFileSync(
      join(consumerDirectory, "package.json"),
      JSON.stringify({ name: "unity-rs-packed-consumer", private: true }),
    );
    execFileSync(
      process.execPath,
      [
        npmCli,
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
      "unity-rs-node",
    );
    // Load the native addon in a child process. Windows keeps a loaded `.node`
    // DLL locked until its process exits, so loading it here would make the
    // package cleanup fail even though every package assertion had succeeded.
    execFileSync(
      process.execPath,
      [__filename, "--verify-installed", installedRoot, packed[0].version],
      { encoding: "utf8", stdio: "pipe" },
    );
  } finally {
    rmSync(temporaryRoot, { recursive: true, force: true });
  }

  console.log("installed Node tarball loads the exact UnityRs runtime surface");
}
