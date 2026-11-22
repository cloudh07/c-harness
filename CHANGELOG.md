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
  record whose repository moved, a worktree with no record, or a damaged record. A record that
  cannot be read at all, such as one another program holds open, is left off the list and
  cannot be discarded. Each row says when its session started. Keep hides one until the next
  start. Discard, pressed twice, deletes its worktree with any uncommitted changes, its
  checkpoint refs and its record, and keeps its `ch/<session>` branch. A double click counts as
  one press, so the warning shows before anything is deleted. Only a folder git lists as that
  session's worktree is deleted; any other is named and left alone.
- `ch-runtime`: an open local session holds `c-harness/sessions/<session>.lock`, so a second
  window cannot start or discard it. The lock is released after the worktree is removed.
- `ch-app`: a usage ledger. Each model turn adds a line to `c-harness/usage/<session>.jsonl`
  with its provider, model, input, output and cache tokens, omp's cost estimate, and whether
  the counts are complete, partial or unavailable. omp's zero means "not reported", so it is
  written as `null`, never as a free turn. The session header sums the turns, and marks a sum
  that misses turns with "≥". If the file cannot be written, the app says so once and tries
  the same lines again after the next turn, when you start a new session, and when you close
  the window or quit. It says so again if the try at a new session fails too. If the try at
  quit fails, those lines are lost, and only a terminal that started the app shows why (a
  Windows release build has none). A write after
  one cut off mid-line, in this run or an earlier one, starts on a new line, and a turn a retry
  wrote twice is read back once.
- `ch-app`: the session header shows how full omp's context window is, as a share of the
  window, read with `get_session_stats` after each turn and after each compaction. Right after
  a compaction omp has no count yet, which is shown as "Context unknown", never as zero. When
  omp ends in the middle of a compaction, the header stops saying it is compacting. From 80%
  the share is drawn in the warning color. A share under one percent reads "<1%".
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
  computer. The header takes its title from the earlier conversation's first prompt, or reads
  "Resumed session" when there is none.
- `ch-runtime`: the session record keeps the path of the file omp keeps the conversation in,
  once omp names it in a `get_session_stats` answer. Only an absolute `.jsonl` path with no
  `..` or control characters is kept or passed back, and on Windows only one on a drive
  letter, never a network share. Records without it still load.
- `ch-runner`, `ch-app`: turn checkpoints for local sessions. Each time the agent finishes a run,
  the runner saves the working tree, untracked files included and ignored ones left out, as a
  commit under `refs/c-harness/<session>/turn-<n>`, up to 1000 per session. A failed save is
  not tried again, but the next run is still saved, with those changes in it. The review panel
  lists the last eight, each with how long ago it was saved. Each shows what changed since the
  one before it in a card in the thread, and Restore puts the working tree back as it was then,
  or as the session started. A
  restore first saves the tree it replaces as a checkpoint of its own, so it can be undone. It
  is refused when it would write over or delete a file no checkpoint holds, such as an ignored
  one. Files come back with the repository's line-ending settings. A prompt waits for a restore,
  and up to 15 seconds for the save of the last run. Only a worktree C-Harness made and locked
  for the session keeps checkpoints, so SSH sessions have none. A resumed session lists the
  checkpoints it had.
- `ch-protocol`: `ch_checkpoint`, `ch_restore`, and `from` and `to` on `ch_diff`, answered by
  `ch_checkpoints`, `ch_checkpointed`, `ch_restored` and a `ch_diff_result` that names its range.
- `ch-core`: a failed prompt now says whether trying again can help, read from omp's own error.
  `retryable` makes it transient, a cost cap or HTTP 401, 402 or 403 means a person has to act,
  and anything else is permanent. Failures from the runner or from omp exiting are permanent.
- `ch-app`: while omp retries a model call, the session shows "Retrying 2/3" with omp's error and
  drops the text of the failed attempt. After a failure the banner says "Gave up: <error>" when
  a later try may work, or what to check when a person has to act. The app never retries on its
  own; omp's `retry.maxRetries` setting is the budget.
- `ch-app`: when the model has sent no text, thinking or tool call 90 seconds after a prompt,
  the thread says so and suggests waiting or stopping the agent. omp reports no event for the
  provider connection, so the 90 seconds cover the connection and the first token together.
  After omp's retry delay the wait starts again. It stops while an approval card waits for you
  and starts again from zero once you answer.
- `ch-runner`: each git command of the repository check before omp starts has 30 seconds to
  finish. A check that fails or hangs ends the start with its own `repo` stage, which the
  banner calls "the repository check", so it is never taken for a provider that does not
  answer. A timeout suggests `git fsck`.
- `ch-app`: the session header has a second line that says where the session runs: "local", or
  the SSH host with the port Settings name, then the folder the agent writes in. That folder is
  the session's worktree once the runner has made it, or the checkout on the host. A long path
  drops its middle, so the drive and the session's own folder stay in view.
- `ch-app`: while a session waits for your answer, the OS shows the notification "C-Harness
  needs you". It never names the session or what it asks, because notifications show on the
  lock screen. It goes away once the session stops waiting, closes or the app quits, and
  clicking it brings the window forward. A local session's record notes that it is waiting,
  so after a restart that session is listed first under "Left from earlier sessions", counts
  under Needs you, and says its request expired. Resume starts it again; there is nothing to approve, because omp
  does not ask the old question again. On Windows the app registers the name its
  notifications show under `HKCU\Software\Classes\AppUserModelId\c-harness`. macOS shows no
  notification for a build that is not an app bundle. An SSH session keeps no record, so a
  restart forgets that it was waiting.
- `ch-app`: when the secret scan refuses a land, the Land section lists each finding by file,
  line and kind ("provider token" or "private key"), never the text it matched, and the session
  moves to Needs you. Land anyway lands that exact change. Edit the work and the scan runs
  again. A change too large to scan says so, and Land anyway lands it unscanned.
- `ch-protocol`: `ch_land` takes `acknowledged_tree`, and `ch_error` can carry `secret_scan`:
  the tree that was scanned, up to 50 findings, how many more there were, and whether the
  change was too large to read.
- `ch-app`: an optional cost cap per session in Settings, in dollars and off by default. Once
  the turns omp priced reach it, the running prompt is aborted, the session says why, and no
  new prompt goes out until you raise or clear the cap. Turns omp could not price are not
  counted, and the header says how many there are.
- `ch-app`: the session header shows the cost as its own item. A session with no priced turn
  reads "Cost unknown", never $0, and turns omp could not price are named beside the sum
  ("$0.12 + 2 unpriced turns"). With a cap set it reads "$1.50 of $5.00 cap", in the warning
  color once the cap is reached.
- `ch-app`: the usage file also keeps omp's own session totals after each turn and a line for
  each passing Verify the app asked for. A check result the app did not ask for is not kept.
- `ch-core`: the usage file read back as totals by token kind and by model, the cost per
  passing Verify, and a check of the summed turns against omp's totals that names each turn
  where they differ. `cargo run -p ch-core --example usage_report -- <usage folder>` prints
  the report, or says it was not run when no file holds a turn.

### Changed

- `ch-app`: the header shows the share of input read from the cache (`99% cached`) instead
  of the cached token count. When some turns went uncounted, the share is of the counted ones
  and marked `~`.
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
- `ch-core`: a warning the session already shows is not added again, so a checkpoint save that
  fails the same way after every run shows one warning.
- `ch-app`: a run that ends with no reply text and no tool calls saves no checkpoint, and the
  thread says so under the run. Any edit you made in the worktree meanwhile goes into the next
  checkpoint.
- `ch-app`: the header names an SSH session after the folder of its remote checkout instead of
  the host, which moved to the new line. A session keeps the host and folder it started with
  when you change Settings.

### Security

- `ch-runner` pins git to the session's own repository and working tree, and refuses a working
  directory nested inside another repository.
- The GitHub token is only ever sent to `https://github.com/`: a land that carries a token is
  refused for any other remote. UNC paths are refused as remotes.
- Land is refused while a verify, diff or restore is still running. A restore is refused while
  any of them runs, or while a checkpoint is still being saved.
- Checkpoints are kept only in a worktree locked for the session, so a restore never rewrites a
  checkout C-Harness did not make. The agent can rewrite these refs, so they are never used as
  evidence. They are deleted with the worktree, and land pushes only the session branch. A
  restore or a diff reads a checkpoint ref only when it points at a commit with C-Harness's
  checkpoint subject, never a tag or another commit, and a restore that changes more paths than
  it can check (16 MiB of names) is refused.
- `ch-runtime` runs git with hooks and `fsmonitor` turned off, as `ch-runner` already did, so
  creating or removing a worktree, or deleting its checkpoints, runs no program the repository
  names. It also drops `GIT_ALTERNATE_OBJECT_DIRECTORIES` from git's environment, like the
  other variables that point git at another repository.
- Approval dialogs the app cannot answer (`confirm`, `input`, `editor`, `ask`) are cancelled,
  which omp treats as a refusal.
- `ch-app`: the session itself refuses a land while the agent works or waits on you, not only
  the Land button. Tests pin down that no button grants anything on its own: an answer goes
  out only for an approval omp still holds open, once, and with a choice omp offered.
- A runner frame that arrives inside omp's `rpc_chunk` sequence is dropped, so omp cannot forge
  verify evidence, a diff, a stop or a landed branch.
- `ch-app` drops a runner reply that names no open request of its kind: evidence must answer a
  verify the app sent, a diff a diff, a stop a stop and a landed branch a land, each only once.
  Dropped replies are logged and kept out of the session.
- omp no longer starts in its own default approval mode, full auto: the app always sends
  `--approval-mode`, and a repository's `.omp` config cannot change it. `ch-runner` refuses
  `--yolo`, `--auto-approve` and an approval mode omp does not know, which omp would silently
  replace with full auto. It also refuses a start without `--approval-mode` or with it twice,
  and accepts `always-ask-cached` only from an omp whose `--version` reads 18.12.3 or newer.
  Settings does not offer that mode.
- The host overlay turns omp's memory off (`memory.backend: "off"`), so omp keeps nothing of
  its own between sessions.
- Verify evidence names the work it ran on: `ch-runner` writes the working tree as a git tree
  (on a private index) when the command starts and when it ends, and sends both as
  `tree_before` and `tree_after`. Land is refused, with the reason, when the work changed while
  that check ran or after it, so a file edited after a passing check cannot land on its
  evidence. `ch-app` refuses Verify while the agent is working. The pull request marks a check
  that ran on earlier work as "(on earlier work)".
- `ch-runner` scans a land for secrets before it commits or pushes anything. It reads every
  line the change adds over the commit the session started from, and refuses the land when a
  line holds a PEM private key header or a token with a known provider prefix and a digit. The
  refusal names the file, line and kind, never the value. Files git treats as binary are not
  scanned. A diff over 32 MiB is refused as too large to scan, never scanned in part. A land
  that sends `acknowledged_tree` skips the scan only when that tree is the one the runner
  computes again, so any edit after the person looked is scanned. The scan is a floor: a key
  with no known prefix, or a PGP key block, passes.
- `ch-app` lands only through a `ch-runner` whose hello announces protocol 3 or later, the first
  that scans a land for secrets. `RUNNER_PROTOCOL_VERSION` is now 3. With an older runner, or
  one that never said hello, the Land button is off and the Land section says why and to
  reinstall `ch-runner` with `--force`.
