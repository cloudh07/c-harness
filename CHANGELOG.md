# Changelog

Notable changes to C-Harness. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- `ch-app`: a desktop window with the inbox groups and an empty state. It follows the system
  light or dark appearance.
- `ch-ui`: light and dark color tokens, status colors, and a WCAG contrast check for text.
- `ch-protocol`: the omp RPC wire layer, ported from ZUS. NDJSON line splitting that skips
  oversized lines without losing the frames around them, `rpc_chunk` reassembly with size
  caps, host error frames, and the pre-flight probe. The probe computes which providers a
  session must run without (`google-antigravity` always, `anthropic` while an OAuth login is
  stored or unknown). It reads omp 18.13.1's answers: the empty-store message on stderr with
  exit 1, and `config get --json` as an object with a `value` list.
- CI on Ubuntu, macOS and Windows: format, clippy, tests, a release build of `ch-app` and
  `ch-runner`, and `cargo deny` (advisories report only).
- `ch-protocol`: the `ch_*` frames the app and `ch-runner` exchange on one NDJSON stream,
  and line routing that passes omp's own frames through untouched. Secrets in these frames are
  hidden from `Debug` output.
- `ch-core`: the session state machine fed by omp events, approval parsing that fails closed,
  risk hints for approval cards, transcript records, and secret redaction ported from ZUS.
- `ch-runner`: starts omp in RPC mode with the model key in its environment only, runs verify
  commands itself, builds the diff, stops the agent, and lands the work as a branch and a
  pull request. It warns when the working tree carries project omp config.
- `ch-runtime`: local sessions in a detached git worktree, and SSH sessions through the system
  `ssh`.
- `ch-vault`: keys and tokens in the OS keychain.
- A release workflow that builds both binaries for Windows, macOS and Linux, plus a static
  Linux `ch-runner`, and drafts a release with `SHA256SUMS`. Releases are not signed yet.
- `SECURITY.md` with the threat model and known limits, `CONTRIBUTING.md`, and issue and pull
  request templates.
- `ch-app`: a bare runner name in Settings means the `ch-runner` beside the app when there is
  one, so a build or an unpacked release runs without editing `PATH` on macOS and Linux.
- `ch-runner`: when omp is not installed, the error says to put `omp` on the `PATH` that
  `ch-runner` sees.

### Security

- `ch-runner` pins git to the session's own repository and working tree, and refuses a working
  directory nested inside another repository.
- The GitHub token is only ever sent to `https://github.com/`: a land that carries a token is
  refused for any other remote. UNC paths are refused as remotes.
- Land is refused while a verify or diff is still running.
- Approval dialogs the app cannot answer (`confirm`, `input`, `editor`, `ask`) are cancelled,
  which omp treats as a refusal.
- A runner frame that arrives inside omp's `rpc_chunk` sequence is dropped, so omp cannot forge
  verify evidence, a diff, a stop or a landed branch.
- `ch-app` drops a runner reply that names no open request of its kind: evidence must answer a
  verify the app sent, a diff a diff, a stop a stop and a landed branch a land, each only once.
  Dropped replies are logged and kept out of the session.
