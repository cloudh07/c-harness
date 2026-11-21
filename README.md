<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/brand/mark-on-dark.svg">
    <img src="assets/brand/mark-on-light.svg" alt="The C-Harness mark: three arcs closing on a core" width="96">
  </picture>
</p>

# C-Harness

A native desktop client for coding agents that run in sandboxes you own. Written in Rust on
GPUI, licensed Apache-2.0.

The client talks to agents over standard protocols (omp RPC today, the Agent Client Protocol
next) instead of scraping a terminal. There is no C-Harness server and no C-Harness account.
Your keys, your cloud and your bill stay yours.

## Status

Pre-alpha. Every piece of the path from prompt to pull request is written and tested on its
own, but nobody has run the whole path end to end yet. As of 2026-11-20:

- The tests of every crate pass on Windows. The integration tests drive `ch-runner` with a fake
  omp that answers the way omp 18.13.1 does.
- On Windows, `ch-runner` was run against omp 18.13.1. It started the agent, reported its exit
  and error output, returned the diff, and stopped it without leaving omp processes behind.
  That run had no model key, so the agent never answered a prompt.
- The crates other than the desktop app type-check and pass clippy for Linux and macOS. They
  have not been run on either.

Not checked yet: a full session with a model key, landing a pull request on GitHub, the desktop
app on macOS and Linux, and the release builds. Until those pass, build it, read it, and try it
on a throwaway repository. Read [SECURITY.md](SECURITY.md) first.

A session goes like this:

1. You pick a repository and write a prompt.
2. `ch-runner` starts the omp agent in a separate git worktree (local) or in a checkout you
   prepared on an SSH host.
3. The agent's text and tool calls stream into the window. Commands omp wants approved stop at
   a card that shows the command and a risk hint. Nothing runs until you choose.
4. When the agent is done, you run your own verify command. `ch-runner` runs it and records
   the real exit code and the exact work it ran on.
5. You read the diff, then land it: the agent is stopped, the work is committed to a new
   branch, pushed, and a pull request is opened. If the work changed after the last check,
   land is refused until you verify again.

Not done yet: cloud sandbox providers, the Agent Client Protocol, cost tracking, plugins, and
signed releases. [ROADMAP](#roadmap) has the order.

## Try it

There is no release yet, so build from source (the platform prerequisites are under
[Building](#building)). You need three things on the machine where sessions run, which for a
local session is your own computer:

1. **omp**, the agent. Install it by following the
   [oh-my-pi README](https://github.com/can1357/oh-my-pi), then check `omp --version`.
2. **git** and **curl** on `PATH`. Local sessions need git 2.33 or newer, which can record why
   a worktree is locked.
3. **`ch-runner`**. Build it together with the app:

   ```sh
   cargo build --release -p ch-app -p ch-runner
   ```

   Both binaries land in `target/release/`. The app looks for `ch-runner` in its own folder
   first, so start it from there: `target/release/c-harness`. For an SSH machine, install the
   runner on it with
   `cargo install --git https://github.com/cloudh07/c-harness ch-runner --locked`.

`ch-runner` looks for `omp` on its own `PATH`. If the session reports that omp could not be
started:

- **Local:** start the app from a terminal, so it inherits your shell's `PATH`. A desktop
  launcher may not read your shell profile.
- **SSH:** run `ssh <host> omp --version`. A non-interactive SSH login often skips the file
  that puts omp on `PATH`. Add omp's folder to `PATH` in a file that such logins read, such as
  `~/.zshenv`, or the top of `~/.bashrc` before its interactive-only guard.

Then open **Settings**:

- **Where.** *This computer*: the git repository to work in. Each session gets its own
  detached worktree, so your checkout is never touched. *SSH*: a host alias from your
  `~/.ssh/config`, the path of a checkout you prepared on that host, and where `ch-runner` is
  installed there. C-Harness uses your system `ssh` and never asks for a password, so
  key-based login must already work.
- **Model.** omp's provider id (`openai`, `openrouter`, `google`, `anthropic` and so on), the
  environment variable omp reads the key from, the key itself, and optionally a model id. The
  key is stored in your OS keychain. Give C-Harness its own key with a spending limit: the
  agent can read it.
- **Verify.** The command that proves the change works, such as `cargo test`. `ch-runner` runs
  it in the session's checkout.
- **Land.** Commit author, the GitHub repository as `owner/name`, the base branch, and a
  [fine-grained personal access token](https://github.com/settings/personal-access-tokens/new)
  for that one repository with **Contents: read and write** and **Pull requests: read and
  write**, and a short expiry. Leave **Workflows** off: then a branch that changes
  `.github/workflows` is refused by GitHub, so the agent cannot edit your CI through C-Harness.
  The token is read from the keychain when you click Land and sent to `ch-runner` only then.

Claude API keys work only where omp has no Claude subscription login stored. Anthropic does not
allow third-party apps to use subscription logins, and omp prefers a stored login over a key,
so `ch-runner` turns the `anthropic` provider off for the session while such a login exists,
or when it cannot tell. `google-antigravity` is always off for the same reason.

### If C-Harness closes during a session

When the window closes, `ch-runner` sees its input end and stops the agent. A local session's
worktree, record and usage file stay. The next start lists the session under "Left from earlier
sessions" on the empty session screen:

- **Resume** starts the runner again in that worktree and omp on the saved conversation: the
  file the session record names, or, when it names none or the file is gone, the worktree's
  most recent conversation (`--continue`), which may be a new, empty one. The earlier prompts
  and replies show again, up to the last 200. Tool calls, approvals that were waiting, verify
  results and the diff do not come back, so verify again before you land. After a compaction
  omp may return only what it kept, so older turns can be missing or show without their
  prompt. Resume is offered for a whole worktree only, and only while Settings runs sessions
  on this computer. It uses the approval mode and model Settings has now.
- **Keep** hides the session until the next start.
- **Discard**, pressed twice, deletes the worktree with its uncommitted changes and keeps the
  `ch/<session>` branch.

### Where things are kept

| What | Where |
|---|---|
| Settings | `c-harness/settings.json` under your OS config folder |
| Model key and GitHub token | The OS keychain. The model key is read when a session starts, the token when you land. |
| Local session worktrees | `c-harness/worktrees/<session>` under your OS local data folder. Each is locked with the reason `c-harness session <session>`, so `git worktree prune` keeps it even if its folder is gone. Closing a landed session removes its worktree. Any other session keeps it, so you can still recover the work. The empty session screen lists these under "Left from earlier sessions", where Resume picks one up again and Discard deletes one with its uncommitted changes and keeps its `ch/<session>` branch. By hand, run `git worktree remove --force --force <path>` from your repository, then delete its session record. |
| Local session records | `c-harness/sessions/<session>.json` under the same data folder: the repository, worktree path, branch, base commit and start time of each local session that still has a worktree, and, once omp names it, the path of the file omp keeps the conversation in (never its contents). It is written before the worktree is created and deleted after it is removed. While a window has the session open it holds `<session>.lock` beside the record, so no other window starts or discards it. |
| Exported transcripts | `c-harness/transcripts` under the same data folder, with secrets removed by pattern |
| Token usage | `c-harness/usage/<session>.jsonl` under the same data folder: one line per model turn with the provider, model, token counts and omp's cost estimate, never the prompt or the reply. A count omp did not report is `null`. Discard and closing a session keep this file. The cost is omp's estimate from its price table, not your provider's bill. |

The config folder is `%APPDATA%` on Windows, `~/Library/Application Support` on macOS and
`~/.config` on Linux. The local data folder is `%LOCALAPPDATA%`, `~/Library/Application
Support` and `~/.local/share`.

macOS may ask for your login password when C-Harness reads the keychain. Builds are not
signed, so it can ask again after you rebuild or update.

## Commitments

These eight rules decide what C-Harness will and will not build. Most of them describe
software that does not exist yet. They are written down now so every later change can be
held against them.

1. **Bring your own everything.** You choose the model, the API key, the coding plan where the
   vendor allows it, and the cloud. C-Harness sells no tokens and never sits on your bill.
2. **Keys stay with you.** Credentials live in the OS keychain. A key goes only into your own
   sandbox, one session at a time, and is wiped when the sandbox ends.
3. **No telemetry by default.** Nothing is collected unless you turn it on, and the list of
   events is published.
4. **Cost is always visible.** Each session shows tokens, estimated cost and sandbox time. You
   can cap spending per session and per day.
5. **Leaving is easy.** Tasks, transcripts and settings export to open formats. Your work lives
   on your own git branches.
6. **Safe by default.** Every session starts omp with an approval mode from Settings: by
   default edits and commands wait for an approval card. The mode is passed on omp's command
   line, which overrides omp's own default (full auto) and a repository's omp config. Approval
   is not a sandbox: a command you approve runs with your full access. Full auto is not offered
   yet; when it is, it must be turned on per project and come with a kill switch.
7. **The core stays free.** The client and the runner are open source. A paid service, if one
   ever exists, adds options and never takes away what you already have.
8. **Vendor terms in the open.** The docs will keep a table of which providers allow
   subscription use in third-party tools and which require an API key, with sources and the
   date each entry was checked.

## Building

The toolchain is pinned in `rust-toolchain.toml`; `rustup` picks it up on first build.

```sh
cargo run -p ch-app
```

Platform prerequisites:

- **Windows:** Visual Studio Build Tools with the Windows SDK.
- **macOS:** Xcode Command Line Tools.
- **Linux (Debian/Ubuntu):**

  ```sh
  sudo apt-get install gcc g++ clang libfontconfig-dev libwayland-dev \
    libxkbcommon-x11-dev libx11-xcb-dev libssl-dev libzstd-dev libasound2-dev \
    libvulkan1 vulkan-validationlayers
  ```

The pure crates build and test without any of that:

```sh
cargo test -p ch-protocol -p ch-core -p ch-runner -p ch-runtime -p ch-vault -p ch-ui
```

The CI workflow runs format, clippy, tests and a release build of `ch-app` and `ch-runner` on
Ubuntu, macOS and Windows. `cargo deny` checks licenses, bans and sources and fails the build
on a violation. Its advisory check only reports, so a newly published advisory does not block
unrelated work.

## Crates

| Crate | What it does |
|---|---|
| `ch-app` | The desktop window. Builds the `c-harness` binary. |
| `ch-ui` | Color tokens and status colors. Plain data, no GPUI. |
| `ch-protocol` | omp RPC framing, the provider probe, and the frames the app and `ch-runner` exchange. No I/O. |
| `ch-core` | Session state, approval parsing, risk hints, transcripts and secret redaction. No I/O. |
| `ch-runtime` | Where a session runs: a local git worktree or a machine over SSH. |
| `ch-runner` | The process next to the agent: starts omp, runs verify, builds the diff, lands the branch. |
| `ch-vault` | Keys and tokens in the OS keychain. |

[ARCHITECTURE.md](ARCHITECTURE.md) shows how they fit together.

## Roadmap

1. **Closed alpha (in progress).** One task from prompt to pull request, locally or over SSH,
   checked end to end on Windows, macOS and Linux.
2. **Public beta.** Several sessions side by side, cost per session with caps, cloud sandbox
   providers, signed releases.
3. **Agent Client Protocol.** Agents other than omp, behind the same approval cards.
4. **Plugins.** A WebAssembly host for providers and tools.

Feedback decides the order inside each step. Open an issue and tell us what you need.

## License

Apache-2.0. See [LICENSE](LICENSE).

`ch-protocol` and parts of `ch-core` are derived from code in ZUS and carry its MIT notice.
See [NOTICE](NOTICE).
