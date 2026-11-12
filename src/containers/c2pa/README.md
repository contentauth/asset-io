Enabled by the `c2pa` feature (also in `all-formats`).

## Representation

- Accepts `c2pa`, `application/c2pa`, and
  `application/x-c2pa-manifest-store`.
- A nonempty sidecar is one `Metadata` segment and one `C2pa` item, both
  covering the entire file. There are no container headers or trailers.
- An empty sidecar has no segments or items.
- Parsing with an explicit format accepts opaque bytes, including placeholders,
  without interpreting the C2PA claims or validating signatures.
- Content detection recognizes the usual `jumb` / `jumd` prefix with the C2PA
  manifest-store type UUID. Empty, opaque, or extended-length JUMBF files should
  be parsed explicitly as `c2pa`.
- Parsing reads nothing: the whole file is the store. Reading an item's bytes is
  subject to the core's read limits.

## Writing

`Updates::new()` copies the sidecar byte-for-byte. `set_c2pa(bytes)` replaces
the entire file, with no added wrapping; `as_update()` does not append another
store. `remove_c2pa()` and `remove_all()` produce an empty sidecar, as does
setting an empty store. Type-scoped removal leaves a different or unknown type
unchanged.

Sidecars cannot contain a separate XMP packet: setting XMP returns
`UnsupportedFeature`, while removing absent XMP is a no-op. `compose` supports
only `ItemKind::C2pa` and returns the bytes unchanged. In-place patches must
preserve the exact item length; use a write to resize a sidecar.

The raw read/write model and MIME aliases follow
[c2pa-rs's sidecar handler](https://github.com/contentauth/c2pa-rs/blob/main/sdk/src/asset_handlers/c2pa_io.rs).
Removal intentionally differs: c2pa-rs treats it as a no-op, while asset-io emits
an empty sidecar to honor its metadata-removal contract.

## Tests

```sh
cargo test --no-default-features --features c2pa
```
