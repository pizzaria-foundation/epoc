//! The TCP framing under MTProto.
//!
//! TCP is a stream and MTProto is messages, so something has to say where one ends. Telegram
//! offers four ways and this implements one.
//!
//! # Why intermediate
//!
//! | | |
//! |---|---|
//! | abridged | 1 byte of `len/4`, escaping to 4 bytes past 127. Saves three bytes per message and costs a branch on every read. |
//! | **intermediate** | 4 bytes of little-endian length. |
//! | full | length, sequence number, payload, CRC32 — with the length counted *including itself*, which is the off-by-four this format is known for. |
//! | padded | intermediate plus random padding, for traffic obfuscation. |
//!
//! Intermediate, because the saving from abridged is three bytes against a 4 KB read buffer
//! and the cost is a length field that means two different things depending on its first
//! byte. `full` adds a CRC over a connection that already has one and a sequence number
//! that duplicates `seq_no` a layer up. Neither is worth the code on a device where every
//! branch is a place to be wrong once and never notice.
//!
//! Obfuscation is the one that might be worth revisiting: `padded` and the obfuscated
//! variants exist because some networks block Telegram by traffic shape. Not needed to make
//! it work, so not here.
//!
//! # The greeting
//!
//! Before any message, the client sends four bytes naming the format — `0xeeeeeeee` for
//! intermediate. It is sent once per connection and never acknowledged; the first sign of
//! getting it wrong is the server closing the connection after the first message, which
//! looks exactly like a bad message rather than a bad greeting.

use alloc::vec::Vec;

/// The four bytes that select intermediate framing.
pub const GREETING: [u8; 4] = [0xee, 0xee, 0xee, 0xee];

/// Longest frame accepted from the server.
///
/// Telegram will not send more than about a megabyte in one message. A length field is the
/// first thing an attacker on the path controls and the first thing a desynchronised stream
/// corrupts, so it is bounded here: without this, four bad bytes ask a phone with 45 MB of
/// RAM to buffer four gigabytes.
pub const MAX_FRAME: usize = 1 << 20;

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Error {
    /// The declared frame length is past [`MAX_FRAME`] or not a multiple of four.
    BadLength(u32),
    /// The server reported an error in place of a frame. See [`Frame::Error`].
    Server(i32),
}

/// What came out of the receive buffer.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Frame {
    /// Not enough bytes yet. Feed more and ask again.
    Incomplete,
    /// A complete message payload.
    Message(Vec<u8>),
    /// A transport-level error.
    ///
    /// Telegram signals these as a four-byte frame holding a negative number, *outside* any
    /// TL encoding — so this arrives before there is a session to report it through, and
    /// before `rpc_error` exists as a concept. `-404` for an unknown `auth_key_id` is the
    /// one that actually happens: it means the key was accepted once and has since been
    /// forgotten, and the answer is to discard it and redo the handshake rather than retry.
    Error(i32),
}

/// Accumulates bytes from a socket and hands back whole frames.
///
/// Separate from the socket on purpose. `symbian::net::TcpStream` delivers whatever arrived,
/// which is not whatever was sent — a 300-byte message can arrive as 40 bytes and then 260,
/// and the reassembly is where that goes wrong. Keeping it here means it is tested by
/// feeding one byte at a time, which is a thing no real network will reliably do on demand
/// and which finds the bug immediately.
#[derive(Default)]
pub struct Transport {
    rx: Vec<u8>,
}

impl Transport {
    pub fn new() -> Self {
        Transport { rx: Vec::new() }
    }

    /// The bytes to send before anything else on a new connection.
    pub fn greeting(&self) -> &'static [u8] {
        &GREETING
    }

    /// Wrap a payload for sending.
    pub fn frame(payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(4 + payload.len());
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(payload);
        out
    }

    /// Add bytes as they arrive from the socket.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.rx.extend_from_slice(bytes);
    }

    pub fn buffered(&self) -> usize {
        self.rx.len()
    }

    /// Take the next complete frame, if there is one.
    ///
    /// Call in a loop until it returns [`Frame::Incomplete`]: one `read` from the socket can
    /// deliver several messages, and a caller that handles only the first leaves the rest in
    /// the buffer until more bytes happen to arrive. That produces a client that works while
    /// traffic is steady and stalls the moment it goes quiet, which is the hardest kind of
    /// bug to reproduce on purpose.
    pub fn next(&mut self) -> core::result::Result<Frame, Error> {
        if self.rx.len() < 4 {
            return Ok(Frame::Incomplete);
        }
        let len = u32::from_le_bytes([self.rx[0], self.rx[1], self.rx[2], self.rx[3]]);

        // A transport error is a four-byte frame whose payload is a negative int32. It is
        // recognised by the *length* being 4 and the value negative, before any TL parsing,
        // because at handshake time there is no session to decode it with.
        if len == 4 {
            if self.rx.len() < 8 {
                return Ok(Frame::Incomplete);
            }
            let code = i32::from_le_bytes([self.rx[4], self.rx[5], self.rx[6], self.rx[7]]);
            self.rx.drain(..8);
            if code < 0 {
                return Ok(Frame::Error(code));
            }
            // A positive four-byte payload is not an error; hand it on as a message rather
            // than swallowing it.
            return Ok(Frame::Message(code.to_le_bytes().to_vec()));
        }

        let len = len as usize;
        if len > MAX_FRAME || len % 4 != 0 {
            // Not recoverable: the stream is desynchronised and every subsequent length is
            // read from the middle of someone else's message. The caller must reconnect.
            return Err(Error::BadLength(len as u32));
        }
        if self.rx.len() < 4 + len {
            return Ok(Frame::Incomplete);
        }

        let payload = self.rx[4..4 + len].to_vec();
        self.rx.drain(..4 + len);
        Ok(Frame::Message(payload))
    }

    /// Forget everything buffered. For use after an error, before reconnecting.
    pub fn reset(&mut self) {
        self.rx.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_round_trips() {
        let payload = alloc::vec![7u8; 16];
        let wire = Transport::frame(&payload);
        assert_eq!(wire.len(), 20);
        let mut t = Transport::new();
        t.feed(&wire);
        assert_eq!(t.next().unwrap(), Frame::Message(payload));
        assert_eq!(t.next().unwrap(), Frame::Incomplete);
    }

    #[test]
    fn one_byte_at_a_time_still_reassembles() {
        // The case a real network produces rarely and a test can produce always. A reader
        // that assumes one read is one message passes every other test here and fails on
        // the first fragmented reply.
        let payload: Vec<u8> = (0..64u8).collect();
        let wire = Transport::frame(&payload);
        let mut t = Transport::new();
        for (i, b) in wire.iter().enumerate() {
            t.feed(&[*b]);
            if i + 1 < wire.len() {
                assert_eq!(t.next().unwrap(), Frame::Incomplete, "completed early at {i}");
            }
        }
        assert_eq!(t.next().unwrap(), Frame::Message(payload));
    }

    #[test]
    fn several_frames_in_one_read_all_come_out() {
        // The bug this prevents: handling only the first message of a read. It leaves the
        // rest buffered until more traffic arrives, so the client works under load and
        // stalls when idle.
        let a = alloc::vec![1u8; 8];
        let b = alloc::vec![2u8; 12];
        let c = alloc::vec![3u8; 4];
        let mut wire = Transport::frame(&a);
        wire.extend(Transport::frame(&b));
        wire.extend(Transport::frame(&c));

        let mut t = Transport::new();
        t.feed(&wire);
        assert_eq!(t.next().unwrap(), Frame::Message(a));
        assert_eq!(t.next().unwrap(), Frame::Message(b));
        assert_eq!(t.next().unwrap(), Frame::Message(c));
        assert_eq!(t.next().unwrap(), Frame::Incomplete);
        assert_eq!(t.buffered(), 0);
    }

    #[test]
    fn a_negative_four_byte_frame_is_a_transport_error() {
        let mut t = Transport::new();
        t.feed(&4u32.to_le_bytes());
        t.feed(&(-404i32).to_le_bytes());
        assert_eq!(t.next().unwrap(), Frame::Error(-404));
    }

    #[test]
    fn a_positive_four_byte_frame_is_a_message() {
        // Only negatives are errors. Treating every short frame as one would silently
        // discard a legitimate reply that happens to be four bytes.
        let mut t = Transport::new();
        t.feed(&4u32.to_le_bytes());
        t.feed(&7i32.to_le_bytes());
        assert_eq!(t.next().unwrap(), Frame::Message(7i32.to_le_bytes().to_vec()));
    }

    #[test]
    fn an_error_frame_split_across_reads_waits() {
        let mut t = Transport::new();
        t.feed(&4u32.to_le_bytes());
        assert_eq!(t.next().unwrap(), Frame::Incomplete);
        t.feed(&(-404i32).to_le_bytes());
        assert_eq!(t.next().unwrap(), Frame::Error(-404));
    }

    #[test]
    fn an_absurd_length_is_an_error_not_an_allocation() {
        let mut t = Transport::new();
        t.feed(&0xffff_fff0u32.to_le_bytes());
        t.feed(&[0u8; 4]);
        assert!(matches!(t.next(), Err(Error::BadLength(_))));
    }

    #[test]
    fn an_unaligned_length_is_rejected() {
        // Every MTProto payload is a whole number of 32-bit words. A length that is not
        // means the stream is desynchronised, and continuing would read the next length
        // from the middle of this message.
        let mut t = Transport::new();
        t.feed(&13u32.to_le_bytes());
        t.feed(&[0u8; 16]);
        assert_eq!(t.next(), Err(Error::BadLength(13)));
    }

    #[test]
    fn the_greeting_is_four_bytes_of_ee() {
        assert_eq!(Transport::new().greeting(), &[0xee, 0xee, 0xee, 0xee]);
    }

    #[test]
    fn an_empty_frame_is_legal_and_consumed() {
        // Length zero is well-formed. A reader that treats it as Incomplete never advances
        // and spins forever on the same four bytes.
        let mut t = Transport::new();
        t.feed(&0u32.to_le_bytes());
        assert_eq!(t.next().unwrap(), Frame::Message(Vec::new()));
        assert_eq!(t.buffered(), 0);
    }
}
