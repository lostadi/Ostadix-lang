#!/usr/bin/env python3
"""Canonicalize GRUB's private rescue tree before invoking real xorriso.

GRUB 2.12 creates one wall-clock-named ``/.disk/*.uuid`` sentinel and embeds
that name in BOOTX64.EFI.  It also lets mtools choose a random FAT volume ID.
Those bytes make otherwise identical ``grub-mkrescue`` runs differ.  This
wrapper is admitted only for the repository ISO builder: it rewrites those
same-length, non-semantic identities to SOURCE_DATE_EPOCH-derived values,
normalizes the private tree timestamps, then execs the caller-selected xorriso.
"""

from __future__ import annotations

from datetime import datetime, timezone
import hashlib
import os
from pathlib import Path
import re
import shutil
import stat
import sys


TOKEN_PATTERN = re.compile(rb"[0-9]{4}(?:-[0-9]{2}){6}")


class CanonicalizationError(ValueError):
    """GRUB's private rescue tree was not the bounded shape we require."""


def _private_grub_tree(arguments: list[str]) -> Path:
    matches: list[Path] = []
    for argument in arguments:
        candidate = Path(argument)
        if (
            candidate.is_dir()
            and (candidate / ".disk").is_dir()
            and (candidate / "efi.img").is_file()
            and (candidate / "efi/boot/bootx64.efi").is_file()
        ):
            matches.append(candidate)
    unique = list(dict.fromkeys(matches))
    if len(unique) != 1:
        raise CanonicalizationError(
            f"xorriso invocation exposes {len(unique)} GRUB rescue trees; expected one"
        )
    tree = unique[0]
    if tree.is_symlink():
        raise CanonicalizationError("GRUB rescue tree must not be a symlink")
    return tree


def _fat_volume_id_offset(image: bytes) -> int:
    if len(image) < 512 or image[510:512] != b"\x55\xaa":
        raise CanonicalizationError("GRUB efi.img lacks a FAT boot-sector signature")
    if image[38] == 0x29:
        return 39
    if image[66] == 0x29:
        return 67
    raise CanonicalizationError("GRUB efi.img lacks a FAT volume-ID field")


def _walk_without_links(tree: Path) -> tuple[list[Path], list[Path]]:
    files: list[Path] = []
    directories: list[Path] = [tree]
    for root_text, directory_names, file_names in os.walk(tree, followlinks=False):
        root = Path(root_text)
        for name in directory_names:
            path = root / name
            state = os.stat(path, follow_symlinks=False)
            if stat.S_ISLNK(state.st_mode) or not stat.S_ISDIR(state.st_mode):
                raise CanonicalizationError(f"GRUB rescue tree directory is unsafe: {path}")
            directories.append(path)
        for name in file_names:
            path = root / name
            state = os.stat(path, follow_symlinks=False)
            if stat.S_ISLNK(state.st_mode) or not stat.S_ISREG(state.st_mode):
                raise CanonicalizationError(f"GRUB rescue tree file is unsafe: {path}")
            files.append(path)
    return files, directories


def canonicalize(arguments: list[str], epoch: int) -> list[str]:
    if epoch < 315532800 or epoch > 2147483647:
        raise CanonicalizationError(
            "SOURCE_DATE_EPOCH must be from 1980-01-01 through signed 32-bit time"
        )
    fixed_token = datetime.fromtimestamp(epoch, timezone.utc).strftime(
        "%Y-%m-%d-%H-%M-%S-00"
    )
    fixed_bytes = fixed_token.encode("ascii")
    tree = _private_grub_tree(arguments)
    disk_directory = tree / ".disk"
    sentinels = sorted(disk_directory.glob("*.uuid"))
    if len(sentinels) != 1 or sentinels[0].is_symlink() or not sentinels[0].is_file():
        raise CanonicalizationError("GRUB rescue tree must contain exactly one regular .uuid sentinel")
    old_token = sentinels[0].name.removesuffix(".uuid").encode("ascii")
    if TOKEN_PATTERN.fullmatch(old_token) is None or len(old_token) != len(fixed_bytes):
        raise CanonicalizationError("GRUB .uuid sentinel has an unexpected wall-clock name")

    files, directories = _walk_without_links(tree)
    auxiliary_boot_candidates = (
        tree / "boot.efi",
        tree / "System/Library/CoreServices/boot.efi",
    )
    auxiliary_boot_artifacts = [
        path for path in auxiliary_boot_candidates if path in files
    ]
    if len(auxiliary_boot_artifacts) != 1:
        raise CanonicalizationError(
            "GRUB rescue tree must contain exactly one admitted auxiliary "
            "boot.efi layout"
        )
    token_artifacts = (
        tree / "efi.img",
        tree / "efi/boot/bootx64.efi",
        auxiliary_boot_artifacts[0],
    )
    token_contents: dict[Path, bytes] = {}
    for path in token_artifacts:
        if path not in files:
            raise CanonicalizationError(
                f"GRUB UUID-bearing artifact is missing or unsafe: {path}"
            )
        content = path.read_bytes()
        count = content.count(old_token)
        if count != 1:
            raise CanonicalizationError(
                f"GRUB UUID-bearing artifact {path} contains {count} wall-clock "
                "tokens; expected exactly one"
            )
        token_contents[path] = content
    for path, content in token_contents.items():
        path.write_bytes(content.replace(old_token, fixed_bytes, 1))

    fixed_sentinel = disk_directory / f"{fixed_token}.uuid"
    if fixed_sentinel != sentinels[0]:
        if fixed_sentinel.exists() or fixed_sentinel.is_symlink():
            raise CanonicalizationError("canonical GRUB .uuid sentinel already exists")
        os.replace(sentinels[0], fixed_sentinel)

    efi_image_path = tree / "efi.img"
    efi_image = bytearray(efi_image_path.read_bytes())
    serial_offset = _fat_volume_id_offset(efi_image)
    bootloader = (tree / "efi/boot/bootx64.efi").read_bytes()
    serial = hashlib.sha256(
        b"OSTADIX/UEFI-ISO-FAT-ID/V1\0" + fixed_bytes + bootloader
    ).digest()[:4]
    if serial == bytes(4):
        serial = b"OSTI"
    efi_image[serial_offset : serial_offset + 4] = serial
    efi_image_path.write_bytes(efi_image)

    timestamp_ns = epoch * 1_000_000_000
    files, directories = _walk_without_links(tree)
    for path in files:
        os.utime(path, ns=(timestamp_ns, timestamp_ns), follow_symlinks=False)
    for path in reversed(directories):
        os.utime(path, ns=(timestamp_ns, timestamp_ns), follow_symlinks=False)

    fixed_iso_date = datetime.fromtimestamp(epoch, timezone.utc).strftime(
        "%Y%m%d%H%M%S00"
    )
    return [
        f"--modification-date={fixed_iso_date}"
        if argument.startswith("--modification-date=")
        else argument
        for argument in arguments
    ]


def main() -> int:
    real_text = os.environ.get("OSTADIX_REAL_XORRISO", "")
    if not real_text:
        print("error: OSTADIX_REAL_XORRISO is required", file=sys.stderr)
        return 2
    real = shutil.which(real_text)
    if real is None:
        print(f"error: real xorriso is unavailable: {real_text}", file=sys.stderr)
        return 127
    # grub-mkrescue performs one read-only capability probe before it creates
    # the private rescue tree. Preserve that exact probe; every image-producing
    # invocation must pass through canonicalize() below.
    if sys.argv[1:] in (
        ["-as", "mkisofs", "-help"],
        ["-as", "mkisofs", "--help"],
        ["-version"],
        ["--version"],
    ):
        os.execv(real, [real, *sys.argv[1:]])
    try:
        epoch = int(os.environ.get("SOURCE_DATE_EPOCH", ""), 10)
        arguments = canonicalize(sys.argv[1:], epoch)
    except (CanonicalizationError, OSError, UnicodeError, ValueError) as error:
        print(f"error: cannot canonicalize GRUB rescue tree: {error}", file=sys.stderr)
        return 1
    os.execv(real, [real, *arguments])
    raise AssertionError("os.execv returned")


if __name__ == "__main__":
    raise SystemExit(main())
