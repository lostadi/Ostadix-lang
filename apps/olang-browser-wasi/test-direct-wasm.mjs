import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { basename } from "node:path";
import { WASI_PREVIEW1_IMPORTS } from "./wasi-preview1-host.mjs";
import {
  BROWSER_BUNDLE_SCHEMA,
  runOlangBrowserBundle,
  WHOLE_PROGRAM_PROVIDER_SCHEMA,
} from "./runner.mjs";

const [sourcePath, wasmPath, expectedStdout] = process.argv.slice(2);
if (!sourcePath || !wasmPath || expectedStdout === undefined) {
  throw new Error(
    "usage: node test-direct-wasm.mjs PROGRAM.O PROGRAM.wasm EXPECTED_STDOUT_SUBSTRING",
  );
}

const sourceBytes = new Uint8Array(await readFile(sourcePath));
const wasmBytes = new Uint8Array(await readFile(wasmPath));

async function sha256(bytes) {
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", bytes));
  return [...digest].map((byte) => byte.toString(16).padStart(2, "0")).join("");
}

async function record(path, bytes) {
  return { path, bytes: bytes.byteLength, sha256: await sha256(bytes) };
}

const empty = new Uint8Array();
const assetBytes = Object.fromEntries([
  "browser-main.mjs",
  "index.html",
  "runner.mjs",
  "wasi-preview1-host.mjs",
].map((path) => [path, empty]));
const adapterBytes = { "adapters/0000.shim": empty };
const manifest = {
  schema: BROWSER_BUNDLE_SCHEMA,
  source: await record("program.O", sourceBytes),
  artifact: await record("program.wasm", wasmBytes),
  assets: await Promise.all([
    record("browser-main.mjs", empty),
    record("index.html", empty),
    record("runner.mjs", empty),
    record("wasi-preview1-host.mjs", empty),
  ]),
  adapters: [{
    name: "fixture_shim.py",
    file: await record("adapters/0000.shim", empty),
  }],
  plan: { path: "program.plan.txt", bytes: 0, sha256: await sha256(empty), nodes: 0 },
  compatibility: {
    local_execution: true,
    class: "browser-local-wasi-preview1",
    blockers: [],
  },
  provider: {
    schema: WHOLE_PROGRAM_PROVIDER_SCHEMA,
    mode: "whole-program",
    required: false,
  },
  backend_grants: [],
  abi: {
    module: "wasi_snapshot_preview1",
    imports: [...WASI_PREVIEW1_IMPORTS],
    required_exports: ["memory", "_start"],
    local_capabilities: [
      "args",
      "environment",
      "clock-realtime",
      "clock-monotonic",
      "crypto-random",
      "stdin-eof",
      "stdout-capture",
      "stderr-capture",
    ],
    denied_capabilities: [
      "filesystem-paths",
      "preopened-directories",
      "process-spawn",
    ],
  },
};

const result = await runOlangBrowserBundle({
  manifest,
  sourceBytes,
  wasmBytes,
  planBytes: empty,
  assetBytes,
  adapterBytes,
});
assert.equal(result.ok, true, result.stderr);
assert.equal(result.executionMode, "browser-local-wasi-preview1");
assert.match(result.stdout, new RegExp(expectedStdout.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")));
console.log(`olang direct browser WASI execution (${basename(wasmPath)}): PASS`);
