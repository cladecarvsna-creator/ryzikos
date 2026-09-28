//! ATA hard disks in PIO mode: the IDE controller QEMU, VirtualBox and
//! VMware emulate. The CPU copies every sector through the data port,
//! which is slow on real hardware but simple and plenty for text files.

use alloc::string::String;
use alloc::vec::Vec;

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
    pub model: String,
}

/// The model name from IDENTIFY data: words 27 to 46, two characters
/// each, high byte first.
pub fn model_name(id: &[u16]) -> String {
    let mut s = String::new();
    for w in &id[27..47] {
        for b in [(w >> 8) as u8, *w as u8] {
            if (0x20..0x7f).contains(&b) {
                s.push(b as char);
            }
        }
    }
    String::from(s.trim())
}

/// Where a drive is: channel base port, control port, slave or master.
const POSITIONS: [(u16, u16, bool); 4] = [
    (0x1f0, 0x3f6, false),
    (0x1f0, 0x3f6, true),
    (0x170, 0x376, false),
    (0x170, 0x376, true),
];

impl Ata {
    /// Every ATA hard disk on the two IDE channels. CD drives (ATAPI)
    /// are skipped.
    pub fn find_all() -> Vec<Ata> {
        let mut out = Vec::new();
        for (base, control, slave) in POSITIONS {
            let mut disk = Ata {
                base,
                control,
                slave,
                sectors: 0,
                model: String::new(),
            };
            if disk.identify() {
                out.push(disk);
            }
        }
        out
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
            self.model = model_name(&id);
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

    /// A drive that is busy or still wants data was cut off in the middle
    /// of a command: reset the channel so it takes a new one.
    pub fn recover(&mut self) {
        if self.status() & (BSY | DRQ) == 0 {
            return;
        }
        unsafe {
            outb(self.control, 0x04); // software reset
            self.delay();
            outb(self.control, 0x00);
        }
        self.delay();
        let _ = self.wait_not_busy();
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

// ---- CD and DVD drives (ATAPI) ----------------------------------------------

const PACKET: u8 = 0xa0;
const IDENTIFY_PACKET: u8 = 0xa1;
/// Bytes in a CD sector.
pub const CD_SECTOR: usize = 2048;

/// A CD or DVD drive on the IDE controller, read with SCSI commands sent
/// as packets. QEMU puts `-cdrom` on the second channel.
pub struct Atapi {
    base: u16,
    control: u16,
    slave: bool,
    pub model: String,
}

impl Atapi {
    /// Every CD or DVD drive on the two IDE channels.
    pub fn find_all() -> Vec<Atapi> {
        let mut out = Vec::new();
        for (base, control, slave) in POSITIONS {
            let mut drive = Atapi {
                base,
                control,
                slave,
                model: String::new(),
            };
            if drive.identify() {
                out.push(drive);
            }
        }
        out
    }

    fn disk(&self) -> Ata {
        Ata {
            base: self.base,
            control: self.control,
            slave: self.slave,
            sectors: 0,
            model: String::new(),
        }
    }

    fn identify(&mut self) -> bool {
        let d = self.disk();
        unsafe {
            outb(self.control, 0x02);
            if inb(self.base + 7) == 0xff {
                return false;
            }
            outb(self.base + 6, 0xa0 | (self.slave as u8) << 4);
            d.delay();
            for reg in 2..=5 {
                outb(self.base + reg, 0);
            }
            outb(self.base + 7, IDENTIFY);
            if inb(self.base + 7) == 0 {
                return false;
            }
            // a CD drive refuses IDENTIFY and leaves its signature
            for _ in 0..TIMEOUT {
                let s = inb(self.base + 7);
                if s & BSY == 0 {
                    break;
                }
                core::hint::spin_loop();
            }
            if inb(self.base + 4) != 0x14 || inb(self.base + 5) != 0xeb {
                // a hard disk took the IDENTIFY: read what it sent, or
                // the next read of the disk gets these words instead
                for _ in 0..256 {
                    if inb(self.base + 7) & DRQ == 0 {
                        break;
                    }
                    inw(self.base);
                }
                return false;
            }
            outb(self.base + 7, IDENTIFY_PACKET);
            d.delay();
            if d.wait_data().is_err() {
                return false;
            }
            let mut id = [0u16; 256];
            for w in id.iter_mut() {
                *w = inw(self.base);
            }
            self.model = model_name(&id);
        }
        true
    }

    /// Read `buf.len() / 2048` sectors from `lba`. A fresh disc answers
    /// the first command with "medium changed", so it is tried again.
    pub fn read(&mut self, lba: u32, buf: &mut [u8]) -> Result<(), IoError> {
        for (i, chunk) in buf.chunks_mut(16 * CD_SECTOR).enumerate() {
            let at = lba + (i * 16) as u32;
            let mut tries = 0;
            while self.read_some(at, chunk).is_err() {
                tries += 1;
                if tries == 3 {
                    return Err(IoError);
                }
            }
        }
        Ok(())
    }

    fn read_some(&mut self, lba: u32, buf: &mut [u8]) -> Result<(), IoError> {
        let d = self.disk();
        let count = (buf.len() / CD_SECTOR) as u16;
        if count == 0 {
            return Ok(());
        }
        d.wait_not_busy()?;
        let limit: u16 = 0xf800;
        unsafe {
            outb(self.base + 6, 0xa0 | (self.slave as u8) << 4);
            d.delay();
            outb(self.base + 1, 0); // PIO, not DMA
            outb(self.base + 4, limit as u8);
            outb(self.base + 5, (limit >> 8) as u8);
            outb(self.base + 7, PACKET);
        }
        d.delay();
        d.wait_data()?;
        // READ (10)
        let packet: [u8; 12] = [
            0x28,
            0,
            (lba >> 24) as u8,
            (lba >> 16) as u8,
            (lba >> 8) as u8,
            lba as u8,
            0,
            (count >> 8) as u8,
            count as u8,
            0,
            0,
            0,
        ];
        for pair in packet.chunks(2) {
            unsafe { outw(self.base, pair[0] as u16 | (pair[1] as u16) << 8) };
        }
        let mut done = 0;
        while done < buf.len() {
            d.delay();
            d.wait_data()?;
            let n = unsafe { inb(self.base + 4) as usize | (inb(self.base + 5) as usize) << 8 };
            if n == 0 || n % 2 != 0 {
                return Err(IoError);
            }
            for _ in 0..n / 2 {
                let w = unsafe { inw(self.base) };
                if done + 1 < buf.len() {
                    buf[done] = w as u8;
                    buf[done + 1] = (w >> 8) as u8;
                }
                done += 2;
            }
        }
        d.delay();
        let s = d.wait_not_busy()?;
        if s & (ERR | DF) != 0 {
            return Err(IoError);
        }
        Ok(())
    }
}
