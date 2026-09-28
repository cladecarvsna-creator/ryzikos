//! SATA disks and CD/DVD drives on an AHCI controller: what VirtualBox
//! gives new virtual machines, QEMU's q35 machine and most real computers
//! since 2008. The controller copies whole blocks to and from memory by
//! itself (DMA) when we fill in a command slot and set its bit.
//!
//! Memory is identity-mapped, so an address is also what the controller
//! needs. Every port gets one command slot and a 64 KiB bounce buffer.

use alloc::alloc::{alloc_zeroed, Layout};
use alloc::string::String;
use alloc::vec::Vec;
use core::ptr::{read_volatile, write_volatile};

use super::ata::{model_name, IoError};
use crate::pci;

// controller registers
const GHC: usize = 0x04;
const PI: usize = 0x0c;
const GHC_AE: u32 = 1 << 31;

// port registers, from 0x100 + 0x80 * port
const P_CLB: usize = 0x00;
const P_CLBU: usize = 0x04;
const P_FB: usize = 0x08;
const P_FBU: usize = 0x0c;
const P_IS: usize = 0x10;
const P_IE: usize = 0x14;
const P_CMD: usize = 0x18;
const P_TFD: usize = 0x20;
const P_SIG: usize = 0x24;
const P_SSTS: usize = 0x28;
const P_SERR: usize = 0x30;
const P_CI: usize = 0x38;

const CMD_ST: u32 = 1 << 0;
const CMD_SUD: u32 = 1 << 1;
const CMD_POD: u32 = 1 << 2;
const CMD_FRE: u32 = 1 << 4;
const CMD_FR: u32 = 1 << 14;
const CMD_CR: u32 = 1 << 15;
/// Task file error, in the port's interrupt status.
const IS_TFES: u32 = 1 << 30;

const SIG_ATA: u32 = 0x0000_0101;
const SIG_ATAPI: u32 = 0xeb14_0101;

const IDENTIFY: u8 = 0xec;
const READ_DMA_EXT: u8 = 0x25;
const WRITE_DMA_EXT: u8 = 0x35;
const FLUSH_EXT: u8 = 0xea;
const PACKET: u8 = 0xa0;

/// Bytes moved by one command.
const BOUNCE: usize = 64 * 1024;
const SECTOR: usize = 512;
const CD_SECTOR: usize = 2048;
/// How many times to poll the controller before giving up.
const TIMEOUT: u32 = 20_000_000;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Disk,
    Cd,
}

/// One device on one port of the controller.
pub struct Drive {
    port: usize,
    pub kind: Kind,
    sectors: u64,
    pub model: String,
    /// Command list (32 headers), received FISes, one command table.
    list: *mut u8,
    table: *mut u8,
    buf: *mut u8,
}

// The drive is only used behind the file system's lock.
unsafe impl Send for Drive {}

fn alloc(size: usize, align: usize) -> *mut u8 {
    unsafe { alloc_zeroed(Layout::from_size_align(size, align).unwrap()) }
}

/// Every disk and CD drive on every AHCI controller.
pub fn find_all() -> Vec<Drive> {
    let mut out = Vec::new();
    for dev in pci::find_class(0x01, 0x06) {
        // prog-if 1: AHCI (0 would be a vendor-specific interface)
        if (dev.read(0x08) >> 8) as u8 != 0x01 {
            continue;
        }
        let abar = dev.bar(5);
        if abar == 0 {
            continue;
        }
        dev.enable_bus_master();
        let hba = abar;
        unsafe {
            let ghc = read_volatile((hba + GHC) as *const u32);
            write_volatile((hba + GHC) as *mut u32, ghc | GHC_AE);
        }
        let ports = unsafe { read_volatile((hba + PI) as *const u32) };
        for port in 0..32 {
            if ports & (1 << port) == 0 {
                continue;
            }
            let base = hba + 0x100 + 0x80 * port;
            if let Some(d) = Drive::probe(base) {
                out.push(d);
            }
        }
    }
    out
}

impl Drive {
    fn reg(&self, r: usize) -> u32 {
        unsafe { read_volatile((self.port + r) as *const u32) }
    }

    fn set(&self, r: usize, v: u32) {
        unsafe { write_volatile((self.port + r) as *mut u32, v) }
    }

    fn probe(port: usize) -> Option<Drive> {
        let ssts = unsafe { read_volatile((port + P_SSTS) as *const u32) };
        // a device is there and the link is up
        if ssts & 0x0f != 3 {
            return None;
        }
        let sig = unsafe { read_volatile((port + P_SIG) as *const u32) };
        let kind = match sig {
            SIG_ATA => Kind::Disk,
            SIG_ATAPI => Kind::Cd,
            _ => return None,
        };
        let mut d = Drive {
            port,
            kind,
            sectors: 0,
            model: String::new(),
            list: alloc(1024 + 256, 1024),
            table: alloc(256, 128),
            buf: alloc(BOUNCE, 4096),
        };
        if d.list.is_null() || d.table.is_null() || d.buf.is_null() {
            return None;
        }
        if !d.start() {
            return None;
        }
        let packet = kind == Kind::Cd;
        // IDENTIFY (PACKET) DEVICE: 512 bytes about the drive
        let cmd = if packet { 0xa1 } else { IDENTIFY };
        if d.command(cmd, 0, 1, false, None, SECTOR).is_err() {
            return None;
        }
        let id: Vec<u16> = (0..256)
            .map(|i| unsafe { u16::from_le_bytes([*d.buf.add(2 * i), *d.buf.add(2 * i + 1)]) })
            .collect();
        d.model = model_name(&id);
        if !packet {
            let lba48 = id[83] & (1 << 10) != 0;
            d.sectors = if lba48 {
                id[100] as u64 | (id[101] as u64) << 16 | (id[102] as u64) << 32 | (id[103] as u64) << 48
            } else {
                id[60] as u64 | (id[61] as u64) << 16
            };
            if d.sectors == 0 {
                return None;
            }
        }
        Some(d)
    }

    /// Stop the port, point it at our memory and start it again.
    fn start(&mut self) -> bool {
        let cmd = self.reg(P_CMD);
        self.set(P_CMD, cmd & !CMD_ST);
        if !self.wait(|d| d.reg(P_CMD) & CMD_CR == 0) {
            return false;
        }
        self.set(P_CMD, self.reg(P_CMD) & !CMD_FRE);
        if !self.wait(|d| d.reg(P_CMD) & CMD_FR == 0) {
            return false;
        }
        let list = self.list as usize;
        self.set(P_CLB, list as u32);
        self.set(P_CLBU, (list >> 32) as u32);
        let fis = list + 1024;
        self.set(P_FB, fis as u32);
        self.set(P_FBU, (fis >> 32) as u32);
        self.set(P_SERR, 0xffff_ffff);
        self.set(P_IS, 0xffff_ffff);
        self.set(P_IE, 0);
        self.set(P_CMD, self.reg(P_CMD) | CMD_FRE | CMD_SUD | CMD_POD);
        // the drive must not be busy when the port starts
        self.wait(|d| d.reg(P_TFD) & 0x88 == 0);
        self.set(P_CMD, self.reg(P_CMD) | CMD_ST);
        true
    }

    fn wait(&self, done: impl Fn(&Drive) -> bool) -> bool {
        for _ in 0..TIMEOUT {
            if done(self) {
                return true;
            }
            core::hint::spin_loop();
        }
        false
    }

    /// Run one command in slot 0, moving `bytes` through the bounce
    /// buffer. `packet` is the SCSI command for CD drives.
    fn command(
        &mut self,
        cmd: u8,
        lba: u64,
        count: u16,
        write: bool,
        packet: Option<&[u8; 12]>,
        bytes: usize,
    ) -> Result<(), IoError> {
        if !self.wait(|d| d.reg(P_TFD) & 0x88 == 0) {
            return Err(IoError);
        }
        unsafe {
            // command header 0
            let h = self.list as *mut u32;
            let flags = 5 // the command FIS is 5 dwords
                | (packet.is_some() as u32) << 5
                | (write as u32) << 6
                | ((bytes > 0) as u32) << 16; // one PRD entry, if data moves
            write_volatile(h, flags);
            write_volatile(h.add(1), 0);
            let t = self.table as usize;
            write_volatile(h.add(2), t as u32);
            write_volatile(h.add(3), (t >> 32) as u32);
            // command FIS: host to device register
            let f = self.table;
            core::ptr::write_bytes(f, 0, 0x90);
            *f = 0x27;
            *f.add(1) = 0x80;
            *f.add(2) = cmd;
            *f.add(3) = packet.is_some() as u8; // features: DMA for packets
            *f.add(4) = lba as u8;
            *f.add(5) = (lba >> 8) as u8;
            *f.add(6) = (lba >> 16) as u8;
            *f.add(7) = 1 << 6; // LBA mode
            *f.add(8) = (lba >> 24) as u8;
            *f.add(9) = (lba >> 32) as u8;
            *f.add(10) = (lba >> 40) as u8;
            *f.add(12) = count as u8;
            *f.add(13) = (count >> 8) as u8;
            if let Some(p) = packet {
                core::ptr::copy_nonoverlapping(p.as_ptr(), f.add(0x40), 12);
            }
            // one PRD entry: the bounce buffer
            let prd = f.add(0x80) as *mut u32;
            let b = self.buf as usize;
            write_volatile(prd, b as u32);
            write_volatile(prd.add(1), (b >> 32) as u32);
            write_volatile(prd.add(2), 0);
            write_volatile(prd.add(3), bytes.saturating_sub(1) as u32);
        }
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        self.set(P_IS, 0xffff_ffff);
        self.set(P_CI, 1);
        let mut ok = false;
        for _ in 0..TIMEOUT {
            if self.reg(P_IS) & IS_TFES != 0 {
                break;
            }
            if self.reg(P_CI) & 1 == 0 {
                ok = true;
                break;
            }
            core::hint::spin_loop();
        }
        if !ok || self.reg(P_TFD) & 0x01 != 0 {
            // clear the error so the next command can run
            self.restart();
            return Err(IoError);
        }
        Ok(())
    }

    fn restart(&mut self) {
        self.set(P_CMD, self.reg(P_CMD) & !CMD_ST);
        self.wait(|d| d.reg(P_CMD) & CMD_CR == 0);
        self.set(P_SERR, 0xffff_ffff);
        self.set(P_IS, 0xffff_ffff);
        self.set(P_CMD, self.reg(P_CMD) | CMD_ST);
    }

    /// A command still in flight was cut off: stop and restart the port
    /// so slot 0 is free again.
    pub fn recover(&mut self) {
        if self.reg(P_CI) & 1 != 0 || self.reg(P_TFD) & 0x88 != 0 {
            self.restart();
        }
    }

    pub fn sectors(&self) -> u64 {
        self.sectors
    }

    /// Read whole 512-byte sectors of a disk.
    pub fn read(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), IoError> {
        if lba + (buf.len() / SECTOR) as u64 > self.sectors {
            return Err(IoError);
        }
        for (i, chunk) in buf.chunks_mut(BOUNCE).enumerate() {
            let at = lba + (i * BOUNCE / SECTOR) as u64;
            let n = chunk.len() / SECTOR;
            self.command(READ_DMA_EXT, at, n as u16, false, None, n * SECTOR)?;
            unsafe { core::ptr::copy_nonoverlapping(self.buf, chunk.as_mut_ptr(), n * SECTOR) };
        }
        Ok(())
    }

    pub fn write(&mut self, lba: u64, buf: &[u8]) -> Result<(), IoError> {
        if lba + (buf.len() / SECTOR) as u64 > self.sectors {
            return Err(IoError);
        }
        for (i, chunk) in buf.chunks(BOUNCE).enumerate() {
            let at = lba + (i * BOUNCE / SECTOR) as u64;
            let n = chunk.len() / SECTOR;
            unsafe { core::ptr::copy_nonoverlapping(chunk.as_ptr(), self.buf, n * SECTOR) };
            self.command(WRITE_DMA_EXT, at, n as u16, true, None, n * SECTOR)?;
        }
        Ok(())
    }

    pub fn flush(&mut self) -> Result<(), IoError> {
        self.command(FLUSH_EXT, 0, 0, false, None, 0)
    }

    /// Read 2048-byte sectors of a CD or DVD. A fresh disc answers the
    /// first command with "medium changed", so each read is tried again.
    pub fn read_cd(&mut self, lba: u32, buf: &mut [u8]) -> Result<(), IoError> {
        for (i, chunk) in buf.chunks_mut(BOUNCE).enumerate() {
            let at = lba + (i * BOUNCE / CD_SECTOR) as u32;
            let n = (chunk.len() / CD_SECTOR) as u16;
            if n == 0 {
                continue;
            }
            let p: [u8; 12] = [
                0x28,
                0,
                (at >> 24) as u8,
                (at >> 16) as u8,
                (at >> 8) as u8,
                at as u8,
                0,
                (n >> 8) as u8,
                n as u8,
                0,
                0,
                0,
            ];
            let mut tries = 0;
            while self
                .command(PACKET, 0, 0, false, Some(&p), n as usize * CD_SECTOR)
                .is_err()
            {
                tries += 1;
                if tries == 3 {
                    return Err(IoError);
                }
            }
            unsafe {
                core::ptr::copy_nonoverlapping(self.buf, chunk.as_mut_ptr(), n as usize * CD_SECTOR)
            };
        }
        Ok(())
    }
}
