//! HTTP/1.1 client over plain TCP or TLS 1.3 and 1.2 (https, see
//! `crate::net::tls`). Downloads a whole page into memory, follows
//! redirects and keeps connections open for the next file from the same
//! server.
//!
//! HTTPS encrypts the connection but does not check the server's
//! certificate: RyzikOS has no list of trusted certificate authorities.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;


use super::url::Url;
use crate::net::tls::{self, Tls};
use crate::net::TcpStream;

/// Pages and downloads bigger than this are refused, to leave memory.
const MAX_BODY: usize = 40 * 1024 * 1024;
const MAX_REDIRECTS: usize = 8;

pub struct Response {
    /// Where the page came from, after redirects.
    pub url: Url,
    pub status: u16,
    pub content_type: String,
    /// The Content-Disposition header: "attachment" asks for a download.
    pub disposition: String,
    /// The server's clock (the Date header), in Unix seconds.
    pub date: Option<i64>,
    /// All the headers, names in lower case.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// Download `url`, following redirects. With `form`, send it as a POST.
pub fn get(url: &Url, form: Option<&str>) -> Result<Response, String> {
    match form {
        Some(f) => request(
            "POST",
            url,
            Some(("application/x-www-form-urlencoded", f.as_bytes())),
        ),
        None => request("GET", url, None),
    }
}

/// Download `url` introducing ourselves as `agent` (VPN subscriptions
/// answer differently for different apps).
pub fn get_as(url: &Url, form: Option<&str>, agent: &str) -> Result<Response, String> {
    let body = form.map(|f| ("application/x-www-form-urlencoded", f.as_bytes()));
    request_as(if form.is_some() { "POST" } else { "GET" }, url, body, Some(agent))
}

/// Send a request with any method and an optional (content type, body).
pub fn request(method: &str, url: &Url, body: Option<(&str, &[u8])>) -> Result<Response, String> {
    request_as(method, url, body, None)
}

fn request_as(
    method: &str,
    url: &Url,
    mut body: Option<(&str, &[u8])>,
    agent: Option<&str>,
) -> Result<Response, String> {
    let mut url = url.clone();
    let mut method = method.to_string();
    for _ in 0..MAX_REDIRECTS {
        let raw = fetch_raw(&method, &url, body, agent)?;
        let head = parse_head(&raw).ok_or("bad reply from server")?;
        for c in &head.cookies {
            store_cookie(&url.host, c);
        }
        if (300..400).contains(&head.status) && head.status != 304 {
            if let Some(location) = &head.location {
                url = url.join(location).ok_or("bad redirect")?;
                // redirects after a POST are plain GETs
                if head.status != 307 && head.status != 308 {
                    method = String::from("GET");
                    body = None;
                }
                continue;
            }
        }
        let data = &raw[head.body_start..];
        let data = if head.chunked {
            dechunk(data)
        } else {
            match head.content_length {
                Some(n) => data[..n.min(data.len())].to_vec(),
                None => data.to_vec(),
            }
        };
        return Ok(Response {
            url,
            status: head.status,
            content_type: head.content_type,
            disposition: head.disposition,
            date: head.date,
            headers: head.headers,
            body: data,
        });
    }
    Err("too many redirects".to_string())
}

/// Cookies by host: name and value.
static COOKIES: crate::sync::IrqMutex<BTreeMap<String, Vec<(String, String)>>> =
    crate::sync::IrqMutex::new(BTreeMap::new());

/// Store a Set-Cookie header (or a document.cookie assignment) for a host.
pub fn store_cookie(host: &str, header: &str) {
    let first = header.split(';').next().unwrap_or("");
    let Some((name, value)) = first.split_once('=') else {
        return;
    };
    let (name, value) = (name.trim().to_string(), value.trim().to_string());
    let expired = header.to_ascii_lowercase().contains("max-age=0");
    // cookies set for the parent domain go to the host we talk to
    let mut jar = COOKIES.lock();
    let list = jar.entry(host.to_string()).or_default();
    list.retain(|(n, _)| *n != name);
    if !expired && list.len() < 64 {
        list.push((name, value));
    }
}

/// The Cookie header value for a host.
pub fn cookies(host: &str) -> String {
    let jar = COOKIES.lock();
    let mut out = String::new();
    if let Some(list) = jar.get(host) {
        for (n, v) in list {
            if !out.is_empty() {
                out.push_str("; ");
            }
            out.push_str(n);
            out.push('=');
            out.push_str(v);
        }
    }
    out
}

/// An open connection to a server, plain or encrypted.
enum Link {
    Plain(TcpStream),
    Tls(Box<Tls<TcpStream>>),
}

impl Link {
    fn open(url: &Url) -> Result<Link, String> {
        let stream = TcpStream::connect_host(&url.host, url.port)?;
        if !url.https {
            return Ok(Link::Plain(stream));
        }
        let opts = tls::Options {
            sni: &url.host,
            alpn: &[],
            reality: None,
        };
        let conn = Tls::connect(stream, &opts).map_err(|e| format!("secure connection failed: {}", e))?;
        Ok(Link::Tls(Box::new(conn)))
    }

    fn write_all(&mut self, data: &[u8]) -> Result<(), String> {
        match self {
            Link::Plain(s) => s.write_all(data),
            Link::Tls(t) => t.write_all(data),
        }
    }

    fn read(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        match self {
            Link::Plain(s) => s.read(buf),
            // the server closing the connection ends the page
            Link::Tls(t) => Ok(t.read(buf, true)?.unwrap_or(0)),
        }
    }
}

/// Connections kept open for the next request to the same server, with
/// when they were last used. Pages load many files from one server, and
/// a new HTTPS connection costs several round trips.
static POOL: crate::sync::IrqMutex<Vec<(String, Link, u64)>> =
    crate::sync::IrqMutex::new(Vec::new());
const POOL_MAX: usize = 8;
/// Servers drop idle connections; do not reuse ones older than this.
const POOL_IDLE_SECS: u64 = 20;

fn pool_key(url: &Url) -> String {
    format!(
        "{}{}:{}",
        if url.https { "s:" } else { "" },
        url.host,
        url.port
    )
}

fn pool_take(key: &str) -> Option<Link> {
    let now = crate::interrupts::ticks();
    let hz = crate::interrupts::TIMER_HZ;
    let stale = {
        let mut pool = POOL.lock();
        let mut stale = Vec::new();
        let mut i = 0;
        while i < pool.len() {
            if now - pool[i].2 > POOL_IDLE_SECS * hz {
                stale.push(pool.remove(i));
            } else {
                i += 1;
            }
        }
        let found = pool.iter().position(|(k, _, _)| k == key);
        (stale, found.map(|i| pool.remove(i).1))
    };
    // closing sockets polls the network, so do it outside the lock
    let (old, found) = stale;
    drop(old);
    found
}

/// Close the kept connections (the VPN went on or off, so they lead
/// the wrong way now).
pub fn drop_pool() {
    let old = core::mem::take(&mut *POOL.lock());
    drop(old);
}

fn pool_put(key: String, link: Link) {
    let evicted = {
        let mut pool = POOL.lock();
        pool.push((key, link, crate::interrupts::ticks()));
        if pool.len() > POOL_MAX {
            Some(pool.remove(0))
        } else {
            None
        }
    };
    drop(evicted);
}

fn fetch_raw(
    method: &str,
    url: &Url,
    body: Option<(&str, &[u8])>,
    agent: Option<&str>,
) -> Result<Vec<u8>, String> {
    log(&format!("{} ", method), &url.to_string());
    let host = if url.port == if url.https { 443 } else { 80 } {
        url.host.clone()
    } else {
        format!("{}:{}", url.host, url.port)
    };
    let mut request = format!(
        "{} {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: {}\r\n\
         Accept: text/html,application/xhtml+xml,*/*;q=0.8\r\nAccept-Language: ru,en;q=0.8\r\n\
         Accept-Encoding: identity\r\nConnection: keep-alive\r\n",
        method,
        url.path,
        host,
        agent.unwrap_or("Mozilla/5.0 (EverOS; x86_64) EverBrowser/0.3")
    );
    let cookie = cookies(&url.host);
    if !cookie.is_empty() {
        request.push_str(&format!("Cookie: {}\r\n", cookie));
    }
    let mut request = request.into_bytes();
    match body {
        Some((ty, data)) => {
            request.extend_from_slice(
                format!(
                    "Content-Type: {}\r\nContent-Length: {}\r\n\r\n",
                    ty,
                    data.len()
                )
                .as_bytes(),
            );
            request.extend_from_slice(data);
        }
        None => request.extend_from_slice(b"\r\n"),
    }

    let key = pool_key(url);
    // a kept connection may have been closed by the server meanwhile:
    // then try again on a new one
    for _ in 0..3 {
        let (mut link, reused) = match pool_take(&key) {
            Some(l) => (l, true),
            None => (Link::open(url)?, false),
        };
        let result = link
            .write_all(&request)
            .and_then(|_| read_response(|buf| link.read(buf)));
        match result {
            Ok(data) => {
                let keep = parse_head(&data).is_some_and(|h| !h.close) && complete(&data);
                if keep {
                    pool_put(key, link);
                }
                return Ok(data);
            }
            Err(e) if reused && e != "cancelled" && !e.starts_with("timed out") => {
                log("stale connection: ", &e);
            }
            Err(e) => return Err(e),
        }
    }
    Err("the server keeps closing the connection".to_string())
}

/// Read until the connection closes or the body is complete.
fn read_response(
    mut read: impl FnMut(&mut [u8]) -> Result<usize, String>,
) -> Result<Vec<u8>, String> {
    let mut data = Vec::new();
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        let n = match read(&mut buf) {
            Ok(n) => n,
            // keep what arrived if the connection broke late
            Err(e) if parse_head(&data).is_some() => {
                log("read ended: ", &e);
                0
            }
            Err(e) => return Err(e),
        };
        if n == 0 {
            break;
        }
        data.extend_from_slice(&buf[..n]);
        if data.len() > MAX_BODY {
            return Err("the file is larger than 40 MB".to_string());
        }
        if complete(&data) {
            break;
        }
    }
    if data.is_empty() {
        return Err("empty reply".to_string());
    }
    Ok(data)
}

fn complete(data: &[u8]) -> bool {
    let Some(head) = parse_head(data) else {
        return false;
    };
    let body = &data[head.body_start..];
    if head.status == 204 || head.status == 304 {
        return true;
    }
    if head.chunked {
        return body.ends_with(b"0\r\n\r\n");
    }
    head.content_length.is_some_and(|n| body.len() >= n)
}

struct Head {
    status: u16,
    content_type: String,
    disposition: String,
    location: Option<String>,
    content_length: Option<usize>,
    chunked: bool,
    body_start: usize,
    cookies: Vec<String>,
    /// The server will close the connection after this reply.
    close: bool,
    date: Option<i64>,
    headers: Vec<(String, String)>,
}

fn parse_head(data: &[u8]) -> Option<Head> {
    let end = data.windows(4).position(|w| w == b"\r\n\r\n")?;
    let text = String::from_utf8_lossy(&data[..end]);
    let mut lines = text.split("\r\n");
    let status_line = lines.next()?;
    let status = status_line.split(' ').nth(1)?.parse().ok()?;
    let mut head = Head {
        status,
        content_type: String::new(),
        disposition: String::new(),
        location: None,
        content_length: None,
        chunked: false,
        body_start: end + 4,
        cookies: Vec::new(),
        close: status_line.starts_with("HTTP/1.0"),
        date: None,
        headers: Vec::new(),
    };
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        head.headers
            .push((name.trim().to_ascii_lowercase(), value.to_string()));
        match name.trim().to_ascii_lowercase().as_str() {
            "content-type" => head.content_type = value.to_ascii_lowercase(),
            "location" => head.location = Some(value.to_string()),
            "content-disposition" => head.disposition = value.to_string(),
            "content-length" => head.content_length = value.parse().ok(),
            "transfer-encoding" => head.chunked = value.to_ascii_lowercase().contains("chunked"),
            "set-cookie" => head.cookies.push(value.to_string()),
            "connection" => head.close = value.to_ascii_lowercase().contains("close"),
            "date" => head.date = parse_date(value),
            _ => {}
        }
    }
    Some(head)
}

fn dechunk(mut body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(line_end) = body.windows(2).position(|w| w == b"\r\n") {
        let size_text = String::from_utf8_lossy(&body[..line_end]);
        let size_text = size_text.split(';').next().unwrap_or("").trim();
        let Ok(size) = usize::from_str_radix(size_text, 16) else {
            break;
        };
        body = &body[line_end + 2..];
        if size == 0 {
            break;
        }
        let take = size.min(body.len());
        out.extend_from_slice(&body[..take]);
        body = &body[take..];
        if body.starts_with(b"\r\n") {
            body = &body[2..];
        }
    }
    out
}

fn log(what: &str, detail: &str) {
    crate::serial::write_str("\nbrowser: ");
    crate::serial::write_str(what);
    crate::serial::write_str(detail);
    crate::serial::write_str("\n");
}

/// An HTTP date such as "Sun, 28 Sep 2026 05:20:24 GMT", in Unix seconds.
pub fn parse_date(text: &str) -> Option<i64> {
    let mut parts = text.split_whitespace().skip(1);
    let day: i64 = parts.next()?.parse().ok()?;
    let month = parts.next()?;
    let month = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ]
    .iter()
    .position(|m| m.eq_ignore_ascii_case(month))? as i64
        + 1;
    let year: i64 = parts.next()?.parse().ok()?;
    let mut hms = parts.next()?.split(':').map(|v| v.parse::<i64>().ok());
    let (h, m, s) = (hms.next()??, hms.next()??, hms.next()??);
    Some(crate::rtc::days_from_civil(year, month, day) * 86400 + h * 3600 + m * 60 + s)
}
