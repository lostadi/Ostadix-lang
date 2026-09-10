#!/usr/bin/env python3
"""Run the supplemental native scalar/unsigned-receipt differential gate."""

from __future__ import annotations

import json
import os
from pathlib import Path
import re
import selectors
import subprocess
import sys
import time

READY = "World native scalar probe: ready\n"
REJECT = "World native scalar probe: REJECT before execution\n"
POST_TICK = "World native scalar post-test timer: online\n"
BODY = "WORLD_NATIVE_SCALAR_UNSIGNED_RECEIPT_HEX="
EXECUTED = "World native scalar actual execution count=1\n"


def run_case(kernel: Path, request: bytes, success: bool, timeout: float):
    command = [
        "qemu-system-x86_64", "-machine", "q35", "-accel", "tcg",
        "-m", "128M", "-kernel", str(kernel), "-display", "none",
        "-monitor", "none", "-serial", "stdio", "-no-reboot", "-no-shutdown",
    ]
    process = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                               stderr=subprocess.PIPE, bufsize=0)
    selector = selectors.DefaultSelector()
    selector.register(process.stdout, selectors.EVENT_READ, "stdout")
    selector.register(process.stderr, selectors.EVENT_READ, "stderr")
    output = {"stdout": bytearray(), "stderr": bytearray()}
    sent = False
    survived = False
    deadline = time.monotonic() + timeout
    target = POST_TICK if success else REJECT
    try:
        while time.monotonic() < deadline:
            for key, _ in selector.select(timeout=0.05):
                chunk = os.read(key.fileobj.fileno(), 4096)
                if not chunk:
                    selector.unregister(key.fileobj)
                else:
                    output[key.data].extend(chunk)
            if sum(map(len, output.values())) > 1024 * 1024:
                raise RuntimeError("native scalar probe exceeded diagnostic bound")
            text = output["stdout"].decode("utf-8", "replace").replace("\r\n", "\n")
            if not sent and READY in text:
                process.stdin.write(request.hex().encode("ascii") + b"\n")
                process.stdin.flush()
                sent = True
            if sent and (target in text or (success and REJECT in text)):
                survived = process.poll() is None
                break
            if process.poll() is not None:
                break
    finally:
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=2)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
        for name, stream in (("stdout", process.stdout), ("stderr", process.stderr)):
            output[name].extend(stream.read())
        selector.close()
    return {name: data.decode("utf-8", "replace").replace("\r\n", "\n")
            for name, data in output.items()}, sent, survived


def require(condition: bool, message: object) -> None:
    if not condition:
        raise RuntimeError(str(message))


def verify_frame_ceiling(assembly: Path) -> None:
    lines = assembly.read_text().splitlines()
    frames = {}
    for index, line in enumerate(lines):
        name = line.strip().removesuffix(":")
        if not line.endswith(":") or not name.startswith((
            "_O_world__native_scalar__", "_O_kernel__world_native_scalar_semantics__",
        )):
            continue
        frame = 0
        for instruction in lines[index + 1:index + 9]:
            match = re.fullmatch(r"\s*sub rsp, ([0-9]+)", instruction)
            if match:
                frame = int(match.group(1))
                break
        frames[name] = frame
    require("_O_world__native_scalar__execute" in frames, "missing native execute assembly")
    require("_O_world__native_scalar__provision" in frames, "missing native provision assembly")
    require(all(frame <= 8192 for frame in frames.values()), frames)
    print(f"World native scalar generated-frame ceiling: PASS (largest={max(frames.values())}, ceiling=8192)")


def main() -> None:
    kernel, fixture_path, results_path, transcript_directory = map(Path, sys.argv[1:5])
    verify_frame_ceiling(kernel.with_suffix(".s"))
    cases = json.loads(fixture_path.read_text())
    transcript_directory.mkdir(parents=True, exist_ok=True)
    required = [
        READY,
        "World native scalar stale context before execution: PASS\n",
        "World native scalar unknown opcode before execution: PASS\n",
        "World native scalar receipt capacity before execution: PASS\n",
        EXECUTED,
        "World native scalar unsigned receipt native validation: PASS\n",
        "World native scalar duplicate attempt execution count=1: PASS\n",
        "World native scalar boundary: trusted fixture context; unsigned and uncommitted; no project or Governor authority\n",
        "World native scalar probe: PASS\n",
        POST_TICK,
    ]
    returned = []
    for index, case in enumerate(cases):
        output, sent, survived = run_case(kernel, bytes.fromhex(case["request"]), True, 15)
        transcript_directory.joinpath(f"success-{index}.log").write_text(
            output["stdout"] + "\n" + output["stderr"])
        text = output["stdout"]
        require(sent and survived, output)
        require(all(text.count(marker) == 1 for marker in required), output)
        require([text.index(marker) for marker in required] == sorted(
            text.index(marker) for marker in required), output)
        require(REJECT not in text, output)
        bindings = re.findall(r"(?m)^WORLD_NATIVE_SCALAR_CONTEXT_SHA256=([0-9a-f]{64})$", text)
        bodies = re.findall(r"(?m)^" + BODY + r"([0-9a-f]+)$", text)
        require(bindings == [case["context_sha256"]], output)
        require(bodies == [case["expected_unsigned"]], output)
        returned.append(bodies[0])
    valid = bytes.fromhex(cases[1]["request"])
    negative = {
        "stale-context": valid[:8] + bytes([valid[8] ^ 1]) + valid[9:],
        "wrong-magic": b"X" + valid[1:],
        "unknown-opcode": valid[:40] + (2).to_bytes(8, "big") + valid[48:],
        "operand-range": valid[:48] + (2**32).to_bytes(8, "big") + valid[56:],
        "truncated": valid[:-1],
        "trailing": valid + b"\0",
    }
    for name, request in negative.items():
        output, sent, survived = run_case(kernel, request, False, 15)
        transcript_directory.joinpath(f"reject-{name}.log").write_text(
            output["stdout"] + "\n" + output["stderr"])
        text = output["stdout"]
        require(sent and survived and text.count(REJECT) == 1, output)
        require(EXECUTED not in text and BODY not in text and POST_TICK not in text, output)
    results_path.write_text(json.dumps(returned, indent=2) + "\n")
    print(f"World native scalar native execution/receipt differential: PASS ({len(cases)} results, {len(negative)} pre-execution rejections)")


if __name__ == "__main__":
    main()
