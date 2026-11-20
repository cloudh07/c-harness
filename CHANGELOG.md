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
  stored or unknown). It is not wired to a running `omp` yet.
- CI on Ubuntu, macOS and Windows: format, clippy, tests, a release build of `ch-app`, and
  `cargo deny` (advisories report only).
