#!/usr/bin/env python3
"""Chunked local realization for the normalize operation example."""

import json
import sys
from pathlib import Path


def normalize(values: list[float], width: int = 2) -> list[float]:
    maximum = max(values)
    if maximum == 0.0:
        raise ValueError("cannot normalize by zero")
    chunks = (values[index : index + width] for index in range(0, len(values), width))
    return [value / maximum for chunk in chunks for value in chunk]


values = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
print(json.dumps({"values": normalize(values)}, separators=(",", ":"), sort_keys=True))
