//! Text helpers for HTML: reading tags, character references and
//! decoding bytes to text.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

pub struct Tag {
    pub name: String,
    pub closing: bool,
    pub attrs: Vec<(String, String)>,
}

/// Read a tag starting at `<`. Returns the index after `>` and the tag.
pub fn read_tag(s: &str, start: usize) -> (usize, Tag) {
    let b = s.as_bytes();
    let mut i = start + 1;
    let closing = b.get(i) == Some(&b'/');
    if closing {
        i += 1;
    }
    let name_start = i;
    while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'>' && b[i] != b'/' {
        i += 1;
    }
    let name = s[name_start..i].to_ascii_lowercase();
    let mut attrs = Vec::new();
    loop {
        while i < b.len() && (b[i].is_ascii_whitespace() || b[i] == b'/') {
            i += 1;
        }
        if i >= b.len() {
            break;
        }
        if b[i] == b'>' {
            i += 1;
            break;
        }
        let an_start = i;
        while i < b.len()
            && !b[i].is_ascii_whitespace()
            && b[i] != b'='
            && b[i] != b'>'
            && b[i] != b'/'
        {
            i += 1;
        }
        let attr_name = s[an_start..i].to_ascii_lowercase();
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        let mut value = String::new();
        if i < b.len() && b[i] == b'=' {
            i += 1;
            while i < b.len() && b[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < b.len() && (b[i] == b'"' || b[i] == b'\'') {
                let quote = b[i];
                i += 1;
                let v_start = i;
                while i < b.len() && b[i] != quote {
                    i += 1;
                }
                value = s[v_start..i].to_string();
                i = (i + 1).min(b.len());
            } else {
                let v_start = i;
                while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'>' {
                    i += 1;
                }
                value = s[v_start..i].to_string();
            }
        }
        if attr_name.is_empty() {
            i += 1;
            continue;
        }
        attrs.push((attr_name, value));
    }
    // keep on a char boundary (attribute values may hold UTF-8)
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    (
        i,
        Tag {
            name,
            closing,
            attrs,
        },
    )
}

pub fn find_ci(hay: &str, needle: &str) -> Option<usize> {
    let h = hay.as_bytes();
    let n = needle.as_bytes();
    if n.len() > h.len() {
        return None;
    }
    (0..=h.len() - n.len()).find(|&i| h[i..i + n.len()].eq_ignore_ascii_case(n))
}

/// Collapse runs of whitespace and trim.
pub fn collapse(s: &str) -> String {
    let mut out = String::new();
    for word in s.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

/// Bytes to text: UTF-8, or Windows-1251 / Latin-1 when the page says so.
pub fn decode(bytes: &[u8], content_type: &str) -> String {
    let mut charset = content_type.split("charset=").nth(1).map(|c| {
        c.trim_matches(|c: char| c == '"' || c == ';' || c.is_whitespace())
            .to_ascii_lowercase()
    });
    if charset.is_none() {
        // look for <meta charset> near the start
        let head = &bytes[..bytes.len().min(4096)];
        let head = String::from_utf8_lossy(head).to_ascii_lowercase();
        if let Some(pos) = head.find("charset=") {
            let rest = &head[pos + 8..];
            let rest = rest.trim_start_matches(['"', '\'']);
            let end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
                .unwrap_or(rest.len());
            charset = Some(rest[..end].to_string());
        }
    }
    match charset.as_deref() {
        Some("windows-1251") | Some("cp1251") => bytes.iter().map(|&b| cp1251(b)).collect(),
        Some("iso-8859-1") | Some("latin1") | Some("windows-1252") => {
            bytes.iter().map(|&b| b as char).collect()
        }
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

fn cp1251(b: u8) -> char {
    match b {
        0..=0x7f => b as char,
        0xc0..=0xff => char::from_u32(0x410 + (b - 0xc0) as u32).unwrap_or('?'),
        0xa8 => 'Ё',
        0xb8 => 'ё',
        0xa0 => '\u{a0}',
        0xab => '«',
        0xbb => '»',
        0x96 => '–',
        0x97 => '—',
        0x85 => '…',
        0x93 => '“',
        0x94 => '”',
        0xb9 => '№',
        _ => '?',
    }
}

/// Replace `&amp;`-style character references.
pub fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(pos) = rest.find('&') {
        out.push_str(&rest[..pos]);
        rest = &rest[pos..];
        let end = rest[1..]
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '#'))
            .map(|e| e + 1)
            .unwrap_or(rest.len());
        let name = &rest[1..end];
        let decoded = if let Some(num) = name.strip_prefix('#') {
            let code = if let Some(hex) = num.strip_prefix(['x', 'X']) {
                u32::from_str_radix(hex, 16).ok()
            } else {
                num.parse().ok()
            };
            code.and_then(char::from_u32)
        } else {
            named_entity(name)
        };
        match decoded {
            Some(c) if end > 1 => {
                out.push(c);
                rest = &rest[end..];
                if rest.starts_with(';') {
                    rest = &rest[1..];
                }
            }
            _ => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn named_entity(name: &str) -> Option<char> {
    Some(match name {
        "amp" | "AMP" => '&',
        "lt" | "LT" => '<',
        "gt" | "GT" => '>',
        "quot" | "QUOT" => '"',
        "apos" => '\'',
        "nbsp" => '\u{a0}',
        "copy" => '©',
        "reg" => '®',
        "trade" => '™',
        "mdash" => '—',
        "ndash" => '–',
        "hellip" => '…',
        "laquo" => '«',
        "raquo" => '»',
        "lsquo" => '‘',
        "rsquo" => '’',
        "ldquo" => '“',
        "rdquo" => '”',
        "bdquo" => '„',
        "middot" => '·',
        "bull" => '•',
        "times" => '×',
        "divide" => '÷',
        "deg" => '°',
        "plusmn" => '±',
        "sect" => '§',
        "para" => '¶',
        "euro" => '€',
        "pound" => '£',
        "yen" => '¥',
        "cent" => '¢',
        "larr" => '←',
        "rarr" => '→',
        "uarr" => '↑',
        "darr" => '↓',
        "shy" => '\u{ad}',
        "zwnj" | "zwj" | "lrm" | "rlm" => '\u{200b}',
        "thinsp" | "ensp" | "emsp" => ' ',
        "iexcl" => '¡',
        "iquest" => '¿',
        "frac12" => '½',
        "frac14" => '¼',
        "eacute" => 'é',
        "egrave" => 'è',
        "aacute" => 'á',
        "agrave" => 'à',
        "ouml" => 'ö',
        "uuml" => 'ü',
        "auml" => 'ä',
        "szlig" => 'ß',
        "ccedil" => 'ç',
        "ntilde" => 'ñ',
        "check" => '✓',
        _ => return None,
    })
}
