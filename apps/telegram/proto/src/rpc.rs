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
/// `auth.authorizationSignUpRequired#44747e9a flags:# terms_of_service:flags.0?help.TermsOfService = auth.Authorization;`
pub const AUTH_AUTHORIZATION_SIGNUP: u32 = 0x4474_7e9a;
/// `account.getPassword#548a30f5 = account.Password;`
pub const ACCOUNT_GET_PASSWORD: u32 = 0x548a_30f5;
/// `auth.checkPassword#d18b4d16 password:InputCheckPasswordSRP = auth.Authorization;`
pub const AUTH_CHECK_PASSWORD: u32 = 0xd18b_4d16;
/// `inputCheckPasswordSRP#d27ff082 srp_id:long A:bytes M1:bytes = InputCheckPasswordSRP;`
pub const INPUT_CHECK_PASSWORD_SRP: u32 = 0xd27f_f082;
/// `auth.resendCode#cae47523 flags:# phone_number:string phone_code_hash:string reason:flags.0?string = auth.SentCode;`
pub const AUTH_RESEND_CODE: u32 = 0xcae4_7523;
/// `auth.logOut#3e72ba19 = auth.LoggedOut;`
pub const AUTH_LOG_OUT: u32 = 0x3e72_ba19;

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
/// server answers `CONNECTION_NOT_INITED`, which arrives as an ordinary RPC error on a
/// session that is otherwise working perfectly — a live handshake on the handset completed,
/// stayed up, and then answered that to `auth.sendCode`, which reads as a bad request
/// rather than a missing preamble.
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

/// `messages.getDialogs#a0f4cb4f flags:# exclude_pinned:flags.0?true folder_id:flags.1?int
/// offset_date:int offset_id:int offset_peer:InputPeer limit:int hash:long = messages.Dialogs;`
pub const MESSAGES_GET_DIALOGS: u32 = 0xa0f4_cb4f;
/// `inputPeerEmpty#7f3b18ea = InputPeer;`
pub const INPUT_PEER_EMPTY: u32 = 0x7f3b_18ea;

/// The first page of the chat list.
///
/// When `offset_peer` is `None` the request is for the very first page — `offset_date` and
/// `offset_id` are set to zero and `inputPeerEmpty` is used, which starts from the newest
/// dialog. With a peer the server returns the page that comes after it, which is how
/// scrolling down loads more chats without re-fetching the ones already held.
///
/// `hash` is zero: it is a checksum of the ids the client already holds, and answering
/// `messages.dialogsNotModified` to a client that holds nothing would be a page of nothing.
/// `limit` is small on purpose — this handset draws twelve rows and a page of a hundred
/// dialogs is a megabyte of TL to walk on a 600 MHz in-order core.
pub fn get_dialogs(
    limit: i32,
    offset_date: i32,
    offset_id: i32,
    offset_peer: Option<(crate::chats::Kind, i64, i64)>,
) -> Vec<u8> {
    let mut w = Writer::with_capacity(64);
    w.ctor(MESSAGES_GET_DIALOGS)
        .uint(0) // flags: not excluding pinned, no folder
        .int(offset_date)
        .int(offset_id);
    match offset_peer {
        Some((kind, id, access_hash)) => input_peer(&mut w, kind, id, access_hash),
        None => { w.ctor(INPUT_PEER_EMPTY); }
    }
    w.int(limit)
        .ulong(0); // hash
    w.finish()
}

/// `inputPeerUser#dde8a54c`, `inputPeerChat#35a95cb9`, `inputPeerChannel#27bcbbfc`.
pub const INPUT_PEER_USER: u32 = 0xdde8_a54c;
pub const INPUT_PEER_CHAT: u32 = 0x35a9_5cb9;
pub const INPUT_PEER_CHANNEL: u32 = 0x27bc_bbfc;
/// `messages.getHistory#4423e6c5`
pub const MESSAGES_GET_HISTORY: u32 = 0x4423_e6c5;
/// `messages.sendMessage#fef48f62`
pub const MESSAGES_SEND_MESSAGE: u32 = 0xfef4_8f62;

/// `upload.getFile#be5335be`
pub const UPLOAD_GET_FILE: u32 = 0xbe53_35be;
/// `inputPhotoFileLocation#40181ffe`
pub const INPUT_PHOTO_FILE_LOCATION: u32 = 0x4018_1ffe;
/// `inputDocumentFileLocation#bad07584`
pub const INPUT_DOCUMENT_FILE_LOCATION: u32 = 0xbad0_7584;
/// `upload.file#096a18d5`
pub const UPLOAD_FILE: u32 = 0x096a_18d5;
/// `upload.fileCdnRedirect#f18cda44`
pub const UPLOAD_FILECDN: u32 = 0xf18c_da44;

/// `messages.getMessages#63c66506`
pub const MESSAGES_GET_MESSAGES: u32 = 0x63c66506;
/// `channels.getMessages#ad8c9a23`
pub const CHANNELS_GET_MESSAGES: u32 = 0xad8c_9a23;
/// `inputChannel#f35aec28`
pub const INPUT_CHANNEL: u32 = 0xf35a_ec28;
/// `inputMessageID#a676a322`
pub const INPUT_MESSAGE_ID: u32 = 0xa676a322;

/// Write a peer in the form a request takes.
///
/// A `Peer` is what the server sends; an `InputPeer` is what it accepts, and the difference
/// is the access hash — proof that the client learned the id legitimately. A chat has none.
fn input_peer(w: &mut Writer, kind: crate::chats::Kind, id: i64, access_hash: i64) {
    use crate::chats::Kind;
    match kind {
        Kind::User => {
            w.ctor(INPUT_PEER_USER).long(id).long(access_hash);
        }
        Kind::Chat => {
            w.ctor(INPUT_PEER_CHAT).long(id);
        }
        Kind::Channel => {
            w.ctor(INPUT_PEER_CHANNEL).long(id).long(access_hash);
        }
    }
}

/// A page of a conversation, newest first.
///
/// `offset_id` is exclusive and counts backwards: zero means "from the newest", and any
/// other value means "older than this message". That is what makes scrolling up cheap —
/// the client asks for the next page by handing back the oldest id it already holds,
/// instead of a page number the server would have to count to.
pub fn get_history(
    kind: crate::chats::Kind,
    id: i64,
    access_hash: i64,
    offset_id: i32,
    limit: i32,
) -> Vec<u8> {
    let mut w = Writer::with_capacity(64);
    w.ctor(MESSAGES_GET_HISTORY);
    input_peer(&mut w, kind, id, access_hash);
    w.int(offset_id)
        .int(0) // offset_date
        .int(0) // add_offset
        .int(limit)
        .int(0) // max_id
        .int(0) // min_id
        .ulong(0); // hash
    w.finish()
}

/// Send a text message.
///
/// `random_id` is the client's own id for it, and the server uses it to discard duplicates:
/// a message resent because the answer was lost arrives once. It must therefore be *the
/// same* across a resend and different across separate messages — a counter would repeat
/// after a reinstall, so it comes from the random source.
pub fn send_message(
    kind: crate::chats::Kind,
    id: i64,
    access_hash: i64,
    text: &str,
    random_id: i64,
) -> Vec<u8> {
    let mut w = Writer::with_capacity(64 + text.len());
    w.ctor(MESSAGES_SEND_MESSAGE).uint(0); // flags: nothing optional
    input_peer(&mut w, kind, id, access_hash);
    w.string(text).long(random_id);
    w.finish()
}

/// The largest chunk a single `upload.getFile` should ask for here.
///
/// Not the protocol's 1 MiB ceiling. `transport::MAX_FRAME` is exactly 1 MiB and treats
/// anything larger as an unrecoverable desynchronisation, and a 1 MiB payload plus the
/// `rpc_result` header, the message header, the padding and the `msg_key` is over it — so
/// asking for the maximum did not download a large photo, it dropped the connection.
///
/// 128 KiB satisfies the alignment rules (a multiple of 4096, and 1 MiB divides evenly by
/// it, so a chunk never straddles a 1 MiB boundary) and leaves the frame budget with room
/// to spare. It also bounds the peak: the reply, the copy out of it and the assembled file
/// are all live at once on a 4 MB heap.
pub const CHUNK: i32 = 128 * 1024;

/// Download part of a photo.
///
/// `thumb_size` must be the `type` of one of the photo's `sizes` — `"m"`, `"x"`, `"y"` and
/// so on. It is **not** optional for a photo: an empty string is accepted by
/// `inputDocumentFileLocation` but refused here with `LOCATION_INVALID`, which is why this
/// used to fail for every photo in every chat. See `chats::SizeOption`.
pub fn get_file_photo(
    id: i64,
    access_hash: i64,
    file_reference: &[u8],
    thumb_size: &str,
    offset: i64,
    limit: i32,
) -> Vec<u8> {
    let mut w = Writer::with_capacity(64 + file_reference.len() + thumb_size.len());
    w.ctor(UPLOAD_GET_FILE)
        .uint(0) // flags: no precise, no cdn_supported
        .ctor(INPUT_PHOTO_FILE_LOCATION)
        .long(id)
        .long(access_hash)
        .bytes(file_reference)
        .string(thumb_size)
        .long(offset)
        .int(limit);
    w.finish()
}

/// Download part of a document (voice message, video, file).
///
/// Here `thumb_size` genuinely is optional: empty means the document itself, and a size
/// type means one of its `thumbs`.
pub fn get_file_document(
    id: i64,
    access_hash: i64,
    file_reference: &[u8],
    thumb_size: &str,
    offset: i64,
    limit: i32,
) -> Vec<u8> {
    let mut w = Writer::with_capacity(64 + file_reference.len() + thumb_size.len());
    w.ctor(UPLOAD_GET_FILE)
        .uint(0)
        .ctor(INPUT_DOCUMENT_FILE_LOCATION)
        .long(id)
        .long(access_hash)
        .bytes(file_reference)
        .string(thumb_size)
        .long(offset)
        .int(limit);
    w.finish()
}

/// `auth.exportAuthorization#e5bfffcd`
pub const AUTH_EXPORT_AUTHORIZATION: u32 = 0xe5bf_ffcd;
/// `auth.importAuthorization#a57a7dad`
pub const AUTH_IMPORT_AUTHORIZATION: u32 = 0xa57a_7dad;
/// `auth.exportedAuthorization#b434e2b8`
pub const AUTH_EXPORTED_AUTHORIZATION: u32 = 0xb434_e2b8;

/// Ask the *current* data centre for a token that authorises us on another one.
///
/// This is the first half of reaching a file that does not live where the session does.
/// A photo carries a `dc_id`, and `upload.getFile` on the wrong data centre answers
/// `FILE_MIGRATE_x` — there is no fallback that serves it anyway. So a client that only
/// ever holds one connection can download only the media that happens to share its home
/// data centre, which is a minority of it.
pub fn export_authorization(dc_id: i32) -> Vec<u8> {
    let mut w = Writer::with_capacity(8);
    w.ctor(AUTH_EXPORT_AUTHORIZATION).int(dc_id);
    w.finish()
}

/// The second half, sent on a freshly handshaken connection to the target data centre.
///
/// Until this succeeds the new connection has an auth key but no *user* — it is
/// authenticated, not authorised, and `upload.getFile` on it returns `AUTH_KEY_UNREGISTERED`.
pub fn import_authorization(id: i64, bytes: &[u8]) -> Vec<u8> {
    let mut w = Writer::with_capacity(24 + bytes.len());
    w.ctor(AUTH_IMPORT_AUTHORIZATION).long(id).bytes(bytes);
    w.finish()
}

/// Read `auth.exportedAuthorization#b434e2b8 id:long bytes:bytes`.
pub fn parse_exported_authorization(body: &[u8]) -> Option<(i64, Vec<u8>)> {
    let mut r = Reader::new(body);
    if r.ctor().ok()? != AUTH_EXPORTED_AUTHORIZATION {
        return None;
    }
    let id = r.long().ok()?;
    let bytes = r.bytes().ok()?.to_vec();
    Some((id, bytes))
}

/// Fetch specific messages by id, used to get a fresh `file_reference` after the previous
/// one expired. The reply is a `messages.Messages` — parse it with `chats::parse_history`.
///
/// **A channel needs [`get_channel_messages`] instead.** `messages.getMessages` is for
/// users and small groups; asking it for a channel's messages returns an empty result,
/// because a channel's id space is separate and the method has no `channel` to scope by.
/// That is not a corner case here: a `file_reference` expires roughly daily, and channels
/// are where most of the media in a Telegram account lives, so the recovery path that used
/// this for everything was broken precisely where it was needed.
pub fn get_messages(ids: &[i32]) -> Vec<u8> {
    let mut w = Writer::with_capacity(12 + ids.len() * 8);
    w.ctor(MESSAGES_GET_MESSAGES)
        .ctor(tl::VECTOR)
        .uint(ids.len() as u32);
    for &id in ids {
        w.ctor(INPUT_MESSAGE_ID).int(id);
    }
    w.finish()
}

/// The same, scoped to a channel.
pub fn get_channel_messages(channel_id: i64, access_hash: i64, ids: &[i32]) -> Vec<u8> {
    let mut w = Writer::with_capacity(32 + ids.len() * 8);
    w.ctor(CHANNELS_GET_MESSAGES)
        .ctor(INPUT_CHANNEL)
        .long(channel_id)
        .long(access_hash)
        .ctor(tl::VECTOR)
        .uint(ids.len() as u32);
    for &id in ids {
        w.ctor(INPUT_MESSAGE_ID).int(id);
    }
    w.finish()
}

/// Refresh messages in whichever peer they belong to.
///
/// The dispatch is here rather than at the call site because getting it wrong is silent:
/// the wrong method returns an empty list, not an error, so the download simply never
/// retries and the user sees the original failure again.
pub fn refresh_messages(
    kind: crate::chats::Kind,
    id: i64,
    access_hash: i64,
    ids: &[i32],
) -> Vec<u8> {
    match kind {
        crate::chats::Kind::Channel => get_channel_messages(id, access_hash, ids),
        crate::chats::Kind::User | crate::chats::Kind::Chat => get_messages(ids),
    }
}

/// Extract the raw bytes from an `upload.file` reply.
///
/// The body is `upload.file#096a18d5 type:storage.FileType mtime:int bytes:bytes`.
/// Returns `None` on `upload.fileCdnRedirect`, which this build does not handle.
pub fn parse_file(body: &[u8]) -> Option<Vec<u8>> {
    let mut r = Reader::new(body);
    let ctor = r.ctor().ok()?;
    match ctor {
        UPLOAD_FILE => {
            // `type:storage.FileType` is a *boxed* constructor — four bytes of id, one of
            // `storage.fileJpeg#7efe0e`, `storage.filePng#a4f63c0` and eight more. Reading
            // it as a TL string, which is what this did, takes the first byte as a length:
            // for a JPEG that is 0x0e, so it consumed 15 bytes padded to 16 instead of 4,
            // and every field after it was read from inside the image. Every download
            // either failed to parse or produced bytes no codec would accept.
            let _file_type = r.ctor().ok()?;
            let _mtime = r.int().ok()?;
            Some(r.bytes().ok()?.to_vec())
        }
        UPLOAD_FILECDN => {
            // CDN redirects need a different path. For now, refuse gracefully.
            None
        }
        _ => None,
    }
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

pub fn account_get_password() -> Vec<u8> {
    let mut w = Writer::with_capacity(4);
    w.ctor(ACCOUNT_GET_PASSWORD);
    w.finish()
}

/// Ask for a fresh code when the first did not arrive.
pub fn auth_resend_code(phone: &str, code_hash: &str) -> Vec<u8> {
    let mut w = Writer::with_capacity(32 + phone.len() + code_hash.len());
    w.ctor(AUTH_RESEND_CODE).uint(0).string(phone).string(code_hash);
    w.finish()
}

pub fn auth_log_out() -> Vec<u8> {
    let mut w = Writer::with_capacity(4);
    w.ctor(AUTH_LOG_OUT);
    w.finish()
}

/// Answer the two-factor challenge with the SRP proof.
///
/// `a` is the client's public value and `m1` the proof, both from [`crate::srp`]. The
/// password itself never leaves the device and never appears in a request — that is the
/// point of SRP over sending a hash.
pub fn auth_check_password(srp_id: i64, a: &[u8], m1: &[u8]) -> Vec<u8> {
    let mut w = Writer::with_capacity(32 + a.len() + m1.len());
    w.ctor(AUTH_CHECK_PASSWORD)
        .ctor(INPUT_CHECK_PASSWORD_SRP)
        .long(srp_id)
        .bytes(a)
        .bytes(m1);
    w.finish()
}

/// The two-factor parameters from `account.getPassword`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PasswordParams {
    pub srp_id: i64,
    /// The server's public value.
    pub srp_b: Vec<u8>,
    pub salt1: Vec<u8>,
    pub salt2: Vec<u8>,
    pub g: u32,
    pub p: Vec<u8>,
    /// What the user set as a reminder, if anything.
    pub hint: String,
}

/// Parse an `account.password`.
///
/// Read through [`crate::walk`] rather than by hand: `account.password` has thirteen fields
/// of which six are flag-gated, and the ones that matter sit behind three of them. Counting
/// that out by hand is how a field ends up read as the one after it.
///
/// Returns `None` when the account has no password — `has_password` clear, which is also
/// what gates `current_algo`, `srp_B` and `srp_id`, so their absence *is* the answer.
pub fn parse_password(body: &[u8]) -> Result<Option<PasswordParams>> {
    use crate::schema as sc;
    use crate::walk::{as_int, as_long, as_str, Walker};

    let (c, f) = Walker::new(body).value().map_err(|_| Error::BadContainer)?;
    if c.id != sc::ACCOUNT_PASSWORD_CTOR {
        return Err(Error::Tl(tl::Error::Unexpected { want: sc::ACCOUNT_PASSWORD_CTOR, got: c.id }));
    }

    let (Some(algo_bytes), Some(srp_b), Some(srp_id)) = (
        f[sc::ACCOUNT_PASSWORD_CURRENT_ALGO].bytes,
        as_str(&f[sc::ACCOUNT_PASSWORD_SRP_B]),
        as_long(&f[sc::ACCOUNT_PASSWORD_SRP_ID]),
    ) else {
        return Ok(None);
    };

    let (ac, af) = Walker::new(algo_bytes).value().map_err(|_| Error::BadContainer)?;
    // passwordKdfAlgoUnknown means the server wants a client that speaks a KDF this one
    // does not. Refusing is correct: guessing at the algorithm produces a proof the server
    // rejects, which reads as a wrong password.
    if ac.id != sc::PASSWORDKDFALGOSHA256SHA256PBKDF2HMACSHA512ITER100000SHA256MODPOW_CTOR {
        return Err(Error::Tl(tl::Error::UnknownConstructor(ac.id)));
    }
    let salt1 = as_str(&af[sc::PASSWORDKDFALGOSHA256SHA256PBKDF2HMACSHA512ITER100000SHA256MODPOW_SALT1]);
    let salt2 = as_str(&af[sc::PASSWORDKDFALGOSHA256SHA256PBKDF2HMACSHA512ITER100000SHA256MODPOW_SALT2]);
    let g = as_int(&af[sc::PASSWORDKDFALGOSHA256SHA256PBKDF2HMACSHA512ITER100000SHA256MODPOW_G]);
    let p = as_str(&af[sc::PASSWORDKDFALGOSHA256SHA256PBKDF2HMACSHA512ITER100000SHA256MODPOW_P]);

    let (Some(salt1), Some(salt2), Some(g), Some(p)) = (salt1, salt2, g, p) else {
        return Err(Error::BadContainer);
    };

    Ok(Some(PasswordParams {
        srp_id,
        srp_b: srp_b.to_vec(),
        salt1: salt1.to_vec(),
        salt2: salt2.to_vec(),
        g: g as u32,
        p: p.to_vec(),
        hint: as_str(&f[sc::ACCOUNT_PASSWORD_HINT]).map(text).unwrap_or_default(),
    }))
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

    /// `storage.FileType` constructors, from api.tl lines 79-88. The ones whose low byte
    /// would be read as a plausible TL string length are the interesting cases.
    const STORAGE_FILE_TYPES: &[(u32, &str)] = &[
        (0x0007_efe0, "storage.fileJpeg"),
        (0x0a4f_63c0, "storage.filePng"),
        (0xcae1_aadf, "storage.fileGif"),
        (0x1081_464c, "storage.fileWebp"),
        (0xaa96_3b05, "storage.fileUnknown"),
        (0x40bc_6f52, "storage.filePartial"),
    ];

    fn upload_file_reply(file_type: u32, payload: &[u8]) -> Vec<u8> {
        // `upload.file#96a18d5 type:storage.FileType mtime:int bytes:bytes`
        let mut w = Writer::new();
        w.ctor(UPLOAD_FILE).ctor(file_type).int(1_700_000_000).bytes(payload);
        w.finish()
    }

    #[test]
    fn a_downloaded_file_survives_its_own_header() {
        // The bug this pins: `type` is a boxed four-byte constructor, and reading it as a
        // TL string took its low byte as a length. For a JPEG that is 0x0e, so 16 bytes
        // were consumed instead of 4 and the payload length was then read from inside the
        // image. Every download failed, and nothing tested it.
        //
        // Every FileType, because the damage depended on the low byte: 0x0e read 14 bytes,
        // 0xc0 read 192, and 0xdf read 223 — different failures from the same line.
        let payload: Vec<u8> = (0u8..=255).collect();
        for (id, name) in STORAGE_FILE_TYPES {
            let got = parse_file(&upload_file_reply(*id, &payload));
            assert_eq!(got.as_deref(), Some(&payload[..]), "{name} misparsed");
        }
    }

    #[test]
    fn an_empty_download_is_empty_rather_than_a_parse_failure() {
        // What the server sends past the end of a file. It has to be distinguishable from
        // "the reply made no sense", because one ends a chunk loop and the other is a bug.
        assert_eq!(parse_file(&upload_file_reply(0x0007_efe0, &[])), Some(Vec::new()));
    }

    #[test]
    fn a_long_download_round_trips_at_the_chunk_size() {
        // Past 253 bytes a TL string switches to the three-byte length form. A chunk is
        // 128 KiB, so that path is the only one ever used in practice.
        let payload = vec![0x5au8; CHUNK as usize];
        let got = parse_file(&upload_file_reply(0x0007_efe0, &payload)).unwrap();
        assert_eq!(got.len(), CHUNK as usize);
        assert!(got.iter().all(|b| *b == 0x5a));
    }

    #[test]
    fn a_cdn_redirect_is_refused_rather_than_misread() {
        let mut w = Writer::new();
        w.ctor(UPLOAD_FILECDN);
        assert_eq!(parse_file(&w.finish()), None);
    }

    #[test]
    fn a_photo_request_carries_the_size_type_it_was_given() {
        // An empty thumb_size is what the server answers LOCATION_INVALID to, so the
        // string has to reach the wire intact.
        let body = get_file_photo(0x1122_3344_5566_7788, 0x99, b"\x01\x02\x03", "x", 0, CHUNK);
        let mut r = Reader::new(&body);
        assert_eq!(r.ctor().unwrap(), UPLOAD_GET_FILE);
        assert_eq!(r.uint().unwrap(), 0); // flags
        assert_eq!(r.ctor().unwrap(), INPUT_PHOTO_FILE_LOCATION);
        assert_eq!(r.long().unwrap(), 0x1122_3344_5566_7788);
        assert_eq!(r.long().unwrap(), 0x99);
        assert_eq!(r.bytes().unwrap(), b"\x01\x02\x03");
        assert_eq!(r.bytes().unwrap(), b"x");
        assert_eq!(r.long().unwrap(), 0);
        assert_eq!(r.int().unwrap(), CHUNK);
    }

    #[test]
    fn the_chunk_size_obeys_the_alignment_rules() {
        // Both are required by the API: the offset and limit must be multiples of 4 KiB,
        // and a chunk must not straddle a 1 MiB boundary — which holds for any limit that
        // divides 1 MiB evenly.
        assert_eq!(CHUNK % 4096, 0);
        assert_eq!((1024 * 1024) % CHUNK, 0);
        // And it must leave room inside a frame for the headers wrapped around it, which
        // is what asking for the full 1 MiB did not.
        assert!((CHUNK as usize) < crate::transport::MAX_FRAME);
    }

    #[test]
    fn a_chunk_offset_stays_within_one_megabyte_block() {
        // offset / 1MiB == (offset + limit - 1) / 1MiB, for every chunk of a large file.
        const MB: i64 = 1024 * 1024;
        let mut offset = 0i64;
        while offset < 8 * MB {
            let last = offset + CHUNK as i64 - 1;
            assert_eq!(offset / MB, last / MB, "chunk at {offset} straddles a boundary");
            offset += CHUNK as i64;
        }
    }

    #[test]
    fn an_exported_authorization_round_trips() {
        // The token that turns a handshaken connection to another data centre into an
        // authorised one. Getting the two fields the wrong way round would produce an
        // importAuthorization the server rejects with no useful text.
        let mut w = Writer::new();
        w.ctor(AUTH_EXPORTED_AUTHORIZATION).long(0x1234_5678_9abc_def0u64 as i64).bytes(&[9, 8, 7]);
        let (id, bytes) = parse_exported_authorization(&w.finish()).unwrap();
        assert_eq!(id, 0x1234_5678_9abc_def0u64 as i64);
        assert_eq!(bytes, vec![9, 8, 7]);

        // And the request it goes into.
        let body = import_authorization(id, &bytes);
        let mut r = Reader::new(&body);
        assert_eq!(r.ctor().unwrap(), AUTH_IMPORT_AUTHORIZATION);
        assert_eq!(r.long().unwrap(), id);
        assert_eq!(r.bytes().unwrap(), &[9, 8, 7]);
    }

    #[test]
    fn a_wrong_constructor_is_not_read_as_an_authorization() {
        // An rpc_error arriving where the token was expected must not be mined for two
        // fields that happen to parse — importing garbage would look like a server fault.
        let mut w = Writer::new();
        w.ctor(RPC_ERROR).int(400).string("DC_ID_INVALID");
        assert_eq!(parse_exported_authorization(&w.finish()), None);
    }

    #[test]
    fn export_asks_for_the_data_centre_it_was_given() {
        let body = export_authorization(4);
        let mut r = Reader::new(&body);
        assert_eq!(r.ctor().unwrap(), AUTH_EXPORT_AUTHORIZATION);
        assert_eq!(r.int().unwrap(), 4);
    }

    #[test]
    fn refreshing_a_channel_uses_the_channel_method() {
        use crate::chats::Kind;

        // A channel is scoped by an inputChannel; a user or a group is not. Sending
        // messages.getMessages for a channel returns an empty list rather than an error, so
        // the file_reference recovery silently did nothing — and channels are where most of
        // an account's media lives.
        let ch = refresh_messages(Kind::Channel, 777, 0x5555, &[42]);
        let mut r = Reader::new(&ch);
        assert_eq!(r.ctor().unwrap(), CHANNELS_GET_MESSAGES);
        assert_eq!(r.ctor().unwrap(), INPUT_CHANNEL);
        assert_eq!(r.long().unwrap(), 777);
        assert_eq!(r.long().unwrap(), 0x5555);
        assert_eq!(r.ctor().unwrap(), tl::VECTOR);
        assert_eq!(r.uint().unwrap(), 1);
        assert_eq!(r.ctor().unwrap(), INPUT_MESSAGE_ID);
        assert_eq!(r.int().unwrap(), 42);

        // And the other two kinds keep the plain method, with no peer in front of the ids.
        for kind in [Kind::User, Kind::Chat] {
            let body = refresh_messages(kind, 777, 0x5555, &[42]);
            let mut r = Reader::new(&body);
            assert_eq!(r.ctor().unwrap(), MESSAGES_GET_MESSAGES, "{kind:?}");
            assert_eq!(r.ctor().unwrap(), tl::VECTOR);
            assert_eq!(r.uint().unwrap(), 1);
            assert_eq!(r.ctor().unwrap(), INPUT_MESSAGE_ID);
            assert_eq!(r.int().unwrap(), 42);
        }
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
