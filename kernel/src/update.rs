//! Updates from GitHub. Every push to main publishes a release with the
//! kernel (`kernel.gz`) and `version.txt`: the build number and the
//! kernel's SHA-256. RyzikOS downloads a newer kernel onto the system
//! disk, and on the next start the disc's GRUB boots that kernel instead
//! of its own (iso/boot/grub/grub.cfg). The disc itself can't be
//! written, so programs and pictures on it stay as they were.

use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

use sha2::{Digest, Sha256};

use crate::fiber::Fiber;
use crate::fs;
use crate::web::http;
use crate::web::url::Url;

/// This kernel's build number, the N in the release "1.0.N". 0 for a
/// build made on someone's own computer.
pub const BUILD: u32 = match option_env!("RYZIKOS_BUILD") {
    Some(s) => parse_build(s),
    None => 0,
};

const fn parse_build(s: &str) -> u32 {
    let b = s.as_bytes();
    let mut n = 0u32;
    let mut i = 0;
    while i < b.len() {
        assert!(b[i].is_ascii_digit(), "RYZIKOS_BUILD must be a number");
        n = n * 10 + (b[i] - b'0') as u32;
        i += 1;
    }
    n
}

/// Where the newest release's files are. A build can point it elsewhere
/// with RYZIKOS_UPDATES (tests use a local server).
const BASE: &str = match option_env!("RYZIKOS_UPDATES") {
    Some(url) => url,
    None => "https://github.com/cladecarvsna-creator/ryzikos/releases/latest/download/",
};

/// On the system disk; `$` hides it like the Recycle Bin. GRUB looks
/// for these exact names.
const DIR: &str = "/$Update";
const KERNEL: &str = "/$Update/kernel.gz";
const ENV: &str = "/$Update/update.env";

/// The version people see, like "1.0.7".
pub fn version() -> String {
    if BUILD == 0 {
        format!("{} (built from source)", crate::gui::VERSION)
    } else {
        format!("{}.{}", crate::gui::VERSION, BUILD)
    }
}

pub fn version_of(build: u32) -> String {
    format!("{}.{}", crate::gui::VERSION, build)
}

/// The newest release.
#[derive(Clone)]
pub struct Latest {
    pub build: u32,
    sha256: [u8; 32],
}

/// Ask GitHub for the newest release.
pub fn check() -> Result<Latest, String> {
    let text = download("version.txt")?;
    let text = String::from_utf8_lossy(&text);
    let mut words = text.split_whitespace();
    let build = words.next().and_then(|w| w.parse::<u32>().ok());
    let sha = words.next().and_then(parse_hex);
    match (build, sha) {
        (Some(build), Some(sha256)) => Ok(Latest { build, sha256 }),
        _ => Err(String::from("the release has no version.txt")),
    }
}

/// Download the release's kernel, check it and put it on the disk for
/// the next start.
pub fn install(latest: &Latest) -> Result<(), String> {
    if fs::storage() != fs::Storage::Disk {
        return Err(String::from("updates need a hard disk to be saved on"));
    }
    let kernel = download("kernel.gz")?;
    if Sha256::digest(&kernel).as_slice() != latest.sha256 {
        return Err(String::from("the download was damaged, try again"));
    }
    // a gzip file; GRUB unpacks it while loading
    if !kernel.starts_with(&[0x1f, 0x8b]) {
        return Err(String::from("the download is not a RyzikOS kernel"));
    }
    let failed = |e: fs::Error| format!("can't save the update: {}", e.message());
    if !fs::is_dir(DIR) {
        fs::create_dir(DIR).map_err(failed)?;
    }
    // GRUB only boots the kernel once update.env names its build, so
    // write that last: a half-written update is never started
    let _ = fs::remove(ENV);
    fs::write(KERNEL, &kernel).map_err(failed)?;
    fs::write(ENV, &grub_env(latest.build)).map_err(failed)?;
    crate::serial::write_str(&format!("update: build {} is ready\n", latest.build));
    Ok(())
}

/// The build waiting on the disk (or already running from it).
pub fn installed() -> Option<u32> {
    let data = fs::read(ENV).ok()?;
    let text = core::str::from_utf8(&data).ok()?;
    text.lines()
        .find_map(|l| l.strip_prefix("upd_build="))
        .and_then(|n| n.trim().parse().ok())
}

/// At start: forget an update that is older than this kernel, as when
/// a newer disc is used.
pub fn clean_up() {
    if let Some(build) = installed() {
        if BUILD != 0 && build < BUILD {
            let _ = fs::remove(DIR);
            crate::serial::write_str("update: removed an older update\n");
        }
    }
}

/// Undo a downloaded update, so the disc's own kernel starts again.
pub fn remove() {
    let _ = fs::remove(DIR);
}

fn download(name: &str) -> Result<Vec<u8>, String> {
    let url = Url::parse(&format!("{}{}", BASE, name)).ok_or("bad address")?;
    let r = http::get(&url, None)?;
    match r.status {
        200..=299 => Ok(r.body),
        404 => Err(String::from("no release with updates on GitHub yet")),
        s => Err(format!("GitHub answered {}", s)),
    }
}

/// A GRUB environment block: exactly 1024 bytes, padded with '#'.
fn grub_env(build: u32) -> Vec<u8> {
    let mut env = format!("# GRUB Environment Block\nupd_build={}\n", build).into_bytes();
    env.resize(1024, b'#');
    env
}

fn parse_hex(s: &str) -> Option<[u8; 32]> {
    let b = s.as_bytes();
    if b.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, pair) in b.chunks(2).enumerate() {
        let digit = |c: u8| (c as char).to_digit(16);
        out[i] = (digit(pair[0])? * 16 + digit(pair[1])?) as u8;
    }
    Some(out)
}

/// Where the updater is, for Settings to show.
#[derive(Clone, PartialEq, Eq)]
pub enum State {
    /// Not checked yet.
    Idle,
    Checking,
    UpToDate,
    Downloading(u32),
    /// Downloaded; the next start runs it.
    Ready(u32),
    Failed(String),
}

type Shared = Rc<RefCell<State>>;

/// Checks and downloads in a fiber, so the desktop keeps running.
pub struct Updater {
    state: Shared,
    fiber: Option<Fiber>,
}

impl Updater {
    pub fn new() -> Self {
        // an update downloaded before and not started yet
        let state = match installed() {
            Some(b) if b > BUILD && BUILD != 0 => State::Ready(b),
            _ => State::Idle,
        };
        Self {
            state: Rc::new(RefCell::new(state)),
            fiber: None,
        }
    }

    pub fn state(&self) -> State {
        self.state.borrow().clone()
    }

    pub fn busy(&self) -> bool {
        self.fiber.is_some()
    }

    /// Look for a newer release and download it.
    pub fn start(&mut self) {
        if self.fiber.is_some() || BUILD == 0 {
            return;
        }
        let state = self.state.clone();
        *state.borrow_mut() = State::Checking;
        self.fiber = Some(Fiber::new(move || {
            let set = |s: State| *state.borrow_mut() = s;
            let latest = match check() {
                Ok(l) => l,
                Err(e) => return set(State::Failed(e)),
            };
            if latest.build <= BUILD {
                return set(State::UpToDate);
            }
            if installed() == Some(latest.build) && fs::exists(KERNEL) {
                return set(State::Ready(latest.build));
            }
            set(State::Downloading(latest.build));
            match install(&latest) {
                Ok(()) => set(State::Ready(latest.build)),
                Err(e) => set(State::Failed(e)),
            }
        }));
    }

    /// Run the download a little. Returns true if the state changed.
    pub fn tick(&mut self) -> bool {
        let before = self.state();
        if let Some(f) = &mut self.fiber {
            if f.resume() {
                self.fiber = None;
            }
        }
        self.state() != before
    }
}
