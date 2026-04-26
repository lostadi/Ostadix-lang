#!/usr/bin/env python3
import re
import sys
from pathlib import Path

if len(sys.argv) < 2:
    print("usage: extract-md-files.py INPUT.md [OUTDIR]")
    sys.exit(1)

src = Path(sys.argv[1])
outdir = Path(sys.argv[2]) if len(sys.argv) > 2 else Path("reconstructed_from_markdown")

text = src.read_text(encoding="utf-8", errors="ignore")
outdir.mkdir(parents=True, exist_ok=True)

lang_ext = {
    "text": "txt",
    "bash": "sh",
    "sh": "sh",
    "shell": "sh",
    "fish": "fish",
    "python": "py",
    "py": "py",
    "nix": "nix",
    "rust": "rs",
    "rs": "rs",
    "toml": "toml",
    "json": "json",
    "yaml": "yml",
    "yml": "yml",
    "lisp": "lisp",
    "common-lisp": "lisp",
    "scheme": "scm",
    "racket": "rkt",
}

fence_re = re.compile(
    r"(?ms)^```([A-Za-z0-9_+\-]*)[^\n]*\n(.*?)\n```"
)

count = 0

for m in fence_re.finditer(text):
    lang = (m.group(1) or "text").strip().lower()
    code = m.group(2).rstrip() + "\n"

    ext = lang_ext.get(lang, "txt")
    count += 1

    target = outdir / "blocks" / f"block-{count:03d}.{ext}"
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(code, encoding="utf-8")

    print(f"wrote {target}")

print(f"\nextracted files: {count}")
