# Architecture

C-Harness is two Rust programs: a GPUI desktop client on your machine and a small runner inside
each sandbox. No C-Harness server sits between them, your code, or your keys. The client talks
only to services you picked (a model provider, a sandbox provider, a git host) and to your own
runner.

Only the client exists today.

## Crates now

```
ch-app ──> ch-ui
ch-protocol            (not wired into ch-app yet)
```

| Crate | Owns | Depends on |
|---|---|---|
| `ch-app` | Window, app lifecycle, the inbox screen | `ch-ui`, `gpui-kit` |
| `ch-ui` | Color tokens, status colors, contrast math | nothing |
| `ch-protocol` | omp RPC framing, `rpc_chunk` reassembly, provider probe, exit classification | `serde`, `serde_json`, `base64` |

Two rules hold across the workspace:

- **GPUI stays in `ch-app`.** Logic that can be wrong in interesting ways lives in crates with
  no GPU dependency, so `cargo test -p ch-protocol -p ch-ui` runs anywhere.
- **GPUI comes from `gpui-kit` only.** `gpui-kit` pins the `gpui-pre` crate family. Adding
  `gpui` or `gpui-pre` next to it would put two copies of the framework in the build.

`ch-protocol` understands the transport envelope and nothing else. omp payloads pass through as
JSON, so a new omp command costs the host no Rust changes.

## Crates planned

| Crate | Will own |
|---|---|
| `ch-core` | Session and task model, event bus, a stream buffer that coalesces updates per frame |
| `ch-runtime` | A `SandboxProvider` trait with SSH, Daytona, E2B and local implementations |
| `ch-runner` | The binary inside the sandbox: starts the agent, encrypts its stdio stream, runs git |
| `ch-vault` | Credential storage in the OS keychain |
| `ch-plugin` | A WebAssembly plugin host |

ACP types will join omp RPC in `ch-protocol`, normalized into one event stream.

## Session flow (planned)

1. The client creates a sandbox through your provider account.
2. It installs `ch-runner` and pushes the session's API key, which is wiped when the sandbox
   ends.
3. It opens an SSH or WebSocket channel to the runner.
4. Agent events stream back. Risky actions stop at an approval card.
5. The result is a commit on a branch, then a pull request.
