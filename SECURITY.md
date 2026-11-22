# Security

## Reporting a vulnerability

Report it privately through GitHub: open the repository's **Security** tab and choose
**Report a vulnerability**. Please don't open a public issue for it. You should hear back within
a week.

Only the latest release and `main` get fixes while C-Harness is in alpha.

## What a session trusts

C-Harness is alpha software. Read this before you point it at a repository you care about.

A session works like this:

1. The desktop app starts `ch-runner`, either on your machine in a separate git worktree or on a
   machine you reach over SSH.
2. `ch-runner` starts the omp agent in RPC mode in that working tree.
3. Your model key goes from the OS keychain to `ch-runner` over the session's stdin, and from
   there into omp's environment only. It is never on a command line.
4. When you land the work, `ch-runner` stops the agent first. Only then does the app send your
   GitHub token. The branch is pushed from a fresh scratch repository with system and global git
   config ignored, and the pull request is opened by `curl` reading its config, token included,
   from stdin.

## Known limits

**Local sessions are not sandboxed.** The agent runs as your user and can read whatever you can
read, including other tools' keys. For a repository you don't fully trust, use an SSH machine you
can throw away.

**The agent can read the model key.** It sits in omp's environment, so any command the agent
runs can print it. Give C-Harness its own key with a spending limit, and revoke the key if a
session behaved oddly.

**Approvals come from omp.** C-Harness starts omp with `--approval-mode` set to the mode chosen
in Settings (`write` unless you pick `always-ask`), shows omp's approval requests as cards and
sends your answer back. `ch-runner` refuses a start that does not name `--approval-mode`, or
names it twice. It accepts `always-ask-cached` only from omp 18.12.3 or newer, read from
`omp --version`, because an older omp does not know the value and replaces it with full auto.
Settings does not offer it. C-Harness does not enforce a policy of its own inside the sandbox.
An approved shell command runs with your full access, so approval limits what runs without
asking, not what a command can do. The risk level on a card is a hint worked out from the
request text, not a guarantee.

**A resumed session uses today's Settings.** Resume starts omp again with the approval mode
and model Settings has now, which may differ from the ones the session started with. omp opens
the conversation file the session record names. The record sits in your local data folder, so
anything that can write there as you can point a resume at another omp conversation.

**A repository can change how omp behaves.** omp reads project config and extensions from
`.omp`, `.claude`, `.codex` and `.gemini` in the working tree. They can loosen approval prompts
and run code inside omp. `ch-runner` warns when it finds them.

**The token is exposed while the branch is pushed.** The `git push` process gets the token
through its environment. On Linux, another process running as the same user can read that
environment. `ch-runner` kills the agent's process group before landing, but a process the agent
detached from its group (with `setsid`, for example) survives. Use a fine-grained token limited to
one repository, with only **Contents: read and write** and **Pull requests: read and write**, and
a short expiry.

**The agent sees ch-runner's environment.** omp and the verify command inherit the environment
`ch-runner` was started with, minus the Claude subscription token variables. Anything exported
in the shell or service that starts `ch-runner` is visible to the agent.

**Stopping the agent is best effort on Windows.** `ch-runner` ends omp with `taskkill /T`, which
follows parent links. A process whose parent has already exited is not found and keeps running.

**Git still runs some repository config.** `ch-runner` and the app turn off hooks and
`fsmonitor` in every git command they run, and `ch-runner` builds the diff without external diff
or textconv drivers. Filter drivers named in `.gitattributes` and defined in the repository's own
config still run each time the work is staged, for a diff, a checkpoint and land, and when a
restore writes files. In a local session the agent can edit that config, so a command it chose
can run after it was stopped. The GitHub token is not in that command's environment.

**omp shares the runner's output stream.** Runner frames and omp's events travel on one pipe.
`ch-runner` drops any line from omp whose `type` starts with `ch_`, and the app drops a runner
frame that comes out of omp's chunked output. Each verify, diff, checkpoint, restore, stop and
land carries a request id, and the app drops a reply that names no open request of that kind.
The ids are sequential, so this catches stray, late and replayed replies; it is not
authentication. Frames that answer no request (the runner's hello, a notice, omp exiting, an
error without an id) are taken as they come. Over SSH the remote command runs through your login
shell, so anything that shell prints on startup lands on the same stream.

**Verification is measured, not reported.** `ch-runner` runs the verify command itself and
records the real exit code. The agent may still have edited the tests or scripts that command
runs, so read the diff.

**Evidence is bound to the work it checked.** `ch-runner` records the working tree as a git
tree before and after each check, and refuses to land when the work differs from the tree the
last check ran on, or when files changed while that check ran. Two limits: a change made and
undone while a check runs is not seen, and a land with no check at all is still allowed (the
pull request then says no checks were run).

**The secret scan is a floor.** Before land commits, `ch-runner` reads every line the change
adds and refuses the land when one holds a PEM private key header, or a token that starts with a
prefix the log redaction knows (`sk-`, `ghp_`, `AKIA`, `xoxb-` and the rest) and holds a digit.
Patterns miss things. A key with no known prefix, a PGP key block, a password assigned in a
config file, a secret split across lines or encoded, and anything in a file git treats as binary
all pass. Only added lines are read, so a secret already in the commit the session started from
is not reported. A diff over 32 MiB is refused unread. Land anyway lands the refused tree
unscanned; any later edit makes a new tree, and that one is scanned. The scan runs in
`ch-runner`, so the app lands only through a runner that announced protocol 3 or later in its
hello. A runner installed before the scan existed would commit and push unscanned, so the app
turns Land off for it and says to reinstall. A runner that never said hello is treated the same
way.
The protocol number is the runner's own claim: it guards against an old install, not a forged
runner. Read the diff before you land.

**Checkpoints are a convenience, not evidence or a backup.** They are refs in your repository,
under `refs/c-harness/<session>/`, and in a local session the agent can rewrite them. Land never
reads them. Ignored files are in no checkpoint: a restore refuses to write over or delete one,
but nothing stops the agent from deleting it. Only a worktree locked for the session keeps
checkpoints, so a restore never rewrites a checkout C-Harness did not make. They are deleted
with the worktree. A restore does not stop the agent: the app offers it only while the agent is
idle, but a process the agent left running can still write files during one, and an ignored
file written between the check and the rewrite can be lost.

**Releases are not signed yet.** macOS and Windows will warn when you open them. Check the
download against `SHA256SUMS` from the same release.

## Logs

Session transcripts stay on your machine. Secrets are removed by pattern before a line is
written, and patterns miss things. Read an exported log before you attach it to an issue.
