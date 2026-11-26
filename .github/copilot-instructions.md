# Project Guidelines

## Design priorities

Weigh every change against these, in order. See
[docs/container-handlers.md](../docs/container-handlers.md#design-priorities)
for how they apply to handlers.

1. **Stable and safe against hostile input.** Assets are untrusted. Check every
   length and offset, bound allocations and nesting with the crate's limits, and
   return errors; never panic, guess or read unbounded. Avoid `unsafe`. Add
   fuzz seeds and conformance checks for new parsing paths.
2. **Highly performant: zero-copy wherever possible.** Stream in one pass with
   bounded buffers. Borrow (`Cow`, slices, `read_slice`) instead of copying,
   copy unchanged media through rather than loading it, and read only the bytes
   a decision needs.
3. **Small code, few dependencies.** `thiserror` is the only required
   dependency. Prefer a few lines of code to a new crate; any new dependency
   must be small, pure Rust, optional behind a feature, and justified in the PR.

## Resource and platform constraints

The crate runs on 32-bit and embedded targets, wasm32/WASI, mobile and servers.
Design for the weakest of them: correct means correct under those limits, not
only on a desktop.

- Memory is a function of metadata size, never asset size. Stream, hash
  incrementally, and buffer only metadata, under a cap.
- Treat every length, count and offset from the file as hostile. Check it
  against the remaining input and a cap before allocating, use checked
  arithmetic, keep values `u64` until they are known to fit `usize`, and
  allocate with `try_reserve`, mapping failure to an error.
- Enforce caps centrally, not only at each call site. Prefer configurable
  limits with conservative defaults to fixed platform assumptions.
- Bound work as well as memory: nesting, counts, decode expansion, reference
  cycles.
- Fail closed: return errors; never panic or abort on input. Catch panics at FFI
  boundaries.
- Don't assume a filesystem, threads, clock, network or entropy source without a
  feature gate.
- Hoist per-asset costs that don't depend on the asset when callers process many
  files, and measure aggregate batch time, not per-call time.
- Test the weakest target: 32-bit, wasm32 and memory-capped runs. Fuzz with caps
  on, including adversarial I/O drivers for the sans-io paths.

## Commit messages and PR titles

PRs are squash-merged: the PR title becomes the commit message and the
`CHANGELOG.md` entry, and CI rejects titles that don't match. When suggesting a
PR title or commit message, use:

```text
type(scope)!: description
```

- `type`: one of `build`, `chore`, `ci`, `docs`, `feat`, `fix`, `perf`,
  `refactor`, `revert`, `style`, `test`, `update`. Use `docs`, not `doc`.
- `(scope)`: optional, lowercase, such as `(gif)` or `(c2pa_c_ffi)`.
- `!`: only for breaking changes.

See [CONTRIBUTING.md](../CONTRIBUTING.md#pull-request-titles) for what each type
is for.

## Build and test

- `make ci` runs what CI runs: formatting, docs, Clippy and tests.
- Formatting needs nightly rustfmt: use `make fmt` / `make check-format`, not
  plain `cargo fmt`.
- No container features are enabled by default, so check and lint with
  `--all-features`; otherwise feature-gated handlers aren't compiled.
- `asset-io-c` is a workspace member; target it with `-p asset-io-c`.
