//! The work waiting to be done over the network, and how much of it is finished.
//!
//! One job at a time, and that is the platform's decision rather than a simplification: `Http::get`
//! is documented as "Replaces any transaction in flight", so a second concurrent download would
//! cancel the first. A queue is therefore the honest model, not a compromise.
//!
//! ## Resumable, because a 2G connection drops
//!
//! [`Job::got`] is bytes already on disk, written down every time it changes. A download that stops
//! at 184 of 320 KB resumes at 184 rather than starting again, which on this phone is the difference
//! between a package arriving and a package never arriving. The partial file is `<name>.part` and is
//! only renamed when the last byte lands, so a half-file is never mistaken for a package.
//!
//! ## The state machine, and why it is written down
//!
//! Same discipline as [`crate::update`]: every transition is persisted **before** the action it
//! authorises. The reason is the same too — closing the screen, or a battery pull, must leave a queue
//! that can be picked up rather than one that has to be reconstructed. And unlike the update journal,
//! this one has no supervisor behind it: the GUI owns it, so its ability to resume *is* its ability to
//! survive being closed.

use alloc::string::String;
use alloc::vec::Vec;

use crate::config::DecodeError;
use crate::crc::crc16;

/// `b"BTQJ"` read as a little-endian u32.
pub const MAGIC: u32 = 0x4A51_5442;
pub const VERSION: u16 = 1;
pub const HEADER_SIZE: usize = 16;
/// Bytes per record: two ids, kind, state, tries, a pad, the last code, the two byte counts, and an
/// offset/length pair for each of the two strings.
///
/// Pinned by `the_record_size_matches_what_the_encoder_writes`, which earned its place immediately:
/// this number was typed as 40, the encoder writes 36, and the test said so before the mistake could
/// become a round-trip bug with sliced strings — which is how it presented in `catalog.rs` and
/// `repo.rs` an hour earlier.
pub const ENTRY_SIZE: u16 = 36;
/// Refused above this. A queue longer than this on a phone is a mistake, not a plan.
pub const MAX_JOBS: usize = 32;
/// Attempts before a job is left alone. Generous, because the failure being retried is usually a
/// tunnel rather than a wrong URL — and each attempt resumes rather than restarting.
pub const MAX_TRIES: u8 = 5;

/// What a job is for.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum JobKind {
    /// Ask a repository what it has now.
    Check,
    /// Fetch one file.
    Download,
}

/// Where a job is.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum JobState {
    /// Waiting its turn.
    Queued,
    /// The one job in flight. At most one row is ever in this state.
    Running,
    /// Finished. Kept on the list so somebody can see what happened, and cleared by the caller.
    Done,
    /// Stopped, and worth retrying — the tries are not spent.
    Failed,
    /// Stopped for good: the tries are gone, or the answer was one no retry will change.
    GaveUp,
    /// The user asked for it to stop.
    Cancelled,
}

impl JobState {
    /// Whether this job still wants the network.
    pub fn pending(self) -> bool {
        matches!(self, JobState::Queued | JobState::Running)
    }

    pub fn describe(self) -> &'static str {
        match self {
            JobState::Queued => "waiting",
            JobState::Running => "running",
            JobState::Done => "done",
            JobState::Failed => "failed",
            JobState::GaveUp => "gave up",
            JobState::Cancelled => "cancelled",
        }
    }
}

/// One piece of network work.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Job {
    /// Stable for the life of the queue, and not the row index — a row that finishes and is cleared
    /// would otherwise renumber everything after it while a download is in flight.
    pub id: u16,
    pub kind: JobKind,
    pub state: JobState,
    /// The repository this is for, so a failed check can be written back to the right row.
    pub repo_id: u16,
    /// What to fetch. For a check this is the API URL; for a download, the asset's.
    pub url: String,
    /// What the row is called on screen, and for a download the file it lands in.
    pub name: String,
    /// Bytes already on disk. The resume point, and the numerator of the progress bar.
    pub got: u64,
    /// What the service said the whole thing weighs, or 0 when it did not say. Zero is why the
    /// progress bar has an indeterminate mode.
    pub total: u64,
    pub tries: u8,
    /// The HTTP status or error code of the last attempt, kept as a number. A code this project has
    /// measured gets words at the point it is shown; one it has not keeps the number, because a wrong
    /// explanation sends whoever debugs it to the wrong place.
    pub last_code: i32,
}

impl Job {
    pub fn check(id: u16, repo_id: u16, url: String, name: String) -> Self {
        Self {
            id,
            kind: JobKind::Check,
            state: JobState::Queued,
            repo_id,
            url,
            name,
            got: 0,
            total: 0,
            tries: 0,
            last_code: 0,
        }
    }

    pub fn download(id: u16, repo_id: u16, url: String, name: String, total: u64) -> Self {
        Self { kind: JobKind::Download, total, ..Self::check(id, repo_id, url, name) }
    }

    /// Progress as a fraction, or `None` when the size is unknown — which is the difference between
    /// a bar that fills and a bar that only says "something is happening".
    pub fn fraction(&self) -> Option<f32> {
        if self.total == 0 {
            return None;
        }
        Some((self.got as f32 / self.total as f32).clamp(0.0, 1.0))
    }

    /// Whether a stopped job is worth offering a retry for.
    pub fn resumable(&self) -> bool {
        matches!(self.state, JobState::Failed) && self.tries < MAX_TRIES
    }

    /// The `Range` offset to ask from, or `None` to ask for the whole thing.
    ///
    /// Only for a download, and only when there is something on disk. A check is a few KB and
    /// resuming one would save nothing while adding a way to get half a JSON document.
    pub fn resume_from(&self) -> Option<u64> {
        (self.kind == JobKind::Download && self.got > 0).then_some(self.got)
    }
}

/// The queue, in order.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Queue {
    pub jobs: Vec<Job>,
}

impl Queue {
    /// The next id. Highest ever used plus one, for the reason [`crate::repo::RepoDb::next_id`]
    /// gives: ids must outlive the rows being cleared.
    pub fn next_id(&self) -> u16 {
        self.jobs.iter().map(|j| j.id).max().map_or(1, |m| m.saturating_add(1))
    }

    pub fn get(&self, id: u16) -> Option<&Job> {
        self.jobs.iter().find(|j| j.id == id)
    }

    pub fn get_mut(&mut self, id: u16) -> Option<&mut Job> {
        self.jobs.iter_mut().find(|j| j.id == id)
    }

    /// Add a job, unless the same URL is already waiting or running.
    ///
    /// Refusing the duplicate is what stops a person who taps Install twice from downloading the same
    /// 320 KB twice — and, worse, from having two jobs write the same `.part`.
    pub fn push(&mut self, job: Job) -> Option<u16> {
        if self.jobs.len() >= MAX_JOBS {
            return None;
        }
        if self.jobs.iter().any(|j| j.state.pending() && j.url == job.url) {
            return None;
        }
        let id = job.id;
        self.jobs.push(job);
        Some(id)
    }

    /// The job in flight, if any.
    pub fn running(&self) -> Option<&Job> {
        self.jobs.iter().find(|j| j.state == JobState::Running)
    }

    /// The next job to start, or `None` when one is already running or nothing is waiting.
    ///
    /// The "already running" half is the whole reason this is a queue: `Http::get` replaces any
    /// transaction in flight, so starting a second job would silently cancel the first and leave its
    /// `.part` frozen at whatever it had reached.
    pub fn next_to_start(&self) -> Option<u16> {
        if self.running().is_some() {
            return None;
        }
        self.jobs.iter().find(|j| j.state == JobState::Queued).map(|j| j.id)
    }

    /// Mark a job as started.
    pub fn start(&mut self, id: u16) {
        if let Some(j) = self.get_mut(id) {
            j.state = JobState::Running;
            j.tries = j.tries.saturating_add(1);
        }
    }

    /// Record progress. Returns whether anything changed, so a caller can decide whether to write the
    /// queue out — this is called for every packet, and writing a file per packet would be the
    /// slowest download on the phone.
    pub fn advance(&mut self, id: u16, got: u64) -> bool {
        match self.get_mut(id) {
            Some(j) if j.got != got => {
                j.got = got;
                true
            }
            _ => false,
        }
    }

    pub fn finish(&mut self, id: u16, state: JobState, code: i32) {
        if let Some(j) = self.get_mut(id) {
            j.state = state;
            j.last_code = code;
        }
    }

    /// A job stopped and may be worth another attempt. Spending the last try turns it into
    /// [`JobState::GaveUp`], so a caller does not have to count.
    /// Put a running job back in the queue because *we* stopped it, not because it failed.
    ///
    /// # Why this is not `fail`
    ///
    /// It was `fail`, and that cost a job. Leaving the packages area releases the network engine,
    /// which called `fail(id, 0)` on whatever was in flight — so an interruption the user caused by
    /// walking away was recorded as an attempt that did not work. Two things followed from that and
    /// both are wrong:
    ///
    /// * `next_to_start` only ever picks a `Queued` job, so a `Failed` one never restarted on its
    ///   own. Three separate comments in `my-epoc` promise that reopening resumes; it did not, and
    ///   the user had to find Retry.
    /// * `start` counts a try and `fail` promotes to `GaveUp` at [`MAX_TRIES`]. So entering and
    ///   leaving the area five times — which costs nothing and looks like nothing — burned every
    ///   attempt and left the job in the one state `retry` refuses.
    ///
    /// So this returns the attempt as well as the state. The bytes on disk and `got` were always
    /// kept; what was missing was the bookkeeping telling the truth about who stopped it.
    pub fn pause(&mut self, id: u16) {
        if let Some(j) = self.get_mut(id) {
            if j.state == JobState::Running {
                j.state = JobState::Queued;
                // Give back what `start` counted. An attempt that was interrupted is not an attempt
                // that failed, and a retry budget spent on the user's navigation is not a budget.
                j.tries = j.tries.saturating_sub(1);
            }
        }
    }

    pub fn fail(&mut self, id: u16, code: i32) {
        if let Some(j) = self.get_mut(id) {
            j.last_code = code;
            j.state = if j.tries >= MAX_TRIES { JobState::GaveUp } else { JobState::Failed };
        }
    }

    /// Put a failed job back in the queue. The bytes already on disk stay, which is what makes this a
    /// resume rather than a restart.
    pub fn retry(&mut self, id: u16) -> bool {
        match self.get_mut(id) {
            Some(j) if j.resumable() => {
                j.state = JobState::Queued;
                true
            }
            _ => false,
        }
    }

    /// Stop a job. A running one keeps its bytes, so cancelling and retrying is not a restart either.
    pub fn cancel(&mut self, id: u16) -> bool {
        match self.get_mut(id) {
            Some(j) if j.state.pending() => {
                j.state = JobState::Cancelled;
                true
            }
            _ => false,
        }
    }

    /// Drop everything that is over, keeping what is still waiting or running.
    pub fn clear_finished(&mut self) {
        self.jobs.retain(|j| j.state.pending());
    }

    /// After a crash or a close, a job that says it was running is not running any more.
    ///
    /// Called once when the queue is read. Without it the very first `next_to_start` would answer
    /// `None` for ever, because a row nobody is driving would look like the job in flight — which is
    /// exactly the state a closed screen leaves behind.
    pub fn reconcile(&mut self) {
        for j in self.jobs.iter_mut().filter(|j| j.state == JobState::Running) {
            j.state = if j.tries >= MAX_TRIES { JobState::GaveUp } else { JobState::Failed };
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let count = self.jobs.len().min(MAX_JOBS);
        let mut blob: Vec<u16> = Vec::new();
        let mut out = Vec::with_capacity(HEADER_SIZE + count * ENTRY_SIZE as usize);

        out.extend_from_slice(&MAGIC.to_le_bytes());
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend_from_slice(&ENTRY_SIZE.to_le_bytes());
        out.extend_from_slice(&(count as u16).to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());

        for j in self.jobs.iter().take(count) {
            let url = push_str(&mut blob, &j.url);
            let name = push_str(&mut blob, &j.name);

            out.extend_from_slice(&j.id.to_le_bytes());
            out.extend_from_slice(&j.repo_id.to_le_bytes());
            out.push(match j.kind {
                JobKind::Check => 0,
                JobKind::Download => 1,
            });
            out.push(state_tag(j.state));
            out.push(j.tries);
            out.push(0);
            out.extend_from_slice(&j.last_code.to_le_bytes());
            out.extend_from_slice(&j.got.to_le_bytes());
            out.extend_from_slice(&j.total.to_le_bytes());
            out.extend_from_slice(&url.0.to_le_bytes());
            out.extend_from_slice(&url.1.to_le_bytes());
            out.extend_from_slice(&name.0.to_le_bytes());
            out.extend_from_slice(&name.1.to_le_bytes());
        }

        for u in &blob {
            out.extend_from_slice(&u.to_le_bytes());
        }
        let crc = crc16(&out);
        out[14..16].copy_from_slice(&crc.to_le_bytes());
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() < HEADER_SIZE {
            return Err(DecodeError::Truncated);
        }
        if u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) != MAGIC {
            return Err(DecodeError::BadMagic);
        }
        let version = u16::from_le_bytes([bytes[4], bytes[5]]);
        if version > VERSION {
            return Err(DecodeError::BadVersion(version));
        }
        let entry_size = u16::from_le_bytes([bytes[6], bytes[7]]) as usize;
        if entry_size < ENTRY_SIZE as usize {
            return Err(DecodeError::BadLayout);
        }
        let count = u16::from_le_bytes([bytes[8], bytes[9]]) as usize;
        if count > MAX_JOBS {
            return Err(DecodeError::TooMany(count));
        }
        let table_end = HEADER_SIZE.checked_add(count * entry_size).ok_or(DecodeError::BadLayout)?;
        if bytes.len() < table_end {
            return Err(DecodeError::BadLayout);
        }
        let mut check = Vec::from(bytes);
        let stored = u16::from_le_bytes([bytes[14], bytes[15]]);
        check[14..16].copy_from_slice(&[0, 0]);
        if crc16(&check) != stored {
            return Err(DecodeError::BadCrc);
        }
        let tail = &bytes[table_end..];
        if tail.len() % 2 != 0 {
            return Err(DecodeError::BadLayout);
        }
        let blob: Vec<u16> =
            tail.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();

        let mut jobs = Vec::with_capacity(count);
        for i in 0..count {
            let r = &bytes[HEADER_SIZE + i * entry_size..];
            let mut got = [0u8; 8];
            got.copy_from_slice(&r[12..20]);
            let mut total = [0u8; 8];
            total.copy_from_slice(&r[20..28]);
            jobs.push(Job {
                id: u16::from_le_bytes([r[0], r[1]]),
                repo_id: u16::from_le_bytes([r[2], r[3]]),
                kind: if r[4] == 0 { JobKind::Check } else { JobKind::Download },
                state: state_of(r[5]),
                tries: r[6],
                last_code: i32::from_le_bytes([r[8], r[9], r[10], r[11]]),
                got: u64::from_le_bytes(got),
                total: u64::from_le_bytes(total),
                url: take_str(&blob, r, 28).ok_or(DecodeError::BadLayout)?,
                name: take_str(&blob, r, 32).ok_or(DecodeError::BadLayout)?,
            });
        }
        Ok(Self { jobs })
    }
}

fn state_tag(s: JobState) -> u8 {
    match s {
        JobState::Queued => 0,
        JobState::Running => 1,
        JobState::Done => 2,
        JobState::Failed => 3,
        JobState::GaveUp => 4,
        JobState::Cancelled => 5,
    }
}

fn state_of(t: u8) -> JobState {
    match t {
        1 => JobState::Running,
        2 => JobState::Done,
        3 => JobState::Failed,
        4 => JobState::GaveUp,
        5 => JobState::Cancelled,
        // An unknown tag is treated as waiting rather than refused: the worst it costs is one extra
        // attempt at something, and refusing the whole file would cost the queue.
        _ => JobState::Queued,
    }
}

fn push_str(blob: &mut Vec<u16>, s: &str) -> (u16, u16) {
    let units: Vec<u16> = s.encode_utf16().take(u16::MAX as usize).collect();
    let off = blob.len() as u16;
    let len = units.len() as u16;
    blob.extend_from_slice(&units);
    (off, len)
}

fn take_str(blob: &[u16], r: &[u8], at: usize) -> Option<String> {
    if r.len() < at + 4 {
        return None;
    }
    let off = u16::from_le_bytes([r[at], r[at + 1]]) as usize;
    let len = u16::from_le_bytes([r[at + 2], r[at + 3]]) as usize;
    let slice = blob.get(off..off.checked_add(len)?)?;
    Some(String::from_utf16_lossy(slice))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dl(id: u16, name: &str, total: u64) -> Job {
        Job::download(
            id,
            1,
            alloc::format!("https://github.com/x/y/releases/download/v1/{name}"),
            String::from(name),
            total,
        )
    }

    #[test]
    fn the_record_size_matches_what_the_encoder_writes() {
        let mut q = Queue::default();
        for i in 0..3u16 {
            let mut j = dl(i + 1, "", 0);
            j.url = String::new();
            q.jobs.push(j);
        }
        assert_eq!(q.encode().len(), HEADER_SIZE + 3 * ENTRY_SIZE as usize);
    }

    #[test]
    fn a_queue_round_trips_including_how_far_it_got() {
        let mut q = Queue::default();
        q.push(dl(1, "launcher.sisx", 320_484));
        q.push(Job::check(2, 7, String::from("https://api.github.com/x"), String::from("x/y")));
        q.start(1);
        q.advance(1, 184_320);
        q.finish(2, JobState::Done, 200);

        let back = Queue::decode(&q.encode()).expect("round trip");
        assert_eq!(back, q);
        // The resume point is the whole reason this file is written to disk.
        assert_eq!(back.get(1).unwrap().got, 184_320);
        assert_eq!(back.get(1).unwrap().resume_from(), Some(184_320));
    }

    #[test]
    fn one_at_a_time_because_the_stack_replaces_a_transaction_in_flight() {
        // `Http::get` is documented as "Replaces any transaction in flight", so a second concurrent
        // download would cancel the first and freeze its .part.
        let mut q = Queue::default();
        q.push(dl(1, "a.sisx", 10));
        q.push(dl(2, "b.sisx", 10));
        assert_eq!(q.next_to_start(), Some(1));
        q.start(1);
        assert_eq!(q.next_to_start(), None, "not while one is running");
        q.finish(1, JobState::Done, 200);
        assert_eq!(q.next_to_start(), Some(2));
    }

    #[test]
    fn tapping_install_twice_does_not_download_it_twice() {
        // Two jobs would also write the same .part, which is worse than the wasted bytes.
        let mut q = Queue::default();
        assert_eq!(q.push(dl(1, "a.sisx", 10)), Some(1));
        assert_eq!(q.push(dl(2, "a.sisx", 10)), None, "same URL, still waiting");
        // Once it is over, asking again is legitimate.
        q.finish(1, JobState::Done, 200);
        assert_eq!(q.push(dl(2, "a.sisx", 10)), Some(2));
    }

    #[test]
    fn a_retry_resumes_rather_than_restarting() {
        let mut q = Queue::default();
        q.push(dl(1, "a.sisx", 320_000));
        q.start(1);
        q.advance(1, 184_000);
        q.fail(1, -33);
        assert_eq!(q.get(1).unwrap().state, JobState::Failed);
        assert!(q.retry(1));
        assert_eq!(q.get(1).unwrap().state, JobState::Queued);
        assert_eq!(q.get(1).unwrap().got, 184_000, "the bytes stay");
        assert_eq!(q.get(1).unwrap().resume_from(), Some(184_000));
    }

    #[test]
    fn the_tries_run_out_and_then_it_is_left_alone() {
        let mut q = Queue::default();
        q.push(dl(1, "a.sisx", 10));
        for _ in 0..MAX_TRIES {
            q.start(1);
            q.fail(1, -33);
            q.retry(1);
        }
        assert_eq!(q.get(1).unwrap().state, JobState::GaveUp);
        assert!(!q.retry(1), "and it is not offered again");
    }

    #[test]
    fn a_closed_screen_leaves_a_queue_that_can_be_picked_up() {
        // Nothing supervises this queue — the GUI owns it — so a row that says "running" after the
        // app was closed would make `next_to_start` answer None for ever.
        let mut q = Queue::default();
        q.push(dl(1, "a.sisx", 10));
        q.start(1);
        q.advance(1, 5);

        let mut back = Queue::decode(&q.encode()).unwrap();
        assert_eq!(back.next_to_start(), None, "before reconciling, it looks busy");
        back.reconcile();
        assert_eq!(back.get(1).unwrap().state, JobState::Failed);
        assert!(back.retry(1));
        assert_eq!(back.next_to_start(), Some(1));
        assert_eq!(back.get(1).unwrap().got, 5, "and it resumes");
    }

    #[test]
    fn a_check_is_never_resumed() {
        // A few KB, and resuming one is a way to get half a JSON document.
        let mut q = Queue::default();
        q.push(Job::check(1, 1, String::from("https://api/x"), String::from("x")));
        q.start(1);
        q.advance(1, 900);
        assert_eq!(q.get(1).unwrap().resume_from(), None);
    }

    #[test]
    fn progress_is_a_fraction_only_when_the_size_is_known() {
        // Zero total is why the bar has an indeterminate mode: a server that sends no length is a
        // real thing, and a bar stuck at 0% reads as broken.
        let mut q = Queue::default();
        q.push(dl(1, "a.sisx", 0));
        q.advance(1, 5_000);
        assert_eq!(q.get(1).unwrap().fraction(), None);

        q.push(dl(2, "b.sisx", 200));
        q.advance(2, 50);
        assert_eq!(q.get(2).unwrap().fraction(), Some(0.25));
        // And a server that lies about the length cannot push the bar past full.
        q.advance(2, 5_000);
        assert_eq!(q.get(2).unwrap().fraction(), Some(1.0));
    }

    #[test]
    fn advance_says_whether_anything_changed() {
        // Called for every packet. Writing the queue to disk per packet would be the slowest
        // download on the phone.
        let mut q = Queue::default();
        q.push(dl(1, "a.sisx", 100));
        assert!(q.advance(1, 10));
        assert!(!q.advance(1, 10), "the same number twice is not news");
        assert!(!q.advance(99, 10), "and neither is a job that is not there");
    }

    #[test]
    fn cancelling_keeps_the_bytes_and_clearing_keeps_what_is_live() {
        let mut q = Queue::default();
        q.push(dl(1, "a.sisx", 100));
        q.push(dl(2, "b.sisx", 100));
        q.start(1);
        q.advance(1, 40);
        assert!(q.cancel(1));
        assert_eq!(q.get(1).unwrap().got, 40, "cancel is not a restart either");
        assert!(!q.cancel(1), "cancelling twice is not an action");

        q.clear_finished();
        assert_eq!(q.jobs.len(), 1, "only the one still waiting");
        assert_eq!(q.jobs[0].id, 2);
    }

    #[test]
    fn ids_outlive_the_rows_being_cleared() {
        let mut q = Queue::default();
        q.push(dl(1, "a.sisx", 1));
        q.push(dl(2, "b.sisx", 1));
        q.finish(1, JobState::Done, 200);
        q.clear_finished();
        assert_eq!(q.next_id(), 3, "not 2, which is still in flight");
    }

    #[test]
    fn one_flipped_byte_is_refused() {
        let mut q = Queue::default();
        q.push(dl(1, "a.sisx", 10));
        let mut b = q.encode();
        let last = b.len() - 1;
        b[last] ^= 0xFF;
        assert_eq!(Queue::decode(&b), Err(DecodeError::BadCrc));
    }

    #[test]
    fn every_state_has_a_word_for_it() {
        for s in [
            JobState::Queued,
            JobState::Running,
            JobState::Done,
            JobState::Failed,
            JobState::GaveUp,
            JobState::Cancelled,
        ] {
            assert!(!s.describe().is_empty());
            assert_eq!(state_of(state_tag(s)), s);
        }
    }

    #[test]
    fn walking_in_and_out_of_the_area_never_uses_up_a_download() {
        // The defect, as the sequence a person actually performs: open the packages list, wander
        // off, come back. Six times. Each trip called `fail`, which counted the interruption as an
        // attempt and left the job in a state `next_to_start` does not pick — so on the fifth trip
        // the job hit `MAX_TRIES`, became `GaveUp`, and `retry` refused it. Nothing on screen said
        // that a download had died of being navigated away from.
        let mut q = Queue::default();
        q.push(dl(1, "launcher.sisx", 320_484));
        for trip in 1..=6 {
            assert_eq!(q.next_to_start(), Some(1), "trip {trip}: the queue offers the job again");
            q.start(1);
            q.advance(1, 10_000 * trip);
            q.pause(1);
            assert_eq!(q.get(1).unwrap().tries, 0, "trip {trip}: an interruption is not an attempt");
        }
        let j = q.get(1).unwrap();
        assert_eq!(j.state, JobState::Queued);
        assert_eq!(j.got, 60_000, "and the bytes were never in doubt");
        assert_eq!(j.resume_from(), Some(60_000));
    }

    #[test]
    fn a_real_failure_still_counts_and_still_gives_up() {
        // The negative control for the test above. `pause` must not have made the retry budget
        // unspendable — a server that refuses five times is a job that should stop asking.
        let mut q = Queue::default();
        q.push(dl(1, "launcher.sisx", 320_484));
        for _ in 0..MAX_TRIES {
            q.start(1);
            q.fail(1, 500);
        }
        assert_eq!(q.get(1).unwrap().state, JobState::GaveUp);
        assert!(!q.get(1).unwrap().resumable());
        assert!(!q.retry(1));
    }
}
