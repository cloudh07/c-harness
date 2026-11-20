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

## Where to start

You don't need omp, a model key or a GitHub token for most work.

- `crates/ch-runner/tests/serve.rs` reruns its own test binary as a fake omp that replays
  `testdata/omp/approval-session.jsonl`. A whole session runs offline: start, an approval,
  verify, diff, and a land that pushes to a local bare repository with a fake pull request
  opener.
- The driver tests in `crates/ch-app/src/driver.rs` play the runner's side in memory, so the app's
  session logic is tested without a window. They do need the GPU toolchain to compile.
- One `ch-vault` test writes to the real OS credential store, so it is ignored by default. Run it
  with `cargo test -p ch-vault -- --ignored` on a machine where that is fine.

Nobody has checked these yet, and each one helps:

- Run a real session with your own model key on a throwaway repository and report what broke.
  The omp fixture was written by hand from omp's docs. A captured session, scrubbed of keys,
  tokens and personal paths, would be a better one.
- Land a pull request on GitHub with a fine-grained token.
- Build and run the desktop app on macOS or Linux.

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
