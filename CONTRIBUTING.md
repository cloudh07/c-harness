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
- On a live omp with `ask.timeout` set in your own config, confirm that the host overlay's
  `ask: timeout: 0` wins. The runner passes it as the last `--config`, so a question from the
  agent should wait for you instead of picking an answer.
- Land a pull request on GitHub with a fine-grained token.
- Build and run the desktop app on macOS or Linux.

## Before each release

[PRIVACY.md](PRIVACY.md) lists everything that leaves the user's machine. Part of that list comes
from watching omp, so check omp again before a release, with the omp version the release ships
against:

```sh
CH_OMP=/path/to/omp cargo test -p ch-runner --test omp_headers -- --ignored --nocapture --test-threads=1
```

The test starts omp with a fake key and a temporary home, sends one prompt to a listener on
127.0.0.1 and refuses every other connection. It compares the request line, header names and the
values of a few harmless headers (user-agent, content-type and the like) with
`testdata/omp/headers-*.txt`. It never records the key or the ids that link requests, and it
prints the hosts omp tried to reach on the way.

If the test fails, read the change. A new header or host that says who the user is, which machine
or repository they are on, or links their requests belongs in `PRIVACY.md` and
`crates/ch-core/src/outbound.rs`. Then accept the new requests with `CH_BLESS=1` and set
`OMP_VERSION_SEEN` to the omp version you checked. Last checked: omp 18.13.1.

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
