# OVFSIMG1 immutable image format

`OVFSIMG1` is a deterministic, bounded, little-endian read-only container for
the native O-core VFS importer. It contains no timestamps, host paths, owner
IDs, aliases, compression, or mutable metadata. The SHA-256 of the complete
image is its immutable identity; builders name it `root-<sha256>.ovfs` and set
its host mode to `0444`.

## Bounds and canonical form

- Maximum image size: 64 KiB in historical native probes; Mode 26 selects an
  isolated 96 KiB backing-store profile for its four-principal image.
- Maximum files: 16.
- Maximum file size: 1 MiB.
- Paths are canonical absolute UTF-8, 1 through 63 bytes: no empty component,
  `.` or `..`, repeated slash, trailing slash, NUL, or non-ASCII control byte.
- Entries are bytewise path-sorted and unique.
- The table immediately follows the header. `data_offset` and every payload
  offset are 4096-byte aligned. Payload extents do not overlap.
- Every padding byte and every reserved field is zero.
- Integers are unsigned little-endian. Arithmetic is checked before use.

## Header (128 bytes)

| Offset | Size | Field |
|---:|---:|---|
| 0 | 8 | magic, ASCII `OVFSIMG1` |
| 8 | 4 | version = 1 |
| 12 | 4 | header size = 128 |
| 16 | 4 | entry size = 128 |
| 20 | 4 | file count |
| 24 | 8 | table offset = 128 |
| 32 | 8 | first data offset, 4096-byte aligned |
| 40 | 8 | exact image size |
| 48 | 32 | SHA-256 of the complete entry table |
| 80 | 32 | SHA-256 of bytes `[data_offset, image_size)` including zero padding |
| 112 | 16 | reserved zero |

## Entry (128 bytes)

| Offset | Size | Field |
|---:|---:|---|
| 0 | 2 | path length, 1 through 63 |
| 2 | 2 | kind = 1 (regular file) |
| 4 | 4 | flags |
| 8 | 8 | absolute payload offset, 4096-byte aligned |
| 16 | 8 | file size |
| 24 | 32 | SHA-256 of file bytes |
| 56 | 64 | path bytes followed by zero padding |
| 120 | 8 | reserved zero |

Flags are `READ = 1`, `EXEC = 2`, and `CORPUS = 4`. `READ` is mandatory,
unknown bits are rejected, and there is deliberately no write bit. Executable
personality artifacts use `READ | EXEC`; negative loader corpus files use
`READ | CORPUS` and are never executable VFS authorities.

The importer may cache an FNV-1a-64 path ID, but the canonical full path is the
authority and must be compared to resolve hash collisions.
