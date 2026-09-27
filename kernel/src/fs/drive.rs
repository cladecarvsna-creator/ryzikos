//! Hard disks and CD/DVD drives, whichever controller they are on: the
//! old IDE one (ATA) or SATA (AHCI).

use alloc::string::String;
use alloc::vec::Vec;

use super::ahci;
use super::ata::{Ata, Atapi, IoError};

pub enum Disk {
    Ide(Ata),
    Sata(ahci::Drive),
}

impl Disk {
    pub fn sectors(&self) -> u64 {
        match self {
            Disk::Ide(a) => a.sectors(),
            Disk::Sata(s) => s.sectors(),
        }
    }

    pub fn read(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), IoError> {
        match self {
            Disk::Ide(a) => a.read(lba, buf),
            Disk::Sata(s) => s.read(lba, buf),
        }
    }

    pub fn write(&mut self, lba: u64, buf: &[u8]) -> Result<(), IoError> {
        match self {
            Disk::Ide(a) => a.write(lba, buf),
            Disk::Sata(s) => s.write(lba, buf),
        }
    }

    pub fn flush(&mut self) -> Result<(), IoError> {
        match self {
            Disk::Ide(a) => a.flush(),
            Disk::Sata(s) => s.flush(),
        }
    }

    pub fn model(&self) -> &str {
        match self {
            Disk::Ide(a) => &a.model,
            Disk::Sata(s) => &s.model,
        }
    }

    pub fn bus(&self) -> &'static str {
        match self {
            Disk::Ide(_) => "IDE",
            Disk::Sata(_) => "SATA",
        }
    }
}

pub enum CdDrive {
    Ide(Atapi),
    Sata(ahci::Drive),
}

impl CdDrive {
    /// Read 2048-byte sectors.
    pub fn read(&mut self, lba: u32, buf: &mut [u8]) -> Result<(), IoError> {
        match self {
            CdDrive::Ide(a) => a.read(lba, buf),
            CdDrive::Sata(s) => s.read_cd(lba, buf),
        }
    }

    pub fn model(&self) -> &str {
        match self {
            CdDrive::Ide(a) => &a.model,
            CdDrive::Sata(s) => &s.model,
        }
    }

    pub fn bus(&self) -> &'static str {
        match self {
            CdDrive::Ide(_) => "IDE",
            CdDrive::Sata(_) => "SATA",
        }
    }
}

/// Every hard disk and CD drive in the computer, IDE ones first.
pub fn find_all() -> (Vec<Disk>, Vec<CdDrive>) {
    let mut disks: Vec<Disk> = Ata::find_all().into_iter().map(Disk::Ide).collect();
    let mut cds: Vec<CdDrive> = Atapi::find_all().into_iter().map(CdDrive::Ide).collect();
    for d in ahci::find_all() {
        match d.kind {
            ahci::Kind::Disk => disks.push(Disk::Sata(d)),
            ahci::Kind::Cd => cds.push(CdDrive::Sata(d)),
        }
    }
    (disks, cds)
}

/// "QEMU HARDDISK (SATA)", or just the bus when the model is unknown.
pub fn describe(model: &str, bus: &str) -> String {
    if model.is_empty() {
        alloc::format!("{} drive", bus)
    } else {
        alloc::format!("{} ({})", model, bus)
    }
}
