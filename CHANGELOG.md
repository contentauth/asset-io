# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.6.0](https://github.com/contentauth/asset-io/compare/v0.5.0...v0.6.0) - 2026-11-13

### Added

- *(pdf)* [**breaking**] write only the new manifests when adding to a PDF's C2PA store ([#40](https://github.com/contentauth/asset-io/pull/40))

### Fixed

- Read PDF metadata with a single Identity Crypt filter, including C2PA streams
  with no `/DecodeParms`, while still rejecting non-Identity crypt filters.

## [0.5.0](https://github.com/contentauth/asset-io/compare/v0.4.0...v0.5.0) - 2026-11-13

### Added

- [**breaking**] abstract the c2pa_box handling apis to be generic over any type ([#39](https://github.com/contentauth/asset-io/pull/39))

### Documentation

- Add design priorities and Copilot instructions ([#37](https://github.com/contentauth/asset-io/pull/37))

### Changed

- Replace `ContainerHandler::c2pa_box_name` and the context-free `box_name` with
  one contextual `box_name` hook. `Structure::box_name` now takes the owning
  segment ID and element, so inspection and C2PA hashing use the same naming rules.
- Replace `C2paBox` with generic `BoxInfo`, whose name is optional for unnamed
  bytes. Add `Structure::boxes()` and replace `c2pa_boxes_with` with
  `boxes_with(item, name)` for caller-selected grouping. Rename the handler's
  continuation hook to `box_continues`.
- Replace `C2paHash` and `c2pa_hash` with `Structure::supports_named_boxes()`,
  a boolean derived from the generic box structure. Hash-binding selection is
  left to callers.
- Identify the C2PA manifest store as its own item kind. Replace
  `ItemKind::Jumbf` with `ItemKind::C2pa` (encoding `Jumbf`, string `"c2pa"`) and
  `ItemInfo::jumbf()` with `ItemInfo::c2pa()`. Handlers identify the store while
  parsing: from the element the container reserves for it, or by superbox type
  where the container carries JUMBF of any type (JPEG). JUMBF of other types is
  `ItemKind::Other` with `Encoding::Jumbf` and its recorded `jumbf_type()`.
  Add `jumbf::item_info` for handlers that classify JUMBF by type.
- `Structure::c2pa_item()` takes no source and reads nothing; remove
  `c2pa_item_async` and `tokio::c2pa_item`. Remove `Structure::store`
  (use `c2pa_store`), `Structure::jumbf_type` and `jumbf_type_async` (use
  `Item::jumbf_type`), and the matching `Asset` methods.
- Rename the JUMBF updates to `Updates::set_c2pa`, `remove_c2pa`, `keep_c2pa`,
  `c2pa`, `is_c2pa_update` and `with_c2pa`, and the conformance option to
  `Conformance::c2pa`. Remove `remove_jumbf_of`, `jumbf_scope` and
  `jumbf_targets`: updates change only the C2PA store and never other JUMBF,
  including on `remove_all`. In JPEG a store must have the C2PA store superbox
  type, and a new one takes a JPEG XT box instance number other JUMBF does not use.
- Dedicated C2PA carriers (PNG, RIFF, GIF, ID3, SVG, ISO BMFF, sidecar) no longer
  read the store's superbox type while parsing.
- C API: metadata kind 2 is the C2PA store, item kind strings use `c2pa`,
  `asset_io_item_jumbf_type` returns the recorded type without reading, and
  `asset_io_updates_remove_jumbf_type` is removed.
- Add the generic `complete_boxes(group, trailing_name)`, where callers choose
  the names. `c2pa_boxes()` is now that with the C2PA names, takes no source and
  reads nothing; remove `c2pa_boxes_async`, `C2PA_BOX` and `AFTER_BOX`.
- `Structure::write_plan`, `write_plan_task` and the default `write` fail with the
  new `Error::UnsupportedItem(kind)` when the handler does not place an XMP or
  C2PA value that `Updates` sets, so a value is never silently dropped. ID3, GIF
  and C2PA sidecar handlers report it for kinds they cannot hold.

### Added

- `WritePlan::reader(src)`: read the planned output without writing it,
  including relocated offset tables.
- `WritePlan::written_items(kind)`: the items a plan writes from supplied data,
  such as the store `Updates` set, found without reading.
- `WritePlan::is_relocated(range)`: whether a write rewrites offsets in a range.
- `Error::Callback` and `Error::callback`: a sink can stop an operation with its
  own error, which is returned unchanged.
- `testing::c2pa_store`, `testing::c2pa_store_of_len` and `testing::jumbf`: build
  C2PA manifest stores and other JUMBF as test data, so tests need not lay out
  JUMBF boxes themselves.
- `testing::rewrite(handler, bytes, &updates)`, replacing `testing::write`: write
  a sample every way `Conformance` does and return the output and its structure.
- `Conformance` also checks that a C2PA placeholder is filled in place (with
  `patch_exact` and `patch`) without changing a byte a data hash covers, and that
  `compose` gives what a write inserts.

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
