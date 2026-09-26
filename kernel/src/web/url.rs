//! Web addresses: parsing `http://host:port/path?query` and resolving
//! links relative to the current page.

use alloc::format;
use alloc::string::{String, ToString};

#[derive(Clone, PartialEq, Eq)]
pub struct Url {
    pub https: bool,
    pub host: String,
    pub port: u16,
    /// Path and query, always starting with '/'.
    pub path: String,
}

impl Url {
    /// Parse an absolute http or https address. A missing scheme means http.
    pub fn parse(text: &str) -> Option<Url> {
        let text = text.trim();
        let (https, rest) = if let Some(rest) = strip_prefix_ci(text, "https://") {
            (true, rest)
        } else if let Some(rest) = strip_prefix_ci(text, "http://") {
            (false, rest)
        } else if text.contains("://") {
            return None;
        } else {
            (false, text)
        };
        let rest = rest.split('#').next().unwrap_or("");
        let split = rest.find(['/', '?']).unwrap_or(rest.len());
        let (authority, path) = rest.split_at(split);
        // drop user:password@
        let authority = authority.rsplit('@').next().unwrap_or(authority);
        let (host, port) = match authority.rsplit_once(':') {
            Some((h, p)) if !p.is_empty() => (h, p.parse().ok()?),
            Some((h, _)) => (h, default_port(https)),
            None => (authority, default_port(https)),
        };
        if host.is_empty() || host.contains(' ') {
            return None;
        }
        let path = if path.is_empty() {
            "/".to_string()
        } else if path.starts_with('?') {
            format!("/{}", path)
        } else {
            path.to_string()
        };
        Some(Url {
            https,
            host: host.to_ascii_lowercase(),
            port,
            path: encode_spaces(&path),
        })
    }

    /// Parse an address that must start with http:// or https://.
    pub fn parse_absolute(text: &str) -> Option<Url> {
        let t = text.trim().to_ascii_lowercase();
        if t.starts_with("http://") || t.starts_with("https://") {
            Url::parse(text)
        } else {
            None
        }
    }

    /// Resolve a link found on this page.
    pub fn join(&self, link: &str) -> Option<Url> {
        let link = link.trim();
        let lower = link.to_ascii_lowercase();
        if lower.starts_with("http://") || lower.starts_with("https://") {
            return Url::parse(link);
        }
        if lower.starts_with("javascript:")
            || lower.starts_with("mailto:")
            || lower.starts_with("tel:")
            || lower.starts_with("data:")
        {
            return None;
        }
        if let Some(rest) = link.strip_prefix("//") {
            let scheme = if self.https { "https://" } else { "http://" };
            return Url::parse(&format!("{}{}", scheme, rest));
        }
        let link = link.split('#').next().unwrap_or("");
        let mut url = self.clone();
        if link.is_empty() {
            return Some(url);
        }
        url.path = if link.starts_with('/') {
            normalize(link)
        } else if link.starts_with('?') {
            let base = self.path.split('?').next().unwrap_or("/");
            format!("{}{}", base, link)
        } else {
            let base = self.path.split('?').next().unwrap_or("/");
            let dir = &base[..base.rfind('/').map_or(0, |i| i + 1)];
            normalize(&format!("{}{}", dir, link))
        };
        url.path = encode_spaces(&url.path);
        Some(url)
    }

    pub fn origin(&self) -> String {
        let scheme = if self.https { "https" } else { "http" };
        if self.port == default_port(self.https) {
            format!("{}://{}", scheme, self.host)
        } else {
            format!("{}://{}:{}", scheme, self.host, self.port)
        }
    }
}

impl core::fmt::Display for Url {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(f, "{}{}", self.origin(), self.path)
    }
}

fn default_port(https: bool) -> u16 {
    if https {
        443
    } else {
        80
    }
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    (s.len() >= prefix.len() && s[..prefix.len()].eq_ignore_ascii_case(prefix))
        .then(|| &s[prefix.len()..])
}

fn encode_spaces(path: &str) -> String {
    let mut out = String::new();
    for c in path.chars() {
        match c {
            ' ' => out.push_str("%20"),
            c if c.is_ascii() && !c.is_ascii_control() => out.push(c),
            c => {
                let mut buf = [0; 4];
                for b in c.encode_utf8(&mut buf).bytes() {
                    out.push_str(&format!("%{:02X}", b));
                }
            }
        }
    }
    out
}

/// Remove `.` and `..` segments from an absolute path.
fn normalize(path: &str) -> String {
    let (path, query) = match path.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (path, None),
    };
    let mut parts: alloc::vec::Vec<&str> = alloc::vec::Vec::new();
    for seg in path.split('/').skip(1) {
        match seg {
            "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    let mut out = String::new();
    for seg in &parts {
        out.push('/');
        out.push_str(seg);
    }
    if out.is_empty() || path.ends_with("/.") || path.ends_with("/..") {
        out.push('/');
    }
    if let Some(q) = query {
        out.push('?');
        out.push_str(q);
    }
    out
}

/// Percent-encode text for a search query.
pub fn encode_query(text: &str) -> String {
    let mut out = String::new();
    for b in text.bytes() {
        match b {
            b' ' => out.push('+'),
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// Undo %XX escapes (for javascript: links).
pub fn decode_percent(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = alloc::vec::Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) =
                u8::from_str_radix(core::str::from_utf8(&b[i + 1..i + 3]).unwrap_or(""), 16)
            {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
