//! The VPN: a proxy client in the style of Happ and v2rayNG. It reads
//! subscriptions and share links (VLESS with REALITY or TLS and XTLS
//! Vision, Trojan, Shadowsocks), and while it is on, every TCP
//! connection a program opens ([`crate::net::TcpStream`]) goes to the
//! VPN server instead, which connects onwards. Host names go to the
//! server as names, so DNS lookups don't leak either.
//!
//! If the server can't be reached while the VPN is on, connections fail
//! rather than quietly going around it.

pub mod link;
mod ss;
use crate::net::tls;
mod vision;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest, Sha224};

pub use link::{Kind, Security, Server};

use crate::fs;
use crate::interrupts;
use crate::net::{self, Host, Socket};
use crate::sync::IrqMutex;

// ---- state -------------------------------------------------------------------------

/// The server connections go through while the VPN is on.
static ROUTE: IrqMutex<Option<Server>> = IrqMutex::new(None);
static SINCE: AtomicU64 = AtomicU64::new(0);
static UP: AtomicU64 = AtomicU64::new(0);
static DOWN: AtomicU64 = AtomicU64::new(0);
/// Bumped each time the VPN goes on or off, so long-lived connections
/// (Telegram's) know to reconnect.
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// The server to tunnel through, if the VPN is on.
pub fn route() -> Option<Server> {
    ROUTE.lock().clone()
}

pub fn is_on() -> bool {
    ROUTE.lock().is_some()
}

pub fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}

/// Bytes sent and received through the VPN since it went on.
pub fn traffic() -> (u64, u64) {
    (UP.load(Ordering::Relaxed), DOWN.load(Ordering::Relaxed))
}

/// Seconds since the VPN went on.
pub fn uptime() -> u64 {
    (interrupts::ticks() - SINCE.load(Ordering::Relaxed)) / interrupts::TIMER_HZ
}

fn set_route(server: Option<Server>) {
    let on = server.is_some();
    *ROUTE.lock() = server;
    SINCE.store(interrupts::ticks(), Ordering::Relaxed);
    UP.store(0, Ordering::Relaxed);
    DOWN.store(0, Ordering::Relaxed);
    GENERATION.fetch_add(1, Ordering::Relaxed);
    // connections the browser kept open went the other way
    crate::web::http::drop_pool();
    crate::serial::write_str(if on { "\nvpn: on\n" } else { "\nvpn: off\n" });
}

/// Check that `server` works, then send everything through it.
pub fn connect(server: &Server) -> Result<u32, String> {
    set_route(None);
    log(&format!("connecting to {} ({})", server.name, server.protocol()));
    let ms = test(server)?;
    set_route(Some(server.clone()));
    log(&format!("connected, {} ms", ms));
    Ok(ms)
}

pub fn disconnect() {
    if is_on() {
        set_route(None);
        log("disconnected");
    }
}

/// What the VPN did lately, for `vpn log`.
static LOG: IrqMutex<Vec<String>> = IrqMutex::new(Vec::new());

pub fn log(line: &str) {
    let mut l = LOG.lock();
    if l.len() >= 40 {
        l.remove(0);
    }
    let secs = interrupts::ticks() / interrupts::TIMER_HZ;
    l.push(format!("[{:>5}s] {}", secs, line));
}

pub fn recent_log() -> Vec<String> {
    LOG.lock().clone()
}

/// The page the delay test loads through the server, as host:port/path
/// (`vpn probe` changes it, for tests without the internet).
static PROBE: IrqMutex<Option<String>> = IrqMutex::new(None);

pub fn set_probe(target: &str) {
    *PROBE.lock() = Some(target.to_string());
}

/// The real delay through `server`: connect, and load a tiny page.
pub fn test(server: &Server) -> Result<u32, String> {
    let r = probe(server);
    if let Err(e) = &r {
        log(&format!("{}: {}", server.name, e));
    }
    r
}

fn probe(server: &Server) -> Result<u32, String> {
    let probe = PROBE
        .lock()
        .clone()
        .unwrap_or_else(|| String::from("cp.cloudflare.com:80/generate_204"));
    let (addr, path) = probe.split_once('/').unwrap_or((probe.as_str(), ""));
    let (host, port) = addr.rsplit_once(':').unwrap_or((addr, "80"));
    let port: u16 = port.parse().unwrap_or(80);
    let start = interrupts::ticks();
    let target = match net::parse_ipv4(host) {
        Some(ip) => Host::Ip(ip),
        None => Host::Name(host.to_string()),
    };
    let mut t = Tunnel::open(server, &target, port)?;
    let request = format!(
        "GET /{} HTTP/1.1\r\nHost: {}\r\nUser-Agent: RyzikOS\r\nConnection: close\r\n\r\n",
        path, host
    );
    t.write_all(request.as_bytes())?;
    let mut buf = [0u8; 512];
    let n = t.read(&mut buf)?;
    if n == 0 {
        return Err(String::from("the server closed the connection (wrong key or settings?)"));
    }
    if !buf[..n].starts_with(b"HTTP/") {
        return Err(String::from("the answer through the server was not a web page"));
    }
    let ms = (interrupts::ticks() - start) * 1000 / interrupts::TIMER_HZ;
    Ok(ms as u32)
}

// ---- the tunnel --------------------------------------------------------------------

enum Wire {
    Plain(Socket),
    Tls(alloc::boxed::Box<tls::Tls>),
}

impl Wire {
    fn write_all(&mut self, data: &[u8]) -> Result<(), String> {
        match self {
            Wire::Plain(s) => s.write_all(data),
            Wire::Tls(t) => t.write_all(data),
        }
    }

    fn read(&mut self, buf: &mut [u8], block: bool) -> Result<Option<usize>, String> {
        match self {
            Wire::Plain(s) if block => s.read(buf).map(Some),
            Wire::Plain(s) => s.try_read(buf),
            Wire::Tls(t) => t.read(buf, block),
        }
    }
}

enum Proto {
    Vless {
        /// The server's response header, until it is complete.
        response: Option<Vec<u8>>,
        writer: Option<vision::Writer>,
        reader: Option<vision::Reader>,
    },
    Trojan,
    Shadowsocks(ss::Session),
}

/// One connection through the VPN server.
pub struct Tunnel {
    wire: Wire,
    proto: Proto,
    /// The request header, sent with the first data.
    header: Option<Vec<u8>>,
    /// Data for the program not yet read.
    pending: Vec<u8>,
    pending_pos: usize,
    eof: bool,
}

/// The address as SOCKS5 writes it (Trojan and Shadowsocks).
fn socks_address(host: &Host, port: u16) -> Vec<u8> {
    let mut v = Vec::new();
    match host {
        Host::Ip(ip) => {
            v.push(1);
            v.extend_from_slice(&ip.octets());
        }
        Host::Name(n) => {
            v.push(3);
            v.push(n.len() as u8);
            v.extend_from_slice(n.as_bytes());
        }
    }
    v.extend_from_slice(&port.to_be_bytes());
    v
}

fn vless_header(uuid: &[u8; 16], flow: &str, host: &Host, port: u16) -> Vec<u8> {
    let mut v = vec![0u8];
    v.extend_from_slice(uuid);
    if flow.is_empty() {
        v.push(0);
    } else {
        // protobuf: field 1 (flow), a string
        v.push((flow.len() + 2) as u8);
        v.push(0x0a);
        v.push(flow.len() as u8);
        v.extend_from_slice(flow.as_bytes());
    }
    v.push(1); // TCP
    v.extend_from_slice(&port.to_be_bytes());
    match host {
        Host::Ip(ip) => {
            v.push(1);
            v.extend_from_slice(&ip.octets());
        }
        Host::Name(n) => {
            v.push(2);
            v.push(n.len() as u8);
            v.extend_from_slice(n.as_bytes());
        }
    }
    v
}

impl Tunnel {
    pub fn open(server: &Server, host: &Host, port: u16) -> Result<Tunnel, String> {
        if let Some(why) = server.unsupported() {
            return Err(format!("{}: {}", server.name, why));
        }
        if let Host::Name(n) = host {
            if n.len() > 255 {
                return Err(String::from("host name too long"));
            }
        }
        let ip = net::resolve(&server.host)
            .map_err(|e| format!("VPN server {}: {}", server.host, e))?;
        let sock = Socket::connect(ip, server.port)
            .map_err(|e| format!("VPN server {}:{}: {}", server.host, server.port, e))?;
        let wire = match server.security {
            Security::None => Wire::Plain(sock),
            Security::Tls | Security::Reality => {
                let reality = if server.security == Security::Reality {
                    let key = link::base64(&server.pbk);
                    let public_key: [u8; 32] = key
                        .as_slice()
                        .try_into()
                        .map_err(|_| String::from("REALITY: the public key (pbk) is not 32 bytes"))?;
                    let short_id = link::hex_bytes::<8>(&server.sid)
                        .ok_or("REALITY: the short id (sid) is not hex")?;
                    Some(tls::Reality { public_key, short_id })
                } else {
                    None
                };
                let alpn: Vec<String> = if !server.alpn.is_empty() {
                    server.alpn.clone()
                } else if reality.is_some() {
                    vec![String::from("h2"), String::from("http/1.1")]
                } else {
                    vec![String::from("http/1.1")]
                };
                let opts = tls::Options {
                    sni: &server.sni,
                    alpn: &alpn,
                    reality: reality.as_ref(),
                };
                Wire::Tls(alloc::boxed::Box::new(tls::Tls::connect(sock, &opts)?))
            }
        };
        let (proto, header) = match server.kind {
            Kind::Vless => {
                let uuid = link::uuid(&server.secret);
                let vision = server.flow == "xtls-rprx-vision";
                (
                    Proto::Vless {
                        response: Some(Vec::new()),
                        writer: vision.then(|| vision::Writer::new(uuid)),
                        reader: vision.then(|| vision::Reader::new(uuid)),
                    },
                    vless_header(&uuid, &server.flow, host, port),
                )
            }
            Kind::Trojan => {
                let hash = Sha224::digest(server.secret.as_bytes());
                let mut h = Vec::new();
                for b in hash {
                    h.extend_from_slice(format!("{:02x}", b).as_bytes());
                }
                h.extend_from_slice(b"\r\n\x01");
                h.extend_from_slice(&socks_address(host, port));
                h.extend_from_slice(b"\r\n");
                (Proto::Trojan, h)
            }
            Kind::Shadowsocks => {
                let method = ss::Method::parse(&server.method).ok_or("unknown cipher")?;
                (
                    Proto::Shadowsocks(ss::Session::new(method, &server.secret)),
                    socks_address(host, port),
                )
            }
            Kind::Vmess => return Err(String::from("VMess is not supported yet")),
        };
        Ok(Tunnel {
            wire,
            proto,
            header: Some(header),
            pending: Vec::new(),
            pending_pos: 0,
            eof: false,
        })
    }

    pub fn write_all(&mut self, data: &[u8]) -> Result<(), String> {
        let header = self.header.take();
        let out = match &mut self.proto {
            Proto::Vless { writer, .. } => {
                let body = match writer {
                    Some(w) => w.wrap(data),
                    None => data.to_vec(),
                };
                let mut out = header.unwrap_or_default();
                out.extend_from_slice(&body);
                out
            }
            Proto::Trojan => {
                let mut out = header.unwrap_or_default();
                out.extend_from_slice(data);
                out
            }
            Proto::Shadowsocks(s) => {
                let mut plain = header.unwrap_or_default();
                plain.extend_from_slice(data);
                s.seal(&plain)
            }
        };
        UP.fetch_add(data.len() as u64, Ordering::Relaxed);
        self.wire.write_all(&out)
    }

    pub fn read(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        let deadline = interrupts::ticks() + 30 * interrupts::TIMER_HZ;
        loop {
            if let Some(n) = self.read_some(buf, true)? {
                return Ok(n);
            }
            // the server sent only padding or a header: wait for more
            if interrupts::ticks() > deadline {
                return Err(String::from("timed out: reading"));
            }
        }
    }

    pub fn try_read(&mut self, buf: &mut [u8]) -> Result<Option<usize>, String> {
        self.read_some(buf, false)
    }

    fn read_some(&mut self, buf: &mut [u8], block: bool) -> Result<Option<usize>, String> {
        if self.pending_pos < self.pending.len() {
            let n = buf.len().min(self.pending.len() - self.pending_pos);
            buf[..n].copy_from_slice(&self.pending[self.pending_pos..self.pending_pos + n]);
            self.pending_pos += n;
            DOWN.fetch_add(n as u64, Ordering::Relaxed);
            return Ok(Some(n));
        }
        if self.eof {
            return Ok(Some(0));
        }
        if self.header.is_some() {
            // the program waits for the server to speak first
            self.write_all(&[])?;
        }
        let mut tmp = vec![0u8; 17 * 1024];
        let n = match self.wire.read(&mut tmp, block)? {
            None => return Ok(None),
            Some(0) => {
                self.eof = true;
                return Ok(Some(0));
            }
            Some(n) => n,
        };
        self.pending.clear();
        self.pending_pos = 0;
        let mut data = &tmp[..n];
        match &mut self.proto {
            Proto::Vless {
                response, reader, ..
            } => {
                if let Some(head) = response {
                    head.extend_from_slice(data);
                    if head.len() < 2 || head.len() < 2 + head[1] as usize {
                        return Ok(None);
                    }
                    let skip = 2 + head[1] as usize;
                    let rest = head.split_off(skip);
                    *response = None;
                    tmp = rest;
                    data = &tmp;
                }
                match reader {
                    Some(r) => {
                        r.unwrap(data, &mut self.pending);
                        if r.direct {
                            if let Wire::Tls(t) = &mut self.wire {
                                // anything still decrypted is plain, then raw
                                let mut more = vec![0u8; 17 * 1024];
                                while t.has_buffered() {
                                    if let Some(k) = t.read(&mut more, false)? {
                                        self.pending.extend_from_slice(&more[..k]);
                                    }
                                }
                                t.switch_to_raw_read();
                            }
                            r.direct = false;
                        }
                    }
                    None => self.pending.extend_from_slice(data),
                }
            }
            Proto::Trojan => self.pending.extend_from_slice(data),
            Proto::Shadowsocks(s) => self.pending = s.open(data)?,
        }
        if self.pending.is_empty() {
            return Ok(None);
        }
        let n = buf.len().min(self.pending.len());
        buf[..n].copy_from_slice(&self.pending[..n]);
        self.pending_pos = n;
        DOWN.fetch_add(n as u64, Ordering::Relaxed);
        Ok(Some(n))
    }
}

// ---- servers and subscriptions ------------------------------------------------------

/// Traffic and expiry a subscription reports (subscription-userinfo).
#[derive(Clone, Copy, Default)]
pub struct Usage {
    pub used: u64,
    pub total: u64,
    /// Unix seconds, or 0 for never.
    pub expire: i64,
}

/// A subscription and its servers, or (with no url) links added by hand.
#[derive(Clone)]
pub struct Group {
    pub url: String,
    pub title: String,
    pub usage: Option<Usage>,
    pub servers: Vec<Server>,
}

pub struct Saved {
    pub groups: Vec<Group>,
    /// The link of the chosen server.
    pub selected: String,
}

fn conf_path() -> Option<String> {
    let user = crate::users::current_name()?;
    Some(fs::join(&fs::app_data(user.as_str()), "vpn.txt"))
}

/// The servers saved for the current user.
pub fn load() -> Saved {
    let mut saved = Saved {
        groups: Vec::new(),
        selected: String::new(),
    };
    let Some(text) = conf_path().and_then(|p| fs::read(&p).ok()) else {
        return saved;
    };
    let text = String::from_utf8_lossy(&text);
    for line in text.lines() {
        let (key, value) = line.split_once(' ').unwrap_or((line, ""));
        match key {
            "sub" | "manual" => saved.groups.push(Group {
                url: value.to_string(),
                title: String::new(),
                usage: None,
                servers: Vec::new(),
            }),
            "title" => {
                if let Some(g) = saved.groups.last_mut() {
                    g.title = value.to_string();
                }
            }
            "usage" => {
                let n: Vec<i64> = value.split(' ').filter_map(|x| x.parse().ok()).collect();
                if let (Some(g), [used, total, expire]) = (saved.groups.last_mut(), n.as_slice()) {
                    g.usage = Some(Usage {
                        used: *used as u64,
                        total: *total as u64,
                        expire: *expire,
                    });
                }
            }
            "selected" => saved.selected = value.to_string(),
            _ if line.contains("://") => {
                if let (Some(g), Some(s)) = (saved.groups.last_mut(), link::parse(line)) {
                    g.servers.push(s);
                }
            }
            _ => {}
        }
    }
    saved
}

pub fn save(saved: &Saved) -> Result<(), String> {
    let path = conf_path().ok_or("nobody is signed in")?;
    let mut out = String::from("# RyzikOS VPN: subscriptions and servers\n");
    for g in &saved.groups {
        if g.url.is_empty() {
            out.push_str("manual\n");
        } else {
            out.push_str(&format!("sub {}\n", g.url));
        }
        if !g.title.is_empty() {
            out.push_str(&format!("title {}\n", g.title));
        }
        if let Some(u) = g.usage {
            out.push_str(&format!("usage {} {} {}\n", u.used, u.total, u.expire));
        }
        for s in &g.servers {
            out.push_str(&s.link);
            out.push('\n');
        }
    }
    if !saved.selected.is_empty() {
        out.push_str(&format!("selected {}\n", saved.selected));
    }
    fs::write(&path, out.as_bytes()).map_err(|e| String::from(e.message()))
}

/// Add servers: a subscription replaces its older copy, and links added
/// by hand join the hand-made group.
pub fn add_group(saved: &mut Saved, group: Group) {
    if let Some(g) = saved.groups.iter_mut().find(|g| g.url == group.url) {
        if group.url.is_empty() {
            for s in group.servers {
                if !g.servers.iter().any(|o| o.link == s.link) {
                    g.servers.push(s);
                }
            }
        } else {
            *g = group;
        }
    } else {
        saved.groups.push(group);
    }
}

/// What a subscription address gave: its title, usage and servers.
pub fn fetch_subscription(address: &str) -> Result<Group, String> {
    let address = address.trim();
    // happ://add/<address> links open the subscription in Happ
    let address = address.strip_prefix("happ://add/").unwrap_or(address);
    let url = crate::web::url::Url::parse(address).ok_or("not a web address")?;
    log(&format!("updating {}", address));
    let r = crate::web::http::get_as(&url, None, "v2rayNG/1.10.16")?;
    if !(200..300).contains(&r.status) {
        return Err(format!("the subscription server answered {}", r.status));
    }
    let text = String::from_utf8_lossy(&r.body);
    let servers = link::parse_list(&text);
    if servers.is_empty() {
        return Err(String::from("no servers in the subscription"));
    }
    let header = |name: &str| {
        r.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    };
    let mut title = header("profile-title").unwrap_or_default();
    if let Some(b) = title.strip_prefix("base64:") {
        title = String::from_utf8_lossy(&link::base64(b)).into_owned();
    }
    if title.is_empty() {
        title = url.host.clone();
    }
    let usage = header("subscription-userinfo").map(|v| {
        let mut u = Usage::default();
        for part in v.split(';') {
            let (k, n) = part.split_once('=').unwrap_or((part, ""));
            let n: i64 = n.trim().parse().unwrap_or(0);
            match k.trim() {
                "upload" | "download" => u.used += n.max(0) as u64,
                "total" => u.total = n.max(0) as u64,
                "expire" => u.expire = n,
                _ => {}
            }
        }
        u
    });
    log(&format!("{}: {} servers", title, servers.len()));
    Ok(Group {
        url: address.to_string(),
        title,
        usage,
        servers,
    })
}

/// Sizes like "1.5 GB".
pub fn size_text(n: u64) -> String {
    const G: u64 = 1 << 30;
    const M: u64 = 1 << 20;
    if n >= G {
        format!("{}.{} GB", n / G, n % G * 10 / G)
    } else if n >= M {
        format!("{}.{} MB", n / M, n % M * 10 / M)
    } else {
        format!("{} KB", n / 1024)
    }
}
