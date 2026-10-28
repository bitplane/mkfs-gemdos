# mkfs-gemdos

Create GEMDOS (Atari TOS) FAT12 disk images in Rust.

GEMDOS is FAT12 with Atari-specific quirks:

- Non-executable boot sector (no boot program)
- BPB fields, including bytes-per-sector, are **little-endian**, as on PC
- Boot sector ends with a **BE16 word-sum checksum** (sum of the 256
  big-endian 16-bit words at offsets 0..511 is `0x1235`; `0x1234` would
  mark the disk executable)
- 24-bit serial number at offsets 0x08–0x0A
- Otherwise the layout matches MS-DOS FAT12

Images can be mounted on Linux as VFAT, used in Atari emulators
(Hatari, STeem), or written to a real Atari floppy.

## Install

From crates.io:

```bash
cargo install mkfs-gemdos
```

This installs a binary called `mkfs-gemdos`. Pre-built binaries for Linux
(x86_64 / aarch64), macOS (universal), and Windows are also attached to
each [GitHub release][releases].

[releases]: https://github.com/bitplane/mkfs-gemdos/releases

## Usage

```bash
# Default: 720 KB DSDD floppy.
mkfs-gemdos out.st

# 1.44 MB high-density floppy.
mkfs-gemdos -s 1440 out.st

# Populate from a directory tree (recursive — subdirectories are preserved).
mkfs-gemdos -d ./payload out.st

# Mount on Linux.
sudo mount -o loop,ro -t vfat out.st /mnt
```

Supported capacities are 360, 720, 1200, 1440, and 2880 KB. Other sizes are
rejected because they do not have a supported physical floppy geometry.

## Filename rules

Source filenames must conform to FAT 8.3:

- ASCII only, no control chars
- No leading dot, at most one dot
- Forbidden punctuation: `" * + , / : ; < = > ? [ \ ] |` and space
- Up to 8 base + 3 extension chars (longer parts are truncated with a warning)

Duplicate 8.3 names within a directory are rejected.

File modification times use local time on Unix and Windows, with two-second
FAT precision. Dates outside 1980–2107 are clamped to the nearest endpoint.

## License

WTFPL with one extra clause: don't blame me. See [LICENSE](LICENSE).
