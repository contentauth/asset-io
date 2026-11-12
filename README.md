# asset-io

[![crates.io](https://img.shields.io/crates/v/asset-io.svg)](https://crates.io/crates/asset-io)
[![Documentation](https://docs.rs/asset-io/badge.svg)](https://docs.rs/asset-io)
[![License](https://img.shields.io/crates/l/asset-io.svg)](#license)

Read, inspect and update media-file metadata through one Rust API.

`asset-io` gives image, audio, video and document formats a common model of
segments and metadata items. With it you can:

- Find and read XMP, EXIF, C2PA manifest stores, other JUMBF and embedded files.
- Replace or remove XMP and C2PA manifest stores without decoding or re-encoding
  media.
- Stream updates in one pass with bounded buffers, copying unchanged media.
- Hash while reading or writing, using a `Sink` that sees every byte tagged.
- Compute C2PA hash geometry (box lists and data-hash exclusions), even before writing.
- Add formats by registering your own handlers at runtime.

The core is runtime-independent and supports blocking, Tokio and caller-driven
I/O. Used by [c2pa-rs](https://github.com/contentauth/c2pa-rs), it handles
container structure and metadata placement, **not** manifest interpretation,
hashing or signing.

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
    let updates = Updates::new().set_xmp(xmp).remove_c2pa();
    let mut output = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open("updated.jpg")?;
    asset.write(&mut output, &updates, &mut NullSink)?;
    Ok(())
}
```

The input is never modified. Content detection takes precedence over the
optional format argument; pass an extension or MIME type, such as `Some("svg")`,
for formats without a signature.

The main types:

- **`Asset`**: a parsed file with its blocking stream. Start here.
- **`Structure`**: segments, items and byte ranges, without owning the bytes.
  Use it for async I/O, write planning and hash geometry.
- **`Updates`**: XMP and C2PA manifest store changes. `write` returns the output's structure;
  `plan` computes it without writing.
- **`HandlerRegistry`**: built-in and custom handlers. Libraries should accept a
  registry and call `Asset::open_with`, so their callers control format support.

The [user guide](https://github.com/contentauth/asset-io/blob/main/docs/guide.md) covers the model, C2PA
support, embedded files and custom I/O in depth.

## Supported formats

Each feature enables one built-in handler that reads and updates metadata.
`all-formats` enables all of them.

| Feature | Formats | C2PA hash | Notes |
|---------|---------|-----------|-------|
| `jpeg` | JPEG, MPO | Box | Multi-Picture Format and Ultra HDR images; JPEG XT JUMBF |
| `png` | PNG | Box | |
| `bmff` | MP4, MOV, M4A, M4V, HEIF, HEIC, AVIF | BMFF | Rewrites chunk offsets; merkle boxes |
| `riff` | WebP, WAV, AVI | Box | |
| `tiff` | TIFF, BigTIFF, DNG; ARW and NEF read-only | Data | [Append-only IFD updates](https://github.com/contentauth/asset-io/blob/main/src/containers/tiff/README.md) |
| `gif` | GIF | Box | [Application extensions](https://github.com/contentauth/asset-io/blob/main/src/containers/gif/mod.rs) |
| `id3` | MP3, FLAC | Data | [Leading ID3v2.3/2.4 tag](https://github.com/contentauth/asset-io/blob/main/src/containers/id3/mod.rs) |
| `svg` | SVG | Data | Splices metadata without re-serializing |
| `pdf` | PDF 1.0–2.0 | — | [Incremental updates](https://github.com/contentauth/asset-io/blob/main/src/containers/pdf/README.md) |
| `c2pa` | C2PA sidecars (`.c2pa`) | — | [Whole-file manifest store](https://github.com/contentauth/asset-io/blob/main/src/containers/c2pa/README.md) |

Optional features:

| Feature | Adds |
|---------|------|
| `tokio` | [Tokio adapters](https://docs.rs/asset-io/latest/asset_io/tokio/) for async parsing, reads and writes |
| `xmp` | `MiniXmp` for minimal XMP reading and modification |
| `exif` | Basic EXIF fields and EXIF JPEG thumbnails |
| `flate` | zlib, deflate and gzip decoding for stored content; enabled by `pdf` |
| `brotli` | Brotli decoding for stored content |
| `memory-mapped` | `MappedFile` for zero-copy reads |

`all-formats` does not enable the optional features.

## Examples

```sh
# Print a file's structure and items.
cargo run --example inspect --features all-formats,xmp,exif -- photo.jpg

# Inspect or edit files concurrently with Tokio, hashing output as it is written.
cargo run --example tokio_client --features all-formats,tokio -- inspect photo.jpg
```

## Documentation

- [API documentation](https://docs.rs/asset-io)
- [User guide](https://github.com/contentauth/asset-io/blob/main/docs/guide.md): the model, updates, C2PA, embedded files and I/O
- [Container handler guide](https://github.com/contentauth/asset-io/blob/main/docs/container-handlers.md):
  writing, testing and adding format handlers
- [C bindings](https://github.com/contentauth/asset-io/blob/main/asset-io-c/README.md) for C, C++, Python and Swift,
  built from this repository
- [Release notes](https://github.com/contentauth/asset-io/blob/main/CHANGELOG.md)

## Contributions and feedback

We welcome contributions to this project. For information on contributing,
providing feedback, and about ongoing work, see
[Contributing](https://github.com/contentauth/asset-io/blob/main/CONTRIBUTING.md).

## License

Licensed under either of [MIT](https://github.com/contentauth/asset-io/blob/main/LICENSE-MIT) or
[Apache-2.0](https://github.com/contentauth/asset-io/blob/main/LICENSE-APACHE), at your option.
