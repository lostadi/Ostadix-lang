import assert from "node:assert/strict";
import { setTimeout as delay } from "node:timers/promises";
import { launchBrowser } from "./browser-process.mjs";

// A subprocess speaks the same private pipe protocol as Chrome. Delays, crashes,
// and inherited pipes exercise failure cases without depending on a local GUI.
const fixture = `
  import { createReadStream, writeSync } from "node:fs";
  import { spawn } from "node:child_process";
  const mode = process.argv[1];
  let input = "";
  let descendant;
  const reply = (message) => {
    const bytes = Buffer.from(JSON.stringify(message) + "\\0");
    // Deliberately split a response across chunks in the transport.
    writeSync(4, bytes.subarray(0, 5));
    setTimeout(() => writeSync(4, bytes.subarray(5)), 5);
  };
  createReadStream(null, { fd: 3 }).on("data", (chunk) => {
    input += chunk;
    let end;
    while ((end = input.indexOf("\\0")) !== -1) {
      const request = JSON.parse(input.slice(0, end));
      input = input.slice(end + 1);
      if (request.method === "Browser.close") {
        if (mode !== "ignore-close") process.exit(0);
        continue;
      }
      if (mode === "hang") continue;
      if (mode === "crash") process.exit(7);
      if (mode === "malformed") { writeSync(4, "bad-json\\0"); continue; }
      if (mode === "flood") { process.stderr.write("x".repeat(9 * 1024 * 1024)); continue; }
      if (mode === "descendant") {
        descendant ??= spawn(process.execPath, ["-e", "setInterval(() => {}, 1000)"], { stdio: "inherit" });
      }
      const wait = mode === "delayed" ? 250 : request.method === "slow" ? 40 : 0;
      setTimeout(() => reply({
        id: request.id,
        result: { method: request.method, sessionId: request.sessionId, descendant: descendant?.pid },
      }), wait);
    }
  });
`;

function start(mode) {
  return launchBrowser(process.execPath, ["--input-type=module", "-e", fixture, mode]);
}

async function rejectsMode(mode, pattern, timeoutMs = 1_000) {
  const browser = start(mode);
  try {
    await assert.rejects(browser.phase("browser startup/readiness", timeoutMs,
      () => browser.send("Browser.getVersion")), pattern);
  } finally {
    await browser.close(30);
  }
}

const reordered = start("normal");
try {
  const result = await reordered.phase("protocol", 2_000, () => Promise.all([
    reordered.send("slow", {}, "session-a"),
    reordered.send("fast", {}, "session-b"),
  ]));
  assert.deepEqual(result.map(({ method, sessionId }) => [method, sessionId]), [
    ["slow", "session-a"], ["fast", "session-b"],
  ]);
} finally {
  await reordered.close(100);
}

const delayed = start("delayed");
try {
  // Combined latency exceeds either phase budget; each phase gets its own clock.
  await delayed.phase("browser startup/readiness", 450, () => delayed.send("Browser.getVersion"));
  await delayed.phase("browser navigation/UI execution", 450, () => delayed.send("Page.navigate"));
  await assert.rejects(delayed.phase("browser navigation/UI execution", 20,
    () => new Promise(() => {})), /browser navigation\/UI execution exceeded 20 ms/);
} finally {
  await delayed.close(100);
}

await rejectsMode("hang", /browser startup\/readiness exceeded 100 ms/, 100);
await rejectsMode("crash", /browser (exited|closed its DevTools pipe)/);
await rejectsMode("malformed", /invalid browser DevTools message/);
await rejectsMode("flood", /diagnostic output bytes/);

const nonexistent = launchBrowser("/does-not-exist/olang-browser", []);
try {
  await assert.rejects(nonexistent.phase("startup", 1_000,
    () => nonexistent.send("Browser.getVersion")), /ENOENT/);
} finally {
  await nonexistent.close(30);
}

if (process.platform !== "win32") {
  const inherited = start("descendant");
  const { descendant } = await inherited.phase("startup", 1_000,
    () => inherited.send("Browser.getVersion"));
  process.kill(descendant, 0);
  await inherited.close(100);
  let stillAlive = true;
  for (let attempt = 0; attempt < 100 && stillAlive; attempt++) {
    try { process.kill(descendant, 0); } catch (error) {
      if (error.code !== "ESRCH") throw error;
      stillAlive = false;
    }
    if (stillAlive) await delay(10);
  }
  assert.equal(stillAlive, false, "browser descendant survived cleanup");
}

const stubborn = start("ignore-close");
await stubborn.phase("startup", 1_000, () => stubborn.send("Browser.getVersion"));
await stubborn.close(30);
assert.throws(() => process.kill(stubborn.pid, 0), { code: "ESRCH" });

const interrupted = start("normal");
await interrupted.phase("startup", 1_000, () => interrupted.send("Browser.getVersion"));
interrupted.abort(new Error("qualification interrupted by SIGTERM"));
await assert.rejects(interrupted.phase("execution", 1_000, () => new Promise(() => {})), /interrupted by SIGTERM/);
await interrupted.close(30);
assert.throws(() => process.kill(interrupted.pid, 0), { code: "ESRCH" });

console.log("olang-browser-wasi browser harness tests: PASS");
