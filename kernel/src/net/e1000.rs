//! Driver for Intel's gigabit network cards: the 8254x (e1000) that QEMU,
//! VirtualBox and VMware emulate, and the 8257x, 82577-82579, I217, I218
//! and I219 (e1000e) built into most PCs with Intel network since 2008.
//! They share the registers and the legacy descriptors used here. The card
//! copies packets to and from memory by itself (DMA) using two rings of
//! descriptors; we poll the rings instead of using interrupts.
//!
//! Memory is identity-mapped, so the address of a buffer is also its
//! physical address, which is what the card needs.

use alloc::vec::Vec;
use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};
use core::sync::atomic::{fence, Ordering};

use super::{delay_ms, Card};
use crate::pci;

/// Device ids (Intel's vendor id 0x8086) of 8254x cards: 82540EM is
/// QEMU's default, 82545EM VMware's.
const IDS: &[u16] = &[
    0x1000, 0x1001, 0x1004, 0x1008, 0x1009, 0x100c, 0x100d, 0x100e, 0x100f, 0x1010, 0x1011,
    0x1012, 0x1013, 0x1014, 0x1015, 0x1016, 0x1017, 0x1018, 0x1019, 0x101a, 0x101d, 0x101e,
    0x1026, 0x1027, 0x1028, 0x1075, 0x1076, 0x1077, 0x1078, 0x1079, 0x107a, 0x107b, 0x107c,
    0x108a, 0x1099, 0x10b5, 0x1107, 0x1112,
];

/// Device ids of e1000e cards.
const NEWER_IDS: &[u16] = &[
    // 82571, 82572, 82573, 82574L (QEMU's e1000e), 82583V
    0x105e, 0x105f, 0x1060, 0x10a4, 0x10a5, 0x10bc, 0x10d9, 0x10da, 0x107d, 0x107e, 0x107f,
    0x10b9, 0x108b, 0x108c, 0x109a, 0x10d3, 0x10f6, 0x150c,
    // built into ICH8, ICH9 and ICH10 chipsets
    0x1049, 0x104a, 0x104b, 0x104c, 0x104d, 0x10c4, 0x10c5, 0x10bd, 0x10bf, 0x10c0, 0x10c2,
    0x10c3, 0x10cb, 0x10cc, 0x10cd, 0x10ce, 0x10e5, 0x10f5, 0x294c, 0x10de, 0x10df, 0x1525,
    // 82577, 82578, 82579 and I217, I218
    0x10ea, 0x10eb, 0x10ef, 0x10f0, 0x1502, 0x1503, 0x153a, 0x153b, 0x155a, 0x1559, 0x15a0,
    0x15a1, 0x15a2, 0x15a3,
    // I219, in its many chipset generations
    0x156f, 0x1570, 0x15b7, 0x15b8, 0x15b9, 0x15bb, 0x15bc, 0x15bd, 0x15be, 0x15d6, 0x15d7,
    0x15d8, 0x15e3, 0x15df, 0x15e0, 0x15e1, 0x15e2, 0x0d4e, 0x0d4f, 0x0d4c, 0x0d4d, 0x0d53,
    0x0d55, 0x15fb, 0x15fc, 0x15f9, 0x15fa, 0x15f4, 0x15f5, 0x1a1c, 0x1a1d, 0x1a1e, 0x1a1f,
    0x0dc5, 0x0dc6, 0x0dc7, 0x0dc8, 0x550a, 0x550b, 0x550c, 0x550d, 0x550e, 0x550f, 0x5510,
    0x5511, 0x57a0, 0x57a1, 0x57b3, 0x57b4, 0x57b5, 0x57b6, 0x57b7, 0x57b8, 0x57b9, 0x57ba,
];

const CTRL: usize = 0x0000;
const REG_STATUS: usize = 0x0008;
const CTRL_EXT: usize = 0x0018;
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
const TXDCTL: usize = 0x3828;
const MTA: usize = 0x5200;
const RAL: usize = 0x5400;
const RAH: usize = 0x5404;

const CTRL_ASDE: u32 = 1 << 5;
const CTRL_SLU: u32 = 1 << 6;
const CTRL_RST: u32 = 1 << 26;
const CTRL_PHY_RST: u32 = 1 << 31;
const CTRL_FRCSPD: u32 = 1 << 11;
const CTRL_FRCDPX: u32 = 1 << 12;
const CTRL_ILOS: u32 = 1 << 7;
/// Software owns the card, not the management engine (e1000e).
const CTRL_EXT_DRV_LOAD: u32 = 1 << 28;
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

pub fn drives(vendor: u16, device: u16) -> bool {
    vendor == 0x8086 && (IDS.contains(&device) || NEWER_IDS.contains(&device))
}

pub struct E1000 {
    mmio: usize,
    mac: [u8; 6],
    /// An e1000e (82571 or newer), not a plain 8254x.
    newer: bool,
    rx_next: usize,
    tx_next: usize,
}

impl E1000 {
    /// Find the card on the PCI bus and start it. Call once.
    pub fn init() -> Option<Self> {
        let ids: Vec<(u16, u16)> = IDS.iter().chain(NEWER_IDS).map(|&d| (0x8086, d)).collect();
        let dev = pci::find(&ids)?;
        dev.enable_bus_master();
        let mmio = dev.bar(0);
        if mmio == 0 {
            return None;
        }
        let device = (dev.read(0) >> 16) as u16;
        let newer = NEWER_IDS.contains(&device);
        let mut nic = E1000 {
            mmio,
            mac: [0; 6],
            newer,
            rx_next: 0,
            tx_next: 0,
        };
        nic.reset();
        nic.read_mac();
        nic.set_up_rings();
        Some(nic)
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
        // newer cards reload their settings from flash, which takes a while
        delay_ms(if self.newer { 20 } else { 1 });
        for _ in 0..100_000 {
            if self.read(CTRL) & CTRL_RST == 0 {
                break;
            }
            core::hint::spin_loop();
        }
        self.write(IMC, 0xffff_ffff);
        if self.newer {
            // tell the firmware a driver has the card now
            self.write(CTRL_EXT, self.read(CTRL_EXT) | CTRL_EXT_DRV_LOAD);
        }
        // let the card and the switch agree on speed by themselves
        let ctrl = self.read(CTRL) & !(CTRL_PHY_RST | CTRL_FRCSPD | CTRL_FRCDPX | CTRL_ILOS);
        self.write(CTRL, ctrl | CTRL_SLU | CTRL_ASDE);
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
            if self.newer {
                // bit 22 must be set on these, and descriptors are written
                // back one at a time
                self.write(TXDCTL, 1 << 22 | 1 << 24 | 1 << 16);
            }
            self.write(TCTL, TCTL_EN | TCTL_PSP | 0x0f << 4 | 0x40 << 12);
            self.write(TIPG, 10 | 8 << 10 | 6 << 20);
        }
    }

    fn tx_free(&self) -> bool {
        unsafe {
            let desc = addr_of!(RINGS.tx[self.tx_next]);
            read_volatile(addr_of!((*desc).status)) & STATUS_DD != 0
        }
    }
}

impl Card for E1000 {
    fn name(&self) -> &'static str {
        if self.newer {
            "Intel e1000e"
        } else {
            "Intel e1000"
        }
    }

    fn mac(&self) -> [u8; 6] {
        self.mac
    }

    fn link_up(&self) -> bool {
        self.read(REG_STATUS) & STATUS_LU != 0
    }

    fn receive(&mut self) -> Option<Vec<u8>> {
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
                (&*addr_of!(BUFFERS.rx[i]))[..len.min(BUF_SIZE)].to_vec()
            } else {
                Vec::new() // dropped, the caller skips it
            };
            write_volatile(addr_of_mut!((*desc).status), 0);
            fence(Ordering::Release);
            // hand the descriptor back to the card
            self.write(RDT, i as u32);
            self.rx_next = (i + 1) % RX_COUNT;
            Some(packet)
        }
    }

    fn can_send(&self) -> bool {
        self.tx_free()
    }

    fn send(&mut self, data: &[u8]) {
        let i = self.tx_next;
        let len = data.len().min(BUF_SIZE);
        unsafe {
            (&mut *addr_of_mut!(BUFFERS.tx[i]))[..len].copy_from_slice(&data[..len]);
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
