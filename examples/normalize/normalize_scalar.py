#!/usr/bin/env python3
"""Reference scalar realization for the normalize operation example."""

import json
import sys
from pathlib import Path


def normalize(values: list[float]) -> list[float]:
    maximum = max(values)
    if maximum == 0.0:
        raise ValueError("cannot normalize by zero")
    return [value / maximum for value in values]


values = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
print(json.dumps({"values": normalize(values)}, separators=(",", ":"), sort_keys=True))
