# omp fixtures

`approval-session.jsonl` is one prompt that asks to run `bash`, gets approved, and settles.

It was written by hand from the omp 18.13.1 RPC docs and from strings in the 18.13.1 binary. It
was not captured from a live session, so field names outside the documented frames (the approval
`select` title and options in particular) may drift. Replace it with a real capture when one
exists, and keep the replacement free of keys, tokens and personal paths.

The reply to the approval line is
`{"type":"extension_ui_response","id":"ui_1","value":"Approve"}`. A cancel or any label not in
`options` means deny.

`headers-anthropic.txt` and `headers-openai.txt` are what omp 18.13.1 sends to a model provider:
request lines, header names, the values of a few harmless headers and the names of the body's
top-level fields. `crates/ch-runner/tests/omp_headers.rs` writes and checks them, and
CONTRIBUTING.md says when to run it. They never hold a key or an id.
