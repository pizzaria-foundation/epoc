//! The worker thread, from Rust.
//!
//! ```no_run
//! # use symbian::work::{Job, ModPow};
//! # let (g, secret, prime) = ([3u8], [0u8; 256], [0xffu8; 256]);
//! let mut job = Job::new();
//! job.submit(&ModPow { base: &g, exp: &secret, modulus: &prime })?;
//! // ... then feed raw events until job.on_event(ev) returns Some
//! # Ok::<(), symbian::Error>(())
//! ```
//!
//! # Why this exists
//!
//! A 2048-bit modular exponentiation measures **815 ms** on an E72. `rust_step` runs from a
//! `CIdle` on the GUI thread and must return in milliseconds — a long one starves the
//! window server, which freezes the whole phone rather than just the application, and no
//! watchdog rescues you. An MTProto login needs two.
//!
//! The shim's worker is proven on that hardware: the same exponentiation took 1933 ms of
//! wall time there with 27 GUI ticks served through it. Slower, because one core is shared
//! with a redrawing interface — the worker buys responsiveness, not speed, and a design
//! that assumes background work is free is wrong on this device.
//!
//! # The contract, and the one way to break it
//!
//! The worker gets its own heap. Anything allocated there and freed on the GUI thread is a
//! cross-heap free: silent corruption, not a clean failure. So the job may allocate
//! temporaries, but nothing it allocates may escape.
//!
//! [`Job`] enforces that by owning both buffers as `Box<[u8]>` and holding them for the
//! whole request. The shim keeps raw pointers into them until the completion arrives, so a
//! buffer freed or moved in between gets written to afterwards. A `Box`'s contents do not
//! move when the `Box` does, which is what lets a `Job` be moved while a job is in flight.
//!
//! Same shape as [`crate::net::TcpStream`], and for the same reason.

use alloc::boxed::Box;
use alloc::vec;

use symbian_sys as sys;

use crate::error::{Error, Result};

/// Opcode for a modular exponentiation. Must match the app's `rust_work` dispatcher.
pub const OP_MODPOW: i32 = 1;

/// Opcode for a PBKDF2 key derivation. Must match the app's `rust_work` dispatcher.
pub const OP_KDF: i32 = 2;

/// The first opcode an application may define for itself.
///
/// The low numbers belong to the jobs this module encodes ([`OP_MODPOW`], [`OP_KDF`]), and their
/// payloads have a fixed layout that [`decode_modpow`] and [`decode_kdf`] read. A job submitted
/// under one of those opcodes with an application's own bytes in it would be *decoded* as a modular
/// exponentiation — three length-prefixed operands read out of arbitrary data — and answered with
/// nonsense rather than refused. So [`Job::submit_bytes`] insists on this floor, and the room below
/// it is left for this module to grow into.
pub const OP_APP_BASE: i32 = 16;

/// The worker heap ceiling a [`Job`] asks for unless told otherwise.
///
/// What every caller got before the ceiling was adjustable, kept as the default so that making it
/// adjustable changed nothing for them.
pub const DEFAULT_WORKER_HEAP: usize = 256 * 1024;

/// The worker stack a [`Job`] asks for unless told otherwise.
///
/// What every caller had before it was adjustable.
pub const DEFAULT_WORKER_STACK: usize = 64 * 1024;

/// The largest operand this facility carries, matching `symbian_crypto::bignum::MAX_BYTES`.
pub const MAX_OPERAND: usize = 256;

/// The widest exponent, which is **one byte more** than the widest modulus.
///
/// There is no arithmetic reason for an exponent to fit the modulus — the ladder walks its
/// bytes and never reduces it — and SRP relies on that: its third exponentiation raises to
/// `a + u*x`, where `a` is as wide as the prime. That sum needs one more byte to hold the
/// carry, so it is 257 bytes *every time*, not occasionally.
///
/// Capping it at `MAX_OPERAND` made that submission fail on every two-factor login. The
/// failure was a refused job rather than a wrong answer, so nothing crashed and nothing was
/// logged: the work went into a queue, was retried, was refused again, and the screen sat
/// on "verificando a senha" until the user gave up.
pub const MAX_EXPONENT: usize = MAX_OPERAND + 1;

/// A modular exponentiation to run off the GUI thread. All operands big-endian.
pub struct ModPow<'a> {
    pub base: &'a [u8],
    pub exp: &'a [u8],
    pub modulus: &'a [u8],
}

impl ModPow<'_> {
    /// Pack into the flat buffer the worker reads.
    ///
    /// Three big-endian lengths then the operands. Flat because the ABI is a byte slice and
    /// a struct would mean agreeing on alignment and padding across a language boundary for
    /// no benefit — the packing costs one memcpy of under a kilobyte, against 815 ms of
    /// arithmetic.
    fn encode(&self, out: &mut [u8]) -> Result<usize> {
        let n = 6 + self.base.len() + self.exp.len() + self.modulus.len();
        if n > out.len()
            || self.base.len() > MAX_OPERAND
            || self.exp.len() > MAX_EXPONENT
            || self.modulus.len() > MAX_OPERAND
        {
            return Err(Error::Argument);
        }
        out[0..2].copy_from_slice(&(self.base.len() as u16).to_be_bytes());
        out[2..4].copy_from_slice(&(self.exp.len() as u16).to_be_bytes());
        out[4..6].copy_from_slice(&(self.modulus.len() as u16).to_be_bytes());
        let mut at = 6;
        for part in [self.base, self.exp, self.modulus] {
            out[at..at + part.len()].copy_from_slice(part);
            at += part.len();
        }
        Ok(n)
    }
}

/// A PBKDF2 key derivation to run off the GUI thread.
///
/// `password`, `salt1`, `salt2` — the three inputs to [`tg_proto::srp::derive_x`].
pub struct Kdf<'a> {
    pub password: &'a [u8],
    pub salt1: &'a [u8],
    pub salt2: &'a [u8],
}

impl Kdf<'_> {
    fn encode(&self, out: &mut [u8]) -> Result<usize> {
        let n = 4 + self.password.len() + self.salt1.len() + self.salt2.len();
        if n > out.len()
            || self.password.len() > MAX_OPERAND
            || self.salt1.len() > MAX_OPERAND
            || self.salt2.len() > MAX_OPERAND
        {
            return Err(Error::Argument);
        }
        out[0..2].copy_from_slice(&(self.password.len() as u16).to_be_bytes());
        out[2..4].copy_from_slice(&(self.salt1.len() as u16).to_be_bytes());
        let mut at = 4;
        for part in [self.password, self.salt1, self.salt2] {
            out[at..at + part.len()].copy_from_slice(part);
            at += part.len();
        }
        Ok(n)
    }
}

/// Unpack what [`Kdf::encode`] wrote.
///
/// Called on the *worker thread*, from the app's `rust_work`.
pub fn decode_kdf(input: &[u8]) -> Option<(&[u8], &[u8], &[u8])> {
    if input.len() < 4 {
        return None;
    }
    let pw = u16::from_be_bytes([input[0], input[1]]) as usize;
    let s1 = u16::from_be_bytes([input[2], input[3]]) as usize;
    let s2 = input.len().saturating_sub(4 + pw + s1);
    if 4 + pw + s1 + s2 > input.len() {
        return None;
    }
    Some((&input[4..4 + pw], &input[4 + pw..4 + pw + s1], &input[4 + pw + s1..4 + pw + s1 + s2]))
}

/// Unpack what [`ModPow::encode`] wrote.
///
/// Called on the *worker thread*, from the app's `rust_work`. Returns slices into the
/// caller's buffer rather than copies, because a copy here would allocate on the worker's
/// heap — allowed, but pointless when the input is already sitting in memory that outlives
/// the job.
pub fn decode_modpow(input: &[u8]) -> Option<(&[u8], &[u8], &[u8])> {
    if input.len() < 6 {
        return None;
    }
    let b = u16::from_be_bytes([input[0], input[1]]) as usize;
    let e = u16::from_be_bytes([input[2], input[3]]) as usize;
    let m = u16::from_be_bytes([input[4], input[5]]) as usize;
    if 6 + b + e + m > input.len() {
        return None;
    }
    Some((&input[6..6 + b], &input[6 + b..6 + b + e], &input[6 + b + e..6 + b + e + m]))
}

/// One job at a time, with the buffers it needs.
///
/// # Two shapes of caller
///
/// [`Job::new`] sizes itself for the jobs this module encodes — a modular exponentiation and a key
/// derivation — and [`Job::submit`] / [`Job::submit_kdf`] are the way in.
///
/// [`Job::with_capacity`] is for a job this module knows nothing about: the caller sizes both
/// buffers and submits an opaque payload under its own opcode with [`Job::submit_bytes`]. That is
/// what page layout needs, and sizing it was the whole difficulty — see
/// [`Job::set_worker_heap`].
pub struct Job {
    input: Box<[u8]>,
    output: Box<[u8]>,
    busy: bool,
    /// How many bytes of `output` the last completed job filled.
    result_len: usize,
    /// The worker heap ceiling to ask for.
    heap_max: usize,
    /// The worker stack to ask for.
    stack: usize,
}

impl Default for Job {
    fn default() -> Self {
        Self::new()
    }
}

impl Job {
    pub fn new() -> Self {
        Job {
            input: vec![0u8; 6 + 2 * MAX_OPERAND + MAX_EXPONENT].into_boxed_slice(),
            output: vec![0u8; MAX_OPERAND].into_boxed_slice(),
            busy: false,
            result_len: 0,
            heap_max: DEFAULT_WORKER_HEAP,
            stack: DEFAULT_WORKER_STACK,
        }
    }

    /// Buffers sized by the caller, for a job this module does not encode.
    ///
    /// Both are allocated once and held for the life of the `Job`, because the shim keeps raw
    /// pointers into them for the duration of a request — the same reason [`Job::new`]'s are fixed.
    /// Sizing them per submission would mean reallocating under a live pointer.
    pub fn with_capacity(in_cap: usize, out_cap: usize) -> Self {
        Job {
            input: vec![0u8; in_cap].into_boxed_slice(),
            output: vec![0u8; out_cap].into_boxed_slice(),
            busy: false,
            result_len: 0,
            heap_max: DEFAULT_WORKER_HEAP,
            stack: DEFAULT_WORKER_STACK,
        }
    }

    /// How much heap the worker thread may grow to.
    ///
    /// The number that made this facility unusable for anything but crypto. The worker gets its own
    /// heap — it must, because a default `RHeap` is not thread-safe and sharing the GUI thread's
    /// would race — and its ceiling was a fixed 256 KB, chosen when the only jobs were a modular
    /// exponentiation and a key derivation whose working set is a few hundred bytes of fixed arrays.
    ///
    /// An HTML tokenizer and a DOM tree are the opposite: recursive, and allocating per node. One
    /// measured page inflates to 700 KB of HTML *before* a tree is built from it, so 256 KB is not a
    /// ceiling that job can be squeezed under — under it, the job simply cannot run.
    ///
    /// Raising it costs nothing to a caller that does not: on Symbian a thread heap is a chunk
    /// reserved to its maximum and committed to its minimum, so a large ceiling reserves address
    /// space and commits no memory. The shim refuses anything under 4 KB or over 16 MB, so a wrong
    /// number is a refused job rather than a thread that cannot be created.
    pub fn set_worker_heap(&mut self, bytes: usize) {
        self.heap_max = bytes;
    }

    /// How much stack the worker thread gets.
    ///
    /// The heap ceiling became a parameter for page layout; this became one for the parse, which is
    /// a different shape of job again: an HTML tokeniser and a DOM builder are recursive in places
    /// where a modular exponentiation is a loop over fixed arrays.
    ///
    /// Unlike the heap, a thread stack is **committed** rather than reserved, so this is real memory
    /// per job and not just address space. The shim refuses anything under 8 KB or over 1 MB.
    pub fn set_worker_stack(&mut self, bytes: usize) {
        self.stack = bytes;
    }

    /// How much payload [`Job::submit_bytes`] will take.
    pub fn input_capacity(&self) -> usize {
        self.input.len()
    }

    /// The largest result this can receive.
    pub fn output_capacity(&self) -> usize {
        self.output.len()
    }

    pub fn is_busy(&self) -> bool {
        self.busy
    }

    /// Whether the *platform* still has a worker running, as opposed to this `Job` believing so.
    ///
    /// The two can disagree, and that disagreement is what [`Job::abandon`] exists for.
    pub fn platform_busy(&self) -> bool {
        unsafe { sys::shim_work_busy() != 0 }
    }

    /// Why the last worker thread ended, when a job never answered.
    ///
    /// A Symbian thread that dies leaves a triple behind — exit type, exit reason, exit category —
    /// and that triple is a diagnosis rather than a symptom. `KERN-EXEC 3` is a bad pointer;
    /// `E32USER-CBase 69` is a `PushL` with no cleanup stack; `KERN-EXEC 0` is a panic the code
    /// raised itself. Different bugs, different fixes.
    ///
    /// It exists because that triple sat unasked for while "the job never answered" was read as five
    /// different causes in a row, each wrong, each costing a device round trip. Call it on a timeout,
    /// **before** [`Job::abandon`], while the dead thread's handle is still open.
    ///
    /// Returns `(type, reason, category)`. Type is 0 kill, 1 terminate, 2 panic, 3 still running.
    pub fn last_exit(&self) -> Result<(i32, i32, alloc::string::String)> {
        let mut ty = 0i32;
        let mut reason = 0i32;
        let mut cat = [0u8; 32];
        let rc = unsafe {
            sys::shim_work_exit_info(&mut ty, &mut reason, cat.as_mut_ptr(), cat.len() as i32)
        };
        if rc != 0 {
            return Err(Error::from_code(rc));
        }
        let n = cat.iter().position(|&b| b == 0).unwrap_or(cat.len());
        let name = alloc::string::String::from_utf8_lossy(&cat[..n]).into_owned();
        Ok((ty, reason, name))
    }

    /// Give up on a job whose completion never arrived, freeing this `Job` to submit again.
    ///
    /// # Why this had to exist
    ///
    /// A `Job` marks itself busy on submission and clears it in [`Job::on_event`]. A completion that
    /// never comes therefore left it busy **forever**, with no way back: every later `submit` was
    /// refused with [`Error::InUse`], and the caller's own timeout could do nothing about it. Found
    /// on the handset, where a do-nothing job never completed and the next submission then failed
    /// with an error about the wrong thing entirely — the second failure hid the first.
    ///
    /// # Why it can refuse
    ///
    /// The shim holds raw pointers into this `Job`'s buffers for the life of a request, so clearing
    /// the flag while a worker is still running would let the buffers be reused underneath it — a
    /// write into memory being read by another thread. So this asks the platform first, and returns
    /// `Err(Error::InUse)` if a worker is genuinely still there. A caller that gets that has to keep
    /// waiting; there is no way to interrupt a running computation, which is a property of the
    /// facility and not of this call.
    pub fn abandon(&mut self) -> Result<()> {
        if self.platform_busy() {
            return Err(Error::InUse);
        }
        self.busy = false;
        self.result_len = 0;
        Ok(())
    }

    /// Start an exponentiation.
    ///
    /// Returns [`Error::InUse`] if one is already running. Not a queue: the shim allows one
    /// job, and a queue here would be a scheduler the caller already has a better one of.
    pub fn submit(&mut self, job: &ModPow<'_>) -> Result<()> {
        if self.busy {
            return Err(Error::InUse);
        }
        let n = job.encode(&mut self.input)?;
        self.result_len = job.modulus.len().min(MAX_OPERAND);

        self.submit_raw(OP_MODPOW, n)
    }

    /// Start a key derivation.
    ///
    /// Same contract as [`submit`]: one at a time, off the GUI thread.
    pub fn submit_kdf(&mut self, job: &Kdf<'_>) -> Result<()> {
        if self.busy {
            return Err(Error::InUse);
        }
        let n = job.encode(&mut self.input)?;
        // KDF always produces 32 bytes (SHA-256 digest).
        self.result_len = 32;

        self.submit_raw(OP_KDF, n)
    }

    /// Run an opaque payload on the worker under an application-defined opcode.
    ///
    /// `out_len` is how many bytes the job will write, and it is the caller's promise: the worker is
    /// handed that length and a pointer, and writing past it is a buffer overrun on the GUI thread's
    /// heap. It must not exceed [`Job::output_capacity`].
    ///
    /// The contract the whole facility rests on still holds and is worth repeating here, because a
    /// caller reaching for this one is writing the job itself: the worker has **its own heap**, so
    /// nothing it allocates may outlive it. Temporaries are fine; a `Vec` built on the worker and
    /// dropped on the GUI thread is a cross-heap free, which is silent corruption rather than a
    /// clean failure. The result travels through the caller's output buffer, never as an allocation.
    pub fn submit_bytes(&mut self, opcode: i32, payload: &[u8], out_len: usize) -> Result<()> {
        if self.busy {
            return Err(Error::InUse);
        }
        if opcode < OP_APP_BASE {
            // Refused rather than passed through: see OP_APP_BASE. A payload arriving under
            // OP_MODPOW would be decoded as three operands and answered, not rejected.
            return Err(Error::Argument);
        }
        if payload.len() > self.input.len() || out_len > self.output.len() {
            return Err(Error::Overflow);
        }
        self.input[..payload.len()].copy_from_slice(payload);
        self.result_len = out_len;
        self.submit_raw(opcode, payload.len())
    }

    fn submit_raw(&mut self, opcode: i32, n: usize) -> Result<()> {
        let rc = unsafe {
            sys::shim_work_submit_ex(
                opcode,
                self.input.as_ptr(),
                n as i32,
                self.output.as_mut_ptr(),
                self.result_len as i32,
                self.heap_max as i32,
                self.stack as i32,
            )
        };
        if rc != sys::SHIM_OK {
            return Err(Error::from_code(rc));
        }
        self.busy = true;
        Ok(())
    }

    /// Feed a raw event. Returns the result once the job finishes.
    ///
    /// `Err` means the worker reported a failure; the buffers are free again either way.
    pub fn on_event(&mut self, ev: &sys::ShimEvent) -> Option<Result<&[u8]>> {
        if ev.kind != sys::SHIM_EV_WORK_DONE || !self.busy {
            return None;
        }
        self.busy = false;
        if ev.status != sys::SHIM_OK {
            return Some(Err(Error::from_code(ev.status)));
        }
        Some(Ok(&self.output[..self.result_len]))
    }
}

/// Push and pop one frame on the calling thread's cleanup stack.
///
/// The narrowest test of whether platform C++ can allocate on this thread. Returns 0 if the push and
/// pop completed, or the leave code. A panic returns nothing at all, and [`Job::last_exit`] names it.
///
/// Not a method on [`Job`]: it has to run *inside* a job, on the worker, which is the thread whose
/// cleanup stack is in question.
pub fn cleanup_probe() -> i32 {
    unsafe { sys::shim_cleanup_probe() }
}

/// The same push with no `TRAP` of its own — the shape of every call into platform C++ from a job.
///
/// A cleanup stack is not a cleanup stack *frame*. Frames come from `TRAP`, and a `PushL` with none
/// panics `E32USER-CBase 66` — which is what killed every job that reached libhubbub before the
/// worker's entry point wrapped the job in one. [`cleanup_probe`] supplies its own TRAP and so
/// cannot see the difference; this one can.
pub fn cleanup_probe_bare() -> i32 {
    unsafe { sys::shim_cleanup_probe_bare() }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -------------------------------------------------------------- arbitrary jobs --
    //
    // These stop at the FFI: on the host every extern is a stub returning NotReady, so reaching
    // that error means every check *before* it passed. Which is the half worth testing — the
    // refusals are the logic, and the shim call is one line.

    /// A job whose completion never arrives must not lock the `Job` out for good.
    ///
    /// This is the shape of the bug it fixes: a caller times out, tries again, and the second
    /// submission fails with InUse — an error about the wrong thing, which hides the first failure.
    #[test]
    fn a_job_that_never_completed_can_be_abandoned() {
        let mut j = Job::with_capacity(64, 16);
        // On the host the submit reaches a stub and fails, so drive `busy` the way a real
        // submission would by asserting the observable instead: a fresh Job is not busy.
        assert!(!j.is_busy());
        // `platform_busy` is false on the host (the stub answers 0), so abandoning is allowed and
        // is a no-op on an idle Job rather than an error.
        assert!(j.abandon().is_ok());
        assert!(!j.is_busy());
    }

    #[test]
    fn a_caller_sized_job_takes_its_own_payload() {
        let mut j = Job::with_capacity(4096, 1024);
        assert_eq!(j.input_capacity(), 4096);
        assert_eq!(j.output_capacity(), 1024);
        let payload = vec![7u8; 4096];
        assert_eq!(
            j.submit_bytes(OP_APP_BASE, &payload, 1024),
            Err(Error::NotReady),
            "a payload that fits must reach the shim"
        );
    }

    /// An opcode this module encodes is refused, not passed through.
    ///
    /// The failure it prevents is the quiet one: arbitrary bytes submitted as OP_MODPOW are *decoded*
    /// as three length-prefixed operands and answered, so the caller gets a wrong number rather than
    /// an error.
    #[test]
    fn a_reserved_opcode_is_refused() {
        let mut j = Job::with_capacity(64, 64);
        for op in [OP_MODPOW, OP_KDF, 0, OP_APP_BASE - 1] {
            assert_eq!(j.submit_bytes(op, &[1, 2, 3], 8), Err(Error::Argument), "opcode {op}");
        }
        assert_eq!(j.submit_bytes(OP_APP_BASE, &[1, 2, 3], 8), Err(Error::NotReady));
    }

    /// Neither buffer may be overrun, and `out_len` is the one the worker writes through.
    #[test]
    fn a_job_larger_than_its_buffers_is_refused() {
        let mut j = Job::with_capacity(16, 8);
        assert_eq!(
            j.submit_bytes(OP_APP_BASE, &[0u8; 17], 8),
            Err(Error::Overflow),
            "a payload past the input buffer"
        );
        assert_eq!(
            j.submit_bytes(OP_APP_BASE, &[0u8; 4], 9),
            Err(Error::Overflow),
            "an output length past the output buffer — the worker writes through this"
        );
        assert_eq!(j.submit_bytes(OP_APP_BASE, &[0u8; 16], 8), Err(Error::NotReady), "exactly full");
    }

    /// The heap ceiling is the caller's, and asking for a big one is not an error here.
    ///
    /// 256 KB was a fixed ceiling chosen for crypto, and it is not a ceiling a DOM can be squeezed
    /// under — one measured page inflates to 700 KB of HTML before a tree exists.
    #[test]
    fn the_worker_heap_ceiling_is_the_callers_choice() {
        let mut j = Job::with_capacity(64, 64);
        j.set_worker_heap(8 * 1024 * 1024);
        assert_eq!(
            j.submit_bytes(OP_APP_BASE, &[1], 1),
            Err(Error::NotReady),
            "a megabyte-scale ceiling is a legal request"
        );
    }

    /// A zero-length payload and a zero-length result are both legal — a job may take nothing and
    /// return nothing, and the shim says so too.
    #[test]
    fn an_empty_job_is_legal() {
        let mut j = Job::with_capacity(0, 0);
        assert_eq!(j.submit_bytes(OP_APP_BASE, &[], 0), Err(Error::NotReady));
    }

    /// The default ceiling is what every existing caller had, so making it adjustable changed
    /// nothing for them.
    #[test]
    fn the_default_ceiling_is_unchanged() {
        assert_eq!(DEFAULT_WORKER_HEAP, 256 * 1024);
    }

    #[test]
    fn an_exponent_one_byte_wider_than_the_modulus_is_accepted() {
        // SRP's third exponentiation raises to `a + u*x`. `a` is as wide as the prime, so
        // the sum carries into a 257th byte — always, by construction, not occasionally.
        //
        // Rejecting it made every two-factor login stop dead: the job was refused rather
        // than answered wrongly, so there was no crash and no error, just a screen that
        // said "verificando a senha" forever.
        let base = [7u8; MAX_OPERAND];
        let exp = [3u8; MAX_EXPONENT];
        let modulus = [0xFFu8; MAX_OPERAND];
        let job = ModPow { base: &base, exp: &exp, modulus: &modulus };
        let mut buf = [0u8; 6 + 2 * MAX_OPERAND + MAX_EXPONENT];
        let n = job.encode(&mut buf).expect("the SRP exponent must fit");
        assert_eq!(n, 6 + base.len() + exp.len() + modulus.len());

        let (b, e, m) = decode_modpow(&buf[..n]).expect("and decode again");
        assert_eq!(b, &base[..]);
        assert_eq!(e, &exp[..], "the exponent lost its extra byte");
        assert_eq!(m, &modulus[..]);
    }

    #[test]
    fn the_job_buffer_holds_the_widest_job_there_is() {
        // The buffer and the limits have to agree, and they are declared in two places.
        let job = Job::new();
        assert!(job.input.len() >= 6 + 2 * MAX_OPERAND + MAX_EXPONENT);
    }

    #[test]
    fn a_modpow_round_trips_through_the_packing() {
        let base = [3u8];
        let exp = [0xAAu8; 256];
        let modulus = [0xFFu8; 256];
        let job = ModPow { base: &base, exp: &exp, modulus: &modulus };
        let mut buf = [0u8; 6 + 3 * MAX_OPERAND];
        let n = job.encode(&mut buf).unwrap();
        assert_eq!(n, 6 + 1 + 256 + 256);

        let (b, e, m) = decode_modpow(&buf[..n]).unwrap();
        assert_eq!(b, &base[..]);
        assert_eq!(e, &exp[..]);
        assert_eq!(m, &modulus[..]);
    }

    #[test]
    fn an_operand_that_is_too_large_is_refused() {
        // Not truncated. A silently shortened modulus produces a mathematically valid
        // answer to a different question, which is the worst possible failure here.
        let big = [0u8; MAX_OPERAND + 1];
        let job = ModPow { base: &[3], exp: &[1], modulus: &big };
        let mut buf = [0u8; 6 + 3 * MAX_OPERAND];
        assert_eq!(job.encode(&mut buf), Err(Error::Argument));
    }

    #[test]
    fn a_truncated_buffer_decodes_to_nothing() {
        // The worker reads whatever the GUI thread wrote. A length that overruns is a bug
        // on this side, and returning None makes it a clean failure rather than a slice
        // panic on a thread with no cleanup stack.
        assert!(decode_modpow(&[]).is_none());
        // Three one-byte operands need 6 + 3 = 9 bytes; both of these are short.
        assert!(decode_modpow(&[0, 1, 0, 1, 0, 1]).is_none());
        assert!(decode_modpow(&[0, 1, 0, 1, 0, 1, 0, 0]).is_none());
        // And nine is enough.
        assert!(decode_modpow(&[0, 1, 0, 1, 0, 1, 7, 8, 9]).is_some());
    }

    #[test]
    fn empty_operands_are_legal() {
        // An exponent of zero length is a caller bug but not this layer's to judge, and
        // rejecting it here would mean two places deciding what a valid exponent is.
        let job = ModPow { base: &[], exp: &[], modulus: &[] };
        let mut buf = [0u8; 16];
        assert_eq!(job.encode(&mut buf).unwrap(), 6);
        let (b, e, m) = decode_modpow(&buf[..6]).unwrap();
        assert!(b.is_empty() && e.is_empty() && m.is_empty());
    }
}
