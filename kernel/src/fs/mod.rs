//! Files that survive a restart: a FAT32 file system on the first ATA
//! hard disk. QEMU gets one with `-drive file=everos-disk.vhd` (see
//! run-windows.bat); a blank disk is formatted on first boot, and Windows
//! can open the same disk image to read the files.
//!
//! Without a hard disk the files live in memory until the next restart.
//!
//! Paths are absolute and use `/`: `/Users/root/Documents/notes.txt`.
//! The apps show them the Windows way, as `C:\Users\root\...`.

pub mod ata;
mod fat;
pub mod recycle;

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};

pub use fat::{same_name, valid_name, Info};

use crate::serial;
use crate::sync::IrqMutex;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    NotFound,
    Exists,
    NotADirectory,
    IsADirectory,
    BadName,
    Full,
    Io,
    Unformatted,
    NoDisk,
}

impl Error {
    pub fn message(self) -> &'static str {
        match self {
            Error::NotFound => "The file or folder was not found.",
            Error::Exists => "A file or folder with this name already exists.",
            Error::NotADirectory => "This is not a folder.",
            Error::IsADirectory => "This is a folder, not a file.",
            Error::BadName => "A name can't be empty or contain \\ / : * ? \" < > |",
            Error::Full => "The disk is full.",
            Error::Io => "The disk could not be read or written.",
            Error::Unformatted => "The disk has an unknown format.",
            Error::NoDisk => "There is no disk.",
        }
    }
}

/// Where files are kept.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Storage {
    /// A hard disk: files survive restarts.
    Disk,
    /// Memory only: files are lost on restart.
    Memory,
    None,
}

static VOLUME: IrqMutex<Option<fat::Volume>> = IrqMutex::new(None);
/// Counts changes, so File Explorer knows when to read a folder again.
static CHANGES: AtomicU32 = AtomicU32::new(0);

/// A number that changes whenever a file or folder does.
pub fn changes() -> u32 {
    CHANGES.load(Ordering::Relaxed)
}

fn changed<T>(r: Result<T, Error>) -> Result<T, Error> {
    CHANGES.fetch_add(1, Ordering::Relaxed);
    r
}
static STORAGE: IrqMutex<Storage> = IrqMutex::new(Storage::None);

/// Size of the disk in memory when there is no hard disk.
const RAM_DISK: usize = 8 * 1024 * 1024;

/// Find the disk and open its file system.
pub fn init() {
    let mut storage = Storage::None;
    let mut volume = None;
    if let Some(disk) = ata::Ata::find() {
        match fat::Volume::open(fat::Device::Ata(disk)) {
            Ok((v, formatted)) => {
                serial::write_str(if formatted {
                    "fs: formatted a blank disk as FAT32\n"
                } else {
                    "fs: mounted FAT32 disk\n"
                });
                volume = Some(v);
                storage = Storage::Disk;
            }
            Err(e) => {
                serial::write_str("fs: disk not usable: ");
                serial::write_str(e.message());
                serial::write_str("\n");
            }
        }
    } else {
        serial::write_str("fs: no hard disk\n");
    }
    if volume.is_none() {
        let dev = fat::Device::Ram(vec![0; RAM_DISK]);
        if let Ok((v, _)) = fat::Volume::open(dev) {
            serial::write_str("fs: files are kept in memory only\n");
            volume = Some(v);
            storage = Storage::Memory;
        }
    }
    *VOLUME.lock() = volume;
    *STORAGE.lock() = storage;
}

pub fn storage() -> Storage {
    *STORAGE.lock()
}

/// Size of the disk in bytes.
pub fn capacity() -> u64 {
    VOLUME.lock().as_ref().map_or(0, |v| v.bytes)
}

fn with<T>(f: impl FnOnce(&mut fat::Volume) -> Result<T, Error>) -> Result<T, Error> {
    match VOLUME.lock().as_mut() {
        Some(v) => f(v),
        None => Err(Error::NoDisk),
    }
}

/// What is in a folder: folders first, then files, each by name. Hidden
/// system items (see `hidden`) are left out.
pub fn list(path: &str) -> Result<Vec<Info>, Error> {
    let mut items = list_all(path)?;
    items.retain(|i| !hidden(&i.name));
    Ok(items)
}

/// Like `list`, hidden items included.
pub fn list_all(path: &str) -> Result<Vec<Info>, Error> {
    let mut items = with(|v| v.list(path))?;
    items.sort_by(|a, b| {
        b.dir.cmp(&a.dir).then_with(|| {
            a.name
                .chars()
                .flat_map(char::to_lowercase)
                .cmp(b.name.chars().flat_map(char::to_lowercase))
        })
    });
    Ok(items)
}

pub fn read(path: &str) -> Result<Vec<u8>, Error> {
    with(|v| v.read(path))
}

/// Create or replace a file.
pub fn write(path: &str, data: &[u8]) -> Result<(), Error> {
    changed(with(|v| v.write(path, data)))
}

pub fn create_dir(path: &str) -> Result<(), Error> {
    changed(with(|v| v.create_dir(path)))
}

/// Delete a file, or a folder and everything in it.
pub fn remove(path: &str) -> Result<(), Error> {
    changed(with(|v| v.remove(path)))
}

pub fn rename(path: &str, new_name: &str) -> Result<(), Error> {
    changed(with(|v| v.rename(path, new_name)))
}

/// Move a file or folder to a new path, which may be in another folder.
pub fn move_path(from: &str, to: &str) -> Result<(), Error> {
    if !exists(from) {
        return Err(Error::NotFound);
    }
    if exists(to) {
        return Err(Error::Exists);
    }
    let inside = to.len() > from.len()
        && same_name(&to[..from.len()], from)
        && to.as_bytes()[from.len()] == b'/';
    if inside {
        // a folder can't go into itself
        return Err(Error::BadName);
    }
    if same_name(&parent(from), &parent(to)) {
        return rename(from, file_name(to));
    }
    copy_tree(from, to)?;
    remove(from)
}

fn copy_tree(from: &str, to: &str) -> Result<(), Error> {
    if is_dir(from) {
        create_dir(to)?;
        for item in list_all(from)? {
            copy_tree(&join(from, &item.name), &join(to, &item.name))?;
        }
        Ok(())
    } else {
        write(to, &read(from)?)
    }
}

/// Items Explorer and the desktop don't show, like Windows hides them:
/// system names starting with `$` (the Recycle Bin) and AppData, where
/// apps keep their settings.
pub fn hidden(name: &str) -> bool {
    name.starts_with('$') || same_name(name, "AppData")
}

pub fn is_dir(path: &str) -> bool {
    with(|v| Ok(v.is_dir(path))).unwrap_or(false)
}

pub fn exists(path: &str) -> bool {
    with(|v| Ok(v.exists(path))).unwrap_or(false)
}

// ---- paths ------------------------------------------------------------------

/// `dir/name`.
pub fn join(dir: &str, name: &str) -> String {
    let mut s = String::from(dir.trim_end_matches('/'));
    s.push('/');
    s.push_str(name);
    s
}

/// The folder a path is in ("/" for the top).
pub fn parent(path: &str) -> String {
    let (dir, _) = fat::split(path);
    if dir.is_empty() {
        String::from("/")
    } else {
        String::from(dir)
    }
}

/// The last part of a path.
pub fn file_name(path: &str) -> &str {
    fat::split(path).1
}

/// A path the Windows way: `C:\Users\root`.
pub fn display(path: &str) -> String {
    let mut s = String::from("C:");
    for part in path.split('/').filter(|p| !p.is_empty()) {
        s.push('\\');
        s.push_str(part);
    }
    if s.len() == 2 {
        s.push('\\');
    }
    s
}

/// A typed path (`C:\Users`, `/Users` or `Users\root`) to the inner form.
pub fn parse(text: &str) -> String {
    let text = text.trim();
    let text = text
        .strip_prefix("C:")
        .or_else(|| text.strip_prefix("c:"))
        .unwrap_or(text);
    let mut s = String::new();
    for part in text
        .split(['/', '\\'])
        .filter(|p| !p.is_empty() && *p != ".")
    {
        if part == ".." {
            if let Some(i) = s.rfind('/') {
                s.truncate(i);
            }
            continue;
        }
        s.push('/');
        s.push_str(part);
    }
    if s.is_empty() {
        s.push('/');
    }
    s
}

/// A user's home folder.
pub fn home(user: &str) -> String {
    join("/Users", user)
}

/// Folders every home has, like on Windows.
pub const LIBRARIES: [&str; 5] = ["Desktop", "Documents", "Downloads", "Music", "Pictures"];

/// Create a user's home folder and its usual folders if they are missing,
/// with a welcome note the first time.
pub fn ensure_home(user: &str) {
    let home = home(user);
    let _ = create_dir("/Users");
    let _ = create_dir(&home);
    for lib in LIBRARIES {
        let path = join(&home, lib);
        if create_dir(&path).is_ok() && lib == "Documents" {
            let note = join(&path, "Welcome.txt");
            let _ = write(&note, WELCOME.as_bytes());
        }
    }
}

const WELCOME: &str = "Welcome to RyzikOS!\r\n\
\r\n\
This note is a file on the disk. Change it, press Ctrl+S, restart\r\n\
RyzikOS and it will still be here.\r\n\
\r\n\
Добро пожаловать в RyzikOS! Этот текст хранится на диске.\r\n\
Раскладка переключается Alt+Shift.\r\n";

/// A name in `dir` that is not taken yet: `base`, `base (2)`, ... with
/// `ext` (like ".txt", or "") after it.
pub fn unique_name(dir: &str, base: &str, ext: &str) -> String {
    use core::fmt::Write;
    for n in 1..1000 {
        let mut name = String::from(base);
        if n > 1 {
            let _ = write!(name, " ({})", n);
        }
        name.push_str(ext);
        if !exists(&join(dir, &name)) {
            return name;
        }
    }
    String::from(base)
}

/// Where apps keep a user's settings: `/Users/<name>/AppData`.
pub fn app_data(user: &str) -> String {
    let dir = join(&home(user), "AppData");
    let _ = create_dir(&dir);
    dir
}
