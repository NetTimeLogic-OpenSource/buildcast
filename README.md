# BuildCast

Cast your builds onto real hardware: a fast, safe disk image writer with a modern
[egui](https://github.com/emilk/egui) GUI and a scriptable CLI. Flashes raw or
compressed images to USB drives and SD cards, creates backups, and understands
`.bmap` files to write only the blocks that matter.

Workspace layout:

| Package | What it is |
|---------|------------|
| `packages/core` | shared library: stream decoding, disk access, copy/verify jobs, bmap support |
| `packages/gui` | the BuildCast desktop app (`buildcast`) |
| `packages/cli` | `buildcast-cli`: scriptable writer; optional `.bmap` flashes only mapped blocks |

## Features

- **Cross-platform**: Windows, macOS and Linux from a single codebase
- **Write images** to USB drives: `.img`, `.iso`, `.bin`, `.raw`, `.dd`, `.wic`, …
- **On-the-fly decompression**: `.gz`, `.bz2`, `.xz`, `.zst`
- **Archives**: `.zip` (incl. ZIP64), `.tar` (also `.tar.gz`/`.tgz`/`.tar.xz`/…);
  the first regular file inside is used as the image
- **bmap support** (GUI and CLI): give a `.bmap` file (auto-detected next to the
  image) and only mapped blocks are written, verified against the embedded sha256
  sums
- **Verify after write**: the device is read back (bypassing OS caches) and compared
  against the image
- **Skip-unchanged writes**: blocks are compared before writing, so reflashing a
  mostly identical image runs at read speed ("Force write" disables the compare)
- **Backups**: read a device into a raw `.dd` file, optionally zstd-compressed
- **Synchronized writes**: unbuffered I/O plus a final device flush
- **Pipelined**: decompression runs on its own thread, overlapping device writes
- **Safe by default**: only removable/USB drives up to 256 GiB are listed, the system
  disk is always excluded, and writing requires an explicit "Erase and write"
  confirmation
- **Eject when done**: media is ejected after a successful write or backup
- Progress with windowed speed and ETA, cancellable at any time, drag & drop,
  light/dark theme with a toggle

## buildcast-cli

```sh
buildcast-cli list                 # candidate drives: id, size, bus, removable
buildcast-cli copy foo.wic.xz --device 2 --yes            # uses foo.wic.bmap
buildcast-cli copy foo.wic.xz --bmap other.bmap --device 2
buildcast-cli copy foo.wic --device 2 --raw               # full sequential write
buildcast-cli copy foo.wic.xz --device 2 --yes --eject    # eject when finished
buildcast-cli read backup.dd.zst --compress --device 2    # backup a drive
```

Exit codes: 0 success, 1 usage error, 2 device error, 3 verification failure,
4 user abort. Non-removable targets need `--force`; the system disk is never
offered. Checksums verify the decompressed source stream while writing
(bmaptool semantics, no read-back).

## Installing

Tagged releases (`git tag v<version> && git push --tags`) are built by CI for all
platforms and published as a GitHub Release with:

- **Windows**: `buildcast-setup-<version>.exe` (Inno Setup installer: Program
  Files, Start Menu/desktop shortcuts, optional CLI on `PATH`) plus a portable zip
- **Linux**: `.deb` package (binaries, desktop entry, icon) plus a portable tar.gz
- **macOS**: `buildcast-<version>-macos.dmg` with `BuildCast.app`

The version lives in one place: `[workspace.package]` in the root `Cargo.toml`.
Everything else (exe resources, installer, bundle, GUI header) derives from it;
the release workflow refuses tags that don't match it.

Local packaging:

```sh
cargo build --workspace --release
ISCC installer\windows\buildcast.iss     # Windows (Inno Setup 6)
cargo deb -p buildcast --no-build        # Linux (cargo install cargo-deb)
bash installer/macos/make_bundle.sh      # macOS
```

## Building

Requires a Rust toolchain (stable). C compiler needed for the zstd/xz codecs.

```sh
cargo build --release
```

Binaries end up in `target/release/`: `buildcast[.exe]` (GUI) and
`buildcast-cli[.exe]`.

Linux needs the usual egui/winit system libraries at runtime (X11 or Wayland,
OpenGL).

## Running / privileges

Raw disk access needs elevated rights:

- **Windows**: the GUI runs as a normal user (browsing, drag & drop, device
  listing need no elevation); each write or backup launches `buildcast-cli`
  elevated, so UAC prompts once per job. Run the CLI itself from an
  Administrator terminal.
- **Linux**: add your user to the `disk` group, or run with `sudo`.
- **macOS**: run with `sudo` (`diskutil` is used for enumeration and unmounting).

## Credits

BuildCast is a from-scratch Rust implementation. Its feature set was inspired by
bzt's [USBImager](https://gitlab.com/bztsrc/usbimager); no code from it is included.

## License

MIT; see [LICENSE](LICENSE).
