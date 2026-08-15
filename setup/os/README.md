# Per-OS Setup Scripts for O-lang

These compatibility entrypoints delegate to the repository-root `setup.sh`.
Keeping one implementation prevents platform scripts from silently omitting new
binaries, verification gates, or safety fixes.

Existing commands may continue using the platform-named entrypoint; every option
is forwarded unchanged to the canonical setup.

## Available scripts

- `setup-macos.sh` — macOS (Homebrew + Xcode CLT)
- `setup-debian.sh` — Debian, Ubuntu, Linux Mint, Pop!_OS, etc. (apt)
- `setup-arch.sh` — Arch, CachyOS, Manjaro, EndeavourOS, etc. (pacman)
- `setup-fedora.sh` — Fedora, CentOS Stream, RHEL, Rocky, AlmaLinux, etc. (dnf)
- `setup-gentoo.sh` — Gentoo (emerge)
- `setup-nixos.sh` — NixOS (nix-env + guidance for declarative)
- `setup-tinycore.sh` — TinyCore Linux (tce-load, minimal)
- `setup-alpine.sh` — Alpine Linux (apk)
- `setup-opensuse.sh` — openSUSE, SLE (zypper)
- `setup-void.sh` — Void Linux (xbps)
- `setup-freebsd.sh` — FreeBSD and similar BSDs (pkg)
- `setup-windows.sh` — Windows bash environments (Git Bash, MSYS, Cygwin, WSL) — strongly recommends WSL + debian script

## Usage

```bash
# From a repository checkout on a Debian-based system (or in Docker)
./setup/os/setup-debian.sh --minimal --verify
```

After running, follow the printed "Runnable forms" at the end.

The root setup detects the platform, installs the selected dependency profile,
builds every public binary, and prints the runnable forms. Optional Python
packages such as matplotlib are reported but never installed implicitly.

For Docker testing (as originally suggested):
```bash
docker run -it -v "$PWD:/workspace" -w /workspace debian bash -c \
  'apt-get update && apt-get install -y sudo curl && ./setup/os/setup-debian.sh'
```

The platform entrypoints require the surrounding repository because they invoke
`../../setup.sh`; use a source checkout or source release, not a copied script.
