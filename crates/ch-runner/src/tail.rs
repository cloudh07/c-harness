//! Cutting process output to a byte budget without splitting a character.

/// The last `max` bytes of `bytes` as text, and whether anything was dropped.
pub fn cut_tail(bytes: &[u8], max: usize) -> (String, bool) {
    let mut start = bytes.len().saturating_sub(max);
    // A cut can land inside a character; skip at most the three bytes that can follow its start.
    let limit = (start + 3).min(bytes.len());
    while start < limit && is_continuation(bytes[start]) {
        start += 1;
    }
    let text = String::from_utf8_lossy(&bytes[start..]);
    if text.len() <= max {
        return (text.into_owned(), start > 0);
    }
    // Invalid bytes widen into U+FFFD, so the text can still be over budget.
    let cut = text.ceil_char_boundary(text.len() - max);
    (text[cut..].to_owned(), true)
}

/// The first `max` bytes of `bytes` as text, and whether anything was dropped.
pub fn cut_head(bytes: &[u8], max: usize) -> (String, bool) {
    if bytes.len() <= max {
        let text = String::from_utf8_lossy(bytes);
        if text.len() <= max {
            return (text.into_owned(), false);
        }
    }
    let mut end = max.min(bytes.len());
    let limit = end.saturating_sub(3);
    while end > limit && end < bytes.len() && is_continuation(bytes[end]) {
        end -= 1;
    }
    let text = String::from_utf8_lossy(&bytes[..end]);
    let cut = text.floor_char_boundary(max.min(text.len()));
    (text[..cut].to_owned(), true)
}

fn is_continuation(byte: u8) -> bool {
    byte & 0b1100_0000 == 0b1000_0000
}

/// Keeps the end of a stream that may be far larger than the budget.
pub struct TailBuffer {
    bytes: Vec<u8>,
    max: usize,
    dropped: bool,
}

impl TailBuffer {
    pub fn new(max: usize) -> Self {
        Self {
            bytes: Vec::new(),
            max,
            dropped: false,
        }
    }

    pub fn push(&mut self, chunk: &[u8]) {
        self.bytes.extend_from_slice(chunk);
        // Draining only past twice the budget keeps the copy cost linear in the input.
        if self.bytes.len() > self.max * 2 {
            let excess = self.bytes.len() - self.max;
            self.bytes.drain(..excess);
            self.dropped = true;
        }
    }

    pub fn text(&self) -> (String, bool) {
        let (text, cut) = cut_tail(&self.bytes, self.max);
        (text, cut || self.dropped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_input_is_kept_whole() {
        assert_eq!(cut_tail(b"hello", 10), ("hello".to_owned(), false));
        assert_eq!(cut_head(b"hello", 10), ("hello".to_owned(), false));
    }

    #[test]
    fn tail_starts_on_a_character_boundary() {
        let text = "aé€😀";
        let bytes = text.as_bytes();
        for max in 0..=bytes.len() {
            let (tail, cut) = cut_tail(bytes, max);
            assert!(tail.len() <= max, "max {max}: {tail:?}");
            assert!(text.ends_with(&tail), "max {max}: {tail:?}");
            assert_eq!(cut, max < bytes.len(), "max {max}");
        }
    }

    #[test]
    fn head_ends_on_a_character_boundary() {
        let text = "aé€😀b";
        let bytes = text.as_bytes();
        for max in 0..=bytes.len() {
            let (head, cut) = cut_head(bytes, max);
            assert!(head.len() <= max, "max {max}: {head:?}");
            assert!(text.starts_with(&head), "max {max}: {head:?}");
            assert_eq!(cut, max < bytes.len(), "max {max}");
        }
    }

    #[test]
    fn invalid_bytes_stay_within_budget() {
        let bytes = [0xff_u8; 16];
        let (tail, cut) = cut_tail(&bytes, 8);
        assert!(tail.len() <= 8);
        assert!(cut);
        let (head, cut) = cut_head(&bytes, 8);
        assert!(head.len() <= 8);
        assert!(cut);
    }

    #[test]
    fn buffer_keeps_the_end_of_a_long_stream() {
        let mut buffer = TailBuffer::new(4);
        for chunk in [b"abc".as_slice(), b"def", b"ghi", b"jkl"] {
            buffer.push(chunk);
        }
        assert_eq!(buffer.text(), ("ijkl".to_owned(), true));
    }

    #[test]
    fn buffer_reports_no_cut_when_everything_fits() {
        let mut buffer = TailBuffer::new(8);
        buffer.push(b"abc");
        buffer.push(b"de");
        assert_eq!(buffer.text(), ("abcde".to_owned(), false));
    }
}
