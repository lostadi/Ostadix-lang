import re
import os
import json
import hashlib
import logging
import argparse
import shutil
from pathlib import Path
from typing import Dict, Any, Optional

# Setup basic logging to replace silent 'pass' statements
logging.basicConfig(level=logging.INFO, format='%(levelname)s: %(message)s')
logger = logging.getLogger(__name__)

# Explicit priority scoring for conflict resolution
SCORE_JSON_WRITE = 10
SCORE_MAIN_STREAM = 8
SCORE_FALLBACK = 0

class ExtractionMethod:
    JSON_WRITE = "json_write"
    JSON_READ = "json_read"
    MARKDOWN_CODE = "markdown_code"
    MAIN_STREAM = "main_stream"

def get_hash(content: str) -> str:
    return hashlib.md5(content.encode('utf-8', errors='ignore')).hexdigest()

class CuratedReconstructor:
    def __init__(self, output_root: str | Path):
        self.output_root = Path(output_root).resolve()
        self.production_files: Dict[str, Dict[str, Any]] = {}
        self.history_fragments: Dict[str, Dict[str, Any]] = {}

    def sanitize_path(self, raw_path: str) -> Optional[str]:
        """
        Strips absolute roots, standardizes separators, and removes 
        user-specific directories generically to yield a clean relative path.
        """
        if not raw_path:
            return None
            
        p = Path(raw_path)
        parts = list(p.parts)

        # Drop absolute root (e.g., '/' or 'C:\')
        if p.is_absolute():
            parts = parts[1:]

        # Generically strip common user directory prefixes
        if len(parts) >= 2 and parts[0] in ('home', 'Users'):
            parts = parts[2:]
        elif len(parts) >= 1 and parts[0] == '~':
            parts = parts[1:]

        # Filter traversal attacks and empty parts
        safe_parts = [part for part in parts if part not in ('..', '.', '')]
        
        if not safe_parts:
            return None
            
        return Path(*safe_parts).as_posix()

    def clean_content(self, content: Any) -> str:
        """
        Safely removes LLM-generated line numbers only if the pattern 
        is consistently applied across the block, avoiding data destruction.
        """
        if not isinstance(content, str) or not content.strip():
            return ""
            
        lines = content.split('\n')
        non_empty_lines = [l for l in lines if l.strip()]
        
        if not non_empty_lines:
            return ""

        # Strict regex: requires a number, punctuation, and a space
        numbering_pattern = re.compile(r"^\s*\d+[\.:]\s(.*)$")
        
        cleaned = []
        is_numbered = True
        for line in lines:
            if not line.strip():
                cleaned.append("")
                continue
                
            m = numbering_pattern.match(line)
            if m:
                cleaned.append(m.group(1))
            else:
                is_numbered = False
                break

        final_content = "\n".join(cleaned) if is_numbered else content
        return final_content.strip() + "\n"

    def add_record(self, target_dict: dict, path: str, content: str, method: str, score: int = SCORE_FALLBACK):
        sanitized_path = self.sanitize_path(path)
        if not sanitized_path: return
        
        # Ignore operational log files globally
        if sanitized_path.startswith('chronicle/'): return 
        
        content = self.clean_content(content)
        if len(content) < 20: return
        
        h = get_hash(content)
        if sanitized_path not in target_dict:
            target_dict[sanitized_path] = {}
            
        existing = target_dict[sanitized_path].get(h)
        if not existing or existing.get('score', -1) < score:
            target_dict[sanitized_path][h] = {
                'content': content, 
                'score': score, 
                'method': method
            }

    def process_val(self, val: Any, depth: int = 0):
        if depth > 20: 
            logger.warning("Max recursion depth exceeded in JSON traversal.")
            return

        if isinstance(val, dict):
            tn = val.get('toolName') or val.get('name')
            
            # 1. Parse File Writes
            if tn in ['write_file', 'apply_patch', 'replace']:
                args = val.get('toolArgs') or val.get('arguments') or {}
                if isinstance(args, str):
                    try: args = json.loads(args)
                    except json.JSONDecodeError: args = {}
                
                if isinstance(args, dict):
                    p = args.get('file_path') or args.get('path')
                    c = args.get('content') or args.get('new_string')
                    if p and c: 
                        self.add_record(self.production_files, p, c, ExtractionMethod.JSON_WRITE, SCORE_JSON_WRITE)
            
            # 2. Parse File Reads
            if tn == 'read_file':
                res = val.get('result', {})
                if isinstance(res, dict):
                    c = res.get('content') or res.get('output')
                    args = val.get('toolArgs') or val.get('arguments') or {}
                    if isinstance(args, str):
                        try: args = json.loads(args)
                        except json.JSONDecodeError: args = {}
                        
                    if isinstance(args, dict):
                        p = args.get('file_path') or args.get('path')
                        if p and c: 
                            self.add_record(self.history_fragments, p, c, ExtractionMethod.JSON_READ)

            # 3. Mine General Strings for Markdown Blocks
            for k, v in val.items():
                if isinstance(v, str):
                    for m in re.finditer(r"```(?:\w+)?\n(.*?)\n
```", v, re.DOTALL):
                        block = m.group(1)
                        # Extract implied path from the first two lines of the block
                        for line in block.split('\n')[:2]:
                            pm = re.search(r"(?:#|//|;;)\s+([~/\w\./\-_]+\.\w{1,10})", line)
                            if pm: 
                                self.add_record(self.history_fragments, pm.group(1), block, ExtractionMethod.MARKDOWN_CODE)
                
                self.process_val(v, depth + 1)
                
        elif isinstance(val, list):
            for item in val: 
                self.process_val(item, depth + 1)

    def run(self, dump_path: str):
        logger.info(f"Mining {dump_path}...")
        
        # Regex tolerates standard dashes, em-dashes, and corrupted unicode equivalents
        header_re = re.compile(r"^#\s+([~/\w\./\-_]+\.\w{1,10})(?:\s+(?:-|—|‚Äî)\s+.*)?$")
        
        current_plain_file = None
        current_plain_content = []

        with open(dump_path, 'r', encoding='utf-8', errors='replace') as f:
            for line in f:
                l_strip = line.strip()
                
                # Check for Markdown-style file headers
                m = header_re.match(l_strip)
                if m:
                    if current_plain_file: 
                        self.add_record(self.production_files, current_plain_file, "".join(current_plain_content), ExtractionMethod.MAIN_STREAM, SCORE_MAIN_STREAM)
                    current_plain_file = m.group(1)
                    current_plain_content = []
                    continue
                
                # Attempt inline JSON parsing
                if l_strip.startswith('{') and l_strip.endswith('}'):
                    try: 
                        self.process_val(json.loads(l_strip))
                    except json.JSONDecodeError: 
                        pass # Valid to pass here if the log format mixes text and JSON unpredictably
                
                # Accumulate plaintext stream
                if current_plain_file: 
                    current_plain_content.append(line)

            # Flush remaining buffer
            if current_plain_file: 
                self.add_record(self.production_files, current_plain_file, "".join(current_plain_content), ExtractionMethod.MAIN_STREAM, SCORE_MAIN_STREAM)

    def _write_safely(self, base_dir: Path, rel_path: str, content: str):
        """Ensures filesystem writes cannot escape the bounded root directory."""
        target_path = (base_dir / rel_path).resolve()
        if not target_path.is_relative_to(base_dir):
            logger.error(f"Path traversal blocked for: {rel_path}")
            return
            
        target_path.parent.mkdir(parents=True, exist_ok=True)
        with open(target_path, 'w', encoding='utf-8') as out:
            out.write(content)

    def finalize(self):
        logger.info("Finalizing reconstruction...")
        
        # 1. Output Production Matrix
        prod_root = self.output_root / "production"
        prod_root.mkdir(parents=True, exist_ok=True)
        
        for path, versions in self.production_files.items():
            best_hash = max(versions.keys(), key=lambda h: versions[h]['score'])
            self._write_safely(prod_root, path, versions[best_hash]['content'])

        # 2. Output History Matrix
        hist_root = self.output_root / "history"
        hist_root.mkdir(parents=True, exist_ok=True)
        
        for path, versions in self.history_fragments.items():
            for i, (h, data) in enumerate(versions.items()):
                suffix = f".v{i+1}" if len(versions) > 1 else ""
                self._write_safely(hist_root, f"{path}{suffix}", data['content'])

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="Curated Reconstructor for LLM traces.")
    parser.add_argument("input", help="Path to the dump/log file.")
    parser.add_argument("--out", default="reconstructed_curated", help="Output directory root.")
    parser.add_argument("--force", action="store_true", help="Overwrite existing output directory.")
    args = parser.parse_args()

    out_dir = Path(args.out)
    if out_dir.exists():
        if args.force:
            shutil.rmtree(out_dir)
        else:
            logger.warning(f"Output directory {out_dir} exists. Use --force to overwrite.")
            exit(1)

    cr = CuratedReconstructor(out_dir)
    cr.run(args.input)
    cr.finalize()
    
    print("\nRECOVERY COMPLETE")
    print(f"Production tracks: {len(cr.production_files)}")
    print(f"Unique history fragments: {len(cr.history_fragments)}")