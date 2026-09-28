//! FAT32, the file system Windows reads from USB sticks and memory cards,
//! with long file names (up to 255 characters, any language).
//!
//! The whole allocation table is kept in memory, so following and
//! allocating clusters costs no disk reads; changed parts of it are
//! written back to both copies on the disk after every change.
//! Directories are small, so they are read and written whole.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use super::drive::Disk;
use super::Error;

const SECTOR: usize = 512;
/// Where a freshly formatted disk's partition starts: 1 MiB, like Windows.
const PART_START: u64 = 2048;
const RESERVED: u32 = 32;
const FATS: u32 = 2;
const ENTRY: usize = 32;

const MASK: u32 = 0x0fff_ffff;
const END: u32 = 0x0fff_ffff;
const FREE: u32 = 0;

pub const ATTR_DIR: u8 = 0x10;
const ATTR_ARCHIVE: u8 = 0x20;
const ATTR_LABEL: u8 = 0x08;
const ATTR_LFN: u8 = 0x0f;

/// Where the sectors come from.
pub enum Device {
    Disk(Disk),
    /// A disk in memory, for when the computer has no hard disk.
    Ram(Vec<u8>),
}

impl Device {
    pub fn sectors(&self) -> u64 {
        match self {
            Device::Disk(a) => a.sectors(),
            Device::Ram(m) => (m.len() / SECTOR) as u64,
        }
    }

    fn read(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), Error> {
        match self {
            Device::Disk(a) => a.read(lba, buf).map_err(|_| Error::Io),
            Device::Ram(m) => {
                let start = lba as usize * SECTOR;
                let src = m.get(start..start + buf.len()).ok_or(Error::Io)?;
                buf.copy_from_slice(src);
                Ok(())
            }
        }
    }

    fn write(&mut self, lba: u64, buf: &[u8]) -> Result<(), Error> {
        match self {
            Device::Disk(a) => a.write(lba, buf).map_err(|_| Error::Io),
            Device::Ram(m) => {
                let start = lba as usize * SECTOR;
                let dst = m.get_mut(start..start + buf.len()).ok_or(Error::Io)?;
                dst.copy_from_slice(buf);
                Ok(())
            }
        }
    }

    fn flush(&mut self) -> Result<(), Error> {
        match self {
            Device::Disk(a) => a.flush().map_err(|_| Error::Io),
            Device::Ram(_) => Ok(()),
        }
    }

    pub fn into_disk(self) -> Option<Disk> {
        match self {
            Device::Disk(d) => Some(d),
            Device::Ram(_) => None,
        }
    }
}

/// The biggest partition RyzikOS makes: 64 GiB. Its whole allocation
/// table is kept in memory, so a bigger one could not be opened.
const MAX_PART: u64 = 64 * 1024 * 1024 * 2;

/// A file or folder as a listing shows it.
#[derive(Clone)]
pub struct Info {
    pub name: String,
    pub dir: bool,
    pub size: u32,
    /// Last change: year, month, day, hour, minute.
    pub modified: (u16, u8, u8, u8, u8),
}

/// A directory entry, with where it sits in its directory.
#[derive(Clone)]
struct Entry {
    name: String,
    attr: u8,
    cluster: u32,
    size: u32,
    /// The first slot (its long name entries) and the short entry itself.
    first: usize,
    slot: usize,
}

impl Entry {
    fn is_dir(&self) -> bool {
        self.attr & ATTR_DIR != 0
    }
}

/// A directory read into memory.
struct Dir {
    chain: Vec<u32>,
    data: Vec<u8>,
}

/// What `open` learns about a FAT32 volume before it takes the device.
struct Geometry {
    start: u64,
    cluster_bytes: usize,
    spc: u32,
    fat_start: u64,
    fat_sectors: u32,
    fats: u32,
    data_start: u64,
    root: u32,
    clusters: u32,
    fat: Vec<u32>,
    dirty: Vec<bool>,
    info_lba: Option<u64>,
    free_count: u32,
}

impl Geometry {
    fn volume(self, dev: Device) -> Volume {
        Volume {
            start: self.start,
            cluster_bytes: self.cluster_bytes,
            spc: self.spc,
            fat_start: self.fat_start,
            fat_sectors: self.fat_sectors,
            fats: self.fats,
            data_start: self.data_start,
            root: self.root,
            clusters: self.clusters,
            fat: self.fat,
            dirty: self.dirty,
            next_free: 2,
            info_lba: self.info_lba,
            free_count: self.free_count,
            info_dirty: true,
            bytes: dev.sectors() * SECTOR as u64,
            dev,
        }
    }
}

pub struct Volume {
    dev: Device,
    /// Where the partition starts, in sectors.
    start: u64,
    cluster_bytes: usize,
    spc: u32,
    fat_start: u64,
    fat_sectors: u32,
    fats: u32,
    data_start: u64,
    root: u32,
    /// Valid clusters are 2 to `clusters + 1`.
    clusters: u32,
    fat: Vec<u32>,
    /// FAT sectors changed since the last flush.
    dirty: Vec<bool>,
    next_free: u32,
    /// The FSInfo sector, which caches the number of free clusters for
    /// Windows, and that number.
    info_lba: Option<u64>,
    free_count: u32,
    info_dirty: bool,
    /// Size in bytes, for the status line.
    pub bytes: u64,
}

fn u16_at(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}

fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

fn put16(b: &mut [u8], i: usize, v: u16) {
    b[i..i + 2].copy_from_slice(&v.to_le_bytes());
}

fn put32(b: &mut [u8], i: usize, v: u32) {
    b[i..i + 4].copy_from_slice(&v.to_le_bytes());
}

/// Whether a sector is a FAT32 boot sector.
fn is_fat32(b: &[u8]) -> bool {
    let spc = b[13];
    b[510] == 0x55
        && b[511] == 0xaa
        && u16_at(b, 11) == SECTOR as u16
        && spc != 0
        && spc.is_power_of_two()
        && u16_at(b, 14) != 0
        && (1..=2).contains(&b[16])
        && u16_at(b, 17) == 0
        && u16_at(b, 22) == 0
        && u32_at(b, 36) != 0
}

fn random() -> u32 {
    let (lo, hi): (u32, u32);
    unsafe { core::arch::asm!("rdtsc", out("eax") lo, out("edx") hi, options(nomem, nostack)) };
    lo ^ hi.rotate_left(16) ^ 0x4576_4f53
}

/// FAT date and time now.
fn now() -> (u16, u16) {
    let (year, month, day) = crate::rtc::date();
    let (h, m, s) = crate::rtc::time();
    let date = (year.saturating_sub(1980) << 9) | (month as u16) << 5 | day as u16;
    let time = (h as u16) << 11 | (m as u16) << 5 | (s as u16 / 2);
    (date, time)
}

/// Write an empty FAT32 file system on the disk, in one partition that
/// fills it, as Windows would. Returns where the partition starts.
pub fn format(dev: &mut Device) -> Result<u64, Error> {
    let total = dev.sectors();
    let start = if total >= 65536 { PART_START } else { 1 };
    let size = total
        .checked_sub(start)
        .map(|s| s.min(MAX_PART))
        .filter(|&s| s >= 4096)
        .ok_or(Error::Full)? as u32;
    // cluster sizes Windows picks for these volume sizes
    let spc: u32 = match size {
        0..=532_480 => 1,
        532_481..=16_777_216 => 8,
        16_777_217..=33_554_432 => 16,
        33_554_433..=67_108_864 => 32,
        _ => 64,
    };
    let per = (256 * spc + FATS) / 2;
    let fat_sectors = (size - RESERVED).div_ceil(per);

    // the partition table, with code that tells the BIOS to try the next
    // disk if someone boots from this one
    let mut mbr = [0u8; SECTOR];
    mbr[..5].copy_from_slice(&[0xcd, 0x18, 0xf4, 0xeb, 0xfd]);
    put32(&mut mbr, 440, random());
    let part = &mut mbr[446..462];
    part[1..4].copy_from_slice(&[0xfe, 0xff, 0xff]);
    part[0] = 0x80; // active, for BIOSes that look for it
    part[4] = 0x0c; // FAT32 with LBA
    part[5..8].copy_from_slice(&[0xfe, 0xff, 0xff]);
    put32(part, 8, start as u32);
    put32(part, 12, size);
    mbr[510] = 0x55;
    mbr[511] = 0xaa;

    // clear the reserved sectors, both FATs and the root folder
    let zeros = vec![0u8; 128 * SECTOR];
    let mut lba = start;
    let clear_end = start + (RESERVED + FATS * fat_sectors + spc) as u64;
    while lba < clear_end {
        let n = (clear_end - lba).min(128) as usize;
        dev.write(lba, &zeros[..n * SECTOR])?;
        lba += n as u64;
    }

    let mut boot = [0u8; SECTOR];
    boot[..3].copy_from_slice(&[0xeb, 0x58, 0x90]);
    boot[3..11].copy_from_slice(b"MSWIN4.1");
    put16(&mut boot, 11, SECTOR as u16);
    boot[13] = spc as u8;
    put16(&mut boot, 14, RESERVED as u16);
    boot[16] = FATS as u8;
    boot[21] = 0xf8;
    put16(&mut boot, 24, 63);
    put16(&mut boot, 26, 255);
    put32(&mut boot, 28, start as u32);
    put32(&mut boot, 32, size);
    put32(&mut boot, 36, fat_sectors);
    put32(&mut boot, 44, 2);
    put16(&mut boot, 48, 1);
    put16(&mut boot, 50, 6);
    boot[64] = 0x80;
    boot[66] = 0x29;
    put32(&mut boot, 67, random());
    boot[71..82].copy_from_slice(b"RYZIKOS    ");
    boot[82..90].copy_from_slice(b"FAT32   ");
    // not bootable: ask the BIOS for the next disk
    boot[90..95].copy_from_slice(&[0xcd, 0x18, 0xf4, 0xeb, 0xfd]);
    boot[510] = 0x55;
    boot[511] = 0xaa;

    let mut info = [0u8; SECTOR];
    put32(&mut info, 0, 0x4161_5252);
    put32(&mut info, 484, 0x6141_7272);
    put32(&mut info, 488, 0xffff_ffff); // free count unknown
    put32(&mut info, 492, 0xffff_ffff);
    put32(&mut info, 508, 0xaa55_0000);

    let mut fat = [0u8; SECTOR];
    put32(&mut fat, 0, 0x0fff_fff8);
    put32(&mut fat, 4, 0x0fff_ffff);
    put32(&mut fat, 8, END); // the root folder

    let mut root = vec![0u8; spc as usize * SECTOR];
    root[..11].copy_from_slice(b"RYZIKOS    ");
    root[11] = ATTR_LABEL;
    let (date, time) = now();
    put16(&mut root, 22, time);
    put16(&mut root, 24, date);

    for copy in [0u64, 6] {
        dev.write(start + copy, &boot)?;
        dev.write(start + copy + 1, &info)?;
    }
    for i in 0..FATS {
        dev.write(start + (RESERVED + i * fat_sectors) as u64, &fat)?;
    }
    dev.write(start + (RESERVED + FATS * fat_sectors) as u64, &root)?;
    dev.write(0, &mbr)?;
    dev.flush()?;
    Ok(start)
}

impl Volume {
    /// Open the FAT32 file system on a disk. With `format_blank`, a blank
    /// disk (only zeros in its first sector) is formatted first; anything
    /// else unknown is left alone. Returns the volume and whether it was
    /// just formatted, or why not and the device back.
    pub fn open(mut dev: Device, format_blank: bool) -> Result<(Volume, bool), (Error, Device)> {
        match Self::open_on(&mut dev, format_blank) {
            Ok((geometry, formatted)) => Ok((geometry.volume(dev), formatted)),
            Err(e) => Err((e, dev)),
        }
    }

    fn open_on(dev: &mut Device, format_blank: bool) -> Result<(Geometry, bool), Error> {
        let mut s0 = [0u8; SECTOR];
        dev.read(0, &mut s0)?;
        let mut formatted = false;
        let start = if is_fat32(&s0) {
            0
        } else if s0.iter().all(|&b| b == 0) {
            if !format_blank {
                return Err(Error::Blank);
            }
            formatted = true;
            format(dev)?
        } else if s0[510..] == [0x55, 0xaa] {
            (0..4)
                .map(|i| &s0[446 + i * 16..462 + i * 16])
                .find(|p| matches!(p[4], 0x0b | 0x0c | 0x1b | 0x1c))
                .map(|p| u32_at(p, 8) as u64)
                .ok_or(Error::Unformatted)?
        } else {
            return Err(Error::Unformatted);
        };
        let mut b = [0u8; SECTOR];
        dev.read(start, &mut b)?;
        if !is_fat32(&b) {
            return Err(Error::Unformatted);
        }
        let spc = b[13] as u32;
        let reserved = u16_at(&b, 14) as u32;
        let fats = b[16] as u32;
        let total = match u16_at(&b, 19) {
            0 => u32_at(&b, 32),
            n => n as u32,
        };
        let fat_sectors = u32_at(&b, 36);
        let meta = reserved + fats * fat_sectors;
        let clusters = total.checked_sub(meta).ok_or(Error::Unformatted)? / spc;
        // the table must fit in memory and in the space the FAT has
        let used = ((clusters as usize + 2) * 4).div_ceil(SECTOR);
        if used > fat_sectors as usize || used > 32 * 1024 {
            return Err(Error::Unformatted);
        }
        let fat_start = start + reserved as u64;
        let mut raw = vec![0u8; used * SECTOR];
        dev.read(fat_start, &mut raw)?;
        let fat: Vec<u32> = raw
            .chunks(4)
            .take(clusters as usize + 2)
            .map(|c| u32_at(c, 0))
            .collect();
        let info = u16_at(&b, 48) as u32;
        let info_lba = (info > 0 && info < reserved).then_some(start + info as u64);
        let free_count = fat.iter().skip(2).filter(|&&v| v & MASK == FREE).count() as u32;
        let geometry = Geometry {
            start,
            cluster_bytes: spc as usize * SECTOR,
            spc,
            fat_start,
            fat_sectors,
            fats,
            data_start: fat_start + (fats * fat_sectors) as u64,
            root: u32_at(&b, 44),
            clusters,
            fat,
            dirty: vec![false; used],
            info_lba,
            free_count,
        };
        Ok((geometry, formatted))
    }

    /// Give the device back, with everything written.
    pub fn into_device(mut self) -> Device {
        let _ = self.sync();
        self.dev
    }

    /// Where the partition starts, in sectors.
    pub fn start(&self) -> u64 {
        self.start
    }

    /// Read or write sectors outside the file system: the boot sectors.
    pub fn read_sectors(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), Error> {
        self.dev.read(lba, buf)
    }

    pub fn write_sectors(&mut self, lba: u64, buf: &[u8]) -> Result<(), Error> {
        self.dev.write(lba, buf)?;
        self.dev.flush()
    }

    // ---- clusters ---------------------------------------------------------

    fn valid(&self, c: u32) -> bool {
        c >= 2 && c < self.clusters + 2
    }

    fn next(&self, c: u32) -> Option<u32> {
        let n = self.fat[c as usize] & MASK;
        self.valid(n).then_some(n)
    }

    fn set(&mut self, c: u32, v: u32) {
        let old = self.fat[c as usize];
        self.fat[c as usize] = (old & !MASK) | (v & MASK);
        self.dirty[c as usize * 4 / SECTOR] = true;
    }

    fn chain(&self, first: u32) -> Vec<u32> {
        let mut out = Vec::new();
        let mut c = first;
        while self.valid(c) && out.len() <= self.clusters as usize {
            out.push(c);
            match self.next(c) {
                Some(n) => c = n,
                None => break,
            }
        }
        out
    }

    /// Take `n` free clusters and link them into a chain.
    fn alloc(&mut self, n: usize) -> Result<Vec<u32>, Error> {
        let mut out = Vec::with_capacity(n);
        let total = self.clusters;
        let mut c = self.next_free;
        for _ in 0..total {
            if out.len() == n {
                break;
            }
            if !self.valid(c) {
                c = 2;
            }
            if self.fat[c as usize] & MASK == FREE {
                out.push(c);
            }
            c += 1;
        }
        if out.len() < n {
            return Err(Error::Full);
        }
        for i in 0..n {
            let next = if i + 1 < n { out[i + 1] } else { END };
            self.set(out[i], next);
        }
        self.free_count -= n as u32;
        self.info_dirty = true;
        self.next_free = c;
        Ok(out)
    }

    fn free(&mut self, first: u32) {
        for c in self.chain(first) {
            self.set(c, FREE);
            self.free_count += 1;
        }
        self.info_dirty = true;
    }

    fn cluster_lba(&self, c: u32) -> u64 {
        self.data_start + (c - 2) as u64 * self.spc as u64
    }

    fn read_chain(&mut self, chain: &[u32]) -> Result<Vec<u8>, Error> {
        let mut data = vec![0u8; chain.len() * self.cluster_bytes];
        for (i, &c) in chain.iter().enumerate() {
            let lba = self.cluster_lba(c);
            let part = &mut data[i * self.cluster_bytes..(i + 1) * self.cluster_bytes];
            self.dev.read(lba, part)?;
        }
        Ok(data)
    }

    /// Write `data` over the clusters of a chain; the last one is padded
    /// with zeros.
    fn write_chain(&mut self, chain: &[u32], data: &[u8]) -> Result<(), Error> {
        let mut buf = vec![0u8; self.cluster_bytes];
        for (i, &c) in chain.iter().enumerate() {
            let part = data
                .get(i * self.cluster_bytes..)
                .unwrap_or(&[])
                .iter()
                .take(self.cluster_bytes);
            buf.fill(0);
            for (d, s) in buf.iter_mut().zip(part) {
                *d = *s;
            }
            let lba = self.cluster_lba(c);
            self.dev.write(lba, &buf)?;
        }
        Ok(())
    }

    /// Bytes not used by any file.
    pub fn free_bytes(&self) -> u64 {
        self.free_count as u64 * self.cluster_bytes as u64
    }

    /// Write the changed parts of the table to every copy, then make the
    /// disk keep everything.
    pub fn sync(&mut self) -> Result<(), Error> {
        for i in 0..self.dirty.len() {
            if !core::mem::replace(&mut self.dirty[i], false) {
                continue;
            }
            let mut buf = [0u8; SECTOR];
            for (j, v) in self
                .fat
                .iter()
                .skip(i * SECTOR / 4)
                .take(SECTOR / 4)
                .enumerate()
            {
                put32(&mut buf, j * 4, *v);
            }
            for copy in 0..self.fats {
                let lba = self.fat_start + (copy * self.fat_sectors) as u64 + i as u64;
                self.dev.write(lba, &buf)?;
            }
        }
        if let Some(lba) = self.info_lba.filter(|_| self.info_dirty) {
            self.info_dirty = false;
            let mut info = [0u8; SECTOR];
            self.dev.read(lba, &mut info)?;
            if u32_at(&info, 0) == 0x4161_5252 && u32_at(&info, 484) == 0x6141_7272 {
                put32(&mut info, 488, self.free_count);
                put32(&mut info, 492, self.next_free);
                self.dev.write(lba, &info)?;
            }
        }
        self.dev.flush()
    }

    // ---- directories ------------------------------------------------------

    fn load_dir(&mut self, first: u32) -> Result<Dir, Error> {
        let first = if first == 0 { self.root } else { first };
        let chain = self.chain(first);
        let data = self.read_chain(&chain)?;
        Ok(Dir { chain, data })
    }

    fn save_dir(&mut self, dir: &Dir) -> Result<(), Error> {
        self.write_chain(&dir.chain, &dir.data)
    }

    /// The live entries of a directory, without "." and "..".
    fn entries(dir: &Dir) -> Vec<Entry> {
        let mut out = Vec::new();
        // long name being collected: checksum, first slot, UTF-16 text,
        // the sequence number expected next
        let mut lfn: Option<(u8, usize, Vec<u16>, u8)> = None;
        for (i, e) in dir.data.chunks(ENTRY).enumerate() {
            match e[0] {
                0 => break,
                0xe5 => {
                    lfn = None;
                    continue;
                }
                _ => {}
            }
            let attr = e[11];
            if attr == ATTR_LFN {
                let seq = e[0] & 0x1f;
                if e[0] & 0x40 != 0 {
                    lfn = Some((e[13], i, vec![0xffff; seq as usize * 13], seq));
                }
                match &mut lfn {
                    Some((sum, _, text, expect)) if *expect == seq && *sum == e[13] && seq > 0 => {
                        let at = (seq as usize - 1) * 13;
                        let offsets = [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30];
                        for (k, &o) in offsets.iter().enumerate() {
                            text[at + k] = u16_at(e, o);
                        }
                        *expect -= 1;
                    }
                    _ => lfn = None,
                }
                continue;
            }
            if attr & ATTR_LABEL != 0 {
                lfn = None;
                continue;
            }
            let short: [u8; 11] = e[..11].try_into().unwrap_or([b' '; 11]);
            let long = lfn
                .take()
                .filter(|(sum, _, _, expect)| *expect == 0 && *sum == checksum(&short));
            let (name, first) = match long {
                Some((_, first, text, _)) => {
                    let units = text.iter().copied().take_while(|&u| u != 0 && u != 0xffff);
                    let name = char::decode_utf16(units)
                        .map(|c| c.unwrap_or('?'))
                        .collect();
                    (name, first)
                }
                None => (short_to_string(&short, e[12]), i),
            };
            if name == "." || name == ".." {
                continue;
            }
            out.push(Entry {
                name,
                attr,
                cluster: (u16_at(e, 20) as u32) << 16 | u16_at(e, 26) as u32,
                size: u32_at(e, 28),
                first,
                slot: i,
            });
        }
        out
    }

    fn find(dir: &Dir, name: &str) -> Option<Entry> {
        Self::entries(dir)
            .into_iter()
            .find(|e| same_name(&e.name, name))
    }

    /// Add entries for `name` to a directory: its long name and a short
    /// entry made from `template` (attributes, cluster, size, times).
    fn add_entry(&mut self, dir: &mut Dir, name: &str, template: &[u8]) -> Result<(), Error> {
        let taken: Vec<[u8; 11]> = dir
            .data
            .chunks(ENTRY)
            .take_while(|e| e[0] != 0)
            .filter(|e| e[0] != 0xe5 && e[11] != ATTR_LFN)
            .map(|e| e[..11].try_into().unwrap_or([0; 11]))
            .collect();
        let (short, exact) = short_name(name, &taken)?;
        let units: Vec<u16> = name.encode_utf16().collect();
        let long = if exact { 0 } else { units.len().div_ceil(13) };
        let need = long + 1;

        // a run of free slots, or room after the end
        let slots = dir.data.len() / ENTRY;
        let mut start = None;
        let mut run = 0;
        for i in 0..slots {
            let b = dir.data[i * ENTRY];
            if b == 0 || b == 0xe5 {
                run += 1;
                if run == need {
                    start = Some(i + 1 - need);
                    break;
                }
            } else {
                run = 0;
            }
        }
        let start = match start {
            Some(s) => s,
            None => {
                // grow the directory by enough clusters
                let per = self.cluster_bytes / ENTRY;
                let n = need.div_ceil(per);
                let extra = self.alloc(n)?;
                if let Some(&last) = dir.chain.last() {
                    self.set(last, extra[0]);
                }
                dir.chain.extend_from_slice(&extra);
                let old = dir.data.len();
                dir.data.resize(old + n * self.cluster_bytes, 0);
                // the free run may begin before the old end
                slots - run
            }
        };

        let sum = checksum(&short);
        for k in 0..long {
            let seq = (long - k) as u8;
            let e = &mut dir.data[(start + k) * ENTRY..(start + k + 1) * ENTRY];
            e.fill(0);
            e[0] = seq | if k == 0 { 0x40 } else { 0 };
            e[11] = ATTR_LFN;
            e[13] = sum;
            let offsets = [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30];
            for (j, &o) in offsets.iter().enumerate() {
                let at = (seq as usize - 1) * 13 + j;
                let u = match at.cmp(&units.len()) {
                    core::cmp::Ordering::Less => units[at],
                    core::cmp::Ordering::Equal => 0,
                    core::cmp::Ordering::Greater => 0xffff,
                };
                put16(e, o, u);
            }
        }
        let e = &mut dir.data[(start + long) * ENTRY..(start + long + 1) * ENTRY];
        e.copy_from_slice(template);
        e[..11].copy_from_slice(&short);
        e[12] = 0;
        Ok(())
    }

    fn remove_entry(dir: &mut Dir, e: &Entry) {
        for slot in e.first..=e.slot {
            dir.data[slot * ENTRY] = 0xe5;
        }
    }

    // ---- paths ------------------------------------------------------------

    /// Find a path: the directory holding it and its entry (None for the
    /// root folder itself).
    fn resolve(&mut self, path: &str) -> Result<(u32, Option<Entry>), Error> {
        let mut dir = self.root;
        let mut found = None;
        let mut parts = path.split(['/', '\\']).filter(|p| !p.is_empty()).peekable();
        while let Some(part) = parts.next() {
            let d = self.load_dir(dir)?;
            let e = Self::find(&d, part).ok_or(Error::NotFound)?;
            if parts.peek().is_some() {
                if !e.is_dir() {
                    return Err(Error::NotFound);
                }
                dir = if e.cluster == 0 { self.root } else { e.cluster };
            } else {
                found = Some(e);
            }
        }
        Ok((dir, found))
    }

    fn dir_cluster(&mut self, path: &str) -> Result<u32, Error> {
        match self.resolve(path)? {
            (_, None) => Ok(self.root),
            (_, Some(e)) if e.is_dir() => Ok(if e.cluster == 0 { self.root } else { e.cluster }),
            _ => Err(Error::NotADirectory),
        }
    }

    // ---- operations -------------------------------------------------------

    pub fn list(&mut self, path: &str) -> Result<Vec<Info>, Error> {
        let c = self.dir_cluster(path)?;
        let dir = self.load_dir(c)?;
        Ok(Self::entries(&dir)
            .into_iter()
            .map(|e| {
                let at = e.slot * ENTRY;
                let raw = &dir.data[at..at + ENTRY];
                Info {
                    name: e.name.clone(),
                    dir: e.is_dir(),
                    size: e.size,
                    modified: decode_time(u16_at(raw, 24), u16_at(raw, 22)),
                }
            })
            .collect())
    }

    pub fn is_dir(&mut self, path: &str) -> bool {
        self.dir_cluster(path).is_ok()
    }

    pub fn exists(&mut self, path: &str) -> bool {
        self.resolve(path).is_ok()
    }

    pub fn read(&mut self, path: &str) -> Result<Vec<u8>, Error> {
        let (_, e) = self.resolve(path)?;
        let e = e.ok_or(Error::IsADirectory)?;
        if e.is_dir() {
            return Err(Error::IsADirectory);
        }
        let chain = self.chain(e.cluster);
        let mut data = self.read_chain(&chain)?;
        data.truncate(e.size as usize);
        Ok(data)
    }

    /// Where a file's data is, for reading parts of it with `read_at`:
    /// its clusters and its size.
    pub fn locate(&mut self, path: &str) -> Result<(Vec<u32>, u64), Error> {
        let (_, e) = self.resolve(path)?;
        let e = e.ok_or(Error::IsADirectory)?;
        if e.is_dir() {
            return Err(Error::IsADirectory);
        }
        Ok((self.chain(e.cluster), e.size as u64))
    }

    /// Read `buf.len()` bytes from `offset` into a file that `open` found.
    /// Clusters that follow each other on the disk are read in one go.
    pub fn read_at(&mut self, chain: &[u32], offset: u64, buf: &mut [u8]) -> Result<(), Error> {
        let cb = self.cluster_bytes;
        let mut done = 0;
        let mut tmp = Vec::new();
        while done < buf.len() {
            let at = offset as usize + done;
            let first = at / cb;
            let skip = at % cb;
            let want = buf.len() - done;
            // how many clusters in a row, up to 256 KiB
            let need = (skip + want).div_ceil(cb).min((256 * 1024 / cb).max(1));
            let mut run = 1;
            while run < need
                && chain.get(first + run).is_some_and(|&c| Some(c) == chain.get(first + run - 1).map(|p| p + 1))
            {
                run += 1;
            }
            let start = *chain.get(first).ok_or(Error::Io)?;
            tmp.resize(run * cb, 0);
            self.dev.read(self.cluster_lba(start), &mut tmp)?;
            let n = (run * cb - skip).min(want);
            buf[done..done + n].copy_from_slice(&tmp[skip..skip + n]);
            done += n;
        }
        Ok(())
    }

    /// Create or replace a file.
    pub fn write(&mut self, path: &str, bytes: &[u8]) -> Result<(), Error> {
        let (parent, name) = split(path);
        let dir_cluster = self.dir_cluster(parent)?;
        let mut dir = self.load_dir(dir_cluster)?;
        let old = Self::find(&dir, name);
        if old.as_ref().is_some_and(|e| e.is_dir()) {
            return Err(Error::IsADirectory);
        }
        if old.is_none() && !valid_name(name) {
            return Err(Error::BadName);
        }
        // write the new contents first, so a full disk keeps the old file
        let n = bytes.len().div_ceil(self.cluster_bytes);
        let chain = if n > 0 { self.alloc(n)? } else { Vec::new() };
        if let Err(e) = self.write_chain(&chain, bytes) {
            if let Some(&c) = chain.first() {
                self.free(c);
            }
            return Err(e);
        }
        let cluster = chain.first().copied().unwrap_or(0);
        let (date, time) = now();
        match old {
            Some(e) => {
                self.free(e.cluster);
                let raw = &mut dir.data[e.slot * ENTRY..(e.slot + 1) * ENTRY];
                raw[11] |= ATTR_ARCHIVE;
                put16(raw, 20, (cluster >> 16) as u16);
                put16(raw, 26, cluster as u16);
                put32(raw, 28, bytes.len() as u32);
                put16(raw, 22, time);
                put16(raw, 24, date);
                put16(raw, 18, date);
            }
            None => {
                let raw = new_entry(ATTR_ARCHIVE, cluster, bytes.len() as u32, date, time);
                if let Err(e) = self.add_entry(&mut dir, name, &raw) {
                    if cluster != 0 {
                        self.free(cluster);
                    }
                    return Err(e);
                }
            }
        }
        self.save_dir(&dir)?;
        self.sync()
    }

    pub fn create_dir(&mut self, path: &str) -> Result<(), Error> {
        let (parent, name) = split(path);
        if !valid_name(name) {
            return Err(Error::BadName);
        }
        let parent_cluster = self.dir_cluster(parent)?;
        let mut dir = self.load_dir(parent_cluster)?;
        if Self::find(&dir, name).is_some() {
            return Err(Error::Exists);
        }
        let c = self.alloc(1)?[0];
        let (date, time) = now();
        let mut data = vec![0u8; self.cluster_bytes];
        let dot = new_entry(ATTR_DIR, c, 0, date, time);
        data[..ENTRY].copy_from_slice(&dot);
        data[..11].copy_from_slice(b".          ");
        let up = if parent_cluster == self.root {
            0
        } else {
            parent_cluster
        };
        let dotdot = new_entry(ATTR_DIR, up, 0, date, time);
        data[ENTRY..2 * ENTRY].copy_from_slice(&dotdot);
        data[ENTRY..ENTRY + 11].copy_from_slice(b"..         ");
        let result = self.write_chain(&[c], &data).and_then(|_| {
            let raw = new_entry(ATTR_DIR, c, 0, date, time);
            self.add_entry(&mut dir, name, &raw)
        });
        if let Err(e) = result {
            self.free(c);
            return Err(e);
        }
        self.save_dir(&dir)?;
        self.sync()
    }

    /// Delete a file, or a folder with everything in it.
    pub fn remove(&mut self, path: &str) -> Result<(), Error> {
        self.remove_inner(path, 0)?;
        self.sync()
    }

    fn remove_inner(&mut self, path: &str, depth: usize) -> Result<(), Error> {
        let (parent, e) = self.resolve(path)?;
        let e = e.ok_or(Error::BadName)?; // not the root folder
        if e.is_dir() {
            if depth > 64 {
                return Err(Error::Io);
            }
            for child in self.list(path)? {
                let mut sub = String::from(path);
                sub.push('/');
                sub.push_str(&child.name);
                self.remove_inner(&sub, depth + 1)?;
            }
        }
        let mut dir = self.load_dir(parent)?;
        // find it again: the folder may have changed underneath
        let e = Self::find(&dir, &e.name).ok_or(Error::NotFound)?;
        Self::remove_entry(&mut dir, &e);
        self.save_dir(&dir)?;
        if e.cluster != 0 {
            self.free(e.cluster);
        }
        Ok(())
    }

    /// Give a file or folder a new name in the same folder.
    pub fn rename(&mut self, path: &str, new_name: &str) -> Result<(), Error> {
        if !valid_name(new_name) {
            return Err(Error::BadName);
        }
        let (parent, e) = self.resolve(path)?;
        let e = e.ok_or(Error::BadName)?;
        if e.name == new_name {
            return Ok(());
        }
        let mut dir = self.load_dir(parent)?;
        if let Some(other) = Self::find(&dir, new_name) {
            if other.slot != e.slot {
                return Err(Error::Exists);
            }
        }
        let template: [u8; ENTRY] = dir.data[e.slot * ENTRY..(e.slot + 1) * ENTRY]
            .try_into()
            .map_err(|_| Error::Io)?;
        Self::remove_entry(&mut dir, &e);
        self.add_entry(&mut dir, new_name, &template)?;
        self.save_dir(&dir)?;
        self.sync()
    }
}

/// A short entry with no name yet.
fn new_entry(attr: u8, cluster: u32, size: u32, date: u16, time: u16) -> [u8; ENTRY] {
    let mut e = [0u8; ENTRY];
    e[..11].fill(b' ');
    e[11] = attr;
    put16(&mut e, 14, time);
    put16(&mut e, 16, date);
    put16(&mut e, 18, date);
    put16(&mut e, 20, (cluster >> 16) as u16);
    put16(&mut e, 22, time);
    put16(&mut e, 24, date);
    put16(&mut e, 26, cluster as u16);
    put32(&mut e, 28, size);
    e
}

fn decode_time(date: u16, time: u16) -> (u16, u8, u8, u8, u8) {
    (
        1980 + (date >> 9),
        ((date >> 5) & 15) as u8,
        (date & 31) as u8,
        (time >> 11) as u8,
        ((time >> 5) & 63) as u8,
    )
}

/// The folder part and the last name of a path.
pub fn split(path: &str) -> (&str, &str) {
    let path = path.trim_end_matches(['/', '\\']);
    match path.rfind(['/', '\\']) {
        Some(i) => (&path[..i], &path[i + 1..]),
        None => ("", path),
    }
}

/// Names compare like on Windows: without regard to case.
pub fn same_name(a: &str, b: &str) -> bool {
    a.chars()
        .flat_map(char::to_lowercase)
        .eq(b.chars().flat_map(char::to_lowercase))
}

/// Whether Windows would accept a name for a file or folder.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name.encode_utf16().count() <= 255
        && !name.ends_with([' ', '.'])
        && !name
            .chars()
            .any(|c| (c as u32) < 0x20 || "\\/:*?\"<>|".contains(c))
}

fn checksum(short: &[u8; 11]) -> u8 {
    short
        .iter()
        .fold(0u8, |sum, &b| sum.rotate_right(1).wrapping_add(b))
}

fn short_to_string(short: &[u8; 11], case: u8) -> String {
    let mut s = String::new();
    let base = short[..8]
        .iter()
        .rposition(|&b| b != b' ')
        .map_or(0, |i| i + 1);
    let ext = short[8..]
        .iter()
        .rposition(|&b| b != b' ')
        .map_or(0, |i| i + 1);
    for (i, &b) in short[..base].iter().enumerate() {
        let b = if i == 0 && b == 0x05 { 0xe5 } else { b };
        let lower = case & 0x08 != 0;
        s.push(if lower { b.to_ascii_lowercase() } else { b } as char);
    }
    if ext > 0 {
        s.push('.');
        for &b in &short[8..8 + ext] {
            let lower = case & 0x10 != 0;
            s.push(if lower { b.to_ascii_lowercase() } else { b } as char);
        }
    }
    s
}

/// The 8.3 name for a long name, unique among `taken`, and whether it
/// is exactly the name (then no long name entries are needed).
fn short_name(name: &str, taken: &[[u8; 11]]) -> Result<([u8; 11], bool), Error> {
    const ALLOWED: &[u8] = b"$%'-_@~`!(){}^#&";
    let keep = |c: char| -> Option<u8> {
        let u = c.to_ascii_uppercase();
        (u.is_ascii_alphanumeric() || (u.is_ascii() && ALLOWED.contains(&(u as u8))))
            .then_some(u as u8)
    };
    let (base, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i + 1..]),
        _ => (name, ""),
    };
    let mut lossy = false;
    let mut b = Vec::new();
    for c in base.chars() {
        match keep(c) {
            Some(u) => b.push(u),
            None if c == ' ' || c == '.' => lossy = true,
            None => {
                b.push(b'_');
                lossy = true;
            }
        }
    }
    let mut x = Vec::new();
    for c in ext.chars() {
        match keep(c) {
            Some(u) => x.push(u),
            None => {
                if c != ' ' {
                    x.push(b'_');
                }
                lossy = true;
            }
        }
    }
    if b.len() > 8 || x.len() > 3 {
        lossy = true;
    }
    if b.is_empty() {
        b.push(b'_');
        lossy = true;
    }
    x.truncate(3);
    let mut short = [b' '; 11];
    for (i, &c) in x.iter().enumerate() {
        short[8 + i] = c;
    }
    let exact_case = name == short_to_string(&make(&b, &short), 0);
    if !lossy && exact_case && b.len() <= 8 {
        let s = make(&b, &short);
        if !taken.contains(&s) {
            return Ok((s, true));
        }
    }
    // BASE~N.EXT
    for n in 1..100_000u32 {
        let mut tail = [0u8; 8];
        let mut len = 0;
        let mut v = n;
        let mut digits = [0u8; 6];
        let mut d = 0;
        while v > 0 {
            digits[d] = b'0' + (v % 10) as u8;
            v /= 10;
            d += 1;
        }
        tail[len] = b'~';
        len += 1;
        for i in (0..d).rev() {
            tail[len] = digits[i];
            len += 1;
        }
        let keep_len = b.len().min(8 - len);
        let mut s = short;
        s[..keep_len].copy_from_slice(&b[..keep_len]);
        s[keep_len..keep_len + len].copy_from_slice(&tail[..len]);
        for c in s[keep_len + len..8].iter_mut() {
            *c = b' ';
        }
        if !taken.contains(&s) {
            return Ok((s, false));
        }
    }
    Err(Error::Full)
}

fn make(base: &[u8], template: &[u8; 11]) -> [u8; 11] {
    let mut s = *template;
    for (i, c) in s[..8].iter_mut().enumerate() {
        *c = base.get(i).copied().unwrap_or(b' ');
    }
    s
}
