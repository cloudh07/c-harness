# Contributing

Thanks for helping. Bug reports, fixes, docs and design feedback are all welcome.

## Before you start

Open an issue before a large change so we can agree on the shape first. Small fixes can go
straight to a pull request.

Security problems go through private reporting, not issues. See [SECURITY.md](SECURITY.md).

## Building and testing

[README.md](README.md#building) lists the platform prerequisites. Before you open a pull request:

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Every crate except `ch-app` builds without the GPU toolchain, so most work only needs:

```sh
cargo test -p ch-protocol -p ch-core -p ch-runner -p ch-runtime -p ch-vault -p ch-ui
```

CI runs the same checks on Ubuntu, macOS and Windows. A change is done when all three pass.

## House rules

- GPUI stays in `ch-app`. Logic that can be wrong lives in a crate with no GPU dependency, with
  tests.
- No `unsafe`. The workspace denies it.
- Paths, shells and processes must work on Windows, macOS and Linux. Don't assume `bash`, `/tmp`
  or `~`.
- New dependencies need a reason in the pull request. `cargo deny` checks their licenses.
- Never commit a real key or token, not even an expired one. Tests use obvious fakes.
- Comments explain why, not what.

## Commits and changelog

Commit messages follow [Conventional Commits](https://www.conventionalcommits.org/)
(`feat:`, `fix:`, `docs:`, `refactor:`, `test:`, `chore:`, `ci:`). Changes users will notice go
in [CHANGELOG.md](CHANGELOG.md) under `[Unreleased]`.

## License

C-Harness is licensed under Apache-2.0. By submitting a contribution you agree it is licensed
under the same terms, as section 5 of the license describes.
