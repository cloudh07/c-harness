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

ch-runner ──> ch-core, ch-protocol   (separate binary, runs next to omp)
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
  `std`, `serde`, `ch-protocol` and `ch-core` (pure logic, no I/O), plus the `git` and `curl`
  already on the machine. Anything added to `ch-core` ships in the runner too.

## One stream, two kinds of frame

The app and `ch-runner` share one NDJSON stream over the runner's stdin and stdout. A line whose
`type` starts with `ch_` belongs to the runner; every other line is omp's and passes through
untouched. omp payloads are never modeled field by field outside `ch-core`'s session reducer, so
a new omp event costs the transport nothing.

| App to runner | Runner to app |
|---|---|
| `ch_start` session id, omp arguments, environment | `ch_runner_hello` version, protocol and working directory |
| `ch_verify` a command and a timeout | `ch_evidence` exit code, duration, commit, output tail |
| `ch_diff` | `ch_diff_result` stat and capped patch |
| `ch_stop` | `ch_stopped` |
| `ch_land` branch, message, author, remote, token, pull request, acknowledged tree | `ch_landed` branch, commit, pull request URL |
| any omp command | any omp event, `ch_notice`, `ch_omp_exited`, `ch_error` |

Verify, diff, stop and land carry an id that the reply repeats, and `ch_error` repeats it when a
request fails. A land the secret scan refused also carries `secret_scan`: the tree it scanned
and where the findings are, never what they matched. The app keeps the ids it is still waiting
on and drops a reply that names none of them, or names one of another kind.

## Before the first session

The first run asks `ch-runtime` to start `ch-runner preflight --workdir <dir>` on the machine
sessions use. The runner prints one `PreflightReport` and exits: omp's version, why git cannot
work in the folder if it cannot, and the test commands the project files point to. Before a
local repository is picked, the app leaves out `--workdir` and the runner checks omp alone. omp
runs in a private temp directory, so the repository's own omp config is not loaded before
anyone has looked at it. The app keeps only test commands from `VERIFY_COMMANDS`, the fixed
list the runner can suggest, so a runner on another machine cannot fill the verify field with
its own command. A runner older than protocol 4 answers "unknown command", which the app shows as
a runner that needs reinstalling. The keychain checks stay in the app: one writes, reads back
and deletes a throwaway entry, the other only looks at whether a model key is stored.

With `--test-model`, the runner first reads a `ModelTestRequest` (the model key and model)
from stdin and asks omp for one word in print mode, tools off, `--max-cost` set. The answer is
scrubbed of the key before it goes back. The app sends this only when the person presses the
button that states the cost.

## A session, end to end

1. The app reads the model key from the keychain and asks `ch-runtime` for a sandbox: a
   detached, locked git worktree of your repository with a session record on disk, or a
   checkout you prepared on a machine you reach over SSH. A local session holds a lock file
   for as long as its window has it open. The session's header keeps the machine and folder
   it got here, so changing Settings later does not relabel it.
2. `ch-runtime` starts `ch-runner` there. The model key travels in `ch_start` over stdin and
   lands in omp's environment only. It never appears on a command line.
3. `ch-runner` checks that it was started at the top of a git working tree and reads the
   commit the session starts from. Each git command of this check has 30 seconds; a failure
   or a hang ends the start with the `repo` stage, kept apart from `start` so a damaged
   repository is not mistaken for a provider. It checks the working tree for project omp
   config (`.omp`, `.claude`, `.codex`, `.gemini`) and warns. It asks omp whether a Claude
   subscription login is stored and which providers the user already turned off, then starts `omp --mode rpc` with a config overlay
   that keeps those off and adds `google-antigravity`, plus `anthropic` while a login is
   stored or the answer was not recognised.
4. omp events stream back. `ch-core` folds them into the session state. An approval request
   becomes a card with a risk hint; an answer the app cannot map to an option is sent as a
   cancel, which omp treats as deny. omp retries a failed model call itself; the session shows
   its count and, once omp gives up, whether a person has to act. The app adds no retries.
   The app times the model's first text, thinking or tool call against its own clock and
   says so after 90 seconds of silence; omp has no event for the provider connection, so
   that wait covers both. A run that ends with no reply and no tool calls saves no
   checkpoint.
5. Verify runs in `ch-runner`, not in the agent, so the exit code is measured.
6. The app sends a land only through a runner whose hello announced protocol 3 or later, the
   first with the secret scan; an older runner, or one that never said hello, cannot land.
   Land stops omp and waits for it, then writes the working tree through a temporary index
   (your checkout is never touched). Before it commits, it scans the lines that tree adds over
   the session's starting commit for private keys and provider tokens, and refuses the land on a
   hit or on a diff too large to read whole. Land anyway sends the refused tree back as
   `acknowledged_tree`; the runner skips the scan only if the tree it writes again is that one.
   Then it commits, pushes from a scratch repository with system and global git config ignored,
   and opens the pull request with `curl` reading the token from stdin. The GitHub token reaches
   the sandbox only in this step.

When the app starts it looks for local sessions an earlier run left behind: records, and
worktrees locked as `c-harness session <session>` or sitting in its worktrees folder. Sessions
another window holds are skipped. Discard deletes only a worktree git lists for that session,
never runs `git worktree prune`, and keeps the `ch/<session>` branch.

Approvals live only in omp's process: `ch-core` fills its pending list from omp's
`extension_ui_request` frames and empties it when omp ends, and Resume never refills it. So a
request cannot outlive omp, and nothing can answer it after a restart. What survives is a
`needs_you` flag on the session record. The app writes it when the session state crosses into
or out of Needs you, on the same tick that shows or removes the OS notification. A resumed
session starts with the flag as it was saved, and the first tick clears it once omp is running
with nothing to answer. Leftovers with the flag sort first, count under Needs you, and say
their request expired.

[SECURITY.md](SECURITY.md) lists what this design does not protect against.

## Next

| Crate | Will own |
|---|---|
| `ch-runtime` providers | Daytona and E2B next to local and SSH |
| `ch-protocol` | Agent Client Protocol types, normalized into the same session events |
| `ch-plugin` | A WebAssembly plugin host |
