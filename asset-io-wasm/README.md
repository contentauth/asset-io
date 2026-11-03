# Wasm / JavaScript sans-I/O parsing proof

This standalone Cargo package tests asset-io's goal of letting the host language
own its native async scheduling. It defaults to JPEG and PNG; other container
features and `all-formats` forward to the core crate.

**Rust runs computation; JavaScript performs I/O.** The Wasm module uses
`wasm32-unknown-unknown`, not WASI. It contains no filesystem access, JS read
callback, Tokio runtime, or Rust-to-JS async bridge. Each `resume()` returns before
JavaScript awaits its Promise. Independent parsers can interleave on the JS event
loop; each parser has only one outstanding read.

## Build and run (Node.js)

Requirements: Rust 1.88+, Node.js 18.16+ and wasm-pack 0.14.0.
From the repository root:

```sh
rustup target add wasm32-unknown-unknown
# Only if wasm-pack is not already installed:
cargo install wasm-pack --version 0.14.0 --locked

wasm-pack build asset-io-wasm --target nodejs --dev --no-opt -- \
  --locked --target-dir "$PWD/asset-io-wasm/target"
cargo build --locked --manifest-path asset-io-wasm/Cargo.toml \
  --example inspect --target-dir asset-io-wasm/target
node --test asset-io-wasm/js/async.test.mjs
node asset-io-wasm/js/inspect.mjs tests/fixtures/Designer.jpeg
node asset-io-wasm/js/inspect.mjs tests/fixtures/GreenCat.png png
```

wasm-pack generates JS glue, TypeScript declarations and the Wasm binary under
ignored `pkg/`. It installs a matching wasm-bindgen CLI if missing. The standalone
Cargo lockfile pins the binding dependencies. No npm packages are needed.

The test suite drives actual Wasm with `node:fs/promises`, and compares its
inspection summary with a native blocking Rust executable. It explicitly awaits
a macrotask between read requests, verifying event-loop progress while Rust is
suspended, not merely immediate Promise resolution. Tests also cover interleaved
tasks, the Blob adapter, EOF, read failures, invalid response bounds/types,
copied response ownership, deferred format errors, and cancellation.

## Async interface

```js
import bindings from "./asset-io-wasm/pkg/asset_io_wasm.js";
import { parseAsync } from "./asset-io-wasm/js/driver.mjs";

const summary = await parseAsync(
  bindings.ParseTask,
  sourceSize, // bigint, in 0..=2^64-1
  async (offset, windowLen) => {
    // offset: bigint; windowLen: number.
    // Use the host's native async I/O and return a Uint8Array.
    return await source.readAt(offset, windowLen);
  },
  { format: "jpg", signal: abortController.signal },
);
```

`format` is an optional extension or MIME type. Omit it to detect the format.
The source size and bytes must remain unchanged throughout parsing.
The driver validates sizes before calling Wasm; use it rather than relying on
low-level wasm-bindgen integer conversion to reject out-of-range JS values.

`readAt` must gather short positive reads itself until it has at least the
requested minimum (reading the whole window is simplest) or reaches EOF.
Responses larger than the window or not `Uint8Array` are errors. Too few bytes
cause unexpected EOF on resume. The Node adapter retries partial reads and
rejects positions outside JavaScript's safe-integer range; request offsets
themselves remain lossless bigint.

The low-level API is synchronous:

- `new ParseTask(size, format)` owns its registry and format.
- `resume()` returns a `ReadRequest`, or `undefined` when finished; failures throw
  and remain terminal. Repeated calls while waiting return the same descriptor.
- `supply(bytes)` copies the response to the pending request. It does not poll.
  An oversized response throws without consuming the request.
- `fail(message)` terminates a pending read with a host error.
- `summaryJson()` returns a source-free summary only after successful completion.
- `free()` releases the parser. Each returned read descriptor also requires
  `free()`, after copying its fields. The provided driver handles both lifetimes.

`parseAsync` rejects with the original host error and always frees its parser.
With `AbortSignal`, cancellation is checked before construction, between resumes,
and after a host read settles. It does **not** interrupt the host's outstanding
read or abandon its Promise; it waits for completion before freeing the task.
A never-settling read therefore cannot be cancelled by this driver alone.
If needed, make `readAt` use the same signal with a cancellation-aware host API.

## Browser Blob/File adapter

Generate ES module bindings separately:

```sh
wasm-pack build asset-io-wasm --target web --out-dir pkg-web --dev --no-opt -- \
  --locked --target-dir "$PWD/asset-io-wasm/target"
```

Then, in a browser module served with the generated Wasm:

```js
import init, { ParseTask } from "./asset-io-wasm/pkg-web/asset_io_wasm.js";
import { parseBlob } from "./asset-io-wasm/js/driver.mjs";

await init();
const summary = await parseBlob(ParseTask, fileInput.files[0]);
```

`parseBlob` uses `Blob.slice(...).arrayBuffer()` Promises. The Blob adapter is
tested using Node's Blob implementation; browser execution is not yet part of
this proof's automated checks. Ignore generated `pkg-web/` output as well.

## Scope and summary schema

This is **parsing only**. It does not yet expose metadata reads, writes, retained
source assets, Web Workers, shared-memory Wasm threads, or production packaging.
It proves native JavaScript async I/O without replacing Rust's parser.

The proof-specific summary is **not the complete C ABI JSON schema**. Schema 1
includes container ID, media type, source size, trailing range, segment kinds,
ranges/roles/subdivision flags, item kinds/encodings/sizes/extents, and subasset
geometry. All byte offsets, lengths and source/item sizes are decimal strings,
preserving u64 precision after `JSON.parse`; segment indices are numbers.
It intentionally omits native names, transfer details and item relations.
The native oracle and Wasm share the same summary helper, so comparison tests
exercise parsing and geometry parity, not two independently implemented serializers.
