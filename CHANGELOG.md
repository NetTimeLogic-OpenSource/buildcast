# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0] - 2026-10-27

### Added

- About dialog with the license, the third-party notices and a CycloneDX SBOM
  export.
- "What's new" dialog after an update.
- The window title shows the progress of a running job.
- Closing the window during a job asks for confirmation and stops the job
  first.

### Changed

- `buildcast-cli` rejects a `--buffer-mib` value outside 1 to 512 instead of
  clamping it.
- An invalid bmap blocks the write until you remove it, instead of silently
  writing the full image.
- The drive list refreshes after a job.
- The theme switch can follow the system theme.
- Update all dependencies, including egui 0.37. Building now needs Rust 1.95.

### Fixed

- The elevated `buildcast-cli` on Windows resolves relative paths against the
  original working directory.
- Linux unmounts only partitions of the target drive. Before, `/dev/sdb` also
  matched `/dev/sdba1`.
- A cancelled backup no longer reports that the device contents are undefined.

## [0.1.7] - 2026-10-24

### Security

- Update the rustls and webpki crates.

## [0.1.6] - 2026-10-23

### Fixed

- Scan drives off the UI thread, so a slow device no longer freezes the window.

## [0.1.5] - 2026-08-13

### Changed

- Success and cancel banners disappear after 6 seconds unless hovered.
- Softer card and window shadows, larger corner radii and more padding.

### Fixed

- "Install now" and job start are disabled while the other one runs.
- Close leaked process handles in the GUI and the CLI on Windows.
- Write CLI progress snapshots atomically.
- The GUI on Linux and macOS refuses non-removable drives unless "Show all
  drives" is on, like the CLI `--force` gate.
- Reject negative disk sizes reported by Windows.

## [0.1.4] - 2026-08-03

### Added

- `buildcast-cli copy` and `read` elevate themselves on Windows.

### Fixed

- The installer no longer warns about the per-user area.
- The Windows file version follows the workspace version.

## [0.1.3] - 2026-08-03

### Fixed

- Build warnings for Windows-only updater code on Linux and macOS.

## [0.1.2] - 2026-08-03

### Added

- Check GitHub releases for updates, with a one-click install on Windows.
- Optional per-user install on Windows.

### Changed

- The GUI runs unelevated and elevates each job through the CLI, so drag and
  drop works on Windows.

## [0.1.1] - 2026-08-03

### Added

- First release: write raw and compressed images, bmap support, verify after
  write, backups, Windows installer, Linux and macOS packages.

[Unreleased]: https://github.com/NetTimeLogic-OpenSource/buildcast/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/NetTimeLogic-OpenSource/buildcast/compare/v0.1.7...v0.2.0
[0.1.7]: https://github.com/NetTimeLogic-OpenSource/buildcast/compare/v0.1.6...v0.1.7
[0.1.6]: https://github.com/NetTimeLogic-OpenSource/buildcast/compare/v0.1.5...v0.1.6
[0.1.5]: https://github.com/NetTimeLogic-OpenSource/buildcast/compare/v0.1.4...v0.1.5
[0.1.4]: https://github.com/NetTimeLogic-OpenSource/buildcast/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/NetTimeLogic-OpenSource/buildcast/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/NetTimeLogic-OpenSource/buildcast/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/NetTimeLogic-OpenSource/buildcast/releases/tag/v0.1.1
