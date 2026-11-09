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
