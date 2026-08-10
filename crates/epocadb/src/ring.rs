//! A fixed-size ring buffer for log lines.
//!
//! Lines are separated by `\n`. When full, the oldest line is dropped — a log that
//! arrived is better than a UI thread blocked on backpressure. A `dropped` counter
//! tracks how many lines were lost so the host can report the gap.
//!
//! # The invariant
//!
//! Every byte in the buffer belongs to a line terminated by `\n`. [`push`](RingBuffer::push)
//! never writes a body without its terminator, and nothing else writes at all. That is what
//! lets [`find_delim`](RingBuffer::find_delim) return `None` only for an empty buffer, and
//! it is the property the earlier version broke: it could skip the terminator when the
//! buffer was one byte short, and then `find_delim` fell through to `head`, `pop` set
//! `tail` one past `head`, and `len()` — a masked subtraction — reported nearly `N`.
//! A ring that believes it holds 2047 bytes of a 2048-byte buffer never accepts another
//! line again.

/// Power-of-two byte buffer with head/tail cursors.
pub struct RingBuffer<const N: usize> {
    buf: [u8; N],
    /// Where the next byte is written.
    head: usize,
    /// Where the next byte is read.
    tail: usize,
    /// How many lines were dropped because the buffer was full.
    pub dropped: u32,
}

impl<const N: usize> RingBuffer<N> {
    /// Evaluated at compile time: a non-power-of-two `N` silently corrupts the wrap mask,
    /// so it must not be expressible. A `debug_assert` in `new` was the previous guard and
    /// said nothing in the release build that actually runs on the phone.
    const POWER_OF_TWO: () = assert!(N.is_power_of_two(), "RingBuffer size must be a power of two");

    pub fn new() -> Self {
        let () = Self::POWER_OF_TWO;
        RingBuffer { buf: [0u8; N], head: 0, tail: 0, dropped: 0 }
    }

    pub fn is_empty(&self) -> bool {
        self.head == self.tail
    }

    /// Bytes currently buffered.
    pub fn len(&self) -> usize {
        self.span(self.tail, self.head)
    }

    /// Bytes that can still be written. One slot stays free so a full buffer is
    /// distinguishable from an empty one.
    fn available(&self) -> usize {
        N - self.len() - 1
    }

    /// Push a line. `\n` is appended; the caller should not include it.
    ///
    /// A line longer than half the buffer is truncated rather than allowed to evict
    /// everything else — a truncated line is still a line, and losing the whole log to
    /// one stray `Debug` of a packet is worse than losing that packet's tail.
    pub fn push(&mut self, line: &str) {
        let max_body = (N / 2).saturating_sub(1);
        let body = &line.as_bytes()[..floor_char_boundary(line, max_body)];
        let need = body.len() + 1;

        while self.available() < need {
            if !self.drop_oldest() {
                // Empty and still short of room is arithmetically impossible — `need` is
                // at most N/2 and an empty buffer has N-1 available. Bail rather than spin.
                return;
            }
        }

        self.write_slice(body);
        self.write_byte(b'\n');
    }

    /// Copy the oldest line into `out`, without its trailing newline, and remove it.
    /// Returns how many bytes were written.
    ///
    /// Takes a caller-provided buffer because a line may wrap the end of the array, and
    /// there is nowhere to borrow a contiguous `&str` from. The version that returned
    /// `Option<&str>` handled that by returning only the part before the wrap — silently,
    /// and only once the buffer had been used enough to wrap at all.
    pub fn pop_into(&mut self, out: &mut [u8]) -> Option<usize> {
        let end = self.find_delim()?;
        let len = self.span(self.tail, end);
        let copied = len.min(out.len());
        self.copy_out(self.tail, copied, out);
        // Consume the whole line even when `out` was too small: leaving a partial line
        // behind would desynchronise every later read.
        self.tail = self.next(end);
        Some(copied)
    }

    /// Drain whole lines into `out`, returning the number of bytes written.
    ///
    /// A line that does not fit stays in the buffer for the next call. The bytes written
    /// are always a whole number of `\n`-terminated lines, so the reader on the other end
    /// never has to reassemble one.
    pub fn drain_into(&mut self, out: &mut [u8]) -> usize {
        let mut n = 0;
        while let Some(end) = self.find_delim() {
            let line_len = self.span(self.tail, end) + 1; // including the \n
            if n + line_len > out.len() {
                break;
            }
            self.copy_out(self.tail, line_len, &mut out[n..]);
            n += line_len;
            self.tail = self.next(end);
        }
        n
    }

    // -- internal helpers --

    /// Distance from `from` to `to` going forward, which is the only direction the
    /// cursors move.
    fn span(&self, from: usize, to: usize) -> usize {
        to.wrapping_sub(from) & (N - 1)
    }

    fn next(&self, pos: usize) -> usize {
        (pos + 1) & (N - 1)
    }

    fn write_byte(&mut self, b: u8) {
        self.buf[self.head] = b;
        self.head = self.next(self.head);
    }

    fn write_slice(&mut self, data: &[u8]) {
        for &b in data {
            self.write_byte(b);
        }
    }

    /// Copy `len` bytes starting at `start` into `out`, following the wrap.
    fn copy_out(&self, start: usize, len: usize, out: &mut [u8]) {
        let first = (N - start).min(len);
        out[..first].copy_from_slice(&self.buf[start..start + first]);
        if len > first {
            out[first..len].copy_from_slice(&self.buf[..len - first]);
        }
    }

    /// Position of the next `\n`, or `None` when the buffer is empty.
    fn find_delim(&self) -> Option<usize> {
        let mut pos = self.tail;
        while pos != self.head {
            if self.buf[pos] == b'\n' {
                return Some(pos);
            }
            pos = self.next(pos);
        }
        None
    }

    /// Drop the oldest line and count it. Returns false when there was nothing to drop.
    fn drop_oldest(&mut self) -> bool {
        match self.find_delim() {
            Some(end) => {
                self.tail = self.next(end);
                self.dropped = self.dropped.saturating_add(1);
                true
            }
            None => false,
        }
    }
}

impl<const N: usize> Default for RingBuffer<N> {
    fn default() -> Self {
        Self::new()
    }
}

/// The largest index `<= max` that does not split a UTF-8 codepoint.
///
/// `str::floor_char_boundary` is still unstable. Truncating mid-codepoint would make the
/// drained bytes invalid UTF-8, which the host decodes with `errors="replace"` — so it
/// would show as a replacement character rather than a crash, and would be that much
/// harder to trace back to here.
fn floor_char_boundary(s: &str, max: usize) -> usize {
    if max >= s.len() {
        return s.len();
    }
    let mut i = max;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;
    use alloc::string::String;
    use alloc::vec::Vec;

    /// Pop every line as an owned String, so a test can assert on order and content
    /// without caring where the wrap fell.
    fn drain_lines<const N: usize>(rb: &mut RingBuffer<N>) -> Vec<String> {
        let mut out = Vec::new();
        let mut scratch = [0u8; 512];
        while let Some(n) = rb.pop_into(&mut scratch) {
            out.push(String::from_utf8_lossy(&scratch[..n]).into_owned());
        }
        out
    }

    #[test]
    fn empty_buffer_has_no_lines() {
        let mut rb = RingBuffer::<64>::new();
        assert!(rb.is_empty());
        assert_eq!(rb.pop_into(&mut [0u8; 32]), None);
        assert_eq!(rb.drain_into(&mut [0u8; 32]), 0);
    }

    #[test]
    fn push_and_pop_one_line() {
        let mut rb = RingBuffer::<64>::new();
        rb.push("hello");
        assert!(!rb.is_empty());
        assert_eq!(drain_lines(&mut rb), ["hello"]);
        assert!(rb.is_empty());
    }

    #[test]
    fn multiple_lines_come_back_in_order() {
        let mut rb = RingBuffer::<64>::new();
        rb.push("first");
        rb.push("second");
        rb.push("third");
        assert_eq!(drain_lines(&mut rb), ["first", "second", "third"]);
        assert!(rb.is_empty());
    }

    #[test]
    fn when_full_the_oldest_line_is_dropped() {
        let mut rb = RingBuffer::<64>::new();
        for i in 0..20 {
            rb.push(&format!("line-{i}"));
        }
        assert!(rb.dropped > 0, "old lines should have been dropped");
        let lines = drain_lines(&mut rb);
        // Whatever survived must be the newest lines, contiguous, ending at the last push.
        assert_eq!(lines.last().unwrap(), "line-19");
        assert!(rb.dropped as usize + lines.len() == 20, "every line is either kept or counted");
    }

    // ---- the wrap, which is where every previous bug lived ----

    #[test]
    fn every_line_survives_the_wrap_intact() {
        // 11 bytes per line into a 64-byte buffer: 64 is not a multiple of 11, so lines
        // straddle the end of the array again and again. The old `pop` returned only the
        // bytes before the wrap — silently, and only once the buffer had been used
        // enough to wrap at all, which no test here ever did.
        let mut rb = RingBuffer::<64>::new();
        for _ in 0..40 {
            rb.push("0123456789");
        }
        let lines = drain_lines(&mut rb);
        assert!(!lines.is_empty());
        for line in &lines {
            assert_eq!(line, "0123456789", "a line was cut by the wrap: {lines:?}");
        }
    }

    #[test]
    fn the_buffer_still_accepts_lines_after_many_wraps() {
        // The corruption the old code produced was permanent: once `tail` passed `head`,
        // `len()` — a masked subtraction — reported nearly N, and the ring never took
        // another line. Cycling through the buffer many times is what surfaces it.
        //
        // The accounting is the real assertion: every line pushed must be either
        // retrievable or counted as dropped. Anything else means bytes went somewhere
        // the bookkeeping does not know about.
        let mut rb = RingBuffer::<64>::new();
        const PUSHED: usize = 500;
        for i in 0..PUSHED {
            rb.push(&format!("entry-{i}"));
            assert!(rb.len() < 64, "len must stay inside the buffer, was {} at {i}", rb.len());
        }
        let dropped = rb.dropped as usize;
        let lines = drain_lines(&mut rb);
        assert!(!lines.is_empty(), "the ring stopped accepting lines");
        assert_eq!(lines.last().unwrap(), "entry-499");
        assert_eq!(dropped + lines.len(), PUSHED, "lines went missing unaccounted for");
        for (i, line) in lines.iter().enumerate() {
            let expected = format!("entry-{}", dropped + i);
            assert_eq!(line, &expected, "line {i} came back wrong: {lines:?}");
        }
    }

    #[test]
    fn drain_into_after_a_wrap_returns_whole_lines() {
        let mut rb = RingBuffer::<64>::new();
        for i in 0..40 {
            rb.push(&format!("w{i}"));
        }
        let mut out = [0u8; 256];
        let n = rb.drain_into(&mut out);
        let text = core::str::from_utf8(&out[..n]).expect("drained bytes must be valid UTF-8");
        assert!(text.ends_with('\n'), "drain must end on a line boundary: {text:?}");
        for line in text.lines() {
            assert!(line.starts_with('w'), "a line was cut by the wrap: {line:?}");
        }
    }

    #[test]
    fn a_line_longer_than_the_buffer_is_truncated_not_rejected() {
        let mut rb = RingBuffer::<64>::new();
        rb.push(&"x".repeat(200));
        assert!(!rb.is_empty(), "even a huge line must produce something");
        let lines = drain_lines(&mut rb);
        assert_eq!(lines.len(), 1);
        assert!(!lines[0].is_empty());
        assert!(lines[0].len() <= 32, "capped at N/2, was {}", lines[0].len());
    }

    #[test]
    fn a_huge_line_does_not_evict_everything_that_follows_it() {
        let mut rb = RingBuffer::<64>::new();
        rb.push(&"x".repeat(500));
        rb.push("after");
        let lines = drain_lines(&mut rb);
        assert!(
            lines.contains(&String::from("after")),
            "a later line must still fit alongside a truncated one, got {lines:?}"
        );
    }

    #[test]
    fn truncation_never_splits_a_codepoint() {
        // Each 'é' is two bytes, so a naive cut at N/2-1 lands mid-character.
        let mut rb = RingBuffer::<64>::new();
        rb.push(&"é".repeat(100));
        let mut scratch = [0u8; 64];
        let n = rb.pop_into(&mut scratch).unwrap();
        core::str::from_utf8(&scratch[..n]).expect("truncated line must stay valid UTF-8");
    }

    #[test]
    fn an_empty_line_is_still_a_line() {
        let mut rb = RingBuffer::<64>::new();
        rb.push("");
        rb.push("next");
        assert_eq!(drain_lines(&mut rb), ["", "next"]);
    }

    // ---- draining ----

    #[test]
    fn drain_into_empties_the_buffer() {
        let mut rb = RingBuffer::<64>::new();
        rb.push("alpha");
        rb.push("beta");
        let mut out = [0u8; 64];
        let n = rb.drain_into(&mut out);
        assert!(n > 0);
        assert!(rb.is_empty());
        let drained = core::str::from_utf8(&out[..n]).unwrap();
        assert_eq!(drained, "alpha\nbeta\n");
    }

    #[test]
    fn drain_into_a_short_buffer_keeps_the_rest_and_loses_nothing() {
        let mut rb = RingBuffer::<64>::new();
        rb.push("aaaa");
        rb.push("bbbb");
        rb.push("cccc");

        // Room for one line only.
        let mut out = [0u8; 6];
        let n = rb.drain_into(&mut out);
        assert_eq!(&out[..n], b"aaaa\n");
        assert!(!rb.is_empty());

        let mut rest = [0u8; 64];
        let n2 = rb.drain_into(&mut rest);
        assert_eq!(&rest[..n2], b"bbbb\ncccc\n", "the earlier partial drain ate a line");
        assert_eq!(rb.dropped, 0, "a drain is not a drop");
    }

    #[test]
    fn drain_into_a_buffer_too_small_for_any_line_takes_nothing() {
        let mut rb = RingBuffer::<64>::new();
        rb.push("longer-than-four");
        let mut out = [0u8; 4];
        assert_eq!(rb.drain_into(&mut out), 0);
        assert!(!rb.is_empty(), "an unfittable line must stay put, not vanish");
    }

    #[test]
    fn dropped_counter_increments_per_line() {
        let mut rb = RingBuffer::<64>::new();
        for i in 0..100 {
            rb.push(&format!("item-{i}"));
        }
        assert!(rb.dropped > 0);
        let before = rb.dropped;
        rb.drain_into(&mut [0u8; 128]);
        assert_eq!(rb.dropped, before, "draining is not dropping");
    }

    #[test]
    fn interleaved_push_and_drain_never_corrupts_the_cursors() {
        // The shape the bridge actually produces: log, flush, log, flush, with the
        // flush buffer sometimes too small. Any cursor bug shows up as a length
        // outside the array or as bytes that are not a whole line.
        let mut rb = RingBuffer::<64>::new();
        let mut out = [0u8; 20];
        for i in 0..300 {
            rb.push(&format!("tick-{i}"));
            if i % 3 == 0 {
                let n = rb.drain_into(&mut out);
                let text = core::str::from_utf8(&out[..n]).unwrap();
                assert!(text.is_empty() || text.ends_with('\n'));
            }
            assert!(rb.len() < 64);
        }
    }
}
