# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- Replace `ContainerHandler::c2pa_box_name` and the context-free `box_name` with
  one contextual `box_name` hook. `Structure::box_name` now takes the owning
  segment ID and element, so inspection and C2PA hashing use the same naming rules.
- Replace `C2paBox` with generic `BoxInfo`, whose name is optional for unnamed
  bytes. Add `Structure::boxes()` and replace `c2pa_boxes_with` with
  `boxes_with(item, name)` for caller-selected grouping. Rename the handler's
  continuation hook to `box_continues`. C2PA naming and validation remain in
  `c2pa_boxes`, with `c2pa_boxes_async` for caller-driven I/O.
- Replace `C2paHash` and `c2pa_hash` with `Structure::supports_named_boxes()`,
  a boolean derived from the generic box structure. Hash-binding selection is
  left to callers.

## [0.4.0](https://github.com/contentauth/asset-io/compare/v0.3.2...v0.4.0) - 2026-11-09

### Added

- add GIF container support
- add ID3 metadata handler for MP3 and FLAC
- svg stream parsing with quick-xml 0.41

### Documentation

- reorganize README and add user and handler guides

### Fixed

- remove orphaned svg doc comment
- fix clippy tests in ci

### Other

- Automate release PRs and changelog with release-plz ([#34](https://github.com/contentauth/asset-io/pull/34))
- make asset-io-c a workspace member ([#33](https://github.com/contentauth/asset-io/pull/33))
- Add required open source files and make some README URLs absolute
- check formatting in make test
- *(gif)* support C2PA box hash

## [0.3.2](https://github.com/contentauth/asset-io/compare/v0.3.1...v0.3.2) - 2026-11-05

### Other

- update to quick-xml 0.41

## [0.3.1](https://github.com/contentauth/asset-io/compare/v0.3.0...v0.3.1) - 2026-11-05

### Added

- *(tiff)* add read-only raw format detection (ARW, NEF)

### Fixed

- fix a C bindings test

### Other

- check the release version and tag in the release workflow

## [0.3.0](https://github.com/contentauth/asset-io/releases/tag/v0.3.0) - 2026-11-04

First crates.io release of `asset-io`, a Rust library for inspecting and
updating media-container structure and metadata.

The 0.3.0 version reflects the project's pre-release development history;
there was no published 0.2.0 release.

### Highlights

- Feature-gated handlers for JPEG, PNG, ISO BMFF, RIFF, TIFF, SVG, and PDF.
- A shared structure and item model for container geometry and metadata.
- Parse, plan, and write APIs, including streaming output and async / caller-driven I/O.
- Optional XMP, EXIF, compression-decoder, and memory-mapped functionality.
- No container handlers enabled by default; consumers opt in to the formats they need.

### Requirements

- Rust 1.88 or later.

### Getting started

```toml
asset-io = { version = "0.3", features = ["jpeg", "pdf"] }
```
