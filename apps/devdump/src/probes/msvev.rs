//! Do Message Server events cross a process boundary, and which folder does MCE reply into?
//!
//! # Why this probe is the one the design rests on
//!
//! `crates/symbian-mtm` wakes a service up when the user replies inside Nokia's Messaging
//! application. The mechanism is a session event: the Message Server tells *every* open
//! session that entries changed, and `shim_msv_observe` turns those into `SHIM_EV_MSV` on the
//! shim's event ring.
//!
//! Nothing measures that. The mechanism is documented and the Messaging application plainly
//! depends on it, but "documented" has been wrong about this platform four times in this
//! directory alone. Three questions have no answer from reading:
//!
//! | question | what it decides |
//! |---|---|
//! | does a session event reach a *different* process at all? | whether a service needs a polling timer |
//! | which sub-kinds, and in what order relative to the body being committed? | whether `is_pending`'s flag test is enough |
//! | which folder does MCE pass to `ReplyL` as the destination? | whether `Descriptor::outgoing` can stop watching two |
//!
//! # Why it cannot be a one-shot
//!
//! Session events are dispatched by an active object. They need a scheduler that idles, and
//! they need the user to be doing something in another application meanwhile. Every other probe
//! here runs in a few hundred milliseconds and exits; this one sits and watches, which is why
//! it is a resident `DaemonApp` of its own rather than a `OneShot` body.
//!
//! # It mirrors the bridge rather than doing the obvious thing
//!
//! When an event arrives it writes one line and **queues the id**. The store read happens on
//! the next timer tick. That is not tidiness: it is exactly what `Bridge::handle_raw` and
//! `Bridge::poll` do, and for the same reason — the event arrives inside the ring drain, where
//! a store read can block on the Message Server. Measuring a design other than the one that
//! ships would tell us very little.
//!
//! # What the operator has to do
//!
//! Reply to one of our messages in the Messaging application while this is running. The report
//! says so as its first line, because a probe whose result depends on a human should say what
//! it wants before it starts waiting.

use alloc::string::String;
use alloc::vec::Vec;

use symbian::fs::ShimFs;
use symbian::msg::{self, Session, StoreEventKind};
use symbian_report::{push_hex, push_i64, Report};

use crate::registry;

/// `apps/mtmdemo`'s type, the one the `mtm` probe registers. Entries of any other type are
/// still logged — knowing that a session event carries *somebody else's* traffic is itself the
/// answer to the first question — but only ours are read in detail.
const MTM_UID: u32 = 0xE0DD_0B01;

/// How long to watch, in seconds.
///
/// Generous, because nothing is waiting: this probe is *detached* — the launcher starts it and
/// finishes the fleet without it, so the operator can take as long as they need to reach the
/// Messaging application and reply. The first version was 100 seconds on the assumption that
/// the launcher would be sitting there, and it cannot: leaving the launcher backgrounds it and
/// the system closes it, which showed up as the fleet starting over.
///
/// It exits a second after seeing a reply, so a successful run costs a fraction of this.
const WINDOW_SECONDS: i32 = 300;

/// The queue of ids to read on the next tick. Bounded because a bulk change can report many,
/// and reading a hundred entries on one tick is the blocking this design exists to avoid.
const MAX_QUEUED: usize = 16;

pub struct Watcher {
    report: Report,
    fs: ShimFs,
    session: Option<Session>,
    started: bool,
    done: bool,
    /// Ticks of the one-second clock.
    seconds: i32,
    /// Every `SHIM_EV_MSV` seen, for the count in the summary.
    events: i32,
    /// Ids awaiting a store read, with the sub-kind that reported them.
    queued: Vec<(msg::EntryId, StoreEventKind)>,
    /// Set once an entry of ours has been seen published in a folder other than the inbox —
    /// which is the reply, and the whole point of the run.
    reply_seen: bool,
}

impl Default for Watcher {
    fn default() -> Self {
        Watcher::new()
    }
}

impl Watcher {
    pub fn new() -> Self {
        /* One shot to get out of the constructor before the rendezvous, exactly as
         * `probes::OneShot` documents: `rust_app_start` runs before `RProcess::Rendezvous`, so
         * work done here would hold the launcher's wait open for the whole run. */
        let _ = symbian::timer_after(1);
        Watcher {
            report: Report::new("msvev"),
            fs: ShimFs,
            session: None,
            started: false,
            done: false,
            seconds: 0,
            events: 0,
            queued: Vec::new(),
            reply_seen: false,
        }
    }

    fn start(&mut self) {
        self.report
            .open_output(&mut self.fs, registry::DIR, &registry::filename(63, "msvev"));
        self.report.head("Message Server session events, across a process boundary");
        self.report.line("");
        self.report.line("WHAT TO DO WHILE THIS RUNS:");
        self.report
            .line("  open Messaging, pick one of our messages, and reply to it.");
        self.report
            .line("  This probe is watching for the event that arrives when you do.");
        self.report.line("");

        self.report.entering(&mut self.fs, "CMsvSession::OpenSyncL");
        match Session::open() {
            Ok(s) => {
                self.report.check("session opened", true);
                self.session = Some(s);
            }
            Err(e) => {
                self.report.check_note("session opened", false, &err(e));
                self.done = true;
                return;
            }
        }

        self.report.entering(&mut self.fs, "shim_msv_observe");
        let observing = self
            .session
            .as_mut()
            .map(|s| s.observe())
            .unwrap_or(Err(symbian::Error::NotReady));
        match observing {
            Ok(()) => self.report.check("event delivery enabled", true),
            Err(e) => {
                self.report
                    .check_note("event delivery enabled", false, &err(e));
                self.done = true;
                return;
            }
        }

        /* A folder census first, so a change in the counts is corroboration for the events —
         * and so a run that sees no events at all still says whether anything happened. */
        for (id, name) in msg::FOLDERS {
            if let Some(s) = self.session.as_mut() {
                if let Ok(n) = s.folder_count(*id) {
                    let mut line = String::from("before: ");
                    line.push_str(name);
                    line.push_str(" = ");
                    push_i64(&mut line, n as i64);
                    self.report.line(&line);
                }
            }
        }

        self.report.line("");
        let mut line = String::from("watching for ");
        push_i64(&mut line, WINDOW_SECONDS as i64);
        line.push_str(" seconds");
        self.report.line(&line);

        /* The clock, and whether it was armed at all.
         *
         * A silent `let _ = timer_every(...)` is what made the first run of this probe
         * unreadable: the report ended at "watching for 100 seconds" with no events and no END,
         * and "nothing arrived" was indistinguishable from "the clock never ticked" — which is
         * the same ambiguity that cost two device trips when a UI tick was never called.
         *
         * So the arming is a check, and the ticks announce themselves below. */
        match symbian::timer_every(1000) {
            Ok(_) => self.report.check("clock armed", true),
            Err(e) => {
                self.report.check_note("clock armed", false, &err(e));
                /* Without a clock nothing will ever read the queue or close the report, so end
                 * here rather than sit until the launcher kills the process. */
                self.done = true;
                return;
            }
        }
        self.report.flush(&mut self.fs);
        self.started = true;
    }

    /// A line every ten seconds, so a report cut short still says how far it got.
    ///
    /// This is the difference between a finding and a shrug: a truncated file showing "40s: 0
    /// events" says forty seconds passed with nothing arriving, where the same file ending at
    /// "watching" says only that somebody read it too early.
    fn heartbeat(&mut self) {
        if self.seconds != 1 && self.seconds % 10 != 0 {
            return;
        }
        let mut line = String::from("  ");
        push_i64(&mut line, self.seconds as i64);
        line.push_str("s: ");
        push_i64(&mut line, self.events as i64);
        line.push_str(" events so far");
        if self.seconds == 1 {
            line.push_str("  (the clock is ticking)");
        }
        self.report.line(&line);
        self.report.flush(&mut self.fs);
    }

    /// One line per event, written before anything is read. The order matters for the same
    /// reason every report here writes its breadcrumb first: if the store read is what kills
    /// the process, the event that preceded it is still on disk.
    fn note_event(&mut self, ev: &symbian_sys::ShimEvent) {
        let Some(store) = msg::store_event(ev) else {
            return;
        };
        self.events += 1;

        let mut line = String::from("  ev ");
        push_i64(&mut line, self.seconds as i64);
        line.push_str("s ");
        line.push_str(kind_name(store.kind));
        line.push_str(" id=");
        push_hex(&mut line, store.id as u32, 8);
        line.push_str(" parent=");
        push_hex(&mut line, store.parent as u32, 8);
        line.push_str(" batch=");
        push_i64(&mut line, store.batch as i64);
        if store.batch > 8 {
            /* The shim caps delivery at 8 per notification; a larger batch means the rest were
             * dropped deliberately and a reader must rescan. Worth seeing in the report,
             * because it is the case the "an event is a hint" design exists for. */
            line.push_str(" (capped)");
        }
        self.report.line(&line);
        self.report.flush(&mut self.fs);

        /* Queue the read for the next tick, mirroring the bridge. Deletions carry nothing to
         * read; the id is already gone. */
        if store.kind != StoreEventKind::Deleted
            && store.id != 0
            && self.queued.len() < MAX_QUEUED
        {
            self.queued.push((store.id, store.kind));
        }
    }

    /// Drain the queue: this is `Bridge::poll`'s half of the split.
    fn read_queued(&mut self) {
        if self.queued.is_empty() {
            return;
        }
        let batch: Vec<(msg::EntryId, StoreEventKind)> = self.queued.drain(..).collect();
        for (id, kind) in batch {
            let Some(session) = self.session.as_mut() else {
                return;
            };
            let entry = match session.entry(id) {
                Ok(e) => e,
                Err(e) => {
                    /* Gone between the event and the read. Not a failure — it is the exact
                     * case that makes an event a hint rather than data, and seeing it in a
                     * report is better than a design note claiming it can happen. */
                    let mut line = String::from("    read ");
                    push_hex(&mut line, id as u32, 8);
                    line.push_str(" -> ");
                    line.push_str(&err(e));
                    self.report.line(&line);
                    continue;
                }
            };

            let mut line = String::from("    ");
            line.push_str(kind_name(kind));
            line.push(' ');
            push_hex(&mut line, id as u32, 8);
            line.push_str(" parent=");
            push_hex(&mut line, entry.parent as u32, 8);
            line.push_str(" mtm=");
            push_hex(&mut line, entry.mtm_uid, 8);
            line.push_str(" flags=");
            push_hex(&mut line, entry.flags as u32, 2);
            if entry.is_message() {
                line.push_str(" message");
            } else if entry.is_service() {
                line.push_str(" service");
            } else if entry.is_folder() {
                line.push_str(" folder");
            }
            self.report.line(&line);

            if entry.mtm_uid != MTM_UID {
                self.report.line("      (not ours)");
                continue;
            }

            /* Ours. The three things the design turns on: which folder, whether the flags say
             * published, and whether the body is already there. */
            let mut line = String::from("      details=\"");
            line.push_str(&entry.details);
            line.push('"');
            if entry.details_truncated {
                line.push_str(" (truncated)");
            }
            self.report.line(&line);

            let mut flags = String::from("      ");
            flags.push_str(if entry.complete() { "complete " } else { "-complete " });
            flags.push_str(if entry.visible() { "visible " } else { "-visible " });
            flags.push_str(if entry.in_preparation() { "in-prep " } else { "-in-prep " });
            flags.push_str(if entry.failed() { "failed" } else { "-failed" });
            self.report.line(&flags);

            match self.session.as_mut().map(|s| s.body(id)) {
                Some(Ok(text)) => {
                    let mut line = String::from("      body ");
                    push_i64(&mut line, text.len() as i64);
                    line.push_str(" chars: \"");
                    line.push_str(head_of(&text));
                    line.push('"');
                    self.report.line(&line);

                    /* The answer to the whole run: an entry of ours, published, with a body,
                     * somewhere that is not the inbox. */
                    let published = entry.complete() && entry.visible() && !entry.in_preparation();
                    if published && entry.parent != symbian_sys::SHIM_MSV_INBOX && !text.is_empty()
                    {
                        let mut line = String::from("      >>> a reply, in ");
                        line.push_str(folder_name(entry.parent));
                        self.report.line(&line);
                        self.reply_seen = true;
                    }
                }
                Some(Err(e)) => {
                    let mut line = String::from("      body unreadable: ");
                    line.push_str(&err(e));
                    self.report.line(&line);
                }
                None => {}
            }
        }
        self.report.flush(&mut self.fs);
    }

    fn finish(&mut self) {
        self.report.line("");
        self.report.num("events seen", self.events as i64);
        self.report.num("seconds watched", self.seconds as i64);

        /* The three findings, stated as checks so `grep FAIL` finds them. */
        self.report.check_note(
            "a session event crossed into this process",
            self.events > 0,
            if self.events > 0 {
                "so a service can be woken rather than poll"
            } else {
                "nothing arrived — a service needs a polling timer, and Bridge::poll is it"
            },
        );
        self.report.check_note(
            "a published reply of ours was observed",
            self.reply_seen,
            if self.reply_seen {
                "with its folder and flags above"
            } else {
                "either nobody replied during the window, or the reply was not reported"
            },
        );

        for (id, name) in msg::FOLDERS {
            if let Some(s) = self.session.as_mut() {
                if let Ok(n) = s.folder_count(*id) {
                    let mut line = String::from("after: ");
                    line.push_str(name);
                    line.push_str(" = ");
                    push_i64(&mut line, n as i64);
                    self.report.line(&line);
                }
            }
        }

        /* The session is dropped with the struct, which closes it. Observation stops with it. */
        self.report.finish(&mut self.fs);
    }
}

impl symbian_app::DaemonApp for Watcher {
    fn handle_raw(&mut self, ev: &symbian_sys::ShimEvent) {
        if self.done {
            return;
        }
        if ev.kind == symbian_sys::SHIM_EV_MSV {
            self.note_event(ev);
            return;
        }
        if ev.kind != symbian_sys::SHIM_EV_TIMER {
            return;
        }
        if !self.started {
            self.start();
            if self.done {
                /* start() failed; write what it managed before exiting. */
                self.report.finish(&mut self.fs);
            }
            return;
        }

        self.seconds += 1;
        self.heartbeat();
        self.read_queued();

        /* Exit early on success: a run that got its answer should not make the operator wait
         * out the window, and the launcher is blocked on this process. One extra second after
         * the reply, so the events that follow it are recorded too. */
        if (self.reply_seen && self.queued.is_empty()) || self.seconds >= WINDOW_SECONDS {
            self.finish();
            self.done = true;
        }
    }

    fn should_exit(&self) -> bool {
        self.done
    }
}

fn kind_name(kind: StoreEventKind) -> &'static str {
    match kind {
        StoreEventKind::Created => "created",
        StoreEventKind::Changed => "changed",
        StoreEventKind::Deleted => "deleted",
        StoreEventKind::Moved => "moved",
        StoreEventKind::MtmInstalled => "mtm-installed",
        StoreEventKind::MtmRemoved => "mtm-removed",
        StoreEventKind::ServerReady => "server-ready",
        StoreEventKind::ServerGone => "server-gone",
    }
}

fn folder_name(id: msg::EntryId) -> &'static str {
    for (fid, name) in msg::FOLDERS {
        if *fid == id {
            return name;
        }
    }
    "an unlisted folder"
}

/// The first line, capped, so one long message does not bury the rest of the report.
fn head_of(s: &str) -> &str {
    let end = s.find(['\n', '\r']).unwrap_or(s.len()).min(48);
    /* Truncate on a char boundary: a UTF-8 body cut mid-sequence would not be a str. */
    let mut cut = end;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    &s[..cut]
}

/// Same shape as the other probes': the raw platform code, because a name for it would be one
/// more thing to keep in step with `e32err.h`.
fn err(e: symbian::Error) -> String {
    let code = match e {
        symbian::Error::Platform(c) => c,
        symbian::Error::NotFound => -1,
        symbian::Error::PathNotFound => -12,
        symbian::Error::AlreadyExists => -11,
        symbian::Error::AccessDenied => -46,
        symbian::Error::NotReady => -18,
        symbian::Error::InUse => -14,
        symbian::Error::Argument => -6,
        _ => -2,
    };
    let mut s = String::from("err ");
    push_i64(&mut s, code as i64);
    s
}
