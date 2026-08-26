//! HTTP through the platform's own stack — asynchronous, and the one route safe on the GUI thread.
//!
//! # Why this exists next to [`crate::tls`], which also fetches URLs
//!
//! [`crate::tls`] blocks. It is a fetch inside `User::WaitForRequest` on a private worker thread,
//! which is right for a headless one-shot and unusable for anything with a window: a browser has to
//! draw while bytes arrive, and it has to abandon a load because the user pressed Back. Neither is
//! something you add to a blocking call afterwards.
//!
//! This module is the other route. `RHTTPSession` is driven by active objects in the calling
//! thread and reports progress as events, so a fetch is a state machine fed from the pump like
//! every other asynchronous thing in this SDK. Nothing here waits, and that is the whole
//! difference.
//!
//! # What comes from the platform, and what that costs
//!
//! HTTP/1.1, chunked decoding, connection reuse, cookies, and redirects — followed automatically
//! for GET, so a 301 costs nothing. TLS arrives the same way, through `CSecureSocket`, which this
//! handset's patched `ssl.dll` negotiates at 1.2.
//!
//! The price is that none of it is ours to fix. Whose certificates the phone trusts is a 2009
//! store; what it does with an unfamiliar `Content-Encoding` is its business; its timeouts are its
//! own. So [`Fetch`] reports what actually came back — [`Response::flags`], and the platform error
//! code kept intact rather than collapsed to a boolean, because an untrusted certificate is only
//! distinguishable from a dead server by its code.
//!
//! # Why a trait
//!
//! Same reason as [`crate::net`]. The FFI needs a phone; the state machine does not, and the state
//! machine is where the bugs are — a body that arrives in four chunks, a transaction that fails
//! after headers, a cancel racing a completion. Behind [`Http`], [`ShimHttp`] is the FFI and the
//! tests replay event sequences the network produces rarely and a test produces every time.
//!
//! # One at a time
//!
//! The shim holds a single transaction, so [`Fetch::start`] replaces whatever was in flight. A page
//! is 70 requests that want to be concurrent, and that is F3's problem: a slot table sized against
//! something real beats one guessed at now.

use alloc::string::String;
use alloc::vec::Vec;

pub mod cache;

use symbian_crypto::inflate::{inflate_any_to, Error as InflateError, Sink};
use symbian_sys as sys;

use crate::error::{Error, Result};

pub use crate::net::RawEvent;

/// What the response turned out to be, past the status code.
///
/// A bitset rather than four bools because it comes across the FFI as one integer, and because the
/// interesting readings are combinations: `GZIP` alone means the stack inflated the body for us,
/// `GZIP | GZIP_MAGIC` means it did not and [`crate::zlib`] has work to do.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Flags(pub i32);

impl Flags {
    /// The response said `Content-Encoding: gzip`.
    pub fn gzip(self) -> bool {
        self.0 & sys::SHIM_HTTP_GZIP != 0
    }

    /// The response said `Transfer-Encoding: chunked`.
    pub fn chunked(self) -> bool {
        self.0 & sys::SHIM_HTTP_CHUNKED != 0
    }

    /// The body starts `1f 8b`, so the compressed bytes came through undecoded.
    pub fn gzip_magic(self) -> bool {
        self.0 & sys::SHIM_HTTP_GZIP_MAGIC != 0
    }

    /// More body arrived than the shim buffers. [`Response::total`] is still the real size.
    pub fn truncated(self) -> bool {
        self.0 & sys::SHIM_HTTP_TRUNCATED != 0
    }

    /// Whether the caller has to inflate the body itself.
    ///
    /// The question every caller actually has, and the reason both flags are reported separately:
    /// this is `gzip declared AND not already decoded`, and getting it from one flag alone is wrong
    /// in one direction or the other.
    pub fn needs_inflate(self) -> bool {
        self.gzip() && self.gzip_magic()
    }
}

/// The response body, accumulated as it arrives and delivered decoded.
///
/// # Why the compressed side is the one held
///
/// The F2 measurements settled this. Every one of ten real pages arrived `Content-Encoding: gzip`
/// with the compressed bytes intact — the platform stack never inflates — and the sizes were
/// asymmetric in the direction that helps: the largest was 294 KB compressed and over a megabyte
/// inflated. So this holds the *compressed* body, which is the small side, and inflates it through
/// a [`Sink`] on completion, which is the side that never has to exist all at once. Peak cost per
/// page is the compressed body plus the 32 KB DEFLATE window, not both bodies.
///
/// # Why not inflate as it arrives
///
/// Because the decoder would have to be resumable — able to stop mid-Huffman-symbol and continue
/// when the next part shows up — and the stack delivers parts as small as **one byte** (measured on
/// `google.com`, whose first nine parts were a byte each). A resumable bit-level decoder is where
/// the subtle bugs live, and the memory it would save is the compressed side: the small one.
pub struct Body {
    buf: Vec<u8>,
    cap: usize,
    dropped: usize,
}

impl Body {
    /// `cap` bounds the compressed bytes held. Past it, bytes are counted and dropped — an honest
    /// short page rather than an allocation that kills the process, which is the same trade
    /// [`crate::zlib`] and the shim's own backlog limit make.
    pub fn with_cap(cap: usize) -> Self {
        Body { buf: Vec::new(), cap, dropped: 0 }
    }

    /// Feed bytes as they arrive from [`Fetch::read`].
    pub fn push(&mut self, bytes: &[u8]) {
        let room = self.cap.saturating_sub(self.buf.len());
        let take = core::cmp::min(room, bytes.len());
        self.buf.extend_from_slice(&bytes[..take]);
        self.dropped += bytes.len() - take;
    }

    /// How many compressed bytes are held.
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Bytes that did not fit. Non-zero means the content is incomplete, and a caller that parses
    /// it anyway is parsing a truncated page — which is why this is asked for rather than logged.
    pub fn dropped(&self) -> usize {
        self.dropped
    }

    /// The compressed bytes, for a caller that wants to cache the response as it came off the wire.
    pub fn raw(&self) -> &[u8] {
        &self.buf
    }

    /// Deliver the decoded body to `sink`, inflating only if the response says to.
    ///
    /// The decision is per response and comes from both flags together: `Content-Encoding: gzip`
    /// alone does not mean the bytes are compressed, because a stack that inflated them may leave
    /// the header behind. See [`Flags::needs_inflate`].
    ///
    /// `max_out` bounds the *decoded* size. DEFLATE's ratio is unbounded, so a page is
    /// attacker-controlled input like any other and the caller has to say what it will hold.
    pub fn decode_to<S: Sink>(&self, flags: Flags, max_out: usize, sink: &mut S) -> Result<usize> {
        if !flags.needs_inflate() {
            sink.write(&self.buf).map_err(|_| Error::Argument)?;
            return Ok(self.buf.len());
        }
        // `inflate_any_to` rather than the gzip form: a body whose header says gzip and whose bytes
        // say zlib is a thing servers do, and sniffing costs two comparisons.
        inflate_any_to(&self.buf, max_out, sink).map_err(inflate_error)
    }
}

/// Map an inflate failure onto this crate's error type, keeping the distinctions a caller can act
/// on: too large is a limit the caller chose and can raise, and everything else is a body that must
/// not be displayed.
///
/// `Platform(-20)` is `KErrCorrupt`, borrowed rather than given its own variant: [`Error`] is
/// matched exhaustively in several places in this workspace, and a new variant would be a breaking
/// change to every one of them for the sake of one call site.
fn inflate_error(e: InflateError) -> Error {
    match e {
        InflateError::TooLarge => Error::Overflow,
        InflateError::Sink => Error::Argument,
        InflateError::Truncated => Error::UnexpectedEof,
        // Corrupt, BadDistance, ChecksumMismatch: the bytes are not what they claim to be.
        _ => Error::Platform(-20),
    }
}

/// A finished transaction.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Response {
    /// The HTTP status of the page that actually loaded — redirects are already followed.
    pub status: u16,
    /// Every body byte the stack delivered, including any past the shim's buffer.
    pub total: usize,
    /// How many body callbacks it took. Diagnostic: it is what chunked encoding looks like from
    /// here, and a page that arrives in one part is a page the stack buffered whole.
    pub parts: u32,
    pub flags: Flags,
}

impl Response {
    /// Whether the status is one that carries a page worth parsing.
    pub fn is_ok(&self) -> bool {
        self.status >= 200 && self.status < 300
    }
}

/// The validators a response carried, if any.
///
/// Empty strings rather than `Option`, because "the server sent no ETag" and "the ETag is empty" are
/// the same thing to every caller, and a nested option at each use site buys nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Validators {
    pub etag: String,
    pub last_modified: String,
}

impl Validators {
    /// Whether a stored copy carrying these could be revalidated instead of refetched.
    pub fn any(&self) -> bool {
        !self.etag.is_empty() || !self.last_modified.is_empty()
    }
}

/// What feeding an event to a [`Fetch`] produced.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Progress {
    /// Not for this fetch, or nothing to report yet.
    Idle,
    /// Headers arrived; the status is known and the body is on its way.
    Head(u16),
    /// Body bytes arrived. The value is the running total, not this part's size — there is
    /// deliberately no "this part" number, because a caller that summed them would drift from the
    /// stack's own count the first time one was dropped.
    Body(usize),
    /// The transaction finished. Read the body with [`Fetch::read`].
    Done(Response),
    /// The server says the stored copy is still current: 304, no body.
    ///
    /// Its own variant rather than a status code to inspect, because it is the one response where
    /// the *absence* of a body is the answer. A caller that treated it as `Done` would replace a
    /// good cached page with nothing.
    NotModified,
    /// It failed. The platform code is kept: see the module note on certificates.
    Failed(Error),
}

/// The platform side of a fetch. See the module note on why this is a trait.
pub trait Http {
    /// Open the session over a bearer handle that is already **up**.
    fn open(&mut self, bearer: i32) -> Result<()>;
    /// Begin a GET. Replaces any transaction in flight.
    fn get(&mut self, url: &str, want_gzip: bool) -> Result<()>;
    /// One POST with a body already in memory.
    ///
    /// Defaulted to `NotSupported` so that a fake in a test only implements it when the thing
    /// under test posts — the same courtesy [`crate::image::Images::describe`] gets.
    fn post(&mut self, _url: &str, _content_type: &str, _body: &[u8]) -> Result<()> {
        Err(Error::from_code(symbian_sys::SHIM_ERR_NOT_SUPPORTED))
    }
    /// The same, conditional on a stored copy's validators. Empty strings mean unconditional.
    fn get_conditional(
        &mut self,
        url: &str,
        want_gzip: bool,
        etag: &str,
        last_modified: &str,
    ) -> Result<()>;
    /// The validators the response carried. Read after the headers arrive.
    fn validators(&mut self) -> Result<Validators>;
    /// Copy out buffered body bytes. Returns how many; 0 when none are held.
    fn read(&mut self, out: &mut [u8]) -> Result<usize>;
    /// Where the bytes came from, after any redirect the stack followed silently.
    ///
    /// Call after the transaction ends and before the next one. `Err(Error::NotReady)` when there is
    /// no transaction to ask about.
    fn effective_url(&mut self) -> Result<String>;
    /// Abandon the transaction, keeping the session.
    fn cancel(&mut self);
    /// Resume the next [`Http::get`] from a byte offset. `Ok(())` means the request will carry
    /// `Range: bytes=N-`.
    ///
    /// Default: not supported, so an implementation that cannot resume says so rather than silently
    /// fetching from the start — a caller that appended the answer to a partial file would otherwise
    /// produce a corrupt package out of two beginnings.
    fn range_from(&mut self, _offset: u64) -> Result<()> {
        Err(Error::Platform(sys::SHIM_ERR_NOT_SUPPORTED))
    }

    /// Throw the session away, so the next fetch builds a new one.
    ///
    /// # Why a session is not always reusable
    ///
    /// Measured on the handset: after a TLS handshake failed — an ECDSA certificate the 2009 stack
    /// will not negotiate — **every later HTTPS request on the same session failed with the same
    /// code**, including hosts that had worked minutes earlier, while cleartext kept working. The
    /// failure looked like it had spread from one site to the web.
    ///
    /// So a transport failure is treated as a session that may be spoiled. Rebuilding one costs a
    /// handshake; not rebuilding it cost the browser every secure page after the first bad one.
    fn reset(&mut self, bearer: i32) -> Result<()>;
}

/// [`Http`] over the shim.
#[derive(Copy, Clone, Debug, Default)]
pub struct ShimHttp;

impl Http for ShimHttp {
    fn open(&mut self, bearer: i32) -> Result<()> {
        Error::check(unsafe { sys::shim_httpc_open(bearer) }).map(|_| ())
    }

    fn get(&mut self, url: &str, want_gzip: bool) -> Result<()> {
        self.get_conditional(url, want_gzip, "", "")
    }

    fn range_from(&mut self, offset: u64) -> Result<()> {
        // SAFETY: no pointers; the shim stores the offset and consumes it on the next GET.
        Error::check(unsafe { sys::shim_httpc_range_from(offset as i64) }).map(|_| ())
    }

    fn post(&mut self, url: &str, content_type: &str, body: &[u8]) -> Result<()> {
        let u: Vec<u16> = url.encode_utf16().collect();
        // The body stays bytes all the way down. The C++ side copies it before the transaction is
        // opened, so this slice does not have to outlive the call.
        Error::check(unsafe {
            sys::shim_httpc_post(
                u.as_ptr(),
                u.len() as i32,
                content_type.as_ptr(),
                content_type.len() as i32,
                body.as_ptr(),
                body.len() as i32,
            )
        })
        .map(|_| ())
    }

    fn get_conditional(
        &mut self,
        url: &str,
        want_gzip: bool,
        etag: &str,
        last_modified: &str,
    ) -> Result<()> {
        // UTF-16 because that is what every string crossing this shim is; the C++ side narrows it
        // and rejects anything above ASCII rather than mangling it, since a mangled URL is a fetch
        // of a different page and a mangled ETag is a cache that never hits.
        let u: Vec<u16> = url.encode_utf16().collect();
        let e: Vec<u16> = etag.encode_utf16().collect();
        let m: Vec<u16> = last_modified.encode_utf16().collect();
        Error::check(unsafe {
            sys::shim_httpc_get_cond(
                u.as_ptr(),
                u.len() as i32,
                if want_gzip { 1 } else { 0 },
                e.as_ptr(),
                e.len() as i32,
                m.as_ptr(),
                m.len() as i32,
            )
        })
        .map(|_| ())
    }

    fn validators(&mut self) -> Result<Validators> {
        let mut buf = [0u16; 128];
        let mut read = |want_etag: i32| -> String {
            let n = unsafe { sys::shim_httpc_validator(want_etag, buf.as_mut_ptr(), 128) };
            if n > 0 {
                String::from_utf16_lossy(&buf[..n as usize])
            } else {
                String::new()
            }
        };
        let etag = read(1);
        let last_modified = read(0);
        Ok(Validators { etag, last_modified })
    }

    fn read(&mut self, out: &mut [u8]) -> Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        let n = unsafe { sys::shim_httpc_read(out.as_mut_ptr(), out.len() as i32) };
        Error::check(n)?;
        Ok(n as usize)
    }

    fn effective_url(&mut self) -> Result<String> {
        // 1024 units, matching the shim's own limit on a URL it will accept.
        let mut buf = [0u16; 1024];
        let n = unsafe { sys::shim_httpc_url(buf.as_mut_ptr(), buf.len() as i32) };
        Error::check(n)?;
        Ok(String::from_utf16_lossy(&buf[..n as usize]))
    }

    fn cancel(&mut self) {
        unsafe {
            sys::shim_httpc_cancel();
        }
    }

    fn reset(&mut self, bearer: i32) -> Result<()> {
        unsafe {
            sys::shim_httpc_close();
        }
        self.open(bearer)
    }
}

/// One fetch, driven by events.
///
/// Start it, feed every event to [`Self::on_event`], and read the body once [`Progress::Done`]
/// arrives.
pub struct Fetch {
    url: String,
    /// Where the bytes came from, once known. Empty until the transaction ends.
    effective: String,
    /// What the response says can be used to revalidate it next time.
    validators: Validators,
    done: bool,
    status: u16,
    total: usize,
}

impl Fetch {
    /// Begin a GET.
    ///
    /// `want_gzip` asks the server for compression. Ask for it: on this handset bandwidth is the
    /// cheap resource and RAM is not, and the answer to whether it arrives inflated is
    /// [`Flags::needs_inflate`] rather than an assumption.
    pub fn start<H: Http>(http: &mut H, url: &str, want_gzip: bool) -> Result<Self> {
        Self::start_conditional(http, url, want_gzip, "", "")
    }

    /// Begin a GET conditional on a stored copy's validators.
    ///
    /// This is the request a cache hit should make, and the reason [`super::cache`] stores the
    /// validators beside the body: the round trip still happens, so the answer is current, but a
    /// server that agrees answers [`Progress::NotModified`] with no body — which on a link metered
    /// by the kilobyte is the difference between a few hundred bytes and a megabyte.
    ///
    /// Empty validators make it an ordinary GET, so a caller with a body it cannot revalidate does
    /// not need a second code path.
    /// The same state machine, driving a POST instead of a GET.
    ///
    /// A separate constructor rather than a flag, because everything after submission is
    /// identical — the events, the body draining, the status — and the only thing that differs is
    /// what was sent. A caller that had to remember which kind of `Fetch` it was holding would be
    /// carrying a distinction that stops mattering the moment the request leaves.
    pub fn post<H: Http>(
        http: &mut H,
        url: &str,
        content_type: &str,
        body: &[u8],
    ) -> Result<Self> {
        http.post(url, content_type, body)?;
        Ok(Fetch {
            url: String::from(url),
            effective: String::new(),
            validators: Validators::default(),
            done: false,
            status: 0,
            total: 0,
        })
    }

    /// Start a GET that resumes from `offset`, for a download with a partial file already on disk.
    ///
    /// **`Progress::Head` carries the status, and the caller must look at it.** 206 means the server
    /// honoured the range and the body is the remainder, so it appends. **200 means it ignored the
    /// range and sent the whole thing**, which is a legitimate answer and the one that will corrupt a
    /// package if it is appended: the partial file has to be discarded and written from the start.
    /// There is no way to make the server behave, so the honest thing is to make the distinction
    /// impossible to miss.
    pub fn start_from<H: Http>(http: &mut H, url: &str, want_gzip: bool, offset: u64) -> Result<Self> {
        if offset > 0 {
            http.range_from(offset)?;
        }
        Self::start(http, url, want_gzip)
    }

    pub fn start_conditional<H: Http>(
        http: &mut H,
        url: &str,
        want_gzip: bool,
        etag: &str,
        last_modified: &str,
    ) -> Result<Self> {
        http.get_conditional(url, want_gzip, etag, last_modified)?;
        Ok(Fetch {
            url: String::from(url),
            effective: String::new(),
            validators: Validators::default(),
            done: false,
            status: 0,
            total: 0,
        })
    }

    /// The URL as asked for.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Where the bytes came from. Falls back to [`Self::url`] until the transaction ends.
    ///
    /// **This, not `url`, is what a relative link resolves against.** The platform follows a 301 for
    /// GET without reporting it, and a page whose links were resolved against what was typed would
    /// point them at the wrong host. The failure would present as a broken site, which is the kind
    /// of bug that gets blamed on the site.
    ///
    /// Two redirects measured on the handset, both of which change where a relative link points:
    /// `http://google.com/` lands on `http://www.google.com/` — the host changes and the scheme does
    /// **not**, so the stack follows the literal `Location` rather than upgrading anything — and
    /// `https://en.m.wikipedia.org/wiki/Symbian` lands on `https://en.wikipedia.org/wiki/Symbian`,
    /// which is a mobile subdomain refusing to serve the mobile site.
    pub fn effective_url(&self) -> &str {
        if self.effective.is_empty() {
            &self.url
        } else {
            &self.effective
        }
    }

    /// Whether the stack redirected us somewhere else.
    pub fn was_redirected(&self) -> bool {
        !self.effective.is_empty() && self.effective != self.url
    }

    /// What this response can be revalidated with next time. Known once the headers arrive.
    pub fn validators(&self) -> &Validators {
        &self.validators
    }

    pub fn is_done(&self) -> bool {
        self.done
    }

    /// Feed a platform event, and ask the platform where the bytes came from once it is over.
    ///
    /// Takes the [`Http`] because the effective URL can only be read while the transaction is still
    /// open — the next GET replaces it — so the one moment to ask is the completion itself.
    pub fn on_event_with<H: Http>(&mut self, http: &mut H, ev: &RawEvent) -> Progress {
        let p = self.on_event(ev);
        // Validators come off the headers, so this is the moment they exist — and they are read
        // even on a 304, because a server is allowed to send a fresh ETag with one and a cache that
        // ignored it would keep revalidating against a stale token.
        if matches!(p, Progress::Head(_) | Progress::NotModified) {
            if let Ok(v) = http.validators() {
                if v.any() {
                    self.validators = v;
                }
            }
        }
        if matches!(p, Progress::Done(_) | Progress::Failed(_) | Progress::NotModified) {
            if let Ok(u) = http.effective_url() {
                if !u.is_empty() {
                    self.effective = u;
                }
            }
        }
        p
    }

    /// Feed a platform event.
    pub fn on_event(&mut self, ev: &RawEvent) -> Progress {
        if self.done {
            // A completion for a fetch already finished, which is what a cancel racing a
            // completion looks like. Reporting it twice would have a caller parse a body it
            // already parsed.
            return Progress::Idle;
        }
        match ev.kind {
            sys::SHIM_EV_HTTP_HEAD => {
                self.status = clamp_status(ev.a);
                if self.status == 304 {
                    return Progress::NotModified;
                }
                Progress::Head(self.status)
            }
            sys::SHIM_EV_HTTP_BODY => {
                self.total = ev.a.max(0) as usize;
                Progress::Body(self.total)
            }
            sys::SHIM_EV_HTTP_DONE => {
                self.done = true;
                if ev.status != sys::SHIM_OK {
                    return Progress::Failed(Error::from_code(ev.status));
                }
                // The status code comes off this event rather than off the earlier HEAD: a
                // transaction can complete without ever reporting headers, and reading it from
                // here means one source of truth for what the response was.
                self.status = clamp_status(ev.a);
                self.total = ev.b.max(0) as usize;
                if self.status == 304 {
                    // No body, and the stored copy stands. Reported as its own thing rather than as
                    // a zero-length Done, which a caller would store over a good page.
                    return Progress::NotModified;
                }
                Progress::Done(Response {
                    status: self.status,
                    total: self.total,
                    parts: ev.d.max(0) as u32,
                    flags: Flags(ev.c),
                })
            }
            _ => Progress::Idle,
        }
    }

    /// Copy out buffered body bytes. Call until it returns 0.
    pub fn read<H: Http>(&mut self, http: &mut H, out: &mut [u8]) -> Result<usize> {
        http.read(out)
    }

    /// Abandon it. This is Back being pressed.
    pub fn cancel<H: Http>(&mut self, http: &mut H) {
        http.cancel();
        self.done = true;
    }
}

/// An HTTP status the platform reported, kept in range.
///
/// Not defensive tidying: the field is an `i32` carrying whatever the stack put there, and a
/// transaction that failed before a response has 0 in it. Clamping keeps a nonsense value from
/// reaching a caller as a plausible one.
fn clamp_status(raw: i32) -> u16 {
    if !(0..=599).contains(&raw) {
        0
    } else {
        raw as u16
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    /// Replays event sequences. The point of the trait: none of this needs a phone.
    #[derive(Default)]
    struct FakeHttp {
        opened: Option<i32>,
        asked: Vec<(String, bool)>,
        /// The conditional headers each request carried.
        conditions: Vec<(String, String)>,
        body: Vec<u8>,
        cancels: u32,
        /// What the platform will claim the bytes came from.
        effective: String,
        /// What the platform will claim the response carried.
        validators: Validators,
        /// How many times the session was thrown away.
        resets: u32,
    }

    impl Http for FakeHttp {
        fn open(&mut self, bearer: i32) -> Result<()> {
            self.opened = Some(bearer);
            Ok(())
        }
        fn get(&mut self, url: &str, want_gzip: bool) -> Result<()> {
            self.get_conditional(url, want_gzip, "", "")
        }
        fn get_conditional(
            &mut self,
            url: &str,
            want_gzip: bool,
            etag: &str,
            last_modified: &str,
        ) -> Result<()> {
            self.asked.push((String::from(url), want_gzip));
            self.conditions.push((String::from(etag), String::from(last_modified)));
            Ok(())
        }
        fn validators(&mut self) -> Result<Validators> {
            Ok(self.validators.clone())
        }
        fn reset(&mut self, bearer: i32) -> Result<()> {
            self.resets += 1;
            self.opened = Some(bearer);
            Ok(())
        }
        fn read(&mut self, out: &mut [u8]) -> Result<usize> {
            let n = core::cmp::min(out.len(), self.body.len());
            out[..n].copy_from_slice(&self.body[..n]);
            self.body.drain(..n);
            Ok(n)
        }
        fn effective_url(&mut self) -> Result<String> {
            Ok(self.effective.clone())
        }
        fn cancel(&mut self) {
            self.cancels += 1;
        }
    }

    fn ev(kind: i32, status: i32, a: i32, b: i32, c: i32, d: i32) -> RawEvent {
        RawEvent { kind, status, a, b, c, d, ..Default::default() }
    }

    #[test]
    fn head_then_body_then_done() {
        let mut h = FakeHttp::default();
        let mut f = Fetch::start(&mut h, "https://example.com/", true).unwrap();
        assert_eq!(h.asked, vec![(String::from("https://example.com/"), true)]);

        assert_eq!(f.on_event(&ev(sys::SHIM_EV_HTTP_HEAD, 0, 200, 0, 0, 0)), Progress::Head(200));
        assert_eq!(f.on_event(&ev(sys::SHIM_EV_HTTP_BODY, 0, 512, 0, 0, 0)), Progress::Body(512));
        assert_eq!(f.on_event(&ev(sys::SHIM_EV_HTTP_BODY, 0, 900, 0, 0, 0)), Progress::Body(900));

        let done = ev(sys::SHIM_EV_HTTP_DONE, sys::SHIM_OK, 200, 900, 0, 2);
        match f.on_event(&done) {
            Progress::Done(r) => {
                assert_eq!(r.status, 200);
                assert_eq!(r.total, 900);
                assert_eq!(r.parts, 2);
                assert!(r.is_ok());
            }
            other => panic!("expected Done, got {:?}", other),
        }
        assert!(f.is_done());
    }

    /// A body arriving in several parts reports the stack's running total, never a sum of its own.
    #[test]
    fn total_comes_from_the_stack_not_from_summing() {
        let mut h = FakeHttp::default();
        let mut f = Fetch::start(&mut h, "http://x/", false).unwrap();
        f.on_event(&ev(sys::SHIM_EV_HTTP_BODY, 0, 100, 0, 0, 0));
        f.on_event(&ev(sys::SHIM_EV_HTTP_BODY, 0, 250, 0, 0, 0));
        match f.on_event(&ev(sys::SHIM_EV_HTTP_DONE, sys::SHIM_OK, 200, 250, 0, 2)) {
            Progress::Done(r) => assert_eq!(r.total, 250),
            other => panic!("expected Done, got {:?}", other),
        }
    }

    /// The failure path keeps the platform code, because that code is the diagnosis.
    #[test]
    fn failure_keeps_the_code() {
        let mut h = FakeHttp::default();
        let mut f = Fetch::start(&mut h, "https://expired.example/", false).unwrap();
        f.on_event(&ev(sys::SHIM_EV_HTTP_HEAD, 0, 0, 0, 0, 0));
        match f.on_event(&ev(sys::SHIM_EV_HTTP_DONE, -7548, 0, 0, 0, 0)) {
            Progress::Failed(e) => assert_eq!(e.code(), -7548),
            other => panic!("expected Failed, got {:?}", other),
        }
        assert!(f.is_done());
    }

    /// A second completion after cancel is ignored rather than parsed twice.
    #[test]
    fn completion_after_cancel_is_ignored() {
        let mut h = FakeHttp::default();
        let mut f = Fetch::start(&mut h, "http://x/", false).unwrap();
        f.cancel(&mut h);
        assert_eq!(h.cancels, 1);
        assert_eq!(
            f.on_event(&ev(sys::SHIM_EV_HTTP_DONE, sys::SHIM_OK, 200, 10, 0, 1)),
            Progress::Idle
        );
    }

    /// The two gzip flags mean different things, and only their combination answers the question
    /// a caller has.
    #[test]
    fn needs_inflate_is_declared_and_not_decoded() {
        let decoded = Flags(sys::SHIM_HTTP_GZIP);
        assert!(decoded.gzip());
        assert!(!decoded.needs_inflate(), "header alone means the stack inflated it");

        let raw = Flags(sys::SHIM_HTTP_GZIP | sys::SHIM_HTTP_GZIP_MAGIC);
        assert!(raw.needs_inflate());

        // Magic without the header: a server sending gzip bytes without saying so. Not our job to
        // guess, and reporting it as needing inflation would be a guess.
        let sneaky = Flags(sys::SHIM_HTTP_GZIP_MAGIC);
        assert!(!sneaky.needs_inflate());
    }

    // ------------------------------------------------------------------------- body --

    /// A gzip member wrapping a run, built here because the crate has no compressor and the
    /// interesting cases are about size. Mirrors the helper in `symbian_crypto::inflate`'s tests.
    fn gzip_of_run(byte: u8, runs: usize) -> (Vec<u8>, Vec<u8>) {
        // One literal then `runs` copies of length 258 at distance 1, using the fixed code.
        let mut out: Vec<u8> = Vec::new();
        let mut acc: u32 = 0;
        let mut have: u32 = 0;
        let bits = |v: u32, n: u32, out: &mut Vec<u8>, acc: &mut u32, have: &mut u32| {
            *acc |= (v & ((1 << n) - 1)) << *have;
            *have += n;
            while *have >= 8 {
                out.push((*acc & 0xFF) as u8);
                *acc >>= 8;
                *have -= 8;
            }
        };
        let fixed = |sym: u32| -> (u32, u32) {
            match sym {
                0..=143 => (0x30 + sym, 8),
                144..=255 => (0x190 + (sym - 144), 9),
                256..=279 => (sym - 256, 7),
                _ => (0xC0 + (sym - 280), 8),
            }
        };
        let code = |c: u32, n: u32, out: &mut Vec<u8>, acc: &mut u32, have: &mut u32| {
            for i in (0..n).rev() {
                bits((c >> i) & 1, 1, out, acc, have);
            }
        };

        bits(1, 1, &mut out, &mut acc, &mut have); // BFINAL
        bits(1, 2, &mut out, &mut acc, &mut have); // fixed
        let (c, n) = fixed(byte as u32);
        code(c, n, &mut out, &mut acc, &mut have);
        for _ in 0..runs {
            let (c, n) = fixed(285);
            code(c, n, &mut out, &mut acc, &mut have);
            code(0, 5, &mut out, &mut acc, &mut have);
        }
        let (c, n) = fixed(256);
        code(c, n, &mut out, &mut acc, &mut have);
        if have > 0 {
            out.push((acc & 0xFF) as u8);
        }

        let plain = vec![byte; 1 + 258 * runs];
        let mut crc = 0xFFFF_FFFFu32;
        for &b in &plain {
            crc ^= b as u32;
            for _ in 0..8 {
                crc = if crc & 1 != 0 { 0xEDB8_8320 ^ (crc >> 1) } else { crc >> 1 };
            }
        }
        let mut member = vec![0x1F, 0x8B, 8, 0, 0, 0, 0, 0, 0, 0];
        member.extend_from_slice(&out);
        member.extend_from_slice(&(!crc).to_le_bytes());
        member.extend_from_slice(&(plain.len() as u32).to_le_bytes());
        (member, plain)
    }

    /// A body the response says is compressed comes out decoded.
    #[test]
    fn a_gzip_body_is_inflated() {
        let (member, plain) = gzip_of_run(b'A', 300); // ~77 KB out, two windows
        let mut body = Body::with_cap(1 << 20);
        // Fed in pieces, because that is how it arrives — including one-byte pieces.
        body.push(&member[..1]);
        body.push(&member[1..2]);
        body.push(&member[2..]);
        assert_eq!(body.len(), member.len());

        let mut out: Vec<u8> = Vec::new();
        let flags = Flags(sys::SHIM_HTTP_GZIP | sys::SHIM_HTTP_GZIP_MAGIC);
        let n = body.decode_to(flags, 1 << 20, &mut out).expect("inflate");
        assert_eq!(n, plain.len());
        assert_eq!(out, plain);
    }

    /// A body the stack already inflated passes through untouched.
    ///
    /// The case that makes both flags necessary: `Content-Encoding: gzip` can survive on a response
    /// whose bytes are no longer compressed, and inflating those would be garbage.
    #[test]
    fn an_already_inflated_body_is_passed_through() {
        let mut body = Body::with_cap(1 << 20);
        body.push(b"<html>hello</html>");
        let mut out: Vec<u8> = Vec::new();
        // gzip declared, magic absent.
        let n = body.decode_to(Flags(sys::SHIM_HTTP_GZIP), 1 << 20, &mut out).unwrap();
        assert_eq!(n, 18);
        assert_eq!(out, b"<html>hello</html>");
    }

    /// No encoding at all is the same pass-through.
    #[test]
    fn a_plain_body_is_passed_through() {
        let mut body = Body::with_cap(1 << 20);
        body.push(b"plain");
        let mut out: Vec<u8> = Vec::new();
        body.decode_to(Flags(0), 1 << 20, &mut out).unwrap();
        assert_eq!(out, b"plain");
    }

    /// The decoded bound is the caller's, and exceeding it is an error rather than an allocation.
    #[test]
    fn max_out_bounds_the_decoded_size() {
        let (member, plain) = gzip_of_run(b'A', 100);
        let mut body = Body::with_cap(1 << 20);
        body.push(&member);
        let mut out: Vec<u8> = Vec::new();
        let flags = Flags(sys::SHIM_HTTP_GZIP | sys::SHIM_HTTP_GZIP_MAGIC);
        assert_eq!(
            body.decode_to(flags, plain.len() - 1, &mut out),
            Err(Error::Overflow),
            "a page that expands past what the caller will hold must not be allocated"
        );
    }

    /// A body past the cap is short, and says so, rather than growing without limit.
    #[test]
    fn a_body_over_its_cap_reports_what_it_dropped() {
        let mut body = Body::with_cap(4);
        body.push(b"abcdefgh");
        assert_eq!(body.len(), 4);
        assert_eq!(body.dropped(), 4);
        assert_eq!(body.raw(), b"abcd");
    }

    /// A corrupt compressed body is an error the caller can act on, not silent garbage.
    #[test]
    fn a_corrupt_gzip_body_is_reported() {
        let (mut member, _) = gzip_of_run(b'A', 50);
        let crc_at = member.len() - 8;
        member[crc_at] ^= 0xFF;
        let mut body = Body::with_cap(1 << 20);
        body.push(&member);
        let mut out: Vec<u8> = Vec::new();
        let flags = Flags(sys::SHIM_HTTP_GZIP | sys::SHIM_HTTP_GZIP_MAGIC);
        assert_eq!(body.decode_to(flags, 1 << 20, &mut out), Err(Error::Platform(-20)));
    }

    /// A silent redirect must be visible, because relative links resolve against where the bytes
    /// came from and not against what was typed.
    #[test]
    fn a_silent_redirect_is_reported() {
        let mut h = FakeHttp { effective: String::from("https://www.google.com/"), ..Default::default() };
        let mut f = Fetch::start(&mut h, "http://google.com/", false).unwrap();

        assert_eq!(f.effective_url(), "http://google.com/", "unknown until it ends");
        assert!(!f.was_redirected());

        f.on_event_with(&mut h, &ev(sys::SHIM_EV_HTTP_DONE, sys::SHIM_OK, 200, 10, 0, 1));
        assert_eq!(f.effective_url(), "https://www.google.com/");
        assert!(f.was_redirected());
        assert_eq!(f.url(), "http://google.com/", "what was asked for is still available");
    }

    /// No redirect: the effective URL is the requested one, and nothing claims otherwise.
    #[test]
    fn without_a_redirect_nothing_is_reported() {
        let mut h = FakeHttp { effective: String::from("https://example.com/"), ..Default::default() };
        let mut f = Fetch::start(&mut h, "https://example.com/", false).unwrap();
        f.on_event_with(&mut h, &ev(sys::SHIM_EV_HTTP_DONE, sys::SHIM_OK, 200, 10, 0, 1));
        assert_eq!(f.effective_url(), "https://example.com/");
        assert!(!f.was_redirected());
    }

    /// A platform that will not say falls back to the requested URL rather than to nothing.
    #[test]
    fn an_unknown_effective_url_falls_back() {
        let mut h = FakeHttp { effective: String::new(), ..Default::default() };
        let mut f = Fetch::start(&mut h, "https://example.com/x", false).unwrap();
        f.on_event_with(&mut h, &ev(sys::SHIM_EV_HTTP_DONE, sys::SHIM_OK, 200, 0, 0, 0));
        assert_eq!(f.effective_url(), "https://example.com/x");
        assert!(!f.was_redirected());
    }

    // ------------------------------------------------------------------ revalidation --

    /// A conditional request carries the stored validators, and an unconditional one carries none.
    #[test]
    fn a_conditional_request_sends_what_it_was_given() {
        let mut h = FakeHttp::default();
        Fetch::start_conditional(&mut h, "https://e.com/", true, "\"v1\"", "Sat, 23 Aug 2026 00:00:00 GMT")
            .unwrap();
        assert_eq!(h.conditions[0], (String::from("\"v1\""), String::from("Sat, 23 Aug 2026 00:00:00 GMT")));

        Fetch::start(&mut h, "https://e.com/x", false).unwrap();
        assert_eq!(h.conditions[1], (String::new(), String::new()), "plain GET sends neither");
    }

    /// 304 is its own outcome, not a zero-length success.
    ///
    /// The distinction is the whole feature: a caller that saw `Done` with no body would replace a
    /// good cached page with nothing, which is the worst of both — the round trip was paid for and
    /// the page was lost.
    #[test]
    fn a_304_is_not_modified_and_not_an_empty_page() {
        let mut h = FakeHttp::default();
        let mut f =
            Fetch::start_conditional(&mut h, "https://e.com/", true, "\"v1\"", "").unwrap();

        assert_eq!(
            f.on_event_with(&mut h, &ev(sys::SHIM_EV_HTTP_HEAD, 0, 304, 0, 0, 0)),
            Progress::NotModified
        );
        assert_eq!(
            f.on_event_with(&mut h, &ev(sys::SHIM_EV_HTTP_DONE, sys::SHIM_OK, 304, 0, 0, 0)),
            Progress::NotModified
        );
        assert!(f.is_done());
    }

    /// A 200 answering a conditional request means the copy was stale, and it is an ordinary Done.
    #[test]
    fn a_200_answering_a_conditional_request_is_a_fresh_page() {
        let mut h = FakeHttp::default();
        let mut f = Fetch::start_conditional(&mut h, "https://e.com/", true, "\"old\"", "").unwrap();
        f.on_event_with(&mut h, &ev(sys::SHIM_EV_HTTP_HEAD, 0, 200, 0, 0, 0));
        match f.on_event_with(&mut h, &ev(sys::SHIM_EV_HTTP_DONE, sys::SHIM_OK, 200, 42, 0, 1)) {
            Progress::Done(r) => assert_eq!(r.total, 42),
            other => panic!("expected Done, got {other:?}"),
        }
    }

    /// The validators come off the headers, ready to be stored beside the body.
    #[test]
    fn the_response_validators_are_captured() {
        let mut h = FakeHttp {
            validators: Validators {
                etag: String::from("\"abc\""),
                last_modified: String::from("Sat, 23 Aug 2026 00:00:00 GMT"),
            },
            ..Default::default()
        };
        let mut f = Fetch::start(&mut h, "https://e.com/", true).unwrap();
        assert!(!f.validators().any(), "unknown until the headers arrive");

        f.on_event_with(&mut h, &ev(sys::SHIM_EV_HTTP_HEAD, 0, 200, 0, 0, 0));
        assert_eq!(f.validators().etag, "\"abc\"");
        assert!(f.validators().any());
    }

    /// A server may send a fresh ETag with a 304, and ignoring it would mean revalidating against a
    /// stale token forever.
    #[test]
    fn a_304_can_refresh_the_validator() {
        let mut h = FakeHttp {
            validators: Validators { etag: String::from("\"v2\""), last_modified: String::new() },
            ..Default::default()
        };
        let mut f = Fetch::start_conditional(&mut h, "https://e.com/", true, "\"v1\"", "").unwrap();
        f.on_event_with(&mut h, &ev(sys::SHIM_EV_HTTP_HEAD, 0, 304, 0, 0, 0));
        assert_eq!(f.validators().etag, "\"v2\"");
    }

    /// A response with no validators says so, rather than reporting empty strings as usable.
    #[test]
    fn no_validators_is_reported_as_none() {
        let mut h = FakeHttp::default();
        let mut f = Fetch::start(&mut h, "https://e.com/", true).unwrap();
        f.on_event_with(&mut h, &ev(sys::SHIM_EV_HTTP_HEAD, 0, 200, 0, 0, 0));
        assert!(!f.validators().any(), "a page that cannot be revalidated must say so");
    }

    /// A session is rebuilt on demand, and the caller can tell that it was.
    #[test]
    fn a_session_can_be_thrown_away() {
        let mut h = FakeHttp::default();
        h.open(7).unwrap();
        assert_eq!(h.resets, 0);
        h.reset(7).unwrap();
        assert_eq!(h.resets, 1);
        assert_eq!(h.opened, Some(7), "and comes back on the same bearer");
    }

    /// A nonsense status never reaches a caller looking plausible.
    #[test]
    fn status_is_clamped() {
        assert_eq!(clamp_status(200), 200);
        assert_eq!(clamp_status(0), 0);
        assert_eq!(clamp_status(-1), 0);
        assert_eq!(clamp_status(70000), 0);
    }

    #[test]
    fn read_drains_the_buffer() {
        let mut h = FakeHttp { body: vec![b'h', b'i', b'!'], ..Default::default() };
        let mut f = Fetch::start(&mut h, "http://x/", false).unwrap();
        let mut buf = [0u8; 2];
        assert_eq!(f.read(&mut h, &mut buf).unwrap(), 2);
        assert_eq!(&buf, b"hi");
        assert_eq!(f.read(&mut h, &mut buf).unwrap(), 1);
        assert_eq!(f.read(&mut h, &mut buf).unwrap(), 0);
    }
}
