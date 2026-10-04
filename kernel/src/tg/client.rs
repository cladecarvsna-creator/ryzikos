//! The client: connects to Telegram, signs in, loads the chat list and
//! chats, sends messages and applies the updates the server pushes. It
//! runs in a fiber and talks to the window through [`Shared`].

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::fmt::Write;
use core::sync::atomic::{AtomicUsize, Ordering};

use smoltcp::wire::Ipv4Address;

use super::crypto;
use super::mtproto::{self, now_ms, Error, Result, Session, Transport};
use super::tl::{self, Kind, Obj, Reader, Value};
use super::{
    Chat, ChatKind, Cmd, Download, FileInfo, FileRef, Info, Message, Peer, Photo, Preview, Shared,
    Stage,
};
use crate::{fs, serial};

/// Telegram's data centres, and the test ones.
const PROD_DCS: [[u8; 4]; 5] = [
    [149, 154, 175, 53],
    [149, 154, 167, 51],
    [149, 154, 175, 100],
    [149, 154, 167, 91],
    [91, 108, 56, 130],
];
const TEST_DCS: [[u8; 4]; 3] = [
    [149, 154, 175, 10],
    [149, 154, 167, 40],
    [149, 154, 175, 117],
];

/// The API layer of schema.tl.
const LAYER: i32 = 229;
const PING_MS: i64 = 60_000;
/// How often to tell Telegram we are still online.
const ONLINE_MS: i64 = 240_000;
/// How long "typing..." lasts without news.
const TYPING_MS: i64 = 6_000;
const HISTORY_PAGE: i32 = 40;
/// Files go up in parts of this size (512 KB must be a multiple of it).
const UPLOAD_PART: usize = 128 * 1024;
const MAX_UPLOAD: usize = 64 << 20;
/// Downloads come in parts of this size (Telegram wants a power of two).
const DOWNLOAD_PART: usize = 512 * 1024;
const MAX_DOWNLOAD: usize = 256 << 20;
/// Pictures in the chat are at most this many pixels wide and high.
const PREVIEW_MAX: usize = 320;

/// The last things the client did: `telegram log` shows them, and the
/// connecting screen shows the newest.
static RECENT: crate::sync::IrqMutex<Vec<String>> = crate::sync::IrqMutex::new(Vec::new());

pub(super) fn log(s: &str) {
    serial::write_str("\ntelegram: ");
    serial::write_str(s);
    serial::write_str("\n");
    let mut recent = RECENT.lock();
    if recent.len() >= 40 {
        recent.remove(0);
    }
    let t = now_ms() / 1000;
    recent.push(format!(
        "{:02}:{:02}:{:02} {}",
        t / 3600 % 24,
        t / 60 % 60,
        t % 60,
        s
    ));
}

/// What the client did lately, oldest first.
pub fn recent_log() -> Vec<String> {
    RECENT.lock().clone()
}

/// The newest line of [`recent_log`], without its time.
pub fn last_step() -> Option<String> {
    let recent = RECENT.lock();
    let line = recent.last()?;
    Some(
        line.split_once(' ')
            .map_or(line.clone(), |(_, s)| s.to_string()),
    )
}

// ---- settings ----------------------------------------------------------------------

/// telegram.conf: `key=value` lines.
#[derive(Clone, Default)]
pub struct Config {
    pub api_id: i32,
    pub api_hash: String,
    /// Use Telegram's test servers.
    pub test: bool,
    /// Connect here instead of Telegram's servers (for a test server).
    pub server: Option<(Ipv4Address, u16)>,
    /// More RSA keys to trust, as moduli (for a test server).
    pub keys: Vec<(u64, Vec<u8>)>,
}

fn user_dir() -> Option<String> {
    let user = crate::users::current_name()?;
    Some(fs::app_data(user.as_str()))
}

fn conf_path() -> Option<String> {
    Some(fs::join(&user_dir()?, "telegram.conf"))
}

fn session_path() -> Option<String> {
    Some(fs::join(&user_dir()?, "telegram.session"))
}

fn read_pairs(path: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    if let Ok(data) = fs::read(path) {
        for line in String::from_utf8_lossy(&data).lines() {
            let line = line.trim();
            if line.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                map.insert(String::from(k.trim()), String::from(v.trim()));
            }
        }
    }
    map
}

fn parse_addr(s: &str) -> Option<(Ipv4Address, u16)> {
    let (ip, port) = s.split_once(':')?;
    let mut parts = [0u8; 4];
    let mut n = 0;
    for p in ip.split('.') {
        *parts.get_mut(n)? = p.parse().ok()?;
        n += 1;
    }
    (n == 4).then_some(())?;
    Some((
        Ipv4Address::new(parts[0], parts[1], parts[2], parts[3]),
        port.parse().ok()?,
    ))
}

/// The api_id and api_hash the release build carries, if any.
pub fn built_in() -> Option<(i32, String)> {
    let id = option_env!("RYZIKOS_TG_API_ID")?.trim().parse().ok()?;
    let hash = option_env!("RYZIKOS_TG_API_HASH")?.trim();
    (hash.len() == 32).then(|| (id, String::from(hash)))
}

pub fn load_config() -> Option<Config> {
    let map = read_pairs(&conf_path()?);
    let own = map
        .get("api_id")
        .and_then(|v| v.parse().ok())
        .zip(map.get("api_hash").filter(|h| h.len() == 32).cloned());
    let from_build = own.is_none();
    let (api_id, api_hash) = own.or_else(built_in)?;
    if from_build {
        // keep the build's keys with the settings, so a later update
        // built without them still signs in
        let _ = save_config(&format!("{}", api_id), &api_hash);
    }
    let mut keys = Vec::new();
    if let Some(hex) = map.get("rsa") {
        let n = crypto::from_hex(hex);
        keys.push((crypto::fingerprint(&n), n));
    }
    Some(Config {
        api_id,
        api_hash,
        test: map.get("test").is_some_and(|v| v == "1"),
        server: map.get("server").and_then(|s| parse_addr(s)),
        keys,
    })
}

/// Check and save what the user typed on the first screen.
fn save_config(api_id: &str, api_hash: &str) -> core::result::Result<(), String> {
    let id: i32 = api_id
        .trim()
        .parse()
        .map_err(|_| String::from("api_id is a number, like 1234567"))?;
    let hash = api_hash.trim().to_ascii_lowercase();
    if hash.len() != 32 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(String::from("api_hash is 32 letters and digits (0-9, a-f)"));
    }
    let path = conf_path().ok_or("nobody is signed in")?;
    // keep the developer options already in the file
    let mut text = format!("api_id={}\r\napi_hash={}\r\n", id, hash);
    for (k, v) in read_pairs(&path) {
        if k != "api_id" && k != "api_hash" {
            let _ = write!(text, "{}={}\r\n", k, v);
        }
    }
    fs::write(&path, text.as_bytes()).map_err(|e| String::from(e.message()))
}

/// telegram.session: the data centre, the key and who signed in.
#[derive(Clone)]
struct Saved {
    dc: i32,
    key: Option<[u8; 256]>,
    user: i64,
}

impl Default for Saved {
    fn default() -> Saved {
        // new accounts start at DC 2, which moves them where they belong
        Saved {
            dc: 2,
            key: None,
            user: 0,
        }
    }
}

fn load_session() -> Saved {
    let Some(path) = session_path() else {
        return Saved::default();
    };
    let map = read_pairs(&path);
    let key = map
        .get("key")
        .map(|h| crypto::from_hex(h))
        .and_then(|k| k.try_into().ok());
    Saved {
        dc: map.get("dc").and_then(|v| v.parse().ok()).unwrap_or(2),
        key,
        user: map.get("user").and_then(|v| v.parse().ok()).unwrap_or(0),
    }
}

fn save_session(s: &Saved) {
    let Some(path) = session_path() else {
        return;
    };
    let mut text = format!("dc={}\r\nuser={}\r\n", s.dc, s.user);
    if let Some(key) = &s.key {
        text.push_str("key=");
        for b in key {
            let _ = write!(text, "{:02x}", b);
        }
        text.push_str("\r\n");
    }
    let _ = fs::write(&path, text.as_bytes());
}

// ---- one connection ----------------------------------------------------------------------

struct Conn {
    t: Transport,
    s: Session,
    /// initConnection was sent.
    inited: bool,
    /// Server messages to acknowledge.
    acks: Vec<i64>,
    /// The request we wait for, and its answer when it comes.
    waiting: i64,
    result: Option<Vec<u8>>,
    /// Our messages the server asked us to send again.
    resend: bool,
    /// Our message the server rejected, with its error code.
    rejected: Option<i32>,
    updates: Vec<Obj>,
    last_sent: i64,
}

fn dc_address(cfg: &Config, dc: i32) -> (Ipv4Address, u16) {
    if let Some(a) = cfg.server {
        return a;
    }
    let ip = if cfg.test {
        TEST_DCS[(dc as usize).clamp(1, TEST_DCS.len()) - 1]
    } else {
        PROD_DCS[(dc as usize).clamp(1, PROD_DCS.len()) - 1]
    };
    // Telegram also listens on 80 and 5222: after a failure try the next
    let port = PORTS[PORT_TRY.load(Ordering::Relaxed) % PORTS.len()];
    (Ipv4Address::new(ip[0], ip[1], ip[2], ip[3]), port)
}

const PORTS: [u16; 3] = [443, 80, 5222];
/// Which of [`PORTS`] to use; moves on after network errors.
static PORT_TRY: AtomicUsize = AtomicUsize::new(0);
/// Network errors in a row on this port.
static PORT_FAILS: AtomicUsize = AtomicUsize::new(0);

/// A connection failed or dropped. A port that worked before gets a
/// second try (a dropped connection is often just the Wi-Fi or the
/// provider for a moment); after that the next port.
fn net_failed() {
    if PORT_FAILS.fetch_add(1, Ordering::Relaxed) >= 1 {
        PORT_FAILS.store(0, Ordering::Relaxed);
        PORT_TRY.fetch_add(1, Ordering::Relaxed);
    }
}

impl Conn {
    /// Connect to a data centre, with our key for it or a new one.
    fn open(cfg: &Config, dc: i32, key: Option<[u8; 256]>) -> Result<(Conn, [u8; 256])> {
        let (ip, port) = dc_address(cfg, dc);
        // know the real time before Telegram sees our first message
        mtproto::clock_offset();
        log(&format!("connecting to DC {} at {}:{}", dc, ip, port));
        let dc_number = if cfg.test { 10000 + dc } else { dc };
        let mut t = Transport::connect(ip, port, dc_number)?;
        let (key, salt, offset) = match key {
            Some(k) => (k, 0, mtproto::clock_offset()),
            None => {
                log("creating an authorization key");
                let k = mtproto::create_key(&mut t, dc_number, &cfg.keys)?;
                log("authorization key ready");
                (k.key, k.salt, k.time_offset)
            }
        };
        let conn = Conn {
            t,
            s: Session::new(key, salt, offset),
            inited: false,
            acks: Vec::new(),
            waiting: 0,
            result: None,
            resend: false,
            rejected: None,
            updates: Vec::new(),
            last_sent: now_ms(),
        };
        Ok((conn, key))
    }

    fn send(&mut self, body: &[u8], content: bool) -> Result<i64> {
        let (id, packet) = self.s.encrypt(body, content);
        self.t.send(&packet)?;
        self.last_sent = now_ms();
        Ok(id)
    }

    fn flush_acks(&mut self) -> Result<()> {
        if self.acks.is_empty() {
            return Ok(());
        }
        let ids: Vec<Value> = self.acks.drain(..).map(Value::Long).collect();
        let ack = Obj::new("msgs_ack", &[("msg_ids", Value::Vector(ids))]);
        self.send(&tl::encode(&ack), false)?;
        Ok(())
    }

    /// Call an API method and wait for its answer.
    fn invoke(&mut self, cfg: &Config, req: &Obj) -> Result<Value> {
        let kind = req.ctor().result.clone();
        let body = if self.inited {
            tl::encode(req)
        } else {
            // the first request says who we are and which layer we speak
            let init = Obj::new(
                "initConnection",
                &[
                    ("api_id", Value::Int(cfg.api_id)),
                    ("device_model", Value::str("RyzikOS PC")),
                    (
                        "system_version",
                        Value::str(concat!("RyzikOS ", env!("CARGO_PKG_VERSION"))),
                    ),
                    ("app_version", Value::str(env!("CARGO_PKG_VERSION"))),
                    ("system_lang_code", Value::str("en")),
                    ("lang_pack", Value::str("")),
                    ("lang_code", Value::str("en")),
                    ("query", req.clone().into()),
                ],
            );
            tl::encode(&Obj::new(
                "invokeWithLayer",
                &[("layer", Value::Int(LAYER)), ("query", init.into())],
            ))
        };
        for _ in 0..5 {
            self.flush_acks()?;
            let msg_id = self.send(&body, true)?;
            self.waiting = msg_id;
            self.result = None;
            self.resend = false;
            self.rejected = None;
            let deadline = now_ms() + mtproto::TIMEOUT_MS;
            while self.result.is_none() && !self.resend && self.rejected.is_none() {
                let packet = self.t.recv(deadline)?;
                self.packet(&packet)?;
            }
            if let Some(code) = self.rejected {
                return Err(Error::Other(format!(
                    "the server rejected a message ({})",
                    code
                )));
            }
            if let Some(raw) = self.result.take() {
                self.waiting = 0;
                let v = parse_result(&raw, &kind)?;
                self.inited = true;
                return Ok(v);
            }
        }
        Err(Error::Other(String::from(
            "the server keeps asking to resend",
        )))
    }

    /// Read whatever has arrived, without waiting; ping when quiet.
    fn poll(&mut self) -> Result<()> {
        while let Some(p) = self.t.poll()? {
            self.packet(&p)?;
        }
        if now_ms() - self.last_sent > PING_MS {
            // keeps the connection (and any NAT on the way) alive
            let ping = Obj::new(
                "ping_delay_disconnect",
                &[
                    ("ping_id", Value::Long(crypto::random_i64())),
                    ("disconnect_delay", Value::Int(75)),
                ],
            );
            self.flush_acks()?;
            self.send(&tl::encode(&ping), true)?;
        } else if self.acks.len() >= 8 {
            self.flush_acks()?;
        }
        Ok(())
    }

    fn packet(&mut self, packet: &[u8]) -> Result<()> {
        let (msg_id, seq, body) = self.s.decrypt(packet)?;
        // the server's messages carry its time: follow its clock
        let offset = (msg_id >> 32) - now_ms() / 1000;
        if (offset - self.s.time_offset).abs() > 2 {
            self.s.time_offset = offset;
        }
        self.message(msg_id, seq, &body)
    }

    fn message(&mut self, msg_id: i64, seq: i32, body: &[u8]) -> Result<()> {
        if seq & 1 == 1 {
            self.acks.push(msg_id);
        }
        let mut r = Reader::new(body);
        let id = r.u32()?;
        match id {
            tl::MSG_CONTAINER => {
                let n = r.u32()?;
                for _ in 0..n {
                    let inner_id = r.i64()?;
                    let inner_seq = r.i32()?;
                    let len = r.i32()? as usize;
                    let inner = r.take(len)?;
                    self.message(inner_id, inner_seq, inner)?;
                }
            }
            tl::RPC_RESULT => {
                let req = r.i64()?;
                if req == self.waiting {
                    self.result = Some(body[12..].to_vec());
                }
            }
            tl::GZIP_PACKED => {
                let data = tl::gunzip(r.bytes()?)?;
                self.message(msg_id, 0, &data)?;
            }
            _ => {
                let obj = match Reader::new(body).obj() {
                    Ok(o) => o,
                    Err(e) => {
                        log(&format!("can't read a message: {}", e));
                        return Ok(());
                    }
                };
                self.service(msg_id, obj);
            }
        }
        Ok(())
    }

    fn service(&mut self, msg_id: i64, obj: Obj) {
        match obj.name() {
            "bad_server_salt" => {
                self.s.salt = obj.int("new_server_salt");
                if obj.int("bad_msg_id") == self.waiting {
                    self.resend = true;
                }
            }
            "bad_msg_notification" => {
                let ours = obj.int("bad_msg_id") == self.waiting;
                match obj.int("error_code") {
                    16 | 17 => {
                        // our clock is off: take the server's time
                        self.s.time_offset = (msg_id >> 32) - now_ms() / 1000;
                        log("fixed the clock offset");
                    }
                    32 | 33 => self.s.reset(),
                    48 => {}
                    code if ours => self.rejected = Some(code as i32),
                    _ => {}
                }
                if ours && self.rejected.is_none() {
                    self.resend = true;
                }
            }
            "new_session_created" => self.s.salt = obj.int("server_salt"),
            "updates"
            | "updatesCombined"
            | "updateShort"
            | "updateShortMessage"
            | "updateShortChatMessage"
            | "updatesTooLong" => self.updates.push(obj),
            _ => {}
        }
    }
}

/// An rpc_result's contents: an error, or a value of the method's type.
fn parse_result(raw: &[u8], kind: &Kind) -> Result<Value> {
    let mut r = Reader::new(raw);
    let id = r.u32()?;
    if id == 0x2144_ca19 {
        let code = r.i32()?;
        let message = String::from_utf8_lossy(r.bytes()?).into_owned();
        return Err(Error::Rpc { code, message });
    }
    if id == tl::GZIP_PACKED {
        let data = tl::gunzip(r.bytes()?)?;
        return parse_result(&data, kind);
    }
    Ok(Reader::new(raw).boxed(kind)?)
}

// ---- the client ---------------------------------------------------------------------------

#[derive(Clone)]
struct UserInfo {
    access_hash: i64,
    name: String,
    bot: bool,
    username: String,
    phone: String,
}

#[derive(Clone)]
struct ChatInfo {
    access_hash: i64,
    title: String,
    broadcast: bool,
    username: String,
    /// We are not (or no longer) in it.
    left: bool,
}

struct Client {
    shared: Rc<RefCell<Shared>>,
    cfg: Config,
    saved: Saved,
    conn: Option<Conn>,
    users: BTreeMap<i64, UserInfo>,
    chats: BTreeMap<i64, ChatInfo>,
    me: i64,
    /// Messages being sent: random id -> chat.
    sending: BTreeMap<i64, Peer>,
    /// The chat list must be loaded again.
    reload: bool,
    /// A chat on screen got new messages: tell the server they were read.
    read: Option<Peer>,
    /// Log in with a QR code rather than the phone number.
    use_qr: bool,
    /// The QR code was scanned (updateLoginToken came).
    login_token: bool,
    /// When to wake up with [`Cmd::Wake`], in ms; 0 for never.
    wake_at: i64,
    /// Connections to other data centres, for their files.
    media: BTreeMap<i32, Conn>,
    /// Chats whose pinned messages changed: load them again.
    pinned_dirty: Vec<Peer>,
    /// When we last told Telegram we are online, in ms.
    online_at: i64,
}

/// How a way of logging in ended.
enum Next {
    /// Signed in (None) or stopped.
    Done(Option<Stop>),
    /// The user picked the other way.
    Switch,
}

/// Why the client stopped.
enum Stop {
    /// The window closed.
    Cancelled,
    /// Start over (after an error, a new key or signing out).
    Restart,
}

/// The fiber's body: run until the window closes.
pub fn run(shared: Rc<RefCell<Shared>>) {
    let mut c = Client {
        shared,
        cfg: Config::default(),
        saved: Saved::default(),
        conn: None,
        users: BTreeMap::new(),
        chats: BTreeMap::new(),
        me: 0,
        sending: BTreeMap::new(),
        reload: false,
        read: None,
        use_qr: true,
        login_token: false,
        wake_at: 0,
        media: BTreeMap::new(),
        pinned_dirty: Vec::new(),
        online_at: 0,
    };
    let mut failures = 0;
    loop {
        let started = now_ms();
        match c.session() {
            Ok(Stop::Cancelled) => break,
            Ok(Stop::Restart) => failures = 0,
            Err(e) => {
                if crate::fiber::cancelled() {
                    break;
                }
                log(&format!("error: {}", e.text()));
                c.conn = None;
                // a connection that worked for a while and then dropped
                // starts the count again, so it comes back quickly
                if c.me != 0 && now_ms() - started > 60_000 {
                    failures = 0;
                }
                failures += 1;
                if matches!(e, Error::KeyUnknown) {
                    // the server dropped our key: make a new one, at once
                    // the first time, but don't go round in circles
                    c.saved = Saved::default();
                    save_session(&c.saved);
                    if failures == 1 {
                        continue;
                    }
                }
                if matches!(e, Error::Net(_)) {
                    net_failed();
                }
                c.update(|s| {
                    s.online = false;
                    s.busy = false;
                    s.error = Some(format!(
                        "Can't reach Telegram: {}. Trying again (attempt {})...",
                        e.text(),
                        failures + 1
                    ));
                });
                // wait a little longer after each failure, up to half a
                // minute; the first time hardly at all
                let wait = if failures == 1 {
                    500
                } else {
                    (1_000i64 << failures.min(5)).min(30_000)
                };
                let until = now_ms() + wait;
                while now_ms() < until && !crate::fiber::cancelled() {
                    if c.shared
                        .borrow()
                        .commands
                        .iter()
                        .any(|c| matches!(c, Cmd::Reload))
                    {
                        break;
                    }
                    crate::fiber::pause();
                }
            }
        }
        if crate::fiber::cancelled() {
            break;
        }
    }
    log("stopped");
}

impl Client {
    fn update(&self, f: impl FnOnce(&mut Shared)) {
        let mut s = self.shared.borrow_mut();
        f(&mut s);
        s.changed();
    }

    fn stage(&self, stage: Stage) {
        self.update(|s| {
            s.stage = stage;
            s.busy = false;
        });
    }

    fn error(&self, text: impl Into<String>) {
        let text = text.into();
        self.update(|s| {
            s.error = Some(text);
            s.busy = false;
        });
    }

    /// A note for the window that goes away by itself, for things that
    /// went wrong once signed in (a link to nowhere, a failed join).
    fn notice(&self, text: impl Into<String>) {
        let text = text.into();
        self.update(|s| {
            s.notice = Some(text);
            s.busy = false;
        });
    }

    /// Wait for the window to ask for something, keeping the connection
    /// served meanwhile. None once the window has closed.
    fn next_command(&mut self) -> Result<Option<Cmd>> {
        loop {
            if crate::fiber::cancelled() {
                return Ok(None);
            }
            if let Some(conn) = &mut self.conn {
                conn.poll()?;
                if !conn.updates.is_empty() {
                    let updates = core::mem::take(&mut conn.updates);
                    for u in updates {
                        self.apply(&u);
                    }
                }
            }
            if core::mem::take(&mut self.reload) && self.me != 0 {
                return Ok(Some(Cmd::Reload));
            }
            if let Some(peer) = self.read.take() {
                let open = self.shared.borrow().open == Some(peer);
                if open {
                    match self.mark_read(peer) {
                        Err(Error::Rpc { .. }) | Ok(()) => {}
                        Err(e) => return Err(e),
                    }
                }
            }
            if self.me != 0 && self.shared.borrow().stage == Stage::Ready {
                if let Some(peer) = self.pinned_dirty.pop() {
                    return Ok(Some(Cmd::Pinned(peer)));
                }
                if now_ms() - self.online_at > ONLINE_MS {
                    self.online_at = now_ms();
                    // online, so Telegram sends who else is online and typing
                    let req = Obj::new("account.updateStatus", &[("offline", Value::Bool(false))]);
                    match self.call(&req) {
                        Err(Error::Rpc { .. }) | Ok(_) => {}
                        Err(e) => return Err(e),
                    }
                }
                self.forget_typing();
            }
            if self.login_token || (self.wake_at != 0 && now_ms() >= self.wake_at) {
                self.login_token = false;
                self.wake_at = 0;
                return Ok(Some(Cmd::Wake));
            }
            let cmd = self.shared.borrow_mut().commands.pop_front();
            if let Some(cmd) = cmd {
                if !cmd.quiet() {
                    self.update(|s| {
                        s.error = None;
                        s.busy = true;
                    });
                }
                return Ok(Some(cmd));
            }
            crate::fiber::pause();
        }
    }

    fn call(&mut self, req: &Obj) -> Result<Value> {
        let conn = self
            .conn
            .as_mut()
            .ok_or_else(|| Error::Other(String::from("not connected")))?;
        let v = conn.invoke(&self.cfg, req);
        // our clock runs on local time in QEMU: the difference to the
        // server's is the time zone, in quarter hours
        let tz = (450 - conn.s.time_offset).div_euclid(900) * 900;
        let offset = conn.s.time_offset;
        let updates = core::mem::take(&mut conn.updates);
        let known = {
            let s = self.shared.borrow();
            (s.tz, s.time_offset)
        };
        if known != (tz, offset) {
            self.update(|s| {
                s.tz = tz;
                s.time_offset = offset;
            });
        }
        for u in updates {
            self.apply(&u);
        }
        v
    }

    /// Call a method; on X_MIGRATE_n, move to data centre n and call again.
    fn call_anywhere(&mut self, req: &Obj) -> Result<Value> {
        match self.call(req) {
            Err(e) if matches!(&e, Error::Rpc { code: 303, .. }) => {
                let dc = ["PHONE_MIGRATE_", "USER_MIGRATE_", "NETWORK_MIGRATE_"]
                    .iter()
                    .find_map(|p| e.number_after(p))
                    .ok_or(e)?;
                log(&format!("moving to DC {}", dc));
                self.connect(dc as i32, None)?;
                self.call(req)
            }
            other => other,
        }
    }

    fn connect(&mut self, dc: i32, key: Option<[u8; 256]>) -> Result<()> {
        self.conn = None;
        let (conn, key) = Conn::open(&self.cfg, dc, key)?;
        self.conn = Some(conn);
        if self.saved.dc != dc || self.saved.key != Some(key) {
            self.saved = Saved {
                dc,
                key: Some(key),
                user: 0,
            };
            save_session(&self.saved);
        }
        PORT_FAILS.store(0, Ordering::Relaxed);
        self.update(|s| {
            s.online = true;
            s.error = None;
        });
        Ok(())
    }

    /// Settings, connection, sign-in, then chats until something breaks.
    fn session(&mut self) -> Result<Stop> {
        self.update(|s| {
            s.stage = Stage::Starting;
            s.online = false;
        });
        self.cfg = loop {
            if let Some(cfg) = load_config() {
                break cfg;
            }
            self.stage(Stage::Config);
            match self.next_command()? {
                None => return Ok(Stop::Cancelled),
                Some(Cmd::Config { api_id, api_hash }) => {
                    if let Err(e) = save_config(&api_id, &api_hash) {
                        self.error(e);
                    }
                }
                Some(_) => {}
            }
        };
        self.update(|s| s.stage = Stage::Starting);
        self.saved = load_session();
        self.connect(self.saved.dc, self.saved.key)?;
        if self.saved.user == 0 {
            if let Some(stop) = self.sign_in()? {
                return Ok(stop);
            }
        }
        match self.start_chats() {
            Err(e) if is_signed_out(&e) => {
                self.sign_out_locally();
                return Ok(Stop::Restart);
            }
            other => other?,
        }
        loop {
            let Some(cmd) = self.next_command()? else {
                return Ok(Stop::Cancelled);
            };
            let result = self.command(cmd);
            self.update(|s| s.busy = false);
            match result {
                Ok(Some(stop)) => return Ok(stop),
                Ok(None) => {}
                Err(e) if is_signed_out(&e) => {
                    self.sign_out_locally();
                    return Ok(Stop::Restart);
                }
                Err(e @ Error::Rpc { .. }) => self.error(friendly(&e)),
                Err(e) => return Err(e),
            }
        }
    }

    // ---- signing in ----------------------------------------------------------------

    /// Log in by QR code or by phone number, as the user picks. None when
    /// signed in.
    fn sign_in(&mut self) -> Result<Option<Stop>> {
        loop {
            let next = if self.use_qr {
                self.qr_login()?
            } else {
                self.phone_login()?
            };
            match next {
                Next::Done(stop) => return Ok(stop),
                Next::Switch => {
                    self.use_qr = !self.use_qr;
                    self.update(|s| s.error = None);
                }
            }
        }
    }

    /// Show a QR code for the phone to scan (Settings, Devices, Link
    /// Desktop Device) and wait until it is scanned; a new code every
    /// half minute.
    fn qr_login(&mut self) -> Result<Next> {
        loop {
            log("asking for a QR code");
            let req = Obj::new(
                "auth.exportLoginToken",
                &[
                    ("api_id", Value::Int(self.cfg.api_id)),
                    ("api_hash", Value::str(&self.cfg.api_hash)),
                    ("except_ids", Value::Vector(Vec::new())),
                ],
            );
            let answer = match self.call(&req) {
                Ok(v) => v
                    .as_obj()
                    .cloned()
                    .ok_or("bad answer to exportLoginToken")?,
                Err(e) if e.is("SESSION_PASSWORD_NEEDED") => {
                    return self.password().map(Next::Done)
                }
                Err(e @ Error::Rpc { .. }) => {
                    self.stage(Stage::Qr(String::new()));
                    self.error(friendly(&e));
                    // try again in a while, unless the user picks the phone
                    self.wake_at = now_ms() + 15_000;
                    match self.wait_login()? {
                        Some(next) => return Ok(next),
                        None => continue,
                    }
                }
                Err(e) => return Err(e),
            };
            if let Some(next) = self.login_token(answer)? {
                return Ok(next);
            }
        }
    }

    /// Act on an auth.LoginToken. None to ask for a new code.
    fn login_token(&mut self, t: Obj) -> Result<Option<Next>> {
        match t.name() {
            "auth.loginTokenSuccess" => {
                self.signed_in(t.obj("authorization"))?;
                Ok(Some(Next::Done(None)))
            }
            "auth.loginTokenMigrateTo" => {
                // the account lives in another data centre: log in there
                let dc = t.int("dc_id") as i32;
                log(&format!("the QR login moves to DC {}", dc));
                self.connect(dc, None)?;
                let req = Obj::new(
                    "auth.importLoginToken",
                    &[("token", Value::Bytes(t.bytes("token").to_vec()))],
                );
                match self.call(&req) {
                    Ok(v) => {
                        let o = v
                            .as_obj()
                            .cloned()
                            .ok_or("bad answer to importLoginToken")?;
                        if o.is("auth.loginTokenMigrateTo") {
                            return Err(Error::Other(String::from("the QR login moved twice")));
                        }
                        self.login_token(o)
                    }
                    Err(e) if e.is("SESSION_PASSWORD_NEEDED") => {
                        self.password().map(|stop| Some(Next::Done(stop)))
                    }
                    Err(Error::Rpc { .. }) => Ok(None),
                    Err(e) => Err(e),
                }
            }
            _ => {
                let link = format!("tg://login?token={}", base64url(t.bytes("token")));
                self.stage(Stage::Qr(link));
                let server_now =
                    now_ms() / 1000 + self.conn.as_ref().map_or(0, |c| c.s.time_offset);
                let left = (t.int("expires") - server_now).clamp(5, 60);
                self.wake_at = now_ms() + left * 1000;
                self.wait_login()
            }
        }
    }

    /// Wait on the QR screen: None when the code was scanned or ran out.
    fn wait_login(&mut self) -> Result<Option<Next>> {
        loop {
            let cmd = self.next_command()?;
            if !matches!(cmd, Some(Cmd::Wake)) {
                self.update(|s| s.busy = false);
            }
            match cmd {
                None => return Ok(Some(Next::Done(Some(Stop::Cancelled)))),
                Some(Cmd::Wake) => return Ok(None),
                Some(Cmd::UsePhone) => {
                    self.wake_at = 0;
                    return Ok(Some(Next::Switch));
                }
                Some(Cmd::LogOut) => return Ok(Some(Next::Done(Some(Stop::Restart)))),
                Some(_) => {}
            }
        }
    }

    /// Ask for the phone number, the code and maybe the password.
    fn phone_login(&mut self) -> Result<Next> {
        self.stage(Stage::Phone);
        let (phone, hash) = loop {
            let phone = match self.next_command()? {
                None => return Ok(Next::Done(Some(Stop::Cancelled))),
                Some(Cmd::Phone(p)) => p,
                Some(Cmd::UseQr) => return Ok(Next::Switch),
                Some(_) => {
                    self.update(|s| s.busy = false);
                    continue;
                }
            };
            let phone: String = phone
                .chars()
                .filter(|c| c.is_ascii_digit() || *c == '+')
                .collect();
            if phone.len() < 5 {
                self.error("Enter the phone number with the country code, like +7 900 123 45 67");
                continue;
            }
            let req = Obj::new(
                "auth.sendCode",
                &[
                    ("phone_number", Value::str(&phone)),
                    ("api_id", Value::Int(self.cfg.api_id)),
                    ("api_hash", Value::str(&self.cfg.api_hash)),
                    ("settings", Obj::new("codeSettings", &[]).into()),
                ],
            );
            match self.call_anywhere(&req) {
                Ok(v) => {
                    let sent = v.as_obj().cloned().ok_or("bad answer to sendCode")?;
                    if sent.is("auth.sentCodeSuccess") {
                        self.signed_in(sent.obj("authorization"))?;
                        return Ok(Next::Done(None));
                    }
                    let hint = code_hint(sent.obj("type"));
                    self.stage(Stage::Code(hint));
                    break (phone, sent.string("phone_code_hash"));
                }
                Err(e @ Error::Rpc { .. }) => self.error(friendly(&e)),
                Err(e) => return Err(e),
            }
        };
        loop {
            let code = match self.next_command()? {
                None => return Ok(Next::Done(Some(Stop::Cancelled))),
                Some(Cmd::Code(c)) => c,
                Some(Cmd::Phone(_)) | Some(Cmd::LogOut) => {
                    return Ok(Next::Done(Some(Stop::Restart)))
                }
                Some(Cmd::UseQr) => return Ok(Next::Switch),
                Some(_) => {
                    self.update(|s| s.busy = false);
                    continue;
                }
            };
            let code: String = code.chars().filter(|c| c.is_ascii_digit()).collect();
            let req = Obj::new(
                "auth.signIn",
                &[
                    ("phone_number", Value::str(&phone)),
                    ("phone_code_hash", Value::str(&hash)),
                    ("phone_code", Value::str(&code)),
                ],
            );
            match self.call(&req) {
                Ok(v) => {
                    let auth = v.as_obj().cloned().ok_or("bad answer to signIn")?;
                    if auth.is("auth.authorizationSignUpRequired") {
                        self.stage(Stage::Phone);
                        self.error(
                            "This number has no Telegram account yet. Sign up in the Telegram app first.",
                        );
                        return Ok(Next::Done(Some(Stop::Restart)));
                    }
                    self.signed_in(Some(&auth))?;
                    return Ok(Next::Done(None));
                }
                Err(e) if e.is("SESSION_PASSWORD_NEEDED") => {
                    return self.password().map(Next::Done)
                }
                Err(e) if e.is("PHONE_CODE_EXPIRED") => {
                    self.error("The code expired. Enter the phone number again.");
                    return Ok(Next::Done(Some(Stop::Restart)));
                }
                Err(e @ Error::Rpc { .. }) => self.error(friendly(&e)),
                Err(e) => return Err(e),
            }
        }
    }

    /// Two-step verification: prove the password with SRP.
    fn password(&mut self) -> Result<Option<Stop>> {
        loop {
            let info = self.call(&Obj::new("account.getPassword", &[]))?;
            let info = info.as_obj().cloned().ok_or("bad account.password")?;
            let hint = info.string("hint");
            self.stage(Stage::Password(hint));
            let password = loop {
                match self.next_command()? {
                    None => return Ok(Some(Stop::Cancelled)),
                    Some(Cmd::Password(p)) => break p,
                    Some(Cmd::Phone(_)) | Some(Cmd::LogOut) => return Ok(Some(Stop::Restart)),
                    Some(_) => {}
                }
            };
            let algo = info
                .obj("current_algo")
                .filter(|a| {
                    a.is("passwordKdfAlgoSHA256SHA256PBKDF2HMACSHA512iter100000SHA256ModPow")
                })
                .ok_or("this password uses an algorithm RyzikOS does not know")?;
            // PBKDF2 with 100000 rounds takes a moment
            crate::fiber::pause();
            let a: [u8; 256] = crypto::random_array();
            let (big_a, m1) = crypto::srp(
                &password,
                algo.bytes("salt1"),
                algo.bytes("salt2"),
                algo.int("g") as u32,
                algo.bytes("p"),
                info.bytes("srp_B"),
                &a,
            )
            .map_err(|e| Error::Other(String::from(e)))?;
            let check = Obj::new(
                "inputCheckPasswordSRP",
                &[
                    ("srp_id", Value::Long(info.int("srp_id"))),
                    ("A", Value::Bytes(big_a.to_vec())),
                    ("M1", Value::Bytes(m1.to_vec())),
                ],
            );
            let req = Obj::new("auth.checkPassword", &[("password", check.into())]);
            match self.call(&req) {
                Ok(v) => {
                    self.signed_in(v.as_obj())?;
                    return Ok(None);
                }
                Err(e @ Error::Rpc { .. }) => {
                    self.error(friendly(&e));
                    // a new srp_B comes with the next getPassword
                }
                Err(e) => return Err(e),
            }
        }
    }

    fn signed_in(&mut self, auth: Option<&Obj>) -> Result<()> {
        let user = auth
            .and_then(|a| a.obj("user"))
            .ok_or("no user after signing in")?;
        self.remember_user(user);
        self.saved.user = user.int("id");
        save_session(&self.saved);
        log("signed in");
        Ok(())
    }

    fn sign_out_locally(&mut self) {
        self.saved.user = 0;
        self.me = 0;
        save_session(&self.saved);
        self.update(|s| {
            s.chats.clear();
            s.history.clear();
            s.me.clear();
        });
    }

    // ---- chats -----------------------------------------------------------------------

    fn start_chats(&mut self) -> Result<()> {
        let me = Obj::new(
            "users.getUsers",
            &[(
                "id",
                Value::Vector(vec![Obj::new("inputUserSelf", &[]).into()]),
            )],
        );
        let users = self.call(&me)?;
        if let Some(u) = users.as_vec().first().and_then(|u| u.as_obj()) {
            self.remember_user(u);
        }
        self.me = self.saved.user;
        let name = self.users.get(&self.me).map(|u| u.name.clone());
        self.update(|s| {
            s.me = name.unwrap_or_default();
            s.stage = Stage::Ready;
        });
        self.online_at = 0;
        self.pinned_dirty.clear();
        self.load_dialogs()?;
        // from now on the server pushes new messages to this session
        self.call(&Obj::new("updates.getState", &[]))?;
        log("ready");
        Ok(())
    }

    fn command(&mut self, cmd: Cmd) -> Result<Option<Stop>> {
        match cmd {
            Cmd::Open(peer) => {
                let loaded = self.shared.borrow().history.contains_key(&peer);
                if !loaded {
                    self.load_history(peer, 0)?;
                }
                self.mark_read(peer)?;
                self.load_pinned(peer)?;
            }
            Cmd::Older(peer) => {
                let oldest = {
                    let s = self.shared.borrow();
                    let h = s.history.get(&peer);
                    if h.is_some_and(|h| h.complete || h.loading) {
                        return Ok(None);
                    }
                    h.and_then(|h| h.messages.iter().find(|m| m.id != 0))
                        .map_or(0, |m| m.id)
                };
                self.load_history(peer, oldest)?;
            }
            Cmd::Send(peer, text, reply_to) => self.send_message(peer, text, reply_to)?,
            Cmd::Edit(peer, id, text) => self.edit(peer, id, text)?,
            Cmd::Delete(peer, ids, revoke) => self.delete(peer, ids, revoke)?,
            Cmd::Forward(from, ids, to) => self.forward(from, ids, to)?,
            Cmd::Pin(peer, id, pin) => self.pin(peer, id, pin)?,
            Cmd::Pinned(peer) => self.load_pinned(peer)?,
            Cmd::Quote(peer, ids) => self.quote(peer, ids)?,
            Cmd::Typing(peer) => {
                let req = Obj::new(
                    "messages.setTyping",
                    &[
                        ("peer", self.input_peer(peer).into()),
                        ("action", Obj::new("sendMessageTypingAction", &[]).into()),
                    ],
                );
                // not worth an error on the screen
                match self.call(&req) {
                    Err(Error::Rpc { .. }) | Ok(_) => {}
                    Err(e) => return Err(e),
                }
            }
            Cmd::Callback(peer, id, data) => self.callback(peer, id, data)?,
            Cmd::SendFile(peer, path, reply_to) => match self.send_file(peer, path, reply_to) {
                // a file that can't be read is no reason to reconnect
                Err(Error::Other(e)) => self.notice(e),
                other => other?,
            },
            Cmd::Preview(peer, id) => self.preview(peer, id)?,
            Cmd::Download(peer, id, open) => self.download(peer, id, open)?,
            Cmd::Search(q) => self.search(q)?,
            Cmd::Show(peer) => self.show(peer),
            Cmd::Resolve(link) => self.resolve(&link)?,
            Cmd::Join(peer) => self.join(peer, true)?,
            Cmd::Leave(peer) => self.join(peer, false)?,
            Cmd::Info(peer) => self.info(peer)?,
            Cmd::Reload => {
                self.load_dialogs()?;
                let open: Vec<Peer> = self.shared.borrow().history.keys().copied().collect();
                self.update(|s| s.history.clear());
                for peer in open {
                    self.load_history(peer, 0)?;
                }
            }
            Cmd::LogOut => {
                let _ = self.call(&Obj::new("auth.logOut", &[]));
                self.sign_out_locally();
                // a fresh key for the next account
                self.saved = Saved::default();
                save_session(&self.saved);
                return Ok(Some(Stop::Restart));
            }
            _ => {}
        }
        Ok(None)
    }

    // ---- files ------------------------------------------------------------------------

    /// Call a method at data centre `dc`: through our main connection when
    /// it is that one, otherwise through one we sign in to with an
    /// exported authorization. Trouble there doesn't break the main one.
    fn call_dc(&mut self, dc: i32, req: &Obj) -> Result<Value> {
        if dc == 0 || dc == self.saved.dc {
            return self.call(req);
        }
        if !self.media.contains_key(&dc) {
            log(&format!("connecting to DC {} for its files", dc));
            let (mut conn, _) =
                Conn::open(&self.cfg, dc, None).map_err(|e| Error::Other(e.text()))?;
            let exported = self.call(&Obj::new(
                "auth.exportAuthorization",
                &[("dc_id", Value::Int(dc))],
            ))?;
            let exported = exported
                .as_obj()
                .cloned()
                .ok_or("bad exported authorization")?;
            conn.invoke(
                &self.cfg,
                &Obj::new(
                    "auth.importAuthorization",
                    &[
                        ("id", Value::Long(exported.int("id"))),
                        ("bytes", Value::Bytes(exported.bytes("bytes").to_vec())),
                    ],
                ),
            )
            .map_err(|e| Error::Other(e.text()))?;
            self.media.insert(dc, conn);
        }
        let conn = self.media.get_mut(&dc).ok_or("no connection")?;
        let result = conn.invoke(&self.cfg, req);
        conn.updates.clear();
        match result {
            Err(e @ Error::Rpc { .. }) => Err(e),
            Err(e) => {
                self.media.remove(&dc);
                Err(Error::Other(e.text()))
            }
            ok => ok,
        }
    }

    /// Serve the main connection while a long download runs elsewhere.
    fn keep_alive(&mut self) -> Result<()> {
        if let Some(conn) = &mut self.conn {
            conn.poll()?;
            let updates = core::mem::take(&mut conn.updates);
            for u in updates {
                self.apply(&u);
            }
        }
        Ok(())
    }

    /// Download a file, or one size (`thumb`) of a photo or a file's
    /// preview. With `progress`, the download of that message counts up.
    fn get_file(
        &mut self,
        f: &FileRef,
        thumb: &str,
        progress: Option<(Peer, i64)>,
    ) -> Result<Vec<u8>> {
        let location = Obj::new(
            if f.photo {
                "inputPhotoFileLocation"
            } else {
                "inputDocumentFileLocation"
            },
            &[
                ("id", Value::Long(f.id)),
                ("access_hash", Value::Long(f.access_hash)),
                ("file_reference", Value::Bytes(f.file_reference.clone())),
                ("thumb_size", Value::str(thumb)),
            ],
        );
        let mut dc = f.dc;
        let mut data: Vec<u8> = Vec::new();
        loop {
            let req = Obj::new(
                "upload.getFile",
                &[
                    ("location", location.clone().into()),
                    ("offset", Value::Long(data.len() as i64)),
                    ("limit", Value::Int(DOWNLOAD_PART as i32)),
                ],
            );
            let v = match self.call_dc(dc, &req) {
                Err(e) if e.number_after("FILE_MIGRATE_").is_some() => {
                    dc = e.number_after("FILE_MIGRATE_").unwrap_or(2) as i32;
                    continue;
                }
                other => other?,
            };
            let part = v
                .as_obj()
                .map(|o| o.bytes("bytes").to_vec())
                .unwrap_or_default();
            let last = part.len() < DOWNLOAD_PART;
            data.extend_from_slice(&part);
            if data.len() > MAX_DOWNLOAD {
                return Err(Error::Other(String::from(
                    "the file is too big (over 256 MB)",
                )));
            }
            if let Some(key) = progress {
                let done = data.len() as i64;
                self.update(|s| {
                    if let Some(d) = s.downloads.get_mut(&key) {
                        d.done = done;
                    }
                });
            }
            if last {
                return Ok(data);
            }
            self.keep_alive()?;
        }
    }

    /// Get a message again, for a fresh file_reference (they expire).
    fn refresh(&mut self, peer: Peer, id: i64) -> Result<Option<Message>> {
        let ids = Value::Vector(alloc::vec![Obj::new(
            "inputMessageID",
            &[("id", Value::Int(id as i32))]
        )
        .into()]);
        let req = match peer {
            Peer::Channel(_) => Obj::new(
                "channels.getMessages",
                &[("channel", self.input_channel(peer).into()), ("id", ids)],
            ),
            _ => Obj::new("messages.getMessages", &[("id", ids)]),
        };
        let v = self.call(&req)?;
        let o = v.as_obj().ok_or("bad messages")?;
        self.remember_all(o);
        let fresh = o
            .vec("messages")
            .iter()
            .filter_map(|m| m.as_obj())
            .find_map(|m| self.convert(m));
        if let Some(m) = &fresh {
            let m = m.clone();
            self.update(|s| {
                if let Some(h) = s.history.get_mut(&peer) {
                    if let Some(old) = h.messages.iter_mut().find(|x| x.id == id) {
                        *old = m;
                    }
                }
            });
        }
        Ok(fresh)
    }

    fn message(&self, peer: Peer, id: i64) -> Option<Message> {
        let s = self.shared.borrow();
        s.history
            .get(&peer)?
            .messages
            .iter()
            .find(|m| m.id == id)
            .cloned()
    }

    /// What to download for a message: the file, the size, and a name.
    fn what(m: &Message, preview: bool) -> Option<(FileRef, String, String, i64)> {
        if let Some(p) = &m.photo {
            let size = if preview { &p.small } else { &p.big };
            return Some((
                p.file.clone(),
                size.clone(),
                format!("photo_{}.jpg", p.file.id as u64 % 1_000_000),
                p.big_size,
            ));
        }
        let f = m.file.as_ref()?;
        if preview {
            let (t, _, _) = f.thumb.clone()?;
            return Some((f.file.clone(), t, f.name.clone(), 0));
        }
        Some((f.file.clone(), String::new(), f.name.clone(), f.size))
    }

    /// Get a file, asking for the message again once if its reference
    /// has run out.
    fn fetch(
        &mut self,
        peer: Peer,
        id: i64,
        preview: bool,
        progress: Option<(Peer, i64)>,
    ) -> Result<Vec<u8>> {
        let m = self.message(peer, id).ok_or("the message is gone")?;
        let (file, thumb, _, _) = Self::what(&m, preview).ok_or("nothing to download")?;
        match self.get_file(&file, &thumb, progress) {
            Err(e) if matches!(&e, Error::Rpc { message, .. } if message.starts_with("FILE_REFERENCE_")) =>
            {
                let m = self.refresh(peer, id)?.ok_or("the message is gone")?;
                let (file, thumb, _, _) = Self::what(&m, preview).ok_or("nothing to download")?;
                self.get_file(&file, &thumb, progress)
            }
            other => other,
        }
    }

    /// Load the picture of a message for the chat.
    fn preview(&mut self, peer: Peer, id: i64) -> Result<()> {
        let result = match self.fetch(peer, id, true, None) {
            Ok(data) => crate::web::image::decode(&data)
                .map(|img| shrink(img, PREVIEW_MAX, PREVIEW_MAX))
                .ok_or(Error::Other(String::from("can't read the picture"))),
            Err(e) => Err(e),
        };
        let state = match result {
            Ok(img) => Preview::Ready(img),
            Err(e @ Error::Net(_)) | Err(e @ Error::KeyUnknown) => {
                self.update(|s| {
                    s.previews.remove(&(peer, id));
                });
                return Err(e);
            }
            Err(e) => {
                log(&format!("no preview for message {}: {}", id, e.text()));
                Preview::Failed
            }
        };
        self.update(|s| {
            s.previews.insert((peer, id), state);
        });
        Ok(())
    }

    /// Download a message's photo or file to Downloads.
    fn download(&mut self, peer: Peer, id: i64, open: bool) -> Result<()> {
        let key = (peer, id);
        let there = self
            .shared
            .borrow()
            .downloads
            .get(&key)
            .and_then(|d| d.path.clone());
        if let Some(path) = there {
            if open && fs::exists(&path) {
                self.update(|s| s.to_open.push(path));
                return Ok(());
            }
        }
        let Some(m) = self.message(peer, id) else {
            return Ok(());
        };
        let Some((_, _, name, total)) = Self::what(&m, false) else {
            return Ok(());
        };
        self.update(|s| {
            s.downloads.insert(
                key,
                Download {
                    total,
                    ..Default::default()
                },
            );
        });
        log(&format!("downloading {}", name));
        let result = self
            .fetch(peer, id, false, Some(key))
            .and_then(|data| save_download(&name, &data).map_err(Error::Other));
        match result {
            Ok(path) => {
                log(&format!("saved {}", path));
                self.update(|s| {
                    if let Some(d) = s.downloads.get_mut(&key) {
                        d.done = d.total.max(d.done);
                        d.path = Some(path.clone());
                    }
                    if open {
                        s.to_open.push(path);
                    }
                });
                Ok(())
            }
            Err(e) => {
                let text = e.text();
                self.update(|s| {
                    if let Some(d) = s.downloads.get_mut(&key) {
                        d.failed = Some(text);
                    }
                });
                match e {
                    Error::Net(_) | Error::KeyUnknown => Err(e),
                    _ => Ok(()),
                }
            }
        }
    }

    // ---- finding chats ----------------------------------------------------------------

    /// Look for people, groups and channels by name or @name.
    fn search(&mut self, q: String) -> Result<()> {
        let req = Obj::new(
            "contacts.search",
            &[("q", Value::str(&q)), ("limit", Value::Int(30))],
        );
        let found = match self.call(&req) {
            Ok(v) => {
                let o = v.as_obj().ok_or("bad search answer")?.clone();
                self.remember_all(&o);
                let mut peers: Vec<Peer> = Vec::new();
                for p in o.vec("my_results").iter().chain(o.vec("results")) {
                    if let Some(peer) = peer_of(p.as_obj()) {
                        if !peers.contains(&peer) {
                            peers.push(peer);
                        }
                    }
                }
                peers.into_iter().map(|p| self.new_chat(p)).collect()
            }
            Err(Error::Rpc { .. }) => Vec::new(),
            Err(e) => return Err(e),
        };
        self.update(|s| s.found = Some((q, found)));
        Ok(())
    }

    /// Put a chat in the list if it isn't there, and have the window open it.
    fn show(&mut self, peer: Peer) {
        let chat = self.new_chat(peer);
        self.update(|s| {
            if s.chat(peer).is_none() {
                // after the pinned chats
                let at = s.chats.iter().take_while(|c| c.pinned).count();
                s.chats.insert(at, chat);
            }
            s.goto = Some(peer);
        });
    }

    /// Open @name, t.me/name or tg://resolve?domain=name.
    fn resolve(&mut self, link: &str) -> Result<()> {
        let l = link.trim();
        let l = l
            .strip_prefix("https://")
            .or_else(|| l.strip_prefix("http://"))
            .unwrap_or(l);
        let name = if let Some(rest) = l.strip_prefix("tg://resolve?domain=") {
            rest.split('&').next().unwrap_or("")
        } else if let Some(rest) = l
            .strip_prefix("t.me/")
            .or_else(|| l.strip_prefix("telegram.me/"))
            .or_else(|| l.strip_prefix("telegram.dog/"))
        {
            rest.trim_start_matches("s/")
                .split(['/', '?'])
                .next()
                .unwrap_or("")
        } else {
            l.trim_start_matches('@')
        };
        if name.is_empty() || name.starts_with('+') || name == "joinchat" {
            self.notice("RyzikOS can't open invite links yet");
            return Ok(());
        }
        if let Some(id) = link
            .strip_prefix("tg://user?id=")
            .and_then(|v| v.parse::<i64>().ok())
        {
            self.show(Peer::User(id));
            return Ok(());
        }
        log(&format!("looking up @{}", name));
        let req = Obj::new(
            "contacts.resolveUsername",
            &[("username", Value::str(name))],
        );
        match self.call(&req) {
            Ok(v) => {
                let o = v.as_obj().ok_or("bad answer")?.clone();
                self.remember_all(&o);
                if let Some(peer) = peer_of(o.obj("peer")) {
                    self.show(peer);
                }
            }
            Err(e @ Error::Rpc { .. }) => {
                self.notice(
                    if e.is("USERNAME_NOT_OCCUPIED") || e.is("USERNAME_INVALID") {
                        format!("Nobody on Telegram is called @{}", name)
                    } else {
                        friendly(&e)
                    },
                );
            }
            Err(e) => return Err(e),
        }
        Ok(())
    }

    /// Join a channel or group we found, or leave one.
    fn join(&mut self, peer: Peer, join: bool) -> Result<()> {
        if !matches!(peer, Peer::Channel(_)) {
            return Ok(());
        }
        let req = Obj::new(
            if join {
                "channels.joinChannel"
            } else {
                "channels.leaveChannel"
            },
            &[("channel", self.input_channel(peer).into())],
        );
        match self.call(&req) {
            Ok(v) => {
                if let Some(o) = v.as_obj() {
                    let o = o.clone();
                    self.apply(&o);
                }
                if let Peer::Channel(id) = peer {
                    if let Some(c) = self.chats.get_mut(&id) {
                        c.left = !join;
                    }
                }
                self.update(|s| {
                    if let Some(c) = s.chats.iter_mut().find(|c| c.peer == peer) {
                        c.left = !join;
                    }
                });
            }
            Err(e @ Error::Rpc { .. }) => self.notice(friendly(&e)),
            Err(e) => return Err(e),
        }
        Ok(())
    }

    /// Load what the profile of a chat shows.
    fn info(&mut self, peer: Peer) -> Result<()> {
        let req = match peer {
            Peer::User(id) => Obj::new(
                "users.getFullUser",
                &[(
                    "id",
                    if id == self.me {
                        Obj::new("inputUserSelf", &[])
                    } else {
                        Obj::new(
                            "inputUser",
                            &[
                                ("user_id", Value::Long(id)),
                                (
                                    "access_hash",
                                    Value::Long(self.users.get(&id).map_or(0, |u| u.access_hash)),
                                ),
                            ],
                        )
                    }
                    .into(),
                )],
            ),
            Peer::Chat(id) => Obj::new("messages.getFullChat", &[("chat_id", Value::Long(id))]),
            Peer::Channel(_) => Obj::new(
                "channels.getFullChannel",
                &[("channel", self.input_channel(peer).into())],
            ),
        };
        let mut info = Info::default();
        if let Peer::User(id) = peer {
            info.phone = self
                .users
                .get(&id)
                .map_or(String::new(), |u| u.phone.clone());
        }
        match self.call(&req) {
            Ok(v) => {
                let o = v.as_obj().ok_or("bad answer")?.clone();
                self.remember_all(&o);
                if let Some(u) = o.obj("full_user") {
                    info.about = u.string("about");
                }
                if let Some(c) = o.obj("full_chat") {
                    info.about = c.string("about");
                    info.members = c.int("participants_count");
                    if info.members == 0 {
                        info.members = c
                            .obj("participants")
                            .map_or(0, |p| p.vec("participants").len() as i64);
                    }
                }
            }
            Err(Error::Rpc { .. }) => {}
            Err(e) => return Err(e),
        }
        // a name learned on the way
        let chat = self.new_chat(peer);
        self.update(|s| {
            if let Some(c) = s.chats.iter_mut().find(|c| c.peer == peer) {
                if c.username.is_empty() {
                    c.username = chat.username.clone();
                }
            }
            s.info.insert(peer, info);
        });
        Ok(())
    }

    fn input_peer(&self, peer: Peer) -> Obj {
        match peer {
            Peer::User(id) if id == self.me => Obj::new("inputPeerSelf", &[]),
            Peer::User(id) => Obj::new(
                "inputPeerUser",
                &[
                    ("user_id", Value::Long(id)),
                    (
                        "access_hash",
                        Value::Long(self.users.get(&id).map_or(0, |u| u.access_hash)),
                    ),
                ],
            ),
            Peer::Chat(id) => Obj::new("inputPeerChat", &[("chat_id", Value::Long(id))]),
            Peer::Channel(id) => Obj::new(
                "inputPeerChannel",
                &[
                    ("channel_id", Value::Long(id)),
                    (
                        "access_hash",
                        Value::Long(self.chats.get(&id).map_or(0, |c| c.access_hash)),
                    ),
                ],
            ),
        }
    }

    fn load_dialogs(&mut self) -> Result<()> {
        let req = Obj::new(
            "messages.getDialogs",
            &[
                ("offset_peer", Obj::new("inputPeerEmpty", &[]).into()),
                ("limit", Value::Int(100)),
            ],
        );
        let v = self.call(&req)?;
        let d = v.as_obj().ok_or("bad dialogs")?;
        self.remember_all(d);
        let mut tops: BTreeMap<(Peer, i64), Message> = BTreeMap::new();
        for m in d.vec("messages").iter().filter_map(|m| m.as_obj()) {
            if let (Some(peer), Some(msg)) = (peer_of(m.obj("peer_id")), self.convert(m)) {
                tops.insert((peer, msg.id), msg);
            }
        }
        let mut chats = Vec::new();
        for dialog in d.vec("dialogs").iter().filter_map(|x| x.as_obj()) {
            if !dialog.is("dialog") {
                continue; // folders
            }
            let Some(peer) = peer_of(dialog.obj("peer")) else {
                continue;
            };
            let top = tops.get(&(peer, dialog.int("top_message")));
            let mut chat = self.new_chat(peer);
            chat.unread = dialog.int("unread_count");
            chat.pinned = dialog.flag("pinned");
            chat.read_out = dialog.int("read_outbox_max_id");
            if let Some(m) = top {
                chat.last = preview(m);
                chat.last_out = m.out;
                chat.date = m.date;
            }
            chats.push(chat);
        }
        // pinned chats on top, in the server's order otherwise
        chats.sort_by_key(|c| !c.pinned);
        let n = chats.len();
        self.update(|s| s.chats = chats);
        log(&format!("{} chats", n));
        Ok(())
    }

    fn new_chat(&self, peer: Peer) -> Chat {
        let (title, kind) = match peer {
            Peer::User(id) if id == self.me => (String::from("Saved Messages"), ChatKind::Saved),
            Peer::User(id) => match self.users.get(&id) {
                Some(u) => (
                    u.name.clone(),
                    if u.bot {
                        ChatKind::Bot
                    } else {
                        ChatKind::Private
                    },
                ),
                None => (String::from("Unknown user"), ChatKind::Private),
            },
            Peer::Chat(id) | Peer::Channel(id) => match self.chats.get(&id) {
                Some(c) => (
                    c.title.clone(),
                    if c.broadcast {
                        ChatKind::Channel
                    } else {
                        ChatKind::Group
                    },
                ),
                None => (String::from("Unknown chat"), ChatKind::Group),
            },
        };
        let (username, left) = match peer {
            Peer::User(id) => (
                self.users
                    .get(&id)
                    .map_or(String::new(), |u| u.username.clone()),
                false,
            ),
            Peer::Chat(id) | Peer::Channel(id) => self
                .chats
                .get(&id)
                .map_or((String::new(), false), |c| (c.username.clone(), c.left)),
        };
        Chat {
            peer,
            title,
            last: String::new(),
            last_out: false,
            date: 0,
            unread: 0,
            pinned: false,
            read_out: 0,
            kind,
            username,
            left,
        }
    }

    /// Load a page of messages older than `before` (0: the newest).
    fn load_history(&mut self, peer: Peer, before: i64) -> Result<()> {
        self.update(|s| s.history.entry(peer).or_default().loading = true);
        let req = Obj::new(
            "messages.getHistory",
            &[
                ("peer", self.input_peer(peer).into()),
                ("offset_id", Value::Int(before as i32)),
                ("limit", Value::Int(HISTORY_PAGE)),
            ],
        );
        let result = self.call(&req);
        let v = match result {
            Ok(v) => v,
            Err(e) => {
                self.update(|s| s.history.entry(peer).or_default().loading = false);
                return Err(e);
            }
        };
        let m = v.as_obj().ok_or("bad history")?;
        self.remember_all(m);
        let mut page: Vec<Message> = m
            .vec("messages")
            .iter()
            .filter_map(|x| x.as_obj())
            .filter_map(|x| self.convert(x))
            .collect();
        page.reverse();
        let complete = (page.len() as i32) < HISTORY_PAGE || m.is("messages.messages");
        self.update(|s| {
            let h = s.history.entry(peer).or_default();
            h.loading = false;
            h.complete = complete;
            if before == 0 {
                // keep messages still being sent
                let pending: Vec<Message> = h.messages.drain(..).filter(|m| m.id == 0).collect();
                h.messages = page;
                h.messages.extend(pending);
            } else {
                page.retain(|m| m.id < before);
                page.append(&mut h.messages);
                h.messages = page;
            }
            // a chat found by the search or a link: its last message
            let newest = h.messages.iter().rev().find(|m| m.id != 0).cloned();
            if let (Some(c), Some(m)) = (s.chats.iter_mut().find(|c| c.peer == peer), newest) {
                if c.date == 0 {
                    c.last = preview(&m);
                    c.last_out = m.out;
                    c.date = m.date;
                }
            }
        });
        Ok(())
    }

    fn mark_read(&mut self, peer: Peer) -> Result<()> {
        let (unread, max_id) = {
            let s = self.shared.borrow();
            let unread = s.chat(peer).map_or(0, |c| c.unread);
            let max = s
                .history
                .get(&peer)
                .and_then(|h| h.messages.iter().rev().find(|m| m.id != 0))
                .map_or(0, |m| m.id);
            (unread, max)
        };
        if unread == 0 || max_id == 0 {
            return Ok(());
        }
        let req = match peer {
            Peer::Channel(_) => Obj::new(
                "channels.readHistory",
                &[
                    ("channel", self.input_channel(peer).into()),
                    ("max_id", Value::Int(max_id as i32)),
                ],
            ),
            _ => Obj::new(
                "messages.readHistory",
                &[
                    ("peer", self.input_peer(peer).into()),
                    ("max_id", Value::Int(max_id as i32)),
                ],
            ),
        };
        self.call(&req)?;
        self.update(|s| {
            if let Some(c) = s.chats.iter_mut().find(|c| c.peer == peer) {
                c.unread = 0;
            }
        });
        Ok(())
    }

    fn input_channel(&self, peer: Peer) -> Obj {
        let (id, hash) = match peer {
            Peer::Channel(id) => (id, self.chats.get(&id).map_or(0, |c| c.access_hash)),
            _ => (0, 0),
        };
        Obj::new(
            "inputChannel",
            &[
                ("channel_id", Value::Long(id)),
                ("access_hash", Value::Long(hash)),
            ],
        )
    }

    fn send_message(&mut self, peer: Peer, text: String, reply_to: i64) -> Result<()> {
        let random_id = crypto::random_i64();
        let date = now_ms() / 1000 + self.conn.as_ref().map_or(0, |c| c.s.time_offset);
        let me = self.me;
        let local = Message {
            id: 0,
            out: true,
            from: String::new(),
            from_id: me,
            text: text.clone(),
            media: None,
            service: false,
            date,
            edited: false,
            random_id,
            failed: false,
            reply_to,
            ..Default::default()
        };
        self.sending.insert(random_id, peer);
        self.update(|s| {
            s.history
                .entry(peer)
                .or_default()
                .messages
                .push(local.clone());
            s.typing.remove(&peer);
        });
        self.bump_chat(peer, &local);
        let req = Obj::new(
            "messages.sendMessage",
            &[
                ("peer", self.input_peer(peer).into()),
                ("reply_to", reply_header(reply_to)),
                ("message", Value::str(&text)),
                ("random_id", Value::Long(random_id)),
            ],
        );
        match self.call(&req) {
            Ok(v) => {
                if let Some(o) = v.as_obj() {
                    if o.is("updateShortSentMessage") {
                        self.sent(random_id, o.int("id"), o.int("date"));
                    } else {
                        self.apply(o);
                    }
                }
                Ok(())
            }
            Err(e) => {
                self.sending.remove(&random_id);
                self.update(|s| {
                    if let Some(m) = s
                        .history
                        .get_mut(&peer)
                        .and_then(|h| h.messages.iter_mut().find(|m| m.random_id == random_id))
                    {
                        m.failed = true;
                    }
                });
                Err(e)
            }
        }
    }

    // ---- changing messages -----------------------------------------------------------

    fn edit(&mut self, peer: Peer, id: i64, text: String) -> Result<()> {
        let req = Obj::new(
            "messages.editMessage",
            &[
                ("peer", self.input_peer(peer).into()),
                ("id", Value::Int(id as i32)),
                ("message", Value::str(&text)),
            ],
        );
        match self.call(&req) {
            Ok(v) => {
                if let Some(o) = v.as_obj() {
                    self.apply(o);
                }
            }
            // the same text as before
            Err(Error::Rpc { message, .. }) if message == "MESSAGE_NOT_MODIFIED" => {}
            Err(e @ Error::Rpc { .. }) => {
                self.notice(format!("Can't edit the message: {}", friendly(&e)));
                return Ok(());
            }
            Err(e) => return Err(e),
        }
        // the server's copy comes with the update; until then ours
        self.change(peer, id, |m| {
            if m.text != text {
                m.text = text.clone();
                m.links.clear();
            }
            m.edited = true;
        });
        Ok(())
    }

    /// Change a message wherever it is shown.
    fn change(&self, peer: Peer, id: i64, f: impl Fn(&mut Message)) {
        self.update(|s| {
            if let Some(m) = s
                .history
                .get_mut(&peer)
                .and_then(|h| h.messages.iter_mut().find(|m| m.id == id))
            {
                f(m);
            }
            if let Some(m) = s.quoted.get_mut(&(peer, id)) {
                f(m);
            }
            if let Some(m) = s
                .pinned
                .get_mut(&peer)
                .and_then(|v| v.iter_mut().find(|m| m.id == id))
            {
                f(m);
            }
        });
    }

    fn delete(&mut self, peer: Peer, ids: Vec<i64>, revoke: bool) -> Result<()> {
        let list = Value::Vector(ids.iter().map(|&i| Value::Int(i as i32)).collect());
        let req = match peer {
            Peer::Channel(_) => Obj::new(
                "channels.deleteMessages",
                &[("channel", self.input_channel(peer).into()), ("id", list)],
            ),
            _ => Obj::new(
                "messages.deleteMessages",
                &[("revoke", Value::Bool(revoke)), ("id", list)],
            ),
        };
        match self.call(&req) {
            Ok(_) => {}
            Err(e @ Error::Rpc { .. }) => {
                self.notice(format!("Can't delete: {}", friendly(&e)));
                return Ok(());
            }
            Err(e) => return Err(e),
        }
        self.deleted(Some(peer), None, &ids);
        Ok(())
    }

    /// Messages are gone: from one chat, from a channel, or (`None`,
    /// `None`) from any chat that isn't a channel.
    fn deleted(&self, peer: Option<Peer>, channel: Option<i64>, ids: &[i64]) {
        let hit = |p: &Peer| match (peer, channel) {
            (Some(x), _) => *p == x,
            (None, Some(c)) => *p == Peer::Channel(c),
            (None, None) => !matches!(p, Peer::Channel(_)),
        };
        self.update(|s| {
            let mut touched = Vec::new();
            for (p, h) in s.history.iter_mut() {
                if hit(p) {
                    let before = h.messages.len();
                    h.messages.retain(|m| m.id == 0 || !ids.contains(&m.id));
                    if h.messages.len() != before {
                        touched.push((*p, h.messages.iter().rev().find(|m| m.id != 0).cloned()));
                    }
                }
            }
            // answers to them now quote "Deleted message"
            for (p, _) in &touched {
                for &id in ids {
                    s.quoted.insert((*p, id), gone(id));
                }
            }
            for (p, v) in s.pinned.iter_mut() {
                if hit(p) {
                    v.retain(|m| !ids.contains(&m.id));
                }
            }
            for (key, m) in s.quoted.iter_mut() {
                if hit(&key.0) && ids.contains(&key.1) {
                    *m = gone(key.1);
                }
            }
            // the chat list shows what is now the last message
            for (p, last) in touched {
                if let Some(c) = s.chats.iter_mut().find(|c| c.peer == p) {
                    match last {
                        Some(m) => {
                            c.last = preview(&m);
                            c.last_out = m.out;
                        }
                        None => c.last.clear(),
                    }
                }
            }
        });
    }

    fn forward(&mut self, from: Peer, ids: Vec<i64>, to: Peer) -> Result<()> {
        let req = Obj::new(
            "messages.forwardMessages",
            &[
                ("from_peer", self.input_peer(from).into()),
                (
                    "id",
                    Value::Vector(ids.iter().map(|&i| Value::Int(i as i32)).collect()),
                ),
                (
                    "random_id",
                    Value::Vector(ids.iter().map(|_| Value::Long(crypto::random_i64())).collect()),
                ),
                ("to_peer", self.input_peer(to).into()),
            ],
        );
        match self.call(&req) {
            Ok(v) => {
                if let Some(o) = v.as_obj() {
                    self.apply(o);
                }
            }
            Err(e @ Error::Rpc { .. }) => {
                self.notice(format!("Can't forward: {}", friendly(&e)));
                return Ok(());
            }
            Err(e) => return Err(e),
        }
        let n = ids.len();
        self.update(|s| {
            s.goto = Some(to);
            s.notice = Some(String::from(if n == 1 {
                "Message forwarded"
            } else {
                "Messages forwarded"
            }));
        });
        Ok(())
    }

    fn pin(&mut self, peer: Peer, id: i64, pin: bool) -> Result<()> {
        let req = Obj::new(
            "messages.updatePinnedMessage",
            &[
                ("unpin", Value::Bool(!pin)),
                ("peer", self.input_peer(peer).into()),
                ("id", Value::Int(id as i32)),
            ],
        );
        match self.call(&req) {
            Ok(v) => {
                if let Some(o) = v.as_obj() {
                    self.apply(o);
                }
            }
            Err(e @ Error::Rpc { .. }) => {
                let what = if pin { "pin" } else { "unpin" };
                self.notice(format!("Can't {}: {}", what, friendly(&e)));
                return Ok(());
            }
            Err(e) => return Err(e),
        }
        self.change(peer, id, |m| m.pinned = pin);
        self.load_pinned(peer)
    }

    /// The pinned messages of a chat, newest first.
    fn load_pinned(&mut self, peer: Peer) -> Result<()> {
        let req = Obj::new(
            "messages.search",
            &[
                ("peer", self.input_peer(peer).into()),
                ("q", Value::str("")),
                ("filter", Obj::new("inputMessagesFilterPinned", &[]).into()),
                ("limit", Value::Int(50)),
            ],
        );
        let v = match self.call(&req) {
            Ok(v) => v,
            // a chat that can't be searched has nothing pinned to show
            Err(Error::Rpc { .. }) => return Ok(()),
            Err(e) => return Err(e),
        };
        let o = v.as_obj().ok_or("bad pinned messages")?;
        self.remember_all(o);
        let mut list: Vec<Message> = o
            .vec("messages")
            .iter()
            .filter_map(|m| m.as_obj())
            .filter_map(|m| self.convert(m))
            .collect();
        list.sort_by_key(|m| -m.id);
        self.update(|s| {
            if let Some(h) = s.history.get_mut(&peer) {
                for m in h.messages.iter_mut() {
                    m.pinned = list.iter().any(|p| p.id == m.id);
                }
            }
            s.pinned.insert(peer, list);
        });
        Ok(())
    }

    /// Load messages that others answer, for what they quote.
    fn quote(&mut self, peer: Peer, ids: Vec<i64>) -> Result<()> {
        let list = Value::Vector(
            ids.iter()
                .map(|&i| Obj::new("inputMessageID", &[("id", Value::Int(i as i32))]).into())
                .collect(),
        );
        let req = match peer {
            Peer::Channel(_) => Obj::new(
                "channels.getMessages",
                &[("channel", self.input_channel(peer).into()), ("id", list)],
            ),
            _ => Obj::new("messages.getMessages", &[("id", list)]),
        };
        let v = match self.call(&req) {
            Ok(v) => v,
            Err(Error::Rpc { .. }) => Value::None,
            Err(e) => return Err(e),
        };
        let mut found: BTreeMap<i64, Message> = BTreeMap::new();
        if let Some(o) = v.as_obj() {
            self.remember_all(o);
            for m in o.vec("messages").iter().filter_map(|m| m.as_obj()) {
                if let Some(msg) = self.convert(m) {
                    found.insert(msg.id, msg);
                }
            }
        }
        self.update(|s| {
            for id in ids {
                let m = found.remove(&id).unwrap_or_else(|| gone(id));
                s.quoted.insert((peer, id), m);
            }
        });
        Ok(())
    }

    /// An inline button with data for the bot was pressed: the bot
    /// answers with a note or a link.
    fn callback(&mut self, peer: Peer, id: i64, data: Vec<u8>) -> Result<()> {
        let req = Obj::new(
            "messages.getBotCallbackAnswer",
            &[
                ("peer", self.input_peer(peer).into()),
                ("msg_id", Value::Int(id as i32)),
                ("data", Value::Bytes(data)),
            ],
        );
        match self.call(&req) {
            Ok(v) => {
                if let Some(a) = v.as_obj() {
                    let text = a.string("message");
                    let url = a.string("url");
                    self.update(|s| {
                        if !text.is_empty() {
                            s.notice = Some(text);
                        }
                        if !url.is_empty() {
                            s.follow = Some(url);
                        }
                    });
                }
                Ok(())
            }
            // bots that don't answer in time are common
            Err(Error::Rpc { message, .. }) if message.contains("TIMEOUT") => Ok(()),
            Err(e @ Error::Rpc { .. }) => {
                self.notice(format!("The bot didn't answer: {}", friendly(&e)));
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// Forget who stopped typing a while ago.
    fn forget_typing(&self) {
        let now = now_ms();
        let stale = self
            .shared
            .borrow()
            .typing
            .values()
            .any(|v| v.iter().any(|t| t.2 <= now));
        if stale {
            self.update(|s| {
                for v in s.typing.values_mut() {
                    v.retain(|t| t.2 > now);
                }
                s.typing.retain(|_, v| !v.is_empty());
            });
        }
    }

    /// Upload a file in parts, then send it: pictures as photos,
    /// everything else as a file.
    fn send_file(&mut self, peer: Peer, path: String, reply_to: i64) -> Result<()> {
        let data = fs::read(&path).map_err(|e| {
            Error::Other(format!(
                "Can't read {}: {}",
                fs::display(&path),
                e.message()
            ))
        })?;
        if data.len() > MAX_UPLOAD {
            return Err(Error::Other(String::from(
                "Files up to 64 MB can be sent from RyzikOS.",
            )));
        }
        let name = String::from(fs::file_name(&path));
        let mime = mime_type(&name);
        let photo = matches!(mime, "image/png" | "image/jpeg") && data.len() <= 10 << 20;
        let random_id = crypto::random_i64();
        let date = now_ms() / 1000 + self.conn.as_ref().map_or(0, |c| c.s.time_offset);
        let label = |done: usize| {
            let kind = if photo { "Photo" } else { "File" };
            if done >= 100 {
                format!("{}: {}", kind, name)
            } else {
                format!("{}: {} (sending, {}%)", kind, name, done)
            }
        };
        let local = Message {
            id: 0,
            out: true,
            from: String::new(),
            from_id: self.me,
            text: String::new(),
            media: Some(label(0)),
            service: false,
            date,
            edited: false,
            random_id,
            failed: false,
            reply_to,
            ..Default::default()
        };
        self.sending.insert(random_id, peer);
        self.update(|s| {
            s.history
                .entry(peer)
                .or_default()
                .messages
                .push(local.clone());
        });
        self.bump_chat(peer, &local);
        let result = self.upload_and_send(
            peer, &data, &name, mime, photo, random_id, reply_to, &label,
        );
        match result {
            Ok(v) => {
                if let Some(o) = v.as_obj() {
                    self.apply(o);
                }
                Ok(())
            }
            Err(e) => {
                self.sending.remove(&random_id);
                let text = format!(
                    "{}: {} (not sent)",
                    if photo { "Photo" } else { "File" },
                    name
                );
                self.update(|s| {
                    if let Some(m) = s
                        .history
                        .get_mut(&peer)
                        .and_then(|h| h.messages.iter_mut().find(|m| m.random_id == random_id))
                    {
                        m.failed = true;
                        m.media = Some(text);
                    }
                });
                Err(e)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn upload_and_send(
        &mut self,
        peer: Peer,
        data: &[u8],
        name: &str,
        mime: &str,
        photo: bool,
        random_id: i64,
        reply_to: i64,
        label: &dyn Fn(usize) -> String,
    ) -> Result<Value> {
        let file_id = crypto::random_i64();
        let parts = data.len().div_ceil(UPLOAD_PART).max(1);
        let big = data.len() > 10 << 20;
        for part in 0..parts {
            if crate::fiber::cancelled() {
                return Err(Error::Other(String::from("cancelled")));
            }
            let chunk = &data[part * UPLOAD_PART..((part + 1) * UPLOAD_PART).min(data.len())];
            let req = if big {
                Obj::new(
                    "upload.saveBigFilePart",
                    &[
                        ("file_id", Value::Long(file_id)),
                        ("file_part", Value::Int(part as i32)),
                        ("file_total_parts", Value::Int(parts as i32)),
                        ("bytes", Value::Bytes(chunk.to_vec())),
                    ],
                )
            } else {
                Obj::new(
                    "upload.saveFilePart",
                    &[
                        ("file_id", Value::Long(file_id)),
                        ("file_part", Value::Int(part as i32)),
                        ("bytes", Value::Bytes(chunk.to_vec())),
                    ],
                )
            };
            self.call(&req)?;
            let done = (part + 1) * 99 / parts;
            let text = label(done);
            self.update(|s| {
                if let Some(m) = s
                    .history
                    .get_mut(&peer)
                    .and_then(|h| h.messages.iter_mut().find(|m| m.random_id == random_id))
                {
                    m.media = Some(text);
                }
            });
        }
        let file = if big {
            Obj::new(
                "inputFileBig",
                &[
                    ("id", Value::Long(file_id)),
                    ("parts", Value::Int(parts as i32)),
                    ("name", Value::str(name)),
                ],
            )
        } else {
            Obj::new(
                "inputFile",
                &[
                    ("id", Value::Long(file_id)),
                    ("parts", Value::Int(parts as i32)),
                    ("name", Value::str(name)),
                    ("md5_checksum", Value::str("")),
                ],
            )
        };
        let media = if photo {
            Obj::new("inputMediaUploadedPhoto", &[("file", file.into())])
        } else {
            let attr = Obj::new(
                "documentAttributeFilename",
                &[("file_name", Value::str(name))],
            );
            Obj::new(
                "inputMediaUploadedDocument",
                &[
                    ("file", file.into()),
                    ("mime_type", Value::str(mime)),
                    ("attributes", Value::Vector(vec![attr.into()])),
                ],
            )
        };
        let req = Obj::new(
            "messages.sendMedia",
            &[
                ("peer", self.input_peer(peer).into()),
                ("reply_to", reply_header(reply_to)),
                ("media", media.into()),
                ("message", Value::str("")),
                ("random_id", Value::Long(random_id)),
            ],
        );
        let v = self.call(&req)?;
        let text = label(100);
        self.update(|s| {
            if let Some(m) = s
                .history
                .get_mut(&peer)
                .and_then(|h| h.messages.iter_mut().find(|m| m.random_id == random_id))
            {
                m.media = Some(text);
            }
        });
        Ok(v)
    }

    /// Our message got its id on the server.
    fn sent(&mut self, random_id: i64, id: i64, date: i64) {
        let Some(peer) = self.sending.remove(&random_id) else {
            return;
        };
        self.update(|s| {
            let Some(h) = s.history.get_mut(&peer) else {
                return;
            };
            // the update with the message may have come first
            if h.messages.iter().any(|m| m.id == id) {
                h.messages.retain(|m| m.random_id != random_id || m.id != 0);
            } else if let Some(m) = h.messages.iter_mut().find(|m| m.random_id == random_id) {
                m.id = id;
                if date != 0 {
                    m.date = date;
                }
            }
        });
    }

    // ---- updates -------------------------------------------------------------------------

    fn apply(&mut self, u: &Obj) {
        match u.name() {
            "updatesTooLong" => self.reload = true,
            "updateShortMessage" | "updateShortChatMessage" => {
                let out = u.flag("out");
                let (peer, from) = if u.is("updateShortMessage") {
                    let other = u.int("user_id");
                    (Peer::User(other), if out { self.me } else { other })
                } else {
                    (Peer::Chat(u.int("chat_id")), u.int("from_id"))
                };
                let text = u.string("message");
                let links = links_of(&text, u.vec("entities"));
                let msg = Message {
                    id: u.int("id"),
                    out,
                    from: self.name_of(from),
                    from_id: from,
                    text,
                    media: None,
                    service: false,
                    date: u.int("date"),
                    edited: false,
                    random_id: 0,
                    failed: false,
                    links,
                    reply_to: reply_of(u),
                    ..Default::default()
                };
                self.incoming(peer, msg);
            }
            "updateShort" => {
                if let Some(inner) = u.obj("update") {
                    self.apply_one(inner);
                }
            }
            "updates" | "updatesCombined" => {
                self.remember_all(u);
                for inner in u.vec("updates").iter().filter_map(|x| x.as_obj()) {
                    self.apply_one(inner);
                }
            }
            _ => {}
        }
    }

    fn apply_one(&mut self, u: &Obj) {
        match u.name() {
            "updateNewMessage" | "updateNewChannelMessage" => {
                if let Some(m) = u.obj("message") {
                    if let (Some(peer), Some(msg)) = (peer_of(m.obj("peer_id")), self.convert(m)) {
                        self.incoming(peer, msg);
                    }
                }
            }
            "updateEditMessage" | "updateEditChannelMessage" => {
                if let Some(m) = u.obj("message") {
                    if let (Some(peer), Some(msg)) = (peer_of(m.obj("peer_id")), self.convert(m)) {
                        let id = msg.id;
                        self.change(peer, id, |old| *old = msg.clone());
                        // the chat list shows the new text of the last one
                        self.update(|s| {
                            let last = s
                                .history
                                .get(&peer)
                                .and_then(|h| h.messages.iter().rev().find(|m| m.id != 0))
                                .is_some_and(|m| m.id == id);
                            if let (true, Some(c)) =
                                (last, s.chats.iter_mut().find(|c| c.peer == peer))
                            {
                                c.last = preview(&msg);
                            }
                        });
                    }
                }
            }
            "updateMessageID" => self.sent(u.int("random_id"), u.int("id"), 0),
            "updateDeleteMessages" | "updateDeleteChannelMessages" => {
                let ids: Vec<i64> = u.vec("messages").iter().map(|v| v.as_i64()).collect();
                let channel = u
                    .is("updateDeleteChannelMessages")
                    .then(|| u.int("channel_id"));
                self.deleted(None, channel, &ids);
            }
            "updatePinnedMessages" | "updatePinnedChannelMessages" => {
                let peer = if u.is("updatePinnedChannelMessages") {
                    Some(Peer::Channel(u.int("channel_id")))
                } else {
                    peer_of(u.obj("peer"))
                };
                let pinned = u.flag("pinned");
                let ids: Vec<i64> = u.vec("messages").iter().map(|v| v.as_i64()).collect();
                if let Some(peer) = peer {
                    for &id in &ids {
                        self.change(peer, id, |m| m.pinned = pinned);
                    }
                    if !pinned {
                        self.update(|s| {
                            if let Some(v) = s.pinned.get_mut(&peer) {
                                v.retain(|m| !ids.contains(&m.id));
                            }
                        });
                    } else if !self.pinned_dirty.contains(&peer) {
                        self.pinned_dirty.push(peer);
                    }
                }
            }
            "updateUserTyping" | "updateChatUserTyping" | "updateChannelUserTyping" => {
                let (peer, who) = match u.name() {
                    "updateUserTyping" => (Peer::User(u.int("user_id")), u.int("user_id")),
                    "updateChatUserTyping" => (
                        Peer::Chat(u.int("chat_id")),
                        peer_of(u.obj("from_id")).map_or(0, peer_id),
                    ),
                    _ => (
                        Peer::Channel(u.int("channel_id")),
                        peer_of(u.obj("from_id")).map_or(0, peer_id),
                    ),
                };
                if who == self.me {
                    return;
                }
                let what = typing_text(u.obj("action"));
                let until = now_ms() + TYPING_MS;
                self.update(|s| {
                    let v = s.typing.entry(peer).or_default();
                    v.retain(|t| t.0 != who);
                    if let Some(what) = what {
                        v.push((who, what, until));
                    }
                });
            }
            "updateUserStatus" => {
                let id = u.int("user_id");
                if let Some(st) = u.obj("status").map(status_of) {
                    self.update(|s| {
                        s.status.insert(id, st);
                    });
                }
            }
            "updateReadHistoryInbox" | "updateReadChannelInbox" => {
                let peer = if u.is("updateReadChannelInbox") {
                    Some(Peer::Channel(u.int("channel_id")))
                } else {
                    peer_of(u.obj("peer"))
                };
                let left = u.int("still_unread_count");
                if let Some(peer) = peer {
                    self.update(|s| {
                        if let Some(c) = s.chats.iter_mut().find(|c| c.peer == peer) {
                            c.unread = left;
                        }
                    });
                }
            }
            "updateReadHistoryOutbox" | "updateReadChannelOutbox" => {
                let peer = if u.is("updateReadChannelOutbox") {
                    Some(Peer::Channel(u.int("channel_id")))
                } else {
                    peer_of(u.obj("peer"))
                };
                let max = u.int("max_id");
                if let Some(peer) = peer {
                    self.update(|s| {
                        if let Some(c) = s.chats.iter_mut().find(|c| c.peer == peer) {
                            c.read_out = c.read_out.max(max);
                        }
                    });
                }
            }
            "updateChannelTooLong" => self.reload = true,
            "updateLoginToken" => self.login_token = true,
            _ => {}
        }
    }

    /// A new message: add it to its chat and move the chat up.
    fn incoming(&mut self, peer: Peer, msg: Message) {
        let known = self.shared.borrow().chat(peer).is_some();
        if !known {
            // a chat that is not in the list yet
            let chat = self.new_chat(peer);
            self.update(|s| s.chats.insert(0, chat));
        }
        let out = msg.out;
        self.bump_chat(peer, &msg);
        let from = msg.from_id;
        self.update(|s| {
            if let Some(v) = s.typing.get_mut(&peer) {
                v.retain(|t| t.0 != from);
            }
            if let Some(h) = s.history.get_mut(&peer) {
                // our copy shown while sending already has the id: the
                // server's has the photo or file itself
                if let Some(m) = h
                    .messages
                    .iter_mut()
                    .find(|m| m.id == msg.id && msg.id != 0 && m.random_id != 0)
                {
                    *m = msg;
                    return;
                }
                if msg.id == 0 || !h.messages.iter().any(|m| m.id == msg.id) {
                    // our own message coming back from the server replaces
                    // the copy shown while sending
                    let pos = h
                        .messages
                        .iter()
                        .position(|m| m.id == 0 && m.out && m.text == msg.text && out);
                    match pos {
                        Some(i) => h.messages[i] = msg,
                        None => h.messages.push(msg),
                    }
                    h.messages
                        .sort_by_key(|m| if m.id == 0 { i64::MAX } else { m.id });
                }
            }
            if !out {
                if let Some(c) = s.chats.iter_mut().find(|c| c.peer == peer) {
                    c.unread += 1;
                }
            }
        });
        if !out && self.shared.borrow().open == Some(peer) {
            self.read = Some(peer);
        }
    }

    fn bump_chat(&self, peer: Peer, msg: &Message) {
        self.update(|s| {
            let Some(i) = s.chats.iter().position(|c| c.peer == peer) else {
                return;
            };
            let mut c = s.chats.remove(i);
            c.last = preview(msg);
            c.last_out = msg.out;
            c.date = msg.date;
            // after the pinned chats
            let at = if c.pinned {
                0
            } else {
                s.chats.iter().take_while(|x| x.pinned).count()
            };
            s.chats.insert(at, c);
        });
    }

    // ---- users, chats and messages ------------------------------------------------------

    fn remember_all(&mut self, o: &Obj) {
        for u in o.vec("users").iter().filter_map(|x| x.as_obj()) {
            self.remember_user(u);
        }
        for c in o.vec("chats").iter().filter_map(|x| x.as_obj()) {
            let id = c.int("id");
            let title = c.string("title");
            let info = match c.name() {
                "chat" | "chatForbidden" => ChatInfo {
                    access_hash: 0,
                    title,
                    broadcast: false,
                    username: String::new(),
                    left: c.flag("left") || c.is("chatForbidden"),
                },
                "channel" | "channelForbidden" => {
                    let old = self.chats.get(&id);
                    // "min" channels come without a usable access hash
                    let hash = if c.flag("min") {
                        old.map_or(c.int("access_hash"), |o| o.access_hash)
                    } else {
                        c.int("access_hash")
                    };
                    let min = c.flag("min");
                    ChatInfo {
                        access_hash: hash,
                        title,
                        broadcast: c.flag("broadcast"),
                        username: match (username_of(c), old) {
                            (u, Some(o)) if u.is_empty() && min => o.username.clone(),
                            (u, _) => u,
                        },
                        // "min" channels don't say whether we are in them
                        left: if min {
                            old.is_none_or(|o| o.left)
                        } else {
                            c.flag("left") || c.is("channelForbidden")
                        },
                    }
                }
                _ => continue,
            };
            self.chats.insert(id, info);
        }
    }

    fn remember_user(&mut self, u: &Obj) {
        if !u.is("user") {
            return;
        }
        let id = u.int("id");
        let old = self.users.get(&id).cloned();
        let mut name = u.string("first_name");
        let last = u.string("last_name");
        if !last.is_empty() {
            if !name.is_empty() {
                name.push(' ');
            }
            name.push_str(&last);
        }
        if u.flag("deleted") {
            name = String::from("Deleted Account");
        }
        if u.flag("min") {
            if let Some(o) = &old {
                // keep what we know better
                if name.is_empty() {
                    name = o.name.clone();
                }
            }
        }
        let hash = match (&old, u.flag("min")) {
            (Some(o), true) => o.access_hash,
            _ => u.int("access_hash"),
        };
        if u.flag("self") {
            self.me = id;
        }
        if let Some(st) = u.obj("status").map(status_of) {
            // the window draws it with the rest of the change
            self.shared.borrow_mut().status.insert(id, st);
        }
        self.users.insert(
            id,
            UserInfo {
                access_hash: hash,
                name,
                bot: u.flag("bot"),
                username: match (username_of(u), &old) {
                    (n, Some(o)) if n.is_empty() && u.flag("min") => o.username.clone(),
                    (n, _) => n,
                },
                phone: match (u.string("phone"), &old) {
                    (p, Some(o)) if p.is_empty() => o.phone.clone(),
                    (p, _) => p,
                },
            },
        );
    }

    fn name_of(&self, id: i64) -> String {
        self.users
            .get(&id)
            .map(|u| u.name.clone())
            .or_else(|| self.chats.get(&id).map(|c| c.title.clone()))
            .unwrap_or_default()
    }

    fn convert(&self, m: &Obj) -> Option<Message> {
        let service = m.is("messageService");
        if !m.is("message") && !service {
            return None;
        }
        let out = m.flag("out");
        let from_id = match peer_of(m.obj("from_id")) {
            Some(Peer::User(id) | Peer::Chat(id) | Peer::Channel(id)) => id,
            None => match peer_of(m.obj("peer_id")) {
                Some(Peer::User(id)) if !out => id,
                Some(Peer::Channel(id)) => id,
                _ => self.me,
            },
        };
        let from = self.name_of(from_id);
        let (text, media) = if service {
            (action_text(m.obj("action"), &from), None)
        } else {
            (m.string("message"), m.obj("media").and_then(media_label))
        };
        let (photo, file, web) = match m.obj("media") {
            Some(md) if !service => parse_media(md),
            _ => (None, None, None),
        };
        let links = if service {
            Vec::new()
        } else {
            links_of(&text, m.vec("entities"))
        };
        Some(Message {
            id: m.int("id"),
            out,
            from,
            from_id,
            text,
            media,
            service,
            date: m.int("date"),
            edited: m.get("edit_date").is_some() && !m.flag("edit_hide"),
            random_id: 0,
            failed: false,
            photo,
            file,
            links,
            web,
            reply_to: reply_of(m),
            pinned: m.flag("pinned"),
            fwd: m.obj("fwd_from").map(|f| {
                match peer_of(f.obj("from_id")) {
                    Some(p) => self.name_of(peer_id(p)),
                    None => f.string("from_name"),
                }
            }),
            buttons: m.obj("reply_markup").map(buttons_of).unwrap_or_default(),
            keyboard: m.obj("reply_markup").and_then(keyboard_of),
        })
    }
}

fn peer_id(p: Peer) -> i64 {
    match p {
        Peer::User(id) | Peer::Chat(id) | Peer::Channel(id) => id,
    }
}

/// The message a message (or an updateShortMessage) answers; 0 for none.
fn reply_of(m: &Obj) -> i64 {
    m.obj("reply_to")
        .filter(|r| r.is("messageReplyHeader") && r.get("reply_to_peer_id").as_obj().is_none())
        .map_or(0, |r| r.int("reply_to_msg_id"))
}

/// The reply_to field of a message we send.
fn reply_header(reply_to: i64) -> Value {
    if reply_to == 0 {
        return Value::None;
    }
    Obj::new(
        "inputReplyToMessage",
        &[("reply_to_msg_id", Value::Int(reply_to as i32))],
    )
    .into()
}

/// What a message that is no more is quoted as.
fn gone(id: i64) -> Message {
    Message {
        id,
        text: String::from("Deleted message"),
        service: true,
        ..Default::default()
    }
}

/// Inline buttons under a message.
fn buttons_of(markup: &Obj) -> Vec<Vec<super::KeyButton>> {
    use super::{ButtonAction, KeyButton};
    if !markup.is("replyInlineMarkup") {
        return Vec::new();
    }
    let mut rows = Vec::new();
    for row in markup.vec("rows").iter().filter_map(|r| r.as_obj()) {
        let mut out = Vec::new();
        for b in row.vec("buttons").iter().filter_map(|b| b.as_obj()) {
            let t = b.obj("type");
            let action = match t.map(|t| t.name()) {
                Some("inlineButtonTypeUrl" | "inlineButtonTypeUrlAuth" | "inlineButtonTypeWebView") => {
                    ButtonAction::Url(t.map_or(String::new(), |t| t.string("url")))
                }
                Some("inlineButtonTypeCallback") => {
                    ButtonAction::Callback(t.map_or(Vec::new(), |t| t.bytes("data").to_vec()))
                }
                Some("inlineButtonTypeCopy") => {
                    ButtonAction::Copy(t.map_or(String::new(), |t| t.string("copy_text")))
                }
                _ => ButtonAction::Other,
            };
            out.push(KeyButton {
                text: b.string("text"),
                action,
            });
        }
        if !out.is_empty() {
            rows.push(out);
        }
    }
    rows
}

/// A bot's keyboard: rows of texts; empty when it hides the keyboard.
fn keyboard_of(markup: &Obj) -> Option<Vec<Vec<String>>> {
    match markup.name() {
        "replyKeyboardHide" => Some(Vec::new()),
        "replyKeyboardMarkup" => Some(
            markup
                .vec("rows")
                .iter()
                .filter_map(|r| r.as_obj())
                .map(|r| {
                    r.vec("buttons")
                        .iter()
                        .filter_map(|b| b.as_obj())
                        .map(|b| b.string("text"))
                        .collect::<Vec<String>>()
                })
                .filter(|r| !r.is_empty())
                .collect(),
        ),
        _ => None,
    }
}

fn status_of(st: &Obj) -> super::Status {
    use super::Status;
    match st.name() {
        "userStatusOnline" => Status::Online(st.int("expires")),
        "userStatusOffline" => Status::Offline(st.int("was_online")),
        "userStatusRecently" => Status::Recently,
        "userStatusLastWeek" => Status::LastWeek,
        "userStatusLastMonth" => Status::LastMonth,
        _ => Status::Hidden,
    }
}

/// "typing", "sending a photo", ...; None when they stopped.
fn typing_text(action: Option<&Obj>) -> Option<String> {
    let name = action.map_or("sendMessageTypingAction", |a| a.name());
    let what = match name {
        "sendMessageCancelAction" => return None,
        "sendMessageRecordVideoAction" | "sendMessageRecordRoundAction" => "recording a video",
        "sendMessageUploadVideoAction" | "sendMessageUploadRoundAction" => "sending a video",
        "sendMessageRecordAudioAction" => "recording a voice message",
        "sendMessageUploadAudioAction" => "sending a voice message",
        "sendMessageUploadPhotoAction" => "sending a photo",
        "sendMessageUploadDocumentAction" => "sending a file",
        "sendMessageChooseStickerAction" => "choosing a sticker",
        "sendMessageGamePlayAction" => "playing a game",
        "sendMessageGeoLocationAction" => "choosing a location",
        "sendMessageChooseContactAction" => "choosing a contact",
        _ => "typing",
    };
    Some(String::from(what))
}

fn peer_of(p: Option<&Obj>) -> Option<Peer> {
    let p = p?;
    match p.name() {
        "peerUser" => Some(Peer::User(p.int("user_id"))),
        "peerChat" => Some(Peer::Chat(p.int("chat_id"))),
        "peerChannel" => Some(Peer::Channel(p.int("channel_id"))),
        _ => None,
    }
}

/// A message as one line for the chat list.
pub fn preview(m: &Message) -> String {
    let mut s = String::new();
    if let Some(media) = &m.media {
        s.push_str(media);
        if !m.text.is_empty() {
            s.push_str(", ");
        }
    }
    for c in m.text.chars() {
        s.push(if c == '\n' { ' ' } else { c });
        if s.len() > 200 {
            break;
        }
    }
    s
}

/// Shrink a picture to fit `max_w` x `max_h`, averaging the pixels.
fn shrink(img: crate::web::image::Image, max_w: usize, max_h: usize) -> crate::web::image::Image {
    let (w, h) = (img.width, img.height);
    if w <= max_w && h <= max_h {
        return img;
    }
    let scale = (max_w as f32 / w as f32).min(max_h as f32 / h as f32);
    let (nw, nh) = (
        ((w as f32 * scale) as usize).max(1),
        ((h as f32 * scale) as usize).max(1),
    );
    let mut pixels = Vec::with_capacity(nw * nh);
    for y in 0..nh {
        let (y0, y1) = (y * h / nh, ((y + 1) * h / nh).max(y * h / nh + 1));
        for x in 0..nw {
            let (x0, x1) = (x * w / nw, ((x + 1) * w / nw).max(x * w / nw + 1));
            let (mut r, mut g, mut b, mut n) = (0u32, 0u32, 0u32, 0u32);
            for sy in y0..y1.min(h) {
                for sx in x0..x1.min(w) {
                    let p = img.pixels[sy * w + sx];
                    r += p >> 16 & 0xff;
                    g += p >> 8 & 0xff;
                    b += p & 0xff;
                    n += 1;
                }
            }
            let n = n.max(1);
            pixels.push(0xff00_0000 | (r / n) << 16 | (g / n) << 8 | b / n);
        }
    }
    crate::web::image::Image {
        width: nw,
        height: nh,
        pixels,
    }
}

/// Save a downloaded file in the user's Downloads, under a name not
/// taken yet. Returns its path.
fn save_download(name: &str, data: &[u8]) -> core::result::Result<String, String> {
    let user = crate::users::current_name().unwrap_or_default();
    let dir = fs::join(&fs::home(user.as_str()), "Downloads");
    if !fs::is_dir(&dir) {
        fs::create_dir(&dir).map_err(|e| String::from(e.message()))?;
    }
    // no slashes or other signs a file name can't have
    let safe: String = name
        .chars()
        .map(|c| {
            if "/\\:*?\"<>|".contains(c) || (c as u32) < 32 {
                '_'
            } else {
                c
            }
        })
        .collect();
    let (stem, ext) = match safe.rfind('.') {
        Some(i) if i > 0 => (&safe[..i], &safe[i..]),
        _ => (safe.as_str(), ""),
    };
    let path = fs::join(&dir, &fs::unique_name(&dir, stem, ext));
    fs::write(&path, data).map_err(|e| String::from(e.message()))?;
    Ok(path)
}

/// The public @name of a user or channel, without the @.
fn username_of(o: &Obj) -> String {
    let name = o.string("username");
    if !name.is_empty() {
        return name;
    }
    o.vec("usernames")
        .iter()
        .filter_map(|u| u.as_obj())
        .find(|u| u.flag("active"))
        .map(|u| u.string("username"))
        .unwrap_or_default()
}

/// The links of a message: from its entities (whose places count UTF-16
/// units) as (first char, end char, target), or found in the text when
/// it has none.
fn links_of(text: &str, entities: &[Value]) -> Vec<(usize, usize, String)> {
    // the char at each UTF-16 place
    let mut at16 = Vec::with_capacity(text.len() + 1);
    let chars: Vec<char> = text.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        for _ in 0..c.len_utf16() {
            at16.push(i);
        }
    }
    at16.push(chars.len());
    let char_at = |o: i64| at16.get(o.max(0) as usize).copied().unwrap_or(chars.len());
    let mut out = Vec::new();
    for e in entities.iter().filter_map(|e| e.as_obj()) {
        let (a, b) = (
            char_at(e.int("offset")),
            char_at(e.int("offset") + e.int("length")),
        );
        if a >= b {
            continue;
        }
        let inner: String = chars[a..b].iter().collect();
        let target = match e.name() {
            "messageEntityUrl" => inner,
            "messageEntityTextUrl" => e.string("url"),
            "messageEntityMention" => inner,
            "messageEntityMentionName" => format!("tg://user?id={}", e.int("user_id")),
            _ => continue,
        };
        out.push((a, b, target));
    }
    if entities.is_empty() {
        // our own messages before the server has seen them
        let mut i = 0;
        while i < chars.len() {
            let start_ok = i == 0 || chars[i - 1].is_whitespace();
            let rest: String = chars[i..chars.len().min(i + 8)].iter().collect();
            let looks = rest.starts_with("http://")
                || rest.starts_with("https://")
                || rest.starts_with("t.me/")
                || (rest.starts_with('@') && rest.len() > 1);
            if start_ok && looks {
                let mut j = i;
                while j < chars.len() && !chars[j].is_whitespace() {
                    j += 1;
                }
                // a full stop or comma after a link is not part of it
                while j > i && matches!(chars[j - 1], '.' | ',' | ')' | '!' | '?' | ':' | ';') {
                    j -= 1;
                }
                if j - i > 1 {
                    out.push((i, j, chars[i..j].iter().collect()));
                }
                i = j.max(i + 1);
            } else {
                i += 1;
            }
        }
    }
    out
}

/// The photo, file and link preview of a message's media.
fn parse_media(md: &Obj) -> (Option<Photo>, Option<FileInfo>, Option<(String, String)>) {
    match md.name() {
        "messageMediaPhoto" => (md.obj("photo").and_then(parse_photo), None, None),
        "messageMediaDocument" => (
            None,
            md.obj("document").and_then(|d| parse_file(md, d)),
            None,
        ),
        "messageMediaWebPage" => {
            let web = md.obj("webpage").filter(|w| w.is("webPage")).map(|w| {
                let site = w.string("site_name");
                let title = w.string("title");
                let title = if title.is_empty() {
                    w.string("description")
                } else {
                    title
                };
                (site, title)
            });
            (
                None,
                None,
                web.filter(|(s, t)| !s.is_empty() || !t.is_empty()),
            )
        }
        _ => (None, None, None),
    }
}

/// A size of a photo: its type letter, width, height and bytes.
fn sizes(list: &[Value]) -> Vec<(String, i32, i32, i64)> {
    list.iter()
        .filter_map(|v| v.as_obj())
        .filter_map(|p| match p.name() {
            "photoSize" => Some((
                p.string("type"),
                p.int("w") as i32,
                p.int("h") as i32,
                p.int("size"),
            )),
            "photoSizeProgressive" => Some((
                p.string("type"),
                p.int("w") as i32,
                p.int("h") as i32,
                p.vec("sizes").last().map_or(0, |v| v.as_i64()),
            )),
            _ => None,
        })
        .collect()
}

fn parse_photo(p: &Obj) -> Option<Photo> {
    if !p.is("photo") {
        return None;
    }
    let all = sizes(p.vec("sizes"));
    let big = all.iter().max_by_key(|s| s.1 as i64 * s.2 as i64)?.clone();
    // about 320 pixels is plenty for the chat
    let small = all
        .iter()
        .filter(|s| s.1.max(s.2) >= 300)
        .min_by_key(|s| s.1 as i64 * s.2 as i64)
        .unwrap_or(&big)
        .clone();
    Some(Photo {
        file: FileRef {
            id: p.int("id"),
            access_hash: p.int("access_hash"),
            file_reference: p.bytes("file_reference").to_vec(),
            dc: p.int("dc_id") as i32,
            photo: true,
        },
        w: big.1,
        h: big.2,
        small: small.0,
        big: big.0,
        big_size: big.3,
    })
}

fn parse_file(md: &Obj, d: &Obj) -> Option<FileInfo> {
    if !d.is("document") {
        return None;
    }
    let mime = d.string("mime_type");
    let mut name = String::new();
    let mut kind = String::from("File");
    let mut wh = (0, 0);
    for a in d.vec("attributes").iter().filter_map(|a| a.as_obj()) {
        match a.name() {
            "documentAttributeFilename" => name = a.string("file_name"),
            "documentAttributeSticker" => kind = String::from("Sticker"),
            "documentAttributeAnimated" => kind = String::from("GIF"),
            "documentAttributeVideo" if kind == "File" => {
                kind = String::from(if md.flag("round") {
                    "Video message"
                } else {
                    "Video"
                });
                wh = (a.int("w") as i32, a.int("h") as i32);
            }
            "documentAttributeAudio" => {
                kind = String::from(if a.flag("voice") {
                    "Voice message"
                } else {
                    "Music"
                });
                let title = a.string("title");
                if name.is_empty() && !title.is_empty() {
                    let performer = a.string("performer");
                    name = if performer.is_empty() {
                        title
                    } else {
                        format!("{} - {}", performer, title)
                    };
                }
            }
            "documentAttributeImageSize" => wh = (a.int("w") as i32, a.int("h") as i32),
            _ => {}
        }
    }
    if name.is_empty() || !name.contains('.') {
        let ext = match mime.as_str() {
            "video/mp4" => "mp4",
            "audio/ogg" => "ogg",
            "audio/mpeg" => "mp3",
            "image/jpeg" => "jpg",
            "image/png" => "png",
            "image/webp" => "webp",
            "application/x-tgsticker" => "tgs",
            "application/pdf" => "pdf",
            _ => "bin",
        };
        let stem = if name.is_empty() {
            kind.to_lowercase().replace(' ', "_")
        } else {
            name
        };
        name = format!("{}_{}.{}", stem, d.int("id") as u64 % 100000, ext);
    }
    // a preview picture (JPEG) of videos and pictures sent as files
    let thumbs = sizes(d.vec("thumbs"));
    let thumb = thumbs
        .iter()
        .filter(|t| t.1.max(t.2) >= 90)
        .max_by_key(|t| t.1 as i64 * t.2 as i64)
        .map(|t| (t.0.clone(), t.1, t.2));
    let thumb = if mime == "image/jpeg" || mime == "image/png" {
        // a picture sent as a file: the file itself when it is small
        thumb.or((d.int("size") < 2 << 20).then(|| (String::new(), wh.0, wh.1)))
    } else {
        thumb
    };
    Some(FileInfo {
        file: FileRef {
            id: d.int("id"),
            access_hash: d.int("access_hash"),
            file_reference: d.bytes("file_reference").to_vec(),
            dc: d.int("dc_id") as i32,
            photo: false,
        },
        name,
        mime,
        size: d.int("size"),
        kind,
        thumb,
    })
}

fn media_label(media: &Obj) -> Option<String> {
    let label = match media.name() {
        "messageMediaEmpty" | "messageMediaWebPage" => return None,
        "messageMediaPhoto" => "Photo",
        "messageMediaGeo" | "messageMediaVenue" => "Location",
        "messageMediaGeoLive" => "Live location",
        "messageMediaContact" => "Contact",
        "messageMediaPoll" => "Poll",
        "messageMediaGame" => "Game",
        "messageMediaInvoice" => "Invoice",
        "messageMediaStory" => "Story",
        "messageMediaGiveaway" | "messageMediaGiveawayResults" => "Giveaway",
        "messageMediaPaidMedia" => "Paid media",
        "messageMediaDice" => return Some(media.string("emoticon")),
        "messageMediaDocument" => {
            let doc = media.obj("document");
            let attrs = doc.map(|d| d.vec("attributes")).unwrap_or(&[]);
            let mut label = String::from("File");
            for a in attrs.iter().filter_map(|a| a.as_obj()) {
                match a.name() {
                    "documentAttributeSticker" => {
                        let alt = a.string("alt");
                        return Some(if alt.is_empty() {
                            String::from("Sticker")
                        } else {
                            format!("Sticker {}", alt)
                        });
                    }
                    "documentAttributeAnimated" => return Some(String::from("GIF")),
                    "documentAttributeVideo" => {
                        return Some(String::from(if a.flag("round_message") {
                            "Video message"
                        } else {
                            "Video"
                        }))
                    }
                    "documentAttributeAudio" => {
                        return Some(String::from(if a.flag("voice") {
                            "Voice message"
                        } else {
                            "Music"
                        }))
                    }
                    "documentAttributeFilename" => {
                        label = format!("File {}", a.string("file_name"));
                    }
                    _ => {}
                }
            }
            return Some(label);
        }
        _ => "Attachment",
    };
    Some(label.to_string())
}

fn action_text(action: Option<&Obj>, who: &str) -> String {
    let Some(a) = action else {
        return String::from("Service message");
    };
    let what = match a.name() {
        "messageActionChatCreate" => format!("created the group «{}»", a.string("title")),
        "messageActionChannelCreate" => format!("Channel «{}» created", a.string("title")),
        "messageActionChatEditTitle" => format!("changed the name to «{}»", a.string("title")),
        "messageActionChatEditPhoto" => String::from("changed the group photo"),
        "messageActionChatAddUser" => String::from("joined the group"),
        "messageActionChatJoinedByLink" | "messageActionChatJoinedByRequest" => {
            String::from("joined the group by link")
        }
        "messageActionChatDeleteUser" => String::from("left the group"),
        "messageActionPinMessage" => String::from("pinned a message"),
        "messageActionPhoneCall" => String::from("Call"),
        "messageActionContactSignUp" => String::from("joined Telegram"),
        "messageActionScreenshotTaken" => String::from("took a screenshot"),
        "messageActionHistoryClear" => String::from("History was cleared"),
        "messageActionChatMigrateTo" | "messageActionChannelMigrateFrom" => {
            String::from("The group was upgraded to a supergroup")
        }
        "messageActionGroupCall" => String::from("Video chat"),
        _ => String::from("Service message"),
    };
    if who.is_empty() || what.starts_with(char::is_uppercase) {
        what
    } else {
        format!("{} {}", who, what)
    }
}

/// Where the login code went, for the code screen.
fn code_hint(kind: Option<&Obj>) -> String {
    String::from(match kind.map(|k| k.name()).unwrap_or("") {
        "auth.sentCodeTypeApp" => "We sent the code to the Telegram app on your other device.",
        "auth.sentCodeTypeSms" | "auth.sentCodeTypeSmsWord" | "auth.sentCodeTypeSmsPhrase" => {
            "We sent the code in an SMS."
        }
        "auth.sentCodeTypeCall" | "auth.sentCodeTypeFlashCall" | "auth.sentCodeTypeMissedCall" => {
            "Telegram will call you with the code."
        }
        "auth.sentCodeTypeEmailCode" => "We sent the code to your email.",
        "auth.sentCodeTypeFragmentSms" => "We sent the code to your Fragment number.",
        _ => "Enter the code Telegram sent you.",
    })
}

/// The type Telegram is told a file has, from its name.
fn mime_type(name: &str) -> &'static str {
    let ext = name
        .rsplit_once('.')
        .map_or("", |(_, e)| e)
        .to_ascii_lowercase();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "txt" | "log" | "md" => "text/plain",
        "html" | "htm" => "text/html",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "mp4" => "video/mp4",
        "avi" => "video/x-msvideo",
        _ => "application/octet-stream",
    }
}

/// Base64 with - and _ and no padding, as in tg://login links.
fn base64url(data: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(data.len() * 4 / 3 + 3);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..=chunk.len() {
            out.push(ABC[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

fn is_signed_out(e: &Error) -> bool {
    matches!(e, Error::Rpc { code: 401, message } if message != "SESSION_PASSWORD_NEEDED")
}

/// An API error in words.
fn friendly(e: &Error) -> String {
    if let Some(secs) = e.number_after("FLOOD_WAIT_") {
        return format!("Too many attempts. Try again in {} seconds.", secs);
    }
    let Error::Rpc { message, .. } = e else {
        return e.text();
    };
    String::from(match message.as_str() {
        "PHONE_NUMBER_INVALID" => "This phone number is not valid.",
        "PHONE_NUMBER_BANNED" => "This phone number is banned from Telegram.",
        "PHONE_NUMBER_FLOOD" => "Too many attempts with this number. Try again later.",
        "PHONE_CODE_INVALID" => "Wrong code. Try again.",
        "PHONE_CODE_EMPTY" => "Enter the code.",
        "PASSWORD_HASH_INVALID" => "Wrong password. Try again.",
        "API_ID_INVALID" | "API_ID_PUBLISHED_FLOOD" => {
            "Telegram does not accept this api_id and api_hash. Check them on my.telegram.org."
        }
        "CHAT_WRITE_FORBIDDEN" | "CHAT_ADMIN_REQUIRED" => "You can't write in this chat.",
        "USER_IS_BLOCKED" | "YOU_BLOCKED_USER" => "This user is blocked.",
        "MESSAGE_TOO_LONG" => "The message is too long.",
        "PEER_FLOOD" => "Telegram limits messages from this account for now.",
        _ => return format!("Telegram said: {}", message),
    })
}
