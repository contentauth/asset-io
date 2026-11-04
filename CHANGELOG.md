# asset-io 0.3.0

First crates.io release of `asset-io`, a Rust library for inspecting and
updating media-container structure and metadata.

The 0.3.0 version reflects the project's pre-release development history;
there was no published 0.2.0 release.

## Highlights

- Feature-gated handlers for JPEG, PNG, ISO BMFF, RIFF, TIFF, SVG, and PDF.
- A shared structure and item model for container geometry and metadata.
- Parse, plan, and write APIs, including streaming output and async / caller-driven I/O.
- Optional XMP, EXIF, compression-decoder, and memory-mapped functionality.
- No container handlers enabled by default; consumers opt in to the formats they need.

## Requirements

- Rust 1.88 or later.

## Getting started

```toml
asset-io = { version = "0.3", features = ["jpeg", "pdf"] }
```
