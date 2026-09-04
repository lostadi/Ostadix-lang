import { spawn } from "node:child_process";
import { constants as fsConstants } from "node:fs";
import {
  access,
  mkdtemp,
  readFile,
  realpath,
  rm,
  stat,
} from "node:fs/promises";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { extname, join, resolve, sep } from "node:path";

const PASS_MARKER = "OSTADIX_BROWSER_WASI_DOM_PASS_V1";
const TEST_PAGE = "/__olang_browser_qualification__.html";
const WAIT_RESOURCE = "/__olang_browser_qualification_wait__.gif";
const DONE_RESOURCE = "/__olang_browser_qualification_done__";
const PROCESS_TIMEOUT_MS = 30_000;
const MAX_PROCESS_OUTPUT_BYTES = 8 * 1024 * 1024;
const ONE_PIXEL_GIF = Buffer.from(
  "R0lGODlhAQABAIAAAAAAAP///ywAAAAAAQABAAACAUwAOw==",
  "base64",
);

const [bundleArgument, expectedStdout] = process.argv.slice(2);
if (!bundleArgument || expectedStdout === undefined) {
  throw new Error(
    "usage: node test-browser.mjs BUNDLE_DIR EXPECTED_STDOUT_SUBSTRING",
  );
}

async function browserTestPage(bundleRoot) {
  const index = await readFile(join(bundleRoot, "index.html"), "utf8");
  if (!index.includes("</body>")) {
    throw new Error("shipped index.html lacks a closing body element");
  }
  const qualification = `
    <img hidden alt="" src="${WAIT_RESOURCE}">
    <script type="module">
      const runButton = document.querySelector("#run");
      const output = document.querySelector("#output");
      const expected = new URL(location.href).searchParams.get("expected");
      const passMarker = ["OSTADIX", "BROWSER", "WASI", "DOM", "PASS", "V1"].join("_");
      const failMarker = ["OSTADIX", "BROWSER", "WASI", "DOM", "FAIL", "V1"].join("_");
      let status = "fail";
      try {
        if (!globalThis.isSecureContext) throw new Error("loopback page is not a secure context");
        if (!(runButton instanceof HTMLButtonElement) || !(output instanceof HTMLElement)) {
          throw new Error("shipped browser UI selectors are missing");
        }
        const completed = new Promise((resolve) => {
          document.addEventListener("olang-browser-run-complete", resolve, { once: true });
        });
        runButton.click();
        const event = await completed;
        if (event.detail?.status !== "success" || event.detail?.exitCode !== 0) {
          throw new Error(\`program failed through UI: \${JSON.stringify(event.detail)}\`);
        }
        if (document.documentElement.dataset.olangExecution !== "success") {
          throw new Error("browser-main did not expose successful UI state");
        }
        if (expected === null || !output.textContent.includes(expected)) {
          throw new Error(\`UI output did not contain the expected marker: \${output.textContent}\`);
        }
        document.documentElement.setAttribute("data-olang-browser-qualification", "pass");
        output.textContent = passMarker;
        status = "pass";
      } catch (error) {
        document.documentElement.setAttribute("data-olang-browser-qualification", "fail");
        if (output) output.textContent = \`\${failMarker}: \${error?.stack ?? error}\`;
      } finally {
        const completion = new URLSearchParams({
          status,
          domStatus: document.documentElement.getAttribute("data-olang-browser-qualification") ?? "",
          domMarker: (output?.textContent ?? "").slice(0, 4096),
        });
        await fetch(\`${DONE_RESOURCE}?\${completion}\`, { cache: "no-store" });
      }
    </script>
  `;
  return index.replace("</body>", `${qualification}</body>`);
}

const MIME_TYPES = Object.freeze({
  ".html": "text/html; charset=utf-8",
  ".json": "application/json; charset=utf-8",
  ".mjs": "text/javascript; charset=utf-8",
  ".O": "text/plain; charset=utf-8",
  ".txt": "text/plain; charset=utf-8",
  ".wasm": "application/wasm",
});

function send(response, status, contentType, body, headOnly = false) {
  const bytes = Buffer.isBuffer(body) ? body : Buffer.from(body);
  response.writeHead(status, {
    "Cache-Control": "no-store",
    "Content-Length": bytes.byteLength,
    "Content-Type": contentType,
    "X-Content-Type-Options": "nosniff",
  });
  response.end(headOnly ? undefined : bytes);
}

async function createBundleServer(bundleRoot) {
  const page = await browserTestPage(bundleRoot);
  const waitingResponses = [];
  const requestLog = [];
  let completionStatus;
  let resolveCompletion;
  const completion = new Promise((resolvePromise) => {
    resolveCompletion = resolvePromise;
  });
  const server = createServer((request, response) => {
    void (async () => {
      const method = request.method ?? "GET";
      if (method !== "GET" && method !== "HEAD") {
        send(response, 405, "text/plain; charset=utf-8", "method not allowed\n", method === "HEAD");
        return;
      }

      const url = new URL(request.url ?? "/", "http://127.0.0.1");
      requestLog.push(`${method} ${url.pathname}${url.search}`);
      if (url.pathname === TEST_PAGE) {
        send(response, 200, MIME_TYPES[".html"], page, method === "HEAD");
        return;
      }
      if (url.pathname === WAIT_RESOURCE) {
        if (completionStatus !== undefined) {
          send(response, 200, "image/gif", ONE_PIXEL_GIF, method === "HEAD");
        } else {
          waitingResponses.push({ response, headOnly: method === "HEAD" });
        }
        return;
      }
      if (url.pathname === DONE_RESOURCE) {
        const status = url.searchParams.get("status");
        const domStatus = url.searchParams.get("domStatus");
        const domMarker = url.searchParams.get("domMarker");
        if (status !== "pass" && status !== "fail") {
          send(response, 400, "text/plain; charset=utf-8", "invalid completion status\n");
          return;
        }
        if (completionStatus !== undefined && completionStatus !== status) {
          send(response, 409, "text/plain; charset=utf-8", "completion status changed\n");
          return;
        }
        const firstCompletion = completionStatus === undefined;
        completionStatus = status;
        send(response, 200, "text/plain; charset=utf-8", `${status}\n`, method === "HEAD");
        for (const waiter of waitingResponses.splice(0)) {
          send(waiter.response, 200, "image/gif", ONE_PIXEL_GIF, waiter.headOnly);
        }
        if (firstCompletion) resolveCompletion({ status, domStatus, domMarker });
        return;
      }

      let pathname;
      try {
        pathname = decodeURIComponent(url.pathname);
      } catch {
        send(response, 400, "text/plain; charset=utf-8", "invalid URL encoding\n");
        return;
      }
      const relative = pathname === "/" ? "index.html" : pathname.replace(/^\/+/, "");
      const candidate = resolve(bundleRoot, relative);
      if (candidate !== bundleRoot && !candidate.startsWith(`${bundleRoot}${sep}`)) {
        send(response, 403, "text/plain; charset=utf-8", "path escapes bundle root\n");
        return;
      }

      let canonical;
      try {
        canonical = await realpath(candidate);
        const metadata = await stat(canonical);
        if (!metadata.isFile()) throw new Error("not a regular file");
      } catch {
        send(response, 404, "text/plain; charset=utf-8", "not found\n");
        return;
      }
      if (!canonical.startsWith(`${bundleRoot}${sep}`)) {
        send(response, 403, "text/plain; charset=utf-8", "symlink escapes bundle root\n");
        return;
      }

      const body = await readFile(canonical);
      const contentType = MIME_TYPES[extname(canonical)] ?? "application/octet-stream";
      send(response, 200, contentType, body, method === "HEAD");
    })().catch((error) => {
      if (!response.headersSent) {
        send(response, 500, "text/plain; charset=utf-8", `server error: ${error.message}\n`);
      } else {
        response.destroy(error);
      }
    });
  });

  await new Promise((resolveListen, rejectListen) => {
    server.once("error", rejectListen);
    server.listen(0, "127.0.0.1", resolveListen);
  });
  server.requestLog = requestLog;
  server.completion = completion;
  return server;
}

function runProcess(command, args, timeoutMs = PROCESS_TIMEOUT_MS, stopWhen) {
  return new Promise((resolveProcess, rejectProcess) => {
    const child = spawn(command, args, { stdio: ["ignore", "pipe", "pipe"] });
    let stdout = "";
    let stderr = "";
    let timedOut = false;
    let excessiveOutput = false;
    let settled = false;
    let stopValue;
    let stopError;
    let stoppedForCompletion = false;

    const append = (current, chunk) => {
      const next = current + chunk;
      if (Buffer.byteLength(next) > MAX_PROCESS_OUTPUT_BYTES) {
        excessiveOutput = true;
        child.kill("SIGKILL");
      }
      return next;
    };
    child.stdout.setEncoding("utf8");
    child.stderr.setEncoding("utf8");
    child.stdout.on("data", (chunk) => { stdout = append(stdout, chunk); });
    child.stderr.on("data", (chunk) => { stderr = append(stderr, chunk); });

    const timer = setTimeout(() => {
      timedOut = true;
      child.kill("SIGKILL");
    }, timeoutMs);

    if (stopWhen) {
      void Promise.resolve(stopWhen).then(
        (value) => {
          stopValue = value;
          if (!settled) {
            stoppedForCompletion = true;
            child.kill("SIGKILL");
          }
        },
        (error) => {
          stopError = error;
          if (!settled) child.kill("SIGKILL");
        },
      );
    }

    child.once("error", (error) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      rejectProcess(error);
    });
    child.once("close", (code, signal) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      resolveProcess({
        code,
        signal,
        stdout,
        stderr,
        timedOut,
        excessiveOutput,
        stopValue,
        stopError,
        stoppedForCompletion,
      });
    });
  });
}

async function findBrowser() {
  const candidates = [
    process.env.CHROME_BIN,
    process.env.GOOGLE_CHROME_BIN,
    "google-chrome",
    "google-chrome-stable",
    "chromium",
    "chromium-browser",
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Chromium.app/Contents/MacOS/Chromium",
  ].filter(Boolean);

  for (const command of [...new Set(candidates)]) {
    try {
      const result = await runProcess(command, ["--version"], 5_000);
      if (result.code === 0) {
        return {
          command,
          version: (result.stdout || result.stderr).trim(),
        };
      }
    } catch (error) {
      if (error?.code !== "ENOENT" && error?.code !== "ENOTDIR") throw error;
    }
  }
  throw new Error(
    "browser qualification requires Google Chrome or Chromium; install one or set CHROME_BIN to its executable",
  );
}

const bundleRoot = await realpath(resolve(bundleArgument));
for (const path of [
  "manifest.json",
  "program.O",
  "program.plan.txt",
  "program.wasm",
  "runner.mjs",
  "wasi-preview1-host.mjs",
]) {
  await access(join(bundleRoot, path), fsConstants.R_OK);
}

const browser = await findBrowser();
const profileDirectory = await mkdtemp(join(tmpdir(), "olang-browser-qualification-"));
let server;

try {
  server = await createBundleServer(bundleRoot);
  const address = server.address();
  if (!address || typeof address === "string") {
    throw new Error("loopback browser-test server did not expose a TCP address");
  }
  const testUrl = new URL(TEST_PAGE, `http://127.0.0.1:${address.port}`);
  testUrl.searchParams.set("expected", expectedStdout);

  const result = await runProcess(browser.command, [
    "--headless",
    "--disable-background-networking",
    "--disable-component-update",
    "--disable-default-apps",
    "--disable-dev-shm-usage",
    "--disable-gpu",
    "--disable-sync",
    "--metrics-recording-only",
    "--no-default-browser-check",
    "--no-first-run",
    "--hide-scrollbars",
    "--mute-audio",
    `--user-data-dir=${profileDirectory}`,
    "--dump-dom",
    testUrl.href,
  ], PROCESS_TIMEOUT_MS, server.completion);
  if (result.timedOut) {
    throw new Error(
      `headless browser exceeded ${PROCESS_TIMEOUT_MS} ms; requests:\n${server.requestLog.join("\n")}\n${result.stderr}`,
    );
  }
  if (result.excessiveOutput) {
    throw new Error(`headless browser exceeded ${MAX_PROCESS_OUTPUT_BYTES} output bytes`);
  }
  if (result.stopError) {
    throw new Error(`browser completion signal failed: ${result.stopError}`);
  }
  if (!result.stopValue) {
    throw new Error(
      `headless browser exited before reporting completion (code ${result.code} signal ${result.signal ?? "none"}):\n${result.stderr}`,
    );
  }
  if (
    result.stopValue.status !== "pass"
    || result.stopValue.domStatus !== "pass"
    || result.stopValue.domMarker !== PASS_MARKER
  ) {
    throw new Error(
      `browser DOM did not contain the qualification marker: ${JSON.stringify(result.stopValue)}`,
    );
  }
  if (!result.stoppedForCompletion && result.code !== 0) {
    throw new Error(
      `headless browser exited with code ${result.code} signal ${result.signal ?? "none"}:\n${result.stderr}`,
    );
  }
  console.log(`${PASS_MARKER} (${browser.version})`);
} finally {
  if (server) {
    server.closeAllConnections?.();
    await new Promise((resolveClose) => server.close(resolveClose));
  }
  await rm(profileDirectory, { recursive: true, force: true, maxRetries: 3 });
}
