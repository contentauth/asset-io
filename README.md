# asset-io

A portable, dynamically extensible container model for media assets.

`asset-io` provides one abstract model for reading, inspecting and updating a wide
variety of image, audio, video and document formats, including JPEG, PNG, ISO BMFF
(HEIF, AVIF, MP4), RIFF (WebP, WAV, AVI), TIFF, SVG and PDF. Applications work with
common structures and metadata items rather than format-specific parsers and writers.

Container support is dynamically extensible: applications register built-in or
third-party handlers with `HandlerRegistry` at runtime. Handler authors use
`container_api` to add formats without changing the client API.

The core is portable and independent of any async runtime. The same handler
implementation runs with blocking streams, async adapters or caller-driven I/O.
It is designed for high performance: updates stream the output in a single pass
with bounded buffers, copying unchanged media without loading the whole asset
into memory. Sinks observe tagged bytes during reads and writes, allowing callers
to hash output without a second pass.

Used by [c2pa-rs](https://github.com/contentauth/c2pa-rs) for media-container access,
`asset-io` separates container mechanics from manifest interpretation, hashing and signing.

See the [release notes](CHANGELOG.md).

A parsed file is a `Structure`: a gapless tiling of `Segment`s (header, payload, trailer) plus `Item`s, the logical units the format defines (XMP, JUMBF, EXIF, embedded files), each assembled from one or more `Extent`s. It holds geometry only. It does not hash, sign or interpret C2PA; callers decide what each byte range means.

## Concepts

- A **structure** (`Structure`) is the parsed description of an asset: its segments, items and sub-assets, and the handler that parsed it. It holds no bytes, so its reads take the stream as an argument. An **`Asset`** pairs a structure with the stream it was parsed from, so its reads need no stream argument. A planned structure (`Structure::plan`) describes an output before it is written.
- A **segment** is a unit of the tiling: what a writer copies, replaces or moves. Its `SegmentKind` is its dominant purpose: `Preamble`, `Metadata`, `MediaData` or `Other`. Its **role**s split it into framing (header, trailer) and payload.
- An **item** has an `ItemKind` (the specification that defines it: `Xmp`, `Jumbf`, `Exif`, `EmbeddedAsset`, `Other`), an `Encoding` (`Xml`, `Tiff`, `Jumbf`, `IsoBmff`, `Asset`, `Unknown`) and a category derived from its kind (metadata or asset). Its native identifiers are kept as they are: `native_type()` (a HEIF item type such as `jpeg` or `hvc1`), `native_id()` (a HEIF `item_ID`), `uri()`, `media_type()`, `jumbf_type()`, and a container-defined `label()` (`manifest`, `update`, `merkle`, ...).
- A **relation** is a reference from one item to another that keeps its native type (`thmb`, `auxl`, `cdsc`, `dimg`), with a common `RelationKind`.
- An item's **transfer encoding** (`TransferEncoding`: `Identity`, `Base64`, `Zlib`, `Deflate`, `Gzip`, `Brotli`, or `Unknown` with the format's own label) says how the bytes the container stores wrap the content: SVG stores a manifest as base64 text, PDF may store metadata as a zlib (`FlateDecode`) stream, PNG may hold compressed XMP in `iTXt`, and a HEIF `mime` item names a `content_encoding`. It is distinct from the `Encoding` of the content. The wrapping is always identified; undoing a compression is optional. `read_item` returns the stored bytes, which are what a hash covers and what `patch_exact` overwrites; `read_content` returns the content, or `UnsupportedFeature` when the decoder is not built in.
- An item's bytes are its **extents**; an item can be multi-extent (JPEG XT packets, extended XMP, HEIF `iloc` extents). "Fragment" is reserved for fragmented ISO BMFF.
- `Structure::items_in(segment)` says what a segment holds; writers decide what to do with a segment from its items, never from its kind.
- `Structure::c2pa_item(src)` and `Structure::c2pa_store(src)` find the C2PA
  manifest store, as `xmp()` finds XMP: the JUMBF item whose superbox type is
  `jumbf::C2PA_STORE_TYPE`, ignoring other JUMBF (such as JPEG XT boxes of other
  types). `c2pa_store` returns its content with any transfer encoding undone and,
  for ISO BMFF, an `update` appended to its `original`. The manifests in it are
  not interpreted. `store()` and the item APIs still give any JUMBF. Handlers
  record each JUMBF item's type when they parse, and planned structures carry it
  from the source or the bytes being written, so the store is found in a plan
  before the output exists.
- `Structure::c2pa_hash()` names the C2PA hard binding an embedded manifest in
  the asset calls for: `C2paHash::Bmff` for ISO BMFF, `C2paHash::Box` when
  every element of the container has a box name (JPEG, PNG, RIFF),
  `C2paHash::Data` for containers without meaningful boxes (SVG), and `None`
  where asset-io cannot support one yet (PDF, sidecars). A caller may choose a
  data hash instead of a box hash. Building and checking the assertion is the
  caller's.
- `Structure::c2pa_boxes(src)` lists the boxes a `c2pa.hash.boxes` covers
  (C2PA 2.4, 18.7), in file order and tiling the asset. Each element is a box,
  named by the handler's `c2pa_box_name`: a JPEG scan with restart markers is
  its `SOS` box plus one box per `RSTn`, a RIFF chunk is a 12-byte box named by
  its form (`WAVE`, `AVIX`) and a `LIST` chunk a box named by its list type
  (`INFO`, `movi`) holding everything nested in it. The later images of a
  Multi-Picture Format JPEG are listed box by box. The C2PA store's segments
  are one `C2PA` box and unnamed bytes at the end one `c2pa.after` box. Each
  box carries the ranges of asset metadata (XMP, EXIF) inside it, the only
  ranges a box hash may exclude besides the store. It works on planned
  structures, so an output's boxes are known before it is written. The finer elements (restart intervals) are
  recorded when the asset is parsed, so listing boxes reads nothing beyond
  finding the store; `c2pa_boxes_with(item)` takes the store's item and reads
  nothing at all.
- The reads have `async` forms over an `Io` — `c2pa_item_async`,
  `c2pa_store_async`, `jumbf_type_async`, `read_item_async`,
  `read_content_async`, and `Structure::parse_async(registry, io, format)` —
  so a caller composes parsing, finding the store and
  its own work into one sans-I/O `Task` and answers its read requests in one
  place. Finding the item reads nothing when the parser recorded each JUMBF
  item's type, as the built-in handlers do.
- `Structure::c2pa_data_hash_exclusions(item)` is where C2PA embedding rules
  reach the container: the ranges a `c2pa.hash.data` excludes for a
  JUMBF item such as `c2pa_item`, on parsed or planned structures. By default it
  excludes the whole segments carrying the item (JPEG APP11, PNG `caBX` with its
  CRC). RIFF overrides it to keep alignment padding hashed and SVG to exclude
  only the base64 text. BMFF and PDF report `UnsupportedFeature`, since they bind
  differently. Ranges are checked, sorted and merged. It does not interpret the
  manifest or compute a hash.

asset-io exposes what each format's own specifications define, including companion specifications (EXIF/TIFF, MPF, JPEG XT, the HEIF item model), and never interprets content that is portable across formats (C2PA manifests, XMP schemas); that belongs to higher-level helpers built on this API. Where such content is placed, including which item is the C2PA manifest store, is container knowledge and is here. See the crate documentation for the full glossary.

## Crate layout and features

`asset-io` is one crate. Each built-in container is a module under
`asset_io::containers` behind a Cargo feature, so a build compiles only the
containers it enables:

```toml
[dependencies]
asset-io = { version = "0.3", features = ["jpeg", "pdf"] }
```

No container is enabled by default. `HandlerRegistry::builtin()` holds the
handlers of the enabled features; `new()` and `default()` are always empty.
Without any container feature the crate is the handler-free API: structures,
registry, I/O, write plans, execution and the conformance checks.

The Rust namespaces separate clients from handler authors:

| Namespace | Surface |
|-----------|---------|
| `asset_io` | Registry, read-only `Asset`/`Structure` model, reads, writes, `Updates`, `Sink`, C2PA geometry and exact patching |
| `asset_io::container_api` | `ContainerHandler`, `FormatSpec`, `StructureBuilder`, `ItemInfo`, `PlanBuilder`, metadata-update decisions, handler I/O traits and container utilities |
| `asset_io::plan` | `WritePlan`, `Step`, validation and offset relocation |
| `asset_io::io` | `Io`, `Reader`, `BoxFuture`, default read window |
| `asset_io::task` | `Task`, `Progress`, `ReadRequest`, `block_on` |
| `asset_io::execute` | `Execution`, `Action` for caller-driven writes |
| `asset_io::containers` | Feature-gated built-in handler factories and typed options |

Author types and advanced driver types are not also exported at the root.
This is a Rust import-path change, not a change to container behavior or the C ABI.

Handlers in other crates depend on `asset-io` with `default-features = false`,
so they do not force any container on the applications that use them, and are
registered at runtime. A registry is composed by adding handlers; the last
registered wins a format string and is tried first when detecting:

```rust
use asset_io::{Asset, HandlerRegistry};

// The built-in handlers, then a crate's handlers and the application's own.
let registry = HandlerRegistry::with_builtin()
    .with_handlers(other_crate::handlers())
    .with_handler(MyHandler);
let asset = Asset::open_with(&registry, std::fs::File::open("image.raw")?, None)?;
```

`extend_from(&other)` adds another registry's handlers the same way.

Container modules keep the boundaries separate crates would:
[`tests/container_isolation.rs`](tests/container_isolation.rs) checks that the rest
of the crate depends on no container, that no container uses another, and that
containers reach the crate only through its public API.

### Tests

Each container's unit tests live in its module; its integration tests are in
[`tests/`](tests) and require its feature. The real-file fixtures in
[`tests/fixtures`](tests/fixtures) are shared by those tests and the C bindings.
Core tests use synthetic handlers and run with no features.

```sh
cargo test --no-default-features
cargo test --no-default-features --features jpeg
cargo test --all-features
```

`make ci` runs the format check, the docs build, Clippy, the handler-free build,
each container on its own, and everything together. Formatting follows c2pa-rs:
`rustfmt.toml` uses options only nightly rustfmt has, so format with `make fmt`
(`cargo +nightly fmt`).

## Containers

| Feature | Container | Read | Write |
|---------|-----------|------|-------|
| `jpeg`  | JPEG      | yes  | yes   |
| `png`   | PNG       | yes  | yes   |
| `bmff`  | ISO BMFF (MP4, HEIC, AVIF, ...) | yes | yes, with streaming chunk-offset rewrite and merkle boxes |
| `riff`  | RIFF (WebP, WAV, AVI) | yes | yes; WebP XMP maintains the `VP8X` flag |
| `svg`   | SVG       | yes  | yes; a splice, nothing re-serialized; the manifest is base64 text |
| `pdf`   | PDF 1.0–2.0 | XMP and the document's C2PA store, found through the catalog | appends an incremental revision; see [its module documentation](src/containers/pdf/README.md) |
| `tiff`  | TIFF, BigTIFF, DNG | yes | yes, append-only IFD updates; see [its module documentation](src/containers/tiff/README.md) |
| `c2pa`  | Raw C2PA sidecar | entire file as one JUMBF item | replaces the file; removal emits an empty sidecar; see [its module documentation](src/containers/c2pa/README.md) |

Decoders for transfer encodings are optional features, in neither the defaults nor `all-formats`:

| Feature | Decodes |
|---------|---------|
| `flate` | zlib, raw deflate and gzip (pure-Rust `flate2`); enabled by `pdf` |
| `brotli` | Brotli (pure-Rust `brotli-decompressor`) |

Containers implement `ContainerHandler` and are found through `HandlerRegistry`, by format string or by content. Callers can register their own handlers.

## Use

Enable the `jpeg` feature for this example.

The API separates resolution, format behavior, and I/O execution:

| Component | Role |
|-----------|------|
| `HandlerRegistry` | Register handlers, resolve format strings, match supplied header bytes, and query format metadata; no stream I/O |
| `container_api::ContainerHandler` | Implement container-specific parsing, planning, and format rules |
| `Structure::parse`, `parse_async`, `parse_task` | Select a handler and run its parser with blocking I/O, inside a sans-I/O computation, or as a caller-driven task |
| `Structure` | Describe a parsed asset and operate through its retained handler; I/O-free, so reads take the source as an argument |
| `Asset` | A `Structure` together with the blocking stream it was parsed from, so reads, writes and patches need no source argument |
| `asset_io::tokio` | Drive parsing, reads, and writes with Tokio streams |

`Asset::open(source, format)` uses the built-in handlers;
`Asset::open_with(&registry, source, format)` takes a registry, and is what a
library should use so its caller's handlers apply.

The parsing functions and `Asset::open` take an optional format: `Some("jpg")` or
`Some("image/jpeg")` selects the named handler, while `None` detects the format.
An unsupported explicit format is an error, never a request to try detection.
Handlers without a detector (such as SVG) require an explicit format.

```rust
use asset_io::{Asset, ItemKind, NullSink, Updates};

let asset = Asset::open(std::fs::File::open("image.jpg")?, None)?;

if let Some(jumbf) = asset.item(ItemKind::Jumbf)? {
    let bytes = asset.read_item(jumbf)?;
}
for item in asset.items() {
    println!("{} ({}): {} bytes", item.kind(), item.encoding(), item.len());
}

let mut out = std::fs::File::create("out.jpg")?; // BMFF also needs Read
let updates = Updates::new().set_xmp(xmp_bytes).remove_jumbf();
asset.write(&mut out, &updates, &mut NullSink)?;
```

`Asset` dereferences to its `Structure`, so geometry such as `items()`,
`segments()` and `c2pa_data_hash_exclusions(item)` is available directly. Its
source may be owned (`File`) or borrowed (`&mut File`). Methods take `&self`, so
an item found with `asset.c2pa_item()?` can be passed straight back to
`read_content`, `c2pa_boxes_with` or `patch_exact`.

To fill a placeholder in an output without parsing it again, pair the structure
`write` returns with the output stream:

```rust
let written = asset.write(&mut out, &Updates::new().set_jumbf(placeholder), &mut NullSink)?;
let output = Asset::from_parts(written, out); // `out` must be Read + Write + Seek
let item = output.c2pa_item()?.expect("the placeholder");
let exclusions = output.c2pa_data_hash_exclusions(item)?;
// ... hash everything outside `exclusions` and sign ...
output.patch_exact(item, &signed_store)?;
```

`Structure` remains the I/O-free core: async and caller-driven code composes
its `*_async` and `*_task` methods directly.

### Tokio

Enable `tokio` alongside the container features to use `asset_io::tokio` with
Tokio's `AsyncRead + AsyncSeek` sources and `AsyncWrite` destinations:

```rust,no_run
# async fn example() -> asset_io::Result<()> {
use asset_io::{tokio as asset, HandlerRegistry, ItemKind, NullSink, Updates};

let registry = HandlerRegistry::builtin();
let mut src = tokio::fs::File::open("image.jpg").await?;
let structure = asset::parse(&registry, &mut src, None).await?;
if let Some(item) = structure.item(ItemKind::Xmp)? {
    let xmp = asset::read_content(&structure, &mut src, item).await?;
    println!("{} bytes of XMP", xmp.len());
}
let updates = Updates::new().remove_jumbf();
let mut out = tokio::fs::File::create("out.jpg").await?;
asset::write(&structure, &mut src, &mut out, &updates, &mut NullSink).await?;
# Ok(())
# }
```

The adapters hide task requests and write steps; the core remains sans-I/O.
`write_plan` and `execute` expose planning and execution separately when callers
need the output layout before writing. `c2pa_item` and `c2pa_store` provide async
store access, and `run` drives custom tasks composed from the core async methods.
Sources must remain unchanged after parsing. Writes start at the destination's
current position, flush on success, and may leave partial output on errors or
cancellation. Sink callbacks are synchronous and should not block.

The [Tokio example](examples/tokio_client.rs) uses these adapters; the
[synchronous inspect example](examples/inspect.rs) uses the blocking APIs.

```sh
cargo run --example tokio_client --features all-formats,tokio -- inspect image.jpg
```

The [HTTPS range example](examples/https_range.rs) shows the same parser over a
seekable URL-backed stream. It issues bounded 64 KiB `Range` requests, including
for offsets above 4 GiB, and requires the server to provide a `HEAD` content
length and honor range requests with `206 Partial Content`. It does not fall
back to downloading the full object if ranges are unsupported:

```sh
cargo run --example https_range --features all-formats,tokio,https-example -- https://example.com/video.mp4
```

This example demonstrates container parsing and structural inspection only.
`asset-io` does not validate C2PA signatures or content hashes; an application
can use the same seekable source with its higher-level validation logic. As
with all asset-io sources, the remote object must stay unchanged for the duration
of validation; use a versioned or immutable URL when that guarantee matters.

## Sinks

`write(src, out, updates, sink)` and `read(src, sink)` both end with a `Sink`. It receives every byte, in order, tagged with its segment and role, so a caller can hash while reading or writing. Runs the sink doesn't `want` are not read.

## Updates

`set_jumbf(store).as_update()` marks the JUMBF as an update to the existing store. ISO BMFF uses it to keep the original manifests untouched and store only the addition. Other containers ignore it.

`Structure::patch_exact` fills a placeholder in place; the replacement must be the same length.

`set_jumbf` replaces the JUMBF of the same type (the type in its description box) and leaves JUMBF of other types alone; `remove_jumbf_of(type)` removes one type, and `remove_jumbf()` removes all. This applies to JPEG, which carries JUMBF of any type. `Item::jumbf_type()` gives each item's type. Generic ISO BMFF boxes that JPEG XT carries in APP11 (Apple writes several) are not JUMBF: they are `Other` items encoded as `IsoBmff`, and JUMBF updates leave them alone. PNG, RIFF and ISO BMFF reserve an element for the manifest store, and the type does not select anything there.

`Structure::plan(src, &updates)` returns the structure `write` would produce, without writing: where each item would land and the output size.

The `Updates` API is deliberately specific to XMP and JUMBF, whose placement each container knows. Replacing, removing and inserting any item by its identity and encoding is the planned direction. In ISO BMFF, XMP is written as the XMP `uuid` box; HEIF items, XMP items included, are carried through a write unchanged.

`Structure::store(src)` reads the JUMBF store as a reader sees it. For ISO BMFF that combines an original store and its update; `jumbf::{addition_to, extend}` convert between the two forms.

`container_api::ContainerHandler::compose(kind, data)` returns the container's own encoding of an item (JPEG APP11 segments, a PNG `caBX` chunk, a BMFF C2PA box), for callers that insert it into an asset themselves. Resolve the handler with `registry.handler(format)`, or use `container_api::handler(&structure)` for a parsed asset. Registry handler lookup, iteration and composition remain available. Callers that use `write` don't need it.

Each handler has an `id()` (`"jpeg"`, `"png"`, `"bmff"`, `"riff"`, `"svg"`, `"pdf"`, `"tiff"`, `"c2pa"`) and declares the formats it serves as `FormatSpec`s: a media type, a preferred extension, and any other accepted media types and extensions. `HandlerRegistry::media_type_for("heic")` gives `image/heic` and `extension_for("image/jpeg")` gives `jpg`; aliases resolve to the preferred values (`image/x-webp` gives `image/webp`).

## Sub-assets

`Structure::subassets()` lists the sub-assets of an asset (the "parts" of a C2PA multi-asset hash): contiguous, in file order, covering every byte, each with its absolute `range()`, media type, segments, and a `relative(offset)` helper for sub-asset-relative hash ranges. A file that embeds nothing else has one. A JPEG that holds several images (Multi-Picture Format, Ultra HDR) has one per image, plus one for any data after the last image; their markers are listed in order, so each can be hashed on its own for a C2PA multi-asset hash. asset-io reports the geometry; the caller decides how to hash it.

`Structure::trailing()` is the range of bytes beyond the end of the container's own structure: data after the last chunk, or everything after the first image of a multi-image JPEG. A box hash of the whole asset lists the structure's boxes and then these bytes as one last box named `AFTER_BOX` (`c2pa.after`, C2PA 18.7.2); the hash of a single sub-asset never includes it. JPEG and PNG report it; the other containers do not yet, so bytes after their last box are not covered by a box hash.

Edits to a multi-picture JPEG add metadata to the first image and shift the sizes and offsets recorded in its MPF index to match. The edit fails instead of writing a stale or ambiguous index: the index must be well formed, appear once, and record the offset of every image and the size of every image but the first exactly. The first image's size may be recorded short of the image, as in Pixel Ultra HDR files, and is corrected on an edit. Parsing and copying unchanged never consult it. Bytes after an image that start like another image but do not parse as one are trailing data.

## Writing a handler

A handler implements `container_api::ContainerHandler`. Import its author API
explicitly, alongside the client model:

```rust
use asset_io::container_api::{
    BoxFuture, ContainerHandler, FormatSpec, Io, ItemInfo, MetadataSlot,
    PlanBuilder, StructureBuilder, WritePlan,
};
use asset_io::{Result, Structure, Updates};
```

Handlers use `Io` and `BoxFuture` for asynchronous reads, without constructing or
driving a `Task`. `container_api` also exposes direct plan construction,
replacement and relocation primitives for formats whose planning needs them,
`ReadSeek`/`ReadWriteSeek` for stream hooks, `Crc32` for framing repair, parser
limits, and `block_on` for optimized nested planning without an async runtime.
The shared reader implementation lives in [`src/io.rs`](src/io.rs);
client task execution lives in [`src/task.rs`](src/task.rs).

Raw author state is accessed through `container_api::attachment::<T>(&structure)`,
`container_api::handler(&structure)` and `container_api::item_info(&item)`,
not inherent client-model methods. Builders still attach opaque state and
carry descriptive item fields; clients read each field through `Item`'s
read-only accessors.

- `parse_async(self: Arc<Self>, io: Io)` builds a `Structure` with `StructureBuilder`, reading the stream through `io` (`io.read_at(offset, &mut buf).await?`, `io.reader(at)` for a run of fields, `io.fetch(..)` for a borrowed slice). Everything but the reads is ordinary code. Return `Box::pin(async move { ... })`.
- `write_plan_async(&self, structure, io, updates)` describes a write as a `WritePlan`: the structure of the output plus ordered `Step`s that copy source bytes or supply new ones. It reads the source only for what it must know (`io`), and does no writing. A handler that cannot write leaves the default. Decide what the output holds from `Updates` (`xmp()`, `jumbf()`, ...; a `MetadataSlot` says whether each existing XMP or JUMBF item is kept, replaced or dropped, and what is left to insert), then describe it in order with a `PlanBuilder`: `copy_segment(id)` for a source segment kept, `put_segment(kind, name, header, payload, trailer)` for a new one, `item(ItemInfo, &[id])` for the item it holds, `carry_items(|item| ..)` for the items of the kept segments, and `finish()`. Decide what a source segment holds from `Structure::items_in(id)`, not from its kind. The builder works out sizes, offsets and the output structure. When a segment is part copied and part changed, `shape_like`/`shape` add it and `copy`, `put` and `relocate` supply its bytes.

The blocking `parse`, `write_plan` and `write` are provided: they run those methods against a `Read + Seek`. The default `write` runs the plan with `WritePlan::execute`, which coalesces reads, feeds the sink and checks the bytes against the structure and role. `plan::validate` checks a plan without any I/O, for a handler's tests. A container with absolute file offsets (ISO BMFF chunk tables) uses `Step::Relocate`, which copies a table of offsets and rewrites them from an `OffsetMap`. Read what to change from `Updates` (`xmp()`, `jumbf()`, ...). Options only one container has live outside `Updates`: attach them with `Updates::set_extension` and read them with `Updates::extension` (ISO BMFF's merkle boxes are `containers::bmff::BmffUpdates`). [`tests/external_handler.rs`](tests/external_handler.rs) is a complete handler written with only the public API. Test it with `testing::Conformance::new(handler).check(name, &sample_bytes)`: it checks the promises every handler makes (the structure tiles the stream, a no-op write is byte-identical, plan, write and stepper agree, the sink sees every byte once and in order, set and remove read back, damaged input never panics), so a handler's own tests cover only what is particular to its format.

Register a handler with `HandlerRegistry::with_handler`; the last handler registered for a format wins. Enums and structs a handler touches are `#[non_exhaustive]`, so new variants and fields are not breaking changes.

## Running with your own I/O

Import `Io`/`Reader` from `asset_io::io`, `Task`/`Progress`/`ReadRequest` and
`block_on` from `asset_io::task`, `WritePlan`/`Step` from `asset_io::plan`,
and `Execution`/`Action` from `asset_io::execute`.

Parsing and planning are `async` code that does no I/O: a `Task` runs it, and when it needs bytes it does not hold (`Io` keeps a 4 KiB window, so a run of tiny reads is one request) `Task::resume` returns a `ReadRequest` with the offset, how much, and the buffer to fill. Fill it however you like (blocking, `async`, from a buffer you already have) and resume. No runtime is involved, and a task is `Send`. Get one from `Structure::parse_task(&registry, total, format)` for a parse, or `Structure::write_plan_task(&updates)` for a plan. `Structure::parse_task` borrows the registry and optional format, and reports selection errors when driven. For a known handler, `Task::new(total, move |io| handler.parse_async(io))` can create a task independent of the registry.

A `WritePlan` runs the same way: `WritePlan::start` gives an `Execution` whose `step` returns `Action::Read { offset, buf }`, `Action::Write(bytes)` or `Action::Done`, and does the sink calls, the checking and the read coalescing itself, so every way of running a plan behaves like `execute`. The blocking calls (`Structure::parse`, `Structure::write`) are these drivers over a `Read + Seek` and a `Write`.

`examples/tokio_client.rs` uses the feature-gated `asset_io::tokio` adapters with
`tokio::fs`, one spawned task per file, hashing the output as it is written.
`tests/tokio_adapter.rs` checks adapter parity with the blocking API and error
propagation; `tests/tokio_driver.rs` also checks low-level task driving and writes
against the blocking API byte for byte.

Its `edit` command reports wall-clock times for parsing, planning, writing with SHA-256, and the total edit. Writing includes output creation and flushing, but not an explicit disk sync. The total excludes loading the replacement XMP, task spawning, and compilation.

## Zero-copy

With the bytes in memory, `Structure::slice`, `slice_item` and `read_slice` borrow instead of copying. The `memory-mapped` feature adds `MappedFile` for that. Mapping is `unsafe`: the file must not change while mapped, so don't map files untrusted parties can write.

## Other modules

- `MiniXmp` (feature `xmp`): minimal XMP read/modify.
- `Structure::exif_info` (feature `exif`): basic EXIF fields, for containers that report EXIF items: JPEG, PNG, WebP, and HEIF/AVIF (the `Exif` item).
- `Structure::xmp`: the XMP packet. For HEIF it is the XMP `uuid` box if there is one, else the XMP item that describes (`cdsc`) the primary item, else the only XMP item.
- `Structure::embedded_thumbnail`: an item with a `Thumbnail` relation (a HEIF `thmb` reference; its data is the coded item unless the item is a `jpeg` or a `mime` item), or with the `exif` feature the JPEG thumbnail inside EXIF data. `Thumbnail::media_type()` says what the data is, or `None` when the container cannot tell (a coded `hvc1`/`av01` item, which is an `Other` item, not an embedded asset); `Item::media_type()` and `SubAsset::media_type()` report media types the same way.

Run `cargo run --example inspect --features all-formats,xmp,exif -- <file>` to print a file's structure and items.

### Embedded files

`Structure::embedded_assets()` lists the `EmbeddedAsset` items: those the format
declares to be files of a media type. In HEIF these are `jpeg` items and `mime`
items with a valid content type other than XMP. A `uri ` item declares no media
type, so it is an `Other` item of unknown encoding, keeping the URI that
identifies its purpose in `Item::uri()`; a URI is never fetched. Apple's
reference image (`tag:apple.com,2025:photo:metadata:provenance:processedimage`)
is such an item. So are `mime` items without a valid content type, and coded HEIF
images such as an `hvc1` thumbnail, which also keep their thumbnail relation. EXIF-internal thumbnails remain available
through `embedded_thumbnail`; MPF images remain `subassets`.

Parsing stays shallow. Opt in to recursive scanning with the same registry,
including any custom handlers:

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
                // The application interprets these bytes.
                println!("{} bytes of box content", bytes.len());
            }
        }
        Err(error) => eprintln!("Embedded item {:?}: {error}", found.item.uri()),
    }
}
# Ok(())
# }
```

Results are depth-first, with parent indices and nesting depth. Child offsets
are relative to the child's stream; `asset.reader()` maps them back to the
original source, even through multi-extent and nested items, without copying
image data. Every embedded asset is reported; one that is unsupported or malformed retains
its error and raw byte ranges, so siblings remain inspectable. Opaque items
(`Other`, unknown encoding, stored as is) are reported only when the registry
recognises their leading bytes; the rest are skipped and count against no
limit. Apple's processed image is found this way as a JPEG, while its two
styles `uri ` items are not reported. Budget overruns fail the scan explicitly:
defaults are depth 4, 64 assets and 256 MiB of cumulative embedded lengths.
Depth is always capped at 32. Scanning follows declared items, not arbitrary
file headers hidden in opaque payloads, and never decodes pixels.

JPEG exposes JUMBF superboxes carried in JPEG XT APP11 packets as `Jumbf` items,
and generic `uuid` boxes carried the same way as `Other` items encoded as
`IsoBmff`. Transport headers are removed and packet bodies reassembled. A UUID box
is not a JUMBF superbox, so its `jumbf_type()` is `None`; interpreting its UUID
and payload belongs to the application.

Add `--embedded` to the `inspect` command to print nested metadata and raw box
prefixes.

## Safety

Lengths are checked, segment counts and nesting are capped, and malformed input returns an error. Fuzz targets are in `fuzz/` (see `FUZZING_SETUP.md`).

## Application C bindings

The standalone [`asset-io-c`](asset-io-c/README.md) package provides a checked-in C
header and shared/static libraries for C++, Python and Swift applications, with
memory/read-at parsing, structure inspection, metadata updates and tagged streaming
output. It supports minimal, JPEG/PNG-default and all-formats builds independently
of this Rust crate. It does not define a dynamically loaded handler/plugin ABI.

## License

MIT OR Apache-2.0
