//! Driver for the Intel 8254x (e1000) network card, the one QEMU emulates
//! by default. The card copies packets to and from memory by itself
//! (DMA) using two rings of descriptors; we poll the rings instead of
//! using interrupts.
//!
//! Memory is identity-mapped, so the address of a buffer is also its
//! physical address, which is what the card needs.

use alloc::vec::Vec;
use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};
use core::sync::atomic::{fence, Ordering};

use smoltcp::phy::{self, Checksum, ChecksumCapabilities, DeviceCapabilities, Medium};
use smoltcp::time::Instant;

use crate::pci;

/// Vendor and device ids of 8254x cards QEMU, VirtualBox and VMware offer.
const IDS: [(u16, u16); 4] = [
    (0x8086, 0x100e), // 82540EM, QEMU's default
    (0x8086, 0x100f), // 82545EM, VMware
    (0x8086, 0x10d3), // 82574L
    (0x8086, 0x1004), // 82543GC
];

const CTRL: usize = 0x0000;
const REG_STATUS: usize = 0x0008;
const EERD: usize = 0x0014;
const IMC: usize = 0x00d8;
const RCTL: usize = 0x0100;
const TCTL: usize = 0x0400;
const TIPG: usize = 0x0410;
const RDBAL: usize = 0x2800;
const RDBAH: usize = 0x2804;
const RDLEN: usize = 0x2808;
const RDH: usize = 0x2810;
const RDT: usize = 0x2818;
const TDBAL: usize = 0x3800;
const TDBAH: usize = 0x3804;
const TDLEN: usize = 0x3808;
const TDH: usize = 0x3810;
const TDT: usize = 0x3818;
const MTA: usize = 0x5200;
const RAL: usize = 0x5400;
const RAH: usize = 0x5404;

const CTRL_ASDE: u32 = 1 << 5;
const CTRL_SLU: u32 = 1 << 6;
const CTRL_RST: u32 = 1 << 26;
/// Link up, in the device status register.
const STATUS_LU: u32 = 1 << 1;

const RCTL_EN: u32 = 1 << 1;
const RCTL_BAM: u32 = 1 << 15;
const RCTL_SECRC: u32 = 1 << 26;

const TCTL_EN: u32 = 1 << 1;
const TCTL_PSP: u32 = 1 << 3;

const TX_CMD_EOP: u8 = 1 << 0;
const TX_CMD_IFCS: u8 = 1 << 1;
const TX_CMD_RS: u8 = 1 << 3;
const STATUS_DD: u8 = 1 << 0;
const RX_STATUS_EOP: u8 = 1 << 1;

const RX_COUNT: usize = 64;
const TX_COUNT: usize = 32;
const BUF_SIZE: usize = 2048;
pub const MTU: usize = 1514;

#[repr(C)]
#[derive(Clone, Copy)]
struct RxDesc {
    addr: u64,
    length: u16,
    checksum: u16,
    status: u8,
    errors: u8,
    special: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct TxDesc {
    addr: u64,
    length: u16,
    cso: u8,
    cmd: u8,
    status: u8,
    css: u8,
    special: u16,
}

#[repr(C, align(128))]
struct Rings {
    rx: [RxDesc; RX_COUNT],
    tx: [TxDesc; TX_COUNT],
}

#[repr(C, align(4096))]
struct Buffers {
    rx: [[u8; BUF_SIZE]; RX_COUNT],
    tx: [[u8; BUF_SIZE]; TX_COUNT],
}

static mut RINGS: Rings = Rings {
    rx: [RxDesc {
        addr: 0,
        length: 0,
        checksum: 0,
        status: 0,
        errors: 0,
        special: 0,
    }; RX_COUNT],
    tx: [TxDesc {
        addr: 0,
        length: 0,
        cso: 0,
        cmd: 0,
        status: 0,
        css: 0,
        special: 0,
    }; TX_COUNT],
};

static mut BUFFERS: Buffers = Buffers {
    rx: [[0; BUF_SIZE]; RX_COUNT],
    tx: [[0; BUF_SIZE]; TX_COUNT],
};

pub struct E1000 {
    mmio: usize,
    pub mac: [u8; 6],
    rx_next: usize,
    tx_next: usize,
}

impl E1000 {
    /// Find the card on the PCI bus and start it. Call once.
    pub fn init() -> Option<Self> {
        let dev = pci::find(&IDS)?;
        dev.enable_bus_master();
        let mmio = dev.bar(0);
        if mmio == 0 {
            return None;
        }
        let mut nic = E1000 {
            mmio,
            mac: [0; 6],
            rx_next: 0,
            tx_next: 0,
        };
        nic.reset();
        nic.read_mac();
        nic.set_up_rings();
        Some(nic)
    }

    /// Whether a cable is plugged in and the link is up.
    pub fn link_up(&self) -> bool {
        self.read(REG_STATUS) & STATUS_LU != 0
    }

    fn read(&self, reg: usize) -> u32 {
        unsafe { read_volatile((self.mmio + reg) as *const u32) }
    }

    fn write(&self, reg: usize, value: u32) {
        unsafe { write_volatile((self.mmio + reg) as *mut u32, value) }
    }

    fn reset(&mut self) {
        self.write(IMC, 0xffff_ffff);
        self.write(CTRL, self.read(CTRL) | CTRL_RST);
        for _ in 0..100_000 {
            if self.read(CTRL) & CTRL_RST == 0 {
                break;
            }
            core::hint::spin_loop();
        }
        self.write(IMC, 0xffff_ffff);
        self.write(CTRL, self.read(CTRL) | CTRL_SLU | CTRL_ASDE);
    }

    fn read_mac(&mut self) {
        // the receive address registers hold the MAC after reset
        let low = self.read(RAL);
        let high = self.read(RAH);
        if low != 0 {
            self.mac[..4].copy_from_slice(&low.to_le_bytes());
            self.mac[4..].copy_from_slice(&(high as u16).to_le_bytes());
        } else {
            for word in 0..3 {
                let w = self.eeprom(word as u32);
                self.mac[word * 2..word * 2 + 2].copy_from_slice(&w.to_le_bytes());
            }
            let [a, b, c, d, e, f] = self.mac;
            self.write(RAL, u32::from_le_bytes([a, b, c, d]));
            self.write(RAH, u16::from_le_bytes([e, f]) as u32 | 1 << 31);
        }
    }

    fn eeprom(&self, word: u32) -> u16 {
        self.write(EERD, word << 8 | 1);
        for _ in 0..100_000 {
            let v = self.read(EERD);
            if v & (1 << 4) != 0 {
                return (v >> 16) as u16;
            }
        }
        0
    }

    fn set_up_rings(&mut self) {
        unsafe {
            let rings = &mut *addr_of_mut!(RINGS);
            let bufs = &*addr_of!(BUFFERS);
            for (i, d) in rings.rx.iter_mut().enumerate() {
                d.addr = bufs.rx[i].as_ptr() as u64;
                d.status = 0;
            }
            for (i, d) in rings.tx.iter_mut().enumerate() {
                d.addr = bufs.tx[i].as_ptr() as u64;
                d.cmd = 0;
                d.status = STATUS_DD; // free
            }
            let rx = rings.rx.as_ptr() as u64;
            let tx = rings.tx.as_ptr() as u64;
            for i in 0..128 {
                self.write(MTA + 4 * i, 0);
            }

            self.write(RDBAL, rx as u32);
            self.write(RDBAH, (rx >> 32) as u32);
            self.write(RDLEN, (RX_COUNT * 16) as u32);
            self.write(RDH, 0);
            self.write(RDT, (RX_COUNT - 1) as u32);
            // 2048-byte buffers, broadcasts, strip the CRC
            self.write(RCTL, RCTL_EN | RCTL_BAM | RCTL_SECRC);

            self.write(TDBAL, tx as u32);
            self.write(TDBAH, (tx >> 32) as u32);
            self.write(TDLEN, (TX_COUNT * 16) as u32);
            self.write(TDH, 0);
            self.write(TDT, 0);
            self.write(TCTL, TCTL_EN | TCTL_PSP | 0x0f << 4 | 0x40 << 12);
            self.write(TIPG, 10 | 8 << 10 | 6 << 20);
        }
    }

    /// Take the next received packet, if any.
    fn receive_packet(&mut self) -> Option<Vec<u8>> {
        let i = self.rx_next;
        unsafe {
            let desc = addr_of_mut!(RINGS.rx[i]);
            let status = read_volatile(addr_of!((*desc).status));
            if status & STATUS_DD == 0 {
                return None;
            }
            fence(Ordering::Acquire);
            let len = read_volatile(addr_of!((*desc).length)) as usize;
            let errors = read_volatile(addr_of!((*desc).errors));
            let packet = if status & RX_STATUS_EOP != 0 && errors == 0 {
                Some((&*addr_of!(BUFFERS.rx[i]))[..len.min(BUF_SIZE)].to_vec())
            } else {
                Some(Vec::new()) // dropped, the caller skips it
            };
            write_volatile(addr_of_mut!((*desc).status), 0);
            fence(Ordering::Release);
            // hand the descriptor back to the card
            self.write(RDT, i as u32);
            self.rx_next = (i + 1) % RX_COUNT;
            packet
        }
    }

    fn tx_free(&self) -> bool {
        unsafe {
            let desc = addr_of!(RINGS.tx[self.tx_next]);
            read_volatile(addr_of!((*desc).status)) & STATUS_DD != 0
        }
    }

    fn send_packet(&mut self, len: usize, fill: impl FnOnce(&mut [u8])) {
        let i = self.tx_next;
        let len = len.min(BUF_SIZE);
        unsafe {
            fill(&mut (&mut *addr_of_mut!(BUFFERS.tx[i]))[..len]);
            let desc = addr_of_mut!(RINGS.tx[i]);
            write_volatile(addr_of_mut!((*desc).length), len as u16);
            write_volatile(
                addr_of_mut!((*desc).cmd),
                TX_CMD_EOP | TX_CMD_IFCS | TX_CMD_RS,
            );
            write_volatile(addr_of_mut!((*desc).status), 0);
        }
        fence(Ordering::Release);
        self.tx_next = (i + 1) % TX_COUNT;
        self.write(TDT, self.tx_next as u32);
    }
}

pub struct RxToken(Vec<u8>);
pub struct TxToken<'a>(&'a mut E1000);

impl phy::RxToken for RxToken {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        f(&self.0)
    }
}

impl phy::TxToken for TxToken<'_> {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        let mut result = None;
        self.0.send_packet(len, |buf| result = Some(f(buf)));
        result.unwrap()
    }
}

impl phy::Device for E1000 {
    type RxToken<'a> = RxToken;
    type TxToken<'a> = TxToken<'a>;

    fn receive(&mut self, _now: Instant) -> Option<(RxToken, TxToken<'_>)> {
        loop {
            let packet = self.receive_packet()?;
            if !packet.is_empty() {
                return Some((RxToken(packet), TxToken(self)));
            }
        }
    }

    fn transmit(&mut self, _now: Instant) -> Option<TxToken<'_>> {
        self.tx_free().then_some(TxToken(self))
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ethernet;
        caps.max_transmission_unit = MTU;
        caps.max_burst_size = Some(RX_COUNT / 2);
        let mut checksum = ChecksumCapabilities::default();
        checksum.ipv4 = Checksum::Both;
        caps.checksum = checksum;
        caps
    }
}
