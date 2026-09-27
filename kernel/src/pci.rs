//! PCI configuration space through the legacy I/O ports 0xcf8/0xcfc.

use crate::port::{inl, outl};

const ADDRESS: u16 = 0xcf8;
const DATA: u16 = 0xcfc;

#[derive(Clone, Copy)]
pub struct Device {
    pub bus: u8,
    pub slot: u8,
    pub function: u8,
}

impl Device {
    fn address(&self, offset: u8) -> u32 {
        1 << 31
            | (self.bus as u32) << 16
            | (self.slot as u32) << 11
            | (self.function as u32) << 8
            | (offset as u32 & 0xfc)
    }

    pub fn read(&self, offset: u8) -> u32 {
        unsafe {
            outl(ADDRESS, self.address(offset));
            inl(DATA)
        }
    }

    pub fn write(&self, offset: u8, value: u32) {
        unsafe {
            outl(ADDRESS, self.address(offset));
            outl(DATA, value);
        }
    }

    /// Base address register `n` as a memory address (I/O BARs give 0).
    pub fn bar(&self, n: u8) -> usize {
        let low = self.read(0x10 + 4 * n);
        if low & 1 != 0 {
            return 0;
        }
        let mut addr = (low & !0xf) as usize;
        if (low >> 1) & 3 == 2 {
            addr |= (self.read(0x14 + 4 * n) as usize) << 32;
        }
        addr
    }

    /// Let the device answer memory accesses and do DMA.
    pub fn enable_bus_master(&self) {
        let command = self.read(0x04);
        self.write(0x04, command | 0x06);
    }
}

/// Find the first device with a vendor and device id from `ids`.
pub fn find(ids: &[(u16, u16)]) -> Option<Device> {
    for bus in 0..=255u8 {
        for slot in 0..32u8 {
            for function in 0..8u8 {
                let dev = Device {
                    bus,
                    slot,
                    function,
                };
                let id = dev.read(0);
                if id & 0xffff == 0xffff {
                    if function == 0 {
                        break;
                    }
                    continue;
                }
                let (vendor, device) = (id as u16, (id >> 16) as u16);
                if ids.contains(&(vendor, device)) {
                    return Some(dev);
                }
            }
        }
    }
    None
}

/// Every device of a class and subclass (like 0x01, 0x06 for SATA).
pub fn find_class(class: u8, subclass: u8) -> alloc::vec::Vec<Device> {
    let mut out = alloc::vec::Vec::new();
    for bus in 0..=255u8 {
        for slot in 0..32u8 {
            for function in 0..8u8 {
                let dev = Device {
                    bus,
                    slot,
                    function,
                };
                let id = dev.read(0);
                if id & 0xffff == 0xffff {
                    if function == 0 {
                        break;
                    }
                    continue;
                }
                let class_reg = dev.read(0x08);
                if (class_reg >> 24) as u8 == class && (class_reg >> 16) as u8 == subclass {
                    out.push(dev);
                }
                // single-function devices answer on every function number
                if function == 0 && dev.read(0x0c) & 0x0080_0000 == 0 {
                    break;
                }
            }
        }
    }
    out
}
