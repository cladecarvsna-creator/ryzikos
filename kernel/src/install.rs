//! Installing RyzikOS from the live CD onto a hard disk. GRUB on the CD
//! loads the files the installer needs next to the kernel (kernel.gz,
//! and GRUB's boot.img and core.img for the disk), so it works from a USB
//! stick too. The disk gets GRUB in its first sectors, before the FAT32
//! partition, and the kernel and a grub.cfg in /boot.

use alloc::format;
use alloc::string::String;

use crate::fs;
use crate::multiboot;
use crate::update;

/// Files an installed RyzikOS keeps in its /boot folder.
const KERNEL: &str = "boot/kernel.gz";
const GRUB_CFG: &str = "boot/grub/grub.cfg";
/// Tells the desktop to show Welcome after the first sign-in.
pub const WELCOME: &str = "/boot/welcome.txt";

/// How much room GRUB needs at the start of a disk, in sectors (the
/// first sector and core.img), if the live CD brought it.
pub fn boot_sectors() -> Option<u64> {
    multiboot::module("core.img").map(|c| 1 + c.len().div_ceil(512) as u64)
}

/// Whether this RyzikOS can install itself: started from the live CD,
/// which brought the files.
pub fn available() -> bool {
    multiboot::live()
        && multiboot::module("kernel.gz").is_some()
        && multiboot::module("boot.img").is_some()
        && boot_sectors().is_some()
}

/// Steps, for the installer's progress bar.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Formatting,
    Copying,
    BootLoader,
    Done,
}

/// Install on other disk `index`; `erase` formats it first. `step` is
/// told each step as it starts.
pub fn install(index: usize, erase: bool, mut step: impl FnMut(Step)) -> Result<(), String> {
    let missing = || String::from("start RyzikOS from its live CD to install it");
    let kernel = multiboot::module("kernel.gz").ok_or_else(missing)?;
    let boot = multiboot::module("boot.img").ok_or_else(missing)?;
    let core = multiboot::module("core.img").ok_or_else(missing)?;
    let failed = |what: &str, e: fs::Error| format!("{}: {}", what, e.message());

    if erase {
        step(Step::Formatting);
        fs::erase_disk(index).map_err(|e| failed("Formatting failed", e))?;
    }
    step(Step::Copying);
    let root = fs::other_path(index);
    let path = |p: &str| format!("{}/{}", root, p);
    for dir in ["boot", "boot/grub"] {
        if !fs::is_dir(&path(dir)) {
            fs::create_dir(&path(dir)).map_err(|e| failed("Can't make /boot", e))?;
        }
    }
    fs::write(&path(KERNEL), &kernel).map_err(|e| failed("Copying failed", e))?;
    fs::write(&path(GRUB_CFG), grub_cfg().as_bytes()).map_err(|e| failed("Copying failed", e))?;

    step(Step::BootLoader);
    fs::write_boot_loader(index, &boot, &core).map_err(|e| failed("Can't write the boot loader", e))?;
    // last: the disk counts as installed once everything is there
    let mark = format!("RyzikOS {}\n", update::version());
    fs::write(&path(&fs::INSTALLED_MARK[1..]), mark.as_bytes())
        .map_err(|e| failed("Copying failed", e))?;
    fs::write(&path(&WELCOME[1..]), b"show Welcome after the first sign-in\n")
        .map_err(|e| failed("Copying failed", e))?;
    crate::serial::write_str(&format!("install: RyzikOS installed on {}\n", root));
    step(Step::Done);
    Ok(())
}

/// GRUB's menu on the installed disk: this kernel, or a newer one that
/// Settings > Update downloaded (see update.rs). Esc during the one
/// second wait before an update shows the menu.
fn grub_cfg() -> String {
    format!(
        r#"# Written by the RyzikOS installer.
set timeout=0
set default=0
insmod all_video

set build={build}
if [ "$build" != 0 -a -f '/$Update/update.env' ]; then
    if load_env --file '/$Update/update.env' upd_build; then
        if [ "$upd_build" -gt "$build" ]; then
            set default=1
            set fallback=0
            set timeout_style=hidden
            set timeout=1
        fi
    fi
fi

menuentry "RyzikOS {version}" {{
    multiboot2 /boot/kernel.gz
    boot
}}

menuentry "RyzikOS {base}.$upd_build (update)" {{
    multiboot2 '/$Update/kernel.gz'
    boot
}}
"#,
        build = update::BUILD,
        version = update::version(),
        base = crate::gui::VERSION,
    )
}
