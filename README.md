# C-Harness

A native desktop client for coding agents that run in sandboxes you own. Written in Rust on
GPUI, licensed Apache-2.0.

The client talks to agents over standard protocols (omp RPC today, the Agent Client Protocol
next) instead of scraping a terminal. There is no C-Harness server and no C-Harness account.
Your keys, your cloud and your bill stay yours.

## Status

Phase 0. Nothing connects to a sandbox yet.

What exists:

- A window with the inbox groups (Needs you, Running, Needs review, Landed) and an empty state.
  It follows the system light or dark appearance.
- `ch-protocol`, the wire layer for hosting `omp` as an RPC child: NDJSON line splitting,
  `rpc_chunk` reassembly with size caps, and the pre-flight probe. The probe builds the list
  of providers a session must run without: `google-antigravity` always, and `anthropic`
  whenever an Anthropic OAuth login is stored or the check cannot tell. Nothing spawns `omp`
  yet, so the list is computed and tested but not yet applied to a live session.
- Color tokens for both themes, with a test that holds body and muted text to WCAG AA on
  every surface.

The first milestone is one agent session in a sandbox over SSH, with an approval card and a
diff, ending in a pull request, on Windows, macOS and Linux.

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
6. **Safe by default.** Destructive commands, pushes and outbound network calls wait for an
   approval card. Full auto must be turned on per project and comes with a kill switch.
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
cargo test -p ch-protocol -p ch-ui
```

The CI workflow runs format, clippy, tests and a release build of `ch-app` on Ubuntu, macOS
and Windows. `cargo deny` checks licenses, bans and sources and fails the build on a
violation. Its advisory check only reports, so a newly published advisory does not block
unrelated work.

## Crates

| Crate | What it does |
|---|---|
| `ch-app` | The desktop window. Builds the `c-harness` binary. |
| `ch-ui` | Color tokens and status colors. Plain data, no GPUI. |
| `ch-protocol` | omp RPC framing and the provider probe. No I/O, no process spawning. |

[ARCHITECTURE.md](ARCHITECTURE.md) lists the crates that come next and how a session will flow.

## License

Apache-2.0. See [LICENSE](LICENSE).

`ch-protocol` is derived from code in ZUS and carries its MIT notice. See [NOTICE](NOTICE).
