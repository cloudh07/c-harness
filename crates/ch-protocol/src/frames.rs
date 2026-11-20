//! NDJSON framing and protocol-v2 `rpc_chunk` reassembly.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::Deserialize;

/// Host mirror of the `maxFrameBytes` omp advertises in its `ready` frame.
///
/// A physical stdout line longer than this is a desynchronised stream, not a big frame: omp
/// caps its own writes here and chunks anything larger.
pub const MAX_FRAME_BYTES: usize = 1_048_576;

/// Host mirror of `maxReassembledFrameBytes`: the ceiling for one `rpc_chunk` sequence.
pub const MAX_REASSEMBLED_FRAME_BYTES: usize = 67_108_864;

/// Upper bound on the `count` field of an `rpc_chunk` header.
///
/// `count` arrives off the wire and sizes an allocation, so it needs a bound of its own.
/// 64 Ki slots covers the 64 MiB reassembly ceiling down to 1 KiB segments.
pub const MAX_CHUNK_COUNT: usize = 65_536;

/// `type` of the synthetic frame the host emits for its own framing failures.
///
/// Namespaced so it can never collide with an omp frame type. Host failures then travel the
/// same frame path as omp output, and the UI dispatches on `type` without a second channel.
pub const HOST_ERROR_FRAME_TYPE: &str = "ch_host_error";

/// Framing failures that force the host to drop bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FramingError {
    /// A stdout line grew past [`MAX_FRAME_BYTES`]. The line is skipped up to its newline.
    LineTooLong,
}

impl std::fmt::Display for FramingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LineTooLong => write!(
                f,
                "stdout desynchronised: a line passed the {MAX_FRAME_BYTES}-byte frame ceiling and was skipped"
            ),
        }
    }
}

impl std::error::Error for FramingError {}

/// Splits a byte stream into NDJSON frames, holding the trailing partial line between reads.
///
/// The carry is bytes rather than text on purpose: the largest frame observed in practice is
/// ~95 KiB and never arrives in a single `read`, so decoding each chunk independently would
/// corrupt any multi-byte character straddling the seam.
#[derive(Debug, Default)]
pub struct LineSplitter {
    carry: Vec<u8>,
    /// Set after an oversized line, until its newline arrives.
    skipping: bool,
}

impl LineSplitter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bytes currently held back waiting for a newline.
    pub fn pending_bytes(&self) -> usize {
        self.carry.len()
    }

    /// Feed one `read` chunk. Returns the frames and errors it produced, in stream order.
    ///
    /// Blank lines are dropped: omp parses "one non-empty JSONL line at a time" and an empty
    /// string is not a frame.
    ///
    /// A line longer than [`MAX_FRAME_BYTES`] yields one [`FramingError::LineTooLong`], and
    /// every byte up to its newline is dropped, whether that newline is in this read or a
    /// later one. Memory stays bounded and the next line parses normally.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<Result<String, FramingError>> {
        let mut out = Vec::new();
        for piece in chunk.split_inclusive(|b| *b == b'\n') {
            let (body, complete) = match piece.split_last() {
                Some((b'\n', body)) => (body, true),
                _ => (piece, false),
            };
            if self.skipping {
                self.skipping = !complete;
                continue;
            }
            if self.carry.len() + body.len() > MAX_FRAME_BYTES {
                out.push(Err(FramingError::LineTooLong));
                self.carry.clear();
                self.skipping = !complete;
                continue;
            }
            self.carry.extend_from_slice(body);
            if complete {
                let line = self.carry.strip_suffix(b"\r").unwrap_or(&self.carry);
                if !line.is_empty() {
                    out.push(Ok(String::from_utf8_lossy(line).into_owned()));
                }
                self.carry.clear();
            }
        }
        out
    }
}

/// Transport envelope of a protocol-v2 chunk.
///
/// This is the only omp frame this crate parses, and it carries no business fields: `data` is
/// an opaque base64 slice of the original frame's UTF-8 bytes.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RpcChunk {
    #[serde(rename = "type")]
    frame_type: String,
    chunk_id: String,
    index: usize,
    count: usize,
    byte_length: usize,
    data: String,
}

#[derive(Debug)]
struct Pending {
    chunk_id: String,
    count: usize,
    byte_length: usize,
    segments: Vec<Option<Vec<u8>>>,
    received: usize,
    bytes: usize,
}

/// One reassembler result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameOutcome {
    /// A complete logical frame, ready to forward verbatim to the UI.
    Frame(String),
    /// A host-detected transport violation. The offending sequence has already been dropped.
    ProtocolError(String),
}

/// Reassembles `rpc_chunk` sequences and passes every other frame through untouched.
///
/// omp specifies chunks as an *uninterrupted* sequence, so at most one sequence is ever in
/// flight. That invariant doubles as the anti-hang guard: a different `chunkId`, or any
/// non-chunk frame, proves the in-flight sequence will never complete, so it is dropped
/// immediately instead of being pinned in memory waiting for a segment that is not coming.
#[derive(Debug, Default)]
pub struct ChunkAssembler {
    pending: Option<Pending>,
}

impl ChunkAssembler {
    pub fn new() -> Self {
        Self::default()
    }

    /// True while a chunk sequence is partially received.
    pub fn has_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Feed one raw stdout frame.
    ///
    /// Returns a list rather than an `Option` because one input frame can legitimately
    /// produce two outputs: a plain frame that interrupts a pending sequence yields both the
    /// discard error and the frame itself, and swallowing either would hide a real fault.
    pub fn feed(&mut self, line: String) -> Vec<FrameOutcome> {
        let mut out = Vec::new();

        // Prefilter before touching the JSON parser: a ~95 KiB `get_state` frame is the common
        // case and must not pay for a full parse just to be forwarded verbatim.
        let chunk = if line.contains("\"rpc_chunk\"") {
            serde_json::from_str::<RpcChunk>(&line)
                .ok()
                .filter(|c| c.frame_type == "rpc_chunk")
        } else {
            None
        };

        let Some(chunk) = chunk else {
            if let Some(stale) = self.pending.take() {
                out.push(FrameOutcome::ProtocolError(format!(
                    "rpc_chunk sequence {} interrupted after {}/{} segments; discarded",
                    stale.chunk_id, stale.received, stale.count
                )));
            }
            out.push(FrameOutcome::Frame(line));
            return out;
        };

        if let Some(error) = self.accept(&chunk, &mut out) {
            self.pending = None;
            out.push(FrameOutcome::ProtocolError(error));
        }
        out
    }

    /// Absorb one chunk. Returns `Some(message)` when the sequence must be abandoned; the
    /// caller then clears `pending`, so a malformed sender can never wedge the stream.
    fn accept(&mut self, chunk: &RpcChunk, out: &mut Vec<FrameOutcome>) -> Option<String> {
        if chunk.count == 0 || chunk.count > MAX_CHUNK_COUNT {
            return Some(format!(
                "rpc_chunk {} declares count {} (limit {MAX_CHUNK_COUNT})",
                chunk.chunk_id, chunk.count
            ));
        }
        if chunk.index >= chunk.count {
            return Some(format!(
                "rpc_chunk {} index {} out of range for count {}",
                chunk.chunk_id, chunk.index, chunk.count
            ));
        }
        // Zero is rejected too: the splitter drops blank lines, and an empty reassembled frame
        // would smuggle one past it.
        if chunk.byte_length == 0 || chunk.byte_length > MAX_REASSEMBLED_FRAME_BYTES {
            return Some(format!(
                "rpc_chunk {} declares byteLength {} (allowed 1 to {MAX_REASSEMBLED_FRAME_BYTES})",
                chunk.chunk_id, chunk.byte_length
            ));
        }

        let segment = match STANDARD.decode(chunk.data.as_bytes()) {
            Ok(bytes) => bytes,
            Err(e) => {
                return Some(format!(
                    "rpc_chunk {} segment {} is not valid base64: {e}",
                    chunk.chunk_id, chunk.index
                ));
            }
        };

        if let Some(stale) = self.pending.take_if(|p| p.chunk_id != chunk.chunk_id) {
            out.push(FrameOutcome::ProtocolError(format!(
                "rpc_chunk sequence {} interleaved by {} after {}/{} segments; discarded",
                stale.chunk_id, chunk.chunk_id, stale.received, stale.count
            )));
        }
        if let Some(pending) = &self.pending
            && (pending.count != chunk.count || pending.byte_length != chunk.byte_length)
        {
            return Some(format!(
                "rpc_chunk {} header changed mid-sequence (count {}->{}, byteLength {}->{})",
                chunk.chunk_id, pending.count, chunk.count, pending.byte_length, chunk.byte_length
            ));
        }

        let pending = self.pending.get_or_insert_with(|| Pending {
            chunk_id: chunk.chunk_id.clone(),
            count: chunk.count,
            byte_length: chunk.byte_length,
            segments: vec![None; chunk.count],
            received: 0,
            bytes: 0,
        });

        if pending.segments[chunk.index].is_some() {
            return Some(format!(
                "rpc_chunk {} segment {} received twice",
                chunk.chunk_id, chunk.index
            ));
        }

        pending.bytes += segment.len();
        if pending.bytes > pending.byte_length {
            return Some(format!(
                "rpc_chunk {} overflows its declared {} bytes",
                chunk.chunk_id, pending.byte_length
            ));
        }
        pending.segments[chunk.index] = Some(segment);
        pending.received += 1;

        // `None` here means "still incomplete", not a failure: nothing to report yet.
        let done = self.pending.take_if(|p| p.received == p.count)?;
        if done.bytes != done.byte_length {
            return Some(format!(
                "rpc_chunk {} reassembled to {} bytes, expected {}",
                done.chunk_id, done.bytes, done.byte_length
            ));
        }

        // Concatenate in *index* order. Segments live in slots instead of being appended on
        // arrival because ordering is a property of the sequence, not of delivery.
        let mut buf = Vec::with_capacity(done.byte_length);
        for segment in done.segments.into_iter().flatten() {
            buf.extend_from_slice(&segment);
        }

        // Strict UTF-8, never lossy: a reassembled frame that is not valid UTF-8 means the
        // segments were mis-ordered or corrupted, and substituting U+FFFD would hand the
        // UI unparseable JSON with no explanation of why.
        match String::from_utf8(buf) {
            Ok(text) => {
                out.push(FrameOutcome::Frame(text));
                None
            }
            Err(e) => Some(format!(
                "rpc_chunk {} did not reassemble to valid UTF-8: {e}",
                done.chunk_id
            )),
        }
    }
}

/// Render a host-side transport failure as a frame the UI can dispatch on.
pub fn host_error_frame(message: &str) -> String {
    serde_json::json!({ "type": HOST_ERROR_FRAME_TYPE, "error": message }).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push(splitter: &mut LineSplitter, chunk: &[u8]) -> Vec<String> {
        splitter
            .push(chunk)
            .into_iter()
            .map(|r| r.expect("no framing error"))
            .collect()
    }

    fn oversized() -> Vec<u8> {
        vec![b'x'; MAX_FRAME_BYTES + 1]
    }

    #[test]
    fn splitter_holds_a_partial_frame_between_reads() {
        let mut splitter = LineSplitter::new();
        assert_eq!(
            push(&mut splitter, br#"{"type":"rea"#),
            Vec::<String>::new()
        );
        assert_eq!(
            push(&mut splitter, br#"dy","protocolVer"#),
            Vec::<String>::new()
        );
        assert_eq!(splitter.pending_bytes(), 28);
        assert_eq!(
            push(&mut splitter, b"sion\":1}\n"),
            vec![r#"{"type":"ready","protocolVersion":1}"#.to_string()]
        );
        assert_eq!(splitter.pending_bytes(), 0);
    }

    #[test]
    fn splitter_yields_every_frame_in_one_read() {
        let mut splitter = LineSplitter::new();
        assert_eq!(
            push(&mut splitter, b"{\"a\":1}\n{\"b\":2}\n{\"c\":3}\n"),
            vec!["{\"a\":1}", "{\"b\":2}", "{\"c\":3}"]
        );
    }

    #[test]
    fn splitter_returns_a_trailing_partial_only_once_completed() {
        let mut splitter = LineSplitter::new();
        assert_eq!(push(&mut splitter, b"{\"a\":1}\n{\"b\""), vec!["{\"a\":1}"]);
        assert_eq!(push(&mut splitter, b":2}\n"), vec!["{\"b\":2}"]);
    }

    #[test]
    fn splitter_drops_blank_lines_and_strips_crlf() {
        let mut splitter = LineSplitter::new();
        assert_eq!(
            push(&mut splitter, b"\n{\"a\":1}\r\n\n\r\n{\"b\":2}\n\n"),
            vec!["{\"a\":1}", "{\"b\":2}"]
        );
    }

    #[test]
    fn splitter_keeps_multibyte_characters_split_across_reads() {
        // "é" is 0xC3 0xA9; a text-level carry would decode each half as U+FFFD.
        let mut splitter = LineSplitter::new();
        assert_eq!(push(&mut splitter, &[b'{', 0xC3]), Vec::<String>::new());
        assert_eq!(push(&mut splitter, &[0xA9, b'}', b'\n']), vec!["{é}"]);
    }

    #[test]
    fn splitter_skips_an_oversized_line_up_to_its_newline() {
        let mut splitter = LineSplitter::new();
        assert_eq!(
            splitter.push(&oversized()),
            vec![Err(FramingError::LineTooLong)]
        );
        assert_eq!(splitter.pending_bytes(), 0);
        // Still the same line: none of it may surface as a frame.
        assert_eq!(
            push(&mut splitter, b"more of the same line"),
            Vec::<String>::new()
        );
        assert_eq!(push(&mut splitter, b"tail\n{\"a\":1}\n"), vec!["{\"a\":1}"]);
    }

    #[test]
    fn splitter_keeps_frames_read_together_with_an_oversized_line() {
        let mut splitter = LineSplitter::new();
        let mut read = b"{\"a\":1}\n".to_vec();
        read.extend(oversized());
        read.extend(b"\n{\"b\":2}\n");
        assert_eq!(
            splitter.push(&read),
            vec![
                Ok("{\"a\":1}".to_string()),
                Err(FramingError::LineTooLong),
                Ok("{\"b\":2}".to_string()),
            ]
        );
    }

    #[test]
    fn splitter_rejects_a_line_that_crosses_the_ceiling_across_reads() {
        let mut splitter = LineSplitter::new();
        assert_eq!(
            push(&mut splitter, &vec![b'x'; MAX_FRAME_BYTES]),
            Vec::<String>::new()
        );
        assert_eq!(
            splitter.push(b"x\n{\"a\":1}\n"),
            vec![Err(FramingError::LineTooLong), Ok("{\"a\":1}".to_string())]
        );
    }

    #[test]
    fn splitter_accepts_a_frame_exactly_at_the_ceiling() {
        let mut splitter = LineSplitter::new();
        let line = vec![b'y'; MAX_FRAME_BYTES];
        assert_eq!(push(&mut splitter, &line), Vec::<String>::new());
        let frames = push(&mut splitter, b"\n");
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].len(), MAX_FRAME_BYTES);
    }

    fn chunk(id: &str, index: usize, count: usize, byte_length: usize, part: &[u8]) -> String {
        serde_json::json!({
            "type": "rpc_chunk",
            "chunkId": id,
            "index": index,
            "count": count,
            "byteLength": byte_length,
            "data": STANDARD.encode(part),
        })
        .to_string()
    }

    fn feed_all(assembler: &mut ChunkAssembler, lines: Vec<String>) -> Vec<FrameOutcome> {
        lines
            .into_iter()
            .flat_map(|line| assembler.feed(line))
            .collect()
    }

    #[test]
    fn assembler_joins_several_chunks_of_one_sequence() {
        let payload = br#"{"type":"response","command":"get_state","success":true}"#;
        let (head, tail) = payload.split_at(20);
        let out = feed_all(
            &mut ChunkAssembler::new(),
            vec![
                chunk("rpc-1", 0, 2, payload.len(), head),
                chunk("rpc-1", 1, 2, payload.len(), tail),
            ],
        );
        assert_eq!(
            out,
            vec![FrameOutcome::Frame(
                String::from_utf8(payload.to_vec()).unwrap()
            )]
        );
    }

    #[test]
    fn assembler_reorders_chunks_that_arrive_out_of_order() {
        let payload = b"0123456789abcdefghij";
        let out = feed_all(
            &mut ChunkAssembler::new(),
            vec![
                chunk("rpc-2", 2, 3, payload.len(), &payload[14..]),
                chunk("rpc-2", 0, 3, payload.len(), &payload[..7]),
                chunk("rpc-2", 1, 3, payload.len(), &payload[7..14]),
            ],
        );
        assert_eq!(
            out,
            vec![FrameOutcome::Frame("0123456789abcdefghij".to_string())]
        );
    }

    #[test]
    fn assembler_drops_an_incomplete_sequence_when_a_new_one_starts() {
        let mut assembler = ChunkAssembler::new();
        assert_eq!(
            assembler.feed(chunk("rpc-3", 0, 4, 8, b"ab")),
            Vec::<FrameOutcome>::new()
        );
        assert!(assembler.has_pending());

        let out = assembler.feed(chunk("rpc-4", 0, 1, 2, b"zz"));
        assert_eq!(
            out,
            vec![
                FrameOutcome::ProtocolError(
                    "rpc_chunk sequence rpc-3 interleaved by rpc-4 after 1/4 segments; discarded"
                        .to_string()
                ),
                FrameOutcome::Frame("zz".to_string()),
            ]
        );
        assert!(
            !assembler.has_pending(),
            "a sequence missing segments must not stay buffered"
        );
    }

    #[test]
    fn assembler_drops_a_sequence_interrupted_by_a_plain_frame() {
        let mut assembler = ChunkAssembler::new();
        assembler.feed(chunk("rpc-5", 0, 2, 4, b"ab"));
        let out = assembler.feed(r#"{"type":"agent_end"}"#.to_string());
        assert_eq!(
            out,
            vec![
                FrameOutcome::ProtocolError(
                    "rpc_chunk sequence rpc-5 interrupted after 1/2 segments; discarded"
                        .to_string()
                ),
                FrameOutcome::Frame(r#"{"type":"agent_end"}"#.to_string()),
            ]
        );
        assert!(!assembler.has_pending());
    }

    #[test]
    fn assembler_forwards_plain_frames_untouched() {
        // Including a business frame that merely mentions the chunk type inside a string: the
        // host must not reinterpret payloads it does not own.
        let line = r#"{"type":"message_update","text":"the \"rpc_chunk\" format"}"#;
        assert_eq!(
            feed_all(&mut ChunkAssembler::new(), vec![line.to_string()]),
            vec![FrameOutcome::Frame(line.to_string())]
        );
    }

    #[test]
    fn assembler_rejects_a_byte_length_mismatch() {
        assert_eq!(
            feed_all(
                &mut ChunkAssembler::new(),
                vec![chunk("rpc-6", 0, 1, 99, b"short")]
            ),
            vec![FrameOutcome::ProtocolError(
                "rpc_chunk rpc-6 reassembled to 5 bytes, expected 99".to_string()
            )]
        );
    }

    #[test]
    fn assembler_rejects_a_duplicated_segment() {
        let mut assembler = ChunkAssembler::new();
        assembler.feed(chunk("rpc-7", 0, 2, 4, b"ab"));
        assert_eq!(
            assembler.feed(chunk("rpc-7", 0, 2, 4, b"ab")),
            vec![FrameOutcome::ProtocolError(
                "rpc_chunk rpc-7 segment 0 received twice".to_string()
            )]
        );
        assert!(!assembler.has_pending());
    }

    #[test]
    fn assembler_rejects_a_count_that_would_size_a_hostile_allocation() {
        let out = feed_all(
            &mut ChunkAssembler::new(),
            vec![chunk("rpc-8", 0, MAX_CHUNK_COUNT + 1, 4, b"ab")],
        );
        assert!(
            matches!(out.as_slice(), [FrameOutcome::ProtocolError(e)] if e.contains("declares count")),
            "{out:?}"
        );
    }

    fn single_error(lines: Vec<String>) -> String {
        let mut assembler = ChunkAssembler::new();
        let out = feed_all(&mut assembler, lines);
        assert!(
            !assembler.has_pending(),
            "a rejected sequence must not stay buffered"
        );
        match out.as_slice() {
            [FrameOutcome::ProtocolError(e)] => e.clone(),
            other => panic!("expected one protocol error, got {other:?}"),
        }
    }

    #[test]
    fn assembler_rejects_an_index_outside_the_declared_count() {
        let e = single_error(vec![chunk("rpc-11", 2, 2, 4, b"ab")]);
        assert!(e.contains("index 2 out of range"), "{e}");
    }

    #[test]
    fn assembler_rejects_a_byte_length_above_the_ceiling_or_zero() {
        let e = single_error(vec![chunk(
            "rpc-12",
            0,
            1,
            MAX_REASSEMBLED_FRAME_BYTES + 1,
            b"ab",
        )]);
        assert!(e.contains("byteLength"), "{e}");
        let e = single_error(vec![chunk("rpc-13", 0, 1, 0, b"")]);
        assert!(e.contains("byteLength 0"), "{e}");
    }

    #[test]
    fn assembler_rejects_data_that_is_not_base64() {
        let line = serde_json::json!({
            "type": "rpc_chunk",
            "chunkId": "rpc-14",
            "index": 0,
            "count": 1,
            "byteLength": 4,
            "data": "not base64!",
        })
        .to_string();
        let e = single_error(vec![line]);
        assert!(e.contains("not valid base64"), "{e}");
    }

    #[test]
    fn assembler_rejects_segments_longer_than_the_declared_length() {
        let e = single_error(vec![chunk("rpc-15", 0, 2, 3, b"abcd")]);
        assert!(e.contains("overflows its declared 3 bytes"), "{e}");
    }

    #[test]
    fn assembler_rejects_a_header_that_changes_mid_sequence() {
        let mut assembler = ChunkAssembler::new();
        assembler.feed(chunk("rpc-16", 0, 3, 6, b"ab"));
        assert_eq!(
            assembler.feed(chunk("rpc-16", 1, 2, 6, b"cd")),
            vec![FrameOutcome::ProtocolError(
                "rpc_chunk rpc-16 header changed mid-sequence (count 3->2, byteLength 6->6)"
                    .to_string()
            )]
        );
        assert!(!assembler.has_pending());
    }

    #[test]
    fn assembler_rejects_a_sequence_that_never_decodes_to_utf8() {
        let out = feed_all(
            &mut ChunkAssembler::new(),
            vec![chunk("rpc-9", 0, 1, 1, &[0xFF])],
        );
        assert!(
            matches!(out.as_slice(), [FrameOutcome::ProtocolError(e)] if e.contains("valid UTF-8")),
            "{out:?}"
        );
    }

    #[test]
    fn assembler_joins_a_multibyte_character_split_across_segments() {
        let payload = "héllo".as_bytes();
        let out = feed_all(
            &mut ChunkAssembler::new(),
            vec![
                chunk("rpc-10", 0, 2, payload.len(), &payload[..2]),
                chunk("rpc-10", 1, 2, payload.len(), &payload[2..]),
            ],
        );
        assert_eq!(out, vec![FrameOutcome::Frame("héllo".to_string())]);
    }

    #[test]
    fn host_error_frame_is_namespaced_json() {
        let frame = host_error_frame("boom");
        let parsed: serde_json::Value = serde_json::from_str(&frame).unwrap();
        assert_eq!(parsed["type"], HOST_ERROR_FRAME_TYPE);
        assert_eq!(parsed["error"], "boom");
    }
}
