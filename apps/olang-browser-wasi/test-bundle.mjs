import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";

const bundleDir = process.argv[2];
const expectedStdout = process.argv[3];
if (!bundleDir) {
  throw new Error("usage: node test-bundle.mjs BUNDLE_DIR [EXPECTED_STDOUT_SUBSTRING]");
}

const read = (path) => readFile(resolve(bundleDir, path));
const manifest = JSON.parse(await read("manifest.json"));
const sourceBytes = new Uint8Array(await read(manifest.source.path));
const wasmBytes = new Uint8Array(await read(manifest.artifact.path));
const planBytes = new Uint8Array(await read(manifest.plan.path));
const assetBytes = Object.fromEntries(await Promise.all(
  manifest.assets.map(async (record) => [record.path, new Uint8Array(await read(record.path))]),
));
const adapterBytes = Object.fromEntries(await Promise.all(
  manifest.adapters.map(async (adapter) => [
    adapter.file.path,
    new Uint8Array(await read(adapter.file.path)),
  ]),
));
const {
  OlangBrowserCompatibilityError,
  runOlangBrowserBundle,
} = await import(pathToFileURL(resolve(bundleDir, "runner.mjs")));

if (manifest.compatibility.local_execution) {
  const result = await runOlangBrowserBundle({
    manifest,
    sourceBytes,
    wasmBytes,
    planBytes,
    assetBytes,
    adapterBytes,
  });
  assert.equal(result.executionMode, "browser-local-wasi-preview1");
  assert.equal(result.ok, true, result.stderr);
  if (expectedStdout !== undefined) {
    assert.match(result.stdout, new RegExp(expectedStdout.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")));
  }
  console.log("olang browser bundle local execution: PASS");
} else {
  await assert.rejects(
    runOlangBrowserBundle({
      manifest,
      sourceBytes,
      wasmBytes,
      planBytes,
      assetBytes,
      adapterBytes,
    }),
    (error) => {
      assert(error instanceof OlangBrowserCompatibilityError);
      for (const blocker of manifest.compatibility.blockers) {
        assert.match(error.message, new RegExp(blocker.diagnostic.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")));
      }
      return true;
    },
  );
  console.log("olang browser bundle provider preflight: PASS");
}
