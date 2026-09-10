#!/usr/bin/env python3
"""Observe two real O-core guests executing a provisioned dependent workload.

QEMU owns emulated Ethernet links. This harness never executes a task or
supplies a result to a guest. The optional relay only perturbs opaque frames.
Boot keys remain in process memory and are not written to evidence artifacts.
"""
from __future__ import annotations

import argparse
import hashlib
import hmac
import json
from pathlib import Path
import re
import secrets
import selectors
import socket
import struct
import subprocess
import tempfile
import threading
import time


def boot_config(node: int, nonce: bytes, key: bytes, initial: int, operands: list[int]) -> bytes:
    if node not in (1, 2) or len(nonce) != 32 or len(key) != 32:
        raise ValueError("invalid native session provisioning")
    if not 2 <= len(operands) <= 4 or any(not 0 <= x <= 0xFFFFFFFF for x in [initial, *operands]):
        raise ValueError("expected 2..4 u32 operands and one u32 initial value")
    config = b"OCNBOOT1" + struct.pack(">QQQQ", node, 3 - node, 1, 1) + nonce + key
    config += struct.pack(">QQQ", node, len(operands), initial)
    config += struct.pack(">4Q", *(operands + [0] * (4 - len(operands))))
    assert len(config) == 160
    return config


def result_observation(coordinator: str, worker: str, initial: int, operands: list[int], faults: bool) -> dict:
    complete = re.findall(r"NATIVE_CLUSTER COMPLETE tasks=(\d+) result=(\d+)\r?\n", coordinator)
    drained = re.findall(r"NATIVE_CLUSTER DRAINED executions=(\d+) duplicates=(\d+) rejected=(\d+)\r?\n", worker)
    if len(complete) != 1 or len(drained) != 1:
        raise ValueError("expected one native completion and one native drain observation")
    tasks, result = map(int, complete[0])
    executions, duplicates, rejected = map(int, drained[0])
    if tasks != len(operands) or executions != tasks or result != initial + sum(operands):
        raise ValueError("native result or execution count disagrees with provisioned graph")
    if faults and (duplicates < 1 or rejected < 1):
        raise ValueError("native replay or authentication rejection was not observed")
    if any(log.count("NATIVE_CLUSTER NIC_QUIESCED\n") != 1 for log in (coordinator.replace("\r", ""), worker.replace("\r", ""))):
        raise ValueError("both native NICs must quiesce before successful completion")
    if any(marker in coordinator + worker for marker in ("NATIVE_CLUSTER FAIL", "NATIVE_CLUSTER ERROR")):
        raise ValueError("native guest reported failure")
    if coordinator.replace("\r", "").count("NATIVE_CLUSTER DRAIN_ACK_CANONICAL_OK\n") != 1:
        raise ValueError("canonical drain acknowledgment checks were not observed")
    graph = hashlib.sha256(struct.pack(">6Q", tasks, initial, *(operands + [0] * (4 - tasks)))).hexdigest()
    for log in (coordinator, worker):
        if re.findall(r"NATIVE_CLUSTER GRAPH ([0-9a-f]{64})\r?\n", log) != [graph]:
            raise ValueError("both native nodes must bind the exact provisioned graph")
    observed_tasks = re.findall(r"NATIVE_CLUSTER TASK task=(\d+) input=([0-9a-f]{64}) output=([0-9a-f]{64}) result=(\d+)\r?\n", worker)
    expected_tasks = []
    accumulator, predecessor_digest = initial, bytes(32)
    for task, operand in enumerate(operands, 1):
        input_digest = hashlib.sha256(struct.pack(">4Q", 1, accumulator, operand, task - 1) + predecessor_digest).hexdigest()
        accumulator += operand
        predecessor_digest = hashlib.sha256(struct.pack(">Q", accumulator)).digest()
        expected_tasks.append((str(task), input_digest, predecessor_digest.hex(), str(accumulator)))
    if observed_tasks != expected_tasks:
        raise ValueError("native ordered dependency inputs/results do not match the provisioned graph")
    return dict(tasks=tasks, result=result, executions=executions, duplicates=duplicates, rejected=rejected,
                graph_sha256=graph, task_observations=[dict(task=int(n), input_sha256=i, output_sha256=o, result=int(v)) for n, i, o, v in observed_tasks])


class FramePerturbation:
    """Duplicate/corrupt a request and drop a result without computing replies."""
    def __init__(self, key: bytes | None = None, drop_results: int = 2):
        self.corrupted = self.duplicated = self.dropped = 0
        self.fenced = 0
        self.key = key
        self.drop_results = drop_results

    def apply(self, frame: bytes) -> list[bytes]:
        if len(frame) != 334 or frame[12:14] != b"\x88\xb5":
            return [frame]
        kind = int.from_bytes(frame[24:26], "big")
        if kind == 1 and not self.corrupted:
            damaged = bytearray(frame)
            damaged[-1] ^= 1
            self.corrupted = self.duplicated = 1
            challenges = []
            if self.key:
                # Positive HMACs with stale generation, nonce, graph,
                # out-of-order sequence and forged dependency respectively.
                for offset in (39, 48, 256, 87, 160):
                    challenge = bytearray(frame)
                    challenge[14 + offset] ^= 2
                    challenge[14 + 288:] = hmac.digest(self.key, challenge[14:14 + 288], "sha256")
                    challenges.append(bytes(challenge))
                self.fenced = len(challenges)
            return [bytes(damaged), *challenges, frame, frame]
        if kind == 2 and self.dropped < self.drop_results:
            self.dropped += 1
            return []
        return [frame]


def listener() -> socket.socket:
    server = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    server.bind(("127.0.0.1", 0))
    server.listen(1)
    server.settimeout(0.2)
    return server


class EthernetRelay:
    def __init__(self, key: bytes, drop_results: int):
        self.servers = [listener(), listener()]
        self.ports = [s.getsockname()[1] for s in self.servers]
        self.stop = threading.Event()
        self.frames = FramePerturbation(key, drop_results)
        self.errors: list[str] = []
        self.clients: list[socket.socket] = []
        self.thread = threading.Thread(target=self.run, daemon=True)
        self.thread.start()

    def run(self):
        try:
            for server in self.servers:
                while not self.stop.is_set():
                    try:
                        client, _ = server.accept()
                        client.settimeout(0.2)
                        self.clients.append(client)
                        break
                    except socket.timeout:
                        continue
            if len(self.clients) != 2:
                return
            buffers = [bytearray(), bytearray()]
            with selectors.DefaultSelector() as selector:
                for i, client in enumerate(self.clients):
                    selector.register(client, selectors.EVENT_READ, i)
                while not self.stop.is_set():
                    for item, _ in selector.select(0.2):
                        i = item.data
                        data = item.fileobj.recv(65536)
                        if not data:
                            return
                        buffers[i].extend(data)
                        while len(buffers[i]) >= 4:
                            size = struct.unpack_from(">I", buffers[i])[0]
                            if size > 65536:
                                raise ValueError("QEMU Ethernet frame exceeds relay bound")
                            if len(buffers[i]) < size + 4:
                                break
                            frame = bytes(buffers[i][4:4 + size])
                            del buffers[i][:4 + size]
                            for forwarded in self.frames.apply(frame):
                                self.clients[1 - i].sendall(struct.pack(">I", len(forwarded)) + forwarded)
        except (OSError, ValueError) as error:
            if not self.stop.is_set():
                self.errors.append(str(error))

    def close(self):
        self.stop.set()
        for sock in [*self.clients, *self.servers]:
            sock.close()
        self.thread.join(timeout=2)


def connect_serial(path: Path, process: subprocess.Popen, deadline: float) -> socket.socket:
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RuntimeError(f"QEMU exited before serial startup ({process.returncode})")
        client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        try:
            client.connect(str(path))
            client.settimeout(0.2)
            return client
        except OSError:
            client.close()
            time.sleep(0.02)
    raise TimeoutError("QEMU serial socket did not become ready")


def run_case(kernel: Path, out: Path, qemu: str, initial: int, operands: list[int], faults: bool, timeout: float, drop_results: int = 2, expected_failure: bool = False) -> dict:
    out.mkdir(parents=True, exist_ok=True)
    kernel_bytes = kernel.read_bytes()
    kernel_digest = hashlib.sha256(kernel_bytes).hexdigest()
    nonce, key = secrets.token_bytes(32), secrets.token_bytes(32)
    relay = EthernetRelay(key, drop_results) if faults else None
    reservation = listener() if not faults else None
    direct_port = reservation.getsockname()[1] if reservation else None
    if reservation:
        reservation.close()
    processes = []
    serials = []
    stderr_files = []
    logs = [bytearray(), bytearray()]
    deadline = time.monotonic() + timeout
    try:
        # Short paths keep Unix-domain sockets valid on macOS as well as Linux.
        with tempfile.TemporaryDirectory(prefix="ocnc-", dir="/tmp") as temporary:
            snapshot = Path(temporary) / "kernel.elf"
            snapshot.write_bytes(kernel_bytes)
            snapshot.chmod(0o400)
            # Node2 opens the direct link before node1 connects.
            for node in (2, 1):
                serial_path = Path(temporary) / f"serial{node}"
                if relay:
                    network = f"socket,id=world,connect=127.0.0.1:{relay.ports[node - 1]}"
                else:
                    direction = "listen" if node == 2 else "connect"
                    network = f"socket,id=world,{direction}=127.0.0.1:{direct_port}"
                command = [qemu, "-machine", "q35", "-accel", "tcg", "-m", "128M", "-smp", "1",
                           "-display", "none", "-monitor", "none", "-no-reboot", "-no-shutdown",
                           "-kernel", str(snapshot), "-chardev", f"socket,id=console,path={serial_path},server=on,wait=on",
                           "-serial", "chardev:console", "-netdev", network,
                           "-device", f"rtl8139,netdev=world,mac=52:54:00:12:34:{node:02x}"]
                error_file = (out / f"node{node}.stderr.log").open("wb")
                stderr_files.append(error_file)
                process = subprocess.Popen(command, stdout=subprocess.DEVNULL, stderr=error_file)
                processes.append(process)
                serial = connect_serial(serial_path, process, deadline)
                serials.append(serial)
                while b"NATIVE_CLUSTER BOOT_READY" not in logs[node - 1]:
                    if time.monotonic() >= deadline:
                        raise TimeoutError("native guest did not reach provisioning point")
                    try:
                        data = serial.recv(4096)
                    except socket.timeout:
                        continue
                    if not data:
                        raise RuntimeError("native serial closed before provisioning")
                    logs[node - 1].extend(data)
                serial.sendall(boot_config(node, nonce, key, initial, operands).hex().encode() + b"\n")
            with selectors.DefaultSelector() as selector:
                for node, serial in zip((2, 1), serials):
                    selector.register(serial, selectors.EVENT_READ, node - 1)
                while time.monotonic() < deadline:
                    for item, _ in selector.select(0.2):
                        data = item.fileobj.recv(4096)
                        if data:
                            logs[item.data].extend(data)
                        else:
                            selector.unregister(item.fileobj)
                    failed = any(marker in log for log in logs for marker in (b"NATIVE_CLUSTER FAIL", b"NATIVE_CLUSTER ERROR"))
                    if expected_failure and failed:
                        if any(b"NATIVE_CLUSTER NIC_ABORT_FAILED" in log for log in logs):
                            raise RuntimeError("native failure left device abort unverified")
                        if all(b"NATIVE_CLUSTER NIC_ABORTED\n" in log.replace(b"\r", b"") for log in logs):
                            break
                    elif failed:
                        raise RuntimeError("native cluster rejected the workload; inspect node transcripts")
                    if (re.search(rb"NATIVE_CLUSTER COMPLETE tasks=\d+ result=\d+\r?\n", logs[0])
                            and re.search(rb"NATIVE_CLUSTER DRAINED executions=\d+ duplicates=\d+ rejected=\d+\r?\n", logs[1])
                            and all(b"NATIVE_CLUSTER NIC_QUIESCED\n" in log.replace(b"\r", b"") for log in logs)):
                        break
                    if relay and relay.errors:
                        raise RuntimeError(f"Ethernet relay failed: {relay.errors}")
                else:
                    raise TimeoutError("native cluster did not complete and drain before timeout")
            if expected_failure:
                decoded = [log.decode("ascii", "replace").replace("\r", "") for log in logs]
                if (re.findall(r"NATIVE_CLUSTER ERROR (\d+)\n", decoded[0]) != ["21"]
                        or re.findall(r"NATIVE_CLUSTER ERROR (\d+)\n", decoded[1]) != ["32"]
                        or any("NATIVE_CLUSTER COMPLETE" in log or "NATIVE_CLUSTER NIC_QUIESCED" in log for log in decoded)
                        or any(log.count("NATIVE_CLUSTER NIC_ABORTED\n") != 1 for log in decoded)
                        or len(re.findall(r"NATIVE_CLUSTER TASK task=1 ", decoded[1])) != 1
                        or len(re.findall(r"NATIVE_CLUSTER TASK ", decoded[1])) != 1):
                    raise ValueError("partition must terminate with bounded failure and NIC abort, with one provisional execution")
                observation = dict(expected_failure=True, executions=1, coordinator_error=21, worker_error=32)
            else:
                observation = result_observation(*(log.decode("ascii", "replace") for log in logs), initial, operands, faults)
            if relay and not expected_failure and (relay.frames.corrupted, relay.frames.duplicated, relay.frames.dropped, relay.frames.fenced) != (1, 1, drop_results, 5):
                raise ValueError("fault injection did not exercise all intended Ethernet perturbations")
            if relay and not expected_failure and (observation["duplicates"] < drop_results or observation["rejected"] < 6):
                raise ValueError("native guest did not reject every challenged fence and retry the lost result")
            result = {"schema": "ostadix.native-cluster-observation/v1", "substrate": "two O-core QEMU TCG x86_64 guests",
                      "link": "opaque Ethernet perturbation relay" if faults else "direct QEMU socket Ethernet",
                      "kernel_sha256": kernel_digest,
                      "initial": initial, "operands": operands, **observation,
                      "dropped_results": relay.frames.dropped if relay else 0,
                      "nonclaims": ["physical multinode", "replicated Governor", "global durable commit", "ProjectBundle lowering", "G4 or G10 qualification"]}
            (out / "result.json").write_text(json.dumps(result, indent=2) + "\n")
            return result
    finally:
        for i, log in enumerate(logs, 1):
            (out / f"node{i}.serial.log").write_bytes(log)
        for serial in serials:
            serial.close()
        for process in processes:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=3)
        for error_file in stderr_files:
            error_file.close()
        if relay:
            relay.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("kernel", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--qemu", default="qemu-system-x86_64")
    parser.add_argument("--timeout", type=float, default=90)
    args = parser.parse_args()
    kernel = args.kernel.resolve(strict=True)
    cases = [("direct", 3, [7, 11, 13, 17], False, 0),
             ("zero", 0, [0, 0], False, 0),
             ("wide-retry", 0xFFFFFFFF, [0xFFFFFFFF, 1, 17], True, 2),
             ("ring-wrap-retry", 7, [11, 13], True, 28)]
    for name, initial, operands, faults, dropped in cases:
        result = run_case(kernel, args.output / name, args.qemu, initial, operands, faults, args.timeout, dropped)
        print(f"native-cluster {name}: PASS result={result['result']} executions={result['executions']}", flush=True)
    run_case(kernel, args.output / "partition-abort", args.qemu, 3, [7, 11], True, args.timeout, 1000000, True)
    print("native-cluster partition-abort: PASS bounded native failure and both NICs aborted", flush=True)


if __name__ == "__main__":
    main()
