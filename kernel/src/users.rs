//! User accounts: a small table of names and password hashes that the
//! login screen checks. At first there is one user, `root`, with an empty
//! password; the shell's `useradd` and `passwd` add users and passwords.
//!
//! The table is saved in [`FILE`] on the system disk every time it
//! changes and read back at boot, so accounts survive a restart, the
//! blue screen's included.
//!
//! Passwords are kept only as salted hashes, never as text. The hash is
//! FNV-1a stretched over many rounds: fine for a hobby OS, not a real
//! password hash like Argon2.

use alloc::string::String;
use alloc::vec::Vec;

use crate::sync::IrqMutex;
use crate::{fs, serial, StackString};

/// Where the table is kept: one `name salt hash` line per user, the
/// numbers in hex. Hidden from Explorer, like the Recycle Bin.
pub const FILE: &str = "/$users.txt";

pub const MAX_NAME: usize = 16;
pub const MAX_USERS: usize = 8;

struct User {
    name: String,
    salt: u64,
    hash: u64,
}

static USERS: IrqMutex<Vec<User>> = IrqMutex::new(Vec::new());
/// Who is signed in, as an index into the table.
static CURRENT: IrqMutex<Option<usize>> = IrqMutex::new(None);

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    BadName,
    Exists,
    Full,
    NoSuchUser,
}

impl Error {
    pub fn message(&self) -> &'static str {
        match self {
            Error::BadName => "names are 1 to 16 letters, digits, '-' or '_'",
            Error::Exists => "that user already exists",
            Error::Full => "the user table is full",
            Error::NoSuchUser => "no such user",
        }
    }
}

fn hash(salt: u64, password: &str) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325 ^ salt;
    for _ in 0..64 {
        for &b in password.as_bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        h ^= h >> 29;
        h = h.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    }
    h
}

/// A different salt for every password, from the time stamp counter.
fn new_salt() -> u64 {
    let (lo, hi): (u32, u32);
    unsafe { core::arch::asm!("rdtsc", out("eax") lo, out("edx") hi, options(nomem, nostack)) };
    ((hi as u64) << 32 | lo as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15)
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Create the initial table: `root` with no password.
pub fn init() {
    let mut users = USERS.lock();
    if users.is_empty() {
        let salt = new_salt();
        users.push(User {
            name: String::from("root"),
            salt,
            hash: hash(salt, ""),
        });
    }
}

/// At boot, after the disks: read the saved table, if there is one.
pub fn load() {
    let Ok(data) = fs::read(FILE) else {
        return;
    };
    let mut loaded: Vec<User> = Vec::new();
    for line in String::from_utf8_lossy(&data).lines() {
        let mut parts = line.split_whitespace();
        let (Some(name), Some(salt), Some(hash), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let (Ok(salt), Ok(hash)) = (u64::from_str_radix(salt, 16), u64::from_str_radix(hash, 16))
        else {
            continue;
        };
        if valid_name(name) && !loaded.iter().any(|u| u.name == name) && loaded.len() < MAX_USERS {
            loaded.push(User {
                name: String::from(name),
                salt,
                hash,
            });
        }
    }
    if loaded.is_empty() {
        serial::write_str("users: the saved accounts can't be read, keeping root\n");
        return;
    }
    let mut users = USERS.lock();
    // root is always there, first
    if !loaded.iter().any(|u| u.name == "root") {
        if let Some(root) = users.iter().position(|u| u.name == "root") {
            loaded.insert(0, users.remove(root));
            loaded.truncate(MAX_USERS);
        }
    }
    *users = loaded;
    serial::write_str("users: loaded the saved accounts\n");
}

/// Write the table to [`FILE`]. The text is made under the lock, the
/// file is written after it is let go.
fn save() {
    let text = {
        let users = USERS.lock();
        let mut text = String::new();
        for u in users.iter() {
            text.push_str(&alloc::format!("{} {:016x} {:016x}\r\n", u.name, u.salt, u.hash));
        }
        text
    };
    if fs::write(FILE, text.as_bytes()).is_err() {
        serial::write_str("users: could not save the accounts\n");
    }
}

pub fn add(name: &str, password: &str) -> Result<(), Error> {
    if !valid_name(name) {
        return Err(Error::BadName);
    }
    let mut users = USERS.lock();
    if users.iter().any(|u| u.name == name) {
        return Err(Error::Exists);
    }
    if users.len() >= MAX_USERS {
        return Err(Error::Full);
    }
    let salt = new_salt();
    users.push(User {
        name: String::from(name),
        salt,
        hash: hash(salt, password),
    });
    drop(users);
    save();
    Ok(())
}

pub fn set_password(name: &str, password: &str) -> Result<(), Error> {
    let mut users = USERS.lock();
    let user = users
        .iter_mut()
        .find(|u| u.name == name)
        .ok_or(Error::NoSuchUser)?;
    user.salt = new_salt();
    user.hash = hash(user.salt, password);
    drop(users);
    save();
    Ok(())
}

pub fn count() -> usize {
    USERS.lock().len()
}

pub type Name = StackString<MAX_NAME>;

fn copy(name: &str) -> Name {
    let mut out = Name::new();
    out.push_str(name);
    out
}

/// The name of user `i`, if there is one. It is a copy, so no lock is
/// held while the caller uses it.
pub fn name(i: usize) -> Option<Name> {
    USERS.lock().get(i).map(|u| copy(&u.name))
}

/// Whether user `i` has a password other than the empty one.
pub fn has_password(i: usize) -> bool {
    USERS
        .lock()
        .get(i)
        .is_some_and(|u| u.hash != hash(u.salt, ""))
}

/// Check a password and, if it is right, sign the user in.
pub fn sign_in(i: usize, password: &str) -> bool {
    let ok = USERS
        .lock()
        .get(i)
        .is_some_and(|u| u.hash == hash(u.salt, password));
    if ok {
        *CURRENT.lock() = Some(i);
    }
    ok
}

pub fn sign_out() {
    *CURRENT.lock() = None;
}

/// Index of the signed-in user.
pub fn current() -> Option<usize> {
    *CURRENT.lock()
}

/// The name of the signed-in user.
pub fn current_name() -> Option<Name> {
    current().and_then(name)
}
