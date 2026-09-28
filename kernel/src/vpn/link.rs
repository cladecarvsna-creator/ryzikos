//! VPN servers from share links (vless://, trojan://, ss://) and from
//! subscriptions: a web address that answers with such links, one per
//! line, often all in base64, the way Happ and v2rayNG read them.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::web::url::decode_percent;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Vless,
    Trojan,
    Shadowsocks,
    Vmess,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Security {
    None,
    Tls,
    Reality,
}

#[derive(Clone, Debug)]
pub struct Server {
    pub name: String,
    pub kind: Kind,
    pub host: String,
    pub port: u16,
    /// The VLESS UUID, the Trojan password or the Shadowsocks password.
    pub secret: String,
    /// Shadowsocks: the cipher.
    pub method: String,
    pub security: Security,
    pub sni: String,
    pub alpn: Vec<String>,
    /// REALITY: the server's public key and the short id.
    pub pbk: String,
    pub sid: String,
    /// VLESS: "xtls-rprx-vision" or empty.
    pub flow: String,
    /// "tcp" (also called "raw"); others are not supported yet.
    pub transport: String,
    /// The link it came from, as saved.
    pub link: String,
}

impl Server {
    /// A short name of the protocol, for the list.
    pub fn protocol(&self) -> String {
        let base = match self.kind {
            Kind::Vless => "VLESS",
            Kind::Trojan => "Trojan",
            Kind::Shadowsocks => "Shadowsocks",
            Kind::Vmess => "VMess",
        };
        match self.security {
            Security::Reality => format!("{} Reality", base),
            Security::Tls if self.kind == Kind::Vless => format!("{} TLS", base),
            _ => String::from(base),
        }
    }

    /// Why RyzikOS can't use this server, if it can't.
    pub fn unsupported(&self) -> Option<String> {
        if self.kind == Kind::Vmess {
            return Some(String::from("VMess is not supported yet"));
        }
        let t = self.transport.as_str();
        if !(t.is_empty() || t == "tcp" || t == "raw") {
            return Some(format!("the {} transport is not supported yet", t));
        }
        if self.kind == Kind::Shadowsocks && super::ss::Method::parse(&self.method).is_none() {
            return Some(format!("the {} cipher is not supported yet", self.method));
        }
        if !(self.flow.is_empty() || self.flow == "xtls-rprx-vision") {
            return Some(format!("the {} flow is not supported", self.flow));
        }
        None
    }
}

/// Decode base64, standard or URL-safe, with or without padding.
pub fn base64(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut bits = 0u32;
    let mut n = 0;
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => continue,
        };
        bits = bits << 6 | v as u32;
        n += 6;
        if n >= 8 {
            n -= 8;
            out.push((bits >> n) as u8);
        }
    }
    out
}

fn looks_base64(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"+/=-_\r\n".contains(&c))
}

/// The query parameters of a link, decoded.
fn params(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter_map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (!k.is_empty()).then(|| (k.to_ascii_lowercase(), decode_percent(v)))
        })
        .collect()
}

fn param<'a>(ps: &'a [(String, String)], key: &str) -> &'a str {
    ps.iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
        .unwrap_or("")
}

/// "host:port", with IPv6 in brackets.
fn host_port(s: &str) -> Option<(String, u16)> {
    let (host, port) = if let Some(rest) = s.strip_prefix('[') {
        let (h, p) = rest.split_once(']')?;
        (h, p.strip_prefix(':')?)
    } else {
        s.rsplit_once(':')?
    };
    let port = port.trim_end_matches('/').parse().ok()?;
    (!host.is_empty()).then(|| (host.to_string(), port))
}

fn blank(kind: Kind, link: &str) -> Server {
    Server {
        name: String::new(),
        kind,
        host: String::new(),
        port: 0,
        secret: String::new(),
        method: String::new(),
        security: Security::None,
        sni: String::new(),
        alpn: Vec::new(),
        pbk: String::new(),
        sid: String::new(),
        flow: String::new(),
        transport: String::from("tcp"),
        link: link.to_string(),
    }
}

/// One share link.
pub fn parse(link: &str) -> Option<Server> {
    let link = link.trim();
    let (scheme, rest) = link.split_once("://")?;
    let (rest, name) = match rest.split_once('#') {
        Some((r, n)) => (r, decode_percent(n)),
        None => (rest, String::new()),
    };
    let mut s = match scheme.to_ascii_lowercase().as_str() {
        "vless" => url_style(Kind::Vless, rest, link)?,
        "trojan" => {
            let mut s = url_style(Kind::Trojan, rest, link)?;
            // Trojan is always TLS unless the link says otherwise
            if s.security == Security::None && !rest.contains("security=none") {
                s.security = Security::Tls;
            }
            s
        }
        "ss" => shadowsocks(rest, link)?,
        "vmess" => vmess(rest, link)?,
        _ => return None,
    };
    if !name.trim().is_empty() {
        s.name = name.trim().to_string();
    } else if s.name.is_empty() {
        s.name = format!("{}:{}", s.host, s.port);
    }
    if s.sni.is_empty() && s.security != Security::None {
        s.sni = s.host.clone();
    }
    Some(s)
}

/// vless://uuid@host:port?params and trojan://password@host:port?params
fn url_style(kind: Kind, rest: &str, link: &str) -> Option<Server> {
    let (main, query) = rest.split_once('?').unwrap_or((rest, ""));
    let (user, addr) = main.rsplit_once('@')?;
    let (host, port) = host_port(addr)?;
    let ps = params(query);
    let mut s = blank(kind, link);
    s.host = host;
    s.port = port;
    s.secret = decode_percent(user);
    s.security = match param(&ps, "security") {
        "reality" => Security::Reality,
        "tls" | "xtls" => Security::Tls,
        _ => Security::None,
    };
    s.sni = param(&ps, "sni").to_string();
    if s.sni.is_empty() {
        s.sni = param(&ps, "peer").to_string();
    }
    s.alpn = param(&ps, "alpn")
        .split(',')
        .filter(|a| !a.is_empty())
        .map(String::from)
        .collect();
    s.pbk = param(&ps, "pbk").to_string();
    s.sid = param(&ps, "sid").to_string();
    s.flow = param(&ps, "flow").to_string();
    let t = param(&ps, "type");
    if !t.is_empty() {
        s.transport = t.to_ascii_lowercase();
    }
    Some(s)
}

/// ss://base64(method:password)@host:port, ss://method:password@host:port
/// or the old ss://base64(method:password@host:port).
fn shadowsocks(rest: &str, link: &str) -> Option<Server> {
    let (main, query) = rest.split_once('?').unwrap_or((rest, ""));
    let main = main.trim_end_matches('/');
    let (user, addr) = match main.rsplit_once('@') {
        Some((u, a)) => {
            let u = decode_percent(u);
            let user = if u.contains(':') {
                u
            } else {
                String::from_utf8(base64(&u)).ok()?
            };
            (user, a.to_string())
        }
        None => {
            let all = String::from_utf8(base64(main)).ok()?;
            let (u, a) = all.rsplit_once('@')?;
            (u.to_string(), a.to_string())
        }
    };
    let (method, password) = user.split_once(':')?;
    let (host, port) = host_port(&addr)?;
    let mut s = blank(Kind::Shadowsocks, link);
    s.host = host;
    s.port = port;
    s.method = method.to_ascii_lowercase();
    s.secret = password.to_string();
    // plugins (obfs, v2ray-plugin) are not supported
    if params(query).iter().any(|(k, _)| k == "plugin") {
        s.transport = String::from("plugin");
    }
    Some(s)
}

/// vmess://base64(JSON): listed so people see it, but not usable yet.
fn vmess(rest: &str, link: &str) -> Option<Server> {
    let json = String::from_utf8(base64(rest)).ok()?;
    let field = |key: &str| -> String {
        let pat = format!("\"{}\"", key);
        let Some(at) = json.find(&pat) else {
            return String::new();
        };
        let after = json[at + pat.len()..].trim_start().trim_start_matches(':').trim_start();
        if let Some(q) = after.strip_prefix('"') {
            q.split('"').next().unwrap_or("").to_string()
        } else {
            after
                .split([',', '}'])
                .next()
                .unwrap_or("")
                .trim()
                .to_string()
        }
    };
    let mut s = blank(Kind::Vmess, link);
    s.host = field("add");
    s.port = field("port").parse().ok()?;
    s.secret = field("id");
    s.name = field("ps");
    Some(s)
}

/// All the servers in a subscription's text (or in pasted text).
pub fn parse_list(text: &str) -> Vec<Server> {
    let trimmed = text.trim();
    let decoded;
    let text = if !trimmed.contains("://") && looks_base64(trimmed) {
        decoded = String::from_utf8_lossy(&base64(trimmed)).into_owned();
        decoded.as_str()
    } else {
        trimmed
    };
    text.split(['\n', '\r', ' '])
        .filter_map(parse)
        .collect()
}

/// Parse a hex string such as REALITY's short id into `N` bytes,
/// zero-padded on the right.
pub fn hex_bytes<const N: usize>(s: &str) -> Option<[u8; N]> {
    let s = s.trim();
    if s.len() > N * 2 || !s.len().is_multiple_of(2) {
        return None;
    }
    let mut out = [0u8; N];
    for (i, pair) in s.as_bytes().chunks(2).enumerate() {
        let hex = core::str::from_utf8(pair).ok()?;
        out[i] = u8::from_str_radix(hex, 16).ok()?;
    }
    Some(out)
}

/// A UUID's 16 bytes. VLESS also accepts any other text, mapped to a
/// UUID the way Xray does (SHA-1 of a fixed namespace and the text).
pub fn uuid(s: &str) -> [u8; 16] {
    let hex: String = s.chars().filter(|c| *c != '-').collect();
    if hex.len() == 32 {
        if let Some(b) = hex_bytes::<16>(&hex) {
            return b;
        }
    }
    use sha1::{Digest, Sha1};
    let mut h = Sha1::new();
    h.update([0u8; 16]);
    h.update(s.as_bytes());
    let d = h.finalize();
    let mut out = [0u8; 16];
    out.copy_from_slice(&d[..16]);
    out[6] = (out[6] & 0x0f) | 0x50;
    out[8] = (out[8] & 0x3f) | 0x80;
    out
}
