import {
  WASI_PREVIEW1_IMPORTS,
  WASI_PREVIEW1_SIGNATURES,
  WasiHostError,
  WasiPreview1Host,
} from "./wasi-preview1-host.mjs";

export const BROWSER_BUNDLE_SCHEMA = "ostadix.olang-browser-bundle/v1";
export const WHOLE_PROGRAM_PROVIDER_SCHEMA = "ostadix.olang-browser-provider/v1";

const EXPECTED_ASSETS = Object.freeze([
  "browser-main.mjs",
  "index.html",
  "runner.mjs",
  "wasi-preview1-host.mjs",
]);
const EXPECTED_LOCAL_CAPABILITIES = Object.freeze([
  "args",
  "environment",
  "clock-realtime",
  "clock-monotonic",
  "crypto-random",
  "stdin-eof",
  "stdout-capture",
  "stderr-capture",
]);
const EXPECTED_DENIED_CAPABILITIES = Object.freeze([
  "filesystem-paths",
  "preopened-directories",
  "process-spawn",
]);

export class OlangBrowserBundleError extends Error {
  constructor(code, message, cause) {
    super(message, cause === undefined ? undefined : { cause });
    this.name = "OlangBrowserBundleError";
    this.code = code;
  }
}

export class OlangBrowserCompatibilityError extends Error {
  constructor(blockers) {
    const diagnostic = blockers.map((blocker) => blocker.diagnostic).join("; ");
    super(`browser-local execution requires an explicit whole-program provider: ${diagnostic}`);
    this.name = "OlangBrowserCompatibilityError";
    this.code = "provider-required";
    this.blockers = blockers;
  }
}

export class OlangBrowserProviderError extends Error {
  constructor(message, cause) {
    super(message, cause === undefined ? undefined : { cause });
    this.name = "OlangBrowserProviderError";
    this.code = "provider-failed";
  }
}

function validateManifest(manifest) {
  const invalid = (message) => {
    throw new OlangBrowserBundleError("manifest-invalid", message);
  };
  const exactKeys = (value, keys, subject) => {
    if (!value || typeof value !== "object" || Array.isArray(value)) {
      invalid(`${subject} must be an object`);
    }
    const actual = Object.keys(value).sort();
    const expected = [...keys].sort();
    if (actual.length !== expected.length || actual.some((key, index) => key !== expected[index])) {
      invalid(`${subject} has non-canonical fields`);
    }
  };
  const exactArray = (actual, expected, subject) => {
    if (
      !Array.isArray(actual)
      || actual.length !== expected.length
      || actual.some((value, index) => value !== expected[index])
    ) {
      invalid(`${subject} does not match the canonical browser contract`);
    }
  };
  const fileRecord = (record, path, subject, extraKeys = []) => {
    exactKeys(record, ["path", "bytes", "sha256", ...extraKeys], subject);
    if (record.path !== path) invalid(`${subject}.path must be ${path}`);
    if (!Number.isSafeInteger(record.bytes) || record.bytes < 0) {
      invalid(`${subject}.bytes must be a non-negative safe integer`);
    }
    if (typeof record.sha256 !== "string" || !/^[0-9a-f]{64}$/.test(record.sha256)) {
      invalid(`${subject}.sha256 must be a lowercase SHA-256 digest`);
    }
  };

  exactKeys(
    manifest,
    [
      "schema",
      "source",
      "artifact",
      "assets",
      "adapters",
      "plan",
      "compatibility",
      "provider",
      "backend_grants",
      "abi",
    ],
    "browser bundle manifest",
  );
  if (manifest.schema !== BROWSER_BUNDLE_SCHEMA) {
    invalid(`browser bundle manifest schema must be ${BROWSER_BUNDLE_SCHEMA}`);
  }
  fileRecord(manifest.source, "program.O", "manifest.source");
  fileRecord(manifest.artifact, "program.wasm", "manifest.artifact");

  if (!Array.isArray(manifest.assets) || manifest.assets.length !== EXPECTED_ASSETS.length) {
    invalid("manifest.assets must list the four canonical browser assets");
  }
  manifest.assets.forEach((record, index) => {
    fileRecord(record, EXPECTED_ASSETS[index], `manifest.assets[${index}]`);
  });

  if (!Array.isArray(manifest.adapters) || manifest.adapters.length === 0) {
    invalid("manifest.adapters must list at least one selected compatibility adapter");
  }
  const adapterNames = new Set();
  manifest.adapters.forEach((adapter, index) => {
    const subject = `manifest.adapters[${index}]`;
    exactKeys(adapter, ["name", "file"], subject);
    if (
      typeof adapter.name !== "string"
      || adapter.name.length === 0
      || adapter.name.includes("\0")
      || adapterNames.has(adapter.name)
    ) {
      invalid(`${subject}.name must be a unique non-empty string without NUL`);
    }
    adapterNames.add(adapter.name);
    fileRecord(
      adapter.file,
      `adapters/${String(index).padStart(4, "0")}.shim`,
      `${subject}.file`,
    );
  });

  fileRecord(manifest.plan, "program.plan.txt", "manifest.plan", ["nodes"]);
  if (!Number.isSafeInteger(manifest.plan.nodes) || manifest.plan.nodes < 0) {
    invalid("manifest.plan.nodes must be a non-negative safe integer");
  }

  exactKeys(
    manifest.compatibility,
    ["local_execution", "class", "blockers"],
    "manifest.compatibility",
  );
  if (typeof manifest.compatibility.local_execution !== "boolean") {
    invalid("manifest.compatibility.local_execution must be boolean");
  }
  if (!Array.isArray(manifest.compatibility.blockers)) {
    invalid("manifest.compatibility.blockers must be an array");
  }
  const blockerNodes = new Set();
  for (const [index, blocker] of manifest.compatibility.blockers.entries()) {
    const keys = ["plan_node", "code", "operation", "required_authorities", "diagnostic"];
    if (blocker?.backend !== undefined) keys.push("backend");
    exactKeys(blocker, keys, `manifest.compatibility.blockers[${index}]`);
    if (!Number.isSafeInteger(blocker.plan_node) || blocker.plan_node < 0) {
      invalid(`manifest.compatibility.blockers[${index}].plan_node must be non-negative`);
    }
    if (blocker.plan_node >= manifest.plan.nodes) {
      invalid(`manifest.compatibility.blockers[${index}].plan_node is out of bounds`);
    }
    if (blockerNodes.has(blocker.plan_node)) {
      invalid(`manifest.compatibility.blockers[${index}].plan_node is duplicated`);
    }
    blockerNodes.add(blocker.plan_node);
    if (!["shim-backend", "effectful-request"].includes(blocker.code)) {
      invalid(`manifest.compatibility.blockers[${index}].code is unsupported`);
    }
    if (typeof blocker.operation !== "string" || blocker.operation.length === 0) {
      invalid(`manifest.compatibility.blockers[${index}].operation must be non-empty`);
    }
    if (
      !Array.isArray(blocker.required_authorities)
      || blocker.required_authorities.some((authority) => typeof authority !== "string")
    ) {
      invalid(`manifest.compatibility.blockers[${index}].required_authorities is invalid`);
    }
    if (blocker.code === "shim-backend") {
      if (blocker.operation !== "exec" || typeof blocker.backend !== "string" || blocker.backend.length === 0) {
        invalid(`manifest.compatibility.blockers[${index}] has an invalid shim shape`);
      }
    } else if (blocker.backend !== undefined || blocker.required_authorities.length !== 0) {
      invalid(`manifest.compatibility.blockers[${index}] has an invalid request shape`);
    }
    const subject = blocker.backend
      ? `backend=${blocker.backend} operation=${blocker.operation}`
      : `operation=${blocker.operation}`;
    const expectedDiagnostic = `P${blocker.plan_node} ${blocker.code} ${subject}`;
    if (blocker.diagnostic !== expectedDiagnostic) {
      invalid(`manifest.compatibility.blockers[${index}].diagnostic is not canonical`);
    }
  }
  const local = manifest.compatibility.blockers.length === 0;
  if (manifest.compatibility.local_execution !== local) {
    invalid("manifest compatibility contradicts its blocker set");
  }
  const expectedClass = local
    ? "browser-local-wasi-preview1"
    : "requires-whole-program-provider";
  if (manifest.compatibility.class !== expectedClass) {
    invalid(`manifest.compatibility.class must be ${expectedClass}`);
  }

  exactKeys(manifest.provider, ["schema", "mode", "required"], "manifest.provider");
  if (
    manifest.provider.schema !== WHOLE_PROGRAM_PROVIDER_SCHEMA
    || manifest.provider.mode !== "whole-program"
    || manifest.provider.required !== !local
  ) {
    invalid("manifest.provider does not match the whole-program provider contract");
  }
  if (!Array.isArray(manifest.backend_grants) || manifest.backend_grants.some(
    (grant) => typeof grant !== "string",
  )) {
    invalid("manifest.backend_grants must be an array of strings");
  }

  exactKeys(
    manifest.abi,
    ["module", "imports", "required_exports", "local_capabilities", "denied_capabilities"],
    "manifest.abi",
  );
  if (manifest.abi.module !== "wasi_snapshot_preview1") {
    invalid("manifest.abi.module must be wasi_snapshot_preview1");
  }
  exactArray(manifest.abi.imports, WASI_PREVIEW1_IMPORTS, "manifest.abi.imports");
  exactArray(
    manifest.abi.required_exports,
    ["memory", "_start"],
    "manifest.abi.required_exports",
  );
  exactArray(
    manifest.abi.local_capabilities,
    EXPECTED_LOCAL_CAPABILITIES,
    "manifest.abi.local_capabilities",
  );
  exactArray(
    manifest.abi.denied_capabilities,
    EXPECTED_DENIED_CAPABILITIES,
    "manifest.abi.denied_capabilities",
  );
  return manifest;
}

async function fetchBytes(url) {
  const response = await fetch(url);
  if (!response.ok) {
    throw new OlangBrowserBundleError(
      "fetch-failed",
      `failed to fetch ${url}: HTTP ${response.status}`,
    );
  }
  return new Uint8Array(await response.arrayBuffer());
}

async function fetchManifest(url) {
  const response = await fetch(url);
  if (!response.ok) {
    throw new OlangBrowserBundleError(
      "fetch-failed",
      `failed to fetch ${url}: HTTP ${response.status}`,
    );
  }
  try {
    return await response.json();
  } catch (error) {
    throw new OlangBrowserBundleError(
      "manifest-invalid",
      `failed to parse browser bundle manifest: ${error?.message ?? String(error)}`,
      error,
    );
  }
}

async function sha256Hex(bytes) {
  const subtle = globalThis.crypto?.subtle;
  if (!subtle || typeof subtle.digest !== "function") {
    throw new OlangBrowserBundleError(
      "crypto-unavailable",
      "Web Crypto SHA-256 is required to verify the browser bundle",
    );
  }
  const digest = new Uint8Array(await subtle.digest("SHA-256", bytes));
  return [...digest].map((byte) => byte.toString(16).padStart(2, "0")).join("");
}

async function verifyFile(record, bytes, subject) {
  if (bytes.byteLength !== record.bytes) {
    throw new OlangBrowserBundleError(
      "integrity-failed",
      `${subject} size mismatch: manifest=${record.bytes} actual=${bytes.byteLength}`,
    );
  }
  const actual = await sha256Hex(bytes);
  if (actual !== record.sha256) {
    throw new OlangBrowserBundleError(
      "integrity-failed",
      `${subject} SHA-256 mismatch: manifest=${record.sha256} actual=${actual}`,
    );
  }
}

function bytesFrom(value, subject) {
  try {
    if (value instanceof Uint8Array) {
      return Uint8Array.from(value);
    }
    if (value instanceof ArrayBuffer) {
      return new Uint8Array(value.slice(0));
    }
    if (ArrayBuffer.isView(value)) {
      return Uint8Array.from(
        new Uint8Array(value.buffer, value.byteOffset, value.byteLength),
      );
    }
  } catch (error) {
    throw new OlangBrowserBundleError(
      "invalid-options",
      `${subject} must be an ArrayBuffer or typed-array view`,
      error,
    );
  }
  throw new OlangBrowserBundleError(
    "invalid-options",
    `${subject} must be an ArrayBuffer or typed-array view`,
  );
}

function immutableManifest(value) {
  let snapshot;
  try {
    snapshot = structuredClone(value);
  } catch (error) {
    throw new OlangBrowserBundleError(
      "manifest-invalid",
      `browser bundle manifest cannot be snapshotted: ${error?.message ?? String(error)}`,
      error,
    );
  }
  const freeze = (item) => {
    if (item && typeof item === "object" && !Object.isFrozen(item)) {
      for (const child of Object.values(item)) freeze(child);
      Object.freeze(item);
    }
    return item;
  };
  return freeze(validateManifest(snapshot));
}

function suppliedNamedBytes(payloads, optionName, path) {
  if (payloads instanceof Map) {
    return payloads.has(path) ? bytesFrom(payloads.get(path), `${optionName}[${path}]`) : null;
  }
  if (payloads && typeof payloads === "object" && !Array.isArray(payloads)) {
    return Object.hasOwn(payloads, path)
      ? bytesFrom(payloads[path], `${optionName}[${path}]`)
      : null;
  }
  throw new OlangBrowserBundleError(
    "invalid-options",
    `${optionName} must be a Map or object containing every declared file`,
  );
}

function decodeUtf8(bytes, subject) {
  try {
    return new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(bytes);
  } catch (error) {
    throw new OlangBrowserBundleError(
      "source-invalid",
      `${subject} must contain well-formed UTF-8`,
      error,
    );
  }
}

const WASM_VALUE_TYPES = Object.freeze({
  0x7f: "i32",
  0x7e: "i64",
  0x7d: "f32",
  0x7c: "f64",
  0x7b: "v128",
  0x70: "funcref",
  0x6f: "externref",
});

class WasmBinaryReader {
  constructor(bytes) {
    this.bytes = bytes;
    this.offset = 0;
  }

  get remaining() {
    return this.bytes.byteLength - this.offset;
  }

  readByte() {
    if (this.remaining < 1) throw new RangeError("unexpected end of WebAssembly binary");
    return this.bytes[this.offset++];
  }

  readU32() {
    let value = 0;
    for (let index = 0; index < 5; index += 1) {
      const byte = this.readByte();
      if (index === 4 && (byte & 0xf0) !== 0) {
        throw new RangeError("invalid WebAssembly u32 LEB128 value");
      }
      value += (byte & 0x7f) * (2 ** (index * 7));
      if ((byte & 0x80) === 0) return value;
    }
    throw new RangeError("unterminated WebAssembly u32 LEB128 value");
  }

  readBytes(length) {
    if (!Number.isSafeInteger(length) || length < 0 || length > this.remaining) {
      throw new RangeError("WebAssembly byte range is out of bounds");
    }
    const bytes = this.bytes.subarray(this.offset, this.offset + length);
    this.offset += length;
    return bytes;
  }

  readName() {
    return new TextDecoder("utf-8", { fatal: true, ignoreBOM: true })
      .decode(this.readBytes(this.readU32()));
  }

  requireEnd(subject) {
    if (this.remaining !== 0) {
      throw new RangeError(`${subject} contains trailing bytes`);
    }
  }
}

function readWasmValueVector(reader) {
  const count = reader.readU32();
  const values = [];
  for (let index = 0; index < count; index += 1) {
    const code = reader.readByte();
    values.push(WASM_VALUE_TYPES[code] ?? `0x${code.toString(16)}`);
  }
  return values;
}

function parseWasmFunctionSignatures(wasmBytes) {
  const reader = new WasmBinaryReader(wasmBytes);
  const header = reader.readBytes(8);
  if (
    header.some((byte, index) => byte !== [0x00, 0x61, 0x73, 0x6d, 1, 0, 0, 0][index])
  ) {
    throw new RangeError("invalid WebAssembly header");
  }
  const types = [];
  const imports = [];
  const functionTypeIndexes = [];
  const functionExports = new Map();
  while (reader.remaining > 0) {
    const sectionId = reader.readByte();
    const payload = new WasmBinaryReader(reader.readBytes(reader.readU32()));
    if (sectionId === 1) {
      const count = payload.readU32();
      for (let index = 0; index < count; index += 1) {
        if (payload.readByte() !== 0x60) {
          throw new RangeError("unsupported WebAssembly function type form");
        }
        types.push({
          parameters: readWasmValueVector(payload),
          results: readWasmValueVector(payload),
        });
      }
      payload.requireEnd("WebAssembly type section");
    } else if (sectionId === 2) {
      const count = payload.readU32();
      for (let index = 0; index < count; index += 1) {
        const module = payload.readName();
        const name = payload.readName();
        const kind = payload.readByte();
        if (kind !== 0) {
          throw new RangeError(`non-function import ${module}:${name}`);
        }
        const typeIndex = payload.readU32();
        if (!types[typeIndex]) {
          throw new RangeError(`missing function type ${typeIndex} for ${module}:${name}`);
        }
        imports.push({ module, name, ...types[typeIndex] });
        functionTypeIndexes.push(typeIndex);
      }
      payload.requireEnd("WebAssembly import section");
    } else if (sectionId === 3) {
      const count = payload.readU32();
      for (let index = 0; index < count; index += 1) {
        functionTypeIndexes.push(payload.readU32());
      }
      payload.requireEnd("WebAssembly function section");
    } else if (sectionId === 7) {
      const count = payload.readU32();
      for (let index = 0; index < count; index += 1) {
        const name = payload.readName();
        const kind = payload.readByte();
        const itemIndex = payload.readU32();
        if (kind === 0) functionExports.set(name, itemIndex);
      }
      payload.requireEnd("WebAssembly export section");
    } else if (sectionId === 8) {
      throw new RangeError(
        "WebAssembly core Start sections are forbidden; invoke the exported WASI _start instead",
      );
    }
  }
  const exportedFunctions = new Map();
  for (const [name, functionIndex] of functionExports) {
    const typeIndex = functionTypeIndexes[functionIndex];
    if (!types[typeIndex]) {
      throw new RangeError(`missing exported function type for ${name}`);
    }
    exportedFunctions.set(name, types[typeIndex]);
  }
  return { imports, exportedFunctions };
}

function sameSignature(actual, expected) {
  return actual.parameters.length === expected.parameters.length
    && actual.parameters.every((value, index) => value === expected.parameters[index])
    && actual.results.length === expected.results.length
    && actual.results.every((value, index) => value === expected.results[index]);
}

async function compileAndVerifyModule(wasmBytes, abi) {
  let module;
  try {
    module = await WebAssembly.compile(wasmBytes);
  } catch (error) {
    throw new OlangBrowserBundleError(
      "abi-mismatch",
      `program.wasm is not a valid WebAssembly module: ${error?.message ?? String(error)}`,
      error,
    );
  }

  const imports = WebAssembly.Module.imports(module);
  const actualImports = imports
    .map((entry) => `${entry.module}:${entry.name}:${entry.kind}`)
    .sort();
  const expectedImports = abi.imports
    .map((name) => `${abi.module}:${name}:function`)
    .sort();
  if (
    actualImports.length !== expectedImports.length
    || actualImports.some((entry, index) => entry !== expectedImports[index])
  ) {
    throw new OlangBrowserBundleError(
      "abi-mismatch",
      `program.wasm imports do not match the manifest WASI contract: expected=${expectedImports.join(",")} actual=${actualImports.join(",")}`,
    );
  }

  let typedModule;
  try {
    typedModule = parseWasmFunctionSignatures(wasmBytes);
  } catch (error) {
    throw new OlangBrowserBundleError(
      "abi-mismatch",
      `cannot inspect program.wasm import signatures: ${error?.message ?? String(error)}`,
      error,
    );
  }
  for (const entry of typedModule.imports) {
    const expected = entry.module === abi.module
      ? WASI_PREVIEW1_SIGNATURES[entry.name]
      : undefined;
    if (!expected || !sameSignature(entry, expected)) {
      const actualText = `(${entry.parameters.join(",")}) -> (${entry.results.join(",")})`;
      const expectedText = expected
        ? `(${expected.parameters.join(",")}) -> (${expected.results.join(",")})`
        : "<no declared signature>";
      throw new OlangBrowserBundleError(
        "abi-mismatch",
        `program.wasm import signature mismatch for ${entry.module}:${entry.name}: expected=${expectedText} actual=${actualText}`,
      );
    }
  }

  const exports = new Map(WebAssembly.Module.exports(module).map((entry) => [entry.name, entry.kind]));
  for (const name of abi.required_exports) {
    const expectedKind = name === "memory" ? "memory" : "function";
    if (exports.get(name) !== expectedKind) {
      throw new OlangBrowserBundleError(
        "abi-mismatch",
        `program.wasm must export ${name} as ${expectedKind}`,
      );
    }
  }
  const startSignature = typedModule.exportedFunctions.get("_start");
  if (
    !startSignature
    || startSignature.parameters.length !== 0
    || startSignature.results.length !== 0
  ) {
    throw new OlangBrowserBundleError(
      "abi-mismatch",
      "program.wasm must export _start with the WASI command signature () -> ()",
    );
  }
  return module;
}

function validateProvider(provider) {
  if (
    !provider
    || provider.schema !== WHOLE_PROGRAM_PROVIDER_SCHEMA
    || typeof provider.executeProgram !== "function"
  ) {
    throw new OlangBrowserProviderError(
      `provider must declare schema ${WHOLE_PROGRAM_PROVIDER_SCHEMA} and executeProgram(request)`,
    );
  }
  return provider;
}

function validateProviderResult(result) {
  if (
    !result
    || typeof result.ok !== "boolean"
    || !Number.isSafeInteger(result.exitCode)
    || result.exitCode < 0
    || result.exitCode > 0xffff_ffff
    || typeof result.stdout !== "string"
    || typeof result.stderr !== "string"
    || result.ok !== (result.exitCode === 0)
  ) {
    throw new OlangBrowserProviderError(
      "whole-program provider returned an invalid result; expected consistent {ok, exitCode, stdout, stderr}",
    );
  }
  return Object.freeze({
    ok: result.ok,
    exitCode: result.exitCode,
    stdout: result.stdout,
    stderr: result.stderr,
  });
}

export async function runOlangBrowserBundle(options = {}) {
  const baseUrl = options.baseUrl ?? new URL("./", import.meta.url);
  const manifest = immutableManifest(
    options.manifest
      ?? await fetchManifest(new URL("manifest.json", baseUrl)),
  );
  const blockers = manifest.compatibility.blockers;

  if (options.source !== undefined && options.sourceBytes !== undefined) {
    throw new OlangBrowserBundleError(
      "invalid-options",
      "provide source or sourceBytes, not both",
    );
  }
  if (options.source !== undefined && typeof options.source !== "string") {
    throw new OlangBrowserBundleError(
      "invalid-options",
      "source must be a primitive string",
    );
  }
  const sourceBytes = options.sourceBytes !== undefined
    ? bytesFrom(options.sourceBytes, "sourceBytes")
    : options.source !== undefined
      ? new TextEncoder().encode(options.source)
      : await fetchBytes(new URL(manifest.source.path, baseUrl));
  const wasmBytes = options.wasmBytes !== undefined
    ? bytesFrom(options.wasmBytes, "wasmBytes")
    : await fetchBytes(new URL(manifest.artifact.path, baseUrl));
  const planBytes = options.planBytes !== undefined
    ? bytesFrom(options.planBytes, "planBytes")
    : await fetchBytes(new URL(manifest.plan.path, baseUrl));
  const assetPayloads = await Promise.all(manifest.assets.map(async (record) => {
    const bytes = options.assetBytes === undefined
      ? await fetchBytes(new URL(record.path, baseUrl))
      : suppliedNamedBytes(options.assetBytes, "assetBytes", record.path);
    if (bytes === null) {
      throw new OlangBrowserBundleError(
        "invalid-options",
        `assetBytes is missing ${record.path}`,
      );
    }
    return [record, bytes];
  }));
  const adapterPayloads = await Promise.all(manifest.adapters.map(async (adapter) => {
    const record = adapter.file;
    const bytes = options.adapterBytes === undefined
      ? await fetchBytes(new URL(record.path, baseUrl))
      : suppliedNamedBytes(options.adapterBytes, "adapterBytes", record.path);
    if (bytes === null) {
      throw new OlangBrowserBundleError(
        "invalid-options",
        `adapterBytes is missing ${record.path}`,
      );
    }
    return [adapter, bytes];
  }));
  await Promise.all([
    verifyFile(manifest.source, sourceBytes, manifest.source.path),
    verifyFile(manifest.artifact, wasmBytes, manifest.artifact.path),
    verifyFile(manifest.plan, planBytes, manifest.plan.path),
    ...assetPayloads.map(([record, bytes]) => verifyFile(record, bytes, record.path)),
    ...adapterPayloads.map(([adapter, bytes]) => (
      verifyFile(adapter.file, bytes, adapter.file.path)
    )),
  ]);
  const source = decodeUtf8(sourceBytes, manifest.source.path);
  const planText = decodeUtf8(planBytes, manifest.plan.path);
  const planNodes = new Map();
  for (const line of planText.split(/\r?\n/u)) {
    const match = /^node ([0-9]+) (.+)$/u.exec(line);
    if (match) planNodes.set(Number(match[1]), match[2]);
  }
  if (
    planNodes.size !== manifest.plan.nodes
    || [...planNodes.keys()].some((node, index) => node !== index)
  ) {
    throw new OlangBrowserBundleError(
      "integrity-failed",
      `${manifest.plan.path} node identity/count mismatch: manifest=${manifest.plan.nodes} actual=${planNodes.size}`,
    );
  }
  const expectedBlockers = new Map();
  for (const [planNode, description] of planNodes) {
    if (description.startsWith("exec ") && description.includes(" execution=shim ")) {
      const backend = / backend=([^ ]+) /u.exec(description)?.[1];
      const requiredText = / required=\[([^\]]*)\]$/u.exec(description)?.[1];
      if (!backend || requiredText === undefined) {
        throw new OlangBrowserBundleError(
          "integrity-failed",
          `cannot classify shim node ${planNode} in ${manifest.plan.path}`,
        );
      }
      expectedBlockers.set(planNode, {
        code: "shim-backend",
        operation: "exec",
        backend,
        requiredAuthorities: requiredText === "" ? [] : requiredText.split(","),
      });
    } else if (description.startsWith("request ")) {
      const operation = / \[([a-z_]+)\]$/u.exec(description)?.[1];
      if (!operation) {
        throw new OlangBrowserBundleError(
          "integrity-failed",
          `cannot classify request node ${planNode} in ${manifest.plan.path}`,
        );
      }
      expectedBlockers.set(planNode, {
        code: "effectful-request",
        operation,
        backend: undefined,
        requiredAuthorities: [],
      });
    }
  }
  if (expectedBlockers.size !== blockers.length) {
    throw new OlangBrowserBundleError(
      "integrity-failed",
      `compatibility blocker count does not match ${manifest.plan.path}: manifest=${blockers.length} actual=${expectedBlockers.size}`,
    );
  }
  for (const blocker of blockers) {
    const expected = expectedBlockers.get(blocker.plan_node);
    if (
      !expected
      || blocker.code !== expected.code
      || blocker.operation !== expected.operation
      || blocker.backend !== expected.backend
      || blocker.required_authorities.length !== expected.requiredAuthorities.length
      || blocker.required_authorities.some(
        (authority, index) => authority !== expected.requiredAuthorities[index],
      )
    ) {
      throw new OlangBrowserBundleError(
        "integrity-failed",
        `compatibility blocker ${blocker.diagnostic} does not match ${manifest.plan.path}`,
      );
    }
  }
  if (options.source !== undefined && source !== options.source) {
    throw new OlangBrowserBundleError(
      "source-invalid",
      "source must round-trip through canonical UTF-8 without replacement",
    );
  }

  const module = await compileAndVerifyModule(wasmBytes, manifest.abi);

  if (!manifest.compatibility.local_execution) {
    if (!options.provider) {
      throw new OlangBrowserCompatibilityError(blockers);
    }
    const provider = validateProvider(options.provider);
    const adapters = Object.freeze(adapterPayloads.map(([adapter, bytes]) => Object.freeze({
      name: adapter.name,
      path: adapter.file.path,
      sha256: adapter.file.sha256,
      bytes: Uint8Array.from(bytes),
    })));
    let providerResult;
    try {
      providerResult = await provider.executeProgram(Object.freeze({
        source,
        manifest,
        adapters,
      }));
    } catch (error) {
      throw new OlangBrowserProviderError(
        `whole-program provider failed: ${error?.message ?? String(error)}`,
        error,
      );
    }
    return Object.freeze({
      ...validateProviderResult(providerResult),
      executionMode: "whole-program-provider",
      manifest,
    });
  }

  const host = new WasiPreview1Host({
    args: options.args ?? [],
    env: options.env ?? {},
    onStdout: options.onStdout,
    onStderr: options.onStderr,
  });

  let instance;
  try {
    const instantiated = await WebAssembly.instantiate(module, host.imports);
    instance = instantiated.instance ?? instantiated;
  } catch (error) {
    throw new WasiHostError(
      "instantiate-failed",
      `failed to instantiate Olang browser bundle: ${error?.message ?? String(error)}`,
      error,
    );
  }
  return Object.freeze({
    ...host.run(instance),
    executionMode: "browser-local-wasi-preview1",
    manifest,
  });
}
