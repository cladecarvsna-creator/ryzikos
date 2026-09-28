//! MP4 and QuickTime files (.mp4, .m4v, .mov, .3gp): where the pictures
//! of the video track are and when each is shown. Phones, cameras,
//! Telegram and most sites save videos this way, nearly always with
//! H.264 pictures, which `gui/video.rs` decodes.
//!
//! Only the index (the `moov` box, or `moof` boxes in fragmented files)
//! is read here; the pictures stay in the file until they are due.

use alloc::string::String;
use alloc::vec::Vec;

/// What the pictures are coded with.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Codec {
    /// H.264 (AVC).
    Avc,
    /// Motion JPEG: every picture a JPEG.
    Jpeg,
    /// Anything else, by its four letters (hvc1, av01, vp09, mp4v...).
    Other([u8; 4]),
}

#[derive(Clone, Copy, Debug)]
pub struct Sample {
    pub offset: u64,
    pub size: u32,
    /// When it is shown, in microseconds from the start.
    pub pts_us: i64,
    /// A key picture: decoding can start here.
    pub key: bool,
}

pub struct Track {
    pub codec: Codec,
    pub width: u32,
    pub height: u32,
    /// Pictures in the order they are decoded (not always shown in).
    pub samples: Vec<Sample>,
    /// Bytes in front of each H.264 unit giving its length (1, 2 or 4).
    pub nal_len: usize,
    /// The H.264 parameter sets (SPS and PPS) with start codes, to go in
    /// front of the first picture decoded.
    pub params: Vec<u8>,
    pub duration_us: i64,
    /// Whether any picture is shown later than it is decoded (B-frames).
    pub reordered: bool,
    /// Whether the file has a sound track too.
    pub has_sound: bool,
}

/// Reads `len` bytes at an offset, or fewer at the end of the file.
pub type ReadAt<'a> = &'a mut dyn FnMut(u64, usize) -> Option<Vec<u8>>;

/// The biggest index read; a two-hour film's is a few MB.
const MAX_MOOV: u64 = 48 * 1024 * 1024;

fn be16(d: &[u8], at: usize) -> u32 {
    d.get(at..at + 2).map_or(0, |b| u16::from_be_bytes([b[0], b[1]]) as u32)
}
fn be32(d: &[u8], at: usize) -> u32 {
    d.get(at..at + 4).map_or(0, |b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}
fn be64(d: &[u8], at: usize) -> u64 {
    (be32(d, at) as u64) << 32 | be32(d, at + 4) as u64
}

/// The boxes in `d`: (type, body).
fn boxes(d: &[u8]) -> Vec<([u8; 4], &[u8])> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while at + 8 <= d.len() {
        let mut size = be32(d, at) as u64;
        let kind = [d[at + 4], d[at + 5], d[at + 6], d[at + 7]];
        let mut head = 8;
        if size == 1 {
            size = be64(d, at + 8);
            head = 16;
        } else if size == 0 {
            size = (d.len() - at) as u64;
        }
        if size < head as u64 || at as u64 + size > d.len() as u64 {
            break;
        }
        out.push((kind, &d[at + head..at + size as usize]));
        at += size as usize;
    }
    out
}

fn child<'a>(d: &'a [u8], kind: &[u8; 4]) -> Option<&'a [u8]> {
    boxes(d).into_iter().find(|(k, _)| k == kind).map(|(_, b)| b)
}

/// Find the top-level boxes of the file without reading their bodies:
/// (type, where the body starts, body size).
fn top_level(read: ReadAt, size: u64) -> Vec<([u8; 4], u64, u64)> {
    let mut out = Vec::new();
    let mut at = 0u64;
    while at + 8 <= size && out.len() < 100_000 {
        let Some(h) = read(at, 16) else { break };
        if h.len() < 8 {
            break;
        }
        let mut len = be32(&h, 0) as u64;
        let kind = [h[4], h[5], h[6], h[7]];
        let mut head = 8;
        if len == 1 {
            if h.len() < 16 {
                break;
            }
            len = be64(&h, 8);
            head = 16;
        } else if len == 0 {
            len = size - at;
        }
        if len < head {
            break;
        }
        out.push((kind, at + head, len.min(size - at) - head));
        at += len;
    }
    out
}

/// Whether the first bytes look like an MP4 or QuickTime file.
pub fn looks_like(head: &[u8]) -> bool {
    matches!(head.get(4..8), Some(b"ftyp" | b"moov" | b"mdat" | b"wide" | b"free" | b"skip"))
}

/// Read the video track of an MP4 file of `size` bytes.
pub fn parse(read: ReadAt, size: u64) -> Result<Track, &'static str> {
    let top = top_level(read, size);
    if !top.iter().any(|(k, ..)| k == b"moov") {
        return Err("This file has no video index (it may not be an MP4 file, or it was not saved to the end).");
    }
    let mut track = None;
    let mut sound = false;
    let mut trex_defaults = (0u32, 0u32, 0u32);
    for &(kind, at, len) in &top {
        if &kind != b"moov" {
            continue;
        }
        if len > MAX_MOOV {
            return Err("This video's index is too big to read.");
        }
        let moov = read(at, len as usize).ok_or("The file could not be read.")?;
        for (k, body) in boxes(&moov) {
            match &k {
                b"trak" => match handler(body) {
                    Some(h) if &h == b"vide" && track.is_none() => track = Some(video_track(body)?),
                    Some(h) if &h == b"soun" => sound = true,
                    _ => {}
                },
                b"mvex" => {
                    if let Some(t) = child(body, b"trex") {
                        // version/flags, track id, description index
                        trex_defaults = (be32(t, 12), be32(t, 16), be32(t, 20));
                    }
                }
                _ => {}
            }
        }
    }
    let (mut t, id, timescale) = track.ok_or("This file has no video track (it may be sound only).")?;
    t.has_sound = sound;
    // fragmented files keep the rest of the index in moof boxes
    let mut dts = t.samples.last().map_or(0, |_| t.duration_us * timescale as i64 / 1_000_000);
    for &(kind, at, len) in &top {
        if &kind != b"moof" || len > MAX_MOOV {
            continue;
        }
        let Some(moof) = read(at, len as usize) else { break };
        fragment(&moof, at - 8, id, timescale, trex_defaults, &mut dts, &mut t.samples);
    }
    if t.samples.is_empty() {
        return Err("This video has no pictures.");
    }
    // start at 0 however the file counts
    let first = t.samples.iter().map(|s| s.pts_us).min().unwrap_or(0);
    let mut last = 0;
    for s in &mut t.samples {
        s.pts_us -= first;
        last = last.max(s.pts_us);
    }
    t.reordered = t.samples.windows(2).any(|w| w[1].pts_us < w[0].pts_us);
    let n = t.samples.len() as i64;
    let step = if n > 1 { last / (n - 1) } else { 40_000 };
    t.duration_us = last + step.max(1);
    if !t.samples.iter().any(|s| s.key) {
        t.samples[0].key = true;
    }
    Ok(t)
}

fn handler(trak: &[u8]) -> Option<[u8; 4]> {
    let mdia = child(trak, b"mdia")?;
    let hdlr = child(mdia, b"hdlr")?;
    hdlr.get(8..12).map(|h| [h[0], h[1], h[2], h[3]])
}

/// The sample table of a video track: (track, track id, timescale).
fn video_track(trak: &[u8]) -> Result<(Track, u32, u32), &'static str> {
    let bad = "This video's index is damaged.";
    let tkhd = child(trak, b"tkhd").ok_or(bad)?;
    let id = if tkhd.first() == Some(&1) { be32(tkhd, 20) } else { be32(tkhd, 12) };
    let mdia = child(trak, b"mdia").ok_or(bad)?;
    let mdhd = child(mdia, b"mdhd").ok_or(bad)?;
    let timescale = if mdhd.first() == Some(&1) { be32(mdhd, 20) } else { be32(mdhd, 12) }.max(1);
    let stbl = child(mdia, b"minf").and_then(|m| child(m, b"stbl")).ok_or(bad)?;
    let stsd = child(stbl, b"stsd").ok_or(bad)?;
    // version/flags, count, then the first sample entry
    let entry = boxes(stsd.get(8..).unwrap_or(&[]));
    let (fourcc, desc) = entry.first().copied().ok_or(bad)?;
    let mut t = Track {
        codec: match &fourcc {
            b"avc1" | b"avc3" => Codec::Avc,
            b"jpeg" | b"mjpa" | b"mjpg" | b"MJPG" => Codec::Jpeg,
            _ => Codec::Other(fourcc),
        },
        width: be16(desc, 24),
        height: be16(desc, 26),
        samples: Vec::new(),
        nal_len: 4,
        params: Vec::new(),
        duration_us: 0,
        reordered: false,
        has_sound: false,
    };
    if t.codec == Codec::Avc {
        // the visual sample entry is 78 bytes, then its boxes
        if let Some(avcc) = desc.get(78..).and_then(|d| child(d, b"avcC")) {
            read_avcc(avcc, &mut t);
        }
    }

    // sizes
    let mut sizes = Vec::new();
    if let Some(stsz) = child(stbl, b"stsz") {
        let fixed = be32(stsz, 4);
        let n = be32(stsz, 8) as usize;
        for i in 0..n.min(10_000_000) {
            sizes.push(if fixed != 0 { fixed } else { be32(stsz, 12 + i * 4) });
        }
    } else if let Some(stz2) = child(stbl, b"stz2") {
        let bits = stz2.get(7).copied().unwrap_or(0);
        let n = be32(stz2, 8) as usize;
        for i in 0..n.min(10_000_000) {
            sizes.push(match bits {
                4 => {
                    let b = stz2.get(12 + i / 2).copied().unwrap_or(0);
                    (if i % 2 == 0 { b >> 4 } else { b & 15 }) as u32
                }
                8 => stz2.get(12 + i).copied().unwrap_or(0) as u32,
                _ => be16(stz2, 12 + i * 2),
            });
        }
    }
    // chunk offsets
    let mut chunks = Vec::new();
    if let Some(stco) = child(stbl, b"stco") {
        for i in 0..(be32(stco, 4) as usize).min(10_000_000) {
            chunks.push(be32(stco, 8 + i * 4) as u64);
        }
    } else if let Some(co64) = child(stbl, b"co64") {
        for i in 0..(be32(co64, 4) as usize).min(10_000_000) {
            chunks.push(be64(co64, 8 + i * 8));
        }
    }
    // samples per chunk: (first chunk, count) runs
    let mut stsc = Vec::new();
    if let Some(b) = child(stbl, b"stsc") {
        for i in 0..(be32(b, 4) as usize).min(10_000_000) {
            stsc.push((be32(b, 8 + i * 12) as usize, be32(b, 12 + i * 12) as usize));
        }
    }
    let mut offsets = Vec::with_capacity(sizes.len());
    let mut s = 0usize;
    for (ci, &base) in chunks.iter().enumerate() {
        let chunk_no = ci + 1;
        let per = stsc
            .iter()
            .rev()
            .find(|&&(first, _)| first <= chunk_no)
            .map_or(1, |&(_, n)| n);
        let mut at = base;
        for _ in 0..per {
            if s >= sizes.len() {
                break;
            }
            offsets.push(at);
            at += sizes[s] as u64;
            s += 1;
        }
    }
    // decode times
    let mut dts = Vec::with_capacity(sizes.len());
    let mut now = 0u64;
    if let Some(stts) = child(stbl, b"stts") {
        for i in 0..be32(stts, 4) as usize {
            let (count, delta) = (be32(stts, 8 + i * 8), be32(stts, 12 + i * 8));
            for _ in 0..count {
                if dts.len() >= sizes.len() {
                    break;
                }
                dts.push(now);
                now += delta as u64;
            }
        }
    }
    // shown later than decoded by
    let mut ctts = Vec::new();
    if let Some(b) = child(stbl, b"ctts") {
        let signed = b.first() == Some(&1);
        for i in 0..be32(b, 4) as usize {
            let count = be32(b, 8 + i * 8);
            let raw = be32(b, 12 + i * 8);
            // version 0 is meant to be unsigned, but some writers put
            // negative offsets there too
            let off = if signed || raw > 0x8000_0000 { raw as i32 as i64 } else { raw as i64 };
            for _ in 0..count {
                if ctts.len() >= sizes.len() {
                    break;
                }
                ctts.push(off);
            }
        }
    }
    let stss = child(stbl, b"stss");
    let mut keys = Vec::new();
    if let Some(b) = stss {
        for i in 0..be32(b, 4) as usize {
            keys.push(be32(b, 8 + i * 4) as usize);
        }
    }
    let n = sizes.len().min(offsets.len());
    t.samples.reserve(n);
    for i in 0..n {
        let d = dts.get(i).copied().unwrap_or(now) as i64 + ctts.get(i).copied().unwrap_or(0);
        t.samples.push(Sample {
            offset: offsets[i],
            size: sizes[i],
            pts_us: d * 1_000_000 / timescale as i64,
            key: stss.is_none() || keys.binary_search(&(i + 1)).is_ok(),
        });
    }
    t.duration_us = now as i64 * 1_000_000 / timescale as i64;
    Ok((t, id, timescale))
}

/// The H.264 decoder configuration: the length size and the parameter
/// sets, turned into start-code form.
fn read_avcc(avcc: &[u8], t: &mut Track) {
    if avcc.len() < 7 {
        return;
    }
    t.nal_len = (avcc[4] & 3) as usize + 1;
    let mut at = 6;
    let take = |at: &mut usize, count: usize, out: &mut Vec<u8>| {
        for _ in 0..count {
            let len = be16(avcc, *at) as usize;
            if let Some(nal) = avcc.get(*at + 2..*at + 2 + len) {
                out.extend_from_slice(&[0, 0, 0, 1]);
                out.extend_from_slice(nal);
            }
            *at += 2 + len;
        }
    };
    let sps = (avcc[5] & 31) as usize;
    take(&mut at, sps, &mut t.params);
    let pps = avcc.get(at).copied().unwrap_or(0) as usize;
    at += 1;
    take(&mut at, pps, &mut t.params);
}

/// Add the samples of one `moof` for our track. `start` is where the
/// moof box begins in the file.
fn fragment(
    moof: &[u8],
    start: u64,
    id: u32,
    timescale: u32,
    trex: (u32, u32, u32),
    dts: &mut i64,
    out: &mut Vec<Sample>,
) {
    for (k, traf) in boxes(moof) {
        if &k != b"traf" {
            continue;
        }
        let Some(tfhd) = child(traf, b"tfhd") else { continue };
        if be32(tfhd, 4) != id {
            continue;
        }
        let flags = be32(tfhd, 0) & 0xff_ffff;
        let mut at = 8;
        let mut base = start;
        if flags & 1 != 0 {
            base = be64(tfhd, at);
            at += 8;
        }
        if flags & 2 != 0 {
            at += 4;
        }
        let (mut def_dur, mut def_size, mut def_flags) = trex;
        if flags & 8 != 0 {
            def_dur = be32(tfhd, at);
            at += 4;
        }
        if flags & 0x10 != 0 {
            def_size = be32(tfhd, at);
            at += 4;
        }
        if flags & 0x20 != 0 {
            def_flags = be32(tfhd, at);
        }
        if let Some(tfdt) = child(traf, b"tfdt") {
            *dts = if tfdt.first() == Some(&1) { be64(tfdt, 4) as i64 } else { be32(tfdt, 4) as i64 };
        }
        let mut next_offset = None;
        for (k2, trun) in boxes(traf) {
            if &k2 != b"trun" {
                continue;
            }
            let version = trun.first().copied().unwrap_or(0);
            let tf = be32(trun, 0) & 0xff_ffff;
            let count = be32(trun, 4) as usize;
            let mut p = 8;
            let mut offset = next_offset.unwrap_or(base);
            if tf & 1 != 0 {
                offset = (base as i64 + be32(trun, p) as i32 as i64) as u64;
                p += 4;
            }
            let mut first_flags = None;
            if tf & 4 != 0 {
                first_flags = Some(be32(trun, p));
                p += 4;
            }
            for i in 0..count.min(1_000_000) {
                let mut dur = def_dur;
                let mut size = def_size;
                let mut sflags = if i == 0 { first_flags.unwrap_or(def_flags) } else { def_flags };
                let mut cto = 0i64;
                if tf & 0x100 != 0 {
                    dur = be32(trun, p);
                    p += 4;
                }
                if tf & 0x200 != 0 {
                    size = be32(trun, p);
                    p += 4;
                }
                if tf & 0x400 != 0 {
                    sflags = be32(trun, p);
                    p += 4;
                }
                if tf & 0x800 != 0 {
                    let raw = be32(trun, p);
                    cto = if version == 1 { raw as i32 as i64 } else { raw as i64 };
                    p += 4;
                }
                out.push(Sample {
                    offset,
                    size,
                    pts_us: (*dts + cto) * 1_000_000 / timescale as i64,
                    // sample_is_non_sync_sample
                    key: sflags & 0x1_0000 == 0,
                });
                offset += size as u64;
                *dts += dur as i64;
            }
            next_offset = Some(offset);
        }
    }
}

/// A short name for a codec, for messages.
pub fn codec_name(c: Codec) -> String {
    match c {
        Codec::Avc => String::from("H.264"),
        Codec::Jpeg => String::from("Motion JPEG"),
        Codec::Other(f) => match &f {
            b"hvc1" | b"hev1" => String::from("H.265 (HEVC)"),
            b"av01" => String::from("AV1"),
            b"vp09" => String::from("VP9"),
            b"vp08" => String::from("VP8"),
            b"mp4v" => String::from("MPEG-4 Part 2"),
            b"s263" | b"h263" => String::from("H.263"),
            _ => f.iter().map(|&b| if b.is_ascii_graphic() { b as char } else { '?' }).collect(),
        },
    }
}

/// Turn one H.264 picture from the file (units with length prefixes)
/// into the start-code form the decoder reads, with `params` in front.
pub fn to_annex_b(sample: &[u8], nal_len: usize, params: &[u8], out: &mut Vec<u8>) {
    out.clear();
    out.extend_from_slice(params);
    let mut at = 0;
    while at + nal_len <= sample.len() {
        let mut len = 0usize;
        for &b in &sample[at..at + nal_len] {
            len = len << 8 | b as usize;
        }
        at += nal_len;
        let end = (at + len).min(sample.len());
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&sample[at..end]);
        at = end;
    }
}
