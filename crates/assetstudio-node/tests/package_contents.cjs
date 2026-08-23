"use strict";

const assert = require("node:assert/strict");
const { execFileSync } = require("node:child_process");
const { readFileSync } = require("node:fs");
const { join, resolve } = require("node:path");

const packageRoot = resolve(__dirname, "..");
const repositoryRoot = resolve(packageRoot, "../..");
const result = JSON.parse(
  execFileSync("npm", ["pack", "--json", "--dry-run"], {
    cwd: packageRoot,
    encoding: "utf8",
  }),
);
assert.equal(result.length, 1, result);
const files = new Set(result[0].files.map((file) => file.path));
const forbiddenComponents = new Set([
  "assetstudio-ffi",
  "assetstudio-gui",
  "assetstudiogui",
]);
const forbiddenSuffixes = [".cs", ".csproj", ".fsproj", ".sln", ".slnx", ".vbproj"];
const forbiddenFiles = [...files].filter((file) => {
  const normalized = file.toLowerCase().replaceAll("\\", "/");
  return normalized.split("/").some((part) => forbiddenComponents.has(part))
    || forbiddenSuffixes.some((suffix) => normalized.endsWith(suffix));
});
assert.deepEqual(forbiddenFiles, [], `out-of-scope GUI/C ABI/.NET files: ${forbiddenFiles}`);

for (const legalFile of [
  "LICENSE",
  "THIRD_PARTY_NOTICES.md",
  "THIRD_PARTY_LICENSES.txt",
]) {
  assert(files.has(legalFile), `${legalFile} is missing from the Node package`);
  assert.deepEqual(
    readFileSync(join(packageRoot, legalFile)),
    readFileSync(join(repositoryRoot, legalFile)),
    `${legalFile} has drifted from the repository copy`,
  );
}

const nativeFiles = [...files].filter((file) => file.endsWith(".node"));
const targetSuffixes = {
  "darwin-arm64": "darwin-arm64",
  "darwin-x64": "darwin-x64",
  "linux-arm64": "linux-arm64-gnu",
  "linux-x64": "linux-x64-gnu",
  "win32-arm64": "win32-arm64-msvc",
  "win32-x64": "win32-x64-msvc",
};
const host = `${process.platform}-${process.arch}`;
const targetSuffix = targetSuffixes[host];
assert.ok(targetSuffix, `unsupported Node package-test host: ${host}`);
assert.deepEqual(nativeFiles, [`assetstudio-node.${targetSuffix}.node`]);
for (const required of ["index.js", "index.d.ts", "package.json", "README.md"]) {
  assert(files.has(required), `${required} is missing from the Node package`);
}

console.log(`node package: ${targetSuffix} addon, license and notices ok`);
