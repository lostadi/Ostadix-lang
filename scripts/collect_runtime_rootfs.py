#!/usr/bin/env python3
"""Collect a declared Linux runtime closure without executing its ELF files.

The trusted host readelf and ldconfig tools inspect dependencies. Runtime data,
dlopen modules, configuration, and service dependencies remain explicit inputs.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shlex
import shutil
import stat
import subprocess
import sys


SCHEMA = "ostadix.runtime-rootfs-closure/v1"
RESERVED = {".ostadix", ".old-root", "proc", "dev", "tmp", "work", "run", "runtime.json"}
DEFAULT_LIBRARY_DIRS = (
    "/lib", "/usr/lib", "/lib64", "/usr/lib64",
    "/lib/x86_64-linux-gnu", "/usr/lib/x86_64-linux-gnu",
    "/lib/aarch64-linux-gnu", "/usr/lib/aarch64-linux-gnu",
    "/lib/arm-linux-gnueabihf", "/usr/lib/arm-linux-gnueabihf",
    "/lib/i386-linux-gnu", "/usr/lib/i386-linux-gnu",
)


class CollectionError(RuntimeError):
    pass


def absolute_path(value):
    if not isinstance(value, str) or not value.startswith("/") or "\0" in value:
        raise CollectionError(f"expected an absolute runtime path: {value!r}")
    return PurePosixPath(os.path.normpath("/" + value.lstrip("/")))


def parse_readelf(output):
    def field(name):
        match = re.search(rf"^\s*{name}:\s*(.+)$", output, re.MULTILINE)
        if not match:
            raise CollectionError(f"readelf omitted ELF {name}")
        return match.group(1).strip()

    interpreter = re.search(r"\[Requesting program interpreter:\s*(.*?)\]", output)
    needed = re.findall(r"\(NEEDED\).*?\[(.*?)\]", output)
    rpath = re.findall(r"\(RPATH\).*?\[(.*?)\]", output)
    runpath = re.findall(r"\(RUNPATH\).*?\[(.*?)\]", output)
    return {
        "class": field("Class"), "machine": field("Machine"),
        "interpreter": interpreter.group(1) if interpreter else None,
        "needed": needed,
        "rpath": ":".join(rpath).split(":") if rpath else [],
        "runpath": ":".join(runpath).split(":") if runpath else [],
        "nodeflib": bool(re.search(r"\(FLAGS_1\).*\bNODEFLIB\b", output)),
    }


def load_library_cache(run=subprocess.run):
    tool = shutil.which("ldconfig")
    if tool is None:
        tool = next((path for path in ("/sbin/ldconfig", "/usr/sbin/ldconfig")
                     if os.path.isfile(path) and os.access(path, os.X_OK)), None)
    if tool is None:
        return {}
    result = run([tool, "-p"], check=True, capture_output=True, text=True,
                 env={**os.environ, "LC_ALL": "C"})
    cache = {}
    for line in result.stdout.splitlines():
        match = re.match(r"\s*(\S+)\s+\([^)]*\)\s+=>\s+(/\S+)\s*$", line)
        if match:
            cache.setdefault(match.group(1), []).append(match.group(2))
    return cache


class Collector:
    """source_root/run injection is for filesystem/parser tests, not a CLI mode."""

    def __init__(self, output, readelf, cache, source_root=Path("/"), run=subprocess.run):
        self.output = Path(output)
        self.source_root = Path(source_root)
        self.readelf = readelf
        self.cache = cache
        self.run = run
        self.files = {}
        self.directories = {}
        self.links = {}
        self.elf = {}
        self.scanned = set()
        self.dependencies = []
        self.abi = None
        self.commands = {}
        self.cache_used = False

    def physical(self, path):
        return self.source_root.joinpath(*path.parts[1:])

    def resolve(self, path):
        """Resolve source links in its filesystem namespace, with a hop bound."""
        pending = list(absolute_path(str(path)).parts[1:])
        resolved = PurePosixPath("/")
        hops = 0
        while pending:
            component = pending.pop(0)
            if component == ".":
                continue
            if component == "..":
                resolved = resolved.parent
                continue
            candidate = resolved / component
            try:
                metadata = self.physical(candidate).lstat()
            except OSError as error:
                raise CollectionError(f"missing source path {candidate}: {error}") from error
            if stat.S_ISLNK(metadata.st_mode):
                hops += 1
                if hops > 40:
                    raise CollectionError(f"symlink cycle or excessive link depth: {path}")
                target = PurePosixPath(os.readlink(self.physical(candidate)))
                if target.is_absolute():
                    resolved = PurePosixPath("/")
                    target_parts = target.parts[1:]
                else:
                    target_parts = target.parts
                pending = list(target_parts) + pending
            else:
                resolved = candidate
        return resolved

    def destination(self, path):
        path = absolute_path(str(path))
        if path == PurePosixPath("/") or path.parts[1] in RESERVED:
            raise CollectionError(f"runtime path collides with launcher-reserved root: {path}")
        return self.output.joinpath(*path.parts[1:])

    def ensure_parents(self, path):
        for parent in reversed(path.parents):
            if parent == PurePosixPath("/"):
                continue
            destination = self.destination(parent)
            destination.mkdir(exist_ok=True)
            if str(parent) not in self.directories:
                try:
                    source = self.resolve(parent)
                    mode = stat.S_IMODE(self.physical(source).stat().st_mode)
                except CollectionError:
                    mode = 0o755  # Synthetic /bin command alias directory.
                self.directories[str(parent)] = mode & 0o777

    def link(self, path, target):
        if path == target:
            return
        destination = self.destination(path)
        self.ensure_parents(path)
        relative = os.path.relpath(self.destination(target), destination.parent)
        if destination.is_symlink():
            if os.readlink(destination) != relative:
                raise CollectionError(f"conflicting runtime link: {path}")
        elif destination.exists():
            raise CollectionError(f"command alias conflicts with a copied runtime file: {path}")
        else:
            destination.symlink_to(relative)
        self.links[str(path)] = relative

    def inspect(self, path):
        path = self.resolve(path)
        key = str(path)
        if key not in self.elf:
            with self.physical(path).open("rb") as stream:
                if stream.read(4) != b"\x7fELF":
                    return None
            result = self.run([self.readelf, "-hW", "-lW", "-dW", str(self.physical(path))],
                              check=True, capture_output=True, text=True,
                              env={**os.environ, "LC_ALL": "C"})
            self.elf[key] = parse_readelf(result.stdout)
        return self.elf[key]

    def search_paths(self, entries, origin):
        paths = []
        for entry in entries:
            expanded = entry.replace("${ORIGIN}", str(origin)).replace("$ORIGIN", str(origin))
            if not expanded or "$" in expanded or not expanded.startswith("/"):
                raise CollectionError(f"non-relocatable/unsupported ELF search path {entry!r} at {origin}")
            paths.append(absolute_path(expanded))
        return paths

    def find_needed(self, name, info, paths):
        if "/" in name:
            if not name.startswith("/"):
                raise CollectionError(f"relative DT_NEEDED path depends on runtime cwd: {name}")
            candidates = [(absolute_path(name), "explicit")]
        else:
            candidates = [(directory / name, "search-path") for directory in paths]
            candidates += [(absolute_path(value), "cache") for value in self.cache.get(name, [])
                           if not info["nodeflib"] or str(absolute_path(value).parent) not in DEFAULT_LIBRARY_DIRS]
            if not info["nodeflib"]:
                candidates += [(PurePosixPath(directory) / name, "default") for directory in DEFAULT_LIBRARY_DIRS]
        for candidate, origin in dict.fromkeys(candidates):
            try:
                resolved = self.resolve(candidate)
            except CollectionError:
                continue
            candidate_info = self.inspect(resolved)
            if candidate_info and (candidate_info["class"], candidate_info["machine"]) == (info["class"], info["machine"]):
                return candidate, origin
        raise CollectionError(f"unresolved ELF dependency {name!r} for {info['class']} {info['machine']}")

    def scan_dependencies(self, source, inherited):
        key = (str(source), tuple(map(str, inherited)))
        if key in self.scanned:
            return
        self.scanned.add(key)
        info = self.inspect(source)
        if info is None:
            with self.physical(source).open("rb") as stream:
                first_line = stream.readline(4096)
            if first_line.startswith(b"#!"):
                self.collect_shebang(first_line[2:].decode("utf-8").strip())
            return
        if info["interpreter"]:
            interpreter = absolute_path(info["interpreter"])
            self.copy(interpreter, inherited=inherited)
            self.dependencies.append({"from": str(source), "kind": "PT_INTERP", "path": str(interpreter)})
        rpath = self.search_paths(info["rpath"], source.parent) if not info["runpath"] else []
        runpath = self.search_paths(info["runpath"], source.parent)
        direct = runpath if info["runpath"] else rpath + list(inherited)
        descendants = tuple(dict.fromkeys(rpath + list(inherited)))
        for needed in info["needed"]:
            found, origin = self.find_needed(needed, info, direct)
            self.copy(found, inherited=descendants)
            self.dependencies.append({"from": str(source), "kind": "DT_NEEDED", "name": needed, "path": str(found)})
            if origin == "cache":
                self.cache_used = True
                self.copy(PurePosixPath("/etc/ld.so.cache"))
                # The real loader may select another compatible cache entry
                # (notably a glibc-hwcaps variant). Retain every candidate for
                # this required SONAME, not unrelated cache contents.
                for value in self.cache.get(needed, []):
                    candidate = absolute_path(value)
                    if info["nodeflib"] and str(candidate.parent) in DEFAULT_LIBRARY_DIRS:
                        continue
                    try:
                        resolved = self.resolve(candidate)
                    except CollectionError:
                        continue
                    candidate_info = self.inspect(resolved)
                    if candidate_info and (candidate_info["class"], candidate_info["machine"]) == (info["class"], info["machine"]):
                        self.copy(candidate, inherited=descendants)

    def collect_shebang(self, text):
        words = shlex.split(text)
        if not words:
            raise CollectionError("empty script interpreter")
        interpreter = absolute_path(words[0])
        self.copy(interpreter)
        if interpreter.name == "env":
            args = words[1:]
            if args and args[0] in {"-S", "--split-string"}:
                args = args[1:]
            if not args or args[0].startswith("-") or "=" in args[0]:
                raise CollectionError("env shebang requires an explicitly named command; unsupported env options")
            command = args[0]
            if command not in self.commands:
                raise CollectionError(f"env shebang command {command!r} must appear in spec.commands")
            self.copy(self.commands[command])

    def copy(self, path, destination=None, ancestors=(), inherited=()):
        path = absolute_path(str(path))
        destination = path if destination is None else destination
        target = self.resolve(path)
        metadata = self.physical(target).stat()
        output = self.destination(destination)
        if stat.S_ISDIR(metadata.st_mode):
            if target in ancestors:
                raise CollectionError(f"directory symlink cycle at {path} -> {target}")
            self.ensure_parents(destination)
            output.mkdir(exist_ok=True)
            self.directories[str(destination)] = stat.S_IMODE(metadata.st_mode) & 0o777
            for child in sorted(self.physical(target).iterdir(), key=lambda item: item.name):
                self.copy(target / child.name, destination / child.name, ancestors + (target,), inherited)
            return
        if not stat.S_ISREG(metadata.st_mode):
            raise CollectionError(f"runtime closure only accepts regular files/directories/links: {path}")
        if target != destination:
            self.copy(target, inherited=inherited)
            self.link(destination, target)
            return
        if str(target) not in self.files:
            self.ensure_parents(target)
            if output.exists() or output.is_symlink():
                raise CollectionError(f"conflicting runtime file: {target}")
            shutil.copyfile(self.physical(target), output)
            mode = stat.S_IMODE(metadata.st_mode) & 0o777
            output.chmod(mode)
            digest = hashlib.sha256()
            with output.open("rb") as stream:
                for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                    digest.update(chunk)
            self.files[str(target)] = {"path": str(target), "sha256": digest.hexdigest(), "bytes": metadata.st_size, "mode": mode}
        self.scan_dependencies(target, inherited)

    def collect(self, spec):
        if not isinstance(spec, dict) or spec.get("schema") != SCHEMA or set(spec) - {"schema", "commands", "paths", "runner"}:
            raise CollectionError("invalid runtime rootfs closure spec schema or fields")
        commands = spec.get("commands")
        if not isinstance(commands, dict) or not commands:
            raise CollectionError("spec.commands must be a nonempty alias-to-absolute-executable map")
        paths = spec.get("paths", [])
        if not isinstance(paths, list):
            raise CollectionError("spec.paths must be a list of absolute runtime data paths")
        for alias, path in commands.items():
            if not isinstance(alias, str) or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._+-]*", alias):
                raise CollectionError(f"invalid command alias: {alias!r}")
            self.commands[alias] = absolute_path(path)
        for alias, path in self.commands.items():
            resolved = self.resolve(path)
            if not self.physical(resolved).stat().st_mode & 0o111:
                raise CollectionError(f"runtime command is not executable: {path}")
            info = self.inspect(resolved)
            if info:
                abi = (info["class"], info["machine"])
                if self.abi is not None and self.abi != abi:
                    raise CollectionError(f"command ELF ABI mismatch at {path}: {abi} versus {self.abi}")
                self.abi = abi
            self.copy(path)
            self.link(PurePosixPath("/bin") / alias, resolved)
        for path in paths:
            self.copy(absolute_path(path))
        if spec.get("runner") is not None:
            runner = absolute_path(spec["runner"])
            info = self.inspect(runner)
            if info is None:
                raise CollectionError("spec.runner must be the same-ABI Linux O/olangc ELF executable")
            if self.abi is not None and self.abi != (info["class"], info["machine"]):
                raise CollectionError("spec.runner ELF ABI differs from the supplied runtime commands")
            # The launcher installs the actual generated self at its reserved
            # runner coordinate. This executable is only an ABI/dependency seed.
            self.scan_dependencies(self.resolve(runner), ())
        self.output.joinpath("runtime.json").write_text(json.dumps({
            "schema": "ostadix.embedded-runtime/v1", "execution": "linux-rootfs-v1", "environment": {},
        }, indent=2) + "\n", encoding="utf-8")
        for path, mode in sorted(self.directories.items(), key=lambda item: len(PurePosixPath(item[0]).parts), reverse=True):
            self.destination(path).chmod(mode)
        return {
            "schema": "ostadix.runtime-rootfs-collection/v1",
            "dependency_inspection": "readelf-and-loader-search-no-runtime-execution",
            "dynamic_dlopen_closure_verified": False, "services_synthesized": False,
            "commands": {name: str(path) for name, path in self.commands.items()},
            "runner": spec.get("runner"), "declared_paths": paths,
            "library_cache": ({"path": "/etc/ld.so.cache", "sha256":
                               self.files[str(self.resolve(PurePosixPath("/etc/ld.so.cache")))]["sha256"]}
                              if self.cache_used else None),
            "files": list(self.files.values()), "symlinks": self.links,
            "directories": self.directories, "dependencies": self.dependencies,
            "elf": self.elf,
        }


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("spec", type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--report", type=Path)
    args = parser.parse_args(argv)
    if sys.platform != "linux":
        raise CollectionError("runtime rootfs collection requires Linux")
    readelf = shutil.which("readelf")
    if readelf is None:
        raise CollectionError("trusted host readelf is required (install binutils)")
    output = args.output.absolute()
    report = args.report.absolute() if args.report else output.with_name(output.name + ".collection.json")
    if report.resolve().is_relative_to(output.resolve()):
        raise CollectionError("collection report must remain outside the runtime image")
    if output.is_symlink() or (output.exists() and (not output.is_dir() or any(output.iterdir()))):
        raise CollectionError("output must be a new or empty directory")
    if report.exists() or report.is_symlink():
        raise CollectionError(f"collection report already exists: {report}")
    spec = json.loads(args.spec.read_text(encoding="utf-8"))
    if not isinstance(spec, dict):
        raise CollectionError("runtime rootfs closure spec must be a JSON object")
    # Do not recursively copy the output back into itself through a data root.
    for declared in spec.get("paths", []):
        source = Path(declared).resolve()
        if source.is_dir() and output.resolve().is_relative_to(source):
            raise CollectionError("output cannot be inside a declared runtime data directory")
    output.mkdir(parents=True, exist_ok=True)
    result = Collector(output, readelf, load_library_cache()).collect(spec)
    with report.open("x", encoding="utf-8") as stream:
        json.dump(result, stream, indent=2, sort_keys=True)
        stream.write("\n")
    print(json.dumps({"rootfs": str(output), "report": str(report), "files": len(result["files"])}))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (CollectionError, OSError, ValueError, subprocess.SubprocessError) as error:
        print(f"runtime rootfs collection failed: {error}", file=sys.stderr)
        raise SystemExit(1)
