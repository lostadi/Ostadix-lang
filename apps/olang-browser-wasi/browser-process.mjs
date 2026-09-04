import { spawn } from "node:child_process";

const MAX_OUTPUT_BYTES = 8 * 1024 * 1024;
const MAX_MESSAGE_BYTES = 8 * 1024 * 1024;

// Chrome reads null-delimited CDP JSON on fd 3 and writes it on fd 4.
// A private pipe needs neither a debugger TCP port nor a browser dependency.
export function launchBrowser(command, args) {
  const child = spawn(command, args, {
    detached: process.platform !== "win32",
    stdio: ["ignore", "pipe", "pipe", "pipe", "pipe"],
  });
  let nextId = 0;
  let protocolBuffer = Buffer.alloc(0);
  let outputBytes = 0;
  let stderr = "";
  let failure;
  let exitResult;
  const pending = new Map();
  let rejectFailure;
  const failed = new Promise((_, reject) => { rejectFailure = reject; });
  // Failure can precede the first phase (for example an ENOENT spawn failure).
  void failed.catch(() => {});
  let resolveClosed;
  const closed = new Promise((resolve) => { resolveClosed = resolve; });

  function fail(error) {
    if (failure) return;
    failure = error;
    rejectFailure(error);
    for (const request of pending.values()) request.reject(error);
    pending.clear();
  }

  child.once("error", fail);
  child.once("exit", (code, signal) => {
    exitResult = { code, signal };
    fail(new Error(`browser exited (code ${code}, signal ${signal ?? "none"})`));
  });
  child.once("close", (code, signal) => {
    exitResult ??= { code, signal };
    resolveClosed(exitResult);
  });
  for (const stream of [child.stdout, child.stderr]) {
    stream.on("data", (chunk) => {
      if (failure) return;
      const remaining = Math.max(0, MAX_OUTPUT_BYTES - outputBytes);
      outputBytes += chunk.length;
      if (stream === child.stderr) stderr += chunk.subarray(0, remaining).toString("utf8");
      if (outputBytes > MAX_OUTPUT_BYTES) {
        fail(new Error(`browser exceeded ${MAX_OUTPUT_BYTES} diagnostic output bytes`));
      }
    });
  }
  child.stdio[3].on("error", fail);
  child.stdio[4].on("error", fail);
  child.stdio[4].on("end", () => fail(new Error("browser closed its DevTools pipe")));
  child.stdio[4].on("data", (chunk) => {
    if (failure) return;
    protocolBuffer = Buffer.concat([protocolBuffer, chunk]);
    let boundary;
    while ((boundary = protocolBuffer.indexOf(0)) !== -1) {
      if (boundary > MAX_MESSAGE_BYTES) {
        fail(new Error("browser DevTools message exceeded its size limit"));
        return;
      }
      const frame = protocolBuffer.subarray(0, boundary);
      protocolBuffer = protocolBuffer.subarray(boundary + 1);
      try {
        const message = JSON.parse(frame.toString("utf8"));
        const request = pending.get(message.id);
        if (!request) continue;
        pending.delete(message.id);
        if (message.error) {
          request.reject(new Error(`${request.method}: ${JSON.stringify(message.error)}`));
        } else {
          request.resolve(message.result);
        }
      } catch (error) {
        fail(new Error(`invalid browser DevTools message: ${error.message}`));
      }
    }
    if (protocolBuffer.length > MAX_MESSAGE_BYTES) {
      fail(new Error("browser DevTools message exceeded its size limit"));
    }
  });

  function send(method, params = {}, sessionId) {
    if (failure) return Promise.reject(failure);
    const id = ++nextId;
    return new Promise((resolve, reject) => {
      pending.set(id, { method, resolve, reject });
      child.stdio[3].write(`${JSON.stringify({ id, method, params, sessionId })}\0`);
    });
  }

  async function phase(label, timeoutMs, action) {
    let timer;
    try {
      return await Promise.race([
        failed,
        Promise.resolve().then(action),
        new Promise((_, reject) => {
          timer = setTimeout(() => reject(new Error(`${label} exceeded ${timeoutMs} ms`)), timeoutMs);
        }),
      ]);
    } finally {
      clearTimeout(timer);
    }
  }

  function killGroup() {
    if (!child.pid) return;
    try {
      if (process.platform === "win32") child.kill("SIGKILL");
      else process.kill(-child.pid, "SIGKILL");
    } catch (error) {
      if (error.code !== "ESRCH") {
        error.message += ` (browser pid ${child.pid}, exit ${JSON.stringify(exitResult)})`;
        throw error;
      }
    }
  }

  async function close(graceMs = 2_000) {
    let timer;
    try {
      if (!failure) void send("Browser.close").catch(() => {});
      await Promise.race([
        closed,
        new Promise((resolve) => { timer = setTimeout(resolve, graceMs); }),
      ]);
    } finally {
      clearTimeout(timer);
      // Include descendants that retain stdout/stderr after the parent exits.
      killGroup();
      for (const stream of child.stdio) stream?.destroy();
      await closed;
    }
  }

  return {
    send,
    phase,
    close,
    abort: fail,
    get stderr() { return stderr; },
    get pid() { return child.pid; },
  };
}
