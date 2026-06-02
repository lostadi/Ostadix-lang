# Per-OS Setup Scripts for O-lang

These are dedicated, simple `.sh` scripts for specific operating systems/distributions. They perform the equivalent of the main `setup.sh` but without runtime detection — just the commands tailored for that OS.

Use the appropriate one directly when you know your target (great for Docker, CI, or clean installs).

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
# Example for a Debian-based system (or in docker)
curl -O https://.../setup-debian.sh   # or copy from repo
chmod +x setup-debian.sh
./setup-debian.sh
```

After running, follow the printed "Runnable forms" at the end.

Each script:
- Installs compilers, make, Python, curl, git, etc.
- Installs Rust via rustup (if missing)
- Builds the Rust edition (`cargo build --release`)
- Builds the C/C++ edition (`cd c_cpp && make`)
- Sets up Python shims (including optional matplotlib)
- Prints exact commands to run examples with Rust, C, AOT, and Python editions.

For the universal detector (tries to pick the right one), use the top-level `setup.sh` in the repo root.

For Docker testing (as originally suggested):
```bash
docker run -it -v "$PWD:/workspace" -w /workspace debian bash -c \
  'apt-get update && apt-get install -y sudo curl && ./setup/os/setup-debian.sh'
```

These scripts are standalone and can be used independently of the main `setup.sh`.
