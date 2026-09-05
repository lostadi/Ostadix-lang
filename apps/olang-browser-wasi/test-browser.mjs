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
import { delimiter, extname, join, resolve, sep } from "node:path";
import { launchBrowser } from "./browser-process.mjs";

const PASS_MARKER = "OSTADIX_BROWSER_WASI_DOM_PASS_V1";
const TEST_PAGE = "/__olang_browser_qualification__.html";
const DONE_RESOURCE = "/__olang_browser_qualification_done__";
// A cold browser has a separate deadline from the unchanged UI execution budget.
const STARTUP_TIMEOUT_MS = 60_000;
const EXECUTION_TIMEOUT_MS = 30_000;

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
    <script>
    // Register during parsing; DOMContentLoaded waits for the shipped module and
    // its dependencies, even when this inline script arrives before they do.
    document.addEventListener("DOMContentLoaded", async () => {
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
    }, { once: true });
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

  for (const candidate of [...new Set(candidates)]) {
    const paths = candidate.includes(sep)
      ? [candidate]
      : (process.env.PATH ?? "").split(delimiter).map((directory) => join(directory, candidate));
    for (const command of paths) {
      try {
        await access(command, fsConstants.X_OK);
        if ((await stat(command)).isFile()) return command;
      } catch (error) {
        if (!["ENOENT", "ENOTDIR", "EACCES"].includes(error?.code)) throw error;
      }
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
let browserProcess;
let interruption;
const interrupted = (signal) => {
  interruption = new Error(`browser qualification interrupted by ${signal}`);
  browserProcess?.abort(interruption);
};
process.on("SIGINT", interrupted);
process.on("SIGTERM", interrupted);

try {
  server = await createBundleServer(bundleRoot);
  const address = server.address();
  if (!address || typeof address === "string") {
    throw new Error("loopback browser-test server did not expose a TCP address");
  }
  const testUrl = new URL(TEST_PAGE, `http://127.0.0.1:${address.port}`);
  testUrl.searchParams.set("expected", expectedStdout);

  browserProcess = launchBrowser(browser, [
    "--headless",
    "--remote-debugging-pipe",
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
    "--password-store=basic",
    "--use-mock-keychain",
    `--user-data-dir=${profileDirectory}`,
    "about:blank",
  ]);
  if (interruption) browserProcess.abort(interruption);
  const started = Date.now();
  const { version, sessionId } = await browserProcess.phase("browser startup/readiness", STARTUP_TIMEOUT_MS, async () => {
    const version = await browserProcess.send("Browser.getVersion");
    const { targetId } = await browserProcess.send("Target.createTarget", { url: "about:blank" });
    const { sessionId } = await browserProcess.send("Target.attachToTarget", { targetId, flatten: true });
    await browserProcess.send("Page.enable", {}, sessionId);
    return { version, sessionId };
  });
  const startupMs = Date.now() - started;
  const executionStarted = Date.now();
  await browserProcess.phase("browser navigation/UI execution", EXECUTION_TIMEOUT_MS, async () => {
    const navigation = await browserProcess.send("Page.navigate", { url: testUrl.href }, sessionId);
    if (navigation.errorText) throw new Error(`browser navigation failed: ${navigation.errorText}`);
    const completion = await server.completion;
    if (
      completion.status !== "pass"
      || completion.domStatus !== "pass"
      || completion.domMarker !== PASS_MARKER
    ) {
      throw new Error(`browser UI qualification failed: ${JSON.stringify(completion)}`);
    }
    // Read the actual DOM independently after the UI sends its completion signal.
    const dom = await browserProcess.send("Runtime.evaluate", {
      expression: `({ status: document.documentElement.getAttribute("data-olang-browser-qualification"),
        execution: document.documentElement.dataset.olangExecution,
        marker: document.querySelector("#output")?.textContent })`,
      returnByValue: true,
    }, sessionId);
    const state = dom.result?.value;
    if (dom.exceptionDetails || state?.status !== "pass" || state?.execution !== "success" || state?.marker !== PASS_MARKER) {
      throw new Error(`browser DOM did not confirm successful execution: ${JSON.stringify(dom)}`);
    }
  });
  console.log(`${PASS_MARKER} (${version.product}; startup=${startupMs}ms execution=${Date.now() - executionStarted}ms)`);
} catch (error) {
  throw new Error(`${error.message}; requests:\n${server?.requestLog.join("\n") ?? ""}\n${browserProcess?.stderr ?? ""}`, { cause: error });
} finally {
  try {
    try {
      await browserProcess?.close();
    } finally {
      if (server) {
        server.closeAllConnections?.();
        await new Promise((resolveClose) => server.close(resolveClose));
      }
      await rm(profileDirectory, { recursive: true, force: true, maxRetries: 3 });
    }
  } finally {
    process.off("SIGINT", interrupted);
    process.off("SIGTERM", interrupted);
  }
}
