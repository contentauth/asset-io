# Using asset-io

This guide covers the asset-io model and its features in more depth than the
[README](../README.md). The [API documentation](https://docs.rs/asset-io) is the
complete reference.

- [The model](#the-model)
- [Opening assets](#opening-assets)
- [Reading items](#reading-items)
- [Updating metadata](#updating-metadata)
- [C2PA support](#c2pa-support)
- [Sub-assets and trailing data](#sub-assets-and-trailing-data)
- [Embedded files](#embedded-files)
- [Other metadata helpers](#other-metadata-helpers)
- [Sinks and zero-copy reads](#sinks-and-zero-copy-reads)
- [Async and custom I/O](#async-and-custom-io)
- [Safety](#safety)

## The model

A parsed file is a `Structure`. It holds geometry only: no file bytes, no
hashing or signing rules, and no interpretation of C2PA manifests or XMP
schemas. Callers decide what each byte range means.

- **Segments** form an ordered, gapless tiling of the whole file: the units a
  writer copies, replaces or moves. Each has framing (header and trailer
  **roles**) and a payload. Its `SegmentKind` gives its dominant purpose:
  `Preamble`, `Metadata`, `MediaData` or `Other`.
- **Elements** are the finest tiling: `elements` expands subdividing segments
  into their smaller structural units, such as JPEG restart intervals.
- **Boxes** group elements according to the container, such as a GIF application
  extension and its data sub-blocks. `boxes()` returns `BoxInfo` values containing
  a name, range and XMP/EXIF metadata ranges. Unnamed bytes have `name: None`.
  This view tiles the asset without C2PA-specific names or restrictions.
  `boxes_with(item, name)` groups each contiguous run of the selected item's
  segments under a supplied name, framing included. Both methods read nothing
  and work on planned structures.
  `supports_named_boxes()` reports whether the container's own structure is
  entirely named boxes, ignoring unnamed trailing bytes. It is a structural
  capability, not a choice of hashing or signing scheme.
- **Items** are the logical units a format defines, such as XMP, EXIF, a C2PA
  manifest store or an embedded file. Each has an `ItemKind` (the specification
  that defines it), an `Encoding` (its serialization) and a category derived from
  its kind (metadata or asset). A C2PA store is kind `C2pa` encoded as `Jumbf`;
  JUMBF of other types is kind `Other`, also encoded as `Jumbf`. An item's bytes are one or more **extents**, possibly spread across
  segments (JPEG XT packets, extended XMP, HEIF `iloc` extents).
- **Native identifiers** are kept exactly: `native_type()` (such as a HEIF item
  type), `native_id()` (a HEIF `item_ID`), `uri()`, `media_type()`,
  `jumbf_type()` and a container-defined `label()`.
- **Relations** are item-to-item references that keep their native type (`thmb`,
  `auxl`, `cdsc`, `dimg`) with a common `RelationKind`.
- A **transfer encoding** describes how stored bytes wrap the content: SVG's
  base64, a PDF `FlateDecode` stream, compressed PNG `iTXt` XMP, or a HEIF
  `content_encoding`. It is distinct from the content's `Encoding`.

`Structure::items_in(segment)` reports what a segment holds. Decide what a
segment contains from its items, not its kind.

asset-io exposes what each format's specifications define, including companion
specifications (EXIF/TIFF, MPF, JPEG XT, the HEIF item model). Where portable
content such as a C2PA manifest is placed is container knowledge and belongs
here; interpreting it does not.

## Opening assets

`Asset` pairs a `Structure` with the stream it was parsed from, so its reads,
writes and patches need no source argument. It dereferences to its `Structure`.
Its source may be owned (`File`) or borrowed (`&mut File`), and must not change
while the structure is in use.

- `Asset::open(source, format)` uses the built-in handlers of the enabled features.
- `Asset::open_with(&registry, source, format)` takes a `HandlerRegistry`.
  Libraries should use it so their callers control which handlers apply.

Content detection runs first and takes precedence over the optional format, so a
JPEG named `.png` still parses as JPEG. If nothing recognizes the content, an
extension or MIME type such as `Some("svg")` selects a handler. Formats without a
signature, such as SVG, require one.

Registries are composed at runtime. The last handler registered for a format
wins and is tried first during detection:

```rust,ignore
use asset_io::{Asset, HandlerRegistry};

let registry = HandlerRegistry::with_builtin()
    .with_handlers(other_crate::handlers())
    .with_handler(MyHandler);
let asset = Asset::open_with(&registry, std::fs::File::open("image.raw")?, None)?;
```

`HandlerRegistry::builtin()` contains only enabled built-in handlers; `new()` and
`default()` are empty. `extend_from(&other)` adds another registry's handlers.
`media_type_for("heic")` and `extension_for("image/jpeg")` resolve format names
and aliases to preferred values.

## Reading items

```rust,ignore
use asset_io::ItemKind;

if let Some(xmp) = asset.item(ItemKind::Xmp)? {
    let stored = asset.read_item(xmp)?;     // bytes as stored in the container
    let content = asset.read_content(xmp)?; // transfer encoding undone
}
for item in asset.items() {
    println!("{} ({}): {} bytes", item.kind(), item.encoding(), item.len());
}
```

`read_item` returns the stored bytes: what a hash covers and what `patch_exact`
overwrites. `read_content` undoes the transfer encoding and returns
`UnsupportedFeature` if its decoder (`flate` or `brotli`) is not enabled.

## Updating metadata

`Updates` describes XMP and C2PA manifest store changes; `write` streams a new
output:

```rust,ignore
use asset_io::{NullSink, Updates};

let updates = Updates::new().set_xmp(xmp_bytes).remove_c2pa();
let written = asset.write(&mut out, &updates, &mut NullSink)?;
```

ISO BMFF outputs must also be readable (`Read + Write + Seek`). `write` returns the
output's `Structure`; `Structure::plan(src, &updates)` computes it without writing,
including where each item lands and the output size.

- `set_c2pa` replaces the C2PA manifest store, or adds one where the container's
  C2PA embedding puts it; `remove_c2pa()` removes it. Other JUMBF is never
  changed. PNG, RIFF, ISO BMFF and most other containers reserve an element for
  the store. JPEG carries JUMBF of any type in APP11, so there a store must have
  the C2PA store superbox type, and a new store takes a box instance number (En)
  that no other JUMBF uses.
- Generic ISO BMFF boxes carried in JPEG XT APP11 packets are not JUMBF. They are
  `Other` items encoded as `IsoBmff`, and C2PA updates leave them alone.
- `set_c2pa(store).as_update()` marks the store as an update to the existing
  store. ISO BMFF stores only the addition and keeps the original manifests
  untouched; other containers ignore the flag. PDF needs no flag: it always writes
  only what a new store adds to the existing one, and keeps the earlier stores as
  `superseded` items (a store that does not extend the existing one is refused).
  `Structure::c2pa_store(src)` reads the combined store, and
  `jumbf::{addition_to, extend}` convert between forms.
- In ISO BMFF, XMP is written as the XMP `uuid` box; HEIF items, XMP items
  included, are carried through unchanged.
- Container-specific options use `Updates::set_extension`, for example
  `containers::bmff::BmffUpdates` for merkle boxes.

Edits to a multi-picture JPEG add metadata to the first image and update the
offsets and sizes in its MPF index. The edit fails rather than writing a stale
or ambiguous index. A first-image size recorded short of the image, as in Pixel
Ultra HDR files, is corrected.

`ContainerHandler::compose(kind, data)` returns a container's native encoding of
an item (JPEG APP11 segments, a PNG `caBX` chunk, a BMFF C2PA box) for callers that
insert it themselves. Resolve the handler with `registry.handler(format)` or
`container_api::handler(&structure)`. Callers using `write` don't need it.

## C2PA support

asset-io locates manifest stores and describes hash geometry. It does not
interpret manifests, compute hashes or sign.

### Finding the store

The manifest store is an `ItemKind::C2pa` item, encoded as JUMBF. Each handler
identifies it while parsing: from the element the container reserves for it, or,
where the container carries JUMBF of any type (JPEG APP11), from the superbox
type. Other JUMBF is an `ItemKind::Other` item with `Encoding::Jumbf` and its
`jumbf_type()`.

`c2pa_item()` returns the store item; it reads nothing, so it also works on
planned structures before the output exists. `c2pa_store()` returns its content
with transfer encoding undone and, for ISO BMFF, any `update` appended to its
`original`.

### Hard bindings

Choosing, building and checking a C2PA hard-binding assertion is the caller's
job, following the applicable container specification. `supports_named_boxes()`
only reports a structural capability; it does not select or recommend a C2PA
binding. For example, ISO BMFF has named boxes but C2PA defines its own
`c2pa.hash.bmff` rules.

`complete_boxes(group, trailing_name)` lists every byte of the asset as named
boxes, in order: what a `c2pa.hash.boxes` covers (C2PA 2.4, 18.7). Box names
come from the handler; for example, a JPEG scan with restart markers is its
`SOS` box plus one per `RSTn`, and a RIFF `LIST` chunk is named by its list
type. The caller names the rest: pass an item and a name to make it one box,
and a name for bytes after the structure. Each box lists the metadata ranges
(XMP, EXIF) a box hash may exclude. It reads nothing and works on planned
structures. An unnamed element inside the structure is `UnsupportedFeature`.

`c2pa_boxes()` is `complete_boxes` with the names C2PA uses: the manifest store
is the `C2PA` box and bytes after the structure are `c2pa.after`.

```rust,ignore
let boxes = asset.c2pa_boxes()?;
```

`c2pa_data_hash_exclusions(item)` returns the ranges a `c2pa.hash.data` excludes,
checked, sorted and merged. The default excludes the whole segments carrying the
store (JPEG APP11, PNG `caBX` with its CRC). RIFF keeps alignment padding hashed;
SVG excludes only the base64 text. ISO BMFF and PDF return `UnsupportedFeature`.

### Placeholders and patching

To sign without reparsing, write a placeholder store, hash the output, then
patch the signed store in place:

```rust,ignore
let written = asset.write(&mut out, &Updates::new().set_c2pa(placeholder), &mut NullSink)?;
let output = Asset::from_parts(written, out); // `out` must be Read + Write + Seek
let item = output.c2pa_item()?.expect("the placeholder");
let exclusions = output.c2pa_data_hash_exclusions(item)?;
// ... hash everything outside `exclusions` and sign ...
output.patch_exact(item, &signed_store)?;
```

The replacement must be exactly the placeholder's length.

## Sub-assets and trailing data

`subassets()` lists contiguous top-level parts covering every byte: the parts of a
C2PA multi-asset hash. Most files have one. A Multi-Picture Format JPEG (including
Ultra HDR) has one per image, plus any data after the last image. Each reports its
range, media type and segments; `relative(offset)` gives sub-asset-relative
offsets.

`trailing()` is the range after the container's own structure. A whole-asset box
hash lists it as one final `AFTER_BOX` (`c2pa.after`, C2PA 18.7.2); a sub-asset
hash never includes it. JPEG and PNG report trailing data; other containers do not
yet.

## Embedded files

`embedded_assets()` lists `EmbeddedAsset` items: those the format declares as
files of a media type. In HEIF these are `jpeg` items and `mime` items with a
valid content type other than XMP. Items without a declared media type, such as
HEIF `uri ` items and coded `hvc1` thumbnails, are `Other` items; URIs are never
fetched.

Parsing stays shallow. Opt in to recursive, bounded scanning with the same
registry, including any custom handlers:

```rust,no_run
use asset_io::{Asset, Encoding, HandlerRegistry, ScanLimits};

# fn main() -> asset_io::Result<()> {
let registry = HandlerRegistry::builtin();
let mut photo = Asset::open_with(&registry, std::fs::File::open("photo.heic")?, None)?;
let embedded = photo.scan_embedded(&registry, ScanLimits::default())?;
for found in &embedded {
    match &found.structure {
        Ok(child) => {
            let mut reader = found.reader(photo.source_mut())?;
            for item in child.items().filter(|i| i.encoding() == Encoding::IsoBmff) {
                let bytes = child.read_item(&mut reader, item)?;
                println!("{} bytes of box content", bytes.len());
            }
        }
        Err(error) => eprintln!("Embedded item {:?}: {error}", found.item.uri()),
    }
}
# Ok(())
# }
```

Results are depth-first with parent indices and depths. Child readers map
offsets back to the original source without copying, even through multi-extent
and nested items. Unsupported or malformed embedded assets keep their errors and
byte ranges. Opaque items are reported only when the registry recognizes their
leading bytes. Defaults allow depth 4, 64 assets and 256 MiB in total; depth is
capped at 32. Scanning never decodes pixels.

## Other metadata helpers

- `Structure::xmp()`: the XMP packet. For HEIF, the XMP `uuid` box if present,
  else the XMP item describing (`cdsc`) the primary item, else the only XMP item.
- `Structure::exif_info()` (feature `exif`): basic EXIF fields for JPEG, PNG, WebP
  and HEIF/AVIF.
- `Structure::embedded_thumbnail()`: a HEIF `thmb` item or, with `exif`, the JPEG
  thumbnail inside EXIF. Its `media_type()` is `None` when the container cannot
  tell, such as for coded `hvc1` or `av01` items.
- `MiniXmp` (feature `xmp`): minimal XMP reading and modification.

Print a file's structure and items with:

```sh
cargo run --example inspect --features all-formats,xmp,exif -- <file>
```

Add `--embedded` to include nested assets.

## Sinks and zero-copy reads

`read(sink)` and `write(out, updates, sink)` pass every byte to a `Sink`, in
order and tagged with its segment and role, so callers can hash while reading or
writing. Ranges the sink does not want are not read.

With bytes in memory, `Structure::slice`, `slice_item` and `read_slice` borrow
rather than copy. The `memory-mapped` feature adds `MappedFile`. Mapping is
`unsafe`: the file must not change while mapped, so do not map files that
untrusted parties can write.

## Async and custom I/O

### Tokio

Enable `tokio` with your format features to use `asset_io::tokio` with
`AsyncRead + AsyncSeek` sources and `AsyncWrite` destinations:

```rust,ignore
use asset_io::{tokio as asset, HandlerRegistry, ItemKind, NullSink, Updates};

let registry = HandlerRegistry::builtin();
let mut src = tokio::fs::File::open("image.jpg").await?;
let structure = asset::parse(&registry, &mut src, None).await?;
if let Some(item) = structure.item(ItemKind::Xmp)? {
    let xmp = asset::read_content(&structure, &mut src, item).await?;
}
let mut out = tokio::fs::File::create("out.jpg").await?;
asset::write(&structure, &mut src, &mut out, &Updates::new().remove_c2pa(), &mut NullSink).await?;
```

`write_plan` and `execute` separate planning from execution; `c2pa_item` and
`c2pa_store` find the store; `run` drives custom tasks. Writes start at the
destination's current position, flush on success, and may leave partial output on
error or cancellation. Sink callbacks are synchronous and should not block.

The [Tokio example](../examples/tokio_client.rs) processes files concurrently and
hashes outputs while writing. Its `edit` command reports parse, plan, write and
total times.

### Driving I/O yourself

The core does no I/O and needs no runtime. Parsing and planning are async
computations run by a `Task` (`asset_io::task`). When a task needs bytes,
`Task::resume` returns a `ReadRequest` with an offset and a buffer to fill; fill
it from any source and resume. `Io` keeps a 4 KiB window, so runs of small reads
become one request. Tasks are `Send`.

- `Structure::parse_task(&registry, total, format)` creates a parse task.
- `Structure::write_plan_task(&updates)` creates a planning task.
- `Task::new(total, move |io| handler.parse_async(io))` runs a known handler.

Writes run the same way: `WritePlan::start` (`asset_io::plan`) returns an
`Execution` (`asset_io::execute`) whose `step` yields `Action::Read`,
`Action::Write` or `Action::Done`. It handles sink calls, validation and read
coalescing, so every driver behaves like the blocking `execute`.

The `*_async` methods (`parse_async`, `c2pa_store_async`, `read_item_async`,
`read_content_async`) compose parsing,
store lookup and your own work into a single task.

## Safety

Lengths are checked, segment counts and nesting are capped, and malformed input
returns an error rather than panicking. Fuzz targets are in [`fuzz/`](../fuzz);
see the [fuzzing setup](../FUZZING_SETUP.md).
