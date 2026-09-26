//! The C library functions QuickJS needs that are easiest to write in
//! Rust: memory, time, maths and logging. See quickjs/libc/libc.c for
//! the rest.

use alloc::alloc::{alloc, dealloc, Layout};
use core::ffi::c_int;

use crate::interrupts;

/// Every block starts with its size, so `free` and `realloc` know it.
const HEADER: usize = 16;

fn layout(size: usize) -> Layout {
    Layout::from_size_align(size + HEADER, 16).unwrap()
}

#[no_mangle]
pub unsafe extern "C" fn malloc(size: usize) -> *mut u8 {
    let p = alloc(layout(size));
    if p.is_null() {
        return p;
    }
    *(p as *mut usize) = size;
    p.add(HEADER)
}

#[no_mangle]
pub unsafe extern "C" fn calloc(n: usize, size: usize) -> *mut u8 {
    let Some(total) = n.checked_mul(size) else {
        return core::ptr::null_mut();
    };
    let p = malloc(total);
    if !p.is_null() {
        core::ptr::write_bytes(p, 0, total);
    }
    p
}

#[no_mangle]
pub unsafe extern "C" fn free(p: *mut u8) {
    if p.is_null() {
        return;
    }
    let base = p.sub(HEADER);
    dealloc(base, layout(*(base as *const usize)));
}

#[no_mangle]
pub unsafe extern "C" fn malloc_usable_size(p: *mut u8) -> usize {
    if p.is_null() {
        0
    } else {
        *(p.sub(HEADER) as *const usize)
    }
}

#[no_mangle]
pub unsafe extern "C" fn realloc(p: *mut u8, size: usize) -> *mut u8 {
    if p.is_null() {
        return malloc(size);
    }
    let old = malloc_usable_size(p);
    if size <= old && size >= old / 2 {
        return p;
    }
    let q = malloc(size);
    if !q.is_null() {
        core::ptr::copy_nonoverlapping(p, q, old.min(size));
        free(p);
    }
    q
}

#[no_mangle]
pub extern "C" fn abort() -> ! {
    panic!("JavaScript engine called abort()");
}

#[no_mangle]
pub unsafe extern "C" fn __everos_assert(expr: *const u8, file: *const u8, line: c_int) -> ! {
    panic!(
        "QuickJS assertion failed: {} at {}:{}",
        cstr(expr),
        cstr(file),
        line
    );
}

unsafe fn cstr<'a>(p: *const u8) -> &'a str {
    let mut n = 0;
    while *p.add(n) != 0 {
        n += 1;
    }
    core::str::from_utf8(core::slice::from_raw_parts(p, n)).unwrap_or("?")
}

#[no_mangle]
pub unsafe extern "C" fn everos_log(s: *const u8, n: usize) {
    let bytes = core::slice::from_raw_parts(s, n);
    crate::serial::write_str(&alloc::string::String::from_utf8_lossy(bytes));
}

// ---- time ----------------------------------------------------------------------

#[repr(C)]
pub struct TimeVal {
    sec: i64,
    usec: i64,
}

#[repr(C)]
pub struct Tm {
    sec: c_int,
    min: c_int,
    hour: c_int,
    mday: c_int,
    mon: c_int,
    year: c_int,
    wday: c_int,
    yday: c_int,
    isdst: c_int,
    gmtoff: i64,
    zone: *const u8,
}

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> i64 {
    // the RTC gives whole seconds; the timer adds the fraction
    static mut BASE: Option<(i64, u64)> = None;
    let ticks = interrupts::ticks();
    let base = unsafe {
        let b = &mut *core::ptr::addr_of_mut!(BASE);
        *b.get_or_insert_with(|| (crate::rtc::unix_time() * 1000, ticks))
    };
    base.0 + ((ticks - base.1) * 1000 / interrupts::TIMER_HZ) as i64
}

#[no_mangle]
pub unsafe extern "C" fn gettimeofday(tv: *mut TimeVal, _tz: *mut u8) -> c_int {
    let ms = now_ms();
    if !tv.is_null() {
        (*tv).sec = ms / 1000;
        (*tv).usec = (ms % 1000) * 1000;
    }
    0
}

/// The clock is treated as UTC, so local time is UTC.
#[no_mangle]
pub unsafe extern "C" fn localtime_r(t: *const i64, out: *mut Tm) -> *mut Tm {
    let secs = *t;
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let (y, m, d) = crate::rtc::civil_from_days(days);
    let o = &mut *out;
    o.sec = (rem % 60) as c_int;
    o.min = (rem / 60 % 60) as c_int;
    o.hour = (rem / 3600) as c_int;
    o.mday = d as c_int;
    o.mon = m as c_int - 1;
    o.year = y as c_int - 1900;
    o.wday = (days + 4).rem_euclid(7) as c_int;
    o.yday = (days - crate::rtc::days_from_civil(y, 1, 1)) as c_int;
    o.isdst = 0;
    o.gmtoff = 0;
    o.zone = c"UTC".as_ptr() as *const u8;
    out
}

// ---- maths -----------------------------------------------------------------------

/// One-argument maths functions, on raw bit patterns (see libc.c).
#[no_mangle]
pub extern "C" fn everos_math1(op: c_int, x: u64) -> u64 {
    let x = f64::from_bits(x);
    let r = match op {
        0 => libm::floor(x),
        1 => libm::ceil(x),
        2 => libm::trunc(x),
        3 => libm::round(x),
        4 => libm::sin(x),
        5 => libm::cos(x),
        6 => libm::tan(x),
        7 => libm::asin(x),
        8 => libm::acos(x),
        9 => libm::atan(x),
        10 => libm::sinh(x),
        11 => libm::cosh(x),
        12 => libm::tanh(x),
        13 => libm::asinh(x),
        14 => libm::acosh(x),
        15 => libm::atanh(x),
        16 => libm::exp(x),
        17 => libm::expm1(x),
        18 => libm::log(x),
        19 => libm::log1p(x),
        20 => libm::log2(x),
        21 => libm::log10(x),
        22 => libm::cbrt(x),
        23 => libm::rint(x),
        _ => f64::NAN,
    };
    r.to_bits()
}

#[no_mangle]
pub extern "C" fn everos_math2(op: c_int, x: u64, y: u64) -> u64 {
    let (x, y) = (f64::from_bits(x), f64::from_bits(y));
    let r = match op {
        0 => libm::pow(x, y),
        1 => libm::atan2(x, y),
        2 => libm::fmod(x, y),
        3 => libm::hypot(x, y),
        4 => libm::fmin(x, y),
        5 => libm::fmax(x, y),
        _ => f64::NAN,
    };
    r.to_bits()
}
