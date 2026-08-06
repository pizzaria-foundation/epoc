//! The auth-key handshake: from an open socket to a 2048-bit shared key.
//!
//! ```text
//!   client                                              server
//!     |--- req_pq_multi(nonce) ------------------------->|
//!     |<-- resPQ(nonce, server_nonce, pq, fingerprints) --|
//!     |    factor pq into p·q                             |
//!     |--- req_DH_params(p, q, RSA(p_q_inner_data)) ----->|
//!     |<-- server_DH_params_ok(IGE(server_DH_inner_data))-|
//!     |    b ← random; g_b = g^b mod p          [815 ms]  |
//!     |--- set_client_DH_params(IGE(g_b)) -------------->|
//!     |<-- dh_gen_ok(new_nonce_hash1) -------------------|
//!     |    auth_key = g_a^b mod p               [815 ms]  |
//! ```
//!
//! # No I/O, and no clock
//!
//! [`Handshake`] consumes message payloads and produces [`Action`]s. It never sends,
//! never blocks and never asks the time. The two exponentiations come back as
//! [`Action::ModPow`] for the caller to run — on the E72 that means the worker thread,
//! because 815 ms on the GUI thread freezes the window server and there is no watchdog that
//! recovers from it.
//!
//! # What is validated, and what that buys
//!
//! | check | what it stops |
//! |---|---|
//! | `nonce` echoed in every reply | a reply from a different handshake being spliced in |
//! | `server_nonce` echoed | the same, in the other direction |
//! | RSA fingerprint is one we hold | talking to something that is not Telegram |
//! | `dh_prime` is the known prime | a chosen prime with small factors, making DH breakable |
//! | `1 < g_a < p-1` | the degenerate values that force the shared key to 1 |
//! | `new_nonce_hash1` | a server that never had the key agreeing that it does |
//!
//! The `dh_prime` check is a hash comparison against a constant rather than a primality
//! test. Verifying that a 2048-bit number is a safe prime means Miller-Rabin on it and on
//! `(p-1)/2`, which is minutes on this hardware — long enough that no login would complete.
//! Telegram's prime has been constant for years; if it changes, this fails loudly with
//! [`Error::UnknownDhPrime`] rather than quietly accepting whatever arrived, which is the
//! right way round.

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;

use symbian_crypto::{Sha1, Sha256};

use crate::crypto::{self, Rng};
use crate::keys;
use crate::pq;
use crate::tl::{self, Reader, Writer};

// -------------------------------------------------------------- constructor ids --

/* From vendor/research/mtproto/mtproto.tl, quoted so each can be checked against it. */

/// `req_pq_multi#be7e8ef1 nonce:int128 = ResPQ;`
const REQ_PQ_MULTI: u32 = 0xbe7e_8ef1;
/// `resPQ#05162463 nonce:int128 server_nonce:int128 pq:bytes server_public_key_fingerprints:Vector<long> = ResPQ;`
const RES_PQ: u32 = 0x0516_2463;
/// `p_q_inner_data#83c95aec pq:bytes p:bytes q:bytes nonce:int128 server_nonce:int128 new_nonce:int256 = P_Q_inner_data;`
const P_Q_INNER_DATA: u32 = 0x83c9_5aec;
/// `req_DH_params#d712e4be nonce:int128 server_nonce:int128 p:bytes q:bytes public_key_fingerprint:long encrypted_data:bytes = Server_DH_Params;`
const REQ_DH_PARAMS: u32 = 0xd712_e4be;
/// `server_DH_params_ok#d0e8075c nonce:int128 server_nonce:int128 encrypted_answer:bytes = Server_DH_Params;`
const SERVER_DH_PARAMS_OK: u32 = 0xd0e8_075c;
/// `server_DH_params_fail#79cb045d nonce:int128 server_nonce:int128 new_nonce_hash:int128 = Server_DH_Params;`
const SERVER_DH_PARAMS_FAIL: u32 = 0x79cb_045d;
/// `server_DH_inner_data#b5890dba nonce:int128 server_nonce:int128 g:int dh_prime:bytes g_a:bytes server_time:int = Server_DH_inner_data;`
const SERVER_DH_INNER_DATA: u32 = 0xb589_0dba;
/// `client_DH_inner_data#6643b654 nonce:int128 server_nonce:int128 retry_id:long g_b:bytes = Client_DH_Inner_Data;`
const CLIENT_DH_INNER_DATA: u32 = 0x6643_b654;
/// `set_client_DH_params#f5045f1f nonce:int128 server_nonce:int128 encrypted_data:bytes = Set_client_DH_params_answer;`
const SET_CLIENT_DH_PARAMS: u32 = 0xf504_5f1f;
/// `dh_gen_ok#3bcbf734 nonce:int128 server_nonce:int128 new_nonce_hash1:int128 = Set_client_DH_params_answer;`
const DH_GEN_OK: u32 = 0x3bcb_f734;
/// `dh_gen_retry#46dc1fb9 ...`
const DH_GEN_RETRY: u32 = 0x46dc_1fb9;
/// `dh_gen_fail#a69dae02 ...`
const DH_GEN_FAIL: u32 = 0xa69d_ae02;

/// `SHA-256` of Telegram's Diffie-Hellman prime.
///
/// The prime itself is 256 bytes and arrives from the server every time, so only the hash
/// needs carrying. Computed in `vendor/research/mtproto` from the published value.
pub(crate) const DH_PRIME_SHA256: [u8; 32] = [
    0x02, 0xf8, 0x5e, 0x76, 0x87, 0xfc, 0x6f, 0x33, 0xba, 0x67, 0x82, 0x26, 0xa9, 0x63, 0xb3,
    0xc8, 0xa1, 0x91, 0xb4, 0x7c, 0x89, 0x0c, 0xf3, 0x0d, 0xeb, 0xe1, 0x7c, 0x1d, 0x62, 0x3b,
    0x5a, 0xf1,
];

// ------------------------------------------------------------------------ errors --

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Error {
    /// The payload could not be parsed as TL.
    Tl(tl::Error),
    /// A cryptographic step failed.
    Crypto(crypto::Error),
    /// A reply arrived in a state that does not expect one.
    OutOfOrder,
    /// A reply carried a nonce that is not the one this handshake sent. Either a stale
    /// message from a previous attempt, or someone splicing.
    NonceMismatch,
    /// `pq` could not be factored, or was not a semiprime.
    NotFactorable(u64),
    /// The server offered no RSA key this build holds. See [`crate::keys`] on rotation.
    NoUsableKey,
    /// `server_DH_params_fail` — the server rejected the inner data.
    ServerRejected,
    /// The `dh_prime` was not Telegram's. Refused rather than used.
    UnknownDhPrime,
    /// `g` or `g_a` outside the range that makes Diffie-Hellman meaningful.
    BadDhParams,
    /// `dh_gen_retry` or `dh_gen_fail`.
    DhGenFailed,
    /// The server's `new_nonce_hash1` did not match the key we derived, so the two sides do
    /// not hold the same key and the server cannot prove it holds one at all.
    KeyMismatch,
}

impl From<tl::Error> for Error {
    fn from(e: tl::Error) -> Self {
        Error::Tl(e)
    }
}

impl From<crypto::Error> for Error {
    fn from(e: crypto::Error) -> Self {
        Error::Crypto(e)
    }
}

pub type Result<T> = core::result::Result<T, Error>;

// ------------------------------------------------------------------------ output --

/// What the caller must do next.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Action {
    /// Frame and send this payload, then wait for a reply.
    Send(Vec<u8>),
    /// Compute `base^exp mod modulus`, all big-endian, and feed the 256-byte result back
    /// through [`Handshake::on_modpow`].
    ///
    /// Not done here because it takes 815 ms on this hardware and this crate has no way to
    /// get off the calling thread. The caller owns that decision — on the E72 it means
    /// `shim_work_submit`.
    ModPow { base: Vec<u8>, exp: Vec<u8>, modulus: Vec<u8> },
    /// Finished.
    Done(Box<AuthKey>),
}

/// The result: a key both sides hold and neither sent.
///
/// `PartialEq` is derived and is **not** constant time. That is fine for what it is used
/// for — tests, and asking whether a stored key is the one just negotiated — and would not
/// be if it were ever used to check a key against attacker-supplied bytes. Nothing does;
/// `symbian_crypto::ct_eq` is there if something ever needs to.
#[derive(Clone, PartialEq, Eq)]
pub struct AuthKey {
    /// 2048 bits, big-endian. Everything after this point is encrypted with it.
    pub key: [u8; 256],
    /// `SHA1(key)[12..20]` little-endian. Prefixes every encrypted message so the server
    /// knows which key to try.
    pub id: u64,
    /// The initial salt, from `new_nonce[0..8] XOR server_nonce[0..8]`. Rotates during a
    /// session; this is only the first one.
    pub salt: [u8; 8],
    /// The server's Unix time, exactly as `server_DH_inner_data` reported it.
    ///
    /// **Absolute, not an offset.** This module has no clock, so it cannot subtract one —
    /// and an earlier version stored this in a field called `time_offset`, which produced
    /// `msg_id`s 56 years in the future and a `bad_msg_notification` with code 16. The live
    /// run against Telegram found that in one line; nothing offline could have, because
    /// every test that had a clock had the same wrong one on both sides.
    ///
    /// The subtraction happens in [`crate::client::Client`], which is given a local time.
    /// It matters because MTProto rejects a `msg_id` more than 30 s ahead or 300 s behind
    /// the server, and a phone's clock is set by hand.
    pub server_time: i32,
}

impl core::fmt::Debug for AuthKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // The key itself is deliberately not printed. A log line is a file, and a file with
        // an auth key in it is the whole account.
        f.debug_struct("AuthKey")
            .field("id", &self.id)
            .field("server_time", &self.server_time)
            .finish_non_exhaustive()
    }
}

// ------------------------------------------------------------------- the machine --

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum State {
    AwaitResPq,
    AwaitDhParams,
    AwaitGb,
    AwaitDhGen,
    AwaitAuthKey,
    Finished,
}

pub struct Handshake {
    state: State,
    nonce: [u8; 16],
    server_nonce: [u8; 16],
    new_nonce: [u8; 32],
    /// The Diffie-Hellman secret. 2048 bits, generated once and never sent.
    b: [u8; 256],
    dh_prime: Vec<u8>,
    g_a: Vec<u8>,
    /// The AES key and IV protecting the DH exchange, derived once `new_nonce` and
    /// `server_nonce` are both known.
    aes: ([u8; 32], [u8; 32]),
    server_time: i32,
    /// `new_nonce_hash1` from `dh_gen_ok`, checked once the key exists.
    expected_hash: [u8; 16],
}

impl Handshake {
    /// Begin. Returns the machine and the first message to send.
    pub fn start<R: Rng>(rng: &mut R) -> (Self, Action) {
        let mut nonce = [0u8; 16];
        rng.fill(&mut nonce);
        let mut new_nonce = [0u8; 32];
        rng.fill(&mut new_nonce);
        let mut b = [0u8; 256];
        rng.fill(&mut b);
        // The top bit set makes b a full 2048-bit exponent. The ladder in `bignum::modpow`
        // runs once per bit of the buffer regardless, so this costs nothing and removes any
        // chance of a short secret from an unlucky draw.
        b[0] |= 0x80;

        let h = Handshake {
            state: State::AwaitResPq,
            nonce,
            server_nonce: [0u8; 16],
            new_nonce,
            b,
            dh_prime: Vec::new(),
            g_a: Vec::new(),
            aes: ([0u8; 32], [0u8; 32]),
            server_time: 0,
            expected_hash: [0u8; 16],
        };

        let mut w = Writer::with_capacity(20);
        w.ctor(REQ_PQ_MULTI).raw(&nonce);
        (h, Action::Send(w.finish()))
    }

    /// The client nonce, for tests and for logging that a handshake is in flight.
    pub fn nonce(&self) -> [u8; 16] {
        self.nonce
    }

    /// Feed an unencrypted message payload from the server.
    pub fn on_message<R: Rng>(&mut self, payload: &[u8], rng: &mut R) -> Result<Action> {
        // Handshake messages carry a zero auth_key_id, a msg_id and a length, then the TL
        // body. The header is skipped rather than validated: the nonces inside are what
        // bind a reply to this handshake, and they are checked. A msg_id check here would
        // add a clock dependency to a machine that deliberately has none.
        let body = strip_unencrypted_header(payload)?;
        match self.state {
            State::AwaitResPq => self.on_res_pq(body, rng),
            State::AwaitDhParams => self.on_dh_params(body, rng),
            State::AwaitDhGen => self.on_dh_gen(body),
            _ => Err(Error::OutOfOrder),
        }
    }

    /// Feed back the result of an [`Action::ModPow`].
    pub fn on_modpow<R: Rng>(&mut self, result: &[u8], rng: &mut R) -> Result<Action> {
        match self.state {
            State::AwaitGb => self.on_g_b(result, rng),
            State::AwaitAuthKey => self.on_auth_key(result),
            _ => Err(Error::OutOfOrder),
        }
    }

    // ---- step 1: resPQ -> req_DH_params ----

    fn on_res_pq<R: Rng>(&mut self, body: &[u8], rng: &mut R) -> Result<Action> {
        let mut r = Reader::new(body);
        r.expect(RES_PQ)?;
        if r.int128()? != self.nonce {
            return Err(Error::NonceMismatch);
        }
        self.server_nonce = r.int128()?;
        let pq_bytes = r.bytes()?;
        let fingerprints = r.vector_long()?;

        // pq is at most 8 bytes, big-endian, and may be shorter with no leading zeros.
        if pq_bytes.is_empty() || pq_bytes.len() > 8 {
            return Err(Error::NotFactorable(0));
        }
        let mut wide = [0u8; 8];
        wide[8 - pq_bytes.len()..].copy_from_slice(pq_bytes);
        let n = u64::from_be_bytes(wide);
        let (p, q) = pq::factor_retry(n).ok_or(Error::NotFactorable(n))?;

        let fp = keys::select(&fingerprints).ok_or(Error::NoUsableKey)?;

        // The AES key for the DH exchange is fixed from here: both nonces are known.
        self.aes = crypto::dh_kdf(&self.new_nonce, &self.server_nonce);

        let inner = {
            let mut w = Writer::with_capacity(160);
            w.ctor(P_Q_INNER_DATA)
                .bytes(pq_bytes)
                .bytes(&trim_be(&p.to_be_bytes()))
                .bytes(&trim_be(&q.to_be_bytes()))
                .raw(&self.nonce)
                .raw(&self.server_nonce)
                .raw(&self.new_nonce);
            w.finish()
        };

        let encrypted =
            crypto::rsa_pad(&inner, &keys::MODULUS, &keys::EXPONENT, rng, 32)?;

        let mut w = Writer::with_capacity(360);
        w.ctor(REQ_DH_PARAMS)
            .raw(&self.nonce)
            .raw(&self.server_nonce)
            .bytes(&trim_be(&p.to_be_bytes()))
            .bytes(&trim_be(&q.to_be_bytes()))
            .ulong(fp)
            .bytes(&encrypted);

        self.state = State::AwaitDhParams;
        Ok(Action::Send(w.finish()))
    }

    // ---- step 2: server_DH_params_ok -> compute g^b ----

    fn on_dh_params<R: Rng>(&mut self, body: &[u8], _rng: &mut R) -> Result<Action> {
        let mut r = Reader::new(body);
        match r.ctor()? {
            SERVER_DH_PARAMS_OK => {}
            SERVER_DH_PARAMS_FAIL => return Err(Error::ServerRejected),
            other => return Err(Error::Tl(tl::Error::UnknownConstructor(other))),
        }
        if r.int128()? != self.nonce || r.int128()? != self.server_nonce {
            return Err(Error::NonceMismatch);
        }
        let encrypted = r.bytes()?;

        let answer = crypto::ige_check_hash(&self.aes.0, &self.aes.1, encrypted)?;

        let mut r = Reader::new(&answer);
        r.expect(SERVER_DH_INNER_DATA)?;
        if r.int128()? != self.nonce || r.int128()? != self.server_nonce {
            return Err(Error::NonceMismatch);
        }
        let g = r.int()?;
        let dh_prime = r.bytes()?;
        let g_a = r.bytes()?;
        let server_time = r.int()?;

        check_dh(g, dh_prime, g_a)?;

        self.dh_prime = dh_prime.to_vec();
        self.g_a = g_a.to_vec();
        self.server_time = server_time;

        self.state = State::AwaitGb;
        Ok(Action::ModPow {
            base: vec![g as u8],
            exp: self.b.to_vec(),
            modulus: self.dh_prime.clone(),
        })
    }

    // ---- step 3: g^b -> set_client_DH_params ----

    fn on_g_b<R: Rng>(&mut self, g_b: &[u8], rng: &mut R) -> Result<Action> {
        let inner = {
            let mut w = Writer::with_capacity(320);
            w.ctor(CLIENT_DH_INNER_DATA)
                .raw(&self.nonce)
                .raw(&self.server_nonce)
                // retry_id is zero on the first attempt. Retries are not implemented: a
                // dh_gen_retry means the server saw a different key than we derived, which
                // in practice means a bug rather than a transient, and restarting the whole
                // handshake is both simpler and more likely to work.
                .ulong(0)
                .bytes(&trim_be(g_b));
            w.finish()
        };

        let encrypted = crypto::ige_with_hash(&self.aes.0, &self.aes.1, &inner, rng)?;

        let mut w = Writer::with_capacity(400);
        w.ctor(SET_CLIENT_DH_PARAMS)
            .raw(&self.nonce)
            .raw(&self.server_nonce)
            .bytes(&encrypted);

        self.state = State::AwaitDhGen;
        Ok(Action::Send(w.finish()))
    }

    // ---- step 4: dh_gen_ok -> compute g_a^b ----

    fn on_dh_gen(&mut self, body: &[u8]) -> Result<Action> {
        let mut r = Reader::new(body);
        match r.ctor()? {
            DH_GEN_OK => {}
            DH_GEN_RETRY | DH_GEN_FAIL => return Err(Error::DhGenFailed),
            other => return Err(Error::Tl(tl::Error::UnknownConstructor(other))),
        }
        if r.int128()? != self.nonce || r.int128()? != self.server_nonce {
            return Err(Error::NonceMismatch);
        }
        self.expected_hash = r.int128()?;

        self.state = State::AwaitAuthKey;
        Ok(Action::ModPow {
            base: self.g_a.clone(),
            exp: self.b.to_vec(),
            modulus: self.dh_prime.clone(),
        })
    }

    // ---- step 5: g_a^b is the key ----

    fn on_auth_key(&mut self, key: &[u8]) -> Result<Action> {
        // Left-padded to the full width. modpow returns the number, and a result with
        // leading zeros is shorter — but auth_key is defined as 256 bytes and its SHA-1
        // covers all of them, so a short one hashes to something the server never computed.
        // This happens once in 256 handshakes and would look like an intermittent server
        // fault.
        let mut auth_key = [0u8; 256];
        if key.len() > 256 {
            return Err(Error::BadDhParams);
        }
        auth_key[256 - key.len()..].copy_from_slice(key);

        let digest = {
            let mut h = Sha1::new();
            h.update(&auth_key);
            h.finish()
        };
        let mut id_bytes = [0u8; 8];
        id_bytes.copy_from_slice(&digest[12..20]);
        let id = u64::from_le_bytes(id_bytes);

        // new_nonce_hash1 = SHA1(new_nonce ++ [1] ++ SHA1(auth_key)[0..8])[4..20]
        //
        // This is the only proof that the server derived the same key. Skipping it means a
        // handshake that "succeeds" against anything that echoes nonces, and the failure
        // then appears as every subsequent message being rejected.
        let computed = {
            let mut h = Sha1::new();
            h.update(&self.new_nonce);
            h.update(&[1u8]);
            h.update(&digest[..8]);
            h.finish()
        };
        if computed[4..20] != self.expected_hash {
            return Err(Error::KeyMismatch);
        }

        let mut salt = [0u8; 8];
        for i in 0..8 {
            salt[i] = self.new_nonce[i] ^ self.server_nonce[i];
        }

        self.state = State::Finished;
        Ok(Action::Done(Box::new(AuthKey {
            key: auth_key,
            id,
            salt,
            server_time: self.server_time,
        })))
    }
}

// ---------------------------------------------------------------------- helpers --

/// Strip the 20-byte unencrypted header: `auth_key_id:long msg_id:long length:int`.
fn strip_unencrypted_header(payload: &[u8]) -> Result<&[u8]> {
    if payload.len() < 20 {
        return Err(Error::Tl(tl::Error::Truncated));
    }
    let len = u32::from_le_bytes([payload[16], payload[17], payload[18], payload[19]]) as usize;
    if 20 + len > payload.len() {
        return Err(Error::Tl(tl::Error::Truncated));
    }
    Ok(&payload[20..20 + len])
}

/// Wrap a body in that header for sending. `msg_id` is the caller's, since this crate has
/// no clock.
pub fn unencrypted(msg_id: u64, body: &[u8]) -> Vec<u8> {
    let mut w = Writer::with_capacity(20 + body.len());
    w.ulong(0) // auth_key_id: zero means unencrypted
        .ulong(msg_id)
        .uint(body.len() as u32)
        .raw(body);
    w.finish()
}

/// Drop leading zero bytes from a big-endian number, keeping at least one.
///
/// TL numbers are minimal-width. Sending `p` as eight bytes with five leading zeros is a
/// different byte string from the one the server hashed into its own `p_q_inner_data`, and
/// the handshake fails at `req_DH_params` with no indication why.
fn trim_be(b: &[u8]) -> Vec<u8> {
    let start = b.iter().position(|&x| x != 0).unwrap_or(b.len() - 1);
    b[start..].to_vec()
}

/// The Diffie-Hellman parameters the server chose.
fn check_dh(g: i32, dh_prime: &[u8], g_a: &[u8]) -> Result<()> {
    // g is published as 2, 3, 4, 5, 6 or 7 for every Telegram DC. Anything else is either
    // a new configuration or an attack, and both deserve a stop.
    if !(2..=7).contains(&g) {
        return Err(Error::BadDhParams);
    }

    if dh_prime.len() != 256 || dh_prime[0] & 0x80 == 0 {
        return Err(Error::BadDhParams);
    }
    let mut h = Sha256::new();
    h.update(dh_prime);
    if h.finish() != DH_PRIME_SHA256 {
        return Err(Error::UnknownDhPrime);
    }

    // 1 < g_a < p-1. Both ends matter: g_a of 0, 1 or p-1 forces the shared secret to a
    // value the attacker knows without solving anything, and it is a one-line check that
    // clients have shipped without.
    if g_a.len() > 256 {
        return Err(Error::BadDhParams);
    }
    let mut wide = [0u8; 256];
    wide[256 - g_a.len()..].copy_from_slice(g_a);
    if wide.iter().all(|&x| x == 0) {
        return Err(Error::BadDhParams);
    }
    // g_a == 1
    if wide[..255].iter().all(|&x| x == 0) && wide[255] == 1 {
        return Err(Error::BadDhParams);
    }
    // g_a >= p-1, tested as g_a >= p or g_a == p-1.
    if wide[..] >= dh_prime[..] {
        return Err(Error::BadDhParams);
    }
    let mut p_minus_1 = [0u8; 256];
    p_minus_1.copy_from_slice(dh_prime);
    // The prime is odd, so subtracting one only touches the last byte.
    p_minus_1[255] -= 1;
    if wide == p_minus_1 {
        return Err(Error::BadDhParams);
    }

    Ok(())
}
