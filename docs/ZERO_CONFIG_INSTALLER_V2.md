# Zero-configuration installer v2

The first source overlay exposed two integration defects that were not caught by
source-only dispatch tests.

1. The overlay copied files with archive timestamps. Cargo could therefore keep
   an older fingerprinted executable even though the source tree now contained
   the zero-configuration commands. A deleted top-level binary could be restored
   from an older dependency artifact without recompiling the modified source.
2. The installed wrapper attempted to distinguish `O` from `o` through the case
   of `$0`. On case-insensitive filesystems and through shell command caches, that
   spelling is not a stable semantic channel.

Installer v2 changes the contract as follows:

- overlay targets receive a current modification time;
- the affected checkout is cleaned before its first post-overlay build;
- `o-node --help`, `octl node --help`, the repository dispatcher, and the
  installed dispatcher are checked after compilation;
- the lowercase wrapper never branches on `$0` case;
- on case-insensitive filesystems both `O` and `o` pass through the dispatcher,
  whose fallback still invokes the native evaluator;
- `ostadix-evaluator` remains the unambiguous raw evaluator entry point;
- broad accidental mode changes are restored from the Git index without
  reverting file contents.

The installer aborts rather than printing success when any runtime surface is
still stale. A clean Rust build remains a host-side verification gate and is not
claimed until the installer completes it on a machine with Cargo.
