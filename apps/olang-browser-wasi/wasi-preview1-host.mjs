// Dependency-free WASI Preview 1 host for Olang browser bundles.
//
// This is intentionally a narrow command host: stdout/stderr, args/env,
// cryptographic randomness, and clocks are implemented. Browser filesystem and
// process authority are not synthesized; those operations fail closed.

export const WASI_PREVIEW1_IMPORTS = Object.freeze([
  "args_get",
  "args_sizes_get",
  "clock_time_get",
  "environ_get",
  "environ_sizes_get",
  "fd_close",
  "fd_fdstat_get",
  "fd_filestat_get",
  "fd_prestat_dir_name",
  "fd_prestat_get",
  "fd_read",
  "fd_readdir",
  "fd_seek",
  "fd_write",
  "path_filestat_get",
  "path_open",
  "path_readlink",
  "path_remove_directory",
  "path_rename",
  "path_unlink_file",
  "poll_oneoff",
  "proc_exit",
  "random_get",
  "sched_yield",
]);

const signature = (parameters, results = ["i32"]) => Object.freeze({
  parameters: Object.freeze(parameters),
  results: Object.freeze(results),
});

// Canonical wasm32 signatures from WASI Preview 1. These are checked against
// the binary type/import sections before any guest code or provider runs.
export const WASI_PREVIEW1_SIGNATURES = Object.freeze({
  args_get: signature(["i32", "i32"]),
  args_sizes_get: signature(["i32", "i32"]),
  clock_time_get: signature(["i32", "i64", "i32"]),
  environ_get: signature(["i32", "i32"]),
  environ_sizes_get: signature(["i32", "i32"]),
  fd_close: signature(["i32"]),
  fd_fdstat_get: signature(["i32", "i32"]),
  fd_filestat_get: signature(["i32", "i32"]),
  fd_prestat_dir_name: signature(["i32", "i32", "i32"]),
  fd_prestat_get: signature(["i32", "i32"]),
  fd_read: signature(["i32", "i32", "i32", "i32"]),
  fd_readdir: signature(["i32", "i32", "i32", "i64", "i32"]),
  fd_seek: signature(["i32", "i64", "i32", "i32"]),
  fd_write: signature(["i32", "i32", "i32", "i32"]),
  path_filestat_get: signature(["i32", "i32", "i32", "i32", "i32"]),
  path_open: signature([
    "i32", "i32", "i32", "i32", "i32", "i64", "i64", "i32", "i32",
  ]),
  path_readlink: signature(["i32", "i32", "i32", "i32", "i32", "i32"]),
  path_remove_directory: signature(["i32", "i32", "i32"]),
  path_rename: signature(["i32", "i32", "i32", "i32", "i32", "i32"]),
  path_unlink_file: signature(["i32", "i32", "i32"]),
  poll_oneoff: signature(["i32", "i32", "i32", "i32"]),
  proc_exit: signature(["i32"], []),
  random_get: signature(["i32", "i32"]),
  sched_yield: signature([]),
});

const ERRNO = Object.freeze({
  SUCCESS: 0,
  BADF: 8,
  FAULT: 21,
  INVAL: 28,
  NOSYS: 52,
  NOTSUP: 58,
  SPIPE: 70,
  NOTCAPABLE: 76,
});

const CLOCK = Object.freeze({ REALTIME: 0, MONOTONIC: 1 });
const FILETYPE_CHARACTER_DEVICE = 2;

export class WasiExit extends Error {
  constructor(code) {
    super(`WASI process exited with status ${code}`);
    this.name = "WasiExit";
    this.code = Number(code) >>> 0;
  }
}

export class WasiHostError extends Error {
  constructor(code, message, cause) {
    super(message, cause === undefined ? undefined : { cause });
    this.name = "WasiHostError";
    this.code = code;
  }
}

function encodeCStringList(values) {
  const encoder = new TextEncoder();
  return values.map((value) => {
    const body = encoder.encode(String(value));
    const encoded = new Uint8Array(body.length + 1);
    encoded.set(body);
    return encoded;
  });
}

function normalizeEnvironment(environment) {
  if (environment instanceof Map) {
    return [...environment.entries()].map(([key, value]) => `${key}=${value}`);
  }
  return Object.entries(environment ?? {})
    .sort(([left], [right]) => left.localeCompare(right))
    .map(([key, value]) => `${key}=${value}`);
}

export class WasiPreview1Host {
  constructor(options = {}) {
    this.args = encodeCStringList(options.args ?? []);
    this.environment = encodeCStringList(normalizeEnvironment(options.env));
    this.stdout = "";
    this.stderr = "";
    this.stdoutDecoder = new TextDecoder();
    this.stderrDecoder = new TextDecoder();
    this.onStdout = options.onStdout ?? null;
    this.onStderr = options.onStderr ?? null;
    this.memory = null;
    this.closedDescriptors = new Set();
    this.imports = Object.freeze({
      wasi_snapshot_preview1: Object.freeze(this.#buildImports()),
    });
  }

  attach(instanceOrMemory) {
    const memory = instanceOrMemory instanceof WebAssembly.Memory
      ? instanceOrMemory
      : instanceOrMemory?.exports?.memory;
    if (!(memory instanceof WebAssembly.Memory)) {
      throw new WasiHostError(
        "missing-memory-export",
        "Olang browser bundle must export WebAssembly memory as `memory`",
      );
    }
    this.memory = memory;
  }

  run(instance) {
    this.attach(instance);
    const start = instance?.exports?._start;
    if (typeof start !== "function") {
      throw new WasiHostError(
        "missing-start-export",
        "Olang browser bundle must export the WASI command entrypoint `_start`",
      );
    }
    let exitCode = 0;
    try {
      start();
    } catch (error) {
      if (error instanceof WasiExit) {
        exitCode = error.code;
      } else {
        throw new WasiHostError(
          "wasm-trap",
          `Olang browser bundle trapped: ${error?.message ?? String(error)}`,
          error,
        );
      }
    }
    this.#appendDecoded(1, new Uint8Array(), false);
    this.#appendDecoded(2, new Uint8Array(), false);
    return Object.freeze({
      ok: exitCode === 0,
      exitCode,
      stdout: this.stdout,
      stderr: this.stderr,
    });
  }

  #bytes(pointer, length) {
    if (!(this.memory instanceof WebAssembly.Memory)) {
      throw new WasiHostError("memory-not-attached", "WASI memory is not attached");
    }
    const start = Number(pointer) >>> 0;
    const size = Number(length) >>> 0;
    const end = start + size;
    if (!Number.isSafeInteger(end) || end > this.memory.buffer.byteLength) {
      throw new RangeError("guest memory range is out of bounds");
    }
    return new Uint8Array(this.memory.buffer, start, size);
  }

  #view(pointer, length) {
    const bytes = this.#bytes(pointer, length);
    return new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  }

  #writeU32(pointer, value) {
    this.#view(pointer, 4).setUint32(0, Number(value) >>> 0, true);
  }

  #writeU64(pointer, value) {
    this.#view(pointer, 8).setBigUint64(0, BigInt.asUintN(64, BigInt(value)), true);
  }

  #writeStringVector(entries, pointers, buffer) {
    let cursor = Number(buffer) >>> 0;
    for (let index = 0; index < entries.length; index += 1) {
      this.#writeU32((Number(pointers) >>> 0) + index * 4, cursor);
      this.#bytes(cursor, entries[index].length).set(entries[index]);
      cursor += entries[index].length;
    }
  }

  #listBytes(entries) {
    return entries.reduce((total, entry) => total + entry.length, 0);
  }

  #descriptorOpen(fd) {
    return Number.isInteger(fd) && fd >= 0 && fd <= 2 && !this.closedDescriptors.has(fd);
  }

  #guard(operation) {
    try {
      return operation();
    } catch (error) {
      if (error instanceof WasiExit) {
        throw error;
      }
      return ERRNO.FAULT;
    }
  }

  #appendDecoded(fd, bytes, stream) {
    const decoder = fd === 1 ? this.stdoutDecoder : this.stderrDecoder;
    const text = decoder.decode(bytes, { stream });
    if (text.length === 0) return;
    if (fd === 1) {
      this.stdout += text;
      this.onStdout?.(text);
    } else {
      this.stderr += text;
      this.onStderr?.(text);
    }
  }

  #buildImports() {
    return {
      args_sizes_get: (countPointer, bytesPointer) => this.#guard(() => {
        this.#writeU32(countPointer, this.args.length);
        this.#writeU32(bytesPointer, this.#listBytes(this.args));
        return ERRNO.SUCCESS;
      }),
      args_get: (pointers, buffer) => this.#guard(() => {
        this.#writeStringVector(this.args, pointers, buffer);
        return ERRNO.SUCCESS;
      }),
      environ_sizes_get: (countPointer, bytesPointer) => this.#guard(() => {
        this.#writeU32(countPointer, this.environment.length);
        this.#writeU32(bytesPointer, this.#listBytes(this.environment));
        return ERRNO.SUCCESS;
      }),
      environ_get: (pointers, buffer) => this.#guard(() => {
        this.#writeStringVector(this.environment, pointers, buffer);
        return ERRNO.SUCCESS;
      }),
      random_get: (buffer, length) => this.#guard(() => {
        const destination = this.#bytes(buffer, length);
        const cryptoObject = globalThis.crypto;
        if (!cryptoObject || typeof cryptoObject.getRandomValues !== "function") {
          return ERRNO.NOSYS;
        }
        for (let offset = 0; offset < destination.length; offset += 65_536) {
          cryptoObject.getRandomValues(destination.subarray(offset, offset + 65_536));
        }
        return ERRNO.SUCCESS;
      }),
      clock_time_get: (clockId, _precision, timePointer) => this.#guard(() => {
        let nanoseconds;
        if (clockId === CLOCK.REALTIME) {
          nanoseconds = BigInt(Date.now()) * 1_000_000n;
        } else if (clockId === CLOCK.MONOTONIC) {
          const milliseconds = globalThis.performance?.now?.() ?? Date.now();
          nanoseconds = BigInt(Math.floor(milliseconds * 1_000_000));
        } else {
          return ERRNO.INVAL;
        }
        this.#writeU64(timePointer, nanoseconds);
        return ERRNO.SUCCESS;
      }),
      fd_write: (fd, iovecs, iovecCount, writtenPointer) => this.#guard(() => {
        if (!this.#descriptorOpen(fd) || (fd !== 1 && fd !== 2)) {
          return ERRNO.BADF;
        }
        const chunks = [];
        let byteCount = 0;
        for (let index = 0; index < iovecCount; index += 1) {
          const iovec = this.#view((Number(iovecs) >>> 0) + index * 8, 8);
          const pointer = iovec.getUint32(0, true);
          const length = iovec.getUint32(4, true);
          const chunk = this.#bytes(pointer, length);
          chunks.push(chunk.slice());
          byteCount += chunk.length;
        }
        const joined = new Uint8Array(byteCount);
        let offset = 0;
        for (const chunk of chunks) {
          joined.set(chunk, offset);
          offset += chunk.length;
        }
        this.#appendDecoded(fd, joined, true);
        this.#writeU32(writtenPointer, byteCount);
        return ERRNO.SUCCESS;
      }),
      fd_read: (fd, _iovecs, _iovecCount, readPointer) => this.#guard(() => {
        if (!this.#descriptorOpen(fd) || fd !== 0) {
          return ERRNO.BADF;
        }
        this.#writeU32(readPointer, 0);
        return ERRNO.SUCCESS;
      }),
      fd_close: (fd) => {
        if (!this.#descriptorOpen(fd)) {
          return ERRNO.BADF;
        }
        this.closedDescriptors.add(fd);
        return ERRNO.SUCCESS;
      },
      fd_fdstat_get: (fd, statPointer) => this.#guard(() => {
        if (!this.#descriptorOpen(fd)) {
          return ERRNO.BADF;
        }
        this.#bytes(statPointer, 24).fill(0);
        this.#view(statPointer, 24).setUint8(0, FILETYPE_CHARACTER_DEVICE);
        return ERRNO.SUCCESS;
      }),
      fd_filestat_get: (fd, statPointer) => this.#guard(() => {
        if (!this.#descriptorOpen(fd)) {
          return ERRNO.BADF;
        }
        this.#bytes(statPointer, 64).fill(0);
        this.#view(statPointer, 64).setUint8(16, FILETYPE_CHARACTER_DEVICE);
        return ERRNO.SUCCESS;
      }),
      fd_prestat_get: () => ERRNO.BADF,
      fd_prestat_dir_name: () => ERRNO.BADF,
      fd_readdir: () => ERRNO.BADF,
      fd_seek: (fd) => this.#descriptorOpen(fd) ? ERRNO.SPIPE : ERRNO.BADF,
      path_filestat_get: () => ERRNO.NOTCAPABLE,
      path_open: () => ERRNO.NOTCAPABLE,
      path_readlink: () => ERRNO.NOTCAPABLE,
      path_remove_directory: () => ERRNO.NOTCAPABLE,
      path_rename: () => ERRNO.NOTCAPABLE,
      path_unlink_file: () => ERRNO.NOTCAPABLE,
      poll_oneoff: () => ERRNO.NOTSUP,
      proc_exit: (code) => {
        throw new WasiExit(code);
      },
      sched_yield: () => ERRNO.SUCCESS,
    };
  }
}
