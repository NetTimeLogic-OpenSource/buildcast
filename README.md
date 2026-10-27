# BuildCast

BuildCast writes disk images to USB drives and SD cards and creates backups of
them. It has an [egui](https://github.com/emilk/egui) desktop app and a CLI for
scripts. With a `.bmap` file, it writes only the blocks that hold data.

| Package | Content |
|---------|---------|
| `packages/core` | Shared library: image decoding, disk access, write, verify and backup jobs, bmap support |
| `packages/gui` | The desktop app (`buildcast`) |
| `packages/cli` | The command line tool (`buildcast-cli`) |

## Features

- Runs on Windows, macOS and Linux.
- Writes `.img`, `.iso`, `.bin`, `.raw`, `.dd` and `.wic` images.
- Decompresses `.gz`, `.bz2`, `.xz` and `.zst` while writing.
- Reads the first regular file from `.zip` (including ZIP64) and `.tar`
  archives, also compressed ones such as `.tar.gz` or `.tar.xz`.
- With a `.bmap` file, writes only the mapped blocks and checks them against
  the sha256 sums in the file. BuildCast finds `foo.wic.bmap` next to
  `foo.wic.xz` on its own.
- Reads the device back after the write and compares it with the image. The
  read bypasses the operating system cache.
- Compares each block before it writes it and skips identical blocks. A
  reflash of a mostly identical image runs at read speed. "Force write" turns
  this off.
- Backs up a device into a `.dd` file, optionally compressed with zstd.
- Ejects the media after a successful job.
- Lists only removable and USB drives up to 256 GiB by default. It never lists
  the system disk and asks for confirmation before it erases a drive.
- Shows speed and time left. You can cancel at any time.
- Shows the changelog after an update. The About dialog lists all third-party
  packages with their licenses and exports a CycloneDX SBOM.

## Command line

```sh
buildcast-cli list                                        # list candidate drives
buildcast-cli copy foo.wic.xz --device 2 --yes            # uses foo.wic.bmap
buildcast-cli copy foo.wic.xz --bmap other.bmap --device 2
buildcast-cli copy foo.wic --device 2 --raw               # write the full image
buildcast-cli copy foo.wic.xz --device 2 --yes --eject    # eject when done
buildcast-cli read backup.dd.zst --compress --device 2    # back up a drive
```

Exit codes:

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | Usage error |
| 2 | Device error |
| 3 | Verification failed |
| 4 | Cancelled |

Non-removable drives need `--force`. The CLI never offers the system disk.
With a bmap, the CLI checks the checksums while it writes, like bmaptool.

## Privileges

Raw disk access needs elevated rights.

- **Windows**: The app runs as a normal user. Each write or backup starts
  `buildcast-cli` elevated, so Windows asks once per job. The CLI elevates
  itself in the same way.
- **Linux**: Add your user to the `disk` group or use `sudo`.
- **macOS**: Use `sudo`. BuildCast uses `diskutil` to list and unmount drives.

## Building

You need Rust (the version in `rust-toolchain.toml` is installed
automatically) and a C compiler for the zstd and xz codecs.

```sh
cargo build --release
```

The binaries are `target/release/buildcast` (app) and
`target/release/buildcast-cli`. On Linux, the app needs X11 or Wayland and
OpenGL at runtime.

Format the code with nightly rustfmt and run the tests with nextest:

```sh
cargo +nightly fmt --all
cargo clippy --workspace --all-targets
cargo nextest run --workspace
```

## Releases

The version is set once, in `[workspace.package]` in the root `Cargo.toml`.
The exe resources, the installer, the app bundle and the app header take it
from there. `CHANGELOG.md` follows [Keep a Changelog](https://keepachangelog.com/)
and [Semantic Versioning](https://semver.org/).

To release:

1. Set the new version in `Cargo.toml`.
2. Rename the `Unreleased` section in `CHANGELOG.md` to the new version.
3. Tag and push: `git tag v<version> && git push --tags`.

CI builds all platforms and publishes a GitHub release. The release notes come
from `CHANGELOG.md`. CI rejects a tag that does not match the version or has no
changelog section. A release contains:

- **Windows**: `buildcast-setup-<version>.exe` (installer with Start menu and
  desktop shortcuts and an option to add the CLI to `PATH`) and a portable zip.
- **Linux**: a `.deb` package and a portable `.tar.gz`.
- **macOS**: `buildcast-<version>-macos.dmg` with `BuildCast.app`.
- A CycloneDX SBOM for each platform.

To package locally:

```sh
cargo build --workspace --release
ISCC installer\windows\buildcast.iss     # Windows, needs Inno Setup 6
cargo deb -p buildcast --no-build        # Linux, needs cargo-deb
bash installer/macos/make_bundle.sh      # macOS
```

## Credits

BuildCast is a new implementation in Rust. Its features were inspired by
[USBImager](https://gitlab.com/bztsrc/usbimager) by bzt. It contains no code
from USBImager.

## License

MIT, see [LICENSE](LICENSE).
