//! A byte buffer for parsing Server-Sent Events.
//!
//! Network chunks split at arbitrary byte offsets, so decoding each chunk to
//! UTF-8 on its own (`String::from_utf8_lossy(&chunk)`) corrupts any
//! multi-byte character — CJK, emoji, accented text — that straddles a chunk
//! boundary, replacing both halves with U+FFFD. When the split lands inside a
//! streamed tool-call argument, the accumulated JSON is then invalid and the
//! whole call silently degrades. Buffering the raw bytes and decoding only
//! complete lines fixes this: lines are delimited by `\n` (byte 0x0A), which
//! is never part of a multi-byte sequence, so a complete line is always valid
//! UTF-8 to the extent the stream is.
#[derive(Default)]
pub(crate) struct LineBuffer {
    buf: Vec<u8>,
}

impl LineBuffer {
    pub(crate) fn push(&mut self, chunk: &[u8]) {
        self.buf.extend_from_slice(chunk);
    }

    /// Pop the next complete line (newline consumed, not included), or `None`
    /// when no full line is buffered yet.
    pub(crate) fn next_line(&mut self) -> Option<String> {
        let pos = self.buf.iter().position(|&b| b == b'\n')?;
        let line: Vec<u8> = self.buf.drain(..=pos).collect();
        Some(String::from_utf8_lossy(&line[..line.len() - 1]).into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reassembles_multibyte_chars_split_across_chunks() {
        // "café 🎉\n" split so that both the é and the emoji straddle a
        // boundary. Naive per-chunk decoding would corrupt both.
        let full = "data: café 🎉\n".as_bytes().to_vec();
        let mut lb = LineBuffer::default();
        // Split at byte 7 (mid-é) and byte 12 (mid-emoji).
        lb.push(&full[..7]);
        assert!(lb.next_line().is_none());
        lb.push(&full[7..12]);
        lb.push(&full[12..]);
        assert_eq!(lb.next_line().as_deref(), Some("data: café 🎉"));
        assert!(lb.next_line().is_none());
    }

    #[test]
    fn yields_multiple_lines_and_keeps_remainder() {
        let mut lb = LineBuffer::default();
        lb.push(b"one\ntwo\npartial");
        assert_eq!(lb.next_line().as_deref(), Some("one"));
        assert_eq!(lb.next_line().as_deref(), Some("two"));
        assert!(lb.next_line().is_none());
        lb.push(b"-rest\n");
        assert_eq!(lb.next_line().as_deref(), Some("partial-rest"));
    }
}
