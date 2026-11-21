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

**Approvals come from omp.** C-Harness shows omp's approval requests as cards and sends your
answer back. It does not enforce a policy of its own inside the sandbox. The risk level on a card
is a hint worked out from the request text, not a guarantee.

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

**Git still runs some repository config.** `ch-runner` turns off hooks and `fsmonitor`, and
builds the diff without external diff or textconv drivers. Filter drivers named in
`.gitattributes` and defined in the repository's own config still run each time the work is
staged, for a diff and for land. In a local session the agent can edit that config, so a command
it chose can run after it was stopped. The GitHub token is not in that command's environment.

**omp shares the runner's output stream.** Runner frames and omp's events travel on one pipe.
`ch-runner` drops any line from omp whose `type` starts with `ch_`, and the app drops a runner
frame that comes out of omp's chunked output. Each verify, diff, stop and land carries a request
id, and the app drops a reply that names no open request of that kind. The ids are sequential,
so this catches stray, late and replayed replies; it is not authentication. Frames that answer no
request (the runner's hello, a notice, omp exiting, an error without an id) are taken as they
come. Over SSH the remote command runs through your login shell, so anything that shell prints
on startup lands on the same stream.

**Verification is measured, not reported.** `ch-runner` runs the verify command itself and
records the real exit code. The agent may still have edited the tests or scripts that command
runs, so read the diff.

**Releases are not signed yet.** macOS and Windows will warn when you open them. Check the
download against `SHA256SUMS` from the same release.

## Logs

Session transcripts stay on your machine. Secrets are removed by pattern before a line is
written, and patterns miss things. Read an exported log before you attach it to an issue.
