import assert from "node:assert/strict";
import {
  WASI_PREVIEW1_IMPORTS,
  WASI_PREVIEW1_SIGNATURES,
  WasiExit,
  WasiPreview1Host,
} from "./wasi-preview1-host.mjs";
import {
  BROWSER_BUNDLE_SCHEMA,
  OlangBrowserBundleError,
  OlangBrowserCompatibilityError,
  runOlangBrowserBundle,
  WHOLE_PROGRAM_PROVIDER_SCHEMA,
} from "./runner.mjs";

const host = new WasiPreview1Host({ args: ["olang"], env: { O_TEST: "1" } });
const memory = new WebAssembly.Memory({ initial: 1 });
host.attach(memory);
const wasi = host.imports.wasi_snapshot_preview1;

assert.deepEqual(Object.keys(wasi).sort(), [...WASI_PREVIEW1_IMPORTS]);
assert.deepEqual(Object.keys(WASI_PREVIEW1_SIGNATURES).sort(), [...WASI_PREVIEW1_IMPORTS]);
assert.equal(wasi.random_get(64, 32), 0);
assert.equal(wasi.args_sizes_get(0, 4), 0);
assert.equal(new DataView(memory.buffer).getUint32(0, true), 1);
assert.equal(wasi.environ_sizes_get(8, 12), 0);
assert.equal(new DataView(memory.buffer).getUint32(8, true), 1);

new Uint8Array(memory.buffer, 256, 4).set(new TextEncoder().encode("pass"));
const view = new DataView(memory.buffer);
view.setUint32(128, 256, true);
view.setUint32(132, 4, true);
assert.equal(wasi.fd_write(1, 128, 1, 140), 0);
assert.equal(host.stdout, "pass");
assert.equal(view.getUint32(140, true), 4);
const euro = new TextEncoder().encode("€");
new Uint8Array(memory.buffer, 300, 2).set(euro.subarray(0, 2));
view.setUint32(160, 300, true);
view.setUint32(164, 2, true);
assert.equal(wasi.fd_write(1, 160, 1, 168), 0);
assert.equal(host.stdout, "pass");
new Uint8Array(memory.buffer, 302, 1).set(euro.subarray(2));
view.setUint32(172, 302, true);
view.setUint32(176, 1, true);
assert.equal(wasi.fd_write(1, 172, 1, 180), 0);
assert.equal(host.stdout, "pass€");
assert.equal(wasi.clock_time_get(0, 0n, 144), 0);
assert(view.getBigUint64(144, true) > 0n);
assert.equal(wasi.path_open(0, 0, 0, 0, 0, 0n, 0n, 0, 0), 76);
assert.throws(() => wasi.proc_exit(7), (error) => error instanceof WasiExit && error.code === 7);

async function sha256(bytes) {
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", bytes));
  return [...digest].map((byte) => byte.toString(16).padStart(2, "0")).join("");
}

async function fileRecord(path, bytes) {
  return { path, bytes: bytes.byteLength, sha256: await sha256(bytes) };
}

const providerSource = "python^(1 + 1)_python";
const providerSourceBytes = new TextEncoder().encode(providerSource);
const emptyWasm = new Uint8Array();
const emptyRecord = async (path) => fileRecord(path, emptyWasm);

const concatBytes = (...parts) => {
  const normalized = parts.map((part) => Uint8Array.from(part));
  const result = new Uint8Array(normalized.reduce((total, part) => total + part.length, 0));
  let offset = 0;
  for (const part of normalized) {
    result.set(part, offset);
    offset += part.length;
  }
  return result;
};
const wasmU32 = (input) => {
  let value = input >>> 0;
  const bytes = [];
  do {
    let byte = value & 0x7f;
    value >>>= 7;
    if (value !== 0) byte |= 0x80;
    bytes.push(byte);
  } while (value !== 0);
  return Uint8Array.from(bytes);
};
const wasmVector = (entries) => concatBytes(wasmU32(entries.length), ...entries);
const wasmName = (value) => {
  const bytes = new TextEncoder().encode(value);
  return concatBytes(wasmU32(bytes.length), bytes);
};
const valueType = Object.freeze({ i32: 0x7f, i64: 0x7e });
const wasmFunctionType = ({ parameters, results }) => concatBytes(
  [0x60],
  wasmVector(parameters.map((type) => [valueType[type]])),
  wasmVector(results.map((type) => [valueType[type]])),
);
const wasmSection = (id, payload) => concatBytes([id], wasmU32(payload.length), payload);

function buildWasiFixture(
  signatureOverrides = {},
  startSignature = { parameters: [], results: [] },
  includeCoreStart = false,
) {
  const signatures = WASI_PREVIEW1_IMPORTS.map(
    (name) => signatureOverrides[name] ?? WASI_PREVIEW1_SIGNATURES[name],
  );
  const uniqueTypes = [];
  const typeIndexes = new Map();
  const typeIndex = (signature) => {
    const key = JSON.stringify(signature);
    if (!typeIndexes.has(key)) {
      typeIndexes.set(key, uniqueTypes.length);
      uniqueTypes.push(signature);
    }
    return typeIndexes.get(key);
  };
  const importTypeIndexes = signatures.map(typeIndex);
  const startType = typeIndex(startSignature);
  const imports = WASI_PREVIEW1_IMPORTS.map((name, index) => concatBytes(
    wasmName("wasi_snapshot_preview1"),
    wasmName(name),
    [0x00],
    wasmU32(importTypeIndexes[index]),
  ));
  const body = Uint8Array.from([0x00, 0x0b]);
  return concatBytes(
    [0x00, 0x61, 0x73, 0x6d, 1, 0, 0, 0],
    wasmSection(1, wasmVector(uniqueTypes.map(wasmFunctionType))),
    wasmSection(2, wasmVector(imports)),
    wasmSection(3, wasmVector([wasmU32(startType)])),
    wasmSection(5, wasmVector([Uint8Array.from([0x00, 0x01])])),
    wasmSection(7, wasmVector([
      concatBytes(wasmName("memory"), [0x02], wasmU32(0)),
      concatBytes(wasmName("_start"), [0x00], wasmU32(imports.length)),
    ])),
    includeCoreStart ? wasmSection(8, wasmU32(imports.length)) : [],
    wasmSection(10, wasmVector([concatBytes(wasmU32(body.length), body)])),
  );
}

const providerWasm = buildWasiFixture();
const assetBytes = Object.fromEntries([
  "browser-main.mjs",
  "index.html",
  "runner.mjs",
  "wasi-preview1-host.mjs",
].map((path) => [path, emptyWasm]));
const adapterPayload = new TextEncoder().encode("# fixture python adapter\n");
const adapterBytes = { "adapters/0000.shim": adapterPayload };
const providerPlanBytes = new TextEncoder().encode(
  "node 0 exec python [env ephemeral] backend=python spec=fixture pure=false "
  + "renderer=Default execution=shim required=[]\n",
);
const localPlanBytes = new TextEncoder().encode("node 0 literal string=fixture\n");
const suppliedClosure = { planBytes: providerPlanBytes, assetBytes, adapterBytes };
const incompatibleManifest = {
  schema: BROWSER_BUNDLE_SCHEMA,
  source: await fileRecord("program.O", providerSourceBytes),
  artifact: await fileRecord("program.wasm", providerWasm),
  assets: await Promise.all([
    emptyRecord("browser-main.mjs"),
    emptyRecord("index.html"),
    emptyRecord("runner.mjs"),
    emptyRecord("wasi-preview1-host.mjs"),
  ]),
  adapters: [{
    name: "python_shim.py",
    file: await fileRecord("adapters/0000.shim", adapterPayload),
  }],
  plan: {
    path: "program.plan.txt",
    bytes: providerPlanBytes.byteLength,
    sha256: await sha256(providerPlanBytes),
    nodes: 1,
  },
  compatibility: {
    local_execution: false,
    class: "requires-whole-program-provider",
    blockers: [{
      plan_node: 0,
      code: "shim-backend",
      operation: "exec",
      backend: "python",
      required_authorities: [],
      diagnostic: "P0 shim-backend backend=python operation=exec",
    }],
  },
  provider: {
    schema: WHOLE_PROGRAM_PROVIDER_SCHEMA,
    mode: "whole-program",
    required: true,
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
await assert.rejects(
  runOlangBrowserBundle({
    manifest: incompatibleManifest,
    wasmBytes: providerWasm,
    source: providerSource,
    ...suppliedClosure,
  }),
  OlangBrowserCompatibilityError,
);
const omittedBlockerManifest = structuredClone(incompatibleManifest);
omittedBlockerManifest.compatibility = {
  local_execution: true,
  class: "browser-local-wasi-preview1",
  blockers: [],
};
omittedBlockerManifest.provider.required = false;
await assert.rejects(
  runOlangBrowserBundle({
    manifest: omittedBlockerManifest,
    wasmBytes: providerWasm,
    source: providerSource,
    ...suppliedClosure,
  }),
  (error) => error instanceof OlangBrowserBundleError
    && error.code === "integrity-failed"
    && /blocker count/.test(error.message),
);
const duplicatedBlockerManifest = structuredClone(incompatibleManifest);
duplicatedBlockerManifest.compatibility.blockers.push(
  structuredClone(duplicatedBlockerManifest.compatibility.blockers[0]),
);
await assert.rejects(
  runOlangBrowserBundle({
    manifest: duplicatedBlockerManifest,
    wasmBytes: providerWasm,
    source: providerSource,
    ...suppliedClosure,
  }),
  (error) => error instanceof OlangBrowserBundleError
    && error.code === "manifest-invalid"
    && /duplicated/.test(error.message),
);
await assert.rejects(
  runOlangBrowserBundle({
    manifest: incompatibleManifest,
    wasmBytes: new Uint8Array([1]),
    source: providerSource,
    ...suppliedClosure,
  }),
  (error) => error instanceof OlangBrowserBundleError && error.code === "integrity-failed",
);
const noImportsWasm = new Uint8Array([
  0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00,
  0x01, 0x04, 0x01, 0x60, 0x00, 0x00,
  0x03, 0x02, 0x01, 0x00,
  0x05, 0x03, 0x01, 0x00, 0x01,
  0x07, 0x13, 0x02,
  0x06, 0x6d, 0x65, 0x6d, 0x6f, 0x72, 0x79, 0x02, 0x00,
  0x06, 0x5f, 0x73, 0x74, 0x61, 0x72, 0x74, 0x00, 0x00,
  0x0a, 0x04, 0x01, 0x02, 0x00, 0x0b,
]);
const abiMismatchManifest = structuredClone(incompatibleManifest);
abiMismatchManifest.artifact = await fileRecord("program.wasm", noImportsWasm);
abiMismatchManifest.compatibility = {
  local_execution: true,
  class: "browser-local-wasi-preview1",
  blockers: [],
};
abiMismatchManifest.provider.required = false;
abiMismatchManifest.plan = {
  path: "program.plan.txt",
  bytes: localPlanBytes.byteLength,
  sha256: await sha256(localPlanBytes),
  nodes: 1,
};
await assert.rejects(
  runOlangBrowserBundle({
    manifest: abiMismatchManifest,
    wasmBytes: noImportsWasm,
    source: providerSource,
    planBytes: localPlanBytes,
    assetBytes,
    adapterBytes,
  }),
  (error) => error instanceof OlangBrowserBundleError && error.code === "abi-mismatch",
);
const providerResult = await runOlangBrowserBundle({
  manifest: incompatibleManifest,
  wasmBytes: providerWasm,
  source: providerSource,
  ...suppliedClosure,
  provider: {
    schema: WHOLE_PROGRAM_PROVIDER_SCHEMA,
    async executeProgram(request) {
      assert.match(request.source, /python/);
      assert.equal(request.adapters.length, 1);
      assert.equal(request.adapters[0].name, "python_shim.py");
      assert.deepEqual(request.adapters[0].bytes, adapterPayload);
      return { ok: true, stdout: "provider-pass", stderr: "", exitCode: 0 };
    },
  },
});
assert.equal(providerResult.executionMode, "whole-program-provider");
assert.equal(providerResult.stdout, "provider-pass");

const wrongSignatureWasm = buildWasiFixture({
  fd_write: { parameters: ["i32", "i32", "i32"], results: ["i32"] },
});
const wrongSignatureManifest = structuredClone(incompatibleManifest);
wrongSignatureManifest.artifact = await fileRecord("program.wasm", wrongSignatureWasm);
await assert.rejects(
  runOlangBrowserBundle({
    manifest: wrongSignatureManifest,
    wasmBytes: wrongSignatureWasm,
    source: providerSource,
    ...suppliedClosure,
    provider: {
      schema: WHOLE_PROGRAM_PROVIDER_SCHEMA,
      async executeProgram() {
        throw new Error("provider must not run after an ABI mismatch");
      },
    },
  }),
  (error) => error instanceof OlangBrowserBundleError
    && error.code === "abi-mismatch"
    && /fd_write/.test(error.message),
);

const wrongStartWasm = buildWasiFixture({}, { parameters: ["i32"], results: [] });
const wrongStartManifest = structuredClone(incompatibleManifest);
wrongStartManifest.artifact = await fileRecord("program.wasm", wrongStartWasm);
await assert.rejects(
  runOlangBrowserBundle({
    manifest: wrongStartManifest,
    wasmBytes: wrongStartWasm,
    source: providerSource,
    ...suppliedClosure,
    provider: {
      schema: WHOLE_PROGRAM_PROVIDER_SCHEMA,
      async executeProgram() {
        throw new Error("provider must not run after a _start ABI mismatch");
      },
    },
  }),
  (error) => error instanceof OlangBrowserBundleError
    && error.code === "abi-mismatch"
    && /_start/.test(error.message),
);

const coreStartWasm = buildWasiFixture({}, { parameters: [], results: [] }, true);
const coreStartManifest = structuredClone(incompatibleManifest);
coreStartManifest.artifact = await fileRecord("program.wasm", coreStartWasm);
await assert.rejects(
  runOlangBrowserBundle({
    manifest: coreStartManifest,
    wasmBytes: coreStartWasm,
    source: providerSource,
    ...suppliedClosure,
    provider: {
      schema: WHOLE_PROGRAM_PROVIDER_SCHEMA,
      async executeProgram() {
        throw new Error("provider must not run for a core Start module");
      },
    },
  }),
  (error) => error instanceof OlangBrowserBundleError
    && error.code === "abi-mismatch"
    && /Start sections are forbidden/.test(error.message),
);

const tamperedAssets = { ...assetBytes, "runner.mjs": new Uint8Array([1]) };
await assert.rejects(
  runOlangBrowserBundle({
    manifest: incompatibleManifest,
    wasmBytes: providerWasm,
    source: providerSource,
    planBytes: providerPlanBytes,
    assetBytes: tamperedAssets,
    adapterBytes,
  }),
  (error) => error instanceof OlangBrowserBundleError && error.code === "integrity-failed",
);
await assert.rejects(
  runOlangBrowserBundle({
    manifest: incompatibleManifest,
    wasmBytes: providerWasm,
    source: providerSource,
    planBytes: providerPlanBytes,
    assetBytes,
    adapterBytes: { "adapters/0000.shim": new Uint8Array([1]) },
  }),
  (error) => error instanceof OlangBrowserBundleError && error.code === "integrity-failed",
);

const loneSurrogate = "\ud800";
const invalidSourceManifest = structuredClone(incompatibleManifest);
invalidSourceManifest.source = await fileRecord(
  "program.O",
  new TextEncoder().encode(loneSurrogate),
);
await assert.rejects(
  runOlangBrowserBundle({
    manifest: invalidSourceManifest,
    wasmBytes: providerWasm,
    source: loneSurrogate,
    ...suppliedClosure,
  }),
  (error) => error instanceof OlangBrowserBundleError && error.code === "source-invalid",
);

await assert.rejects(
  runOlangBrowserBundle({
    manifest: incompatibleManifest,
    wasmBytes: providerWasm,
    source: providerSource,
    ...suppliedClosure,
    provider: {
      schema: WHOLE_PROGRAM_PROVIDER_SCHEMA,
      async executeProgram() {
        return { ok: true, stdout: "missing-exit-code", stderr: "" };
      },
    },
  }),
  /consistent \{ok, exitCode, stdout, stderr\}/,
);

const mutableManifest = structuredClone(incompatibleManifest);
const mutableSourceBytes = Uint8Array.from(providerSourceBytes);
const mutableWasmBytes = Uint8Array.from(providerWasm);
const immutableRun = runOlangBrowserBundle({
  manifest: mutableManifest,
  sourceBytes: mutableSourceBytes,
  wasmBytes: mutableWasmBytes,
  ...suppliedClosure,
});
mutableManifest.compatibility.local_execution = true;
mutableManifest.compatibility.class = "browser-local-wasi-preview1";
mutableManifest.compatibility.blockers = [];
mutableSourceBytes.fill(0);
mutableWasmBytes.fill(0);
await assert.rejects(immutableRun, OlangBrowserCompatibilityError);

const bomSourceBytes = concatBytes([0xef, 0xbb, 0xbf], providerSourceBytes);
const bomManifest = structuredClone(incompatibleManifest);
bomManifest.source = await fileRecord("program.O", bomSourceBytes);
let providerSawBom = false;
const bomResult = await runOlangBrowserBundle({
  manifest: bomManifest,
  sourceBytes: bomSourceBytes,
  wasmBytes: providerWasm,
  ...suppliedClosure,
  provider: {
    schema: WHOLE_PROGRAM_PROVIDER_SCHEMA,
    async executeProgram(request) {
      providerSawBom = request.source.codePointAt(0) === 0xfeff;
      return { ok: true, exitCode: 0, stdout: "bom-pass", stderr: "" };
    },
  },
});
assert.equal(providerSawBom, true);
assert.equal(bomResult.stdout, "bom-pass");

console.log("olang-browser-wasi host tests: PASS");
