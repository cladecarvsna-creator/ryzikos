//! JavaScript, through the QuickJS engine (kernel/quickjs, compiled by
//! build.rs). Scripts call into the kernel with `__native(op, ...args)`,
//! which lands in the current [`Host`].

mod libc;

use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::{c_char, c_int, c_void};
use core::ptr::null_mut;

pub use libc::now_ms;

use crate::interrupts;

extern "C" {
    fn ejs_new(memory_limit: usize, stack_size: usize) -> *mut c_void;
    fn ejs_free(ctx: *mut c_void);
    fn ejs_eval(
        ctx: *mut c_void,
        src: *const u8,
        len: usize,
        filename: *const c_char,
        module: c_int,
        out: *mut *mut u8,
        out_len: *mut usize,
    ) -> c_int;
}

/// What a native call returns to JavaScript.
pub enum Value {
    Undefined,
    Null,
    Bool(bool),
    Int(i64),
    Str(String),
    /// JSON text, parsed on the JavaScript side.
    Json(String),
    /// Thrown as a TypeError.
    Error(String),
}

/// The embedder: answers `__native` calls from scripts.
pub trait Host {
    fn call(&mut self, op: &str, args: &[Option<&str>]) -> Value;
    /// The full address of an imported module.
    fn resolve_module(&mut self, _base: &str, _name: &str) -> Option<String> {
        None
    }
    /// The source of a module, by its full address.
    fn load_module(&mut self, _name: &str) -> Option<String> {
        None
    }
}

/// The host of the script that is running now.
static mut HOST: Option<*mut dyn Host> = None;
/// Scripts are stopped when the timer passes this tick.
static mut DEADLINE: u64 = u64::MAX;

pub struct Context {
    ctx: *mut c_void,
}

/// How long one script, event handler or timer may run.
pub const SCRIPT_MS: u64 = 4000;

impl Context {
    pub fn new() -> Option<Context> {
        let ctx = unsafe { ejs_new(96 * 1024 * 1024, 600 * 1024) };
        if ctx.is_null() {
            None
        } else {
            Some(Context { ctx })
        }
    }

    /// Run `source` as a global script with `host` answering native calls.
    /// Returns the completion value as text, or the error with its stack.
    pub fn eval(
        &mut self,
        host: &mut dyn Host,
        source: &str,
        filename: &str,
    ) -> Result<String, String> {
        self.eval_as(host, source, filename, false)
    }

    /// Run `source` as an ES module.
    pub fn eval_module(
        &mut self,
        host: &mut dyn Host,
        source: &str,
        filename: &str,
    ) -> Result<String, String> {
        self.eval_as(host, source, filename, true)
    }

    fn eval_as(
        &mut self,
        host: &mut dyn Host,
        source: &str,
        filename: &str,
        module: bool,
    ) -> Result<String, String> {
        let mut name = Vec::with_capacity(filename.len() + 1);
        name.extend_from_slice(filename.as_bytes());
        name.retain(|&b| b != 0);
        name.push(0);
        let mut out: *mut u8 = null_mut();
        let mut out_len = 0usize;
        let r = self.with_host(host, |ctx| unsafe {
            ejs_eval(
                ctx,
                source.as_ptr(),
                source.len(),
                name.as_ptr() as *const c_char,
                module as c_int,
                &mut out,
                &mut out_len,
            )
        });
        let text = if out.is_null() {
            String::new()
        } else {
            let s = String::from_utf8_lossy(unsafe { core::slice::from_raw_parts(out, out_len) })
                .into_owned();
            unsafe { libc::free(out) };
            s
        };
        if r == 0 {
            Ok(text)
        } else {
            Err(text)
        }
    }

    fn with_host<R>(&mut self, host: &mut dyn Host, f: impl FnOnce(*mut c_void) -> R) -> R {
        // Safety: the host outlives the call, and HOST is cleared (or
        // restored, for nested calls) before it returns.
        let host: *mut (dyn Host + '_) = host;
        let host: *mut (dyn Host + 'static) = unsafe { core::mem::transmute(host) };
        unsafe {
            let saved = (HOST, DEADLINE);
            HOST = Some(host);
            if saved.0.is_none() {
                DEADLINE = interrupts::ticks() + SCRIPT_MS * interrupts::TIMER_HZ / 1000;
            }
            let r = f(self.ctx);
            HOST = saved.0;
            DEADLINE = saved.1;
            r
        }
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        unsafe { ejs_free(self.ctx) };
    }
}

#[no_mangle]
unsafe extern "C" fn everos_js_native(
    op: *const u8,
    op_len: usize,
    argc: c_int,
    argv: *const *const u8,
    lens: *const usize,
    out: *mut *mut u8,
    out_len: *mut usize,
) -> c_int {
    let text = |p: *const u8, n: usize| -> Option<&str> {
        if p.is_null() {
            None
        } else {
            Some(core::str::from_utf8(core::slice::from_raw_parts(p, n)).unwrap_or(""))
        }
    };
    let op = text(op, op_len).unwrap_or("");
    let mut args = Vec::with_capacity(argc as usize);
    for i in 0..argc as usize {
        args.push(text(*argv.add(i), *lens.add(i)));
    }
    let Some(host) = HOST else {
        return 0;
    };
    let value = (*host).call(op, &args);
    let (kind, s) = match value {
        Value::Undefined => (0, None),
        Value::Str(s) => (1, Some(s)),
        Value::Int(v) => (2, Some(alloc::format!("{}", v))),
        Value::Json(s) => (3, Some(s)),
        Value::Bool(true) => (4, None),
        Value::Bool(false) => (5, None),
        Value::Null => (6, None),
        Value::Error(s) => (7, Some(s)),
    };
    if let Some(s) = s {
        let p = libc::malloc(s.len().max(1));
        if !p.is_null() {
            core::ptr::copy_nonoverlapping(s.as_ptr(), p, s.len());
            *out = p;
            *out_len = s.len();
        }
    }
    kind
}

/// A malloc'ed, NUL-terminated copy of `s` for C.
unsafe fn c_string(s: &str) -> *mut u8 {
    let p = libc::malloc(s.len() + 1);
    if !p.is_null() {
        core::ptr::copy_nonoverlapping(s.as_ptr(), p, s.len());
        *p.add(s.len()) = 0;
    }
    p
}

unsafe fn from_c<'a>(p: *const u8) -> &'a str {
    if p.is_null() {
        return "";
    }
    let mut n = 0;
    while *p.add(n) != 0 {
        n += 1;
    }
    core::str::from_utf8(core::slice::from_raw_parts(p, n)).unwrap_or("")
}

#[no_mangle]
unsafe extern "C" fn everos_js_resolve_module(base: *const u8, name: *const u8) -> *mut u8 {
    let Some(host) = HOST else {
        return null_mut();
    };
    match (*host).resolve_module(from_c(base), from_c(name)) {
        Some(r) => c_string(&r),
        None => null_mut(),
    }
}

#[no_mangle]
unsafe extern "C" fn everos_js_load_module(name: *const u8, len: *mut usize) -> *mut u8 {
    let Some(host) = HOST else {
        return null_mut();
    };
    match (*host).load_module(from_c(name)) {
        Some(src) => {
            *len = src.len();
            c_string(&src)
        }
        None => null_mut(),
    }
}

#[no_mangle]
extern "C" fn everos_js_interrupt() -> c_int {
    // a page loading in the background lets the desktop run meanwhile
    crate::fiber::pause_if_slice_used();
    (crate::fiber::cancelled() || interrupts::ticks() > unsafe { DEADLINE }) as c_int
}

/// The running script's globals, kept while its fiber is paused.
pub struct SavedState(Option<*mut dyn Host>, u64);

pub fn save_state() -> SavedState {
    unsafe {
        let s = SavedState(HOST, DEADLINE);
        HOST = None;
        DEADLINE = u64::MAX;
        s
    }
}

/// Put the globals back; the time limit does not count `paused` ticks.
pub fn restore_state(s: SavedState, paused: u64) {
    unsafe {
        HOST = s.0;
        DEADLINE = s.1.saturating_add(paused);
    }
}

/// A host for the shell's `js` command: console output only.
pub struct ConsoleHost;

impl Host for ConsoleHost {
    fn call(&mut self, op: &str, args: &[Option<&str>]) -> Value {
        match op {
            "log" => {
                crate::println!("{}", args.first().copied().flatten().unwrap_or(""));
                Value::Undefined
            }
            "now" => Value::Int(now_ms()),
            _ => Value::Undefined,
        }
    }
}

/// Glue every page and the shell get: console.log and friends.
pub const BASE_PRELUDE: &str = r#"
globalThis.window = globalThis; globalThis.self = globalThis;
(function(){
  const fmt = a => a.map(v => typeof v === 'string' ? v : (() => { try { return JSON.stringify(v) ?? String(v); } catch(e) { return String(v); } })()).join(' ');
  const log = (...a) => __native('log', fmt(a));
  globalThis.console = { log, info: log, warn: log, error: log, debug: log, trace: log, dir: log, table: log, group: log, groupCollapsed: log, groupEnd(){}, time(){}, timeEnd(){}, assert(c, ...a){ if(!c) log('Assertion failed:', ...a); } };
})();
"#;
