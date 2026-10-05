//! The time shown everywhere: the computer's clock (the CMOS RTC) plus a
//! correction, in a time zone. Settings > Time & language sets them.
//!
//! PCs keep local time in the RTC (Windows does), QEMU keeps UTC unless
//! started with `-rtc base=localtime`, so the RTC alone can't say what
//! time it is. With "Set time automatically" on, the first time the
//! network is up we ask a web server for the real (UTC) time and keep
//! how far the RTC is from it. The first such check also guesses the
//! time zone from that difference, so a PC whose clock was right before
//! still shows the same time. Setting the time by hand keeps a
//! correction too; the RTC itself is never written, so other systems on
//! the same PC keep their clock.
//!
//! Everything here is plain atomics, so the crash screen can read the
//! time without the heap.

use alloc::format;
use alloc::string::String;
use core::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, Ordering};

use crate::{fs, rtc, serial};

/// Where the settings are kept, on the system disk.
const FILE: &str = "/boot/clock.txt";

/// Seconds to add to the RTC (read as if it were UTC) for UTC.
static ADJUST: AtomicI64 = AtomicI64::new(0);
/// Minutes east of UTC.
static ZONE: AtomicI32 = AtomicI32::new(0);
/// The time zone was picked (or guessed), not just the default.
static ZONE_SET: AtomicBool = AtomicBool::new(false);
/// Take the time from the internet.
static AUTO: AtomicBool = AtomicBool::new(true);
/// The internet time was read since RyzikOS started.
static SYNCED: AtomicBool = AtomicBool::new(false);

/// Time zones to pick from: minutes east of UTC and places in them.
pub const ZONES: [(i32, &str); 37] = [
    (-720, "Baker Island"),
    (-660, "Pago Pago"),
    (-600, "Honolulu"),
    (-540, "Anchorage"),
    (-480, "Los Angeles, Vancouver"),
    (-420, "Denver, Phoenix"),
    (-360, "Chicago, Mexico City"),
    (-300, "New York, Toronto"),
    (-240, "Halifax, Caracas"),
    (-210, "Newfoundland"),
    (-180, "Buenos Aires, Sao Paulo"),
    (-120, "South Georgia"),
    (-60, "Azores, Cape Verde"),
    (0, "London, Lisbon, UTC"),
    (60, "Berlin, Paris, Warsaw"),
    (120, "Kyiv, Athens, Kaliningrad"),
    (180, "Moscow, Istanbul, Minsk"),
    (210, "Tehran"),
    (240, "Dubai, Baku, Samara"),
    (270, "Kabul"),
    (300, "Almaty, Astana, Tashkent"),
    (330, "India"),
    (345, "Kathmandu"),
    (360, "Bishkek, Omsk, Dhaka"),
    (390, "Yangon"),
    (420, "Novosibirsk, Bangkok, Jakarta"),
    (480, "Beijing, Irkutsk, Singapore"),
    (540, "Tokyo, Seoul, Yakutsk"),
    (570, "Adelaide, Darwin"),
    (600, "Sydney, Vladivostok"),
    (630, "Lord Howe Island"),
    (660, "Magadan, Solomon Islands"),
    (720, "Auckland, Kamchatka"),
    (765, "Chatham Islands"),
    (780, "Tonga, Samoa"),
    (840, "Kiritimati"),
    (-570, "Marquesas"),
];

/// Read the settings, once the disk is mounted.
pub fn init() {
    let Ok(data) = fs::read(FILE) else {
        return;
    };
    let text = String::from_utf8_lossy(&data);
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "adjust" => {
                if let Ok(v) = value.parse() {
                    ADJUST.store(v, Ordering::Relaxed);
                }
            }
            "zone" => {
                if let Ok(v) = value.parse::<i32>() {
                    ZONE.store(v.clamp(-720, 840), Ordering::Relaxed);
                    ZONE_SET.store(true, Ordering::Relaxed);
                }
            }
            "auto" => AUTO.store(value == "1", Ordering::Relaxed),
            _ => {}
        }
    }
    serial::write_str(&format!(
        "clock: zone {}, automatic {}\n",
        zone_name(zone()),
        if auto() { "on" } else { "off" }
    ));
}

fn save() {
    let mut text = format!(
        "auto: {}\nadjust: {}\n",
        auto() as u8,
        ADJUST.load(Ordering::Relaxed)
    );
    if ZONE_SET.load(Ordering::Relaxed) {
        text.push_str(&format!("zone: {}\n", zone()));
    }
    if !fs::is_dir("/boot") {
        let _ = fs::create_dir("/boot");
    }
    if let Err(e) = fs::write(FILE, text.as_bytes()) {
        serial::write_str(&format!("clock: could not save: {}\n", e.message()));
    }
}

/// Seconds since the Unix epoch, UTC, as well as RyzikOS knows it.
pub fn utc() -> i64 {
    rtc::unix_time() + ADJUST.load(Ordering::Relaxed)
}

/// Seconds since the Unix epoch in the local time zone.
pub fn local() -> i64 {
    utc() + zone() as i64 * 60
}

/// Minutes east of UTC.
pub fn zone() -> i32 {
    ZONE.load(Ordering::Relaxed)
}

pub fn auto() -> bool {
    AUTO.load(Ordering::Relaxed)
}

pub fn synced() -> bool {
    SYNCED.load(Ordering::Relaxed)
}

/// The local date and time: (year, month, day) and (hour, minute, second).
pub fn now() -> ((u16, u8, u8), (u8, u8, u8)) {
    split(local())
}

/// A moment in seconds as a date and a time of day.
pub fn split(t: i64) -> ((u16, u8, u8), (u8, u8, u8)) {
    let (y, m, d) = rtc::civil_from_days(t.div_euclid(86400));
    let s = t.rem_euclid(86400);
    (
        (y.clamp(0, 9999) as u16, m as u8, d as u8),
        ((s / 3600) as u8, (s / 60 % 60) as u8, (s % 60) as u8),
    )
}

/// The local date as (year, month 1-12, day 1-31).
pub fn date() -> (u16, u8, u8) {
    now().0
}

/// "UTC+05:00".
pub fn offset_text(minutes: i32) -> String {
    let sign = if minutes < 0 { '-' } else { '+' };
    let m = minutes.abs();
    format!("UTC{}{:02}:{:02}", sign, m / 60, m % 60)
}

/// "UTC+05:00  Almaty, Astana, Tashkent".
pub fn zone_name(minutes: i32) -> String {
    match ZONES.iter().find(|z| z.0 == minutes) {
        Some((_, places)) => format!("{}  {}", offset_text(minutes), places),
        None => offset_text(minutes),
    }
}

/// The zones in order, west to east.
fn sorted_zones() -> alloc::vec::Vec<i32> {
    let mut v: alloc::vec::Vec<i32> = ZONES.iter().map(|z| z.0).collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// Move to the next zone east (`step` 1) or west (-1).
pub fn step_zone(step: i32) {
    let zones = sorted_zones();
    let now = zone();
    let next = if step > 0 {
        zones.iter().copied().find(|&z| z > now)
    } else {
        zones.iter().rev().copied().find(|&z| z < now)
    };
    if let Some(z) = next {
        set_zone(z);
    }
}

pub fn set_zone(minutes: i32) {
    ZONE.store(minutes.clamp(-720, 840), Ordering::Relaxed);
    ZONE_SET.store(true, Ordering::Relaxed);
    serial::write_str(&format!("clock: time zone {}\n", offset_text(minutes)));
    save();
}

pub fn set_auto(on: bool) {
    AUTO.store(on, Ordering::Relaxed);
    if on {
        SYNCED.store(false, Ordering::Relaxed);
    }
    save();
}

/// The user typed the local date and time.
pub fn set_local(t: i64) {
    let utc = t - zone() as i64 * 60;
    ADJUST.store(utc - rtc::unix_time(), Ordering::Relaxed);
    AUTO.store(false, Ordering::Relaxed);
    if !ZONE_SET.load(Ordering::Relaxed) {
        ZONE_SET.store(true, Ordering::Relaxed);
    }
    let ((y, mo, d), (h, mi, _)) = split(t);
    serial::write_str(&format!(
        "clock: set by hand to {:02}.{:02}.{} {:02}:{:02}\n",
        d, mo, y, h, mi
    ));
    save();
}

/// The real time (UTC) is `utc`: keep the RTC's difference from it.
pub fn set_utc(utc: i64) {
    let raw = rtc::unix_time();
    if !ZONE_SET.load(Ordering::Relaxed) {
        // the RTC holds local time on most PCs: its distance from UTC,
        // to the quarter hour, is the time zone
        let diff = raw - utc;
        let quarters = (diff + 450).div_euclid(900);
        let minutes = (quarters * 15) as i32;
        if (-720..=840).contains(&minutes) {
            ZONE.store(minutes, Ordering::Relaxed);
            ZONE_SET.store(true, Ordering::Relaxed);
        }
    }
    ADJUST.store(utc - raw, Ordering::Relaxed);
    SYNCED.store(true, Ordering::Relaxed);
    serial::write_str(&format!(
        "clock: synced from the internet, the computer's clock is {} s off UTC, zone {}\n",
        raw - utc,
        offset_text(zone())
    ));
    save();
}

/// Ask web servers for the time (the Date header of their answer).
/// Blocks on the network, so it runs on a fiber.
pub fn fetch_utc() -> Option<i64> {
    // tests build with RYZIKOS_TIME_URL pointing at a local server
    let test = option_env!("RYZIKOS_TIME_URL");
    for url in [
        test.unwrap_or(""),
        "http://www.google.com/generate_204",
        "https://api.github.com/zen",
    ] {
        let Some(u) = crate::web::url::Url::parse(url) else {
            continue;
        };
        if let Ok(Some(utc)) = crate::web::http::get(&u, None).map(|r| r.date) {
            return Some(utc);
        }
    }
    None
}

/// Read "DD.MM.YYYY HH:MM" (or with seconds) as a moment in seconds.
pub fn parse(text: &str) -> Option<i64> {
    let mut parts = text.split_whitespace();
    let date = parts.next()?;
    let time = parts.next().unwrap_or("00:00");
    if parts.next().is_some() {
        return None;
    }
    let d: alloc::vec::Vec<&str> = date.split(['.', '/', '-']).collect();
    let t: alloc::vec::Vec<&str> = time.split(':').collect();
    if d.len() != 3 || !(2..=3).contains(&t.len()) {
        return None;
    }
    let (day, month, year): (i64, i64, i64) =
        (d[0].parse().ok()?, d[1].parse().ok()?, d[2].parse().ok()?);
    let (h, mi): (i64, i64) = (t[0].parse().ok()?, t[1].parse().ok()?);
    let s: i64 = match t.get(2) {
        Some(s) => s.parse().ok()?,
        None => 0,
    };
    let days_in = |y: i64, m: i64| match m {
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if !(2000..=2099).contains(&year)
        || !(1..=12).contains(&month)
        || day < 1
        || day > days_in(year, month)
        || !(0..24).contains(&h)
        || !(0..60).contains(&mi)
        || !(0..60).contains(&s)
    {
        return None;
    }
    Some(rtc::days_from_civil(year, month, day) * 86400 + h * 3600 + mi * 60 + s)
}
