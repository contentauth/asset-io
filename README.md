# asset-io

[![crates.io](https://img.shields.io/crates/v/asset-io.svg)](https://crates.io/crates/asset-io)
[![Documentation](https://docs.rs/asset-io/badge.svg)](https://docs.rs/asset-io)
[![License](https://img.shields.io/crates/l/asset-io.svg)](#license)

Read, inspect and update media-file metadata through one Rust API.

`asset-io` works with JPEG, PNG, HEIF, AVIF, MP4, WebP, WAV, AVI, TIFF, SVG,
PDF and C2PA sidecars. It lets you:

- Find and read XMP, JUMBF (including C2PA manifest stores), EXIF and embedded files.
- Replace or remove XMP and JUMBF without decoding or re-encoding media.
- Stream updates with bounded buffers, copying unchanged media rather than
  loading the whole file into memory.
- Observe tagged bytes with a `Sink` to hash while reading or writing.
- Add container formats by registering your own handlers.

The core is runtime-independent, with blocking, Tokio and caller-driven I/O.
Used by [c2pa-rs](https://github.com/contentauth/c2pa-rs), it handles container
structure and metadata placement, **not** manifest interpretation, hashing
algorithms or signing.

## Installation

Enable the formats you need. **No formats are enabled by default.**

```toml
[dependencies]
asset-io = { version = "0.3", features = ["jpeg", "png"] }
```

Requires Rust 1.88 or later.

## Usage

Read metadata and write an updated copy:

```rust,no_run
use std::fs::{self, File, OpenOptions};

use asset_io::{Asset, NullSink, Updates};

fn main() -> asset_io::Result<()> {
    let asset = Asset::open(File::open("photo.jpg")?, None)?;

    for item in asset.items() {
        println!("{} ({}): {} bytes", item.kind(), item.encoding(), item.len());
    }
    if let Some(item) = asset.c2pa_item()? {
        let store = asset.read_content(item)?;
        println!("C2PA store: {} bytes", store.len());
    }

    let xmp = fs::read("new.xmp")?;
    let updates = Updates::new().set_xmp(xmp).remove_jumbf();
    let mut output = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open("updated.jpg")?;
    asset.write(&mut output, &updates, &mut NullSink)?;
    Ok(())
}
```

This leaves the input untouched and creates a new output file, failing if it
already exists. Keep the source unchanged while using its parsed structure.

Content detection takes precedence over the optional format argument. Pass an
extension or MIME type, such as `Some("svg")`, for formats without a detector.
Only handlers enabled or registered in your application are available.

### The API in brief

- **`Asset`** pairs a parsed file with its blocking stream. Start here for
  reading and writing files.
- **`Structure`** describes segments, metadata items and byte ranges without
  owning the bytes. Use it for async I/O, write planning and hashing geometry.
- **`Updates`** describes XMP and JUMBF changes. `write` returns the output's
  structure; `plan` describes it without writing.
- **`HandlerRegistry`** selects built-in or custom handlers. Libraries should
  accept a registry and use `Asset::open_with` so callers control format support.

`read_item` returns container-stored bytes; `read_content` undoes transfer
encoding (such as base64 or compression). Missing optional decoders return
`UnsupportedFeature`.

## Supported formats and features

Every built-in handler supports reading and metadata updates:

| Feature | Formats | Notes |
|---------|---------|-------|
| `jpeg` | JPEG | Includes multi-picture JPEG (MPF) |
| `png` | PNG | |
| `bmff` | MP4, HEIF, HEIC, AVIF | Rewrites chunk offsets when needed |
| `riff` | WebP, WAV, AVI | |
| `svg` | SVG | Splices metadata without re-serializing the document |
| `pdf` | PDF 1.0-2.0 | [Incremental updates](src/containers/pdf/README.md) |
| `tiff` | TIFF, BigTIFF, DNG | [Append-only IFD updates](src/containers/tiff/README.md) |
| `c2pa` | Raw C2PA sidecars | [Whole-file JUMBF storage](src/containers/c2pa/README.md) |
| `all-formats` | All of the above | |

Optional features:

| Feature | Adds |
|---------|------|
| `tokio` | Adapters for Tokio streams |
| `memory-mapped` | `MappedFile` for borrowed reads; the file must not change while mapped |
| `xmp` | `MiniXmp` for minimal XMP read/modify |
| `exif` | Basic EXIF fields and EXIF JPEG thumbnails |
| `flate` | zlib, deflate and gzip decoding; also enabled by `pdf` |
| `brotli` | Brotli decoding |
| `cli` | The `aio` tool, all formats, decoders, XMP, EXIF and Tokio |

`all-formats` does not enable every optional feature.

## Async I/O

Enable `tokio` alongside your format features. The
[`asset_io::tokio` adapters](https://docs.rs/asset-io/latest/asset_io/tokio/)
provide async parsing, metadata reads and writes over Tokio streams; no custom
task driver is needed.

See the [Tokio example](examples/tokio_client.rs) for inspection, updates and
hashing during writes:

```sh
cargo run --example tokio_client --features all-formats,tokio -- inspect photo.jpg
```

Other runtimes can drive the core's read requests and write steps directly;
see the [API documentation](https://docs.rs/asset-io).

## Command-line tool

Install from this checkout:

```sh
cargo install --path . --features cli
# Or run without installing:
cargo run --features cli --bin aio -- inspect 'tests/fixtures/*.jpeg'
```

```sh
aio inspect 'photos/*.jpg'
aio inspect video.mp4 --max-depth 3
aio inspect photo.jpg --segments
aio inspect photo.jpg --json
aio items image.heic
aio items photo.jpg --kind xmp --json
aio extract 'photos/*.jpg' --output-dir extracted/
aio extract photo.jpg --output-dir extracted/ --stored
aio extract photo.jpg --kind xmp --output metadata.xmp
aio extract image.heic --item 3 --output item.bin
aio update photo.jpg --xmp-file new.xmp --output updated.jpg
aio update 'photos/*.jpg' --remove-jumbf --output-dir updated/
aio update photo.jpg --xmp-file new.xmp --output updated.jpg --force
aio embedded 'photos/*.jpg'
```

Inspection commands (`inspect`, `items`, and `embedded`) default to readable
output; `--json` gives a versioned JSON array. Extraction and update retain JSON
as their default; `--human` requests a readable report.
Quote globs to let `aio` expand them. Unmatched patterns and per-file failures
produce a non-zero exit status, with per-file errors included in output.

`inspect` displays an element hierarchy with offsets, readable sizes, and exact
byte counts. Known element names accompany raw identifiers, for example
`SOI (0xFFD8)` and `APP11 (0xFFEB)` for JPEG markers, in both hierarchy and
segment views. The hierarchy also always shows each element's full path in a
separate `Path` column, for example `/moov/trak/mdia`. Paths are reported by the
container walker and can repeat for sibling elements with the same name.
JSON retains raw `name` and `path` values and provides `box_name` separately.
Parent sizes include their children and must not be summed.
`--segments` instead displays the non-overlapping physical segment table with
header, payload, and trailer byte counts. JSON retains segment and item details
and includes hierarchy `nodes` unless `--segments` is selected.

`inspect` also automatically scans for embedded files and shows each detected
file's container, media type, size, source item index, and logical items. This
includes opaque items whose bytes identify a supported format, such as the JPEG
stored in a HEIF `processedimage` URI item. The outer item's native kind remains
unchanged; its detected contents are reported separately. Unknown opaque bytes
are not treated as files, and URIs are never fetched.

Automatic scans use the library defaults: at most 4 nesting levels, 64 assets,
and 256 MiB of cumulative embedded asset sizes, without loading whole assets
into memory. Budget failures are reported as errors. Use `aio embedded` to set
different limits with `--max-depth`, `--max-assets`, and `--max-total-bytes`.
Both commands include `embedded_count` and an `embedded` array in JSON; each
entry's `source_item_index` refers to its parent's item list. Human-readable
output shows the embedded files' item tables as well.

`items` displays stored sizes, encodings, extent counts, labels, native identifiers,
media types, and relations. Its `Content` column identifies C2PA manifest stores
using the standard JUMBF type, while keeping their underlying kind as `jumbf`.
JSON exposes `content_type: "c2pa_manifest_store"` (otherwise `null`).
The same identification appears in `inspect` and embedded-item JSON. When the
type is not already recorded, inspection uses the library's bounded JUMBF header
reader, including transfer decoding where needed; it does not load the full store.
Identification does not validate manifests or verify signatures.
Both `items` and `extract`
accept `--item INDEX`, `--kind KIND`, and `--label LABEL`; combined filters are
intersected. Kinds are `xmp`, `jumbf`, `exif`, `embedded_asset`, and `other`.
Labels match exactly. Indexes refer to the original item list and are local to
the current file, not stable across edits.

Extraction with `--output-dir` writes decoded content to a separate subdirectory
per input and reports output paths; `--stored` assembles stored bytes across
extents without decoding. `--output` requires one input and exactly one selected
item; no match is an extraction error. Extracting a coded image item does not
transcode it into a standalone JPEG or PNG. Updates accept
`--xmp-file`, `--jumbf-file` or their removal flags and never edit inputs in
place. Use `--output` for one input or `--output-dir` for multiple inputs.
`embedded` scans recursively with resource limits.

Extraction and update refuse existing output files unless `--force` is given.
Neither overwrites an input, even with `--force`; directory and symlink
destinations are rejected. Each file is written to a temporary file beside its
destination, flushed and synced, then published atomically. Failed writes leave
existing destination files intact. Bulk operations are not a single transaction:
earlier successful files remain if a later file fails.

## Documentation

- [API documentation](https://docs.rs/asset-io): C2PA store access and hash
  geometry, exact patching, embedded assets, zero-copy reads and custom I/O.
- [Container handler guide](docs/container-handlers.md): implementing,
  registering and testing a format.
- [Synchronous inspection example](examples/inspect.rs).
- [Application C bindings](asset-io-c/README.md): a standalone package for C,
  C++, Python and Swift; not a dynamically loaded handler ABI.
- [Release notes](CHANGELOG.md) and [fuzzing setup](FUZZING_SETUP.md).

## License

[MIT](LICENSE-MIT) OR [Apache-2.0](LICENSE-APACHE)
