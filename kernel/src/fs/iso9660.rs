//! CDs and DVDs: the ISO 9660 file system, read only. Long names come
//! from the Joliet tree when the disc has one, or from Rock Ridge "NM"
//! entries; plain 8.3 names lose their ";1".
//!
//! The RyzikOS disc itself is one: it carries sample videos and photos.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use super::ata::{Atapi, CD_SECTOR};
use super::{Error, Info};

/// Biggest file read into memory at once.
const MAX_FILE: usize = 96 * 1024 * 1024;

#[derive(Clone)]
struct Record {
    name: String,
    dir: bool,
    lba: u32,
    size: u32,
    modified: (u16, u8, u8, u8, u8),
}

pub struct Disc {
    dev: Atapi,
    root: Record,
    joliet: bool,
    pub label: String,
}

impl Disc {
    /// Read the volume descriptors of the disc in `dev`.
    pub fn mount(mut dev: Atapi) -> Option<Disc> {
        let mut sector = vec![0u8; CD_SECTOR];
        let mut primary = None;
        let mut joliet = None;
        let mut label = String::new();
        for lba in 16..48 {
            dev.read(lba, &mut sector).ok()?;
            if &sector[1..6] != b"CD001" {
                return None;
            }
            match sector[0] {
                1 if primary.is_none() => {
                    primary = parse_record(&sector[156..190], false);
                    label = String::from(
                        core::str::from_utf8(&sector[40..72])
                            .unwrap_or("")
                            .trim_end(),
                    );
                }
                2 if sector[88] == b'%'
                    && sector[89] == b'/'
                    && matches!(sector[90], b'@' | b'C' | b'E') =>
                {
                    joliet = parse_record(&sector[156..190], true);
                }
                255 => break,
                _ => {}
            }
        }
        let (root, is_joliet) = match (joliet, primary) {
            (Some(j), _) => (j, true),
            (None, Some(p)) => (p, false),
            _ => return None,
        };
        if label.is_empty() {
            label = String::from("Disc");
        }
        Some(Disc {
            dev,
            root,
            joliet: is_joliet,
            label,
        })
    }

    fn read_dir(&mut self, dir: &Record) -> Result<Vec<Record>, Error> {
        let len = (dir.size as usize).min(4 * 1024 * 1024);
        let sectors = len.div_ceil(CD_SECTOR);
        let mut data = vec![0u8; sectors * CD_SECTOR];
        self.dev.read(dir.lba, &mut data).map_err(|_| Error::Io)?;
        let mut out = Vec::new();
        let mut at = 0;
        while at < len {
            let n = data[at] as usize;
            if n == 0 {
                // records don't cross sectors: go on at the next one
                at = (at / CD_SECTOR + 1) * CD_SECTOR;
                continue;
            }
            if at + n > data.len() || n < 34 {
                break;
            }
            let rec = &data[at..at + n];
            let name_len = rec[32] as usize;
            let dot = name_len == 1 && (rec[33] == 0 || rec[33] == 1);
            if !dot {
                if let Some(r) = parse_record(rec, self.joliet) {
                    // a file in several extents shows up once
                    if !out.iter().any(|o: &Record| o.name == r.name) {
                        out.push(r);
                    }
                }
            }
            at += n;
        }
        Ok(out)
    }

    fn find(&mut self, path: &str) -> Result<Record, Error> {
        let mut cur = self.root.clone();
        for part in path.split('/').filter(|p| !p.is_empty()) {
            if !cur.dir {
                return Err(Error::NotADirectory);
            }
            let items = self.read_dir(&cur)?;
            cur = items
                .into_iter()
                .find(|r| super::same_name(&r.name, part))
                .ok_or(Error::NotFound)?;
        }
        Ok(cur)
    }

    /// What is in a folder of the disc; `path` is inside the disc.
    pub fn list(&mut self, path: &str) -> Result<Vec<Info>, Error> {
        let dir = self.find(path)?;
        if !dir.dir {
            return Err(Error::NotADirectory);
        }
        Ok(self
            .read_dir(&dir)?
            .into_iter()
            .map(|r| Info {
                name: r.name,
                dir: r.dir,
                size: r.size,
                modified: r.modified,
            })
            .collect())
    }

    pub fn read(&mut self, path: &str) -> Result<Vec<u8>, Error> {
        let r = self.find(path)?;
        if r.dir {
            return Err(Error::IsADirectory);
        }
        let len = r.size as usize;
        if len > MAX_FILE {
            return Err(Error::Full);
        }
        let mut data = vec![0u8; len.div_ceil(CD_SECTOR) * CD_SECTOR];
        self.dev.read(r.lba, &mut data).map_err(|_| Error::Io)?;
        data.truncate(len);
        Ok(data)
    }

    pub fn is_dir(&mut self, path: &str) -> bool {
        self.find(path).is_ok_and(|r| r.dir)
    }

    pub fn exists(&mut self, path: &str) -> bool {
        self.find(path).is_ok()
    }

    /// Whether the disc can still be read (it may have been taken out).
    pub fn present(&mut self) -> bool {
        let mut sector = vec![0u8; CD_SECTOR];
        self.dev.read(16, &mut sector).is_ok() && &sector[1..6] == b"CD001"
    }
}

fn parse_record(rec: &[u8], joliet: bool) -> Option<Record> {
    if rec.len() < 34 {
        return None;
    }
    let lba = u32::from_le_bytes(rec[2..6].try_into().ok()?);
    let size = u32::from_le_bytes(rec[10..14].try_into().ok()?);
    let modified = (
        1900 + rec[18] as u16,
        rec[19],
        rec[20],
        rec[21],
        rec[22],
    );
    let dir = rec[25] & 0x02 != 0;
    let name_len = rec[32] as usize;
    let raw = rec.get(33..33 + name_len)?;
    let mut name = if joliet {
        let units: Vec<u16> = raw
            .chunks_exact(2)
            .map(|p| u16::from_be_bytes([p[0], p[1]]))
            .collect();
        char::decode_utf16(units)
            .map(|c| c.unwrap_or('?'))
            .collect::<String>()
    } else {
        let mut n: String = raw.iter().map(|&b| b as char).collect();
        // Rock Ridge keeps the real name in the system use area
        let mut su = 33 + name_len + (name_len + 1) % 2;
        while su + 4 <= rec.len() {
            let len = rec[su + 2] as usize;
            if len < 4 || su + len > rec.len() {
                break;
            }
            if &rec[su..su + 2] == b"NM" && len > 5 {
                n = String::from(core::str::from_utf8(&rec[su + 5..su + len]).unwrap_or(&n));
                break;
            }
            su += len;
        }
        n
    };
    if let Some(i) = name.find(';') {
        name.truncate(i);
    }
    if name.ends_with('.') {
        name.pop();
    }
    if name.is_empty() {
        return None;
    }
    Some(Record {
        name,
        dir,
        lba,
        size,
        modified,
    })
}
