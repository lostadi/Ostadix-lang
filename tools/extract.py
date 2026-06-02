#!/usr/bin/env python3
"""
extract.py

Losslessly extract prose + scripts/files from Markdown-ish, TXT, pasted-chat,
or raw artifact dumps.

Core rule:
    input extension does NOT determine parsing behavior.

So all of these are treated as text containers:
    .md
    .txt
    .log
    .jsonl
    .json
    extensionless text dumps
    renamed .txt files pretending to be .md

It extracts:
    1. Markdown/prose regions as ordered .md segment files
    2. fenced code blocks:
           ```python
           ...
           ```
    3. raw file-marker sections:
           o_parser.py
           <file contents>

           File: src/main.rs
           <file contents>

           ### backends/python_backend.py
           <file contents>

Outputs:
    extracted_segments/
      0000_original.<ext>
      reconstructed.md
      manifest.json
      segments/
        0001_markdown.md
        0002_o_parser.py
        ...
      production/
        o_parser.py
        backends/python_backend.py
        ...

Losslessness:
    reconstructed.md should hash-match the original input.

Usage:
    python3 extract.py dump.md
    python3 extract.py dump.txt
    python3 extract.py raw_transcript
    python3 extract.py folder_of_dumps/
    python3 extract.py folder_of_dumps/ --all
    python3 extract.py dump.md -o recovered
    python3 extract.py dump.md --debug
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import shlex
import shutil
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Iterable, Optional


# ─────────────────────────────────────────────────────────────────────────────
# supported input containers
# ─────────────────────────────────────────────────────────────────────────────

INPUT_TEXT_EXTS = {
    "",
    ".md",
    ".markdown",
    ".mdown",
    ".mkd",
    ".txt",
    ".text",
    ".log",
    ".dump",
    ".out",
    ".stdout",
    ".stderr",
    ".chat",
    ".transcript",
    ".json",
    ".jsonl",
}


# ─────────────────────────────────────────────────────────────────────────────
# supported output/source file types
# ─────────────────────────────────────────────────────────────────────────────

SUPPORTED_EXTS = {
    ".O",
    ".o",
    ".md",
    ".markdown",
    ".mdown",
    ".mkd",
    ".txt",
    ".text",
    ".py",
    ".rs",
    ".rkt",
    ".scm",
    ".lisp",
    ".cl",
    ".hy",
    ".c",
    ".h",
    ".cpp",
    ".cc",
    ".cxx",
    ".hpp",
    ".hh",
    ".java",
    ".kt",
    ".kts",
    ".go",
    ".js",
    ".jsx",
    ".ts",
    ".tsx",
    ".html",
    ".htm",
    ".css",
    ".scss",
    ".sh",
    ".bash",
    ".zsh",
    ".fish",
    ".ps1",
    ".json",
    ".jsonl",
    ".yaml",
    ".yml",
    ".toml",
    ".tml",
    ".xml",
    ".sql",
    ".tex",
    ".bib",
    ".nix",
    ".r",
    ".lua",
    ".rb",
    ".php",
    ".swift",
    ".m",
    ".mm",
    ".pl",
    ".pm",
    ".hs",
    ".lhs",
    ".ml",
    ".mli",
    ".fs",
    ".fsx",
    ".scala",
    ".dart",
    ".dockerfile",
    ".gitignore",
    ".env",
}

SPECIAL_FILENAMES = {
    "Cargo.toml": "toml",
    "Dockerfile": "dockerfile",
    "Makefile": "make",
    "makefile": "make",
    "README.md": "markdown",
    "README": "text",
    "SPEC.md": "markdown",
    "LICENSE": "text",
    ".gitignore": "gitignore",
    ".env": "env",
}

EXT_BY_LANG = {
    # O language
    "o": ".O",
    "O": ".O",
    "olang": ".O",
    "o-lang": ".O",
    "o_lang": ".O",

    # prose/document formats
    "markdown": ".md",
    "md": ".md",
    "text": ".txt",
    "txt": ".txt",
    "plain": ".txt",
    "latex": ".tex",
    "tex": ".tex",
    "bibtex": ".bib",
    "bib": ".bib",

    # lisps / symbolic langs
    "racket": ".rkt",
    "rkt": ".rkt",
    "scheme": ".scm",
    "scm": ".scm",
    "lisp": ".lisp",
    "common-lisp": ".lisp",
    "cl": ".lisp",
    "hy": ".hy",

    # common programming languages
    "python": ".py",
    "py": ".py",
    "rust": ".rs",
    "rs": ".rs",
    "c": ".c",
    "h": ".h",
    "cpp": ".cpp",
    "c++": ".cpp",
    "cc": ".cc",
    "cxx": ".cxx",
    "hpp": ".hpp",
    "java": ".java",
    "kotlin": ".kt",
    "kt": ".kt",
    "go": ".go",
    "javascript": ".js",
    "js": ".js",
    "jsx": ".jsx",
    "typescript": ".ts",
    "ts": ".ts",
    "tsx": ".tsx",
    "swift": ".swift",
    "ruby": ".rb",
    "rb": ".rb",
    "php": ".php",
    "lua": ".lua",
    "perl": ".pl",
    "pl": ".pl",
    "haskell": ".hs",
    "hs": ".hs",
    "ocaml": ".ml",
    "ml": ".ml",
    "scala": ".scala",
    "dart": ".dart",

    # shell/config/web
    "bash": ".sh",
    "sh": ".sh",
    "shell": ".sh",
    "zsh": ".zsh",
    "fish": ".fish",
    "powershell": ".ps1",
    "ps1": ".ps1",
    "html": ".html",
    "htm": ".html",
    "css": ".css",
    "scss": ".scss",
    "json": ".json",
    "jsonl": ".jsonl",
    "yaml": ".yaml",
    "yml": ".yml",
    "toml": ".toml",
    "tml": ".tml",
    "xml": ".xml",
    "sql": ".sql",
    "dockerfile": ".Dockerfile",
    "nix": ".nix",
    "make": ".mk",
    "gitignore": ".gitignore",
    "env": ".env",
}

LANG_BY_EXT = {
    ".O": "O",
    ".o": "O",
    ".md": "markdown",
    ".markdown": "markdown",
    ".mdown": "markdown",
    ".mkd": "markdown",
    ".txt": "text",
    ".text": "text",
    ".py": "python",
    ".rs": "rust",
    ".rkt": "racket",
    ".scm": "scheme",
    ".lisp": "lisp",
    ".cl": "lisp",
    ".hy": "hy",
    ".c": "c",
    ".h": "c",
    ".cpp": "cpp",
    ".cc": "cpp",
    ".cxx": "cpp",
    ".hpp": "cpp",
    ".hh": "cpp",
    ".java": "java",
    ".kt": "kotlin",
    ".kts": "kotlin",
    ".go": "go",
    ".js": "javascript",
    ".jsx": "javascript",
    ".ts": "typescript",
    ".tsx": "typescript",
    ".html": "html",
    ".htm": "html",
    ".css": "css",
    ".scss": "scss",
    ".sh": "bash",
    ".bash": "bash",
    ".zsh": "zsh",
    ".fish": "fish",
    ".ps1": "powershell",
    ".json": "json",
    ".jsonl": "jsonl",
    ".yaml": "yaml",
    ".yml": "yaml",
    ".toml": "toml",
    ".tml": "tml",
    ".xml": "xml",
    ".sql": "sql",
    ".tex": "latex",
    ".bib": "bibtex",
    ".nix": "nix",
    ".r": "r",
    ".lua": "lua",
    ".rb": "ruby",
    ".php": "php",
    ".swift": "swift",
    ".pl": "perl",
    ".pm": "perl",
    ".hs": "haskell",
    ".lhs": "haskell",
    ".ml": "ocaml",
    ".mli": "ocaml",
    ".scala": "scala",
    ".dart": "dart",
    ".dockerfile": "dockerfile",
    ".gitignore": "gitignore",
    ".env": "env",
}


# ─────────────────────────────────────────────────────────────────────────────
# regexes
# ─────────────────────────────────────────────────────────────────────────────

# permissive Markdown fence opener:
#   ```python
#   ~~~rust
#   > ```python
#   ```python filename=foo.py
OPEN_FENCE_RE = re.compile(
    r"^(?P<prefix>[ \t]*(?:>[ \t]*)*)"
    r"(?P<fence>`{3,}|~{3,})"
    r"(?P<info>[^\r\n]*)"
    r"(?P<newline>\r?\n?)$"
)

# strong file marker detector.
# matches:
#   o_parser.py
#   backends/python_backend.py
#   File: src/main.rs
#   ### Cargo.toml
#   - README.md
#   Presented file: o_value.py
#
# deliberately requires a real supported file extension or special filename.
FILE_MARKER_RE = re.compile(
    r"""
    ^[ \t]*
    (?:
        \#{1,6}[ \t]+ |
        [-*+][ \t]+ |
        \d+\.[ \t]+ |
        (?:
            File(?:[ \t]+\d+)? |
            Path |
            Filename |
            Name |
            Presented[ \t]+file |
            Create |
            Output |
            Saved |
            Wrote
        )
        [ \t]*[:\-][ \t]*
    )?
    [`"']?
    (?P<path>
        (?:
            [A-Za-z0-9_.+@=\-]+/
        )*
        [A-Za-z0-9_.+@=\-]+
    )
    [`"']?
    [ \t]*:?
    [ \t]*$
    """,
    re.VERBOSE,
)

# patterns that are probably terminal/log lines, not file markers.
REJECT_MARKER_PREFIXES = (
    "python ",
    "python3 ",
    "cargo ",
    "git ",
    "cd ",
    "rm ",
    "rmdir ",
    "ls ",
    "find ",
    "bat ",
    "cat ",
    "nano ",
    "vim ",
    "nvim ",
    "emacs ",
    "chmod ",
    "chown ",
    "wget ",
    "curl ",
    "rsync ",
    "cp ",
    "mv ",
    "mkdir ",
    "touch ",
    "grep ",
    "rg ",
    "sed ",
    "awk ",
    "./",
    "",
    "ustad@",
    "[main",
    "warning:",
    "error:",
    "fatal:",
    "hint:",
    "remote:",
    "to ",
    "from ",
)


# ─────────────────────────────────────────────────────────────────────────────
# data model
# ─────────────────────────────────────────────────────────────────────────────

@dataclass
class Segment:
    index: int
    kind: str
    file: str
    sha256: str
    byte_count: int
    language: Optional[str] = None
    source_path: Optional[str] = None
    production_path: Optional[str] = None
    info: Optional[str] = None
    opener: Optional[str] = None
    closer: Optional[str] = None
    closed: Optional[bool] = None
    extraction_mode: Optional[str] = None


# ─────────────────────────────────────────────────────────────────────────────
# basic IO / hashing
# ─────────────────────────────────────────────────────────────────────────────

def text_bytes(text: str) -> bytes:
    return text.encode("utf-8", errors="surrogateescape")


def sha256_text(text: str) -> str:
    return hashlib.sha256(text_bytes(text)).hexdigest()


def read_text(path: Path) -> str:
    with path.open("r", encoding="utf-8", errors="surrogateescape", newline="") as f:
        return f.read()


def write_text(path: Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8", errors="surrogateescape", newline="") as f:
        f.write(text)


def looks_binary(path: Path, sample_size: int = 8192) -> bool:
    try:
        with path.open("rb") as f:
            sample = f.read(sample_size)
    except OSError:
        return True

    return b"\x00" in sample


# ─────────────────────────────────────────────────────────────────────────────
# name/path/language inference
# ─────────────────────────────────────────────────────────────────────────────

def safe_path(path: str) -> str:
    """
    sanitize a relative path while preserving directory structure.
    prevents absolute paths and path traversal.
    """
    path = path.strip().strip("`'\"")
    path = path.replace("\\", "/")

    parts: list[str] = []

    for part in path.split("/"):
        part = part.strip()

        if not part or part in {".", ".."}:
            continue

        part = re.sub(r"[^A-Za-z0-9._+@=\-]+", "_", part)
        part = part.strip()

        if part:
            parts.append(part)

    return "/".join(parts) if parts else "unknown.txt"


def safe_flat_name(path: str) -> str:
    return safe_path(path).replace("/", "__")


def split_info(info: str) -> list[str]:
    info = info.strip()

    if not info:
        return []

    try:
        return shlex.split(info)
    except ValueError:
        return info.split()


def is_supported_path(path: str) -> bool:
    cleaned = safe_path(path)
    name = Path(cleaned).name

    if name in SPECIAL_FILENAMES:
        return True

    suffix = Path(cleaned).suffix

    if suffix == ".O":
        return True

    return suffix.lower() in {ext.lower() for ext in SUPPORTED_EXTS}


def language_from_path(path: str) -> str:
    cleaned = safe_path(path)
    name = Path(cleaned).name

    if name in SPECIAL_FILENAMES:
        return SPECIAL_FILENAMES[name]

    suffix = Path(cleaned).suffix

    if suffix == ".O":
        return "O"

    return LANG_BY_EXT.get(suffix.lower(), "text")


def requested_filename_from_info(info: str) -> Optional[str]:
    tokens = split_info(info)

    for token in tokens:
        raw = token.strip().strip("'\"")
        lowered = raw.lower()

        for prefix in ("filename=", "file=", "path=", "name="):
            if lowered.startswith(prefix):
                candidate = raw.split("=", 1)[1].strip().strip("'\"")
                if is_supported_path(candidate):
                    return safe_path(candidate)

    for token in tokens:
        raw = token.strip().strip("'\"")

        if is_supported_path(raw):
            return safe_path(raw)

    return None


def language_from_info(info: str) -> str:
    requested = requested_filename_from_info(info)

    if requested:
        return language_from_path(requested)

    tokens = split_info(info)

    if tokens:
        first = tokens[0].strip().strip("{}").strip("'\"")
        lowered = first.lower()

        if first == "O" or lowered in {"o", "olang", "o-lang", "o_lang"}:
            return "O"

        if lowered in EXT_BY_LANG:
            return lowered

        if first.startswith("."):
            fake = "x" + first
            return language_from_path(fake)

    return "text"


def ext_for_language(language: str) -> str:
    if language == "O":
        return ".O"

    return EXT_BY_LANG.get(language, ".txt")


# ─────────────────────────────────────────────────────────────────────────────
# fence / marker detection
# ─────────────────────────────────────────────────────────────────────────────

def is_open_fence(line: str) -> Optional[re.Match[str]]:
    return OPEN_FENCE_RE.match(line)


def is_close_fence(line: str, fence_char: str, fence_len: int) -> bool:
    stripped = line.rstrip("\r\n")

    pattern = (
        r"^[ \t]*(?:>[ \t]*)*"
        + re.escape(fence_char)
        + r"{"
        + str(fence_len)
        + r",}[ \t]*$"
    )

    return re.match(pattern, stripped) is not None


def detect_file_marker(line: str) -> Optional[str]:
    stripped = line.strip()

    if not stripped:
        return None

    lowered = stripped.lower()

    if lowered.startswith(REJECT_MARKER_PREFIXES):
        return None

    match = FILE_MARKER_RE.match(line)

    if not match:
        return None

    candidate = match.group("path").strip()

    if " " in candidate:
        return None

    if not is_supported_path(candidate):
        return None

    return safe_path(candidate)


# ─────────────────────────────────────────────────────────────────────────────
# segment writing
# ─────────────────────────────────────────────────────────────────────────────

def make_segment_filename(
    index: int,
    kind: str,
    language: str,
    source_path: Optional[str],
) -> str:
    if kind == "markdown":
        return f"{index:04d}_markdown.md"

    if source_path:
        return f"{index:04d}_{safe_flat_name(source_path)}"

    ext = ext_for_language(language)
    slug = "O" if language == "O" else re.sub(r"[^A-Za-z0-9_+\-]+", "_", language.lower())

    return f"{index:04d}_{slug}{ext}"


def unique_path(path: Path) -> Path:
    if not path.exists():
        return path

    parent = path.parent
    stem = path.stem
    suffix = path.suffix

    i = 2

    while True:
        candidate = parent / f"{stem}_{i}{suffix}"

        if not candidate.exists():
            return candidate

        i += 1


def add_segment(
    segments: list[Segment],
    out_dir: Path,
    index: int,
    kind: str,
    content: str,
    language: str,
    source_path: Optional[str] = None,
    info: Optional[str] = None,
    opener: Optional[str] = None,
    closer: Optional[str] = None,
    closed: Optional[bool] = None,
    extraction_mode: Optional[str] = None,
) -> int:
    if content == "":
        return index

    segments_dir = out_dir / "segments"
    production_dir = out_dir / "production"

    segment_file_name = make_segment_filename(index, kind, language, source_path)
    segment_rel = Path("segments") / segment_file_name
    segment_abs = out_dir / segment_rel

    write_text(segment_abs, content)

    production_path_str = None

    if kind == "code":
        if source_path:
            production_rel = Path(safe_path(source_path))
        else:
            production_rel = Path(segment_file_name)

        production_abs = unique_path(production_dir / production_rel)
        write_text(production_abs, content)
        production_path_str = str(production_abs.relative_to(production_dir))

    segments.append(
        Segment(
            index=index,
            kind=kind,
            file=str(segment_rel),
            language=language,
            source_path=source_path,
            production_path=production_path_str,
            info=info,
            opener=opener,
            closer=closer,
            closed=closed,
            extraction_mode=extraction_mode,
            sha256=sha256_text(content),
            byte_count=len(text_bytes(content)),
        )
    )

    return index + 1


# ─────────────────────────────────────────────────────────────────────────────
# splitting logic
# ─────────────────────────────────────────────────────────────────────────────

def split_source(source: str, out_dir: Path, debug: bool = False) -> list[Segment]:
    """
    Split source into:
      - markdown prose segments
      - fenced code segments
      - raw file-marker code segments

    Reconstruction preserves opener/marker lines exactly.
    Production files contain only the recovered file body.
    """
    lines = source.splitlines(keepends=True)

    segments: list[Segment] = []
    markdown_buf: list[str] = []

    index = 1
    i = 0

    while i < len(lines):
        line = lines[i]

        fence_match = is_open_fence(line)
        file_marker = detect_file_marker(line)

        # fenced code block mode
        if fence_match:
            index = add_segment(
                segments=segments,
                out_dir=out_dir,
                index=index,
                kind="markdown",
                content="".join(markdown_buf),
                language="markdown",
                extraction_mode="markdown-prose",
            )
            markdown_buf = []

            opener = line
            fence = fence_match.group("fence")
            fence_char = fence[0]
            fence_len = len(fence)
            info = fence_match.group("info").strip()

            source_path = requested_filename_from_info(info)
            language = language_from_info(info)

            if debug:
                print(f"DEBUG fence line {i + 1}: language={language} path={source_path!r}")

            i += 1
            code_buf: list[str] = []
            closer: Optional[str] = None

            while i < len(lines):
                candidate = lines[i]

                if is_close_fence(candidate, fence_char, fence_len):
                    closer = candidate
                    i += 1
                    break

                code_buf.append(candidate)
                i += 1

            index = add_segment(
                segments=segments,
                out_dir=out_dir,
                index=index,
                kind="code",
                content="".join(code_buf),
                language=language,
                source_path=source_path,
                info=info,
                opener=opener,
                closer=closer,
                closed=closer is not None,
                extraction_mode="fenced-code",
            )

            continue

        # raw file-marker mode
        if file_marker:
            index = add_segment(
                segments=segments,
                out_dir=out_dir,
                index=index,
                kind="markdown",
                content="".join(markdown_buf),
                language="markdown",
                extraction_mode="markdown-prose",
            )
            markdown_buf = []

            marker_line = line
            source_path = file_marker
            language = language_from_path(source_path)

            if debug:
                print(f"DEBUG marker line {i + 1}: language={language} path={source_path}")

            i += 1
            code_buf = []

            while i < len(lines):
                next_line = lines[i]

                if is_open_fence(next_line) or detect_file_marker(next_line):
                    break

                code_buf.append(next_line)
                i += 1

            index = add_segment(
                segments=segments,
                out_dir=out_dir,
                index=index,
                kind="code",
                content="".join(code_buf),
                language=language,
                source_path=source_path,
                opener=marker_line,
                closer=None,
                closed=None,
                extraction_mode="raw-file-marker",
            )

            continue

        # ordinary prose/markdown/text mode
        markdown_buf.append(line)
        i += 1

    add_segment(
        segments=segments,
        out_dir=out_dir,
        index=index,
        kind="markdown",
        content="".join(markdown_buf),
        language="markdown",
        extraction_mode="markdown-prose",
    )

    return segments


def reconstruct(out_dir: Path, segments: list[Segment]) -> str:
    pieces: list[str] = []

    for seg in segments:
        body = read_text(out_dir / seg.file)

        if seg.kind == "markdown":
            pieces.append(body)
            continue

        if seg.kind == "code":
            if seg.opener:
                pieces.append(seg.opener)

            pieces.append(body)

            if seg.closer:
                pieces.append(seg.closer)

            continue

        raise ValueError(f"unknown segment kind: {seg.kind}")

    return "".join(pieces)


# ─────────────────────────────────────────────────────────────────────────────
# diagnostics
# ─────────────────────────────────────────────────────────────────────────────

def count_fenceish(source: str) -> int:
    return sum(
        1
        for line in source.splitlines()
        if "```" in line or "~~~" in line
    )


def count_markerish(source: str) -> int:
    return sum(
        1
        for line in source.splitlines(keepends=True)
        if detect_file_marker(line)
    )


def summarize_segments(segments: list[Segment]) -> dict[str, int]:
    counts: dict[str, int] = {}

    for seg in segments:
        key = f"{seg.kind}:{seg.language or 'unknown'}"
        counts[key] = counts.get(key, 0) + 1

    return counts


# ─────────────────────────────────────────────────────────────────────────────
# extraction entry
# ─────────────────────────────────────────────────────────────────────────────

def extract(input_path: Path, out_dir: Path, debug: bool = False, clean: bool = True) -> None:
    if looks_binary(input_path):
        raise ValueError(f"input appears to be binary, not text: {input_path}")

    source = read_text(input_path)

    if clean and out_dir.exists():
        shutil.rmtree(out_dir)

    out_dir.mkdir(parents=True, exist_ok=True)

    original_suffix = input_path.suffix or ".txt"
    original_copy = out_dir / f"0000_original{original_suffix}"
    shutil.copyfile(input_path, original_copy)

    segments = split_source(source, out_dir, debug=debug)

    reconstructed = reconstruct(out_dir, segments)
    write_text(out_dir / "reconstructed.md", reconstructed)

    original_hash = sha256_text(source)
    reconstructed_hash = sha256_text(reconstructed)
    lossless = original_hash == reconstructed_hash

    code_count = sum(1 for seg in segments if seg.kind == "code")
    markdown_count = sum(1 for seg in segments if seg.kind == "markdown")
    fenceish = count_fenceish(source)
    markerish = count_markerish(source)

    manifest = {
        "source": str(input_path),
        "original_copy": original_copy.name,
        "reconstructed": "reconstructed.md",
        "original_sha256": original_hash,
        "reconstructed_sha256": reconstructed_hash,
        "lossless": lossless,
        "byte_count": len(text_bytes(source)),
        "segment_count": len(segments),
        "markdown_segment_count": markdown_count,
        "code_segment_count": code_count,
        "fenceish_line_count": fenceish,
        "markerish_line_count": markerish,
        "segment_summary": summarize_segments(segments),
        "segments": [asdict(seg) for seg in segments],
    }

    write_text(
        out_dir / "manifest.json",
        json.dumps(manifest, indent=2, ensure_ascii=False) + "\n",
    )

    print(f"input:            {input_path}")
    print(f"output:           {out_dir}")
    print(f"bytes:            {len(text_bytes(source))}")
    print(f"segments:         {len(segments)}")
    print(f"markdown:         {markdown_count}")
    print(f"code/scripts:     {code_count}")
    print(f"fence-ish lines:  {fenceish}")
    print(f"marker-ish lines: {markerish}")
    print(f"lossless:         {lossless}")
    print()

    if code_count == 0:
        print("No scripts were detected.")
        print("Useful diagnostics:")
        print(f"  grep -nE '```|~~~|<code|<pre' {input_path}")
        print(
            "  grep -nE '(^|[ /])([A-Za-z0-9_.+-]+)"
            "\\.(py|rs|rkt|toml|md|O|html|sh|json|yaml|yml|tex)\\b' "
            f"{input_path}"
        )
        print()

    if not lossless:
        print("WARNING: reconstructed.md does not hash-match the original input.")
        print("The extracted production files may still be useful, but reconstruction is not exact.")
        print()

    for seg in segments:
        prod = f" -> production/{seg.production_path}" if seg.production_path else ""
        print(f"{seg.index:04d}  {seg.kind:<8} {seg.language or '':<12} {seg.file}{prod}")


# ─────────────────────────────────────────────────────────────────────────────
# directory handling
# ─────────────────────────────────────────────────────────────────────────────

def iter_inputs(path: Path, include_all: bool = False) -> Iterable[Path]:
    if path.is_file():
        yield path
        return

    if not path.is_dir():
        raise FileNotFoundError(path)

    for child in sorted(path.rglob("*")):
        if not child.is_file():
            continue

        if include_all:
            yield child
            continue

        if child.suffix.lower() in INPUT_TEXT_EXTS:
            yield child


def output_dir_for_input(base_out: Path, input_file: Path, root: Path) -> Path:
    try:
        rel = input_file.relative_to(root)
    except ValueError:
        rel = Path(input_file.name)

    rel_no_suffix = rel.with_suffix("")
    safe_parts = [safe_path(part) for part in rel_no_suffix.parts]

    return base_out.joinpath(*safe_parts)


# ─────────────────────────────────────────────────────────────────────────────
# CLI
# ─────────────────────────────────────────────────────────────────────────────

def main() -> None:
    parser = argparse.ArgumentParser(
        description=(
            "Losslessly extract Markdown/TXT prose plus fenced/unfenced scripts "
            "from raw text, pasted chats, and artifact dumps."
        )
    )

    parser.add_argument(
        "input",
        help="input file or directory; .md and .txt are parsed identically",
    )

    parser.add_argument(
        "-o",
        "--out",
        default="extracted_segments",
        help="output directory, default: extracted_segments",
    )

    parser.add_argument(
        "--debug",
        action="store_true",
        help="print detected fences/file markers",
    )

    parser.add_argument(
        "--all",
        action="store_true",
        help="when input is a directory, try every file regardless of extension",
    )

    parser.add_argument(
        "--no-clean",
        action="store_true",
        help="do not delete the output directory before extraction",
    )

    args = parser.parse_args()

    input_path = Path(args.input).expanduser().resolve()
    base_out = Path(args.out).expanduser().resolve()

    if not input_path.exists():
        raise FileNotFoundError(input_path)

    files = list(iter_inputs(input_path, include_all=args.all))

    if not files:
        print(f"no input text files found under: {input_path}")
        return

    multi = input_path.is_dir()

    for file_path in files:
        if multi:
            out_dir = output_dir_for_input(base_out, file_path, input_path)
        else:
            out_dir = base_out

        print()
        print("=" * 80)
        print(f"extracting: {file_path}")
        print("=" * 80)

        try:
            extract(
                input_path=file_path,
                out_dir=out_dir,
                debug=args.debug,
                clean=not args.no_clean,
            )
        except UnicodeDecodeError as e:
            print(f"skipped non-text file: {file_path}")
            print(f"reason: {e}")
        except ValueError as e:
            print(f"skipped: {file_path}")
            print(f"reason: {e}")
        except Exception as e:
            print(f"failed: {file_path}")
            print(f"{type(e).__name__}: {e}")


if __name__ == "__main__":
    main()
