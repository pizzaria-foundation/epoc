//! Above the encrypted layer: unwrapping what arrives, and the calls a login needs.
//!
//! # A reply is not one message
//!
//! `Session::decrypt` hands back a body. That body is rarely the answer to anything on its
//! own. A single `help.getConfig` produced this, recorded from a real server:
//!
//! ```text
//! msg_container(0x73f1f8dc)          two messages: an ack and new_session_created
//! rpc_result(0xf35c6d01)             req_msg_id, then
//!   gzip_packed(0x3072cfa1)            the Config, deflated
//! ```
//!
//! So three unwrappings before a single field of the answer is visible, and a client that
//! handles only the outer layer sees nothing at all. [`unwrap`] flattens all of it into a
//! list of [`Update`]s.
//!
//! The gzip layer is why `symbian-crypto` has an inflate: the server compresses anything
//! that benefits, without asking, and the Config for one call was 404 bytes compressed.
//!
//! # Acknowledgement
//!
//! Every content message the server sends must be acknowledged with `msgs_ack`, or it
//! resends it — forever, on a timer, which on a metered connection is a real cost and on a
//! phone is a real battery cost. [`Update`] carries the `msg_id` that needs acking so the
//! caller cannot forget which ones.

use alloc::string::String;
use alloc::vec::Vec;

use symbian_crypto::inflate::inflate_gzip;

use crate::tl::{self, Reader, Writer};

/* Constructor ids. The four service ones are commented out in mtproto.tl as "parsed
 * manually", which is the schema's way of saying they are not ordinary boxed types --
 * rpc_result's payload is bare, and a container's elements have no length prefix of their
 * own. They are quoted here from that file all the same. */

/// `msg_container#73f1f8dc messages:vector<message> = MessageContainer;`
pub const MSG_CONTAINER: u32 = 0x73f1_f8dc;
/// `rpc_result#f35c6d01 req_msg_id:long result:Object = RpcResult;`
pub const RPC_RESULT: u32 = 0xf35c_6d01;
/// `gzip_packed#3072cfa1 packed_data:bytes = Object;`
pub const GZIP_PACKED: u32 = 0x3072_cfa1;
/// `rpc_error#2144ca19 error_code:int error_message:string = RpcError;`
pub const RPC_ERROR: u32 = 0x2144_ca19;
/// `msgs_ack#62d6b459 msg_ids:Vector<long> = MsgsAck;`
pub const MSGS_ACK: u32 = 0x62d6_b459;
/// `bad_server_salt#edab447b bad_msg_id:long bad_msg_seqno:int error_code:int new_server_salt:long = BadMsgNotification;`
pub const BAD_SERVER_SALT: u32 = 0xedab_447b;
/// `bad_msg_notification#a7eff811 bad_msg_id:long bad_msg_seqno:int error_code:int = BadMsgNotification;`
pub const BAD_MSG_NOTIFICATION: u32 = 0xa7ef_f811;
/// `new_session_created#9ec20908 first_msg_id:long unique_id:long server_salt:long = NewSession;`
pub const NEW_SESSION_CREATED: u32 = 0x9ec2_0908;
/// `pong#347773c5 msg_id:long ping_id:long = Pong;`
pub const PONG: u32 = 0x3477_73c5;
/// `ping#7abe77ec ping_id:long = Pong;`
pub const PING: u32 = 0x7abe_77ec;

/// `invokeWithLayer#da9b0d0d {X:Type} layer:int query:!X = X;`
pub const INVOKE_WITH_LAYER: u32 = 0xda9b_0d0d;
/// `initConnection#c1cd5ea9 {X:Type} flags:# api_id:int device_model:string system_version:string app_version:string system_lang_code:string lang_pack:string lang_code:string ... query:!X = X;`
pub const INIT_CONNECTION: u32 = 0xc1cd_5ea9;
/// `help.getConfig#c4f9186b = Config;`
pub const HELP_GET_CONFIG: u32 = 0xc4f9_186b;
/// `auth.sendCode#a677244f phone_number:string api_id:int api_hash:string settings:CodeSettings = auth.SentCode;`
pub const AUTH_SEND_CODE: u32 = 0xa677_244f;
/// `codeSettings#ad253d78 flags:# ... = CodeSettings;`
pub const CODE_SETTINGS: u32 = 0xad25_3d78;
/// `auth.signIn#8d52a951 flags:# phone_number:string phone_code_hash:string phone_code:flags.0?string email_verification:flags.1?EmailVerification = auth.Authorization;`
pub const AUTH_SIGN_IN: u32 = 0x8d52_a951;
/// `auth.sentCode#5e002502 flags:# type:auth.SentCodeType phone_code_hash:string next_type:flags.1?auth.CodeType timeout:flags.2?int = auth.SentCode;`
pub const AUTH_SENT_CODE: u32 = 0x5e00_2502;
/// `auth.authorization#2ea2c0d4 flags:# ... user:User = auth.Authorization;`
pub const AUTH_AUTHORIZATION: u32 = 0x2ea2_c0d4;

/// The API layer this client speaks, from the `// LAYER` line at the end of `api.tl`.
///
/// Sent once per connection in `invokeWithLayer`. The server adapts its replies to it, so
/// raising this number means every constructor this crate parses has to be rechecked
/// against the new schema — it is a version pin, not a preference.
pub const LAYER: i32 = 228;

/// Largest `gzip_packed` payload this will inflate.
///
/// A length inside a message the server sent, on a handset with 45 MB of RAM. It is
/// authenticated by `msg_key`, so it is not attacker-controlled — but a bound that cannot
/// fire is cheaper than an argument about whether it can.
const MAX_INFLATED: usize = 1 << 20;

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Error {
    Tl(tl::Error),
    /// A `gzip_packed` payload that would not inflate, or was larger than [`MAX_INFLATED`].
    BadGzip,
    /// A container whose declared count does not fit the bytes it arrived in.
    BadContainer,
}

impl From<tl::Error> for Error {
    fn from(e: tl::Error) -> Self {
        Error::Tl(e)
    }
}

pub type Result<T> = core::result::Result<T, Error>;

/// One thing the server said, after every wrapper is off.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Update {
    /// The answer to a call. `body` starts at the result's own constructor id.
    Result { req_msg_id: u64, msg_id: u64, body: Vec<u8> },
    /// The call failed. `code` is HTTP-like; `text` is machine-readable, such as
    /// `PHONE_NUMBER_INVALID` or `FLOOD_WAIT_42`.
    RpcError { req_msg_id: u64, code: i32, text: String },
    /// The salt has rotated. Adopt it with [`crate::session::Session::set_salt`] and resend
    /// whatever was rejected; ignoring this means every message failing an hour after login.
    NewSalt { salt: u64 },
    /// A new session was created server-side, usually after a reconnect. Carries a salt too.
    NewSession { salt: u64, first_msg_id: u64 },
    /// The server rejected a message's `msg_id` or `seq_no`. Code 16/17 mean the clock is
    /// out and the offset needs recomputing; 32/33 mean `seq_no` drifted.
    BadMessage { bad_msg_id: u64, code: i32 },
    /// A reply to `ping`.
    Pong { ping_id: u64 },
    /// The server acknowledged messages it received.
    Ack { msg_ids: Vec<u64> },
    /// Something this build does not parse. Kept rather than dropped: an unknown update is
    /// normal — the schema is thousands of constructors and this crate knows fifteen — and
    /// silently discarding them makes a client that "works" while missing half the traffic.
    Unknown { msg_id: u64, ctor: u32 },
}

impl Update {
    /// The `msg_id` to acknowledge, if this needs acknowledging.
    ///
    /// Acks and pongs do not; the rest do. Not acking makes the server resend on a timer,
    /// forever, which costs data and battery on exactly the device that can least afford it.
    pub fn needs_ack(&self) -> Option<u64> {
        match self {
            Update::Result { msg_id, .. } => Some(*msg_id),
            Update::NewSession { first_msg_id, .. } => Some(*first_msg_id),
            Update::Unknown { msg_id, .. } => Some(*msg_id),
            _ => None,
        }
    }
}

/// Flatten a decrypted body into everything it contains.
///
/// Recursive in effect but not in code: containers do not nest in practice, and a fixed
/// two-level walk cannot be driven into a stack overflow by a malformed message.
pub fn unwrap(msg_id: u64, body: &[u8]) -> Result<Vec<Update>> {
    let mut out = Vec::new();
    let mut r = Reader::new(body);
    let ctor = r.ctor()?;

    if ctor == MSG_CONTAINER {
        let count = r.uint()? as usize;
        // Each element is msg_id(8) + seqno(4) + len(4) at minimum.
        if count.saturating_mul(16) > r.remaining() {
            return Err(Error::BadContainer);
        }
        for _ in 0..count {
            let inner_id = r.ulong()?;
            let _seq = r.uint()?;
            let len = r.uint()? as usize;
            if len > r.remaining() {
                return Err(Error::BadContainer);
            }
            let inner = r.raw(len)?;
            // One level only. A container inside a container is not something Telegram
            // sends, and allowing it would mean bounding the depth.
            out.push(parse_one(inner_id, inner)?);
        }
        return Ok(out);
    }

    out.push(parse_one_with_ctor(msg_id, ctor, &mut r, body)?);
    Ok(out)
}

fn parse_one(msg_id: u64, body: &[u8]) -> Result<Update> {
    let mut r = Reader::new(body);
    let ctor = r.ctor()?;
    parse_one_with_ctor(msg_id, ctor, &mut r, body)
}

fn parse_one_with_ctor(
    msg_id: u64,
    ctor: u32,
    r: &mut Reader<'_>,
    whole: &[u8],
) -> Result<Update> {
    match ctor {
        RPC_RESULT => {
            let req_msg_id = r.ulong()?;
            let rest = &whole[r.pos()..];
            let body = maybe_inflate(rest)?;

            // An error is delivered *as* the result, not alongside it.
            let mut ir = Reader::new(&body);
            if ir.ctor() == Ok(RPC_ERROR) {
                let code = ir.int()?;
                let message = ir.bytes()?;
                return Ok(Update::RpcError { req_msg_id, code, text: text(message) });
            }
            Ok(Update::Result { req_msg_id, msg_id, body })
        }
        BAD_SERVER_SALT => {
            let _bad = r.ulong()?;
            let _seq = r.uint()?;
            let _code = r.int()?;
            Ok(Update::NewSalt { salt: r.ulong()? })
        }
        BAD_MSG_NOTIFICATION => {
            let bad_msg_id = r.ulong()?;
            let _seq = r.uint()?;
            Ok(Update::BadMessage { bad_msg_id, code: r.int()? })
        }
        NEW_SESSION_CREATED => {
            let first_msg_id = r.ulong()?;
            let _unique = r.ulong()?;
            Ok(Update::NewSession { salt: r.ulong()?, first_msg_id })
        }
        PONG => {
            let _mid = r.ulong()?;
            Ok(Update::Pong { ping_id: r.ulong()? })
        }
        MSGS_ACK => Ok(Update::Ack { msg_ids: r.vector_long()? }),
        GZIP_PACKED => {
            let inner = maybe_inflate(whole)?;
            parse_one(msg_id, &inner)
        }
        other => Ok(Update::Unknown { msg_id, ctor: other }),
    }
}

/// Inflate if the bytes begin with `gzip_packed`, otherwise pass them through.
///
/// The server compresses whatever benefits, unannounced, at any nesting level — the
/// recorded `help.getConfig` reply had it inside `rpc_result`. A client that does not check
/// sees a constructor id of `0x3072cfa1` where it expected a Config and has no way to know
/// what happened.
fn maybe_inflate(bytes: &[u8]) -> Result<Vec<u8>> {
    if bytes.len() < 4 {
        return Err(Error::Tl(tl::Error::Truncated));
    }
    let ctor = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    if ctor != GZIP_PACKED {
        return Ok(bytes.to_vec());
    }
    let mut r = Reader::new(bytes);
    r.ctor()?;
    let packed = r.bytes()?;
    inflate_gzip(packed, MAX_INFLATED).map_err(|_| Error::BadGzip)
}

// --------------------------------------------------------------------- requests --

/// Wrap a query in `initConnection` and `invokeWithLayer`.
///
/// Required on the first call of every connection and harmless afterwards. Without it the
/// server answers `CONNECTION_LAYER_INVALID` — which is at least a legible error, unlike
/// most of what goes wrong here.
pub fn init_connection(api_id: i32, device: &str, system: &str, app: &str, query: &[u8])
    -> Vec<u8>
{
    let mut w = Writer::with_capacity(96 + query.len());
    w.ctor(INVOKE_WITH_LAYER)
        .int(LAYER)
        .ctor(INIT_CONNECTION)
        .uint(0) // flags: no proxy, no params
        .int(api_id)
        .string(device)
        .string(system)
        .string(app)
        .string("en") // system_lang_code
        .string("") // lang_pack, empty for a non-official client
        .string("en") // lang_code
        .raw(query);
    w.finish()
}

pub fn get_config() -> Vec<u8> {
    let mut w = Writer::with_capacity(4);
    w.ctor(HELP_GET_CONFIG);
    w.finish()
}

pub fn ping(ping_id: u64) -> Vec<u8> {
    let mut w = Writer::with_capacity(12);
    w.ctor(PING).ulong(ping_id);
    w.finish()
}

pub fn msgs_ack(ids: &[u64]) -> Vec<u8> {
    let mut w = Writer::with_capacity(12 + ids.len() * 8);
    w.ctor(MSGS_ACK).ctor(tl::VECTOR).uint(ids.len() as u32);
    for &id in ids {
        w.ulong(id);
    }
    w.finish()
}

/// Ask for a login code.
///
/// `api_id` and `api_hash` identify the *application*, not the user, and come from
/// my.telegram.org. There is no default worth shipping: Telegram bans the ones that leak
/// into public clients, so a hardcoded pair is a client that stops working without warning.
pub fn auth_send_code(phone: &str, api_id: i32, api_hash: &str) -> Vec<u8> {
    let mut w = Writer::with_capacity(64 + phone.len() + api_hash.len());
    w.ctor(AUTH_SEND_CODE)
        .string(phone)
        .int(api_id)
        .string(api_hash)
        // codeSettings with every flag clear. The optional fields are all opt-ins for
        // delivery methods a Symbian handset cannot do: no app hash, no flash call, no
        // Firebase.
        .ctor(CODE_SETTINGS)
        .uint(0);
    w.finish()
}

/// Sign in with the code the user typed.
///
/// `phone_code_hash` comes from the `auth.sentCode` reply and ties the code to the request
/// that produced it.
pub fn auth_sign_in(phone: &str, code_hash: &str, code: &str) -> Vec<u8> {
    let mut w = Writer::with_capacity(32 + phone.len() + code_hash.len() + code.len());
    w.ctor(AUTH_SIGN_IN)
        .uint(1) // flags: bit 0 set, phone_code present
        .string(phone)
        .string(code_hash)
        .string(code);
    w.finish()
}

/// What `auth.sendCode` returned.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SentCode {
    /// Pass back to [`auth_sign_in`].
    pub phone_code_hash: String,
    /// How many digits the code has, when the server says.
    pub code_length: Option<i32>,
}

/// Parse an `auth.sentCode`.
///
/// The `type` field is a boxed `auth.SentCodeType` with a dozen constructors — SMS, a call,
/// the Telegram app itself, a missed call, an email. Only the length is read, from the ones
/// that carry it, because the rest changes what the *user* should be told and the UI does
/// not have those screens yet. An unrecognised type is not an error: the code still arrives.
pub fn parse_sent_code(body: &[u8]) -> Result<SentCode> {
    let mut r = Reader::new(body);
    r.expect(AUTH_SENT_CODE).map_err(Error::Tl)?;
    let _flags = r.uint()?;

    let type_ctor = r.ctor()?;
    // auth.sentCodeTypeSms, ...App, ...Call, ...FlashCall and ...MissedCall all begin with
    // a length. The others do not, and are skipped.
    let code_length = match type_ctor {
        0xc000_bba2 | 0x3dbb_5986 | 0x5353_e5a7 | 0xab03_c6d9 | 0x82006484 => {
            Some(r.int()?)
        }
        _ => None,
    };

    // The hash follows the type in every case, but only if the type was fully consumed.
    // For an unrecognised type the reader is mid-struct and the hash cannot be found, which
    // is reported rather than guessed at.
    let hash = r.bytes()?;
    Ok(SentCode {
        phone_code_hash: text(hash),
        code_length,
    })
}

/// TL bytes as text.
///
/// Telegram sends UTF-8, and `msg_key` has already authenticated these bytes by the time
/// anything calls this — so invalid UTF-8 here means corruption that should have been
/// impossible. Replacing the bad bytes rather than failing keeps a mangled error message
/// readable, which is worth more than a parse error that says less than the string would
/// have.
///
/// A free function rather than a trait method on `String`: `from_utf8_lossy_owned` is a
/// name std is likely to take, and shadowing it would break on a future toolchain.
fn text(b: &[u8]) -> String {
    match core::str::from_utf8(b) {
        Ok(s) => String::from(s),
        Err(_) => b.iter().map(|&c| if c.is_ascii() { c as char } else { '?' }).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn wrap_result(req_msg_id: u64, inner: &[u8]) -> Vec<u8> {
        let mut w = Writer::new();
        w.ctor(RPC_RESULT).ulong(req_msg_id).raw(inner);
        w.finish()
    }

    #[test]
    fn a_plain_result_comes_through() {
        let inner = 0xdead_beefu32.to_le_bytes();
        let got = unwrap(99, &wrap_result(7, &inner)).unwrap();
        assert_eq!(got, vec![Update::Result { req_msg_id: 7, msg_id: 99, body: inner.to_vec() }]);
    }

    #[test]
    fn an_rpc_error_is_not_a_result() {
        // Errors arrive *as* the result. A parser that treats rpc_result as always
        // successful hands the error bytes to whatever expected a Config.
        let mut w = Writer::new();
        w.ctor(RPC_ERROR).int(420).string("FLOOD_WAIT_42");
        let got = unwrap(1, &wrap_result(7, &w.finish())).unwrap();
        assert_eq!(
            got,
            vec![Update::RpcError { req_msg_id: 7, code: 420, text: String::from("FLOOD_WAIT_42") }]
        );
    }

    #[test]
    fn a_container_yields_every_message() {
        let a = {
            let mut w = Writer::new();
            w.ctor(NEW_SESSION_CREATED).ulong(11).ulong(22).ulong(0xabcd);
            w.finish()
        };
        let b = {
            let mut w = Writer::new();
            w.ctor(MSGS_ACK).ctor(tl::VECTOR).uint(2).ulong(5).ulong(6);
            w.finish()
        };
        let mut w = Writer::new();
        w.ctor(MSG_CONTAINER).uint(2);
        w.ulong(101).uint(0).uint(a.len() as u32).raw(&a);
        w.ulong(102).uint(0).uint(b.len() as u32).raw(&b);

        let got = unwrap(1, &w.finish()).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0], Update::NewSession { salt: 0xabcd, first_msg_id: 11 });
        assert_eq!(got[1], Update::Ack { msg_ids: vec![5, 6] });
    }

    #[test]
    fn a_container_with_a_lying_count_is_refused() {
        // Not a hypothetical: a desynchronised stream produces exactly this, and the naive
        // loop asks for a million allocations before failing.
        let mut w = Writer::new();
        w.ctor(MSG_CONTAINER).uint(100_000);
        assert_eq!(unwrap(1, &w.finish()), Err(Error::BadContainer));
    }

    #[test]
    fn a_gzipped_result_is_inflated() {
        // The recorded help.getConfig reply was exactly this shape: rpc_result wrapping
        // gzip_packed. A client that does not inflate sees 0x3072cfa1 where it expected a
        // Config, and nothing says why.
        let payload = b"\x0e\x24\x1a\xcc some config bytes that compress nicely nicely nicely";
        let gz = gzip(payload);
        let mut inner = Writer::new();
        inner.ctor(GZIP_PACKED).bytes(&gz);
        let got = unwrap(1, &wrap_result(7, &inner.finish())).unwrap();
        match &got[0] {
            Update::Result { body, .. } => assert_eq!(body, payload),
            other => panic!("expected a Result, got {other:?}"),
        }
    }

    #[test]
    fn an_unknown_constructor_is_kept_rather_than_dropped() {
        let mut w = Writer::new();
        w.ctor(0x1234_5678);
        let got = unwrap(42, &w.finish()).unwrap();
        assert_eq!(got, vec![Update::Unknown { msg_id: 42, ctor: 0x1234_5678 }]);
    }

    #[test]
    fn only_the_right_things_need_acking() {
        assert_eq!(
            Update::Result { req_msg_id: 1, msg_id: 9, body: vec![] }.needs_ack(),
            Some(9)
        );
        assert_eq!(Update::Ack { msg_ids: vec![] }.needs_ack(), None);
        assert_eq!(Update::Pong { ping_id: 1 }.needs_ack(), None);
        assert_eq!(Update::NewSalt { salt: 1 }.needs_ack(), None);
    }

    #[test]
    fn init_connection_wraps_in_the_right_order() {
        // invokeWithLayer outermost, then initConnection, then the query. Reversed, the
        // server answers CONNECTION_LAYER_INVALID and the cause is not obvious from it.
        let out = init_connection(6, "Nokia E72", "Symbian 9.3", "0.1", &get_config());
        let mut r = Reader::new(&out);
        assert_eq!(r.ctor().unwrap(), INVOKE_WITH_LAYER);
        assert_eq!(r.int().unwrap(), LAYER);
        assert_eq!(r.ctor().unwrap(), INIT_CONNECTION);
        assert_eq!(r.uint().unwrap(), 0);
        assert_eq!(r.int().unwrap(), 6);
        assert_eq!(r.bytes().unwrap(), b"Nokia E72");
        assert_eq!(r.bytes().unwrap(), b"Symbian 9.3");
        assert_eq!(r.bytes().unwrap(), b"0.1");
        assert_eq!(r.bytes().unwrap(), b"en");
        assert_eq!(r.bytes().unwrap(), b"");
        assert_eq!(r.bytes().unwrap(), b"en");
        assert_eq!(r.ctor().unwrap(), HELP_GET_CONFIG);
        assert!(r.is_empty());
    }

    #[test]
    fn sign_in_sets_the_flag_for_the_code() {
        // phone_code is flags.0. Sending the string with the flag clear means the server
        // reads the next field as something else entirely.
        let out = auth_sign_in("+5511999999999", "hash", "12345");
        let mut r = Reader::new(&out);
        assert_eq!(r.ctor().unwrap(), AUTH_SIGN_IN);
        assert_eq!(r.uint().unwrap(), 1);
        assert_eq!(r.bytes().unwrap(), b"+5511999999999");
        assert_eq!(r.bytes().unwrap(), b"hash");
        assert_eq!(r.bytes().unwrap(), b"12345");
    }

    #[test]
    fn a_sent_code_reply_parses() {
        let mut w = Writer::new();
        w.ctor(AUTH_SENT_CODE)
            .uint(0)
            .ctor(0xc000_bba2) // auth.sentCodeTypeSms
            .int(5)
            .string("abc123");
        let got = parse_sent_code(&w.finish()).unwrap();
        assert_eq!(got.phone_code_hash, "abc123");
        assert_eq!(got.code_length, Some(5));
    }

    #[test]
    fn an_unknown_code_type_does_not_lose_the_hash() {
        // New delivery methods appear regularly. The length is optional; the hash is not,
        // and without it there is no way to sign in at all.
        let mut w = Writer::new();
        w.ctor(AUTH_SENT_CODE).uint(0).ctor(0x0bad_0bad).string("xyz");
        let got = parse_sent_code(&w.finish()).unwrap();
        assert_eq!(got.phone_code_hash, "xyz");
        assert_eq!(got.code_length, None);
    }

    /// Minimal gzip, so the inflate test has something real to chew on.
    fn gzip(data: &[u8]) -> Vec<u8> {
        // Stored (uncompressed) deflate blocks inside a gzip wrapper: valid, and it means
        // this helper cannot itself have a compression bug.
        let mut out = vec![0x1f, 0x8b, 0x08, 0x00, 0, 0, 0, 0, 0x00, 0xff];
        for (i, chunk) in data.chunks(65535).enumerate() {
            let last = (i + 1) * 65535 >= data.len();
            out.push(if last { 1 } else { 0 });
            out.extend_from_slice(&(chunk.len() as u16).to_le_bytes());
            out.extend_from_slice(&(!(chunk.len() as u16)).to_le_bytes());
            out.extend_from_slice(chunk);
        }
        let crc = crc32(data);
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out
    }

    fn crc32(data: &[u8]) -> u32 {
        let mut c = 0xffff_ffffu32;
        for &b in data {
            c ^= b as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { (c >> 1) ^ 0xedb8_8320 } else { c >> 1 };
            }
        }
        !c
    }
}
