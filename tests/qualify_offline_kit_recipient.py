#!/usr/bin/env python3
"""Build a tiny native-host kit and exercise its Python 3.10 recipient path."""

from __future__ import annotations

import argparse
from pathlib import Path
import shutil
import subprocess
import sys

PROJECT_ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(PROJECT_ROOT))

from tests.test_offline_kit import OfflineKitTests


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--recipient-python", required=True)
    parser.add_argument("--expected-host", required=True)
    parser.add_argument("--work-root", required=True)
    arguments = parser.parse_args()

    if sys.version_info < (3, 11):
        parser.error("fixture construction requires Python 3.11 or newer")
    recipient_python = Path(arguments.recipient_python).resolve(strict=True)
    work_root = Path(arguments.work_root).resolve(strict=False)
    work_root.mkdir(parents=True, exist_ok=False)
    archive = work_root / "offline-fixture.zip"
    extracted = work_root / "extracted"

    case = OfflineKitTests("test_build_is_deterministic_and_manifest_is_exact")
    case.setUp()
    try:
        if case.host != arguments.expected_host:
            raise RuntimeError(
                f"runner host mismatch: expected={arguments.expected_host} actual={case.host}"
            )
        shutil.copy2(case._build(), archive)
        kit_root = case._extract_generated_zip(archive, extracted)
    finally:
        case.tearDown()

    command = PROJECT_ROOT / "scripts/build_offline_kit.py"
    subprocess.run(
        [recipient_python, command, "verify", archive], check=True
    )
    extraction = kit_root / ".offline"
    for _attempt in range(2):
        subprocess.run(
            [
                recipient_python,
                command,
                "extract",
                "--kit-root",
                kit_root,
                "--destination",
                extraction,
            ],
            check=True,
        )
    print(
        "offline-kit recipient qualification: PASS "
        f"host={arguments.expected_host} recipient={recipient_python}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
