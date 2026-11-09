# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
