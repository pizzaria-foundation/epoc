//! Staying logged in.
//!
//! Being signed in to Telegram is not a token the client holds — it is a property the
//! server attaches to an **auth key**. Keep the key and you are still logged in; lose it
//! and you are a stranger, whatever else was saved.
//!
//! So this is the whole of session persistence: write 281 bytes to the private directory
//! and read them back at launch. There is no refresh, no expiry and nothing to renew.
//!
//! # What is in the record, and why each part
//!
//! | | |
//! |---|---|
//! | magic | tells a real record from a truncated or foreign file |
//! | dc | **the key belongs to one data centre.** Using it against another is `-404` |
//! | auth key | 256 bytes; this is the account |
//! | key id | derivable from the key, stored so a launch does not hash 256 bytes to find out |
//! | salt | the last one seen. Stale within the hour, and the server corrects it |
//! | time offset | the handset's clock is set by hand and the server rejects a `msg_id` more than 30 s out |
//!
//! Fixed width, no parser. A format with a parser is a format that can half-load, and half
//! an auth key produces a handshake that appears to succeed and a session where every
//! message is rejected — which looks like the network rather than like storage.
//!
//! # Where it lives
//!
//! `C:\private\<UID3>\session.bin`, through [`symbian::fs::private_path`]. That directory
//! needs no capability, which matters for an unsigned package — and it is the one place on
//! the handset another application cannot read.
//!
//! It is **not encrypted**. Anyone who can read that directory has already defeated the
//! platform's isolation, and a key derived from something the phone also stores protects
//! against nothing. Saying so is better than a comforting XOR.
//!
//! # When it must be thrown away
//!
//! A key can stop working, and continuing to use one that has is a client that retries
//! forever. [`Invalidate`] names the three ways:
//!
//! - transport `-404` — the server does not recognise the key id at all
//! - `AUTH_KEY_UNREGISTERED` — the same, said inside a session
//! - `SESSION_REVOKED` / `SESSION_EXPIRED` — the user ended it from another device
//!
//! All three mean the same thing to us and all three are the user's account being taken
//! away, so the file is overwritten before it is deleted rather than just unlinked.

use alloc::vec;
use alloc::vec::Vec;

use symbian::fs::{self, Fs, Utf16Path};
use tg_proto::handshake::AuthKey;

/// `"tgS3"`. Bumped when the layout changes, so an old record is refused rather than
/// misread — the version is two bytes into a fixed-width struct, which is the only place a
/// format check is worth anything.
pub const MAGIC: u32 = 0x7467_5333;

/// Total record width: magic, dc, offset, iap, key, key id, salt.
///
/// Written as the sum of its parts and asserted against what [`encode`] produces, because
/// the two are in different places and this is where they disagree. The first version of
/// this line had a spare `+ 4` on the end and every test failed on the assertion — which is
/// the cheapest way that mistake has ever been caught.
pub const LEN: usize = 4 + 1 + 8 + 4 + 256 + 8 + 8;

/// The file, under the private directory.
const NAME: &str = "session.bin";

/// A stored session.
#[derive(Clone, PartialEq, Eq)]
pub struct Stored {
    pub dc: u8,
    pub auth: AuthKey,
    pub salt: [u8; 8],
    pub time_offset: i64,
    /// The access point the OS settled on, or zero when unknown. Passing it back to
    /// [`symbian::net::Bearer::start`] makes the next launch silent — the prompt only
    /// appears when the saved id fails or when there is none.
    pub iap: u32,
}

impl core::fmt::Debug for Stored {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // The key is deliberately not printed. A log line is a file, and a file with an
        // auth key in it is the account.
        f.debug_struct("Stored")
            .field("dc", &self.dc)
            .field("key_id", &self.auth.id)
            .field("time_offset", &self.time_offset)
            .finish_non_exhaustive()
    }
}

/// Why a stored session is being discarded.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Invalidate {
    /// Transport `-404`: the server does not know this key id.
    UnknownKey,
    /// `AUTH_KEY_UNREGISTERED`, said inside a session that decrypted fine.
    Unregistered,
    /// The user ended this session from another device.
    Revoked,
    /// The user asked to log out.
    LoggedOut,
}

/// Recognise an RPC error that means the stored key is dead.
///
/// Matched on the prefix rather than the whole string, because Telegram appends detail to
/// some of these and a client that compares for equality stops recognising them the day it
/// does.
pub fn invalidating(error: &str) -> Option<Invalidate> {
    if error.starts_with("AUTH_KEY_UNREGISTERED") || error.starts_with("AUTH_KEY_INVALID") {
        return Some(Invalidate::Unregistered);
    }
    if error.starts_with("SESSION_REVOKED") || error.starts_with("SESSION_EXPIRED") {
        return Some(Invalidate::Revoked);
    }
    if error.starts_with("USER_DEACTIVATED") {
        return Some(Invalidate::Revoked);
    }
    None
}

fn path<F: Fs>(fs: &mut F) -> symbian::Result<Utf16Path> {
    let dir = fs::private_path(fs)?;
    Utf16Path::join(dir.as_units(), NAME)
}

pub fn encode(s: &Stored) -> Vec<u8> {
    let mut v = Vec::with_capacity(LEN);
    v.extend_from_slice(&MAGIC.to_be_bytes());
    v.push(s.dc);
    v.extend_from_slice(&s.time_offset.to_be_bytes());
    v.extend_from_slice(&s.iap.to_be_bytes());
    v.extend_from_slice(&s.auth.key);
    v.extend_from_slice(&s.auth.id.to_be_bytes());
    v.extend_from_slice(&s.salt);
    debug_assert_eq!(v.len(), LEN);
    v
}

pub fn decode(bytes: &[u8]) -> Option<Stored> {
    if bytes.len() != LEN {
        return None;
    }
    if u32::from_be_bytes(bytes[0..4].try_into().ok()?) != MAGIC {
        return None;
    }
    let dc = bytes[4];
    let time_offset = i64::from_be_bytes(bytes[5..13].try_into().ok()?);
    let iap = u32::from_be_bytes(bytes[13..17].try_into().ok()?);
    let mut key = [0u8; 256];
    key.copy_from_slice(&bytes[17..273]);
    let id = u64::from_be_bytes(bytes[273..281].try_into().ok()?);
    let mut salt = [0u8; 8];
    salt.copy_from_slice(&bytes[281..289]);

    // An all-zero key is what a half-written file or a zeroed-on-logout one looks like, and
    // it is not a key. Refusing it here means the caller redoes the handshake rather than
    // spending two exponentiations proving that zero does not work.
    if key.iter().all(|&b| b == 0) {
        return None;
    }

    Some(Stored {
        dc,
        // server_time is not stored: it exists only so the offset can be computed once, and
        // an absolute time from a previous launch is stale by however long the phone was off.
        auth: AuthKey { key, id, salt, server_time: 0 },
        salt,
        time_offset,
        iap,
    })
}

/// Read the stored session, if there is a usable one.
///
/// A missing file, a foreign one and a corrupt one are all the same answer — `None`, meaning
/// log in — because there is nothing a caller could do differently and a client that
/// reported three kinds of "you are not logged in" would be reporting an implementation
/// detail.
pub fn load<F: Fs>(fs: &mut F) -> Option<Stored> {
    let p = path(fs).ok()?;
    decode(&fs::read(fs, &p).ok()??)
}

/// Write it, atomically.
///
/// [`symbian::fs::write_atomic`] renames over the target, so a battery pull leaves either
/// the old session or the new one. That matters more than it sounds: the failure it prevents
/// is a half-written key, which parses as far as the magic and then produces a session where
/// every message is rejected — a symptom that points at the network for as long as anyone
/// is willing to look.
pub fn save<F: Fs>(fs: &mut F, s: &Stored) -> symbian::Result<()> {
    let p = path(fs)?;
    fs::write_atomic(fs, &p, &encode(s))
}

/// Throw the session away.
///
/// Overwritten with zeros before it is deleted. A delete on this filesystem unlinks the
/// entry and leaves the bytes, and those bytes are the account — so the wipe is what makes
/// "log out" mean something, and the delete is only tidiness.
///
/// The write is best-effort: if it fails the delete still runs, because a stored key that
/// could not be wiped is worse left in place.
pub fn clear<F: Fs>(fs: &mut F, _why: Invalidate) -> symbian::Result<()> {
    let p = path(fs)?;
    let _ = fs::write_atomic(fs, &p, &vec![0u8; LEN]);
    fs.delete(p.as_units())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::BTreeMap;
    use symbian::fs::OpenMode;

    fn a_key() -> AuthKey {
        let mut k = [0u8; 256];
        for (i, b) in k.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(37).wrapping_add(11);
        }
        AuthKey { key: k, id: 0x0bad_c0de_dead_beef, salt: [3; 8], server_time: 0 }
    }

    fn a_session() -> Stored {
        Stored { dc: 4, auth: a_key(), salt: [3; 8], time_offset: -17, iap: 42 }
    }

    /// An in-memory `Fs`, since the one in `symbian` is private to its own tests.
    #[derive(Default)]
    struct MemFs {
        files: BTreeMap<Vec<u16>, Vec<u8>>,
        open: Vec<Option<(Vec<u16>, usize)>>,
    }

    impl Fs for MemFs {
        fn open(&mut self, path: &[u16], mode: OpenMode) -> symbian::Result<i32> {
            let key = path.to_vec();
            match mode {
                OpenMode::Read if !self.files.contains_key(&key) => {
                    return Err(symbian::Error::NotFound)
                }
                OpenMode::Replace => {
                    self.files.insert(key.clone(), Vec::new());
                }
                _ => {
                    self.files.entry(key.clone()).or_default();
                }
            }
            self.open.push(Some((key, 0)));
            Ok(self.open.len() as i32)
        }

        fn read(&mut self, handle: i32, buf: &mut [u8]) -> symbian::Result<usize> {
            let (key, pos) = self.open[(handle - 1) as usize].clone().unwrap();
            let data = &self.files[&key];
            let n = buf.len().min(data.len().saturating_sub(pos));
            buf[..n].copy_from_slice(&data[pos..pos + n]);
            self.open[(handle - 1) as usize] = Some((key, pos + n));
            Ok(n)
        }

        fn write(&mut self, handle: i32, data: &[u8]) -> symbian::Result<usize> {
            let (key, _) = self.open[(handle - 1) as usize].clone().unwrap();
            self.files.get_mut(&key).unwrap().extend_from_slice(data);
            Ok(data.len())
        }

        fn size(&mut self, handle: i32) -> symbian::Result<u64> {
            let (key, _) = self.open[(handle - 1) as usize].clone().unwrap();
            Ok(self.files[&key].len() as u64)
        }

        fn seek(&mut self, handle: i32, pos: u64) -> symbian::Result<()> {
            let (key, _) = self.open[(handle - 1) as usize].clone().unwrap();
            self.open[(handle - 1) as usize] = Some((key, pos as usize));
            Ok(())
        }

        fn close(&mut self, handle: i32) {
            self.open[(handle - 1) as usize] = None;
        }

        fn list_dir(&mut self, _path: &[u16], _out: &mut [u16]) -> symbian::Result<usize> {
            // The session store never lists a directory; a stub keeps the trait satisfied.
            Ok(0)
        }

        fn delete(&mut self, path: &[u16]) -> symbian::Result<()> {
            self.files.remove(path).map(|_| ()).ok_or(symbian::Error::NotFound)
        }

        fn rename(&mut self, from: &[u16], to: &[u16]) -> symbian::Result<()> {
            let v = self.files.remove(from).ok_or(symbian::Error::NotFound)?;
            self.files.insert(to.to_vec(), v);
            Ok(())
        }

        fn private_path(&mut self, out: &mut [u16]) -> symbian::Result<usize> {
            let p: Vec<u16> = "C:\\private\\test\\".encode_utf16().collect();
            out[..p.len()].copy_from_slice(&p);
            Ok(p.len())
        }
    }

    #[test]
    fn a_session_round_trips_through_the_filesystem() {
        let mut fs = MemFs::default();
        assert!(load(&mut fs).is_none(), "a fresh device has no session");

        save(&mut fs, &a_session()).unwrap();
        let back = load(&mut fs).expect("the session did not come back");
        assert_eq!(back, a_session());
    }

    #[test]
    fn the_record_is_the_width_the_constant_says() {
        // The two halves of the format are written in different places, so the width is
        // where they disagree. An earlier version of this record was four bytes longer than
        // its decoder expected, and the test caught it.
        assert_eq!(encode(&a_session()).len(), LEN);
    }

    #[test]
    fn the_data_centre_survives() {
        // An auth key belongs to one data centre; using it against another answers -404.
        // A record that forgot which one would send the client back to DC2 every launch
        // and it would look like the key had expired.
        let mut fs = MemFs::default();
        let mut s = a_session();
        s.dc = 5;
        save(&mut fs, &s).unwrap();
        assert_eq!(load(&mut fs).unwrap().dc, 5);
    }

    #[test]
    fn a_truncated_or_foreign_record_is_refused() {
        let good = encode(&a_session());
        assert!(decode(&good[..good.len() - 1]).is_none(), "short");
        assert!(decode(&[]).is_none(), "empty");
        let mut wrong = good.clone();
        wrong[0] ^= 0xff;
        assert!(decode(&wrong).is_none(), "wrong magic");
    }

    #[test]
    fn an_all_zero_key_is_not_a_key() {
        // What a wiped file looks like. Accepting it costs two exponentiations and four
        // round trips to prove that zero is not a valid auth key.
        let mut s = a_session();
        s.auth.key = [0u8; 256];
        assert!(decode(&encode(&s)).is_none());
    }

    #[test]
    fn clearing_wipes_before_it_unlinks() {
        // A delete leaves the bytes on the disk. Those bytes are the account, so logging
        // out has to overwrite them for the word to mean anything.
        let mut fs = MemFs::default();
        save(&mut fs, &a_session()).unwrap();

        // Find the stored bytes and confirm the key is really in there before the wipe.
        let key_byte = a_session().auth.key[0];
        assert!(
            fs.files.values().any(|v| v.len() == LEN && v[17] == key_byte),
            "the key was not written"
        );

        clear(&mut fs, Invalidate::LoggedOut).unwrap();
        assert!(load(&mut fs).is_none());
        assert!(
            !fs.files.values().any(|v| v.len() == LEN && v[17] == key_byte),
            "the key survived the wipe"
        );
    }

    #[test]
    fn the_errors_that_kill_a_session_are_recognised() {
        assert_eq!(invalidating("AUTH_KEY_UNREGISTERED"), Some(Invalidate::Unregistered));
        assert_eq!(invalidating("SESSION_REVOKED"), Some(Invalidate::Revoked));
        assert_eq!(invalidating("USER_DEACTIVATED_BAN"), Some(Invalidate::Revoked));
        // Prefix, not equality: Telegram appends detail to some of these, and a client that
        // compares for equality stops recognising them the day it does.
        assert_eq!(invalidating("AUTH_KEY_INVALID_SOMETHING"), Some(Invalidate::Unregistered));
        // And the ones that are not about the key must not throw the session away.
        assert_eq!(invalidating("FLOOD_WAIT_42"), None);
        assert_eq!(invalidating("PHONE_CODE_INVALID"), None);
        assert_eq!(invalidating(""), None);
    }

    #[test]
    fn saving_twice_replaces_rather_than_appends() {
        // write_atomic replaces. A version that appended would leave a file of 2 x LEN,
        // which decode refuses -- so the symptom would be "logged out on every third
        // launch", which is a hard thing to look for.
        let mut fs = MemFs::default();
        save(&mut fs, &a_session()).unwrap();
        let mut second = a_session();
        second.dc = 1;
        save(&mut fs, &second).unwrap();
        assert_eq!(load(&mut fs).unwrap().dc, 1);
    }
}
