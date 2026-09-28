//! Archives for the Archiver app and the `zip`/`unzip` commands.
//!
//! ZIP files can be made, opened, changed and unpacked; their files are
//! packed with Deflate (what every ZIP tool reads) or stored as they are.
//! .tar, .tar.gz/.tgz and .gz files can be opened and unpacked.
//!
//! An archive is read into memory whole. Long work reports its progress
//! through a callback, which the app uses to pause its fiber and draw.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use miniz_oxide::deflate::core::{
    compress, create_comp_flags_from_zip_params, CompressorOxide, TDEFLFlush, TDEFLStatus,
};

/// The biggest archive or unpacked file handled: everything is in memory.
pub const MAX_BYTES: usize = 48 * 1024 * 1024;

/// Year, month, day, hour, minute, as `fs::Info` has them.
pub type Stamp = (u16, u8, u8, u8, u8);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Zip,
    Tar,
    /// A .tar.gz or .tgz: a tar inside gzip.
    TarGz,
    /// A single file packed with gzip.
    Gzip,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Zip => "ZIP",
            Kind::Tar => "TAR",
            Kind::TarGz => "TAR.GZ",
            Kind::Gzip => "GZIP",
        }
    }

    /// Only ZIP files can have files added or removed.
    pub fn editable(self) -> bool {
        self == Kind::Zip
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Method {
    Store,
    Deflate,
    /// Password protected: can't be unpacked.
    Encrypted,
    /// Another packing method (LZMA, BZip2, ...), by its ZIP number.
    Other(u16),
}

impl Method {
    pub fn name(self) -> String {
        match self {
            Method::Store => String::from("Stored"),
            Method::Deflate => String::from("Deflate"),
            Method::Encrypted => String::from("Encrypted"),
            Method::Other(12) => String::from("BZip2"),
            Method::Other(14) => String::from("LZMA"),
            Method::Other(9) => String::from("Deflate64"),
            Method::Other(93) => String::from("Zstandard"),
            Method::Other(n) => format!("Method {}", n),
        }
    }
}

/// A file or folder in an archive.
#[derive(Clone, Debug)]
pub struct Entry {
    /// Its path inside the archive, parts separated by `/`, without a
    /// `/` at either end.
    pub name: String,
    pub dir: bool,
    pub size: u64,
    /// Bytes it takes in the archive.
    pub packed: u64,
    pub modified: Stamp,
    pub method: Method,
    crc: u32,
    /// ZIP: where its local header starts. TAR: where its data starts.
    offset: usize,
    /// ZIP: the raw flags and the DOS time and date, kept when copied.
    flags: u16,
    dos: (u16, u16),
}

pub struct Archive {
    pub kind: Kind,
    /// The archive file's size.
    pub disk: usize,
    /// The archive's bytes (a tar.gz's unpacked tar).
    data: Vec<u8>,
    pub entries: Vec<Entry>,
}

pub type Progress<'a> = &'a mut dyn FnMut(u64, u64, &str) -> bool;

// ---- CRC-32 ------------------------------------------------------------------

const CRC_TABLE: [u32; 256] = {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xedb8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
};

pub fn crc32(data: &[u8]) -> u32 {
    crc32_update(0, data)
}

fn crc32_update(crc: u32, data: &[u8]) -> u32 {
    let mut c = !crc;
    for &b in data {
        c = CRC_TABLE[((c ^ b as u32) & 0xff) as usize] ^ (c >> 8);
    }
    !c
}

// ---- small helpers -------------------------------------------------------------

fn u16_at(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(at..at + 4)?.try_into().ok()?))
}

fn put16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn put32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn from_dos(time: u16, date: u16) -> Stamp {
    (
        1980 + (date >> 9),
        ((date >> 5) & 15) as u8,
        (date & 31) as u8,
        (time >> 11) as u8,
        ((time >> 5) & 63) as u8,
    )
}

fn to_dos(s: Stamp) -> (u16, u16) {
    let (y, mo, d, h, mi) = s;
    let y = y.clamp(1980, 2107);
    let date = (y - 1980) << 9 | (mo.clamp(1, 12) as u16) << 5 | d.clamp(1, 31) as u16;
    let time = (h.min(23) as u16) << 11 | (mi.min(59) as u16) << 5;
    (time, date)
}

fn from_unix(secs: i64) -> Stamp {
    let days = secs.div_euclid(86400);
    let rest = secs.rem_euclid(86400);
    let (y, m, d) = crate::rtc::civil_from_days(days);
    (
        y.clamp(0, 9999) as u16,
        m as u8,
        d as u8,
        (rest / 3600) as u8,
        (rest / 60 % 60) as u8,
    )
}

/// Code page 866, which Russian Windows uses for names in ZIP files that
/// are not marked as UTF-8.
fn cp866(b: u8) -> char {
    match b {
        0x00..=0x7f => b as char,
        0x80..=0xaf => char::from_u32(0x410 + (b - 0x80) as u32).unwrap_or('?'),
        0xe0..=0xef => char::from_u32(0x440 + (b - 0xe0) as u32).unwrap_or('?'),
        0xf0 => 'Ё',
        0xf1 => 'ё',
        0xf2 => 'Є',
        0xf3 => 'є',
        0xf4 => 'Ї',
        0xf5 => 'ї',
        0xf6 => 'Ў',
        0xf7 => 'ў',
        0xf8 => '°',
        0xfc => '№',
        0xff => ' ',
        _ => '_',
    }
}

/// Names that read as UTF-8 are taken as UTF-8 (7-Zip marks them, older
/// tools may not), the rest as CP866.
fn decode_name(raw: &[u8]) -> String {
    match core::str::from_utf8(raw) {
        Ok(s) => String::from(s),
        Err(_) => raw.iter().map(|&b| cp866(b)).collect(),
    }
}

/// A path from an archive made safe to unpack: `\` becomes `/`, and
/// empty, `.` and `..` parts and a drive letter are dropped, so nothing
/// lands outside the folder it is unpacked into.
pub fn clean_path(name: &str) -> String {
    let mut out = String::new();
    for part in name.split(['/', '\\']) {
        let part = part.trim();
        if part.is_empty() || part == "." || part == ".." || part.ends_with(':') {
            continue;
        }
        let part: String = part
            .chars()
            .map(|c| {
                if "*?\"<>|:".contains(c) || c.is_control() {
                    '_'
                } else {
                    c
                }
            })
            .collect();
        if !out.is_empty() {
            out.push('/');
        }
        out.push_str(&part);
    }
    out
}

// ---- reading -------------------------------------------------------------------

impl Archive {
    /// What kind of archive a file name says it is.
    pub fn kind_of(name: &str) -> Option<Kind> {
        let lower = name.to_ascii_lowercase();
        if lower.ends_with(".zip") {
            Some(Kind::Zip)
        } else if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
            Some(Kind::TarGz)
        } else if lower.ends_with(".tar") {
            Some(Kind::Tar)
        } else if lower.ends_with(".gz") {
            Some(Kind::Gzip)
        } else {
            None
        }
    }

    /// An empty ZIP archive, to add files to.
    pub fn new_zip() -> Archive {
        Archive {
            kind: Kind::Zip,
            disk: 0,
            data: Vec::new(),
            entries: Vec::new(),
        }
    }

    /// Read an archive; `name` is its file name, for a .gz file's inner
    /// name. The contents decide the kind, not the name.
    pub fn parse(data: Vec<u8>, name: &str) -> Result<Archive, String> {
        if data.len() > MAX_BYTES {
            return Err(String::from(
                "The archive is too big: RyzikOS opens archives up to 48 MB.",
            ));
        }
        let disk = data.len();
        if data.starts_with(b"PK\x03\x04") || data.starts_with(b"PK\x05\x06") {
            return parse_zip(data);
        }
        if data.starts_with(&[0x1f, 0x8b]) {
            let (inner, inner_name) = gunzip(&data)?;
            if is_tar(&inner) {
                let entries = parse_tar(&inner)?;
                return Ok(Archive {
                    kind: Kind::TarGz,
                    disk,
                    data: inner,
                    entries,
                });
            }
            let inner_name = inner_name.unwrap_or_else(|| {
                let base = crate::fs::file_name(name);
                let lower = base.to_ascii_lowercase();
                if lower.ends_with(".gz") && base.len() > 3 {
                    String::from(&base[..base.len() - 3])
                } else {
                    format!("{}.out", base)
                }
            });
            let size = inner.len() as u64;
            let entry = Entry {
                name: clean_path(&inner_name),
                dir: false,
                size,
                packed: data.len() as u64,
                modified: gzip_time(&data),
                method: Method::Deflate,
                crc: 0,
                offset: 0,
                flags: 0,
                dos: (0, 0),
            };
            return Ok(Archive {
                kind: Kind::Gzip,
                disk,
                data: inner,
                entries: vec![entry],
            });
        }
        if is_tar(&data) {
            let entries = parse_tar(&data)?;
            return Ok(Archive {
                kind: Kind::Tar,
                disk,
                data,
                entries,
            });
        }
        if data.is_empty() && Archive::kind_of(name) == Some(Kind::Zip) {
            return Ok(Archive::new_zip());
        }
        if data.starts_with(b"Rar!") {
            return Err(String::from(
                "RAR archives can't be opened yet. Only ZIP, TAR and GZIP can.",
            ));
        }
        if data.starts_with(b"7z\xbc\xaf\x27\x1c") {
            return Err(String::from(
                "7z archives can't be opened yet. Only ZIP, TAR and GZIP can.",
            ));
        }
        Err(String::from(
            "This is not an archive RyzikOS knows. It opens ZIP, TAR and GZIP.",
        ))
    }

    /// The bytes of an unpacked file.
    pub fn read(&self, i: usize) -> Result<Vec<u8>, String> {
        let e = &self.entries[i];
        if e.dir {
            return Ok(Vec::new());
        }
        match self.kind {
            Kind::Zip => {
                let raw = self.raw_zip_data(e)?;
                let out = match e.method {
                    Method::Store => raw.to_vec(),
                    Method::Deflate => miniz_oxide::inflate::decompress_to_vec_with_limit(
                        raw, MAX_BYTES,
                    )
                    .map_err(|_| format!("{} is damaged: it could not be unpacked.", e.name))?,
                    Method::Encrypted => {
                        return Err(format!(
                            "{} is protected with a password, which RyzikOS can't open yet.",
                            e.name
                        ))
                    }
                    m => {
                        return Err(format!(
                            "{} is packed with {}, which RyzikOS can't unpack yet.",
                            e.name,
                            m.name()
                        ))
                    }
                };
                if out.len() as u64 != e.size || crc32(&out) != e.crc {
                    return Err(format!(
                        "{} is damaged: its checksum does not match.",
                        e.name
                    ));
                }
                Ok(out)
            }
            Kind::Tar | Kind::TarGz => {
                let end = e.offset + e.size as usize;
                self.data
                    .get(e.offset..end)
                    .map(|d| d.to_vec())
                    .ok_or_else(|| format!("{} is cut short in the archive.", e.name))
            }
            Kind::Gzip => Ok(self.data.clone()),
        }
    }

    /// A ZIP entry's packed bytes, after its local header.
    fn raw_zip_data(&self, e: &Entry) -> Result<&[u8], String> {
        let d = &self.data;
        let at = e.offset;
        let bad = || format!("{} is damaged: its header is missing.", e.name);
        if u32_at(d, at) != Some(0x0403_4b50) {
            return Err(bad());
        }
        let name_len = u16_at(d, at + 26).ok_or_else(bad)? as usize;
        let extra_len = u16_at(d, at + 28).ok_or_else(bad)? as usize;
        let start = at + 30 + name_len + extra_len;
        d.get(start..start + e.packed as usize).ok_or_else(bad)
    }

    /// Bytes of all files, unpacked and packed.
    pub fn totals(&self) -> (u64, u64) {
        let files = self.entries.iter().filter(|e| !e.dir);
        files.fold((0, 0), |(s, p), e| (s + e.size, p + e.packed))
    }

    /// The archive's size on disk.
    pub fn bytes(&self) -> usize {
        self.data.len()
    }
}

fn parse_zip(data: Vec<u8>) -> Result<Archive, String> {
    let damaged = || String::from("The ZIP file is damaged: its table of contents was not found.");
    // the end record is in the last 22 + 65535 bytes (a comment can follow it)
    let low = data.len().saturating_sub(22 + 65535);
    let mut end = None;
    let mut at = data.len().saturating_sub(22);
    loop {
        if u32_at(&data, at) == Some(0x0605_4b50) {
            end = Some(at);
            break;
        }
        if at == low || at == 0 {
            break;
        }
        at -= 1;
    }
    let end = end.ok_or_else(damaged)?;
    let count = u16_at(&data, end + 10).ok_or_else(damaged)? as usize;
    let dir_at = u32_at(&data, end + 16).ok_or_else(damaged)?;
    if count == 0xffff || dir_at == 0xffff_ffff {
        return Err(String::from(
            "This is a ZIP64 archive, for files over 4 GB, which RyzikOS can't open.",
        ));
    }
    let mut entries = Vec::with_capacity(count);
    let mut p = dir_at as usize;
    for _ in 0..count {
        if u32_at(&data, p) != Some(0x0201_4b50) {
            return Err(damaged());
        }
        let flags = u16_at(&data, p + 8).ok_or_else(damaged)?;
        let method = u16_at(&data, p + 10).ok_or_else(damaged)?;
        let time = u16_at(&data, p + 12).ok_or_else(damaged)?;
        let date = u16_at(&data, p + 14).ok_or_else(damaged)?;
        let crc = u32_at(&data, p + 16).ok_or_else(damaged)?;
        let packed = u32_at(&data, p + 20).ok_or_else(damaged)?;
        let size = u32_at(&data, p + 24).ok_or_else(damaged)?;
        let name_len = u16_at(&data, p + 28).ok_or_else(damaged)? as usize;
        let extra_len = u16_at(&data, p + 30).ok_or_else(damaged)? as usize;
        let comment_len = u16_at(&data, p + 32).ok_or_else(damaged)? as usize;
        let attrs = u32_at(&data, p + 38).ok_or_else(damaged)?;
        let offset = u32_at(&data, p + 42).ok_or_else(damaged)? as usize;
        let raw = data.get(p + 46..p + 46 + name_len).ok_or_else(damaged)?;
        let full = decode_name(raw);
        let dir = full.ends_with('/') || full.ends_with('\\') || attrs & 0x10 != 0 && size == 0;
        let name = clean_path(&full);
        p += 46 + name_len + extra_len + comment_len;
        if name.is_empty() {
            continue;
        }
        let method = if flags & 1 != 0 {
            Method::Encrypted
        } else {
            match method {
                0 => Method::Store,
                8 => Method::Deflate,
                n => Method::Other(n),
            }
        };
        entries.push(Entry {
            name,
            dir,
            size: size as u64,
            packed: packed as u64,
            modified: from_dos(time, date),
            method,
            crc,
            offset,
            flags,
            dos: (time, date),
        });
    }
    Ok(Archive {
        kind: Kind::Zip,
        disk: data.len(),
        data,
        entries,
    })
}

fn is_tar(d: &[u8]) -> bool {
    d.len() >= 512 && d.get(257..262) == Some(b"ustar")
}

fn octal(field: &[u8]) -> u64 {
    // GNU tar writes big sizes as binary, with the top bit set
    if field.first().is_some_and(|b| b & 0x80 != 0) {
        return field[1..].iter().fold(0u64, |n, &b| n << 8 | b as u64);
    }
    field
        .iter()
        .skip_while(|b| **b == b' ')
        .take_while(|b| (b'0'..=b'7').contains(b))
        .fold(0u64, |n, &b| n * 8 + (b - b'0') as u64)
}

fn c_string(field: &[u8]) -> String {
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    decode_name(&field[..end])
}

fn parse_tar(d: &[u8]) -> Result<Vec<Entry>, String> {
    let mut entries = Vec::new();
    let mut at = 0;
    let mut long_name: Option<String> = None;
    while at + 512 <= d.len() {
        let h = &d[at..at + 512];
        if h.iter().all(|&b| b == 0) {
            break;
        }
        let size = octal(&h[124..136]);
        let data_at = at + 512;
        let next = data_at + (size as usize).div_ceil(512) * 512;
        if data_at + size as usize > d.len() {
            return Err(String::from("The TAR archive is cut short."));
        }
        let kind = h[156];
        let body = &d[data_at..data_at + size as usize];
        match kind {
            // a GNU long name, for the next entry
            b'L' => long_name = Some(c_string(body)),
            // a PAX header: "len key=value\n" records
            b'x' => {
                for rec in body.split(|&b| b == b'\n') {
                    let rec = String::from_utf8_lossy(rec);
                    if let Some((_, kv)) = rec.split_once(' ') {
                        if let Some(path) = kv.strip_prefix("path=") {
                            long_name = Some(String::from(path));
                        }
                    }
                }
            }
            b'0' | 0 | b'7' | b'5' => {
                let name = long_name.take().unwrap_or_else(|| {
                    let prefix = c_string(&h[345..500]);
                    let name = c_string(&h[0..100]);
                    if prefix.is_empty() {
                        name
                    } else {
                        format!("{}/{}", prefix, name)
                    }
                });
                let dir = kind == b'5' || name.ends_with('/');
                let name = clean_path(&name);
                if !name.is_empty() {
                    entries.push(Entry {
                        name,
                        dir,
                        size: if dir { 0 } else { size },
                        packed: if dir { 0 } else { size },
                        modified: from_unix(octal(&h[136..148]) as i64),
                        method: Method::Store,
                        crc: 0,
                        offset: data_at,
                        flags: 0,
                        dos: (0, 0),
                    });
                }
            }
            // links and devices are skipped
            _ => long_name = None,
        }
        at = next;
    }
    Ok(entries)
}

fn gzip_time(d: &[u8]) -> Stamp {
    match u32_at(d, 4) {
        Some(t) if t != 0 => from_unix(t as i64),
        _ => (1980, 1, 1, 0, 0),
    }
}

/// Unpack a .gz file: its contents and the name stored in it.
fn gunzip(d: &[u8]) -> Result<(Vec<u8>, Option<String>), String> {
    let bad = || String::from("The GZIP file is damaged.");
    if d.len() < 18 || d[2] != 8 {
        return Err(bad());
    }
    let flags = d[3];
    let mut at = 10;
    if flags & 4 != 0 {
        at += 2 + u16_at(d, at).ok_or_else(bad)? as usize;
    }
    let mut name = None;
    if flags & 8 != 0 {
        let end = d
            .get(at..)
            .ok_or_else(bad)?
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(bad)?;
        name = Some(decode_name(&d[at..at + end]));
        at += end + 1;
    }
    if flags & 16 != 0 {
        let end = d
            .get(at..)
            .ok_or_else(bad)?
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(bad)?;
        at += end + 1;
    }
    if flags & 2 != 0 {
        at += 2;
    }
    let body = d.get(at..d.len() - 8).ok_or_else(bad)?;
    let out =
        miniz_oxide::inflate::decompress_to_vec_with_limit(body, MAX_BYTES).map_err(|_| bad())?;
    if u32_at(d, d.len() - 8) != Some(crc32(&out)) {
        return Err(bad());
    }
    Ok((out, name))
}

// ---- writing -------------------------------------------------------------------

/// How hard to pack: stored as is, fast, normal, smallest.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Level {
    Store,
    Fast,
    Normal,
    Best,
}

impl Level {
    pub const ALL: [Level; 4] = [Level::Store, Level::Fast, Level::Normal, Level::Best];

    pub fn name(self) -> &'static str {
        match self {
            Level::Store => "Store",
            Level::Fast => "Fast",
            Level::Normal => "Normal",
            Level::Best => "Best",
        }
    }

    fn zlib(self) -> i32 {
        match self {
            Level::Store => 0,
            Level::Fast => 1,
            Level::Normal => 6,
            Level::Best => 9,
        }
    }
}

/// Builds a ZIP file.
pub struct ZipWriter {
    out: Vec<u8>,
    central: Vec<u8>,
    count: usize,
}

/// A chunk of work between progress reports.
const CHUNK: usize = 128 * 1024;

impl ZipWriter {
    pub fn new() -> Self {
        Self {
            out: Vec::new(),
            central: Vec::new(),
            count: 0,
        }
    }

    fn record(
        &mut self,
        name: &str,
        method: u16,
        flags: u16,
        dos: (u16, u16),
        crc: u32,
        packed: u32,
        size: u32,
        dir: bool,
    ) {
        let offset = self.out.len() as u32;
        let name = name.as_bytes();
        // UTF-8 names; the data-descriptor bit of a copied entry is dropped
        // since sizes come first here
        let flags = (flags & !0x0008) | 0x0800;
        let out = &mut self.out;
        put32(out, 0x0403_4b50);
        put16(out, 20);
        put16(out, flags);
        put16(out, method);
        put16(out, dos.0);
        put16(out, dos.1);
        put32(out, crc);
        put32(out, packed);
        put32(out, size);
        put16(out, name.len() as u16);
        put16(out, 0);
        out.extend_from_slice(name);

        let c = &mut self.central;
        put32(c, 0x0201_4b50);
        put16(c, 20); // made by: MS-DOS compatible, version 2.0
        put16(c, 20);
        put16(c, flags);
        put16(c, method);
        put16(c, dos.0);
        put16(c, dos.1);
        put32(c, crc);
        put32(c, packed);
        put32(c, size);
        put16(c, name.len() as u16);
        put16(c, 0);
        put16(c, 0);
        put16(c, 0);
        put16(c, 0);
        put32(c, if dir { 0x10 } else { 0x20 });
        put32(c, offset);
        c.extend_from_slice(name);
        self.count += 1;
    }

    pub fn add_dir(&mut self, name: &str, modified: Stamp) {
        let mut n = String::from(name);
        n.push('/');
        self.record(&n, 0, 0, to_dos(modified), 0, 0, 0, true);
    }

    /// Pack a file. `progress` gets the bytes done so far in this file;
    /// it returns false to stop, and then so does this.
    pub fn add_file(
        &mut self,
        name: &str,
        data: &[u8],
        modified: Stamp,
        level: Level,
        progress: &mut dyn FnMut(u64) -> bool,
    ) -> bool {
        let mut crc = 0;
        for (i, chunk) in data.chunks(CHUNK).enumerate() {
            crc = crc32_update(crc, chunk);
            if i % 8 == 7 && !progress(0) {
                return false;
            }
        }
        let packed = if level == Level::Store || data.len() < 64 {
            None
        } else {
            match deflate(data, level, progress) {
                Some(p) => Some(p),
                None => return false,
            }
        };
        let dos = to_dos(modified);
        match packed {
            Some(p) if p.len() < data.len() => {
                self.record(
                    name,
                    8,
                    0,
                    dos,
                    crc,
                    p.len() as u32,
                    data.len() as u32,
                    false,
                );
                self.out.extend_from_slice(&p);
            }
            // packing would make it bigger: stored as it is
            _ => {
                self.record(
                    name,
                    0,
                    0,
                    dos,
                    crc,
                    data.len() as u32,
                    data.len() as u32,
                    false,
                );
                self.out.extend_from_slice(data);
            }
        }
        true
    }

    /// Copy an entry from another ZIP file without unpacking it, under a
    /// new name.
    pub fn copy(&mut self, from: &Archive, i: usize, name: &str) -> Result<(), String> {
        let e = &from.entries[i];
        if e.dir {
            let mut n = String::from(name);
            n.push('/');
            self.record(&n, 0, 0, e.dos, 0, 0, 0, true);
            return Ok(());
        }
        let raw = from.raw_zip_data(e)?;
        let method = match e.method {
            Method::Store => 0,
            Method::Deflate => 8,
            Method::Other(n) => n,
            Method::Encrypted => u16_at(&from.data, e.offset + 8).unwrap_or(0),
        };
        self.record(
            name,
            method,
            e.flags,
            e.dos,
            e.crc,
            e.packed as u32,
            e.size as u32,
            false,
        );
        self.out.extend_from_slice(raw);
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.out.len() + self.central.len()
    }

    pub fn finish(mut self) -> Vec<u8> {
        let at = self.out.len() as u32;
        let size = self.central.len() as u32;
        let count = self.count.min(0xffff) as u16;
        let mut out = core::mem::take(&mut self.out);
        out.append(&mut self.central);
        put32(&mut out, 0x0605_4b50);
        put16(&mut out, 0);
        put16(&mut out, 0);
        put16(&mut out, count);
        put16(&mut out, count);
        put32(&mut out, size);
        put32(&mut out, at);
        put16(&mut out, 0);
        out
    }
}

/// Raw Deflate, a chunk at a time so `progress` can pause between chunks.
fn deflate(data: &[u8], level: Level, progress: &mut dyn FnMut(u64) -> bool) -> Option<Vec<u8>> {
    let flags = create_comp_flags_from_zip_params(level.zlib(), -15, 0);
    let mut d = CompressorOxide::new(flags);
    let mut out = vec![0u8; data.len() / 2 + 1024];
    let (mut in_pos, mut out_pos) = (0, 0);
    loop {
        let end = (in_pos + CHUNK).min(data.len());
        let flush = if end == data.len() {
            TDEFLFlush::Finish
        } else {
            TDEFLFlush::None
        };
        let (status, used, wrote) =
            compress(&mut d, &data[in_pos..end], &mut out[out_pos..], flush);
        in_pos += used;
        out_pos += wrote;
        match status {
            TDEFLStatus::Done => {
                out.truncate(out_pos);
                return Some(out);
            }
            TDEFLStatus::Okay => {
                if out.len() - out_pos < 64 * 1024 {
                    out.resize(out.len() + out.len() / 2 + 64 * 1024, 0);
                }
                if !progress(in_pos as u64) {
                    return None;
                }
            }
            _ => return None,
        }
    }
}

// ---- working with files on disk -------------------------------------------------

/// Something to put in an archive: a file or folder on disk and the name
/// it gets inside.
pub struct Source {
    pub path: String,
    pub name: String,
}

/// A file or folder found under a source.
struct Item {
    path: String,
    name: String,
    dir: bool,
    size: u64,
    modified: Stamp,
}

fn gather(path: &str, name: &str, out: &mut Vec<Item>) -> Result<(), String> {
    let fail = |e: crate::fs::Error| format!("{}: {}", crate::fs::file_name(path), e.message());
    if crate::fs::is_dir(path) {
        let items = crate::fs::list_all(path).map_err(fail)?;
        out.push(Item {
            path: String::from(path),
            name: String::from(name),
            dir: true,
            size: 0,
            modified: now(),
        });
        for i in items {
            gather(
                &crate::fs::join(path, &i.name),
                &format!("{}/{}", name, i.name),
                out,
            )?;
        }
    } else {
        let parent = crate::fs::parent(path);
        let file = crate::fs::file_name(path);
        let info = crate::fs::list_all(&parent)
            .map_err(fail)?
            .into_iter()
            .find(|i| crate::fs::same_name(&i.name, file))
            .ok_or_else(|| fail(crate::fs::Error::NotFound))?;
        out.push(Item {
            path: String::from(path),
            name: String::from(name),
            dir: false,
            size: info.size as u64,
            modified: info.modified,
        });
    }
    Ok(())
}

fn now() -> Stamp {
    let (y, mo, d) = crate::rtc::date();
    let (h, mi, _) = crate::rtc::time();
    (y, mo, d, h, mi)
}

/// `name` is `under` or inside it.
pub fn within(name: &str, under: &str) -> bool {
    crate::fs::same_name(name, under)
        || name.len() > under.len()
            && name.is_char_boundary(under.len())
            && name.as_bytes()[under.len()] == b'/'
            && crate::fs::same_name(&name[..under.len()], under)
}

/// A new ZIP file: what `archive` holds (it must be a ZIP) with
/// `sources` added. Files with the same name are replaced.
pub fn add(
    archive: &Archive,
    sources: &[Source],
    level: Level,
    progress: Progress,
) -> Result<Vec<u8>, String> {
    let mut items = Vec::new();
    for s in sources {
        gather(&s.path, &clean_path(&s.name), &mut items)?;
    }
    let total: u64 = items.iter().map(|i| i.size).sum::<u64>() + archive.bytes() as u64;
    if total as usize > MAX_BYTES {
        return Err(String::from(
            "That is too much: an archive can hold up to 48 MB here.",
        ));
    }
    let mut zip = ZipWriter::new();
    let mut done = 0u64;
    // what the archive had, less what is being replaced
    for (i, e) in archive.entries.iter().enumerate() {
        if items
            .iter()
            .any(|it| crate::fs::same_name(&it.name, &e.name))
        {
            continue;
        }
        zip.copy(archive, i, &e.name)?;
        done += e.packed;
        if !progress(done, total, &e.name) {
            return Err(String::from("Stopped."));
        }
    }
    // folders the new items are in, so every tool shows them
    let mut dirs: Vec<String> = Vec::new();
    for it in &items {
        let mut at = 0;
        while let Some(p) = it.name[at..].find('/') {
            let dir = &it.name[..at + p];
            at += p + 1;
            let known = archive
                .entries
                .iter()
                .any(|e| e.dir && crate::fs::same_name(&e.name, dir))
                || items
                    .iter()
                    .any(|o| o.dir && crate::fs::same_name(&o.name, dir))
                || dirs.iter().any(|d| crate::fs::same_name(d, dir));
            if !known {
                dirs.push(String::from(dir));
            }
        }
    }
    for d in dirs {
        zip.add_dir(&d, now());
    }
    for it in &items {
        if it.dir {
            zip.add_dir(&it.name, it.modified);
            continue;
        }
        if !progress(done, total, &it.name) {
            return Err(String::from("Stopped."));
        }
        let data =
            crate::fs::read(&it.path).map_err(|e| format!("{}: {}", it.name, e.message()))?;
        let base = done;
        let ok = zip.add_file(&it.name, &data, it.modified, level, &mut |n| {
            progress(base + n, total, &it.name)
        });
        if !ok {
            return Err(String::from("Stopped."));
        }
        done += data.len() as u64;
        if zip.len() > MAX_BYTES {
            return Err(String::from(
                "That is too much: an archive can hold up to 48 MB here.",
            ));
        }
    }
    Ok(zip.finish())
}

/// A new ZIP file without the entries named (folders with all in them).
pub fn remove(archive: &Archive, names: &[String]) -> Result<Vec<u8>, String> {
    let mut zip = ZipWriter::new();
    for (i, e) in archive.entries.iter().enumerate() {
        if names.iter().any(|n| within(&e.name, n)) {
            continue;
        }
        zip.copy(archive, i, &e.name)?;
    }
    Ok(zip.finish())
}

/// A folder and the folders it is in, made where missing.
fn make_dirs(path: &str) -> Result<(), String> {
    if path == "/" || crate::fs::is_dir(path) {
        return Ok(());
    }
    make_dirs(&crate::fs::parent(path))?;
    match crate::fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(_) if crate::fs::is_dir(path) => Ok(()),
        Err(e) => Err(format!("{}: {}", crate::fs::display(path), e.message())),
    }
}

/// Unpack the entries named in `names` (and all inside those that are
/// folders), or everything when it is empty, into the folder `dest`.
/// `strip` is the archive folder they are in, left off their paths.
/// Returns how many files were written.
pub fn extract(
    archive: &Archive,
    names: &[String],
    strip: &str,
    dest: &str,
    progress: Progress,
) -> Result<usize, String> {
    let chosen: Vec<usize> = (0..archive.entries.len())
        .filter(|&i| names.is_empty() || names.iter().any(|n| within(&archive.entries[i].name, n)))
        .collect();
    let total: u64 = chosen.iter().map(|&i| archive.entries[i].size).sum();
    make_dirs(dest)?;
    let mut done = 0u64;
    let mut files = 0;
    for i in chosen {
        let e = &archive.entries[i];
        let rel = if !strip.is_empty() && within(&e.name, strip) && e.name.len() > strip.len() {
            &e.name[strip.len() + 1..]
        } else {
            e.name.as_str()
        };
        let mut path = String::from(dest);
        for part in rel.split('/') {
            path = crate::fs::join(&path, part);
        }
        if !progress(done, total, &e.name) {
            return Err(String::from("Stopped."));
        }
        if e.dir {
            make_dirs(&path)?;
            continue;
        }
        make_dirs(&crate::fs::parent(&path))?;
        let data = archive.read(i)?;
        crate::fs::write(&path, &data).map_err(|err| format!("{}: {}", rel, err.message()))?;
        done += e.size;
        files += 1;
    }
    progress(total, total, "");
    Ok(files)
}
