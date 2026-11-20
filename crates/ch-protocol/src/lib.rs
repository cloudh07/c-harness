//! Wire-level plumbing for hosting `omp` as an RPC child process.
//!
//! Everything here is pure: bytes and strings in, values out. No process spawning, no GPUI,
//! no I/O. The logic that can be wrong in interesting ways lives here so that
//! `cargo test -p ch-protocol` reaches it without a GPU toolchain.
//!
//! This crate never models omp's *business* payloads. It understands the transport envelope
//! (`rpc_chunk`) and nothing else, so new omp commands cost the host zero Rust changes.

mod frames;
mod probe;
pub mod runner;

pub use frames::{
    ChunkAssembler, FrameOutcome, FramingError, HOST_ERROR_FRAME_TYPE, LineSplitter,
    MAX_CHUNK_COUNT, MAX_FRAME_BYTES, MAX_REASSEMBLED_FRAME_BYTES, host_error_frame,
};
pub use probe::{
    ANTHROPIC_PROVIDER, ExitReason, GOOGLE_ANTIGRAVITY_PROVIDER, NO_OAUTH_ACCOUNTS,
    PROBE_SENTINEL_PROVIDER, PROVIDER_LIST_PREFIX, SUBSCRIPTION_TOKEN_ENV, classify_exit,
    host_config_overlay, host_disabled_providers, parse_configured_providers,
    parse_disabled_providers, parse_oauth_account_count, parse_version, selects_anthropic_model,
    selects_antigravity_model, validate_approval_mode, validate_model, validate_thinking_level,
};
