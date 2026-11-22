# Architecture

C-Harness is two Rust programs: a GPUI desktop client on your machine and `ch-runner`, a small
process that sits next to the agent wherever the session runs. No C-Harness server sits between
them, your code, or your keys. The client talks only to services you picked (a model provider,
a git host) and to your own runner.

## Crates

```
ch-app ──> ch-ui
   │
   ├────> ch-core ─────> ch-protocol
   ├────> ch-runtime ──> ch-protocol
   └────> ch-vault

ch-runner ──> ch-protocol        (separate binary, runs next to omp)
```

| Crate | Owns | I/O |
|---|---|---|
| `ch-app` | Window, screens, icons and the brand mark, wiring a session's lines into state | GPUI |
| `ch-ui` | Light and dark color tokens, status and tool group colors, contrast math | none |
| `ch-protocol` | omp RPC framing, `rpc_chunk` reassembly, provider probe, the `ch_*` runner frames and line routing | none |
| `ch-core` | Session state machine, approval parsing that fails closed, risk hints, transcript records, secret redaction | none |
| `ch-runtime` | `SandboxProvider`: a local detached worktree, or a host over the system `ssh` | processes |
| `ch-runner` | Starts omp, forwards its frames, runs verify, builds the diff, stops the agent, lands the branch | processes, git, curl |
| `ch-vault` | Keys and tokens in the OS keychain | keychain |

Three rules hold across the workspace:

- **GPUI stays in `ch-app`.** Logic that can be wrong in interesting ways lives in crates with
  no GPU dependency, so `cargo test` reaches it on any machine.
- **GPUI comes from `gpui-kit` only.** `gpui-kit` pins the `gpui-pre` crate family. Adding
  `gpui` or `gpui-pre` next to it would put two copies of the framework in the build.
- **`ch-runner` depends on nothing heavy.** It runs on machines you don't control, so it is
  `std`, `serde` and `ch-protocol`, plus the `git` and `curl` already on the machine.

## One stream, two kinds of frame

The app and `ch-runner` share one NDJSON stream over the runner's stdin and stdout. A line whose
`type` starts with `ch_` belongs to the runner; every other line is omp's and passes through
untouched. omp payloads are never modeled field by field outside `ch-core`'s session reducer, so
a new omp event costs the transport nothing.

| App to runner | Runner to app |
|---|---|
| `ch_start` session id, omp arguments, environment | `ch_runner_hello` version and working directory |
| `ch_verify` a command and a timeout | `ch_evidence` exit code, duration, commit, output tail |
| `ch_diff` | `ch_diff_result` stat and capped patch |
| `ch_stop` | `ch_stopped` |
| `ch_land` branch, message, author, remote, token, pull request | `ch_landed` branch, commit, pull request URL |
| any omp command | any omp event, `ch_notice`, `ch_omp_exited`, `ch_error` |

Verify, diff, stop and land carry an id that the reply repeats, and `ch_error` repeats it when a
request fails. The app keeps the ids it is still waiting on and drops a reply that names none of
them, or names one of another kind.

## A session, end to end

1. The app reads the model key from the keychain and asks `ch-runtime` for a sandbox: a
   detached, locked git worktree of your repository with a session record on disk, or a
   checkout you prepared on a machine you reach over SSH. A local session holds a lock file
   for as long as its window has it open.
2. `ch-runtime` starts `ch-runner` there. The model key travels in `ch_start` over stdin and
   lands in omp's environment only. It never appears on a command line.
3. `ch-runner` checks the working tree for project omp config (`.omp`, `.claude`, `.codex`,
   `.gemini`) and warns. It asks omp whether a Claude subscription login is stored and which
   providers the user already turned off, then starts `omp --mode rpc` with a config overlay
   that keeps those off and adds `google-antigravity`, plus `anthropic` while a login is
   stored or the answer was not recognised.
4. omp events stream back. `ch-core` folds them into the session state. An approval request
   becomes a card with a risk hint; an answer the app cannot map to an option is sent as a
   cancel, which omp treats as deny. omp retries a failed model call itself; the session shows
   its count and, once omp gives up, whether a person has to act. The app adds no retries.
5. Verify runs in `ch-runner`, not in the agent, so the exit code is measured.
6. Land stops omp and waits for it, then commits through a temporary index (your checkout is
   never touched), pushes from a scratch repository with system and global git config ignored,
   and opens the pull request with `curl` reading the token from stdin. The GitHub token reaches
   the sandbox only in this step.

When the app starts it looks for local sessions an earlier run left behind: records, and
worktrees locked as `c-harness session <session>` or sitting in its worktrees folder. Sessions
another window holds are skipped. Discard deletes only a worktree git lists for that session,
never runs `git worktree prune`, and keeps the `ch/<session>` branch.

[SECURITY.md](SECURITY.md) lists what this design does not protect against.

## Next

| Crate | Will own |
|---|---|
| `ch-runtime` providers | Daytona and E2B next to local and SSH |
| `ch-protocol` | Agent Client Protocol types, normalized into the same session events |
| `ch-plugin` | A WebAssembly plugin host |
