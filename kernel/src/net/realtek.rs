//! Drivers for Realtek network cards: the gigabit RTL8111/8168 family (and
//! its relatives RTL8169 and the 100 Mbit RTL8101/8102/8103), which most
//! desktop boards without Intel network have, and the old RTL8139 that QEMU
//! can emulate. Like the e1000, the cards copy packets by DMA and we poll.
//!
//! Memory is identity-mapped, so buffer addresses are physical addresses.

use alloc::vec::Vec;
use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};
use core::sync::atomic::{fence, Ordering};

use super::{delay_ms, Card};
use crate::pci;
use crate::port::{inb, inl, inw, outb, outl, outw};

const REALTEK: u16 = 0x10ec;

pub fn drives(vendor: u16, device: u16) -> bool {
    RTL8169_IDS.contains(&(vendor, device)) || RTL8139_IDS.contains(&(vendor, device))
}

// ---- RTL8169 / RTL8168 / RTL8111 / RTL8101 -----------------------------

/// Gigabit and newer 100 Mbit chips with descriptor rings. D-Link and
/// others sell the same chip under their own vendor id.
const RTL8169_IDS: [(u16, u16); 8] = [
    (REALTEK, 0x8161), // RTL8111/8168, newer boards
    (REALTEK, 0x8168), // RTL8111/8168
    (REALTEK, 0x8169), // RTL8169
    (REALTEK, 0x8167), // RTL8169SC
    (REALTEK, 0x8136), // RTL8101E/8102E/8103E
    (0x1186, 0x4300),  // D-Link DGE-528T
    (0x1186, 0x4302),  // D-Link DGE-530T
    (0x1259, 0xc107),  // Allied Telesyn
];

const IDR0: usize = 0x00;
const MAR0: usize = 0x08;
const TNPDS: usize = 0x20;
const CR: usize = 0x37;
const TPPOLL: usize = 0x38;
const IMR: usize = 0x3c;
const ISR: usize = 0x3e;
const TCR: usize = 0x40;
const RCR: usize = 0x44;
const CR9346: usize = 0x50;
const PHY_STATUS: usize = 0x6c;
const RMS: usize = 0xda;
const CPLUS_CR: usize = 0xe0;
const RDSAR: usize = 0xe4;
const MTPS: usize = 0xec;

const CR_RST: u8 = 1 << 4;
const CR_RE: u8 = 1 << 3;
const CR_TE: u8 = 1 << 2;
const TPPOLL_NPQ: u8 = 1 << 6;
const PHY_LINK: u8 = 1 << 1;

const OWN: u32 = 1 << 31;
const EOR: u32 = 1 << 30;
const FS: u32 = 1 << 29;
const LS: u32 = 1 << 28;
/// Receive error summary.
const RX_RES: u32 = 1 << 21;

const RX_COUNT: usize = 64;
const TX_COUNT: usize = 32;
const BUF_SIZE: usize = 2048;

#[repr(C)]
#[derive(Clone, Copy)]
struct Desc {
    opts1: u32,
    opts2: u32,
    addr: u64,
}

const EMPTY: Desc = Desc {
    opts1: 0,
    opts2: 0,
    addr: 0,
};

/// The chips want the rings on 256-byte boundaries.
#[repr(C, align(256))]
struct Rings {
    rx: [Desc; RX_COUNT],
    tx: [Desc; TX_COUNT],
}

#[repr(C, align(4096))]
struct Buffers {
    rx: [[u8; BUF_SIZE]; RX_COUNT],
    tx: [[u8; BUF_SIZE]; TX_COUNT],
}

static mut RINGS: Rings = Rings {
    rx: [EMPTY; RX_COUNT],
    tx: [EMPTY; TX_COUNT],
};

static mut BUFFERS: Buffers = Buffers {
    rx: [[0; BUF_SIZE]; RX_COUNT],
    tx: [[0; BUF_SIZE]; TX_COUNT],
};

pub struct Rtl8169 {
    mmio: usize,
    mac: [u8; 6],
    rx_next: usize,
    tx_next: usize,
}

impl Rtl8169 {
    pub fn init() -> Option<Self> {
        let dev = pci::find(&RTL8169_IDS)?;
        dev.write(0x04, dev.read(0x04) | 0x07);
        // registers are in the first memory BAR (BAR 2 on the RTL8168)
        let mmio = (0..6).map(|n| dev.bar(n)).find(|&a| a != 0)?;
        let mut nic = Rtl8169 {
            mmio,
            mac: [0; 6],
            rx_next: 0,
            tx_next: 0,
        };
        nic.start();
        Some(nic)
    }

    fn r8(&self, reg: usize) -> u8 {
        unsafe { read_volatile((self.mmio + reg) as *const u8) }
    }
    fn w8(&self, reg: usize, v: u8) {
        unsafe { write_volatile((self.mmio + reg) as *mut u8, v) }
    }
    fn r16(&self, reg: usize) -> u16 {
        unsafe { read_volatile((self.mmio + reg) as *const u16) }
    }
    fn w16(&self, reg: usize, v: u16) {
        unsafe { write_volatile((self.mmio + reg) as *mut u16, v) }
    }
    fn w32(&self, reg: usize, v: u32) {
        unsafe { write_volatile((self.mmio + reg) as *mut u32, v) }
    }

    fn start(&mut self) {
        self.w16(IMR, 0);
        self.w8(CR, CR_RST);
        for _ in 0..1000 {
            if self.r8(CR) & CR_RST == 0 {
                break;
            }
            delay_ms(1);
        }
        for i in 0..6 {
            self.mac[i] = self.r8(IDR0 + i);
        }
        unsafe {
            let rings = &mut *addr_of_mut!(RINGS);
            let bufs = &*addr_of!(BUFFERS);
            for (i, d) in rings.rx.iter_mut().enumerate() {
                d.addr = bufs.rx[i].as_ptr() as u64;
                d.opts2 = 0;
                d.opts1 = OWN | if i == RX_COUNT - 1 { EOR } else { 0 } | BUF_SIZE as u32;
            }
            for (i, d) in rings.tx.iter_mut().enumerate() {
                d.addr = bufs.tx[i].as_ptr() as u64;
                d.opts2 = 0;
                d.opts1 = if i == TX_COUNT - 1 { EOR } else { 0 };
            }
        }
        fence(Ordering::Release);
        let (rx, tx) = unsafe { (addr_of!(RINGS.rx) as u64, addr_of!(RINGS.tx) as u64) };

        // unlock the configuration registers
        self.w8(CR9346, 0xc0);
        // receive checksums off, PCI multiple read/write on
        self.w16(CPLUS_CR, self.r16(CPLUS_CR) | 1 << 3);
        self.w16(RMS, BUF_SIZE as u16);
        self.w8(MTPS, 0x3b);
        self.w32(TNPDS, tx as u32);
        self.w32(TNPDS + 4, (tx >> 32) as u32);
        self.w32(RDSAR, rx as u32);
        self.w32(RDSAR + 4, (rx >> 32) as u32);
        self.w8(CR, CR_RE | CR_TE);
        // standard gap between frames, unlimited DMA bursts
        self.w32(TCR, 0x03 << 24 | 0x7 << 8);
        // unlimited DMA, no FIFO threshold; broadcast, multicast, our MAC
        self.w32(RCR, 0x7 << 13 | 0x7 << 8 | 0x0e);
        for i in 0..8 {
            self.w8(MAR0 + i, 0xff);
        }
        self.w8(CR9346, 0x00);
        self.w16(ISR, 0xffff);
    }
}

impl Card for Rtl8169 {
    fn name(&self) -> &'static str {
        "Realtek RTL8111/8168"
    }

    fn mac(&self) -> [u8; 6] {
        self.mac
    }

    fn link_up(&self) -> bool {
        self.r8(PHY_STATUS) & PHY_LINK != 0
    }

    fn receive(&mut self) -> Option<Vec<u8>> {
        let i = self.rx_next;
        unsafe {
            let desc = addr_of_mut!(RINGS.rx[i]);
            let opts1 = read_volatile(addr_of!((*desc).opts1));
            if opts1 & OWN != 0 {
                // an overflowed ring stops the card until acknowledged
                if self.r16(ISR) != 0 {
                    self.w16(ISR, 0xffff);
                }
                return None;
            }
            fence(Ordering::Acquire);
            // the length counts the 4-byte CRC at the end
            let len = (opts1 & 0x3fff) as usize;
            let whole = opts1 & (FS | LS) == FS | LS;
            let packet = if whole && opts1 & RX_RES == 0 && len > 4 {
                (&*addr_of!(BUFFERS.rx[i]))[..(len - 4).min(BUF_SIZE)].to_vec()
            } else {
                Vec::new()
            };
            let eor = if i == RX_COUNT - 1 { EOR } else { 0 };
            write_volatile(addr_of_mut!((*desc).opts2), 0);
            fence(Ordering::Release);
            // hand the descriptor back to the card
            write_volatile(addr_of_mut!((*desc).opts1), OWN | eor | BUF_SIZE as u32);
            self.rx_next = (i + 1) % RX_COUNT;
            Some(packet)
        }
    }

    fn can_send(&self) -> bool {
        unsafe { read_volatile(addr_of!(RINGS.tx[self.tx_next].opts1)) & OWN == 0 }
    }

    fn send(&mut self, data: &[u8]) {
        let i = self.tx_next;
        // the chip does not pad short frames itself
        let len = data.len().clamp(60, BUF_SIZE);
        unsafe {
            let buf = &mut *addr_of_mut!(BUFFERS.tx[i]);
            let n = data.len().min(BUF_SIZE);
            buf[..n].copy_from_slice(&data[..n]);
            buf[n..len].fill(0);
            let desc = addr_of_mut!(RINGS.tx[i]);
            let eor = if i == TX_COUNT - 1 { EOR } else { 0 };
            write_volatile(addr_of_mut!((*desc).opts2), 0);
            fence(Ordering::Release);
            write_volatile(addr_of_mut!((*desc).opts1), OWN | eor | FS | LS | len as u32);
        }
        fence(Ordering::Release);
        self.tx_next = (i + 1) % TX_COUNT;
        self.w8(TPPOLL, TPPOLL_NPQ);
    }
}

// ---- RTL8139 ------------------------------------------------------------

const RTL8139_IDS: [(u16, u16); 1] = [(REALTEK, 0x8139)];

const TSD0: u16 = 0x10;
const TSAD0: u16 = 0x20;
const RBSTART: u16 = 0x30;
const CAPR: u16 = 0x38;
const IMR_8139: u16 = 0x3c;
const ISR_8139: u16 = 0x3e;
const RCR_8139: u16 = 0x44;
const CONFIG1: u16 = 0x52;
const MSR: u16 = 0x58;

const CR_BUFE: u8 = 1 << 0;
const TSD_OWN: u32 = 1 << 13;
const MSR_LINKB: u8 = 1 << 2;
/// The receive buffer, plus room for a packet that runs past its end.
const RX_RING: usize = 8192;

#[repr(C, align(16))]
struct Rx8139([u8; RX_RING + 16 + 2048]);
#[repr(C, align(16))]
struct Tx8139([[u8; BUF_SIZE]; 4]);

static mut RX_8139: Rx8139 = Rx8139([0; RX_RING + 16 + 2048]);
static mut TX_8139: Tx8139 = Tx8139([[0; BUF_SIZE]; 4]);

pub struct Rtl8139 {
    io: u16,
    mac: [u8; 6],
    rx_offset: usize,
    tx_next: usize,
    tx_used: [bool; 4],
}

impl Rtl8139 {
    pub fn init() -> Option<Self> {
        let dev = pci::find(&RTL8139_IDS)?;
        dev.write(0x04, dev.read(0x04) | 0x07);
        let bar = dev.read(0x10);
        if bar & 1 == 0 {
            return None;
        }
        let io = (bar & !3) as u16;
        let mut nic = Rtl8139 {
            io,
            mac: [0; 6],
            rx_offset: 0,
            tx_next: 0,
            tx_used: [false; 4],
        };
        unsafe {
            outb(io + CONFIG1, 0); // power on
            outb(io + CR as u16, CR_RST);
            for _ in 0..1000 {
                if inb(io + CR as u16) & CR_RST == 0 {
                    break;
                }
                delay_ms(1);
            }
            for i in 0..6 {
                nic.mac[i] = inb(io + i as u16);
            }
            outl(io + RBSTART, addr_of!(RX_8139) as u32);
            outw(io + IMR_8139, 0);
            outb(io + CR as u16, CR_RE | CR_TE);
            // 8 KiB ring that may run over its end; broadcast, multicast,
            // our MAC
            outl(io + RCR_8139, 1 << 7 | 0x0e);
            outw(io + ISR_8139, 0xffff);
        }
        Some(nic)
    }
}

impl Card for Rtl8139 {
    fn name(&self) -> &'static str {
        "Realtek RTL8139"
    }

    fn mac(&self) -> [u8; 6] {
        self.mac
    }

    fn link_up(&self) -> bool {
        unsafe { inb(self.io + MSR) & MSR_LINKB == 0 }
    }

    fn receive(&mut self) -> Option<Vec<u8>> {
        let io = self.io;
        unsafe {
            let isr = inw(io + ISR_8139);
            if isr != 0 {
                outw(io + ISR_8139, isr);
            }
            if inb(io + CR as u16) & CR_BUFE != 0 {
                return None;
            }
            let ring = &*addr_of!(RX_8139.0);
            let at = self.rx_offset;
            let status = u16::from_le_bytes([ring[at], ring[at + 1]]);
            let len = u16::from_le_bytes([ring[at + 2], ring[at + 3]]) as usize;
            let packet = if status & 1 != 0 && (64..=1518).contains(&len) {
                ring[at + 4..at + len].to_vec() // leave out the CRC
            } else {
                Vec::new()
            };
            self.rx_offset = (at + len + 4 + 3) & !3;
            self.rx_offset %= RX_RING;
            // the card keeps its read pointer 16 bytes behind
            outw(io + CAPR, (self.rx_offset as u16).wrapping_sub(16));
            Some(packet)
        }
    }

    fn can_send(&self) -> bool {
        let i = self.tx_next;
        !self.tx_used[i] || unsafe { inl(self.io + TSD0 + 4 * i as u16) } & TSD_OWN != 0
    }

    fn send(&mut self, data: &[u8]) {
        let i = self.tx_next;
        let len = data.len().clamp(60, 1792);
        unsafe {
            let buf = &mut (*addr_of_mut!(TX_8139.0))[i];
            let n = data.len().min(len);
            buf[..n].copy_from_slice(&data[..n]);
            buf[n..len].fill(0);
            fence(Ordering::Release);
            outl(self.io + TSAD0 + 4 * i as u16, buf.as_ptr() as u32);
            outl(self.io + TSD0 + 4 * i as u16, len as u32);
        }
        self.tx_used[i] = true;
        self.tx_next = (i + 1) % 4;
    }
}
