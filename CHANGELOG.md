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
- The C-Harness mark: in the sidebar, on the empty session screen, and as the Windows app
  icon embedded in `c-harness.exe`. `assets/brand` holds the source files and the rules for
  using them.
- `ch-app`: a theme switch in the sidebar with Match system, Light and Dark. The choice is
  saved with the other settings.
- `ch-ui`: a full light palette next to the dark one, with tool group colors and tint chips.
  Tests check text, status and tool contrast in both themes.
- `ch-app`: Lucide icons, and provider logos from lobe-icons in the model chip and Settings.
  Each tool call in the thread shows the icon and color of its group: read, edit, shell, web,
  MCP or agent.
- `ch-app`: the session screen is a thread with the prompt box at the bottom and a review
  panel on the right. The panel holds the changed files, the checks, the land step and the
  session details. When the panel is closed and the agent finishes, the thread asks you to
  review.
- `ch-app`: approval cards carry a shield in the color of their risk.
- `ch-app`: an Approvals choice in Settings, "Ask before edits and commands" (omp's `write`,
  the default) or "Ask before every tool" (`always-ask`). It is sent to omp as
  `--approval-mode` on every start. Full auto is not offered.
- `ch-runtime`: a record of each local session in `c-harness/sessions/<session>.json` under the
  local data folder, with the repository, worktree path, branch, base commit and start time. It
  is written before the worktree is created and deleted after the worktree is removed, so a
  crash still leaves a way back to the work. A session id that still has a record is not
  reused.
- `ch-app`: the empty session screen lists local sessions an earlier run left behind, with
  what is left of each: a live worktree, a worktree whose folder is gone, a record only, a
  record whose repository moved, a worktree with no record, or a damaged record. Keep hides one
  until the next start. Discard, pressed twice, deletes its worktree with any uncommitted
  changes, its checkpoint refs and its record, and keeps its `ch/<session>` branch. Only a
  folder git lists as that session's worktree is deleted; any other is named and left alone.
- `ch-runtime`: an open local session holds `c-harness/sessions/<session>.lock`, so a second
  window cannot start or discard it. The lock is released after the worktree is removed.
- `ch-app`: a usage ledger. Each model turn adds a line to `c-harness/usage/<session>.jsonl`
  with its provider, model, input, output and cache tokens, omp's cost estimate, and whether
  the counts are complete, partial or unavailable. omp's zero means "not reported", so it is
  written as `null`, never as a free turn. The session header sums the turns, and marks a sum
  that misses turns with "≥". If the file cannot be written, the app says so once and stops
  writing it.
- `ch-app`: the session header shows how full omp's context window is, as a share of the
  window, read with `get_session_stats` after each turn and after each compaction. Right after
  a compaction omp has no count yet, which is shown as "Context unknown", never as zero.
- `ch-core`: omp's automatic compaction shows as a notice that says whether it finished, was
  stopped, was skipped or failed, and whether omp will retry. A newer compaction notice replaces
  the older one. The transcript keeps the compaction outcome but not omp's summary of the
  conversation, and keeps neither the context questions nor their answers, which name omp's
  session file.
- `ch-app`: Resume on a leftover local session whose worktree is whole. It starts the runner
  again in that worktree and omp on the saved conversation: `--resume=<file>` with the file the
  session record names, or `--continue` when it names none or the file is gone, which may open
  a new conversation. The earlier turns are read back with `get_messages_page` before the
  first prompt is accepted, and the last 200 are shown with a count of the rest. Only prompt
  and reply text comes back, at most 32,000 characters each: a longer prompt keeps its start
  and a longer reply its end. Usage turns number on from the session's ledger. A failed resume
  shows its reason on the row. Resume is offered only while Settings runs sessions on this
  computer.
- `ch-runtime`: the session record keeps the path of the file omp keeps the conversation in,
  once omp names it in a `get_session_stats` answer. Only an absolute `.jsonl` path with no
  `..` or control characters is kept or passed back, and on Windows only one on a drive
  letter, never a network share. Records without it still load.

### Changed

- `ch-runtime`: local worktrees are created locked, with the reason
  `c-harness session <session>`, at the commit their record names. `git worktree prune` no
  longer drops a session whose folder went missing; removing the session still does. Local
  sessions need git 2.33 or newer.
- `ch-runtime`: a session id is claimed by creating its record with create-new, so two starts
  of one id cannot both win.
- `ch-app`: New session moved to the sidebar. It waits until the agent is stopped, and a land
  in flight keeps the session open unless the runner is gone.
- The session screen calls the coding agent "the agent". Settings, install help and error
  messages still name omp, because that is the program you install and configure.
- `ch-core`: once omp has ended, a line the runner could not pass to it is a warning, not a
  failure, so it no longer replaces why omp ended or fails a session that was stopped.
- `ch-app`: the context window is also read when omp says it is ready, so a session learns
  where omp keeps its conversation before the first turn. An answer without a count, before
  any count, leaves the meter blank. The transcript keeps neither history page requests nor
  their answers.

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
- omp no longer starts in its own default approval mode, full auto: the app always sends
  `--approval-mode`, and a repository's `.omp` config cannot change it. `ch-runner` refuses
  `--yolo`, `--auto-approve` and an approval mode omp does not know, which omp would silently
  replace with full auto.
- The host overlay turns omp's memory off (`memory.backend: "off"`), so omp keeps nothing of
  its own between sessions.
- Verify evidence names the work it ran on: `ch-runner` writes the working tree as a git tree
  (on a private index) when the command starts and when it ends, and sends both as
  `tree_before` and `tree_after`. Land is refused, with the reason, when the work changed while
  that check ran or after it, so a file edited after a passing check cannot land on its
  evidence. `ch-app` refuses Verify while the agent is working. The pull request marks a check
  that ran on earlier work as "(on earlier work)".
