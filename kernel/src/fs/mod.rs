//! Files that survive a restart: a FAT32 file system on the first ATA
//! hard disk. QEMU gets one with `-drive file=everos-disk.vhd` (see
//! run-windows.bat); a blank disk is formatted on first boot, and Windows
//! can open the same disk image to read the files.
//!
//! Without a hard disk the files live in memory until the next restart.
//!
//! Paths are absolute and use `/`: `/Users/root/Documents/notes.txt`.
//! A CD or DVD in the drive shows up read only under `/Disc`.

mod ahci;
pub mod ata;
pub mod drive;
mod fat;
mod iso9660;
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
    /// Only zeros: never formatted.
    Blank,
    NoDisk,
    ReadOnly,
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
            Error::Blank => "The disk is blank.",
            Error::NoDisk => "There is no disk or disc here.",
            Error::ReadOnly => "The disc can only be read, not changed.",
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
/// Where the first CD or DVD drive's disc is. More drives get "/Disc 2"...
pub const DISC_PATH: &str = "/Disc";
/// Counts changes, so File Explorer knows when to read a folder again.
static CHANGES: AtomicU32 = AtomicU32::new(0);

/// A CD or DVD drive and the disc in it, if any.
struct DiscSlot {
    drive: drive::CdDrive,
    disc: Option<iso9660::Disc>,
    path: String,
}

/// A hard disk besides the system disk, at "/Disk 2" and so on. Only
/// FAT32 disks can be opened; the others are listed with why not.
struct OtherDisk {
    path: String,
    name: String,
    detail: String,
    bytes: u64,
    volume: Option<fat::Volume>,
    /// The disk itself, while it has no volume open.
    disk: Option<drive::Disk>,
    status: &'static str,
}

static DISCS: IrqMutex<Vec<DiscSlot>> = IrqMutex::new(Vec::new());
static OTHERS: IrqMutex<Vec<OtherDisk>> = IrqMutex::new(Vec::new());
/// The system disk's model and bus, for Files and Settings.
static SYSTEM_DETAIL: IrqMutex<String> = IrqMutex::new(String::new());

/// A number that changes whenever a file or folder does.
pub fn changes() -> u32 {
    CHANGES.load(Ordering::Relaxed)
}

/// Count a change when something may have changed: a folder that was
/// already there, for one, changes nothing, and the desktop redraws on
/// every change. A disk error or a full disk can stop halfway, so those
/// count.
fn changed<T>(r: Result<T, Error>) -> Result<T, Error> {
    if matches!(r, Ok(_) | Err(Error::Io | Error::Full)) {
        CHANGES.fetch_add(1, Ordering::Relaxed);
    }
    r
}
static STORAGE: IrqMutex<Storage> = IrqMutex::new(Storage::None);

/// Size of the disk in memory when there is no hard disk.
const RAM_DISK: usize = 8 * 1024 * 1024;

fn say(parts: &[&str]) {
    for p in parts {
        serial::write_str(p);
    }
    serial::write_str("\n");
}

/// A file an installed RyzikOS keeps on its disk; the disk that has it
/// holds the system's files, and the live CD's GRUB looks for it.
pub const INSTALLED_MARK: &str = "/boot/ryzikos-installed.txt";

/// Find the disks and drives and open their file systems. From the live
/// CD the system's files are kept in memory and no disk is changed.
/// Otherwise the disk RyzikOS is installed on keeps them, or else the
/// first disk that holds FAT32 (or is blank, and gets formatted).
pub fn init() {
    let live = crate::multiboot::live();
    let mut storage = Storage::None;
    let (disks, cds) = drive::find_all();
    if disks.is_empty() {
        serial::write_str("fs: no hard disk\n");
    }
    if live {
        serial::write_str("fs: live CD, the system's files are kept in memory\n");
    }
    // open every disk first, then pick the system disk
    let mut opened = Vec::new();
    for disk in disks {
        let detail = drive::describe(disk.model(), disk.bus());
        let bytes = disk.sectors() * 512;
        let mb = alloc::format!("{} MiB", bytes >> 20);
        say(&["fs: found disk ", &detail, ", ", &mb]);
        let result = fat::Volume::open(fat::Device::Disk(disk), false);
        opened.push((detail, bytes, result));
    }
    let system = if live {
        None
    } else {
        let installed = opened.iter_mut().position(|(_, _, r)| {
            r.as_mut().is_ok_and(|(v, _)| v.exists(INSTALLED_MARK))
        });
        installed.or_else(|| {
            opened
                .iter()
                .position(|(_, _, r)| matches!(r, Ok(_) | Err((Error::Blank, _))))
        })
    };
    let mut volume = None;
    let mut others = Vec::new();
    let mut number = 0;
    for (i, (detail, bytes, result)) in opened.into_iter().enumerate() {
        if Some(i) == system {
            let result = match result {
                Err((Error::Blank, dev)) => fat::Volume::open(dev, true),
                r => r,
            };
            match result {
                Ok((v, formatted)) => {
                    serial::write_str(if formatted {
                        "fs: formatted a blank disk as FAT32\n"
                    } else {
                        "fs: mounted FAT32 disk\n"
                    });
                    volume = Some(v);
                    storage = Storage::Disk;
                    *SYSTEM_DETAIL.lock() = detail;
                    continue;
                }
                Err((e, dev)) => {
                    say(&["fs: disk not usable: ", e.message()]);
                    others.push(other(&mut number, detail, bytes, Err((e, dev))));
                }
            }
        } else {
            others.push(other(&mut number, detail, bytes, result));
        }
    }
    if volume.is_none() {
        let dev = fat::Device::Ram(vec![0; RAM_DISK]);
        if let Ok((v, _)) = fat::Volume::open(dev, true) {
            serial::write_str("fs: files are kept in memory only\n");
            volume = Some(v);
            storage = Storage::Memory;
        }
    }
    *VOLUME.lock() = volume;
    *STORAGE.lock() = storage;
    *OTHERS.lock() = others;
    let slots = cds
        .into_iter()
        .enumerate()
        .map(|(i, drive)| {
            say(&["fs: found CD/DVD drive ", &drive::describe(drive.model(), drive.bus())]);
            DiscSlot {
                drive,
                disc: None,
                path: if i == 0 {
                    String::from(DISC_PATH)
                } else {
                    alloc::format!("{} {}", DISC_PATH, i + 1)
                },
            }
        })
        .collect();
    *DISCS.lock() = slots;
    refresh_disc();
}

/// A disk that doesn't keep the system's files, at "/Disk N".
fn other(
    number: &mut usize,
    detail: String,
    bytes: u64,
    result: Result<(fat::Volume, bool), (Error, fat::Device)>,
) -> OtherDisk {
    *number += 1;
    let name = alloc::format!("Disk {}", number);
    let path = alloc::format!("/{}", name);
    let (volume, disk, status) = match result {
        Ok((v, _)) => {
            say(&["fs: ", &name, " has FAT32"]);
            (Some(v), None, "FAT32")
        }
        Err((e, dev)) => {
            let status = match e {
                Error::Blank => "Blank",
                Error::Unformatted => "Unknown format, can't be opened",
                _ => "Can't be read",
            };
            (None, dev.into_disk(), status)
        }
    };
    OtherDisk {
        path,
        name,
        detail,
        bytes,
        volume,
        disk,
        status,
    }
}

/// Read the disc in a drive, if one is in.
fn mount_disc(slot: &mut DiscSlot) -> bool {
    slot.disc = iso9660::Disc::mount(&mut slot.drive);
    if let Some(d) = &slot.disc {
        serial::write_str("fs: disc in the drive: ");
        serial::write_str(&d.label);
        serial::write_str("\n");
    }
    slot.disc.is_some()
}

/// The name of the first disc in a drive, if there is one.
pub fn disc_label() -> Option<String> {
    DISCS
        .lock()
        .iter()
        .find_map(|s| s.disc.as_ref().map(|d| d.label.clone()))
}

/// Check the drives again: a disc may have been put in or taken out.
/// Returns whether any drive has a disc.
pub fn refresh_disc() -> bool {
    let mut any = false;
    for slot in DISCS.lock().iter_mut() {
        let still = slot.disc.is_some() && iso9660::present(&mut slot.drive);
        if !still {
            let had = slot.disc.take().is_some();
            if mount_disc(slot) || had {
                CHANGES.fetch_add(1, Ordering::Relaxed);
            }
        }
        any |= slot.disc.is_some();
    }
    any
}

/// The part of `path` under the mount point `at`, if it is there.
fn under<'a>(path: &'a str, at: &str) -> Option<&'a str> {
    let rest = path.get(at.len()..)?;
    if !same_name(&path[..at.len()], at) {
        return None;
    }
    if rest.is_empty() || rest.starts_with('/') {
        Some(if rest.is_empty() { "/" } else { rest })
    } else {
        None
    }
}

/// Which file system a path is on, and the path inside it.
enum Target<'a> {
    System(&'a str),
    Other(usize, &'a str),
    Disc(usize, &'a str),
}

fn route(path: &str) -> Target<'_> {
    for (i, s) in DISCS.lock().iter().enumerate() {
        if let Some(inner) = under(path, &s.path) {
            return Target::Disc(i, inner);
        }
    }
    for (i, d) in OTHERS.lock().iter().enumerate() {
        if let Some(inner) = under(path, &d.path) {
            return Target::Other(i, inner);
        }
    }
    Target::System(path)
}

/// Whether a path is on a CD or DVD (and so can't be changed).
pub fn is_on_disc(path: &str) -> bool {
    matches!(route(path), Target::Disc(..))
}

/// Whether files under a path can't be changed: on a disc, or on a disk
/// that can't be opened.
pub fn is_read_only(path: &str) -> bool {
    match route(path) {
        Target::System(_) => false,
        Target::Disc(..) => true,
        Target::Other(i, _) => OTHERS.lock()[i].volume.is_none(),
    }
}

fn with_disc<T>(
    i: usize,
    f: impl FnOnce(&iso9660::Disc, &mut drive::CdDrive) -> Result<T, Error>,
) -> Result<T, Error> {
    let mut slots = DISCS.lock();
    let DiscSlot { drive, disc, .. } = &mut slots[i];
    match disc {
        Some(d) => f(d, drive),
        None => Err(Error::NoDisk),
    }
}

/// Run `f` on the FAT32 volume `path` is on, with the path inside it.
fn on_volume<T>(path: &str, f: impl FnOnce(&mut fat::Volume, &str) -> Result<T, Error>) -> Result<T, Error> {
    match route(path) {
        Target::System(p) => with(|v| f(v, p)),
        Target::Other(i, p) => match OTHERS.lock()[i].volume.as_mut() {
            Some(v) => f(v, p),
            None => Err(Error::Unformatted),
        },
        Target::Disc(..) => Err(Error::ReadOnly),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DriveKind {
    /// The disk with the system's files (or memory, without one).
    System,
    Disk,
    Cd,
}

/// A disk or drive, as Files shows it.
#[derive(Clone)]
pub struct DriveInfo {
    pub kind: DriveKind,
    pub name: String,
    /// Where its files are.
    pub path: String,
    /// Whether the files can be read: a disc is in, the format is known.
    pub ready: bool,
    /// Model and bus: "QEMU HARDDISK (SATA)".
    pub detail: String,
    /// "FAT32", "No disc", "Unknown format"...
    pub status: String,
    pub bytes: u64,
    pub free: Option<u64>,
}

/// Every disk and drive in the computer, the system disk first.
pub fn drives() -> Vec<DriveInfo> {
    let mut out = Vec::new();
    let memory = storage() != Storage::Disk;
    let (bytes, free) = VOLUME
        .lock()
        .as_ref()
        .map_or((0, None), |v| (v.bytes, Some(v.free_bytes())));
    out.push(DriveInfo {
        kind: DriveKind::System,
        name: String::from(if memory { "Memory" } else { "System Disk" }),
        path: String::from("/"),
        ready: true,
        detail: if memory {
            String::from("No hard disk: files are lost on restart")
        } else {
            SYSTEM_DETAIL.lock().clone()
        },
        status: String::from("FAT32"),
        bytes,
        free,
    });
    for d in OTHERS.lock().iter() {
        out.push(DriveInfo {
            kind: DriveKind::Disk,
            name: d.name.clone(),
            path: d.path.clone(),
            ready: d.volume.is_some(),
            detail: d.detail.clone(),
            status: String::from(d.status),
            bytes: d.volume.as_ref().map_or(d.bytes, |v| v.bytes),
            free: d.volume.as_ref().map(|v| v.free_bytes()),
        });
    }
    let slots = DISCS.lock();
    let many = slots.len() > 1;
    for (i, s) in slots.iter().enumerate() {
        let drive_name = if many {
            alloc::format!("CD/DVD Drive {}", i + 1)
        } else {
            String::from("CD/DVD Drive")
        };
        out.push(match &s.disc {
            Some(d) => DriveInfo {
                kind: DriveKind::Cd,
                name: d.label.clone(),
                path: s.path.clone(),
                ready: true,
                detail: alloc::format!("{}, {}", drive_name, drive::describe(s.drive.model(), s.drive.bus())),
                status: String::from("Disc, read only"),
                bytes: d.bytes(),
                free: None,
            },
            None => DriveInfo {
                kind: DriveKind::Cd,
                name: drive_name,
                path: s.path.clone(),
                ready: false,
                detail: drive::describe(s.drive.model(), s.drive.bus()),
                status: String::from("No disc"),
                bytes: 0,
                free: None,
            },
        });
    }
    out
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
    let mut items = match route(path) {
        Target::Disc(i, inner) => with_disc(i, |d, dev| d.list(dev, inner))?,
        _ => on_volume(path, |v, p| v.list(p))?,
    };
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
    match route(path) {
        Target::Disc(i, inner) => with_disc(i, |d, dev| d.read(dev, inner)),
        _ => on_volume(path, |v, p| v.read(p)),
    }
}

/// Create or replace a file.
pub fn write(path: &str, data: &[u8]) -> Result<(), Error> {
    changed(on_volume(path, |v, p| v.write(p, data)))
}

pub fn create_dir(path: &str) -> Result<(), Error> {
    changed(on_volume(path, |v, p| v.create_dir(p)))
}

/// Delete a file, or a folder and everything in it.
pub fn remove(path: &str) -> Result<(), Error> {
    changed(on_volume(path, |v, p| v.remove(p)))
}

pub fn rename(path: &str, new_name: &str) -> Result<(), Error> {
    changed(on_volume(path, |v, p| v.rename(p, new_name)))
}

/// Move a file or folder to a new path, which may be in another folder.
pub fn move_path(from: &str, to: &str) -> Result<(), Error> {
    if is_on_disc(from) || is_on_disc(to) {
        return Err(Error::ReadOnly);
    }
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
    match route(path) {
        Target::Disc(i, inner) => with_disc(i, |d, dev| Ok(d.is_dir(dev, inner))).unwrap_or(false),
        _ => on_volume(path, |v, p| Ok(v.is_dir(p))).unwrap_or(false),
    }
}

pub fn exists(path: &str) -> bool {
    match route(path) {
        Target::Disc(i, inner) => with_disc(i, |d, dev| Ok(d.exists(dev, inner))).unwrap_or(false),
        _ => on_volume(path, |v, p| Ok(v.exists(p))).unwrap_or(false),
    }
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

/// A path as the apps show it: `/Users/root`.
pub fn display(path: &str) -> String {
    let mut s = String::new();
    for part in path.split('/').filter(|p| !p.is_empty()) {
        s.push('/');
        s.push_str(part);
    }
    if s.is_empty() {
        s.push('/');
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

/// Folders every home has.
pub const LIBRARIES: [&str; 6] = [
    "Desktop",
    "Documents",
    "Downloads",
    "Music",
    "Pictures",
    "Videos",
];

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
    if !is_dir(&dir) {
        let _ = create_dir(&dir);
    }
    dir
}

// ---- installing RyzikOS -------------------------------------------------------

/// A disk RyzikOS can be installed on (one of the other disks).
#[derive(Clone)]
pub struct InstallDisk {
    pub index: usize,
    pub name: String,
    /// Model and bus.
    pub detail: String,
    pub bytes: u64,
    /// "FAT32", "Blank", "Unknown format..."
    pub status: &'static str,
    /// FAT32 in the first partition with room before it for the boot
    /// loader: RyzikOS can go next to the files without formatting.
    pub keep: bool,
    /// Bytes in use, for a FAT32 disk.
    pub used: u64,
}

/// The disks RyzikOS can be installed on. `boot_sectors` is how much
/// room the boot loader needs at the start of the disk.
pub fn install_disks(boot_sectors: u64) -> Vec<InstallDisk> {
    let mut others = OTHERS.lock();
    others
        .iter_mut()
        .enumerate()
        .map(|(index, d)| {
            let (keep, used) = match d.volume.as_mut() {
                Some(v) => {
                    let mut mbr = [0u8; 512];
                    let first = v.read_sectors(0, &mut mbr).is_ok()
                        && mbr[510..] == [0x55, 0xaa]
                        && u32::from_le_bytes([mbr[454], mbr[455], mbr[456], mbr[457]]) as u64
                            == v.start();
                    (first && v.start() > boot_sectors, v.bytes - v.free_bytes())
                }
                None => (false, 0),
            };
            InstallDisk {
                index,
                name: d.name.clone(),
                detail: d.detail.clone(),
                bytes: d.volume.as_ref().map_or(d.bytes, |v| v.bytes),
                status: d.status,
                keep,
                used,
            }
        })
        .collect()
}

/// Where another disk's files are: "/Disk 1".
pub fn other_path(index: usize) -> String {
    OTHERS.lock()[index].path.clone()
}

/// Erase another disk: one FAT32 partition over all of it (up to 64 GiB).
pub fn erase_disk(index: usize) -> Result<(), Error> {
    let mut others = OTHERS.lock();
    let d = others.get_mut(index).ok_or(Error::NoDisk)?;
    let mut dev = match (d.volume.take(), d.disk.take()) {
        (Some(v), _) => v.into_device(),
        (None, Some(disk)) => fat::Device::Disk(disk),
        (None, None) => return Err(Error::NoDisk),
    };
    let formatted = fat::format(&mut dev);
    let result = match formatted {
        Ok(_) => fat::Volume::open(dev, false),
        Err(e) => Err((e, dev)),
    };
    CHANGES.fetch_add(1, Ordering::Relaxed);
    match result {
        Ok((v, _)) => {
            d.volume = Some(v);
            d.status = "FAT32";
            say(&["fs: erased ", &d.name, ", now FAT32"]);
            Ok(())
        }
        Err((e, dev)) => {
            d.disk = dev.into_disk();
            d.status = "Can't be read";
            Err(e)
        }
    }
}

/// Make another disk start GRUB: `boot` (GRUB's boot.img) goes in the
/// first sector, keeping the partition table, and `core` (core.img)
/// right after it, before the first partition. The first partition is
/// marked active.
pub fn write_boot_loader(index: usize, boot: &[u8], core: &[u8]) -> Result<(), Error> {
    let mut others = OTHERS.lock();
    let v = others
        .get_mut(index)
        .and_then(|d| d.volume.as_mut())
        .ok_or(Error::NoDisk)?;
    let sectors = core.len().div_ceil(512) as u64;
    if boot.len() < 440 || sectors + 1 >= v.start() {
        return Err(Error::Full);
    }
    let mut mbr = [0u8; 512];
    v.read_sectors(0, &mut mbr)?;
    mbr[..440].copy_from_slice(&boot[..440]);
    for p in 0..4 {
        mbr[446 + p * 16] = if p == 0 { 0x80 } else { 0 };
    }
    let mut padded = core.to_vec();
    padded.resize(sectors as usize * 512, 0);
    v.write_sectors(1, &padded)?;
    v.write_sectors(0, &mbr)?;
    Ok(())
}
