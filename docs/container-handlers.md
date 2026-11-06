# Developing a container handler

A handler adds format-specific parsing and metadata-update rules to `asset-io`.
Applications keep using the same `Asset` and `Structure` APIs. Handlers can live
in this crate or in a separate crate and are registered at runtime, not loaded
through a plugin ABI.

Start with the complete, tested public-API example in
[`tests/external_handler.rs`](../tests/external_handler.rs). The
[`container_api` reference](https://docs.rs/asset-io/latest/asset_io/container_api/)
documents the author-facing traits and builders.

## Dependency and registration

External handlers should not force built-in formats on their users:

```toml
[dependencies]
asset-io = { version = "0.3", default-features = false }
```

Implement `container_api::ContainerHandler`, then register an instance with
`HandlerRegistry::new().with_handler(MyHandler)`, or add it to
`HandlerRegistry::with_builtin()`. Pass the registry to `Asset::open_with`.

- `new()` and `default()` create empty registries.
- `builtin()` includes only handlers whose Cargo features are enabled.
- The last registered handler wins a format string and is tried first during
  content detection.
- `with_handlers` adds a collection; `extend_from` adds another registry's
  handlers.

Each handler declares an `id()` and `FormatSpec`s: media types, preferred
extensions and aliases. The registry uses them for lookup and for
`media_type_for` / `extension_for` queries.

Content detection runs before format lookup. If nothing recognizes the header,
the caller's extension or MIME type selects the handler. A handler without a
detector requires an explicit format.

## The model and its invariants

A `Structure` holds geometry, not file bytes:

- **Segments** form an ordered, gapless, non-overlapping tiling of the entire
  stream. Each has header, payload and trailer ranges.
- **Items** describe logical content such as XMP, JUMBF, EXIF and embedded files.
  An item consists of one or more byte extents, possibly across segments.
- **Kinds and encodings** describe what an item is and how its content is
  serialized. Transfer encoding describes wrapping such as base64 or zlib,
  separately from content encoding.
- **Native identifiers and relations** preserve format-defined types, IDs,
  labels and references, including HEIF thumbnail and auxiliary-image links.
- **Sub-assets** describe contiguous top-level parts, such as images in an MPF
  JPEG. Embedded-file items are distinct from these.

Use segment kinds for their dominant purpose (`Preamble`, `Metadata`,
`MediaData`, `Other`), not to decide which metadata a segment holds. Query
`Structure::items_in(id)` for that.

Expose what the container and its companion specifications define. Metadata
placement is container knowledge; interpreting C2PA manifests, XMP schemas or
other portable content belongs above the handler. Record each JUMBF item's
superbox type so clients can distinguish the C2PA store from other JUMBF.

Check lengths and offsets, bound allocations and nesting, and return errors for
malformed or unsupported input rather than guessing. The builders and parser
limits exported by `container_api` help enforce these invariants.

## Implement parsing

Import author types explicitly; they are not exported at the crate root:

```rust
use asset_io::container_api::{
    BoxFuture, ContainerHandler, FormatSpec, Io, ItemInfo, MetadataSlot,
    PlanBuilder, StructureBuilder, WritePlan,
};
use asset_io::{Result, Structure, Updates};
```

The core hook is `parse_async(self: Arc<Self>, io: Io)`. Return a boxed future
that reads the format and builds a `Structure`:

1. Read only the bytes needed using `io.read_at(offset, &mut buf).await?`,
   `io.reader(at)` for sequential fields, or `io.fetch(..)` for borrowed slices.
2. Create a `StructureBuilder` with the handler, media type and stream length.
3. Add segments with their framing and payload sizes.
4. Describe logical items with `ItemInfo` and their extents or segment IDs.
5. Finish the builder to validate and return the structure.

Handler code is async but does no actual I/O. It uses `Io` and `BoxFuture`;
it does not construct or drive a `Task`. Blocking, Tokio and caller-driven
clients all run the same parser.

Use builder attachments for container-private parsed state. Author hooks access
it through `container_api::attachment::<T>(&structure)`. The related helpers
`handler(&structure)` and `item_info(&item)` expose the retained handler and
descriptive item fields without adding mutable author state to the client API.

## Describe metadata updates

Implement `write_plan_async(&self, structure, io, updates)` to support writing.
It returns a `WritePlan`: the output structure and ordered steps that copy source
bytes or supply new ones. Planning may read the source but must not write it.
A read-only handler can leave the default implementation.

Use `MetadataSlot` to track whether existing XMP or JUMBF is kept, replaced or
dropped, and whether new metadata remains to insert. Then build the output with
`PlanBuilder`:

1. Iterate source segments, checking their items to decide what to retain.
2. `copy_segment(id)` retains a source segment.
3. `put_segment(kind, name, header, payload, trailer)` inserts a new segment.
4. `item(ItemInfo, &[id])` describes an item in the new segments.
5. `carry_items(...)` carries descriptions for items in copied segments.
6. `finish()` calculates sizes, offsets and the output structure.

For partially changed segments, use `shape_like` / `shape` to describe geometry
and `copy`, `put` and `relocate` to supply bytes. Containers with absolute file
offsets can use `Step::Relocate` and an `OffsetMap` to rewrite offset tables.

Blocking `parse`, `write_plan` and `write` are provided. The default writer
executes the plan, coalesces reads, feeds the sink and checks output against its
declared structure and roles. `plan::validate` checks a plan without I/O.

Keep container-specific options outside the common `Updates` fields: use
`Updates::set_extension` / `extension`. The built-in
`containers::bmff::BmffUpdates` is an example.

Preserve format-specific update rules: framing, checksums, alignment, references
and indexes must still be valid when metadata moves. Planned structures must
describe the output accurately before it exists, including JUMBF types and any
supported C2PA box names or data-hash exclusions. Handlers expose those ranges;
they do not compute hashes.

`ContainerHandler::compose` is available for callers that need an item's native
container encoding rather than a full write.

## Test the handler

Run the shared conformance checks on representative samples:

```rust,ignore
asset_io::testing::Conformance::new(handler).check("sample", &sample_bytes)?;
```

They check the promises common to handlers: complete segment coverage,
byte-identical no-op writes, agreement between planning and execution, ordered
sink coverage, metadata set/remove round trips and no panics on damaged input.
Add format-specific tests for constraints the common checks cannot know.
Public enums and structs are non-exhaustive; allow for future variants.

For built-in handlers, keep unit tests in the container module and integration
tests under [`tests/`](../tests), gated on the format's feature:

```sh
cargo test --no-default-features
cargo test --no-default-features --features jpeg
cargo test --all-features
```

[`tests/container_isolation.rs`](../tests/container_isolation.rs) enforces the
module boundaries: the core depends on no container, containers do not depend on
each other, and containers reach the crate only through its public API.

`make ci` checks formatting, documentation, Clippy, the handler-free build,
individual containers and the combined build. Use `make fmt` for formatting;
the repository's rustfmt options require nightly. See the
[fuzzing setup](../FUZZING_SETUP.md) for hostile-input testing.

## Client I/O drivers

These are separate from implementing a handler:

| Namespace | Purpose |
|-----------|---------|
| `asset_io::io` | Shared `Io` and `Reader` implementation |
| `asset_io::task` | `Task`, `Progress`, `ReadRequest` and `block_on` |
| `asset_io::plan` | `WritePlan`, `Step`, validation and relocation |
| `asset_io::execute` | Caller-driven `Execution` and `Action` |
| `asset_io::tokio` | Feature-gated Tokio adapters |

A parse or planning task yields read requests for a client to fill; a write
execution yields read and write actions. Drivers own the actual I/O. See the
[Tokio example](../examples/tokio_client.rs) and the
[API reference](https://docs.rs/asset-io) for client-side integration.
