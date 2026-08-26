//! The repositories somebody registered, and what happened the last time each was asked.
//!
//! A repository is a place to look for packages. Today that means a GitHub repository and its latest
//! release; [`RepoKind`] is an enum from the first day so that a second kind — a mirror, a plain
//! index, our own server — is a variant rather than a rewrite.
//!
//! ## The last result is part of the record
//!
//! [`Repo::last`] is stored, not recomputed, and that is the difference between a screen that can
//! explain itself and one that shrugs. A repository that answered "GitHub's hourly limit" an hour ago
//! should still say so when the phone has no radio, because the useful next action — *wait* — comes
//! from the old answer and not from a fresh failure to connect.

use alloc::string::String;
use alloc::vec::Vec;

use crate::config::DecodeError;
use crate::crc::crc16;
use crate::github::RepoError;

/// `b"BTRP"` read as a little-endian u32.
pub const MAGIC: u32 = 0x5052_5442;
pub const VERSION: u16 = 1;
pub const HEADER_SIZE: usize = 16;
/// Bytes per record: id, kind, flags, last-result tag, a pad, the found count, the check time, and an
/// offset/length pair for each of the three strings.
///
/// **Exactly what the encoder writes**, and this file got it wrong the same way `catalog.rs` did an
/// hour earlier: a record size smaller than the stride makes each record read the middle of the one
/// before it, and the strings come out sliced. Both are now pinned by
/// `the_record_size_matches_what_the_encoder_writes`, which is the only kind of test that catches a
/// number typed from memory.
pub const ENTRY_SIZE: u16 = 28;
/// Refused above this. Registering nine repositories on a phone is already unusual.
pub const MAX_REPOS: usize = 16;

/// Where a repository lives.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum RepoKind {
    /// A GitHub repository's latest release.
    Github,
}

impl RepoKind {
    fn tag(&self) -> u8 {
        match self {
            RepoKind::Github => 0,
        }
    }

    fn from_tag(t: u8) -> Option<Self> {
        match t {
            0 => Some(RepoKind::Github),
            // An unknown kind is not a kind to guess at: a record written by a newer build describes
            // something this one cannot ask, and asking it wrongly is worse than not listing it.
            _ => None,
        }
    }
}

/// What the last check produced. Stored, so the screen can explain itself offline.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum LastResult {
    /// Never asked.
    Never,
    /// Found this many installable packages.
    Found(u16),
    /// Failed, and the reason is worth keeping in words rather than as a code.
    Failed(FailReason),
}

/// The reasons a check fails, as a byte that survives a round trip.
///
/// A narrower list than [`RepoError`] on purpose: what a person can do about it is the only thing
/// this needs to distinguish. "Wait an hour" and "fix the name" and "there is nothing there" are
/// three actions; everything else is one.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum FailReason {
    NotFound,
    RateLimited,
    NoPackages,
    /// The server answered something else, or the answer was unreadable, or the radio was down.
    Refused,
}

impl FailReason {
    pub fn of(e: &RepoError) -> Self {
        match e {
            RepoError::NotFound => FailReason::NotFound,
            RepoError::RateLimited => FailReason::RateLimited,
            RepoError::NoPackages => FailReason::NoPackages,
            _ => FailReason::Refused,
        }
    }

    pub fn describe(self) -> &'static str {
        match self {
            FailReason::NotFound => "not found — private, renamed, or no release",
            FailReason::RateLimited => "GitHub's hourly limit; try again later",
            FailReason::NoPackages => "no .sis in the latest release",
            FailReason::Refused => "could not be read",
        }
    }
}

impl LastResult {
    /// The line the Repos row carries.
    pub fn describe(self) -> String {
        match self {
            LastResult::Never => String::from("not checked yet"),
            LastResult::Found(0) => String::from("nothing offered"),
            LastResult::Found(1) => String::from("1 package"),
            LastResult::Found(n) => alloc::format!("{n} packages"),
            LastResult::Failed(r) => String::from(r.describe()),
        }
    }

    fn tag(self) -> u8 {
        match self {
            LastResult::Never => 0,
            LastResult::Found(_) => 1,
            LastResult::Failed(FailReason::NotFound) => 2,
            LastResult::Failed(FailReason::RateLimited) => 3,
            LastResult::Failed(FailReason::NoPackages) => 4,
            LastResult::Failed(FailReason::Refused) => 5,
        }
    }

    fn from_parts(tag: u8, count: u16) -> Self {
        match tag {
            1 => LastResult::Found(count),
            2 => LastResult::Failed(FailReason::NotFound),
            3 => LastResult::Failed(FailReason::RateLimited),
            4 => LastResult::Failed(FailReason::NoPackages),
            5 => LastResult::Failed(FailReason::Refused),
            _ => LastResult::Never,
        }
    }
}

/// One registered repository.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Repo {
    /// Stable within this database, and what a [`crate::catalog::CatEntry`] points back to. Not the
    /// row index: rows move when one is removed, and a catalogue that pointed at row 2 would then
    /// belong to somebody else.
    pub id: u16,
    pub kind: RepoKind,
    /// `owner` and `repo` for GitHub. Kept apart rather than as one string, because they are two
    /// fields in every URL this builds.
    pub owner: String,
    pub repo: String,
    /// A plain substring an asset's name must contain, or empty for all of them. The cheap stand-in
    /// for Obtainium's regex — enough for a release that ships several of our packages at once.
    pub filter: String,
    /// Off means listed and never asked. For a repository somebody is done with but does not want to
    /// forget the name of.
    pub enabled: bool,
    /// Unix seconds of the last check, or 0.
    pub checked_s: i64,
    pub last: LastResult,
}

impl Repo {
    pub fn github(id: u16, owner: String, repo: String) -> Self {
        Self {
            id,
            kind: RepoKind::Github,
            owner,
            repo,
            filter: String::new(),
            enabled: true,
            checked_s: 0,
            last: LastResult::Never,
        }
    }

    /// `owner/repo`, which is how the row is labelled and how a person thinks of it.
    pub fn label(&self) -> String {
        alloc::format!("{}/{}", self.owner, self.repo)
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct RepoDb {
    pub repos: Vec<Repo>,
}

impl RepoDb {
    pub fn get(&self, id: u16) -> Option<&Repo> {
        self.repos.iter().find(|r| r.id == id)
    }

    pub fn get_mut(&mut self, id: u16) -> Option<&mut Repo> {
        self.repos.iter_mut().find(|r| r.id == id)
    }

    /// The next free id.
    ///
    /// One past the highest ever used rather than the count, because ids have to outlive removals: a
    /// catalogue entry still pointing at a repository that was deleted must not suddenly belong to
    /// the next one added.
    pub fn next_id(&self) -> u16 {
        self.repos.iter().map(|r| r.id).max().map_or(1, |m| m.saturating_add(1))
    }

    /// Register `owner/repo`, or say why not.
    ///
    /// Refuses a duplicate rather than adding a second row for the same place: two rows would both
    /// contribute catalogue entries and the screen would show every package twice.
    pub fn add_github(&mut self, owner: String, repo: String) -> Result<u16, AddError> {
        if owner.is_empty() || repo.is_empty() {
            return Err(AddError::BadTarget);
        }
        if self.repos.len() >= MAX_REPOS {
            return Err(AddError::Full);
        }
        if self
            .repos
            .iter()
            .any(|r| r.owner.eq_ignore_ascii_case(&owner) && r.repo.eq_ignore_ascii_case(&repo))
        {
            return Err(AddError::Duplicate);
        }
        let id = self.next_id();
        self.repos.push(Repo::github(id, owner, repo));
        Ok(id)
    }

    /// Forget a repository. The caller clears its catalogue rows.
    pub fn remove(&mut self, id: u16) -> bool {
        let before = self.repos.len();
        self.repos.retain(|r| r.id != id);
        self.repos.len() != before
    }

    pub fn encode(&self) -> Vec<u8> {
        let count = self.repos.len().min(MAX_REPOS);
        let mut blob: Vec<u16> = Vec::new();
        let mut out = Vec::with_capacity(HEADER_SIZE + count * ENTRY_SIZE as usize);

        out.extend_from_slice(&MAGIC.to_le_bytes());
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend_from_slice(&ENTRY_SIZE.to_le_bytes());
        out.extend_from_slice(&(count as u16).to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());

        for r in self.repos.iter().take(count) {
            // Three strings in one slot each: owner, repo, filter.
            let owner = push_str(&mut blob, &r.owner);
            let repo = push_str(&mut blob, &r.repo);
            let filter = push_str(&mut blob, &r.filter);
            let mut flags = 0u8;
            if r.enabled {
                flags |= 0x01;
            }
            let count_field = match r.last {
                LastResult::Found(n) => n,
                _ => 0,
            };

            out.extend_from_slice(&r.id.to_le_bytes());
            out.push(r.kind.tag());
            out.push(flags);
            out.push(r.last.tag());
            out.push(0);
            out.extend_from_slice(&count_field.to_le_bytes());
            out.extend_from_slice(&r.checked_s.to_le_bytes());
            out.extend_from_slice(&owner.0.to_le_bytes());
            out.extend_from_slice(&owner.1.to_le_bytes());
            out.extend_from_slice(&repo.0.to_le_bytes());
            out.extend_from_slice(&repo.1.to_le_bytes());
            out.extend_from_slice(&filter.0.to_le_bytes());
            out.extend_from_slice(&filter.1.to_le_bytes());
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
        if count > MAX_REPOS {
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

        let mut repos = Vec::with_capacity(count);
        for i in 0..count {
            let r = &bytes[HEADER_SIZE + i * entry_size..];
            // A record whose kind this build does not know is skipped rather than guessed at.
            let Some(kind) = RepoKind::from_tag(r[2]) else { continue };
            let mut when = [0u8; 8];
            when.copy_from_slice(&r[8..16]);
            repos.push(Repo {
                id: u16::from_le_bytes([r[0], r[1]]),
                kind,
                enabled: r[3] & 0x01 != 0,
                last: LastResult::from_parts(r[4], u16::from_le_bytes([r[6], r[7]])),
                checked_s: i64::from_le_bytes(when),
                owner: take_str(&blob, r, 16).ok_or(DecodeError::BadLayout)?,
                repo: take_str(&blob, r, 20).ok_or(DecodeError::BadLayout)?,
                filter: take_str(&blob, r, 24).unwrap_or_default(),
            });
        }
        Ok(Self { repos })
    }
}

/// Why a repository was not registered.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum AddError {
    BadTarget,
    Duplicate,
    Full,
}

impl AddError {
    pub fn describe(self) -> &'static str {
        match self {
            AddError::BadTarget => "not an owner/repo",
            AddError::Duplicate => "already registered",
            AddError::Full => "no room for another",
        }
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

    fn db() -> RepoDb {
        let mut d = RepoDb::default();
        d.add_github(String::from("pizzaria-foundation"), String::from("home")).unwrap();
        d.add_github(String::from("BurntSushi"), String::from("ripgrep")).unwrap();
        d
    }

    #[test]
    fn the_record_size_matches_what_the_encoder_writes() {
        // With no strings the blob is empty, so the whole file is the header plus the records — and
        // the stride the encoder used is arithmetic rather than a number somebody typed.
        let mut d = RepoDb::default();
        for i in 0..3u16 {
            d.repos.push(Repo {
                id: i + 1,
                kind: RepoKind::Github,
                owner: String::new(),
                repo: String::new(),
                filter: String::new(),
                enabled: true,
                checked_s: 0,
                last: LastResult::Never,
            });
        }
        let bytes = d.encode();
        assert_eq!(bytes.len(), HEADER_SIZE + 3 * ENTRY_SIZE as usize);
    }

    #[test]
    fn a_registry_round_trips_with_its_last_answer() {
        let mut d = db();
        let r = d.get_mut(1).unwrap();
        r.last = LastResult::Found(3);
        r.checked_s = 1_700_000_000;
        r.filter = String::from("launcher");
        d.get_mut(2).unwrap().last = LastResult::Failed(FailReason::RateLimited);

        let back = RepoDb::decode(&d.encode()).expect("round trip");
        assert_eq!(back, d);
        assert_eq!(back.get(1).unwrap().last, LastResult::Found(3));
        assert_eq!(back.get(2).unwrap().last.describe(), "GitHub's hourly limit; try again later");
    }

    #[test]
    fn an_empty_registry_is_valid_and_one_flipped_byte_is_not() {
        assert!(RepoDb::decode(&RepoDb::default().encode()).unwrap().repos.is_empty());
        let mut b = db().encode();
        let last = b.len() - 1;
        b[last] ^= 0xFF;
        assert_eq!(RepoDb::decode(&b), Err(DecodeError::BadCrc));
    }

    #[test]
    fn ids_outlive_removals() {
        // A catalogue entry still pointing at a deleted repository must not suddenly belong to the
        // next one added. Reusing the count as an id is exactly how that happens.
        let mut d = db();
        assert!(d.remove(1));
        let id = d.add_github(String::from("a"), String::from("b")).unwrap();
        assert_eq!(id, 3, "not 1, and not 2");
        assert!(!d.remove(99), "removing what is not there is not an error");
    }

    #[test]
    fn the_same_place_is_refused_rather_than_listed_twice() {
        // Two rows would both contribute catalogue entries, and every package would appear twice.
        let mut d = db();
        assert_eq!(
            d.add_github(String::from("PIZZARIA-FOUNDATION"), String::from("Home")),
            Err(AddError::Duplicate),
            "case is not a difference in a GitHub name"
        );
        assert_eq!(d.add_github(String::new(), String::from("x")), Err(AddError::BadTarget));
    }

    #[test]
    fn a_full_registry_says_so() {
        let mut d = RepoDb::default();
        for i in 0..MAX_REPOS {
            d.add_github(alloc::format!("o{i}"), String::from("r")).unwrap();
        }
        assert_eq!(d.add_github(String::from("one"), String::from("more")), Err(AddError::Full));
    }

    #[test]
    fn every_last_result_has_a_sentence() {
        assert_eq!(LastResult::Never.describe(), "not checked yet");
        assert_eq!(LastResult::Found(0).describe(), "nothing offered");
        assert_eq!(LastResult::Found(1).describe(), "1 package");
        assert_eq!(LastResult::Found(4).describe(), "4 packages");
        for r in [
            FailReason::NotFound,
            FailReason::RateLimited,
            FailReason::NoPackages,
            FailReason::Refused,
        ] {
            assert!(!LastResult::Failed(r).describe().is_empty());
        }
    }

    #[test]
    fn a_failure_keeps_only_the_distinctions_a_person_can_act_on() {
        // "Wait an hour", "fix the name" and "there is nothing there" are three actions. Everything
        // else is one, and pretending otherwise would be a screen full of codes.
        assert_eq!(FailReason::of(&RepoError::RateLimited), FailReason::RateLimited);
        assert_eq!(FailReason::of(&RepoError::NotFound), FailReason::NotFound);
        assert_eq!(FailReason::of(&RepoError::NoPackages), FailReason::NoPackages);
        assert_eq!(FailReason::of(&RepoError::Status(500)), FailReason::Refused);
        assert_eq!(FailReason::of(&RepoError::BadJson(7)), FailReason::Refused);
        assert_eq!(FailReason::of(&RepoError::BadTarget), FailReason::Refused);
    }

    #[test]
    fn a_record_of_a_kind_this_build_does_not_know_is_skipped_not_guessed() {
        let mut b = db().encode();
        // Corrupt the first record's kind byte, then fix the CRC so only the kind is in question.
        b[HEADER_SIZE + 2] = 9;
        b[14..16].copy_from_slice(&[0, 0]);
        let crc = crc16(&b);
        b[14..16].copy_from_slice(&crc.to_le_bytes());
        let back = RepoDb::decode(&b).expect("the rest still decodes");
        assert_eq!(back.repos.len(), 1, "the unknown one is absent");
        assert_eq!(back.repos[0].id, 2);
    }

    #[test]
    fn the_label_is_what_a_person_calls_it() {
        assert_eq!(db().get(1).unwrap().label(), "pizzaria-foundation/home");
    }
}
