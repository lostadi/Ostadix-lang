#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
G2_ROOT="${OCORE_G2_EVIDENCE_DIR:-$ROOT/target/ocore-g2-aarch64}"
FIRST="$G2_ROOT/first"
SECOND="$G2_ROOT/second"
TRANSCRIPT="$G2_ROOT/qemu-transcript.log"
ARTIFACT_DIGESTS="$G2_ROOT/artifacts.sha256"
TRANSCRIPT_DIGEST="$G2_ROOT/transcript.sha256"
TIMEOUT_SECONDS="${OCORE_G2_TIMEOUT_SECONDS:-30}"

if ! command -v git >/dev/null 2>&1; then
  echo "error: git is required for the native AArch64 G2 attestation header" >&2
  exit 127
fi
SOURCE_COMMIT="$(git -C "$ROOT" rev-parse HEAD)"
printf '%s\n' \
  'WORLD_ALPHA_ATTESTATION_TRANSCRIPT_V1' \
  'gate=G2' \
  'evidence_class=qemu_tcg_aarch64' \
  "source_commit=$SOURCE_COMMIT" \
  'command=./ocore/kernel/smoke-aarch64-g2-qemu.sh'

for tool in cargo clang cmp git grep python3 qemu-system-aarch64 shasum; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "error: $tool is required for the native AArch64 G2 gate" >&2
    exit 127
  fi
done

mkdir -p "$FIRST" "$SECOND"

OCORE_G2_BUILD_DIR="$FIRST" "$ROOT/ocore/kernel/build-aarch64-g2.sh" >/dev/null
OCORE_G2_BUILD_DIR="$SECOND" "$ROOT/ocore/kernel/build-aarch64-g2.sh" >/dev/null

for artifact in kernel.o kernel.s boot.o vectors.o kernel.elf; do
  if ! cmp -s "$FIRST/$artifact" "$SECOND/$artifact"; then
    echo "error: AArch64 G2 rebuild is not deterministic for $artifact" >&2
    exit 1
  fi
done

# Parse ELF directly so this gate does not depend on the host's `file`, `nm`, or
# objdump understanding cross-architecture objects.  Both the O-core object and
# linked kernel must be ELF64 little-endian EM_AARCH64=183; the object must carry
# two distinct compiled .oc EL0 entry symbols plus the semantic dispatcher.
python3 - "$FIRST/kernel.o" "$FIRST/kernel.elf" <<'PY'
from pathlib import Path
import struct
import sys

obj_path, elf_path = map(Path, sys.argv[1:])

def header(path: Path) -> bytes:
    data = path.read_bytes()
    if len(data) < 64 or data[:4] != b"\x7fELF":
        raise SystemExit(f"{path}: not an ELF object")
    if data[4] != 2 or data[5] != 1:
        raise SystemExit(f"{path}: expected little-endian ELF64")
    machine = struct.unpack_from("<H", data, 18)[0]
    if machine != 183:
        raise SystemExit(f"{path}: expected EM_AARCH64=183, got {machine}")
    return data

obj = header(obj_path)
header(elf_path)

section_offset = struct.unpack_from("<Q", obj, 40)[0]
section_size = struct.unpack_from("<H", obj, 58)[0]
section_count = struct.unpack_from("<H", obj, 60)[0]
if section_size < 64 or section_count == 0:
    raise SystemExit("kernel.o: absent ELF section table")

sections = []
for index in range(section_count):
    offset = section_offset + index * section_size
    if offset + 64 > len(obj):
        raise SystemExit("kernel.o: truncated ELF section table")
    sections.append({
        "type": struct.unpack_from("<I", obj, offset + 4)[0],
        "offset": struct.unpack_from("<Q", obj, offset + 24)[0],
        "size": struct.unpack_from("<Q", obj, offset + 32)[0],
        "link": struct.unpack_from("<I", obj, offset + 40)[0],
        "entsize": struct.unpack_from("<Q", obj, offset + 56)[0],
    })

symbols = set()
for section in sections:
    if section["type"] != 2:  # SHT_SYMTAB
        continue
    if section["link"] >= len(sections) or section["entsize"] < 24:
        raise SystemExit("kernel.o: malformed symbol table")
    strings = sections[section["link"]]
    string_data = obj[strings["offset"]:strings["offset"] + strings["size"]]
    table = obj[section["offset"]:section["offset"] + section["size"]]
    for offset in range(0, len(table), section["entsize"]):
        if offset + 24 > len(table):
            break
        name_offset = struct.unpack_from("<I", table, offset)[0]
        if name_offset >= len(string_data):
            continue
        end = string_data.find(b"\0", name_offset)
        if end < 0:
            continue
        symbols.add(string_data[name_offset:end].decode("ascii", "strict"))

required = {"g2_user_a_entry", "g2_user_b_entry", "g2_kernel_svc"}
missing = sorted(required - symbols)
if missing:
    raise SystemExit(f"kernel.o: missing compiled O-core symbols: {missing}")
PY

# A hand-authored assembly transcript would not demonstrate O-core compilation.
# All positive native markers must originate in g2_kernel.oc and therefore may
# appear in compiler-generated kernel.s, but never in boot/vector source.
native_markers=(
  'G2 AArch64 resident EL2 HVC round-trip: PASS'
  'G2 AArch64 EL1 kernel: online'
  'G2 AArch64 real SVC/ERET path: PASS'
  'G2 AArch64 EL0 principal A: online'
  'G2 AArch64 forged capability: denied'
  'G2 AArch64 over-rights request: denied'
  'G2 AArch64 endpoint request queued: PASS'
  'G2 AArch64 EL0 principal B: online'
  'G2 AArch64 attenuated capability read: PASS'
  'G2 AArch64 attenuated capability write: denied'
  'G2 AArch64 endpoint reply queued: PASS'
  'G2 AArch64 EL0 fault contained: PASS'
  'G2 AArch64 process slot reuse stale denial: PASS'
  'G2 AArch64 endpoint request/reply: PASS'
  'G2 AArch64 capability slot reuse stale denial: PASS'
  'G2 AArch64 EL0 exit contained: PASS'
  'G2 AArch64 teardown and reclamation: PASS'
  'G2 AArch64 EL0 process lifecycle: PASS'
  'G2 AArch64 IPC capability lifecycle: PASS'
  'G2 AArch64 post-lifecycle counter progress: PASS'
)
for marker in "${native_markers[@]}"; do
  if grep -Fq "$marker" \
      "$ROOT/ocore/kernel/aarch64/boot.S" \
      "$ROOT/ocore/kernel/aarch64/vectors.S"; then
    echo "error: positive G2 marker is embedded in hand-written assembly: $marker" >&2
    exit 1
  fi
  if ! grep -Fq "$marker" "$ROOT/ocore/runtime/aarch64/g2_kernel.oc"; then
    echo "error: positive G2 marker is not owned by compiled O-core: $marker" >&2
    exit 1
  fi
done

(
  cd "$FIRST"
  shasum -a 256 kernel.o kernel.s boot.o vectors.o kernel.elf
) > "$ARTIFACT_DIGESTS"
read -r OBJECT_DIGEST _ < <(shasum -a 256 "$FIRST/kernel.o")
read -r ELF_DIGEST _ < <(shasum -a 256 "$FIRST/kernel.elf")

printf 'artifact:g2-kernel-object:sha256=%s\n' "$OBJECT_DIGEST"
printf 'artifact:g2-kernel-elf:sha256=%s\n' "$ELF_DIGEST"

echo 'G2 AArch64 ocorec object: PASS'
echo 'G2 AArch64 deterministic object/image rebuild: PASS'
echo 'G2 AArch64 ELF64 EM_AARCH64=183: PASS'

python3 - "$FIRST/kernel.elf" "$TRANSCRIPT" "$TIMEOUT_SECONDS" <<'PY'
from pathlib import Path
import os
import selectors
import subprocess
import sys
import time

kernel = sys.argv[1]
transcript_path = Path(sys.argv[2])
timeout = float(sys.argv[3])
command = [
    "qemu-system-aarch64",
    "-accel", "tcg",
    "-machine", "virt,virtualization=on,gic-version=3",
    "-cpu", "cortex-a57",
    "-smp", "1",
    "-m", "128M",
    "-kernel", kernel,
    "-nographic",
    "-no-reboot",
    "-no-shutdown",
]

process = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
selector = selectors.DefaultSelector()
assert process.stdout is not None
assert process.stderr is not None
selector.register(process.stdout, selectors.EVENT_READ, "stdout")
selector.register(process.stderr, selectors.EVENT_READ, "stderr")
raw = bytearray()
qemu_stderr = bytearray()
deadline = time.monotonic() + timeout
terminal = b"G2 AArch64 post-lifecycle counter progress: PASS\n"
terminal_seen = None
survived = False

while time.monotonic() < deadline:
    now = time.monotonic()
    if terminal_seen is not None and now - terminal_seen >= 0.5:
        survived = process.poll() is None
        break
    if process.poll() is not None:
        break
    for key, _ in selector.select(timeout=0.05):
        chunk = os.read(key.fd, 4096)
        if not chunk:
            selector.unregister(key.fileobj)
            continue
        if key.data == "stdout":
            raw.extend(chunk)
            if terminal_seen is None and terminal in raw.replace(b"\r\n", b"\n"):
                terminal_seen = time.monotonic()
        else:
            qemu_stderr.extend(chunk)

if process.poll() is None:
    process.terminate()
    try:
        process.wait(timeout=2)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=2)
stdout_remainder = process.stdout.read()
stderr_remainder = process.stderr.read()
if stdout_remainder:
    raw.extend(stdout_remainder)
if stderr_remainder:
    qemu_stderr.extend(stderr_remainder)
selector.close()
transcript_path.write_bytes(raw)
stderr_path = transcript_path.with_name("qemu-stderr.log")
stderr_path.write_bytes(qemu_stderr)

text = raw.decode("utf-8", "replace").replace("\r\n", "\n")
stderr_text = qemu_stderr.decode("utf-8", "replace").replace("\r\n", "\n")
markers = [
    "G2 AArch64 resident EL2 HVC round-trip: PASS\n",
    "G2 AArch64 EL1 kernel: online\n",
    "G2 AArch64 EL0 principal A: online\n",
    "G2 AArch64 real SVC/ERET path: PASS\n",
    "G2 AArch64 forged capability: denied\n",
    "G2 AArch64 over-rights request: denied\n",
    "G2 AArch64 endpoint request queued: PASS\n",
    "G2 AArch64 EL0 principal B: online\n",
    "G2 AArch64 attenuated capability read: PASS\n",
    "G2 AArch64 attenuated capability write: denied\n",
    "G2 AArch64 endpoint reply queued: PASS\n",
    "G2 AArch64 EL0 fault contained: PASS\n",
    "G2 AArch64 process slot reuse stale denial: PASS\n",
    "G2 AArch64 endpoint request/reply: PASS\n",
    "G2 AArch64 capability slot reuse stale denial: PASS\n",
    "G2 AArch64 EL0 exit contained: PASS\n",
    "G2 AArch64 teardown and reclamation: PASS\n",
    "G2 AArch64 EL0 process lifecycle: PASS\n",
    "G2 AArch64 IPC capability lifecycle: PASS\n",
    "G2 AArch64 post-lifecycle counter progress: PASS\n",
]
missing = [marker for marker in markers if marker not in text]
wrong_count = [marker for marker in markers if text.count(marker) != 1]
positions = [text.find(marker) for marker in markers]
forbidden = [
    marker for marker in (
        "G2 AArch64 native invariant failure",
        "G2 AArch64 unexpected exception",
        "panic",
        "LEAKED",
    ) if marker in text
]
unexpected_stderr = [
    line for line in stderr_text.splitlines()
    if "terminating on signal 15" not in line
]
if (
    missing
    or wrong_count
    or positions != sorted(positions)
    or forbidden
    or unexpected_stderr
    or not survived
):
    print("G2 AArch64 native compiler QEMU smoke: FAIL", file=sys.stderr)
    if missing:
        print("missing:", repr(missing), file=sys.stderr)
    if wrong_count:
        print("wrong marker count:", repr(wrong_count), file=sys.stderr)
    if positions != sorted(positions):
        print("native marker causal order is invalid", file=sys.stderr)
    if forbidden:
        print("forbidden:", repr(forbidden), file=sys.stderr)
    if unexpected_stderr:
        print("unexpected QEMU stderr:", repr(unexpected_stderr), file=sys.stderr)
    if not survived:
        print("QEMU did not survive the post-lifecycle observation window", file=sys.stderr)
    print("command:", repr(command), file=sys.stderr)
    print("transcript:", text, file=sys.stderr)
    raise SystemExit(1)

print(text, end="")
PY

(
  cd "$G2_ROOT"
  shasum -a 256 qemu-transcript.log
) > "$TRANSCRIPT_DIGEST"
printf '%s\n' \
  'G2 AArch64 artifact digests: artifacts.sha256 (ephemeral evidence directory)' \
  'G2 AArch64 serial transcript: qemu-transcript.log (ephemeral evidence directory)' \
  'G2 AArch64 serial transcript digest: transcript.sha256 (ephemeral evidence directory)'

# These normalized observations are validator inputs, not author-supplied
# claims.  Each line is emitted only after the live transcript, artifact, and
# causal-order checks above have succeeded.
printf '%s\n' \
  '@evidence event=aarch64_native_object format=elf64 machine=183 result=pass' \
  '@evidence event=el2_resident result=pass' \
  '@evidence event=el2_hvc_roundtrip domain=0x4f4d registers=preserved stack=preserved result=pass' \
  '@evidence event=el1_execution result=pass' \
  '@evidence event=el0_execution principals=2 result=pass' \
  '@evidence event=svc_eret_roundtrip result=pass' \
  '@evidence event=ipc_request_reply result=pass' \
  '@evidence event=capability_attenuation result=pass' \
  '@evidence event=stale_generation_rejected kinds=process,capability result=pass' \
  '@evidence event=lifecycle_terminal result=pass' \
  '@evidence event=reclamation result=pass' \
  '@evidence event=counter_progress phase=post_lifecycle poll_bound=1000000 result=pass'

# Exact boundary: this is single-vCPU QEMU TCG on the virt platform.  It is not
# physical AArch64/SMMU evidence, SMP, an MMU-isolation proof, a Linux/Plan 9
# kernel boot, hardware virtualization, PCI/DMA/IOMMU isolation, or a physical
# device assignment result.
echo 'G2 nonclaim: QEMU TCG virt is not physical AArch64, SMMU, SMP, or hardware-isolation evidence.'
echo 'G2 nonclaim: the bounded MMU-off EL0 corpus is not a general protected-memory or driver platform.'
echo 'G2 nonclaim: no Linux/Plan 9 kernel, KVM/SVM, PCI, DMA, IOMMU, or physical device is exercised.'
echo 'G2 AArch64 native compiler QEMU smoke: PASS'
