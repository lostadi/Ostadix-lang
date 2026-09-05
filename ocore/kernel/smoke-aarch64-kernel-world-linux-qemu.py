#!/usr/bin/env python3
"""Boot a genuine pinned Linux Image below the bounded O-core EL2 monitor."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import selectors
import shutil
import signal
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[2]
MARKERS = [
    "KW AArch64 EL2 monitor: online",
    "KW native virtio negative selftests: PASS",
    "KW stage2 guest-only RAM: online",
    "Linux version 6.12.43-ostadix-kernelworld",
    "KW LINUX SERVICE HEALTHY",
    "KW Linux block pattern and service health observed: PASS",
    "KW virtio pending read withdrawal: begun",
    "KW LINUX IOERR CONSUMED",
    "KW LINUX POST-WITHDRAWAL ALIVE",
    "KW virtio guest-consumed IOERR completion: PASS",
    "KW guest RAM stage2 mappings withdrawn: PASS",
    "KW real Linux revoked-access stage2 fault contained: PASS",
    "KW post-Linux EL2 counter progress: PASS",
]
FORBIDDEN = ["KW terminal unexpected exit:", "KW Linux boot payload header invalid", "Kernel panic", "KW LINUX FAIL"]


def identity(path):
    path = Path(path).resolve(strict=True)
    data = path.read_bytes()
    return {"path": str(path), "bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()}


def snapshot_inputs(inputs, directory):
    """Copy each source once, then bind evidence and QEMU to those exact bytes."""
    result = {}
    for name, source in inputs.items():
        source = Path(source).resolve(strict=True)
        data = source.read_bytes()
        snapshot = directory / name
        with snapshot.open("xb") as output:
            output.write(data)
            output.flush()
            os.fsync(output.fileno())
        snapshot.chmod(0o400)
        result[name] = {"source_path": str(source), "snapshot_path": str(snapshot),
                        "bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()}
    return result


def complete_lines(text):
    return [line.removesuffix("\r") for line in text.split("\n")[:-1]]


def validate_transcript(text):
    lines = complete_lines(text)
    positions = []
    for marker in MARKERS:
        matches = [index for index, line in enumerate(lines)
                   if line == marker or (marker.startswith("Linux version ") and line.startswith(marker + " "))]
        if len(matches) != 1:
            return False
        positions.append(matches[0])
    fault = revoked_access(text)
    return (positions == sorted(positions) and not any(marker in text for marker in FORBIDDEN)
            and fault is not None
            and positions[10] < fault["line"] < positions[11])


def revoked_access(text):
    records = [(index, re.fullmatch(r"KW revoked access esr/ipa/pc ([0-9a-f]{16}) ([0-9a-f]{16}) ([0-9a-f]{16})", line))
               for index, line in enumerate(complete_lines(text)) if line.startswith("KW revoked access")]
    if len(records) != 1 or records[0][1] is None:
        return None
    index, match = records[0]
    esr, ipa, pc = (int(value, 16) for value in match.groups())
    if esr >> 26 not in (0x20, 0x24) or esr & 0x3c != 4:
        return None
    if not 0x40000000 <= ipa < 0x60000000 or pc == 0 or pc & 3:
        return None
    return {"esr": esr, "ipa": ipa, "pc": pc, "line": index}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--payload-dir", type=Path, default=ROOT / "target/ocore-real-linux/payload")
    parser.add_argument("--build-dir", type=Path, default=ROOT / "target/ocore-real-linux/monitor")
    parser.add_argument("--output-dir", type=Path, default=ROOT / "target/ocore-real-linux/evidence")
    parser.add_argument("--qemu", default="qemu-system-aarch64")
    parser.add_argument("--timeout", type=float, default=120)
    args = parser.parse_args()
    if not 0 < args.timeout <= 600:
        parser.error("timeout must be within (0, 600] seconds")
    qemu = shutil.which(args.qemu)
    if not qemu:
        parser.error("qemu-system-aarch64 is required")
    payload = args.payload_dir.resolve(strict=True)
    build = args.build_dir.resolve(strict=True)
    manifest = json.loads((payload / "linux-payload.json").read_text())
    if manifest["linux_version"] != "6.12.43" or manifest["source_modifications"] is not False:
        parser.error("expected unmodified pinned upstream Linux 6.12.43")
    if manifest["source_archive_sha256"] != "0fcbbbbcd456e87bbbfc8bf37af541fda62ccfcce76903503424fd101ef7bdee":
        parser.error("unexpected pinned Linux source archive")
    args.output_dir.mkdir(parents=True, exist_ok=True)
    # Private snapshots keep concurrent rebuilds from changing admitted inputs
    # between digest inspection and QEMU's later loader-file opens.
    with tempfile.TemporaryDirectory(prefix="linux-run-", dir=args.output_dir.resolve()) as temporary:
        snapshots = Path(temporary)
        artifacts = snapshot_inputs({
            "Image": payload / "Image", "initramfs": payload / "initramfs.cpio",
            "linux_config": payload / "linux.config", "monitor": build / "monitor.elf",
            "guest_dtb": build / "guest.dtb",
        }, snapshots)
        artifacts["qemu"] = identity(qemu)
        return run(args, parser, qemu, manifest, artifacts)


def run(args, parser, qemu, manifest, artifacts):
    for name, key in (("Image", "Image"), ("linux_config", "linux.config")):
        expected = manifest["artifacts"][key]
        if any(artifacts[name][field] != expected[field] for field in ("bytes", "sha256")):
            parser.error(f"{name} differs from the built payload manifest")
    command = [qemu, "-machine", "virt,virtualization=on,gic-version=2", "-accel", "tcg,thread=single",
               "-cpu", "cortex-a57", "-smp", "1", "-m", "1G", "-nodefaults", "-nic", "none",
               "-display", "none", "-monitor", "none", "-serial", "stdio", "-no-reboot", "-no-shutdown",
               "-kernel", artifacts["monitor"]["snapshot_path"],
               "-device", f"loader,file={artifacts['Image']['snapshot_path']},addr=0x48000000,force-raw=on",
               "-device", f"loader,file={artifacts['initramfs']['snapshot_path']},addr=0x50000000,force-raw=on",
               "-device", f"loader,file={artifacts['guest_dtb']['snapshot_path']},addr=0x4fe00000,force-raw=on"]
    started = time.monotonic()
    process = subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                               stderr=subprocess.STDOUT, start_new_session=True, bufsize=0)
    capture = bytearray()
    selector = selectors.DefaultSelector()
    selector.register(process.stdout, selectors.EVENT_READ)
    reason = "timeout"
    completed_at = None
    try:
        while time.monotonic() - started < args.timeout:
            for key, _ in selector.select(timeout=0.1):
                chunk = os.read(key.fd, 16384)
                if not chunk:
                    selector.unregister(key.fileobj)
                    continue
                capture.extend(chunk)
            if len(capture) > 2 * 1024 * 1024:
                reason = "capture capacity exceeded"
                break
            text = capture.decode("utf-8", "replace").replace("\r", "")
            if any(marker in text for marker in FORBIDDEN):
                reason = "terminal failure marker"
                break
            if process.poll() is not None:
                reason = f"QEMU exited {process.returncode}"
                break
            if MARKERS[-1] in complete_lines(text):
                if completed_at is None:
                    completed_at = time.monotonic()
                elif time.monotonic() - completed_at >= 0.2:
                    reason = "completed"
                    break
    finally:
        selector.close()
        if process.poll() is None:
            os.killpg(process.pid, signal.SIGTERM)
            try:
                process.wait(timeout=2)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait(timeout=2)
        # Never wait on a pipe inherited by a surviving helper after QEMU's
        # direct process exits. Preserve already-buffered diagnostics only.
        os.set_blocking(process.stdout.fileno(), False)
        try:
            remainder = os.read(process.stdout.fileno(), 65536)
        except BlockingIOError:
            remainder = b""
        capture.extend(remainder)
        process.stdout.close()
    text = capture.decode("utf-8", "replace").replace("\r", "")
    passed = reason == "completed" and validate_transcript(text)
    (args.output_dir / "console.log").write_text(text)
    result = {"schema": "ostadix.kernel-world-real-linux-bringup/v1", "passed": passed, "reason": reason,
              "elapsed_seconds": time.monotonic() - started, "artifacts": artifacts,
              "linux_source": manifest, "qemu_version": subprocess.check_output([qemu, "--version"], text=True).splitlines()[0],
              "revoked_access": revoked_access(text),
              "command": command, "observed_markers": [m for m in MARKERS if m in text],
              "scope": "one real Linux guest under O-core EL2 stage2 with an emulated virtio block endpoint",
              "nonclaims": ["G7 passage", "separate host EL1 broker boundary", "physical hardware", "DMA/IOMMU isolation",
                            "distributed consensus", "signed authority receipts", "physical frame reuse after teardown"]}
    (args.output_dir / "result.json").write_text(json.dumps(result, indent=2) + "\n")
    print(text, end="")
    print(f"O-core real Linux bring-up: {'PASS' if passed else 'FAIL'} ({reason})")
    return 0 if passed else 1


if __name__ == "__main__":
    raise SystemExit(main())
