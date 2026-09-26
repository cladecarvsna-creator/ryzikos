//! ATA hard disks in PIO mode: the IDE controller QEMU, VirtualBox and
//! VMware emulate. The CPU copies every sector through the data port,
//! which is slow on real hardware but simple and plenty for text files.

use crate::port::{inb, inw, outb, outw};

const SECTOR: usize = 512;

// status register bits
const ERR: u8 = 0x01;
const DRQ: u8 = 0x08;
const DF: u8 = 0x20;
const BSY: u8 = 0x80;

const IDENTIFY: u8 = 0xec;
const READ: u8 = 0x20;
const WRITE: u8 = 0x30;
const FLUSH: u8 = 0xe7;

/// How long to poll the status register before giving up.
const TIMEOUT: u32 = 5_000_000;

#[derive(Debug)]
pub struct IoError;

pub struct Ata {
    base: u16,
    control: u16,
    slave: bool,
    sectors: u64,
}

impl Ata {
    /// Find the first ATA hard disk on the two IDE channels. CD drives
    /// (ATAPI) are skipped.
    pub fn find() -> Option<Ata> {
        for (base, control) in [(0x1f0, 0x3f6), (0x170, 0x376)] {
            for slave in [false, true] {
                let mut disk = Ata {
                    base,
                    control,
                    slave,
                    sectors: 0,
                };
                if disk.identify() {
                    return Some(disk);
                }
            }
        }
        None
    }

    pub fn sectors(&self) -> u64 {
        self.sectors
    }

    fn status(&self) -> u8 {
        unsafe { inb(self.base + 7) }
    }

    /// Reading the alternate status four times takes the 400 ns the
    /// drive needs after being selected.
    fn delay(&self) {
        for _ in 0..4 {
            unsafe { inb(self.control) };
        }
    }

    fn select(&self, lba_top: u8) {
        unsafe {
            outb(
                self.base + 6,
                0xe0 | (self.slave as u8) << 4 | (lba_top & 0x0f),
            )
        };
        self.delay();
    }

    fn wait_not_busy(&self) -> Result<u8, IoError> {
        for _ in 0..TIMEOUT {
            let s = self.status();
            if s == 0xff {
                return Err(IoError); // nothing on this channel
            }
            if s & BSY == 0 {
                return Ok(s);
            }
            core::hint::spin_loop();
        }
        Err(IoError)
    }

    /// Wait until the drive has a sector for us or wants one.
    fn wait_data(&self) -> Result<(), IoError> {
        for _ in 0..TIMEOUT {
            let s = self.status();
            if s & BSY == 0 {
                if s & (ERR | DF) != 0 {
                    return Err(IoError);
                }
                if s & DRQ != 0 {
                    return Ok(());
                }
            }
            core::hint::spin_loop();
        }
        Err(IoError)
    }

    fn identify(&mut self) -> bool {
        unsafe {
            // no interrupts from the drive: we poll
            outb(self.control, 0x02);
            if inb(self.base + 7) == 0xff {
                return false; // floating bus: no controller
            }
            outb(self.base + 6, 0xa0 | (self.slave as u8) << 4);
            self.delay();
            for reg in 2..=5 {
                outb(self.base + reg, 0);
            }
            outb(self.base + 7, IDENTIFY);
            if inb(self.base + 7) == 0 {
                return false; // no drive
            }
            if self.wait_not_busy().is_err() {
                return false;
            }
            // ATAPI and SATA devices put a signature here instead
            if inb(self.base + 4) != 0 || inb(self.base + 5) != 0 {
                return false;
            }
            if self.wait_data().is_err() {
                return false;
            }
            let mut id = [0u16; 256];
            for w in id.iter_mut() {
                *w = inw(self.base);
            }
            let lba28 = id[60] as u64 | (id[61] as u64) << 16;
            // only LBA28 commands are used, so stay below 128 GiB
            self.sectors = lba28.min(0x0fff_ffff);
            self.sectors > 0
        }
    }

    fn command(&self, cmd: u8, lba: u64, count: usize) -> Result<(), IoError> {
        if lba + count as u64 > self.sectors || count == 0 || count > 256 {
            return Err(IoError);
        }
        self.wait_not_busy()?;
        self.select((lba >> 24) as u8);
        unsafe {
            outb(self.base + 2, count as u8); // 256 is sent as 0
            outb(self.base + 3, lba as u8);
            outb(self.base + 4, (lba >> 8) as u8);
            outb(self.base + 5, (lba >> 16) as u8);
            outb(self.base + 7, cmd);
        }
        Ok(())
    }

    /// Read whole sectors into `buf`.
    pub fn read(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), IoError> {
        for (i, chunk) in buf.chunks_mut(128 * SECTOR).enumerate() {
            let count = chunk.len() / SECTOR;
            self.command(READ, lba + (i * 128) as u64, count)?;
            for sector in chunk.chunks_mut(SECTOR) {
                self.wait_data()?;
                for pair in sector.chunks_mut(2) {
                    let w = unsafe { inw(self.base) };
                    pair[0] = w as u8;
                    pair[1] = (w >> 8) as u8;
                }
            }
        }
        Ok(())
    }

    /// Write whole sectors from `buf`.
    pub fn write(&mut self, lba: u64, buf: &[u8]) -> Result<(), IoError> {
        for (i, chunk) in buf.chunks(128 * SECTOR).enumerate() {
            let count = chunk.len() / SECTOR;
            self.command(WRITE, lba + (i * 128) as u64, count)?;
            for sector in chunk.chunks(SECTOR) {
                self.wait_data()?;
                for pair in sector.chunks(2) {
                    unsafe { outw(self.base, pair[0] as u16 | (pair[1] as u16) << 8) };
                }
            }
            let s = self.wait_not_busy()?;
            if s & (ERR | DF) != 0 {
                return Err(IoError);
            }
        }
        Ok(())
    }

    /// Ask the drive to put its write cache on the disk.
    pub fn flush(&mut self) -> Result<(), IoError> {
        self.wait_not_busy()?;
        self.select(0);
        unsafe { outb(self.base + 7, FLUSH) };
        let s = self.wait_not_busy()?;
        if s & (ERR | DF) != 0 {
            return Err(IoError);
        }
        Ok(())
    }
}
