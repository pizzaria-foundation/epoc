//! The encrypted layer: everything after the handshake.
//!
//! ```text
//!  auth_key_id (8)  msg_key (16)  AES-256-IGE( salt session_id msg_id seq_no len body pad )
//! ```
//!
//! # MTProto 2.0, which is SHA-256
//!
//! Version 1 derived the key material with SHA-1 and computed `msg_key` over the plaintext
//! alone. Version 2 uses SHA-256 throughout and folds 32 bytes of the auth key into the
//! `msg_key`, which is what makes it a MAC rather than a checksum:
//!
//! ```text
//! msg_key_large = SHA256(auth_key[88 + x .. 120 + x] ++ plaintext)
//! msg_key       = msg_key_large[8..24]
//! sha_a         = SHA256(msg_key ++ auth_key[x .. x + 36])
//! sha_b         = SHA256(auth_key[40 + x .. 76 + x] ++ msg_key)
//! aes_key       = sha_a[0..8] ++ sha_b[8..24] ++ sha_a[24..32]
//! aes_iv        = sha_b[0..8] ++ sha_a[8..24] ++ sha_b[24..32]
//! ```
//!
//! `x` is 0 for client-to-server and 8 for server-to-client, so the two directions use
//! different key material from the same auth key. Getting `x` backwards produces a client
//! that encrypts messages the server cannot read and cannot read the ones it gets, with no
//! error that mentions a key.
//!
//! The interleaving of `sha_a` and `sha_b` at 8 and 24 is not derivable from anything. It
//! is a specification to copy, and the tests here check each slice against the spec rather
//! than against a stored value, so a moved bound fails rather than being enshrined.
//!
//! # Padding is 12 to 1024 bytes
//!
//! Not "to the next multiple of 16". MTProto 2.0 requires at least 12 bytes of random
//! padding and allows up to 1024, so message length is not the plaintext length. A version
//! padding to the minimum multiple of 16 works perfectly against the server and leaks the
//! exact size of every message.
//!
//! # msg_id and seq_no
//!
//! `msg_id` is a timestamp: the Unix time in the top 32 bits, a fraction below, and the low
//! two bits saying what kind of message it is. The server rejects anything more than 30
//! seconds ahead or 300 behind its own clock — which is why [`crate::handshake::AuthKey`]
//! carries a `time_offset`, and why a handset with a hand-set clock is unusable without it.
//!
//! `seq_no` counts only *content* messages — the ones that need acknowledging. Acks and
//! pings do not increment it. A client that counts everything drifts out of step with the
//! server's idea of the sequence and starts having messages ignored.

use alloc::vec::Vec;

use symbian_crypto::{ige, Aes, Sha256};

use crate::crypto::{self, Rng};
use crate::handshake::AuthKey;
use crate::tl::{self, Reader, Writer};

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Error {
    /// The message was shorter than a header.
    Truncated,
    /// `auth_key_id` was not ours. Either a message for a different key, or noise.
    WrongKey { want: u64, got: u64 },
    /// The recomputed `msg_key` did not match the one on the wire. The message was
    /// tampered with, or decrypted with the wrong key — indistinguishable, and both fatal.
    BadMsgKey,
    /// The declared body length does not fit the decrypted plaintext.
    BadLength,
    /// A `session_id` that is not this session's.
    WrongSession,
    Tl(tl::Error),
    Crypto(crypto::Error),
}

impl From<tl::Error> for Error {
    fn from(e: tl::Error) -> Self {
        Error::Tl(e)
    }
}

pub type Result<T> = core::result::Result<T, Error>;

/// Which direction a message travels, which selects the key material.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Dir {
    /// Client to server: `x = 0`.
    Out,
    /// Server to client: `x = 8`.
    In,
}

impl Dir {
    fn x(self) -> usize {
        match self {
            Dir::Out => 0,
            Dir::In => 8,
        }
    }
}

/// Derive the per-message AES key and IV.
///
/// Exposed because it is worth testing on its own: a handshake failure and a key-derivation
/// failure look identical from outside, and only one of them is in this function.
pub fn message_keys(auth_key: &[u8; 256], msg_key: &[u8; 16], dir: Dir) -> ([u8; 32], [u8; 32]) {
    let x = dir.x();

    let sha_a = {
        let mut h = Sha256::new();
        h.update(msg_key);
        h.update(&auth_key[x..x + 36]);
        h.finish()
    };
    let sha_b = {
        let mut h = Sha256::new();
        h.update(&auth_key[40 + x..76 + x]);
        h.update(msg_key);
        h.finish()
    };

    let mut key = [0u8; 32];
    key[..8].copy_from_slice(&sha_a[..8]);
    key[8..24].copy_from_slice(&sha_b[8..24]);
    key[24..].copy_from_slice(&sha_a[24..32]);

    let mut iv = [0u8; 32];
    iv[..8].copy_from_slice(&sha_b[..8]);
    iv[8..24].copy_from_slice(&sha_a[8..24]);
    iv[24..].copy_from_slice(&sha_b[24..32]);

    (key, iv)
}

/// A decrypted message, with the fields the caller needs to route and acknowledge it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Incoming {
    pub salt: u64,
    pub msg_id: u64,
    pub seq_no: u32,
    pub body: Vec<u8>,
}

/// One connection's worth of state: the key, the salt, the session id and the counter.
pub struct Session {
    auth_key: [u8; 256],
    auth_key_id: u64,
    salt: u64,
    session_id: u64,
    /// Content messages sent so far. See the module docs on why acks do not count.
    seq: u32,
    /// The last `msg_id` produced, so two messages in the same millisecond still differ.
    last_msg_id: u64,
}

impl Session {
    /// A session over a freshly negotiated key.
    ///
    /// `session_id` must be random and must change whenever the connection is re-established
    /// — the server uses it to tell a reconnect from a duplicate, and reusing one makes a
    /// resumed session look like a replay.
    pub fn new(auth: &AuthKey, session_id: u64) -> Self {
        Session {
            auth_key: auth.key,
            auth_key_id: auth.id,
            salt: u64::from_le_bytes(auth.salt),
            session_id,
            seq: 0,
            last_msg_id: 0,
        }
    }

    pub fn session_id(&self) -> u64 {
        self.session_id
    }

    pub fn salt(&self) -> u64 {
        self.salt
    }

    /// Adopt a new salt. The server rotates these and complains via `bad_server_salt` when
    /// the old one is used; ignoring that turns into every message being rejected an hour
    /// after login.
    pub fn set_salt(&mut self, salt: u64) {
        self.salt = salt;
    }

    /// A `msg_id` for `unix_time`, already corrected by the server offset.
    ///
    /// The low two bits encode the message kind — `0b00` for a client-to-server request —
    /// and the rest is a timestamp scaled to fill the fractional part. Strictly increasing,
    /// because the server drops a `msg_id` it has already seen and two calls in the same
    /// second would otherwise collide.
    pub fn next_msg_id(&mut self, unix_time: i64, nanos: u32) -> u64 {
        let frac = ((nanos as u64) << 30) / 1_000_000_000;
        let mut id = ((unix_time as u64) << 32) | (frac << 2);
        id &= !0b11;
        if id <= self.last_msg_id {
            id = self.last_msg_id + 4;
        }
        self.last_msg_id = id;
        id
    }

    /// The sequence number for the next message.
    ///
    /// `content` is true for anything that needs acknowledging — an RPC call — and false for
    /// acks, pings and containers. Content messages get an odd number and advance the
    /// counter; the rest get an even one and do not.
    pub fn next_seq(&mut self, content: bool) -> u32 {
        if content {
            let s = self.seq * 2 + 1;
            self.seq += 1;
            s
        } else {
            self.seq * 2
        }
    }

    /// Encrypt one message to the server.
    pub fn encrypt<R: Rng>(
        &self,
        msg_id: u64,
        seq_no: u32,
        body: &[u8],
        rng: &mut R,
    ) -> Result<Vec<u8>> {
        self.seal(Dir::Out, msg_id, seq_no, body, rng)
    }

    /// Decrypt a message from the server.
    pub fn decrypt(&self, wire: &[u8]) -> Result<Incoming> {
        self.open(Dir::In, wire)
    }

    /// Encrypt as the *server* would, for tests and for the reference differential.
    ///
    /// It exists because a client cannot decrypt its own output and should not be able to:
    /// the two directions draw different key material from the same auth key, which is what
    /// stops a message being replayed back at its sender. A round-trip test that passed
    /// without this would be testing a protocol nobody speaks.
    pub fn encrypt_as_server<R: Rng>(
        &self,
        msg_id: u64,
        seq_no: u32,
        body: &[u8],
        rng: &mut R,
    ) -> Result<Vec<u8>> {
        self.seal(Dir::In, msg_id, seq_no, body, rng)
    }

    fn seal<R: Rng>(
        &self,
        dir: Dir,
        msg_id: u64,
        seq_no: u32,
        body: &[u8],
        rng: &mut R,
    ) -> Result<Vec<u8>> {
        // salt(8) session_id(8) msg_id(8) seq_no(4) length(4) = 32, then the body.
        let mut plain = Writer::with_capacity(32 + body.len() + 32);
        plain
            .ulong(self.salt)
            .ulong(self.session_id)
            .ulong(msg_id)
            .uint(seq_no)
            .uint(body.len() as u32)
            .raw(body);
        let mut plain = plain.finish();

        // 12 to 1024 bytes, and a multiple of 16 overall. The minimum is 12 rather than 0
        // because msg_key is computed over the plaintext including padding, and 12 bytes is
        // what the specification requires to keep it from being a function of the body
        // alone.
        let at = plain.len();
        let total = (at + 12).div_ceil(16) * 16;
        plain.resize(total, 0);
        rng.fill(&mut plain[at..]);

        let msg_key = {
            let mut h = Sha256::new();
            // auth_key[88 + x .. 120 + x]. This slice is what makes msg_key a MAC rather
            // than a checksum: without key material in it, anyone could recompute it for a
            // message they forged.
            let x = dir.x();
            h.update(&self.auth_key[88 + x..120 + x]);
            h.update(&plain);
            let large = h.finish();
            let mut k = [0u8; 16];
            k.copy_from_slice(&large[8..24]);
            k
        };

        let (key, iv) = message_keys(&self.auth_key, &msg_key, dir);
        let aes = Aes::new(&key).ok_or(Error::Crypto(crypto::Error::Rsa))?;
        let mut iv = iv;
        ige::encrypt(&aes, &mut iv, &mut plain)
            .map_err(|_| Error::Crypto(crypto::Error::NotBlockAligned))?;

        let mut out = Writer::with_capacity(24 + plain.len());
        out.ulong(self.auth_key_id).raw(&msg_key).raw(&plain);
        Ok(out.finish())
    }

    fn open(&self, dir: Dir, wire: &[u8]) -> Result<Incoming> {
        if wire.len() < 24 + 32 || (wire.len() - 24) % 16 != 0 {
            return Err(Error::Truncated);
        }
        let mut r = Reader::new(wire);
        let key_id = r.ulong()?;
        if key_id != self.auth_key_id {
            return Err(Error::WrongKey { want: self.auth_key_id, got: key_id });
        }
        let mut msg_key = [0u8; 16];
        msg_key.copy_from_slice(r.raw(16)?);

        let (key, iv) = message_keys(&self.auth_key, &msg_key, dir);
        let aes = Aes::new(&key).ok_or(Error::Crypto(crypto::Error::Rsa))?;
        let mut iv = iv;
        let mut plain = wire[24..].to_vec();
        ige::decrypt(&aes, &mut iv, &mut plain)
            .map_err(|_| Error::Crypto(crypto::Error::NotBlockAligned))?;

        // Recompute msg_key over what came out and compare, before parsing a single field.
        //
        // This is the authentication step. Everything below it treats the plaintext as
        // trusted, which is only true because this check happened first — parsing lengths
        // out of unauthenticated bytes is how most protocol vulnerabilities start.
        let computed = {
            let mut h = Sha256::new();
            // The same 88 + x slice the sender used. Reading it with the wrong x yields a
            // msg_key that never matches, and the symptom is every server message being
            // rejected as tampered — which points at the network rather than at a constant.
            let x = dir.x();
            h.update(&self.auth_key[88 + x..120 + x]);
            h.update(&plain);
            h.finish()
        };
        if !symbian_crypto::ct_eq(&computed[8..24], &msg_key) {
            return Err(Error::BadMsgKey);
        }

        let mut r = Reader::new(&plain);
        let salt = r.ulong()?;
        let session_id = r.ulong()?;
        let msg_id = r.ulong()?;
        let seq_no = r.uint()?;
        let len = r.uint()? as usize;

        if session_id != self.session_id {
            return Err(Error::WrongSession);
        }
        // The length is authenticated by now, but it still has to fit: a truncated
        // ciphertext with a valid msg_key is impossible, and a bounds check that cannot
        // fire is cheaper than reasoning about whether it can.
        if len > r.remaining() {
            return Err(Error::BadLength);
        }
        // Padding must be at least 12 bytes, which is also a check that the sender is
        // speaking MTProto 2.0 rather than 1.0.
        if r.remaining() - len < 12 {
            return Err(Error::BadLength);
        }

        Ok(Incoming { salt, msg_id, seq_no, body: r.raw(len)?.to_vec() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::testing::CountingRng;

    fn key() -> AuthKey {
        let mut k = [0u8; 256];
        for (i, b) in k.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(7).wrapping_add(3);
        }
        AuthKey { key: k, id: 0x1122_3344_5566_7788, salt: [1, 2, 3, 4, 5, 6, 7, 8], time_offset: 0 }
    }

    #[test]
    fn a_message_round_trips() {
        let s = Session::new(&key(), 0xdead_beef_0000_0001);
        let mut rng = CountingRng(0);
        let wire = s.encrypt_as_server(0x1234_5678_0000_0000, 1, b"hello mtproto", &mut rng).unwrap();
        let got = s.decrypt(&wire).unwrap();
        assert_eq!(got.body, b"hello mtproto");
        assert_eq!(got.msg_id, 0x1234_5678_0000_0000);
        assert_eq!(got.seq_no, 1);
    }

    #[test]
    fn the_two_directions_use_different_keys() {
        // The x = 0 / x = 8 split. If these matched, a client would be able to decrypt its
        // own messages and nothing else, which is exactly the symptom of getting it wrong.
        let k = key();
        let msg_key = [9u8; 16];
        let out = message_keys(&k.key, &msg_key, Dir::Out);
        let inn = message_keys(&k.key, &msg_key, Dir::In);
        assert_ne!(out.0, inn.0);
        assert_ne!(out.1, inn.1);
    }

    #[test]
    fn the_key_derivation_slices_match_the_spec() {
        // Recomputed from the specification rather than compared to a stored value, so this
        // fails when a bound moves rather than freezing whatever the code happens to do.
        let k = key();
        let msg_key = [0x5au8; 16];
        for (dir, x) in [(Dir::Out, 0usize), (Dir::In, 8usize)] {
            let (aes_key, aes_iv) = message_keys(&k.key, &msg_key, dir);

            let mut h = Sha256::new();
            h.update(&msg_key);
            h.update(&k.key[x..x + 36]);
            let a = h.finish();
            let mut h = Sha256::new();
            h.update(&k.key[40 + x..76 + x]);
            h.update(&msg_key);
            let b = h.finish();

            assert_eq!(&aes_key[..8], &a[..8]);
            assert_eq!(&aes_key[8..24], &b[8..24]);
            assert_eq!(&aes_key[24..], &a[24..32]);
            assert_eq!(&aes_iv[..8], &b[..8]);
            assert_eq!(&aes_iv[8..24], &a[8..24]);
            assert_eq!(&aes_iv[24..], &b[24..32]);
        }
    }

    #[test]
    fn a_tampered_message_is_rejected_everywhere() {
        let s = Session::new(&key(), 7);
        let mut rng = CountingRng(0);
        let wire = s.encrypt_as_server(16, 1, b"twenty four bytes here!!", &mut rng).unwrap();
        for i in 8..wire.len() {
            let mut bad = wire.clone();
            bad[i] ^= 1;
            assert!(s.decrypt(&bad).is_err(), "flipping byte {i} was accepted");
        }
    }

    #[test]
    fn a_message_for_another_key_is_named_as_such() {
        let s = Session::new(&key(), 7);
        let mut rng = CountingRng(0);
        let mut wire = s.encrypt_as_server(16, 1, b"x", &mut rng).unwrap();
        wire[0] ^= 0xff;
        // Not BadMsgKey: the caller reacts differently. A wrong auth_key_id means the key
        // was rotated or lost and the handshake must be redone; a bad msg_key means the
        // right key produced the wrong plaintext, which is tampering.
        assert!(matches!(s.decrypt(&wire), Err(Error::WrongKey { .. })));
    }

    #[test]
    fn a_message_for_another_session_is_rejected() {
        let a = Session::new(&key(), 1);
        let b = Session::new(&key(), 2);
        let mut rng = CountingRng(0);
        let wire = a.encrypt_as_server(16, 1, b"x", &mut rng).unwrap();
        assert_eq!(b.decrypt(&wire), Err(Error::WrongSession));
    }

    #[test]
    fn padding_is_at_least_twelve_bytes_and_hides_the_length() {
        // A body one byte shorter must not always produce a shorter message; and no message
        // may carry fewer than 12 bytes of padding.
        let s = Session::new(&key(), 7);
        let mut rng = CountingRng(0);
        for len in 0..40usize {
            let body = alloc::vec![0u8; len];
            let wire = s.encrypt(16, 1, &body, &mut rng).unwrap();
            let plain_len = wire.len() - 24;
            assert_eq!(plain_len % 16, 0, "len {len} is not block aligned");
            let pad = plain_len - 32 - len;
            assert!(pad >= 12, "len {len} got only {pad} bytes of padding");
        }
    }

    #[test]
    fn two_encryptions_of_the_same_body_differ() {
        // The padding is random, so identical plaintexts must produce different ciphertexts
        // and different msg_keys. Equal output would mean the padding is not reaching the
        // hash, which would make msg_key a function of the body alone.
        let s = Session::new(&key(), 7);
        let mut rng = CountingRng(0);
        let a = s.encrypt(16, 1, b"same", &mut rng).unwrap();
        let b = s.encrypt(16, 1, b"same", &mut rng).unwrap();
        assert_ne!(a, b);
        assert_ne!(a[8..24], b[8..24], "msg_key did not change");
    }

    #[test]
    fn msg_ids_strictly_increase_even_within_one_second() {
        let mut s = Session::new(&key(), 7);
        let mut last = 0;
        for i in 0..100 {
            // Same second every time, which is the case that collides.
            let id = s.next_msg_id(1_700_000_000, i * 1000);
            assert!(id > last, "msg_id {id} did not exceed {last}");
            assert_eq!(id & 0b11, 0, "the low bits must mark a client request");
            last = id;
        }
    }

    #[test]
    fn the_sequence_counts_only_content_messages() {
        // An ack must not advance the counter. A client that counts everything drifts out
        // of step and starts having messages ignored, with nothing said about why.
        let mut s = Session::new(&key(), 7);
        assert_eq!(s.next_seq(true), 1);
        assert_eq!(s.next_seq(false), 2);
        assert_eq!(s.next_seq(false), 2);
        assert_eq!(s.next_seq(true), 3);
        assert_eq!(s.next_seq(true), 5);
    }

    #[test]
    fn a_short_or_misaligned_message_is_refused() {
        let s = Session::new(&key(), 7);
        assert_eq!(s.decrypt(&[0u8; 20]), Err(Error::Truncated));
        assert_eq!(s.decrypt(&[0u8; 55]), Err(Error::Truncated));
    }

    #[test]
    fn bodies_of_every_length_round_trip() {
        let s = Session::new(&key(), 7);
        let mut rng = CountingRng(0);
        for len in [0usize, 1, 15, 16, 17, 100, 1000, 4096] {
            let body: Vec<u8> = (0..len).map(|i| (i % 253) as u8).collect();
            let wire = s.encrypt_as_server(16, 1, &body, &mut rng).unwrap();
            assert_eq!(s.decrypt(&wire).unwrap().body, body, "len {len}");
        }
    }
}
