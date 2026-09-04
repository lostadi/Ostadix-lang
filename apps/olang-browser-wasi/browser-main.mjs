import { runOlangBrowserBundle } from "./runner.mjs";

const runButton = document.querySelector("#run");
const output = document.querySelector("#output");

runButton.addEventListener("click", async () => {
  runButton.disabled = true;
  document.documentElement.dataset.olangExecution = "running";
  output.textContent = "Running Olang WebAssembly...";
  let completion = { status: "error", exitCode: null };
  try {
    const result = await runOlangBrowserBundle();
    const status = result.ok && result.exitCode === 0 ? "success" : "failure";
    document.documentElement.dataset.olangExecution = status;
    output.textContent = [
      `status: ${status}`,
      `exit: ${result.exitCode}`,
      `stdout:\n${result.stdout || "<empty>"}`,
      `stderr:\n${result.stderr || "<empty>"}`,
    ].join("\n");
    completion = { status, exitCode: result.exitCode };
  } catch (error) {
    document.documentElement.dataset.olangExecution = "error";
    output.textContent = `${error.name}: ${error.message}`;
  } finally {
    runButton.disabled = false;
    document.dispatchEvent(new CustomEvent("olang-browser-run-complete", {
      detail: Object.freeze(completion),
    }));
  }
});
