# Developing a container handler

A handler adds format-specific parsing and metadata-update rules to asset-io.
Applications keep using the same `Asset` and `Structure` APIs. Handlers can live
in this crate or in a separate crate; either way they are registered at runtime,
not loaded through a plugin ABI.

Start with [`tests/external_handler.rs`](../tests/external_handler.rs): a
complete handler written against only the public API. The
[`container_api` reference](https://docs.rs/asset-io/latest/asset_io/container_api/)
documents the author-facing traits and builders, and the [user guide](guide.md)
describes the model clients see.

- [Design priorities](#design-priorities)
- [Namespaces](#namespaces)
- [Dependency and registration](#dependency-and-registration)
- [Model invariants](#model-invariants)
- [Parsing](#parsing)
- [Planning writes](#planning-writes)
- [C2PA hooks](#c2pa-hooks)
- [Testing](#testing)
- [Adding a built-in container](#adding-a-built-in-container)

## Design priorities

Every handler, and every change to the core, is weighed against these, in order:

1. **Stable and safe against hostile input.** Assets come from untrusted
   sources, and a handler must never panic, hang or exhaust memory on them.
   - Check every length, offset and count before using it, with checked
     arithmetic.
   - Bound allocations, segment counts and nesting with the limits
     `container_api` exports, such as `MAX_SEGMENTS` and `MAX_SEGMENT_SIZE`.
   - Return an error for malformed or unsupported input rather than guessing.
   - Don't use `unsafe`.
   - Cover new parsing paths with the conformance checks and add fuzz seeds.
2. **Highly performant: zero-copy wherever possible.**
   - Parse by reading only the bytes a decision needs; never load the whole
     asset.
   - Describe writes as plans that copy unchanged segments straight through in
     one streaming pass with bounded buffers.
   - Borrow new data (`Cow`, slices) rather than copying it, as the GIF handler
     does with sub-block payloads.
   - Record what later operations need, such as JUMBF types or C2PA box
     boundaries, while parsing, so they don't reread the asset.
3. **Small code, few dependencies.**
   - The core requires only `thiserror`.
   - Prefer a few lines of format-specific code to a general-purpose crate.
   - A dependency a container needs must be small, pure Rust, and optional
     behind that container's feature, like `quick-xml` for SVG.
   - Justify any new dependency in its pull request.

## Namespaces

Author and advanced driver types are not exported at the crate root:

| Namespace | Contents |
|-----------|----------|
| `asset_io` | Client model: `HandlerRegistry`, `Asset`, `Structure`, `Item`, `Updates`, `Sink` |
| `asset_io::container_api` | `ContainerHandler`, `FormatSpec`, `StructureBuilder`, `ItemInfo`, `PlanBuilder`, `MetadataSlot`, `Io`, `BoxFuture`, `Crc32`, parser limits, `block_on` |
| `asset_io::plan` | `WritePlan`, `Step`, `validate` and offset relocation |
| `asset_io::io` | `Io`, `Reader`, `BoxFuture` |
| `asset_io::task` | `Task`, `Progress`, `ReadRequest`, `block_on` |
| `asset_io::execute` | `Execution`, `Action` for caller-driven writes |
| `asset_io::containers` | Feature-gated built-in handler factories and typed options |
| `asset_io::testing` | `Conformance` checks |

## Dependency and registration

External handlers should not force built-in formats on their users:

```toml
[dependencies]
asset-io = { version = "0.3", default-features = false }
```

Implement `container_api::ContainerHandler` and register an instance with
`HandlerRegistry::new().with_handler(MyHandler)` or
`HandlerRegistry::with_builtin().with_handler(MyHandler)`. Applications pass the
registry to `Asset::open_with`. The last handler registered for a format wins and
is tried first during content detection.

Each handler declares an `id()` and one or more `FormatSpec`s: a media type, a
preferred extension and any aliases. Implement `detect(header)` if the format has
a recognizable signature. Without one, callers must name the format.

## Model invariants

- **Segments** tile the entire stream: ordered, gapless and non-overlapping, each
  with header, payload and trailer ranges.
- **Items** describe logical content (XMP, JUMBF, EXIF, embedded files) through
  one or more extents, possibly across segments.
- Use `SegmentKind` for a segment's dominant purpose. Writers decide what a
  segment holds from `Structure::items_in(id)`, never from its kind.
- Preserve native identifiers and relations. Record each JUMBF item's superbox
  type with `ItemInfo::jumbf().jumbf_type(..)` so clients can find the C2PA store
  without reading it.
- Expose what the format's specifications define. Placing portable content such as
  C2PA manifests is container knowledge; interpreting it is not.
- Treat input as hostile; see [Design priorities](#design-priorities).

## Parsing

Import author types explicitly:

```rust,ignore
use asset_io::container_api::{
    BoxFuture, ContainerHandler, FormatSpec, Io, ItemInfo, MetadataSlot,
    PlanBuilder, StructureBuilder, WritePlan,
};
use asset_io::{Result, Structure, Updates};
```

The core hook is `parse_async(self: Arc<Self>, io: Io)`. Return
`Box::pin(async move { ... })` that builds a `Structure`:

1. Read only what you need: `io.read_at(offset, &mut buf).await?`, `io.reader(at)`
   for sequential fields, or `io.fetch(..)` for a borrowed slice.
2. Create a `StructureBuilder` with the handler, media type and stream length.
3. Push segments with their header, payload and trailer sizes.
4. Add items with `ItemInfo` and their segments or extents.
5. Call `finish()`, which validates the tiling.

Handler code is async but performs no I/O itself; it never constructs or drives a
`Task`. Blocking, Tokio and caller-driven clients all run the same parser, and the
blocking `parse` is provided.

Attach container-private state with `StructureBuilder::attach`. Hooks read it
with `container_api::attachment::<T>(&structure)`; `handler(&structure)` and
`item_info(&item)` give the retained handler and an item's descriptive fields.

## Planning writes

Implement `write_plan_async(&self, structure, io, updates)` to support writing.
It returns a `WritePlan`: the output structure plus ordered steps that copy source
bytes or supply new ones. Planning may read the source but never writes. A
read-only handler keeps the default.

Use a `MetadataSlot` per metadata kind to decide whether each existing XMP or
JUMBF item is kept, replaced or dropped, and what remains to insert. Then
describe the output in order with `PlanBuilder`:

- `copy_segment(id)` keeps a source segment.
- `put_segment(kind, name, header, payload, trailer)` adds a new segment.
- `item(ItemInfo, &[id])` describes an item in new segments.
- `carry_items(|item| ..)` carries items from copied segments.
- `finish()` computes sizes, offsets and the output structure.

For a segment that is partly copied and partly changed, describe it with
`shape_like` or `shape` and supply its bytes with `copy`, `put` and `relocate`.
Formats with absolute file offsets (ISO BMFF chunk tables) use `Step::Relocate`
with an `OffsetMap`.

The default `write` executes the plan, coalescing reads, feeding the sink and
checking output against the declared structure and roles. Keep framing,
checksums, alignment, references and indexes valid when metadata moves. Options
only one container understands go through `Updates::set_extension` and
`Updates::extension`.

`compose(kind, data)` returns the container's native encoding of an item for
callers that insert it themselves.

## C2PA hooks

Each has a default; override only where the format differs:

| Hook | Default | Override when |
|------|---------|---------------|
| `c2pa_hash` | `Box` if every element has a box name, else `Data` | The format uses another binding or none |
| `box_name` | FourCCs and numeric IDs as text | Names use other text (JPEG marker symbols, GIF block names) |
| `c2pa_box_name` | `box_name` of the element's name | The C2PA name is not the element name (RIFF lists chunks by form or list type) |
| `c2pa_box_continues` | `false` | One box spans several segments (GIF application-extension sub-blocks) |
| `c2pa_data_hash_exclusions` | Every segment carrying the store | Padding stays hashed (RIFF), only part is excluded (SVG), or the format binds differently (`UnsupportedFeature`) |

Planned structures must give the same answers as the written output, so clients
can compute hash geometry before writing. When a reference implementation such as
c2pa-rs exists, test box names and ranges against it.

## Testing

Run the shared conformance checks on representative samples:

```rust,ignore
asset_io::testing::Conformance::new(handler).check("sample", &sample_bytes);
```

They verify the promises every handler makes: segments tile the stream, a no-op
write is byte-identical, planning, writing and stepping agree, the sink sees every
byte once and in order, metadata set and remove round-trip, and damaged input
never panics. `plan::validate` checks a plan without I/O. A handler's own tests
then cover only what is particular to its format.

Public enums and structs are `#[non_exhaustive]`; handle unknown variants.

## Adding a built-in container

Built-in containers are modules under `src/containers/` behind a Cargo feature of
the same name. [`tests/container_isolation.rs`](../tests/container_isolation.rs)
enforces that the core depends on no container, containers don't depend on each
other, and containers use only the public API.

Register a new container `NAME` in each of these places:

- [ ] `src/containers/NAME/mod.rs` with `pub const ID`, `pub fn handler()` and
      module docs describing its structure and update policy; unit tests in
      `src/containers/NAME/tests.rs`
- [ ] `Cargo.toml`: a `NAME` feature, added to `all-formats`
- [ ] `src/containers/mod.rs`: the feature-gated `pub mod` and its entry in
      `register_builtin`
- [ ] `src/lib.rs`: the feature list in the crate documentation
- [ ] `asset-io-c/Cargo.toml`: a `NAME` feature, added to `all-formats`
- [ ] `Makefile`: `test-each-format`
- [ ] `tests/container_isolation.rs`: `CONTAINERS`
- [ ] `tests/registry.rs`: format resolution expectations
- [ ] `tests/hash_exclusions.rs`: hash kind and box or data-hash geometry
- [ ] `fuzz/seeds/{fuzz_parse,fuzz_write,fuzz_conform}/`: small seed files
- [ ] [README](../README.md) formats table and the
      [guide's hard-binding table](guide.md#hard-bindings)
- [ ] [CHANGELOG](../CHANGELOG.md)

Then run the checks CI runs, plus the new container on its own:

```sh
make ci
cargo test --no-default-features --features NAME
make test-c-ffi
```

`make ci` checks formatting with nightly rustfmt (`make fmt` applies it), builds
docs, runs Clippy with all and no features, and runs the tests. Configure your
editor to check with all features enabled; otherwise feature-gated containers are
not compiled and their errors only appear in CI.
