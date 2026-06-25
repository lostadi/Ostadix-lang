#!/usr/bin/env python3
"""Shared helpers for backend shims.

Backend stdout is an O boundary. A script that prints a scalar or JSON value
should hand that value back to O as data, while arbitrary text remains text.
Languages can print a tagged OValue JSON envelope for exact control.
"""

import json
import math
import re
import struct


INT64_MIN = -(2**63)
INT64_MAX = 2**63 - 1
INT_RE = re.compile(r"^[+-]?\d+$")
FLOAT_RE = re.compile(
    r"^[+-]?(?:(?:\d+\.\d*)|(?:\.\d+)|(?:\d+[eE][+-]?\d+)|(?:\d+\.\d*[eE][+-]?\d+))$"
)


def trim_stdout(output):
    """Drop the command-style trailing newline without changing other text."""
    if output.endswith("\n"):
        output = output[:-1]
        if output.endswith("\r"):
            output = output[:-1]
    return output


def int_to_oval(value):
    if INT64_MIN <= value <= INT64_MAX:
        return {"t": "int", "v": value}
    return {"t": "number", "v": {"kind": "int", "v": str(value)}}


def float_to_oval(value):
    if math.isfinite(value):
        return {"t": "float", "v": value}
    return {
        "t": "number",
        "v": {
            "kind": "binary_float",
            "format": "f64",
            "bits": list(struct.pack(">d", value)),
        },
    }


def json_value_to_oval(value):
    if value is None:
        return {"t": "null"}
    if isinstance(value, bool):
        return {"t": "bool", "v": value}
    if isinstance(value, int):
        return int_to_oval(value)
    if isinstance(value, float):
        return float_to_oval(value)
    if isinstance(value, str):
        return {"t": "str", "v": value}
    if isinstance(value, list):
        return {"t": "list", "v": [json_value_to_oval(item) for item in value]}
    if isinstance(value, dict):
        if isinstance(value.get("t"), str):
            return value
        return {
            "t": "map",
            "v": {str(key): json_value_to_oval(item) for key, item in value.items()},
        }
    return {"t": "str", "v": str(value)}


def stdout_to_oval(output):
    text = trim_stdout(output)
    stripped = text.strip()

    if stripped:
        try:
            return json_value_to_oval(json.loads(stripped))
        except Exception:
            pass

        if INT_RE.match(stripped):
            return int_to_oval(int(stripped))

        if FLOAT_RE.match(stripped):
            try:
                return float_to_oval(float(stripped))
            except Exception:
                pass

    return {"t": "str", "v": text}


def stdout_result(output):
    return stdout_to_oval(output)
