# Contributing

We welcome contributions to this project!

Before you start, we ask that you understand the following guidelines.

## Code of conduct

This project adheres to the Adobe [code of conduct](CODE_OF_CONDUCT.md). By participating,
you are expected to uphold this code. Please report unacceptable behavior to
[Grp-opensourceoffice@adobe.com](mailto:Grp-opensourceoffice@adobe.com).

## Have a question?

Start by filing an issue. The existing committers on this project work to reach
consensus around project direction and issue solutions within issue threads
(when appropriate).

<!-- Not applicable to all repos, e.g. example apps -->

## Contributor license agreement

All third-party contributions to this project must be accompanied by a signed contributor
license agreement. This gives Adobe permission to redistribute your contributions
as part of the project. [Sign our CLA](https://opensource.adobe.com/cla.html). You
only need to submit an Adobe CLA one time, so if you have submitted one previously,
you are good to go!

NOTE: the Adobe CLA is tied to each _individual_ contributor. There is no mechanism to sign the CLA on behalf of an entire organization.

### Code reviews

All submissions should come in the form of pull requests and need to be reviewed
by project committers. Read [GitHub's pull request documentation](https://help.github.com/articles/about-pull-requests/)
for more information on sending pull requests.

Code submissions will need to pass all automated tests in place at the time of submission.
These include such things as Rust code format, Clippy/lint checks, and unit test coverage.

We encourage you to raise an issue in GitHub before starting work on a major addition to the crate.
This will give us an opportunity to discuss API design and avoid duplicate efforts.

### Pull request titles

Pull requests are squash-merged: the PR title becomes the commit message and the
PR's `CHANGELOG.md` entry, and its description becomes the commit body. Titles
must follow [conventional commits](https://www.conventionalcommits.org/en/v1.0.0/#specification);
the [PR title workflow](.github/workflows/pr-title.yml) checks this:

```text
type(scope)!: description
```

`type` is required and must be one of the following:

| Type | Use for | Changelog section |
|------|---------|-------------------|
| `feat` | New functionality | Added (bumps the minor version) |
| `fix` | Bug fixes | Fixed |
| `perf` | Performance improvements | Performance |
| `docs` | Documentation only | Documentation |
| `refactor` | Code changes that neither fix bugs nor add features | Other |
| `test` | Tests only | Other |
| `build` | Build system, Cargo manifests and dependencies | Other |
| `ci` | CI and release workflows | Other |
| `chore` | Maintenance not covered above | Other |
| `style` | Formatting only | Other |
| `update` | Dependency updates | Other |
| `revert` | Reverting an earlier change | Other |

`(scope)` is optional, lowercase, and names the affected module or component,
such as `(gif)`, `(tiff)` or `(c2pa_c_ffi)`. Add `!` before the colon for a
breaking change. The description is required. For example:

```text
feat(gif): Support C2PA box hash
```

Commits within a pull request don't need to follow this format, since they are
squashed into one commit titled with the PR title.

## Development

Formatting uses nightly-only rustfmt options, so install a nightly toolchain
(`rustup toolchain add nightly`). Before opening a pull request, run the checks CI
runs:

```sh
make ci    # nightly rustfmt check, docs, Clippy and tests
make fmt   # apply formatting
```

No container formats are enabled by default. Configure your editor to check with
all features (for rust-analyzer, `"rust-analyzer.cargo.features": "all"`), or
errors in feature-gated containers appear only in CI.

To add a container format, follow the
[container handler guide](docs/container-handlers.md#adding-a-built-in-container).
See [FUZZING_SETUP.md](FUZZING_SETUP.md) for fuzzing.

## Releases

Maintainers release in two steps. Pull requests are squash-merged, so each PR's
title becomes its `CHANGELOG.md` entry; keep titles conventional.

1. **Merge the release PR.** On each push to `main`, the
   [Release PR workflow](.github/workflows/release-plz.yml) runs
   [release-plz](https://release-plz.dev) to open or update a pull request
   that bumps the workspace version and adds a `CHANGELOG.md` entry generated
   from commit messages. New features bump the minor version. Review the entry;
   new commits on `main` regenerate the PR, replacing manual edits. Configuration
   is in [release-plz.toml](release-plz.toml).
2. **Run the [Release workflow](.github/workflows/release.yml)** from `main`.
   It publishes `asset-io` to crates.io with trusted publishing, tags `vX.Y.Z`
   and creates the GitHub release from the version's `CHANGELOG.md` section. It
   stops before publishing if that version is already on crates.io, its tag
   exists, or the changelog has no entry for it.

Run the Release workflow soon after merging the release PR: commits merged in
between are released without a changelog entry. `asset-io-c` is not published.

## From contributor to committer

We love contributions from our community! If you'd like to go a step beyond contributor
and become a committer with full write access and a say in the project, you must
be invited to the project. The existing committers employ an internal nomination
process that must reach lazy consensus (silence is approval) before invitations
are issued. If you feel you are qualified and want to get more deeply involved,
feel free to reach out to existing committers to have a conversation about that.

## Security issues

Do not create a public GitHub issue for any suspected security vulnerabilities. 
For guidance on reporting security issues, please see [SECURITY.md in the c2pa-rs repository](https://github.com/contentauth/c2pa-rs/blob/main/SECURITY.md).

